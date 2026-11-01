"""The devices bluff paired with, and which one of them gets bluff's input."""

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
    address: str
    name: str
    connected: bool


class Hosts:
    """Paired hosts, kept in sync with BlueZ, and the one chosen to get input.

    BlueZ notifies every subscribed host at once, so once the chosen host is
    connected every other host is disconnected; with advertising off (see
    `Peripheral`) they have nothing to reconnect to and wait quietly. Hosts are
    never blocked: a phone whose reconnects keep getting rejected gives up and
    has to be reconnected by hand. bluff's addresses live in `store` because the
    adapter's bond list also holds headphones and other devices bluff must not
    touch; the chosen address lives next to it.
    """

    def __init__(self, adapter_path: str, store: Path = STORE) -> None:
        self._adapter = adapter_path
        self._store = store
        self._chosen_store = store.with_name("chosen")
        self.addresses = set(store.read_text().split()) if store.exists() else set()
        self.chosen_address = self._chosen_store.read_text().strip() if self._chosen_store.exists() else ""
        self.hosts: dict[str, Host] = {}
        self._accepted: set[str] = set()
        self._bus: MessageBus | None = None
        self._tasks: set[asyncio.Task[None]] = set()
        self.on_change: Callable[[], None] = lambda: None

    @property
    def chosen(self) -> Host | None:
        return next((host for host in self.hosts.values() if host.address == self.chosen_address), None)

    @property
    def settled(self) -> bool:
        """Whether the host that should get input is connected, so no other host needs to find bluff."""
        chosen = self.chosen
        if chosen is None:
            return any(host.connected for host in self.hosts.values())
        return chosen.connected

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
                if path in self.hosts and interfaces[DEVICE].get("Blocked", Variant("b", False)).value:
                    await self._unblock(self.hosts[path], bus)
        await self._enforce()

    async def _unblock(self, host: Host, bus: MessageBus) -> None:
        # Earlier versions blocked the hosts that were not chosen.
        try:
            await call(bus, "org.bluez", host.path, PROPERTIES, "Set", "ssv",
                       [DEVICE, "Blocked", Variant("b", False)])  # fmt: skip
            logger.info("unblocked %r", host.name)
        except DBusError as error:
            logger.warning("could not unblock %r: %s", host.name, error.text)

    def accepted(self, path: str) -> None:
        """Remember a device the agent let pair; it becomes a host once BlueZ reports it paired."""
        self._accepted.add(path)

    def _update(self, path: str, props: dict[str, Variant]) -> None:
        host = self.hosts.get(path)
        if host is None:
            address = props.get("Address", Variant("s", "")).value
            if address not in self.addresses:
                return
            host = self.hosts[path] = Host(path, address, "", False)
        if "Alias" in props:
            host.name = props["Alias"].value
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
            self._spawn(self._enforce())
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
        """Choose the host at `path`; the others are disconnected once it connects."""
        host = self.hosts[path]
        self.chosen_address = host.address
        self._chosen_store.parent.mkdir(parents=True, exist_ok=True)
        self._chosen_store.write_text(f"{host.address}\n")
        logger.info("input is for %r from now on", host.name)
        await self._enforce()
        self.on_change()

    async def _enforce(self) -> None:
        chosen = self.chosen
        if self._bus is None or chosen is None or not chosen.connected:
            return
        for host in list(self.hosts.values()):
            if host is chosen or not host.connected:
                continue
            logger.info("disconnecting %r so only %r gets input", host.name, chosen.name)
            try:
                await call(self._bus, "org.bluez", host.path, DEVICE, "Disconnect")
            except DBusError as error:
                logger.warning("could not disconnect %r: %s", host.name, error.text)

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
