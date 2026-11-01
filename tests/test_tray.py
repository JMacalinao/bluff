import asyncio

from bluff.ble import Peripheral
from bluff.bridge import Bridge
from bluff.tray import Item, Tray, pixmaps


def make_tray() -> tuple[Tray, Peripheral, asyncio.Event]:
    peripheral = Peripheral("test")
    quit = asyncio.Event()
    return Tray(Bridge(peripheral, frozenset({1}), 10, 2), quit), peripheral, quit


def test_icon_and_menu_follow_connection() -> None:
    tray, peripheral, _ = make_tray()
    assert tray.icon() == "disconnected"
    toggle = dict(tray.menu())[Item.TOGGLE]
    assert toggle["enabled"] == ("b", False)

    peripheral.mouse.notifying = True
    assert tray.icon() == "laptop"
    assert dict(tray.menu())[Item.TOGGLE]["enabled"] == ("b", True)


def test_quit_item_sets_event() -> None:
    tray, _, quit = make_tray()
    tray.clicked(Item.QUIT)
    assert quit.is_set()


def test_every_state_has_pixmaps() -> None:
    for state in ("disconnected", "laptop", "ipad", "pairing"):
        frames = pixmaps(state)
        assert [w for w, _, _ in frames] == [16, 22, 24, 32, 48, 64]
        assert all(len(data) == w * h * 4 for w, h, data in frames)
