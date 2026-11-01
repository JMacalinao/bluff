"""HID report descriptor and report builders for a combo keyboard and mouse."""

import struct

from bluff.keymap import USAGES

KEYBOARD_ID = 1
MOUSE_ID = 2

# fmt: off
REPORT_MAP = bytes([
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,  # Generic Desktop, Keyboard, Collection (Application)
    0x85, KEYBOARD_ID,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7,  # modifiers: 8 x 1 bit
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x75, 0x08, 0x95, 0x01, 0x81, 0x01,  # reserved byte
    0x15, 0x00, 0x26, 0xFF, 0x00,  # 6 key slots, usages 0-255
    0x19, 0x00, 0x2A, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00,
    0xC0,
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01,  # Generic Desktop, Mouse, Collection (Application)
    0x85, MOUSE_ID,
    0x09, 0x01, 0xA1, 0x00,  # Pointer, Collection (Physical)
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05,  # buttons 1-5
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x05, 0x81, 0x02,
    0x75, 0x03, 0x95, 0x01, 0x81, 0x01,  # padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31,  # X, Y: int16 relative
    0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06,
    0x09, 0x38,  # wheel: int8 relative
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0x05, 0x0C, 0x0A, 0x38, 0x02,  # Consumer AC Pan (horizontal scroll): int8 relative
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0xC0,
    0xC0,
])
# fmt: on

_MODIFIER_FIRST = 0xE0
_MAX_KEYS = 6
_ROLLOVER = bytes([0x01] * _MAX_KEYS)

MOUSE_BUTTONS = {0x110: 0, 0x111: 1, 0x112: 2, 0x113: 3, 0x114: 4}
"""Linux BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA to HID button bit."""


class Keyboard:
    """Pressed-key state that renders to a keyboard input report."""

    def __init__(self) -> None:
        self.modifiers = 0
        self.keys: list[int] = []

    def key(self, code: int, pressed: bool) -> bool:
        """Apply a Linux key event; return whether the report changed."""
        usage = USAGES.get(code)
        if usage is None:
            return False
        before = (self.modifiers, list(self.keys))
        if usage >= _MODIFIER_FIRST:
            bit = 1 << (usage - _MODIFIER_FIRST)
            self.modifiers = self.modifiers | bit if pressed else self.modifiers & ~bit
        elif pressed and usage not in self.keys:
            self.keys.append(usage)
        elif not pressed and usage in self.keys:
            self.keys.remove(usage)
        return before != (self.modifiers, self.keys)

    def report(self) -> bytes:
        if len(self.keys) > _MAX_KEYS:
            keys = _ROLLOVER
        else:
            keys = bytes(self.keys).ljust(_MAX_KEYS, b"\0")
        return bytes([self.modifiers, 0]) + keys

    def clear(self) -> None:
        self.modifiers = 0
        self.keys.clear()


def _clamp(value: int, limit: int) -> int:
    return max(-limit, min(limit, value))


def mouse_report(buttons: int, dx: int, dy: int, wheel: int = 0, pan: int = 0) -> bytes:
    """Pack a mouse input report; deltas outside the report range are clamped."""
    return struct.pack(
        "<Bhhbb",
        buttons,
        _clamp(dx, 32767),
        _clamp(dy, 32767),
        _clamp(wheel, 127),
        _clamp(pan, 127),
    )
