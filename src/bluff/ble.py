"""BlueZ GATT server, advertisement and pairing agent for a HID-over-GATT peripheral."""

import logging
from collections.abc import Awaitable, Callable
from typing import Annotated

from dbus_fast import BusType, DBusError, Variant
from dbus_fast.aio import MessageBus
from dbus_fast.annotations import DBusBool, DBusBytes, DBusObjectPath, DBusSignature, DBusStr, DBusUInt16
from dbus_fast.constants import PropertyAccess
from dbus_fast.service import ServiceInterface, dbus_method, dbus_property

from bluff.bus import call
from bluff.hid import KEYBOARD_ID, MOUSE_ID, REPORT_MAP

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

    async def _confirm(self, question: str) -> None:
        if self.confirm is None:
            logger.warning("refused pairing (%s); not in pairing mode", question)
        elif await self.confirm(question):
            return
        raise DBusError("org.bluez.Error.Rejected", "pairing rejected")

    @dbus_method()
    async def RequestConfirmation(self, device: DBusObjectPath, passkey: Annotated[int, DBusSignature("u")]) -> None:
        await self._confirm(f"Pair with {device}? The device must show {passkey:06d}.")

    @dbus_method()
    async def RequestAuthorization(self, device: DBusObjectPath) -> None:
        await self._confirm(f"Allow {device} to pair?")

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
        self.keyboard, self.mouse = self._build()

    def _add[T: GattObject](self, obj: T) -> T:
        self._objects.append(obj)
        return obj

    def _build(self) -> tuple[GattCharacteristic, GattCharacteristic]:
        hid = self._add(GattService(f"{ROOT}/service0", "1812"))
        self._add(
            GattCharacteristic(hid, 0, "2a4a", ["read"], bytes([0x11, 0x01, 0x00, 0x02]))
        )  # HID 1.11, normally connectable
        self._add(GattCharacteristic(hid, 1, "2a4b", SECURE_READ, REPORT_MAP))
        self._add(GattCharacteristic(hid, 2, "2a4c", ["write-without-response"]))
        reports = []
        for index, report_id in enumerate((KEYBOARD_ID, MOUSE_ID), start=3):
            report = self._add(GattCharacteristic(hid, index, "2a4d", SECURE_NOTIFY))
            self._add(GattDescriptor(report, "2908", bytes([report_id, 1])))  # Report Reference: input
            report.listener = lambda: self.on_subscription_change()
            reports.append(report)
        battery = self._add(GattService(f"{ROOT}/service1", "180f"))
        self._add(GattCharacteristic(battery, 0, "2a19", ["read", "notify"], bytes([100])))
        info = self._add(GattService(f"{ROOT}/service2", "180a"))
        # PnP ID: USB vendor space, Linux Foundation VID 0x1d6b, arbitrary PID and version.
        self._add(GattCharacteristic(info, 0, "2a50", ["read"], bytes([0x02, 0x6B, 0x1D, 0x46, 0x42, 0x00, 0x01])))
        return reports[0], reports[1]

    @property
    def connected(self) -> bool:
        return self.keyboard.notifying or self.mouse.notifying

    @property
    def pairing(self) -> bool:
        return self._agent.confirm is not None

    async def set_pairing(self, confirm: Confirm | None) -> None:
        """Become discoverable and accept pairing through `confirm`, or stop with None."""
        self._agent.confirm = confirm
        if self._bus:
            await self._advertise(self._bus, replace=True)
        logger.info("pairing mode %s", "on" if confirm else "off")

    async def _advertise(self, bus: MessageBus, replace: bool) -> None:
        advertising = "org.bluez.LEAdvertisingManager1"
        if replace:
            await call(
                bus, "org.bluez", self.adapter_path, advertising, "UnregisterAdvertisement", "o", [ADVERTISEMENT]
            )
            bus.unexport(ADVERTISEMENT)
        bus.export(ADVERTISEMENT, Advertisement(self.name, discoverable=self.pairing))
        await call(
            bus, "org.bluez", self.adapter_path, advertising, "RegisterAdvertisement", "oa{sv}", [ADVERTISEMENT, {}]
        )

    async def start(self) -> None:
        bus = self._bus = await MessageBus(bus_type=BusType.SYSTEM).connect()
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
        await self._advertise(bus, replace=False)
        logger.info("advertising as %r on %s", self.name, self.adapter_path)
