import asyncio

from bluff.ble import Peripheral
from bluff.bridge import Bridge
from bluff.tray import Item, Tray


def make_tray() -> tuple[Tray, Peripheral, asyncio.Event]:
    peripheral = Peripheral("test")
    quit = asyncio.Event()
    return Tray(Bridge(peripheral, frozenset({1}), 10, 2), quit), peripheral, quit


def test_icon_and_menu_follow_connection() -> None:
    tray, peripheral, _ = make_tray()
    assert tray.icon() == "bluetooth-disabled"
    toggle = dict(tray.menu())[Item.TOGGLE]
    assert toggle["enabled"] == ("b", False)

    peripheral.mouse.notifying = True
    assert tray.icon() == "input-keyboard"
    assert dict(tray.menu())[Item.TOGGLE]["enabled"] == ("b", True)


def test_quit_item_sets_event() -> None:
    tray, _, quit = make_tray()
    tray.clicked(Item.QUIT)
    assert quit.is_set()
