"""Route laptop input either to the local compositor or to the iPad."""

import asyncio
import logging
from collections.abc import Callable

from bluff.ble import Peripheral
from bluff.evdev import BTN_LEFT, EV_KEY, EV_REL, EV_SYN, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, Device, DeviceInfo, Kind
from bluff.hid import MOUSE_BUTTONS, Keyboard, mouse_report
from bluff.touchpad import Touchpad

logger = logging.getLogger(__name__)

# BLE notifies at the connection interval anyway; coalescing keeps the queue short.
MOUSE_INTERVAL = 0.008
_KEY_REPEAT = 2
_REL_SLOTS = {REL_X: 0, REL_Y: 1, REL_WHEEL: 2, REL_HWHEEL: 3}


class Bridge:
    """Reads every input device; while active, grabs them and forwards to the peripheral.

    Parameters
    ----------
    peripheral
        The registered HID peripheral.
    hotkey
        Linux keycodes that toggle the target when held together and released.
    pixels_per_mm, mm_per_tick
        Touchpad scaling, see `Touchpad`.
    """

    def __init__(
        self, peripheral: Peripheral, hotkey: frozenset[int], pixels_per_mm: float, mm_per_tick: float
    ) -> None:
        self.peripheral = peripheral
        self.hotkey = hotkey
        self.active = False
        self._pixels_per_mm = pixels_per_mm
        self._mm_per_tick = mm_per_tick
        self._devices: list[Device] = []
        self._touchpads: dict[Device, Touchpad] = {}
        self._clicks: dict[Device, int] = {}
        self._held: set[int] = set()
        self._toggle_pending = False
        self._keyboard = Keyboard()
        self._buttons = 0
        self._motion = [0, 0, 0, 0]
        self._flush_handle: asyncio.TimerHandle | None = None
        self._last_flush = 0.0
        self.on_change: Callable[[], None] = lambda: None
        peripheral.on_subscription_change = self._subscription_changed

    def open(self, infos: list[DeviceInfo]) -> None:
        for info in infos:
            device = Device(info, self._event)
            if info.kind is Kind.TOUCHPAD:
                self._touchpads[device] = Touchpad(device.resolution(0), self._pixels_per_mm, self._mm_per_tick)
            self._devices.append(device)
            logger.info("using %s %r (%s)", info.kind, info.name, info.path)

    def _event(self, device: Device, type_: int, code: int, value: int) -> None:
        kind = device.info.kind
        if kind is Kind.KEYBOARD:
            if type_ == EV_KEY and value != _KEY_REPEAT:
                self._key(code, bool(value))
            return
        touchpad = self._touchpads.get(device)
        if touchpad:
            touchpad.event(type_, code, value)
        if type_ == EV_KEY and code == BTN_LEFT and touchpad:
            if value:
                bit = self._clicks[device] = touchpad.click_button()
            else:
                bit = self._clicks.pop(device, 0)
            self._button(bit, bool(value))
        elif type_ == EV_KEY and code in MOUSE_BUTTONS:
            self._button(MOUSE_BUTTONS[code], bool(value))
        elif type_ == EV_REL and code in _REL_SLOTS:
            self._motion[_REL_SLOTS[code]] += value
            self._schedule_flush()
        elif type_ == EV_SYN and touchpad:
            for slot, amount in enumerate(touchpad.sync()):
                self._motion[slot] += amount
            self._schedule_flush()

    def _key(self, code: int, pressed: bool) -> None:
        if pressed:
            self._held.add(code)
            self._toggle_pending |= self.hotkey <= self._held
        else:
            self._held.discard(code)
        if self.active and self._keyboard.key(code, pressed):
            self.peripheral.keyboard.notify(self._keyboard.report())
        # Switching only once everything is released keeps the compositor from seeing a stuck key.
        if self._toggle_pending and not self._held:
            self._toggle_pending = False
            self.set_active(not self.active)

    def _button(self, bit: int, pressed: bool) -> None:
        self._buttons = self._buttons | 1 << bit if pressed else self._buttons & ~(1 << bit)
        self._flush()

    def _schedule_flush(self) -> None:
        if self._flush_handle is None and self.active and any(self._motion):
            loop = asyncio.get_running_loop()
            delay = max(0.0, self._last_flush + MOUSE_INTERVAL - loop.time())
            self._flush_handle = loop.call_later(delay, self._flush)

    def _flush(self) -> None:
        if self._flush_handle:
            self._flush_handle.cancel()
            self._flush_handle = None
        self._last_flush = asyncio.get_running_loop().time()
        motion, self._motion = self._motion, [0, 0, 0, 0]
        if self.active:
            self.peripheral.mouse.notify(mouse_report(self._buttons, *motion))

    def set_active(self, active: bool) -> None:
        """Grab input for the iPad, or release it back to the laptop."""
        if active and not self.peripheral.connected:
            logger.warning("no iPad subscribed yet; staying on the laptop")
            return
        if not active:
            self._keyboard.clear()
            self._buttons = 0
            self.peripheral.keyboard.notify(self._keyboard.report())
            self._flush()
        for device in self._devices:
            device.grab(active)
        self.active = active
        logger.info("input now goes to %s", "the iPad" if active else "the laptop")
        self.on_change()

    def _subscription_changed(self) -> None:
        if self.active and not self.peripheral.connected:
            logger.warning("iPad went away; returning input to the laptop")
            self.set_active(False)
        self.on_change()
