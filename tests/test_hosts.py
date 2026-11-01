from pathlib import Path
from typing import Any, cast

import pytest
from dbus_fast import Message, MessageType, Variant
from dbus_fast.aio import MessageBus

from bluff.hosts import DEVICE, Host, Hosts

IPAD = "/org/bluez/hci0/dev_34_EE_16_4F_E0_BD"
PHONE = "/org/bluez/hci0/dev_8C_A3_EC_B6_17_1D"
HEADPHONES = "/org/bluez/hci0/dev_88_C9_E8_3C_59_49"


class FakeBus:
    """Answers GetAll with `devices` and records every other call as (path, member)."""

    def __init__(self, devices: dict[str, dict[str, Variant]]) -> None:
        self.devices = devices
        self.calls: list[tuple[str, str]] = []

    async def call(self, message: Message) -> Message:
        body: list[Any] = []
        if message.member == "GetAll":
            body = [self.devices.get(message.path or "", {})]
        else:
            self.calls.append((message.path or "", message.member or ""))
        return Message(message_type=MessageType.METHOD_RETURN, reply_serial=1, body=body)


def device(address: str, alias: str, connected: bool = True) -> dict[str, Variant]:
    return {"Address": Variant("s", address), "Alias": Variant("s", alias), "Connected": Variant("b", connected)}


def changed(path: str, **props: Variant) -> Message:
    return Message(
        message_type=MessageType.SIGNAL,
        path=path,
        interface="org.freedesktop.DBus.Properties",
        member="PropertiesChanged",
        signature="sa{sv}as",
        body=[DEVICE, props, []],
    )


def make_hosts(tmp_path: Path, bus: FakeBus, chosen: str = "") -> Hosts:
    store = tmp_path / "hosts"
    store.write_text("34:EE:16:4F:E0:BD\n8C:A3:EC:B6:17:1D\n")
    if chosen:
        (tmp_path / "chosen").write_text(f"{chosen}\n")
    hosts = Hosts("/org/bluez/hci0", store)
    hosts._bus = cast(MessageBus, bus)
    for path, props in bus.devices.items():
        hosts._update(path, props)
    return hosts


def test_only_stored_addresses_become_hosts(tmp_path: Path) -> None:
    bus = FakeBus({
        IPAD: device("34:EE:16:4F:E0:BD", "iPad"),
        PHONE: device("8C:A3:EC:B6:17:1D", "Fold"),
        HEADPHONES: device("88:C9:E8:3C:59:49", "WH-1000XM5"),
    })  # fmt: skip
    hosts = make_hosts(tmp_path, bus)
    assert sorted(host.name for host in hosts.hosts.values()) == ["Fold", "iPad"]


@pytest.mark.asyncio
async def test_choosing_a_connected_host_disconnects_the_others(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad"), PHONE: device("8C:A3:EC:B6:17:1D", "Fold")})
    hosts = make_hosts(tmp_path, bus)
    await hosts.use(PHONE)
    assert bus.calls == [(IPAD, "Disconnect")]
    assert (tmp_path / "chosen").read_text() == "8C:A3:EC:B6:17:1D\n"


@pytest.mark.asyncio
async def test_the_old_host_stays_until_the_chosen_one_connects(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad"), PHONE: device("8C:A3:EC:B6:17:1D", "Fold", False)})
    hosts = make_hosts(tmp_path, bus)
    await hosts.use(PHONE)
    assert bus.calls == []
    assert not hosts.settled

    hosts._on_signal(changed(PHONE, Connected=Variant("b", True)))
    await next(iter(hosts._tasks))
    assert bus.calls == [(IPAD, "Disconnect")]
    assert hosts.settled


@pytest.mark.asyncio
async def test_signals_update_known_hosts_only(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad")})
    hosts = make_hosts(tmp_path, bus, chosen="34:EE:16:4F:E0:BD")
    changes: list[None] = []
    hosts.on_change = lambda: changes.append(None)
    hosts._on_signal(changed(IPAD, Connected=Variant("b", False)))
    hosts._on_signal(changed(HEADPHONES, Connected=Variant("b", False)))
    assert hosts.chosen == Host(IPAD, "34:EE:16:4F:E0:BD", "iPad", False)
    assert HEADPHONES not in hosts.hosts
    assert len(changes) == 1


@pytest.mark.asyncio
async def test_new_pairing_is_stored_and_chosen(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad"), PHONE: device("8C:A3:EC:B6:17:1D", "Fold")})
    store = tmp_path / "state" / "hosts"
    hosts = Hosts("/org/bluez/hci0", store)
    hosts._bus = cast(MessageBus, bus)
    hosts.addresses.add("34:EE:16:4F:E0:BD")
    hosts._update(IPAD, bus.devices[IPAD])

    hosts.accepted(PHONE)
    hosts._on_signal(changed(PHONE, Paired=Variant("b", True)))
    await next(iter(hosts._tasks))

    assert store.read_text() == "34:EE:16:4F:E0:BD\n8C:A3:EC:B6:17:1D\n"
    assert hosts.chosen_address == "8C:A3:EC:B6:17:1D"
    assert bus.calls == [(IPAD, "Disconnect")]
