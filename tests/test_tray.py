import asyncio

from bluff.ble import Peripheral
from bluff.bridge import Bridge
from bluff.hosts import Host
from bluff.tray import FIRST_HOST_ITEM, Item, Tray, pixmaps


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


def test_hosts_show_as_radio_items_and_switch_on_click() -> None:
    tray, peripheral, _ = make_tray()
    peripheral.hosts.hosts = {
        "/b": Host("/b", "iPad", blocked=True, connected=False),
        "/a": Host("/a", "Fold", blocked=False, connected=True),
    }
    used: list[str] = []

    async def use(path: str) -> None:
        used.append(path)

    peripheral.hosts.use = use
    items = dict(tray.menu())
    assert items[FIRST_HOST_ITEM]["label"] == ("s", "Fold")
    assert items[FIRST_HOST_ITEM]["toggle-state"] == ("i", 1)
    assert items[FIRST_HOST_ITEM + 1]["toggle-state"] == ("i", 0)

    async def click() -> None:
        tray.clicked(FIRST_HOST_ITEM + 1)
        await asyncio.gather(*tray._tasks)

    asyncio.run(click())
    assert used == ["/b"]
