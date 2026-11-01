from bluff.evdev import ABS_X, ABS_Y, BTN_TOOL_DOUBLETAP, BTN_TOOL_FINGER, BTN_TOUCH, EV_ABS, EV_KEY
from bluff.touchpad import Touchpad


def frame(pad: Touchpad, x: int, y: int) -> tuple[int, int, int, int]:
    pad.event(EV_ABS, ABS_X, x)
    pad.event(EV_ABS, ABS_Y, y)
    return pad.sync()


def test_one_finger_moves_without_jump_on_touch() -> None:
    pad = Touchpad(resolution=10, pixels_per_mm=10, mm_per_tick=2)
    pad.event(EV_KEY, BTN_TOUCH, 1)
    pad.event(EV_KEY, BTN_TOOL_FINGER, 1)
    assert frame(pad, 500, 500) == (0, 0, 0, 0)
    assert frame(pad, 510, 495) == (10, -5, 0, 0)


def test_two_fingers_scroll_and_right_click() -> None:
    pad = Touchpad(resolution=10, pixels_per_mm=10, mm_per_tick=2)
    pad.event(EV_KEY, BTN_TOOL_DOUBLETAP, 1)
    assert pad.click_button() == 1
    frame(pad, 100, 100)
    assert frame(pad, 100, 60) == (0, 0, 2, 0)


def test_fractions_carry_over() -> None:
    pad = Touchpad(resolution=10, pixels_per_mm=1, mm_per_tick=2)
    pad.event(EV_KEY, BTN_TOOL_FINGER, 1)
    frame(pad, 0, 0)
    assert frame(pad, 6, 0)[0] == 0
    assert frame(pad, 12, 0)[0] == 1
