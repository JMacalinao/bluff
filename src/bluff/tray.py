"""Tray icon (StatusNotifierItem with a DBusMenu) and notification prompts on the session bus."""

import asyncio
import logging
import os
import struct
from collections.abc import Coroutine
from enum import IntEnum
from functools import cache
from importlib.resources import files
from typing import Annotated, Any

from dbus_fast import DBusError, Message, MessageType, Variant
from dbus_fast.aio import MessageBus
from dbus_fast.annotations import DBusBool, DBusInt32, DBusObjectPath, DBusSignature, DBusStr, DBusUInt32
from dbus_fast.constants import PropertyAccess
from dbus_fast.errors import DBusFastError
from dbus_fast.service import ServiceInterface, dbus_method, dbus_property, dbus_signal

from bluff.bridge import Bridge
from bluff.bus import call
from bluff.hosts import Host

logger = logging.getLogger(__name__)

ITEM_PATH = "/StatusNotifierItem"
MENU_PATH = "/MenuBar"
WATCHER = "org.kde.StatusNotifierWatcher"
NOTIFICATIONS = "org.freedesktop.Notifications"
PAIRING_SECONDS = 120
PROMPT_SECONDS = 60
FIRST_HOST_ITEM = 100

Props = dict[str, Variant]
Layout = Annotated[list[Any], DBusSignature("u(ia{sv}av)")]
ToolTipStruct = Annotated[list[Any], DBusSignature("(sa(iiay)ss)")]
Pixmaps = Annotated[list[Any], DBusSignature("a(iiay)")]


@cache
def pixmaps(state: str) -> list[list[Any]]:
    """Tray pixmaps for a state, as built by brandkit/build.py: frames of (width, height, ARGB32)."""
    data = files("bluff").joinpath("icons", f"tray-{state}.argb").read_bytes()
    frames, offset = [], 0
    while offset < len(data):
        width, height = struct.unpack_from(">II", data, offset)
        size = width * height * 4
        frames.append([width, height, data[offset + 8 : offset + 8 + size]])
        offset += 8 + size
    return frames


class Item(IntEnum):
    STATUS = 1
    TOGGLE = 2
    PAIR = 3
    QUIT = 4


class StatusNotifierItem(ServiceInterface):
    def __init__(self, tray: Tray) -> None:
        super().__init__("org.kde.StatusNotifierItem")
        self._tray = tray

    @dbus_property(access=PropertyAccess.READ)
    def Category(self) -> DBusStr:
        return "Hardware"

    @dbus_property(access=PropertyAccess.READ)
    def Id(self) -> DBusStr:
        return "bluff"

    @dbus_property(access=PropertyAccess.READ)
    def Title(self) -> DBusStr:
        return "bluff"

    @dbus_property(access=PropertyAccess.READ)
    def Status(self) -> DBusStr:
        return "NeedsAttention" if self._tray.peripheral.pairing else "Active"

    @dbus_property(access=PropertyAccess.READ)
    def IconName(self) -> DBusStr:
        return ""

    @dbus_property(access=PropertyAccess.READ)
    def IconPixmap(self) -> Pixmaps:
        return pixmaps(self._tray.icon())

    @dbus_property(access=PropertyAccess.READ)
    def ToolTip(self) -> ToolTipStruct:
        return ["", pixmaps(self._tray.icon()), "bluff", self._tray.summary()]

    @dbus_property(access=PropertyAccess.READ)
    def Menu(self) -> DBusObjectPath:
        return MENU_PATH

    @dbus_property(access=PropertyAccess.READ)
    def ItemIsMenu(self) -> DBusBool:
        return False

    @dbus_method()
    def Activate(self, x: DBusInt32, y: DBusInt32) -> None:
        self._tray.clicked(Item.TOGGLE)

    @dbus_method()
    def SecondaryActivate(self, x: DBusInt32, y: DBusInt32) -> None:
        pass

    @dbus_method()
    def ContextMenu(self, x: DBusInt32, y: DBusInt32) -> None:
        pass

    @dbus_method()
    def Scroll(self, delta: DBusInt32, orientation: DBusStr) -> None:
        pass

    @dbus_signal()
    def NewIcon(self) -> None:
        pass

    @dbus_signal()
    def NewToolTip(self) -> None:
        pass

    @dbus_signal()
    def NewStatus(self, status: str) -> DBusStr:
        return status


class DBusMenu(ServiceInterface):
    """The com.canonical.dbusmenu protocol for a flat menu."""

    def __init__(self, tray: Tray) -> None:
        super().__init__("com.canonical.dbusmenu")
        self._tray = tray
        self.revision = 1

    def _items(self) -> list[tuple[int, Props]]:
        return [(int(item), {k: Variant(*v) for k, v in props.items()}) for item, props in self._tray.menu()]

    @dbus_property(access=PropertyAccess.READ)
    def Version(self) -> DBusUInt32:
        return 3

    @dbus_property(access=PropertyAccess.READ)
    def TextDirection(self) -> DBusStr:
        return "ltr"

    @dbus_property(access=PropertyAccess.READ)
    def Status(self) -> DBusStr:
        return "normal"

    @dbus_property(access=PropertyAccess.READ)
    def IconThemePath(self) -> Annotated[list[str], DBusSignature("as")]:
        return []

    @dbus_method()
    def GetLayout(
        self, parent: DBusInt32, depth: DBusInt32, names: Annotated[list[str], DBusSignature("as")]
    ) -> Layout:
        children = [Variant("(ia{sv}av)", [item, props, []]) for item, props in self._items()]
        return [self.revision, [0, {"children-display": Variant("s", "submenu")}, children]]

    @dbus_method()
    def GetGroupProperties(
        self, ids: Annotated[list[int], DBusSignature("ai")], names: Annotated[list[str], DBusSignature("as")]
    ) -> Annotated[list[Any], DBusSignature("a(ia{sv})")]:
        return [[item, props] for item, props in self._items() if not ids or item in ids]

    @dbus_method()
    def GetProperty(self, item: DBusInt32, name: DBusStr) -> Annotated[Variant, DBusSignature("v")]:
        props = dict(self._items()).get(item, {})
        if name not in props:
            raise DBusError("org.freedesktop.DBus.Error.InvalidArgs", f"no property {name} on {item}")
        return props[name]

    @dbus_method()
    def Event(
        self, item: DBusInt32, event: DBusStr, data: Annotated[Variant, DBusSignature("v")], timestamp: DBusUInt32
    ) -> None:
        if event == "clicked":
            self._tray.clicked(item)

    @dbus_method()
    def EventGroup(
        self, events: Annotated[list[Any], DBusSignature("a(isvu)")]
    ) -> Annotated[list[int], DBusSignature("ai")]:
        for item, event, _, _ in events:
            if event == "clicked":
                self._tray.clicked(item)
        return []

    @dbus_method()
    def AboutToShow(self, item: DBusInt32) -> DBusBool:
        return False

    @dbus_method()
    def AboutToShowGroup(
        self, ids: Annotated[list[int], DBusSignature("ai")]
    ) -> Annotated[list[Any], DBusSignature("aiai")]:
        return [[], []]

    @dbus_signal()
    def LayoutUpdated(self) -> Annotated[list[int], DBusSignature("ui")]:
        return [self.revision, 0]


class Tray:
    """Shows where input goes, switches on click, and runs pairing with notification prompts."""

    def __init__(self, bridge: Bridge, quit: asyncio.Event) -> None:
        self.bridge = bridge
        self.peripheral = bridge.peripheral
        self._quit = quit
        self._bus: MessageBus | None = None
        self._item = StatusNotifierItem(self)
        self._menu = DBusMenu(self)
        self._pairing_timer: asyncio.TimerHandle | None = None
        self._tasks: set[asyncio.Task[None]] = set()
        self._prompts: dict[int, asyncio.Future[bool]] = {}

    def icon(self) -> str:
        if self.peripheral.pairing:
            return "pairing"
        if not self.peripheral.connected:
            return "disconnected"
        return "ipad" if self.bridge.active else "laptop"

    def summary(self) -> str:
        if self.peripheral.pairing:
            return "Waiting for a device to pair"
        if not self.peripheral.connected:
            return "No device connected"
        return "Input goes to the device" if self.bridge.active else "Device connected; input stays on the laptop"

    def _hosts(self) -> list[Host]:
        return sorted(self.peripheral.hosts.hosts.values(), key=lambda host: (host.name, host.path))

    def menu(self) -> list[tuple[int, dict[str, tuple[str, Any]]]]:
        can_switch = self.peripheral.connected and not self.bridge.active
        hosts = [
            (FIRST_HOST_ITEM + index, {"label": ("s", host.name), "toggle-type": ("s", "radio"),
                                       "toggle-state": ("i", 0 if host.blocked else 1)})
            for index, host in enumerate(self._hosts())
        ]  # fmt: skip
        return [
            (Item.STATUS, {"label": ("s", self.summary()), "enabled": ("b", False)}),
            (Item.TOGGLE, {"label": ("s", "Switch to device"), "enabled": ("b", can_switch)}),
            *hosts,
            (Item.PAIR, {"label": ("s", "Stop pairing" if self.peripheral.pairing else "Pair a new device…")}),
            (Item.QUIT, {"label": ("s", "Quit bluff")}),
        ]

    def refresh(self) -> None:
        self._item.NewIcon()
        self._item.NewToolTip()
        self._item.NewStatus("NeedsAttention" if self.peripheral.pairing else "Active")
        self._menu.revision += 1
        self._menu.LayoutUpdated()

    def clicked(self, item: int) -> None:
        match item:
            case Item.TOGGLE if self.peripheral.connected and not self.bridge.active:
                self.bridge.set_active(True)
            case Item.PAIR:
                self._spawn(self._set_pairing(not self.peripheral.pairing))
            case Item.QUIT:
                self._quit.set()
            case _ if 0 <= item - FIRST_HOST_ITEM < len(hosts := self._hosts()):
                self._spawn(self.peripheral.hosts.use(hosts[item - FIRST_HOST_ITEM].path))

    async def start(self) -> None:
        bus = self._bus = await MessageBus().connect()
        bus.export(ITEM_PATH, self._item)
        bus.export(MENU_PATH, self._menu)
        bus.add_message_handler(self._on_signal)
        name = f"org.kde.StatusNotifierItem-{os.getpid()}-1"
        await bus.request_name(name)
        for rule in (
            f"type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{WATCHER}'",
            f"type='signal',interface='{NOTIFICATIONS}'",
        ):
            await call(
                bus, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", "s", [rule]
            )
        self.bridge.on_change = self.refresh
        await self._register(name)

    async def _register(self, name: str) -> None:
        if self._bus is None:
            return
        try:
            await call(self._bus, WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem", "s", [name])
            logger.info("tray icon registered")
        except DBusError as error:
            logger.warning("no tray to show the icon in yet (%s); waiting for one", error.text)

    def _on_signal(self, message: Message) -> None:
        if message.message_type != MessageType.SIGNAL:
            return
        if message.member == "NameOwnerChanged" and message.body[0] == WATCHER and message.body[2]:
            self._spawn(self._register(f"org.kde.StatusNotifierItem-{os.getpid()}-1"))
        elif message.interface == NOTIFICATIONS and message.body and message.body[0] in self._prompts:
            future = self._prompts.pop(message.body[0])
            if not future.done():
                future.set_result(message.member == "ActionInvoked" and message.body[1] == "accept")

    async def _set_pairing(self, on: bool) -> None:
        if self._pairing_timer:
            self._pairing_timer.cancel()
            self._pairing_timer = None
        await self.peripheral.set_pairing(self.ask if on else None)
        if on:
            loop = asyncio.get_running_loop()
            self._pairing_timer = loop.call_later(PAIRING_SECONDS, lambda: self._spawn(self._set_pairing(False)))
        self.refresh()

    async def ask(self, question: str) -> bool:
        """Show the pairing question as a notification with Accept and Reject buttons."""
        if self._bus is None:
            return False
        actions = ["accept", "Accept", "reject", "Reject"]
        hints = {"urgency": Variant("y", 2)}
        body = ["bluff", 0, "", "Pair with bluff?", question, actions, hints, 0]
        [notification] = await call(self._bus, NOTIFICATIONS, "/org/freedesktop/Notifications", NOTIFICATIONS,
                                    "Notify", "susssasa{sv}i", body)  # fmt: skip
        future = self._prompts[notification] = asyncio.get_running_loop().create_future()
        try:
            accepted = await asyncio.wait_for(future, PROMPT_SECONDS)
        except TimeoutError:
            self._prompts.pop(notification, None)
            accepted = False
        if accepted:
            self._spawn(self._set_pairing(False))
        return accepted

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)


async def start_tray(bridge: Bridge, quit: asyncio.Event) -> Tray | None:
    """Start the tray, or log and return None when there is no usable session bus."""
    tray = Tray(bridge, quit)
    try:
        await tray.start()
    except (DBusFastError, OSError) as error:
        logger.warning("running without a tray icon: %s", error)
        return None
    return tray
