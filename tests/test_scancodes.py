from bluff.keymap import KEYCODES
from bluff.windows.scancodes import LLKHF_EXTENDED, VK_NUMLOCK, VK_PAUSE, keycode, wheel_ticks


def test_plain_scan_codes_are_linux_keycodes() -> None:
    assert keycode(0x1E, 0, 0x41) == KEYCODES["A"]
    assert keycode(0x58, 0, 0x7B) == KEYCODES["F12"]
    assert keycode(0x70, 0, 0) is None


def test_extended_scan_codes() -> None:
    assert keycode(0x5B, LLKHF_EXTENDED, 0x5B) == KEYCODES["LEFTMETA"]
    assert keycode(0x48, LLKHF_EXTENDED, 0x26) == KEYCODES["UP"]
    assert keycode(0x48, 0, 0x68) == KEYCODES["KP8"]
    assert keycode(0x2A, LLKHF_EXTENDED, 0x10) is None  # fake shift around NumLock-off navigation keys


def test_numlock_and_pause_swap_back() -> None:
    assert keycode(0x45, LLKHF_EXTENDED, VK_NUMLOCK) == KEYCODES["NUMLOCK"]
    assert keycode(0x45, 0, VK_PAUSE) == KEYCODES["PAUSE"]


def test_wheel_ticks_keep_the_remainder() -> None:
    assert wheel_ticks(120) == (1, 0)
    assert wheel_ticks(50) == (0, 50)
    assert wheel_ticks(250) == (2, 10)
    assert wheel_ticks(-130) == (-1, -10)
