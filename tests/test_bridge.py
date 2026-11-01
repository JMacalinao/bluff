import socket
from pathlib import Path
from typing import cast

import pytest

from bluff.ble import Peripheral
from bluff.bridge import Bridge
from bluff.evdev import EV_KEY, Device, DeviceInfo, Kind
from bluff.keymap import KEYCODES


class FakeDevice:
    def __init__(self) -> None:
        self.info = DeviceInfo("kbd", Path("/dev/null"), Kind.KEYBOARD)
        self.grabbed = False

    def grab(self, on: bool) -> None:
        self.grabbed = on


def press(bridge: Bridge, device: FakeDevice, *names: str) -> None:
    for name in names:
        bridge._event(cast(Device, device), EV_KEY, KEYCODES[name], 1)
    for name in names:
        bridge._event(cast(Device, device), EV_KEY, KEYCODES[name], 0)


@pytest.mark.asyncio
async def test_hotkey_toggles_after_release_only_when_subscribed() -> None:
    peripheral = Peripheral("test")
    bridge = Bridge(peripheral, frozenset({KEYCODES["LEFTCTRL"], KEYCODES["LEFTALT"]}), 10, 2)
    device = FakeDevice()
    bridge._devices.append(cast(Device, device))

    press(bridge, device, "LEFTCTRL", "LEFTALT")
    assert not bridge.active

    peripheral.keyboard.sockets["/a"] = cast(socket.socket, None)
    press(bridge, device, "LEFTCTRL", "LEFTALT")
    assert bridge.active and device.grabbed

    peripheral.keyboard.sockets.clear()
    peripheral.on_subscription_change()
    assert not bridge.active and not device.grabbed
