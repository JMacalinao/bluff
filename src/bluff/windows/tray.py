"""Notification-area icon: shows where input goes, switches on click, and runs pairing mode."""

import asyncio
import logging
import threading
from collections.abc import Callable, Coroutine, Iterator
from functools import cache
from typing import Any

import pystray
from PIL import Image

from bluff.bridge import Bridge
from bluff.icons import pixmaps
from bluff.windows.ble import Peripheral

logger = logging.getLogger(__name__)

PAIRING_SECONDS = 120


@cache
def image(state: str) -> Image.Image:
    """The largest frame of a tray icon; Windows scales it to the notification area."""
    width, height, argb = max(pixmaps(state), key=lambda frame: frame[0])
    rgba = bytearray(len(argb))
    rgba[0::4], rgba[1::4], rgba[2::4], rgba[3::4] = argb[1::4], argb[2::4], argb[3::4], argb[0::4]
    return Image.frombytes("RGBA", (width, height), bytes(rgba))


class Tray:
    """The icon runs on its own thread; its menu reads state there and hands clicks to the event loop."""

    def __init__(self, bridge: Bridge[Peripheral], quit: asyncio.Event) -> None:
        self.bridge = bridge
        self.peripheral = bridge.peripheral
        self._quit = quit
        self._loop = asyncio.get_running_loop()
        self._pairing_timer: asyncio.TimerHandle | None = None
        self._tasks: set[asyncio.Task[None]] = set()
        self._icon = pystray.Icon("bluff", image(self.icon()), "bluff", pystray.Menu(self._items))

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

    def _items(self) -> Iterator[pystray.MenuItem]:
        can_switch = self.peripheral.connected and not self.bridge.active
        yield pystray.MenuItem(self.summary(), None, enabled=False)
        # The default item is what a left click on the icon does.
        yield pystray.MenuItem("Switch to device", self._later(self._switch), enabled=can_switch, default=True)
        for host in self.peripheral.hosts:
            yield pystray.MenuItem(
                self.peripheral.names.get(host, host),
                self._later(lambda host=host: self.peripheral.use(host)),
                checked=lambda _, host=host: host == self.peripheral.chosen_address,
                radio=True,
            )
        pair = "Stop pairing" if self.peripheral.pairing else "Pair a new device…"
        yield pystray.MenuItem(pair, self._later(lambda: self._spawn(self._set_pairing(not self.peripheral.pairing))))
        yield pystray.MenuItem("Quit bluff", self._later(self._quit.set))

    def _later(self, action: Callable[[], object]) -> Callable[[], object]:
        return lambda: self._loop.call_soon_threadsafe(action)

    def _switch(self) -> None:
        if self.peripheral.connected and not self.bridge.active:
            self.bridge.set_active(True)

    def refresh(self) -> None:
        self._icon.icon = image(self.icon())
        self._icon.title = f"bluff: {self.summary()}"
        self._icon.update_menu()

    async def _set_pairing(self, on: bool) -> None:
        if self._pairing_timer:
            self._pairing_timer.cancel()
            self._pairing_timer = None
        await self.peripheral.set_pairing(on)
        if on:
            self._pairing_timer = self._loop.call_later(PAIRING_SECONDS, lambda: self._spawn(self._set_pairing(False)))
        self.refresh()

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> None:
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    def start(self) -> None:
        self.bridge.on_change = self.refresh
        self._spawn(self._stop_on_quit())
        threading.Thread(target=self._icon.run, name="tray", daemon=True).start()
        self.refresh()

    async def _stop_on_quit(self) -> None:
        await self._quit.wait()
        self._icon.stop()
