"""Windows keyboard and wheel input in the units Linux input events use."""

LLKHF_EXTENDED = 0x01
VK_PAUSE, VK_NUMLOCK = 0x13, 0x90
WHEEL_DELTA = 120

# Scan codes with the E0 prefix; plain set-1 scan codes 1 to 88 already equal Linux keycodes.
_EXTENDED = {
    0x1C: 96,  # KPENTER
    0x1D: 97,  # RIGHTCTRL
    0x20: 113,  # MUTE
    0x2E: 114,  # VOLUMEDOWN
    0x30: 115,  # VOLUMEUP
    0x35: 98,  # KPSLASH
    0x37: 99,  # SYSRQ
    0x38: 100,  # RIGHTALT
    0x47: 102,  # HOME
    0x48: 103,  # UP
    0x49: 104,  # PAGEUP
    0x4B: 105,  # LEFT
    0x4D: 106,  # RIGHT
    0x4F: 107,  # END
    0x50: 108,  # DOWN
    0x51: 109,  # PAGEDOWN
    0x52: 110,  # INSERT
    0x53: 111,  # DELETE
    0x5B: 125,  # LEFTMETA
    0x5C: 126,  # RIGHTMETA
    0x5D: 127,  # COMPOSE
}


def keycode(scan: int, flags: int, vk: int) -> int | None:
    """The Linux keycode for a key the keyboard hook saw, None for keys bluff does not send."""
    # The hook swaps these two: NumLock arrives as E0 45, Pause as a plain 45.
    if vk == VK_NUMLOCK:
        return 69
    if vk == VK_PAUSE:
        return 119
    if flags & LLKHF_EXTENDED:
        return _EXTENDED.get(scan)
    return scan if 0 < scan <= 88 else None


def wheel_ticks(pending: int) -> tuple[int, int]:
    """Split a wheel delta into whole notches and the remainder, both keeping the sign."""
    ticks = int(pending / WHEEL_DELTA)
    return ticks, pending - ticks * WHEEL_DELTA
