"""Translate absolute touchpad events into relative mouse motion and scrolling."""

from bluff.evdev import (
    ABS_X,
    ABS_Y,
    BTN_TOOL_DOUBLETAP,
    BTN_TOOL_FINGER,
    BTN_TOOL_QUADTAP,
    BTN_TOOL_TRIPLETAP,
    BTN_TOUCH,
    EV_ABS,
    EV_KEY,
)

_FINGERS = {BTN_TOOL_FINGER: 1, BTN_TOOL_DOUBLETAP: 2, BTN_TOOL_TRIPLETAP: 3, BTN_TOOL_QUADTAP: 4}


class Touchpad:
    """One finger moves the pointer, two fingers scroll, a two-finger click is a right click.

    Parameters
    ----------
    resolution
        Device units per millimeter, from the ABS_X axis info.
    pixels_per_mm
        Pointer counts sent per millimeter of finger travel.
    mm_per_tick
        Finger travel per wheel tick when scrolling.
    """

    def __init__(self, resolution: int, pixels_per_mm: float, mm_per_tick: float) -> None:
        self._units_per_mm = resolution or 30  # shortcut: guess when the device reports none
        self._pixels_per_mm = pixels_per_mm
        self._mm_per_tick = mm_per_tick
        self.fingers = 0
        self._pos = [0, 0]
        self._last: list[int] | None = None
        self._carry = [0.0, 0.0]

    def event(self, type_: int, code: int, value: int) -> None:
        if type_ == EV_ABS and code in (ABS_X, ABS_Y):
            self._pos[code] = value
        elif type_ == EV_KEY and code in _FINGERS:
            if value:
                self.fingers = _FINGERS[code]
            elif self.fingers == _FINGERS[code]:
                self.fingers = 0
            self._restart()
        elif type_ == EV_KEY and code == BTN_TOUCH:
            self._restart()

    def click_button(self) -> int:
        """HID button bit for a physical click with the current finger count."""
        return 1 if self.fingers == 2 else 0

    def sync(self) -> tuple[int, int, int, int]:
        """Close an event frame; return (dx, dy, wheel, pan)."""
        last, self._last = self._last, list(self._pos)
        if last is None or self.fingers not in (1, 2):
            return 0, 0, 0, 0
        mm = [(self._pos[i] - last[i]) / self._units_per_mm for i in (0, 1)]
        if self.fingers == 1:
            dx, dy = self._take([m * self._pixels_per_mm for m in mm])
            return dx, dy, 0, 0
        pan, down = self._take([m / self._mm_per_tick for m in mm])
        return 0, 0, -down, pan

    def _take(self, amounts: list[float]) -> tuple[int, int]:
        """Emit whole counts and keep the fractions for the next frame."""
        whole = []
        for i, amount in enumerate(amounts):
            total = self._carry[i] + amount
            whole.append(int(total))
            self._carry[i] = total - int(total)
        return whole[0], whole[1]

    def _restart(self) -> None:
        self._last = None
        self._carry = [0.0, 0.0]
