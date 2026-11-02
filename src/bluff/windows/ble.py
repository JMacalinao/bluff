"""WinRT GATT server for a HID-over-GATT peripheral. Windows runs pairing itself."""

import asyncio
import logging
import os
from collections.abc import Callable, Coroutine
from pathlib import Path
from typing import Any
from uuid import UUID

from winrt.windows.devices.bluetooth import BluetoothLEDevice
from winrt.windows.devices.bluetooth.genericattributeprofile import (
    GattCharacteristicProperties,
    GattCommunicationStatus,
    GattLocalCharacteristic,
    GattLocalCharacteristicParameters,
    GattLocalDescriptorParameters,
    GattLocalService,
    GattProtectionLevel,
    GattServiceProvider,
    GattServiceProviderAdvertisementStatus,
    GattServiceProviderAdvertisingParameters,
    GattSubscribedClient,
)
from winrt.windows.storage.streams import DataWriter, IBuffer

from bluff.hid import KEYBOARD_ID, MOUSE_ID, REPORT_MAP

logger = logging.getLogger(__name__)

STORE = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local") / "bluff" / "hosts"
# Report reads need an encrypted link, which is what makes iPadOS bond.
SECURE = GattProtectionLevel.ENCRYPTION_REQUIRED


def _uuid(short: int) -> UUID:
    return UUID(f"0000{short:04x}-0000-1000-8000-00805f9b34fb")


def _buffer(data: bytes) -> IBuffer:
    writer = DataWriter()
    writer.write_bytes(data)
    return writer.detach_buffer()


def address(client: GattSubscribedClient) -> str:
    """The host's address, from a device id like ``BluetoothLE#BluetoothLE<ours>-<host>``."""
    return client.session.device_id.id.rpartition("-")[2]


async def _characteristic(
    service: GattLocalService,
    short: int,
    properties: GattCharacteristicProperties,
    value: bytes | None = None,
    protection: GattProtectionLevel = GattProtectionLevel.PLAIN,
) -> GattLocalCharacteristic:
    parameters = GattLocalCharacteristicParameters()
    parameters.characteristic_properties = properties
    parameters.read_protection_level = protection
    if value is not None:
        parameters.static_value = _buffer(value)
    result = await service.create_characteristic_async(_uuid(short), parameters)
    if result.characteristic is None:
        raise RuntimeError(f"could not create characteristic {short:04x}: {result.error}")
    return result.characteristic


async def _provider(short: int) -> GattServiceProvider:
    result = await GattServiceProvider.create_async(_uuid(short))
    if result.service_provider is None:
        raise RuntimeError(f"could not create service {short:04x}: {result.error}")
    return result.service_provider


class Report:
    """Input report sent to the chosen host only, or to every subscribed host when none is chosen."""

    def __init__(self, characteristic: GattLocalCharacteristic, target: Callable[[], str | None]) -> None:
        self.characteristic = characteristic
        self._target = target
        self._queue: asyncio.Queue[tuple[IBuffer, GattSubscribedClient]] = asyncio.Queue()
        self._sender: asyncio.Task[None] | None = None

    def addresses(self) -> list[str]:
        return [address(client) for client in self.characteristic.subscribed_clients]

    def notify(self, value: bytes) -> None:
        target = self._target()
        for client in self.characteristic.subscribed_clients:
            if target is None or address(client) == target:
                self._queue.put_nowait((_buffer(value), client))
        if self._sender is None:
            self._sender = asyncio.create_task(self._send())

    async def _send(self) -> None:
        # One notification at a time: WinRT may finish concurrent ones out of order, and a
        # key release that overtakes its press leaves the key held on the host.
        while True:
            value, client = await self._queue.get()
            result = await self.characteristic.notify_value_async(value, client)
            if result.status != GattCommunicationStatus.SUCCESS:
                logger.warning("report to %s failed: %s", address(client), result.status)


class Peripheral:
    """HID-over-GATT keyboard and mouse published through WinRT.

    Windows owns the adapter's name and pairing: hosts see the computer name,
    and Windows asks the user to confirm a new pairing. Outside pairing mode
    bluff advertises without being discoverable, so only bonded hosts find it.
    Bluff remembers the hosts that subscribed in `store`; a host subscribing
    for the first time gets the input from then on.
    """

    def __init__(self, store: Path = STORE) -> None:
        self._store = store
        self._chosen_store = store.with_name("chosen")
        self.addresses = set(store.read_text().split()) if store.exists() else set()
        self.chosen_address = self._chosen_store.read_text().strip() if self._chosen_store.exists() else ""
        self.names: dict[str, str] = {}
        self.pairing = False
        self.on_subscription_change: Callable[[], None] = lambda: None
        self._providers: list[GattServiceProvider] = []
        self._tasks: set[asyncio.Task[None]] = set()
        self._reports: list[Report] = []

    @property
    def keyboard(self) -> Report:
        return self._reports[0]

    @property
    def mouse(self) -> Report:
        return self._reports[1]

    @property
    def hosts(self) -> list[str]:
        """Addresses of the hosts subscribed now."""
        return self.keyboard.addresses()

    def _target(self) -> str | None:
        return self.chosen_address or None

    @property
    def connected(self) -> bool:
        """Whether input sent now reaches a host: the chosen one, or any when none is chosen."""
        target = self._target()
        return any(target is None or host == target for host in self.hosts)

    def use(self, host: str) -> None:
        """Send input only to the host with address `host` from now on."""
        self.chosen_address = host
        self._chosen_store.parent.mkdir(parents=True, exist_ok=True)
        self._chosen_store.write_text(f"{host}\n")
        logger.info("input is for %r from now on", self.names.get(host, host))
        self.on_subscription_change()

    async def set_pairing(self, on: bool) -> None:
        """Advertise as discoverable so a new host can pair, or stop."""
        self.pairing = on
        self._advertise()
        logger.info("pairing mode %s", "on" if on else "off")

    def _advertise(self) -> None:
        parameters = GattServiceProviderAdvertisingParameters()
        parameters.is_connectable = True
        parameters.is_discoverable = self.pairing
        for provider in self._providers:
            # Stopping a provider that never started raises.
            if provider.advertisement_status != GattServiceProviderAdvertisementStatus.CREATED:
                provider.stop_advertising()
            provider.start_advertising(parameters)

    async def start(self) -> None:
        hid = await _provider(0x1812)
        service = hid.service
        properties = GattCharacteristicProperties
        await _characteristic(service, 0x2A4A, properties.READ, bytes([0x11, 0x01, 0x00, 0x02]))  # HID 1.11
        await _characteristic(service, 0x2A4B, properties.READ, REPORT_MAP, SECURE)
        await _characteristic(service, 0x2A4C, properties.WRITE_WITHOUT_RESPONSE)
        loop = asyncio.get_running_loop()
        for report_id, size in ((KEYBOARD_ID, 8), (MOUSE_ID, 7)):
            report = await _characteristic(service, 0x2A4D, properties.READ | properties.NOTIFY, bytes(size), SECURE)
            reference = GattLocalDescriptorParameters()
            reference.static_value = _buffer(bytes([report_id, 1]))  # Report Reference: input
            reference.read_protection_level = SECURE
            await report.create_descriptor_async(_uuid(0x2908), reference)
            # WinRT raises this on one of its own threads.
            report.add_subscribed_clients_changed(lambda *_: loop.call_soon_threadsafe(self._clients_changed))
            self._reports.append(Report(report, self._target))
        battery = await _provider(0x180F)
        await _characteristic(battery.service, 0x2A19, properties.READ, bytes([100]))
        # Device Information (180a) is Windows' own; it refuses to publish another.
        self._providers = [hid, battery]
        self._advertise()
        logger.info("advertising as a keyboard and mouse")

    def _clients_changed(self) -> None:
        for host in self.hosts:
            if host not in self.names:
                self.names[host] = host
                self._spawn(self._name(host))
            if host not in self.addresses:
                self.addresses.add(host)
                self._store.parent.mkdir(parents=True, exist_ok=True)
                self._store.write_text("".join(f"{a}\n" for a in sorted(self.addresses)))
                logger.info("new host %s", host)
                self.use(host)
        logger.info("subscribed hosts: %s", ", ".join(self.names[host] for host in self.hosts) or "none")
        self.on_subscription_change()

    async def _name(self, host: str) -> None:
        client = next((c for c in self.keyboard.characteristic.subscribed_clients if address(c) == host), None)
        if client is None:
            return
        device = await BluetoothLEDevice.from_id_async(client.session.device_id.id)
        if device is not None and device.name:
            self.names[host] = device.name
            self.on_subscription_change()

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
