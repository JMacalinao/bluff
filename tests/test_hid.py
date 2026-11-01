from bluff.hid import REPORT_MAP, Keyboard, mouse_report
from bluff.keymap import KEYCODES


def test_keyboard_report_tracks_modifiers_and_keys() -> None:
    kb = Keyboard()
    assert kb.key(KEYCODES["LEFTMETA"], True)
    assert kb.key(KEYCODES["H"], True)
    assert kb.report() == bytes([0x08, 0, 0x0B, 0, 0, 0, 0, 0])
    assert kb.key(KEYCODES["H"], False)
    assert kb.report() == bytes([0x08, 0, 0, 0, 0, 0, 0, 0])


def test_keyboard_ignores_unmapped_and_duplicate_presses() -> None:
    kb = Keyboard()
    assert not kb.key(0x2FF, True)
    assert kb.key(KEYCODES["A"], True)
    assert not kb.key(KEYCODES["A"], True)


def test_keyboard_reports_rollover_past_six_keys() -> None:
    kb = Keyboard()
    for name in "ABCDEFG":
        kb.key(KEYCODES[name], True)
    assert kb.report()[2:] == bytes([0x01] * 6)


def test_mouse_report_packs_and_clamps() -> None:
    assert mouse_report(0b101, -1, 40000, 3, -200) == bytes([0x05, 0xFF, 0xFF, 0xFF, 0x7F, 0x03, 0x81])


def test_report_map_collections_balance() -> None:
    assert REPORT_MAP.count(0xC0) >= 3
    assert len(REPORT_MAP) < 512
