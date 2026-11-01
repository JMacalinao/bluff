"""The devices bluff paired with, and which of them BlueZ lets connect."""

import asyncio
import logging
import os
from collections.abc import Callable, Coroutine
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from dbus_fast import DBusError, Message, MessageType, Variant
from dbus_fast.aio import MessageBus

from bluff.bus import call

logger = logging.getLogger(__name__)

DEVICE = "org.bluez.Device1"
PROPERTIES = "org.freedesktop.DBus.Properties"
STORE = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "bluff" / "hosts"


@dataclass
class Host:
    path: str
    name: str
    blocked: bool
    connected: bool


class Hosts:
    """Paired hosts, kept in sync with BlueZ; `use` lets one connect and blocks the rest.

    BlueZ notifies every subscribed host at once, so blocking the others is the
    only way to type on just one. Blocking keeps the bond, and BlueZ remembers
    it across restarts. bluff's addresses live in `store` because the adapter's
    bond list also holds headphones and other devices bluff must not touch.
    """

    def __init__(self, adapter_path: str, store: Path = STORE) -> None:
        self._adapter = adapter_path
        self._store = store
        self.addresses = set(store.read_text().split()) if store.exists() else set()
        self.hosts: dict[str, Host] = {}
        self._accepted: set[str] = set()
        self._bus: MessageBus | None = None
        self._tasks: set[asyncio.Task[None]] = set()
        self.on_change: Callable[[], None] = lambda: None

    async def start(self, bus: MessageBus) -> None:
        self._bus = bus
        bus.add_message_handler(self._on_signal)
        rule = f"type='signal',sender='org.bluez',interface='{PROPERTIES}',member='PropertiesChanged',arg0='{DEVICE}'"
        await call(
            bus, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", "s", [rule]
        )
        [objects] = await call(bus, "org.bluez", "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects")
        for path, interfaces in objects.items():
            if DEVICE in interfaces and path.startswith(f"{self._adapter}/"):
                self._update(path, interfaces[DEVICE])

    def accepted(self, path: str) -> None:
        """Remember a device the agent let pair; it becomes a host once BlueZ reports it paired."""
        self._accepted.add(path)

    def _update(self, path: str, props: dict[str, Variant]) -> None:
        host = self.hosts.get(path)
        if host is None:
            if props.get("Address", Variant("s", "")).value not in self.addresses:
                return
            host = self.hosts[path] = Host(path, "", False, False)
        if "Alias" in props:
            host.name = props["Alias"].value
        if "Blocked" in props:
            host.blocked = props["Blocked"].value
        if "Connected" in props:
            host.connected = props["Connected"].value

    def _on_signal(self, message: Message) -> None:
        if message.message_type != MessageType.SIGNAL or message.member != "PropertiesChanged":
            return
        if not message.body or message.body[0] != DEVICE:
            return
        path, changed = message.path or "", message.body[1]
        if path in self._accepted and "Paired" in changed and changed["Paired"].value:
            self._accepted.discard(path)
            self._spawn(self._adopt(path))
        elif path in self.hosts:
            self._update(path, changed)
            self.on_change()

    async def _adopt(self, path: str) -> None:
        if self._bus is None:
            return
        [props] = await call(self._bus, "org.bluez", path, PROPERTIES, "GetAll", "s", [DEVICE])
        self.addresses.add(props["Address"].value)
        self._store.parent.mkdir(parents=True, exist_ok=True)
        self._store.write_text("".join(f"{address}\n" for address in sorted(self.addresses)))
        self._update(path, props)
        logger.info("new host %r", self.hosts[path].name)
        await self.use(path)

    async def use(self, path: str) -> None:
        """Let the host at `path` connect and block every other host."""
        if self._bus is None:
            return
        # Unblock first so there is never a moment where every host is shut out.
        for host in sorted(self.hosts.values(), key=lambda host: host.path != path):
            blocked = host.path != path
            try:
                await call(self._bus, "org.bluez", host.path, PROPERTIES, "Set", "ssv",
                           [DEVICE, "Blocked", Variant("b", blocked)])  # fmt: skip
            except DBusError as error:
                logger.warning("could not %s %r: %s", "block" if blocked else "unblock", host.name, error.text)
                continue
            host.blocked = blocked
        logger.info("only %r may connect now", self.hosts[path].name)
        self.on_change()

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
