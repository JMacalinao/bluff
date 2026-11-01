"""BlueZ GATT server, advertisement and pairing agent for a HID-over-GATT peripheral."""

import asyncio
import logging
import socket
from collections.abc import Awaitable, Callable
from typing import Annotated, Any

from dbus_fast import BusType, DBusError, Variant
from dbus_fast.aio import MessageBus
from dbus_fast.annotations import DBusBool, DBusBytes, DBusObjectPath, DBusSignature, DBusStr, DBusUInt16
from dbus_fast.constants import PropertyAccess
from dbus_fast.service import ServiceInterface, dbus_method, dbus_property

from bluff.bus import call
from bluff.hid import KEYBOARD_ID, MOUSE_ID, REPORT_MAP
from bluff.hosts import Host, Hosts

logger = logging.getLogger(__name__)

Options = Annotated[dict[str, Variant], DBusSignature("a{sv}")]
Strings = Annotated[list[str], DBusSignature("as")]
type Confirm = Callable[[str], Awaitable[bool]]

ROOT = "/org/bluff/gatt"
ADVERTISEMENT = "/org/bluff/advertisement"
AGENT = "/org/bluff/agent"
APPEARANCE_KEYBOARD = 0x03C1
# Report flags force an encrypted link, which is what makes iPadOS bond.
SECURE_READ = ["encrypt-read"]
SECURE_NOTIFY = ["encrypt-read", "notify"]
RESUBSCRIBE_SECONDS = 5


def _read(value: bytes, options: dict[str, Variant]) -> bytes:
    offset = options["offset"].value if "offset" in options else 0
    return value[offset:]


class GattService(ServiceInterface):
    def __init__(self, path: str, uuid: str) -> None:
        super().__init__("org.bluez.GattService1")
        self.path = path
        self._uuid = uuid

    @dbus_property(access=PropertyAccess.READ)
    def UUID(self) -> DBusStr:
        return self._uuid

    @dbus_property(access=PropertyAccess.READ)
    def Primary(self) -> DBusBool:
        return True


class GattCharacteristic(ServiceInterface):
    def __init__(self, service: GattService, index: int, uuid: str, flags: list[str], value: bytes = b"") -> None:
        super().__init__("org.bluez.GattCharacteristic1")
        self.path = f"{service.path}/char{index}"
        self._service = service.path
        self._uuid = uuid
        self._flags = flags
        self.value = value
        self.notifying = False
        self.listener: Callable[[], None] | None = None

    def notify(self, value: bytes) -> None:
        self.value = value
        if self.notifying:
            self.emit_properties_changed({"Value": value})

    @dbus_property(access=PropertyAccess.READ)
    def UUID(self) -> DBusStr:
        return self._uuid

    @dbus_property(access=PropertyAccess.READ)
    def Service(self) -> DBusObjectPath:
        return self._service

    @dbus_property(access=PropertyAccess.READ)
    def Flags(self) -> Strings:
        return self._flags

    @dbus_property(access=PropertyAccess.READ)
    def Value(self) -> DBusBytes:
        return self.value

    @dbus_method()
    def ReadValue(self, options: Options) -> DBusBytes:
        return _read(self.value, options)

    @dbus_method()
    def WriteValue(self, value: DBusBytes, options: Options) -> None:
        logger.debug("write to %s: %s", self._uuid, value.hex())

    @dbus_method()
    def StartNotify(self) -> None:
        logger.info("host subscribed to %s", self.path)
        self.notifying = True
        if self.listener:
            self.listener()

    @dbus_method()
    def StopNotify(self) -> None:
        logger.info("host unsubscribed from %s", self.path)
        self.notifying = False
        if self.listener:
            self.listener()


class ReportCharacteristic(GattCharacteristic):
    """Input report delivered to one host at a time.

    Changing `Value` would notify every subscribed host. Exposing `NotifyAcquired`
    makes BlueZ call `AcquireNotify` once per subscribing host instead, with the
    host's device path, and send whatever is written to that host's socket to
    that host only.
    """

    def __init__(self, service: GattService, index: int) -> None:
        super().__init__(service, index, "2a4d", SECURE_NOTIFY)
        self.sockets: dict[str, socket.socket] = {}
        self.target: Callable[[], str | None] = lambda: None

    def notify(self, value: bytes) -> None:
        """Send `value` to the target host, or to every host when there is no target."""
        self.value = value
        target = self.target()
        for device, sock in list(self.sockets.items()):
            if target is None or device == target:
                try:
                    sock.send(value)
                except OSError as error:
                    logger.warning("dropping %s from %s: %s", device, self.path, error)
                    # Not from here: dropping tells the bridge, which may be sending this very report.
                    asyncio.get_running_loop().call_soon(self._drop, device, sock)

    @dbus_property(access=PropertyAccess.READ)
    def NotifyAcquired(self) -> DBusBool:
        return bool(self.sockets)

    @dbus_method()
    def AcquireNotify(self, options: Options) -> Annotated[list[Any], DBusSignature("hq")]:
        return list(self.acquire(options["device"].value, options["mtu"].value if "mtu" in options else 23))

    def acquire(self, device: str, mtu: int) -> tuple[int, int]:
        """Open a socket for `device` and return BlueZ's end with the MTU."""
        ours, theirs = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        if device in self.sockets:
            self._drop(device, self.sockets[device])
        self.sockets[device] = ours
        loop = asyncio.get_running_loop()
        # BlueZ never writes to a notify socket, so readable means it hung up.
        loop.add_reader(ours.fileno(), self._drop, device, ours)
        # shortcut: dbus_fast sends the fd after this returns, so our copy closes a little later
        # (BlueZ's hangup only reaches us once it is closed); hook into the send if 5 s ever races.
        loop.call_later(5, theirs.close)
        logger.info("host %s subscribed to %s", device, self.path)
        if self.listener:
            self.listener()
        return theirs.fileno(), mtu

    def _drop(self, device: str, sock: socket.socket) -> None:
        if sock.fileno() != -1:
            asyncio.get_running_loop().remove_reader(sock.fileno())
            sock.close()
        if self.sockets.get(device) is sock:
            del self.sockets[device]
            logger.info("host %s unsubscribed from %s", device, self.path)
            if self.listener:
                self.listener()


class GattDescriptor(ServiceInterface):
    def __init__(self, characteristic: GattCharacteristic, uuid: str, value: bytes) -> None:
        super().__init__("org.bluez.GattDescriptor1")
        self.path = f"{characteristic.path}/desc0"
        self._characteristic = characteristic.path
        self._uuid = uuid
        self._value = value

    @dbus_property(access=PropertyAccess.READ)
    def UUID(self) -> DBusStr:
        return self._uuid

    @dbus_property(access=PropertyAccess.READ)
    def Characteristic(self) -> DBusObjectPath:
        return self._characteristic

    @dbus_property(access=PropertyAccess.READ)
    def Flags(self) -> Strings:
        return SECURE_READ

    @dbus_method()
    def ReadValue(self, options: Options) -> DBusBytes:
        return _read(self._value, options)


type GattObject = GattService | GattCharacteristic | GattDescriptor


class Advertisement(ServiceInterface):
    def __init__(self, name: str, discoverable: bool) -> None:
        super().__init__("org.bluez.LEAdvertisement1")
        self._name = name
        self._discoverable = discoverable

    @dbus_property(access=PropertyAccess.READ)
    def Type(self) -> DBusStr:
        return "peripheral"

    @dbus_property(access=PropertyAccess.READ)
    def ServiceUUIDs(self) -> Strings:
        return ["1812"]

    @dbus_property(access=PropertyAccess.READ)
    def LocalName(self) -> DBusStr:
        return self._name

    @dbus_property(access=PropertyAccess.READ)
    def Appearance(self) -> DBusUInt16:
        return APPEARANCE_KEYBOARD

    @dbus_property(access=PropertyAccess.READ)
    def Discoverable(self) -> DBusBool:
        return self._discoverable

    @dbus_method()
    def Release(self) -> None:
        logger.info("advertisement released by BlueZ")


class Agent(ServiceInterface):
    """Pairing agent: numeric comparison through `confirm`, or refuse when it is None.

    Being the default agent matters even when refusing: with no agent, BlueZ
    accepts Just Works pairing from anyone in range.
    """

    def __init__(self) -> None:
        super().__init__("org.bluez.Agent1")
        self.confirm: Confirm | None = None
        self.on_accept: Callable[[str], None] = lambda device: None

    async def _confirm(self, question: str) -> None:
        if self.confirm is None:
            logger.warning("refused pairing (%s); not in pairing mode", question)
        elif await self.confirm(question):
            return
        raise DBusError("org.bluez.Error.Rejected", "pairing rejected")

    @dbus_method()
    async def RequestConfirmation(self, device: DBusObjectPath, passkey: Annotated[int, DBusSignature("u")]) -> None:
        await self._confirm(f"Pair with {device}? The device must show {passkey:06d}.")
        self.on_accept(device)

    @dbus_method()
    async def RequestAuthorization(self, device: DBusObjectPath) -> None:
        await self._confirm(f"Allow {device} to pair?")
        self.on_accept(device)

    @dbus_method()
    def AuthorizeService(self, device: DBusObjectPath, uuid: DBusStr) -> None:
        pass

    @dbus_method()
    def Cancel(self) -> None:
        logger.info("pairing cancelled")

    @dbus_method()
    def Release(self) -> None:
        pass


class Peripheral:
    """HID-over-GATT keyboard and mouse registered on a BlueZ adapter."""

    def __init__(self, name: str, adapter: str = "hci0") -> None:
        self.name = name
        self.adapter_path = f"/org/bluez/{adapter}"
        self._bus: MessageBus | None = None
        self._agent = Agent()
        self._objects: list[GattObject] = []
        self.on_subscription_change: Callable[[], None] = lambda: None
        self.hosts = Hosts(self.adapter_path)
        self.hosts.on_change = lambda: self.on_subscription_change()
        self.hosts.on_connect = self._host_connected
        self._advertising = False
        self._register_lock = asyncio.Lock()
        self._spare_exported = False
        self._registered_at = -RESUBSCRIBE_SECONDS
        self._tasks: set[asyncio.Task[None]] = set()
        self._agent.on_accept = self.hosts.accepted
        self.keyboard, self.mouse = self._build()

    def _add[T: GattObject](self, obj: T) -> T:
        self._objects.append(obj)
        return obj

    def _build(self) -> tuple[ReportCharacteristic, ReportCharacteristic]:
        hid = self._add(GattService(f"{ROOT}/service0", "1812"))
        self._add(
            GattCharacteristic(hid, 0, "2a4a", ["read"], bytes([0x11, 0x01, 0x00, 0x02]))
        )  # HID 1.11, normally connectable
        self._add(GattCharacteristic(hid, 1, "2a4b", SECURE_READ, REPORT_MAP))
        self._add(GattCharacteristic(hid, 2, "2a4c", ["write-without-response"]))
        reports = []
        for index, report_id in enumerate((KEYBOARD_ID, MOUSE_ID), start=3):
            report = self._add(ReportCharacteristic(hid, index))
            self._add(GattDescriptor(report, "2908", bytes([report_id, 1])))  # Report Reference: input
            report.listener = lambda: self.on_subscription_change()
            report.target = self._target
            reports.append(report)
        battery = self._add(GattService(f"{ROOT}/service1", "180f"))
        self._add(GattCharacteristic(battery, 0, "2a19", ["read", "notify"], bytes([100])))
        info = self._add(GattService(f"{ROOT}/service2", "180a"))
        # PnP ID: USB vendor space, Linux Foundation VID 0x1d6b, arbitrary PID and version.
        self._add(GattCharacteristic(info, 0, "2a50", ["read"], bytes([0x02, 0x6B, 0x1D, 0x46, 0x42, 0x00, 0x01])))
        # Not part of the app at first; see _resubscribe.
        self._spare = GattCharacteristic(info, 1, "2a26", ["read"], b"bluff")
        return reports[0], reports[1]

    def _target(self) -> str | None:
        chosen = self.hosts.chosen
        return chosen.path if chosen else None

    @property
    def connected(self) -> bool:
        """Whether input sent now reaches a host: the chosen one, or any when none is chosen."""
        target = self._target()
        return any(target is None or device == target for device in self.keyboard.sockets)

    @property
    def pairing(self) -> bool:
        return self._agent.confirm is not None

    async def set_pairing(self, confirm: Confirm | None) -> None:
        """Become discoverable and accept pairing through `confirm`, or stop with None."""
        self._agent.confirm = confirm
        await self._update_advertising()
        logger.info("pairing mode %s", "on" if confirm else "off")

    def _host_connected(self, host: Host) -> None:
        task = asyncio.create_task(self._resubscribe(host))
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def _resubscribe(self, host: Host) -> None:
        # A bonded host keeps its subscription inside BlueZ across disconnects, so when it comes
        # back BlueZ sees nothing new and never calls AcquireNotify. Registering the app again
        # drops those subscriptions; connected hosts get Service Changed and subscribe again.
        await asyncio.sleep(RESUBSCRIBE_SECONDS)
        async with self._register_lock:
            if self._bus is None or not host.connected or host.path in self.keyboard.sockets:
                return
            # Hosts that came back together share one registration; a second toggle would undo it.
            if asyncio.get_running_loop().time() - self._registered_at < RESUBSCRIBE_SECONDS:
                return
            logger.info("%r is back without subscribing; registering again so it does", host.name)
            gatt = "org.bluez.GattManager1"
            await call(self._bus, "org.bluez", self.adapter_path, gatt, "UnregisterApplication", "o", [ROOT])
            # The same layout keeps the database hash, and bonded hosts then trust their old
            # subscriptions and write none. Toggling a spare characteristic changes the hash.
            if self._spare_exported:
                self._bus.unexport(self._spare.path)
            else:
                self._bus.export(self._spare.path, self._spare)
            self._spare_exported = not self._spare_exported
            await call(self._bus, "org.bluez", self.adapter_path, gatt, "RegisterApplication", "oa{sv}", [ROOT, {}])
            self._registered_at = asyncio.get_running_loop().time()

    async def _update_advertising(self) -> None:
        if self._bus is None:
            return
        manager = "org.bluez.LEAdvertisingManager1"
        if self._advertising:
            await call(self._bus, "org.bluez", self.adapter_path, manager, "UnregisterAdvertisement", "o",
                       [ADVERTISEMENT])  # fmt: skip
            self._bus.unexport(ADVERTISEMENT)
        self._bus.export(ADVERTISEMENT, Advertisement(self.name, discoverable=self.pairing))
        await call(self._bus, "org.bluez", self.adapter_path, manager, "RegisterAdvertisement", "oa{sv}",
                   [ADVERTISEMENT, {}])  # fmt: skip
        self._advertising = True

    async def start(self) -> None:
        bus = self._bus = await MessageBus(bus_type=BusType.SYSTEM, negotiate_unix_fd=True).connect()
        for obj in self._objects:
            bus.export(obj.path, obj)
        bus.export(AGENT, self._agent)

        await call(
            bus, "org.bluez", "/org/bluez", "org.bluez.AgentManager1", "RegisterAgent", "os", [AGENT, "DisplayYesNo"]
        )
        await call(bus, "org.bluez", "/org/bluez", "org.bluez.AgentManager1", "RequestDefaultAgent", "o", [AGENT])
        powered = ["org.bluez.Adapter1", "Powered", Variant("b", True)]
        await call(bus, "org.bluez", self.adapter_path, "org.freedesktop.DBus.Properties", "Set", "ssv", powered)
        await call(
            bus, "org.bluez", self.adapter_path, "org.bluez.GattManager1", "RegisterApplication", "oa{sv}", [ROOT, {}]
        )
        await self.hosts.start(bus)
        for host in self.hosts.hosts.values():
            if host.connected:
                self._host_connected(host)
        await self._update_advertising()
        logger.info("registered as %r on %s", self.name, self.adapter_path)
