from pathlib import Path
from typing import Any, cast

import pytest
from dbus_fast import Message, MessageType, Variant
from dbus_fast.aio import MessageBus

from bluff.hosts import DEVICE, Hosts

IPAD = "/org/bluez/hci0/dev_34_EE_16_4F_E0_BD"
PHONE = "/org/bluez/hci0/dev_8C_A3_EC_B6_17_1D"
HEADPHONES = "/org/bluez/hci0/dev_88_C9_E8_3C_59_49"


class FakeBus:
    """Answers GetAll with `devices` and records every Set."""

    def __init__(self, devices: dict[str, dict[str, Variant]]) -> None:
        self.devices = devices
        self.sets: list[tuple[str, str, bool]] = []

    async def call(self, message: Message) -> Message:
        if message.member == "Set":
            _, name, value = message.body
            self.sets.append((message.path or "", name, value.value))
        body: list[Any] = [self.devices.get(message.path or "", {})] if message.member == "GetAll" else []
        return Message(message_type=MessageType.METHOD_RETURN, reply_serial=1, body=body)


def device(address: str, alias: str, blocked: bool = False) -> dict[str, Variant]:
    return {
        "Address": Variant("s", address),
        "Alias": Variant("s", alias),
        "Blocked": Variant("b", blocked),
        "Connected": Variant("b", True),
    }


def changed(path: str, **props: Variant) -> Message:
    return Message(
        message_type=MessageType.SIGNAL,
        path=path,
        interface="org.freedesktop.DBus.Properties",
        member="PropertiesChanged",
        signature="sa{sv}as",
        body=[DEVICE, props, []],
    )


def make_hosts(tmp_path: Path, bus: FakeBus) -> Hosts:
    store = tmp_path / "hosts"
    store.write_text("34:EE:16:4F:E0:BD\n8C:A3:EC:B6:17:1D\n")
    hosts = Hosts("/org/bluez/hci0", store)
    hosts._bus = cast(MessageBus, bus)
    for path, props in bus.devices.items():
        hosts._update(path, props)
    return hosts


def test_only_stored_addresses_become_hosts(tmp_path: Path) -> None:
    bus = FakeBus({
        IPAD: device("34:EE:16:4F:E0:BD", "iPad"),
        PHONE: device("8C:A3:EC:B6:17:1D", "Fold", blocked=True),
        HEADPHONES: device("88:C9:E8:3C:59:49", "WH-1000XM5"),
    })  # fmt: skip
    hosts = make_hosts(tmp_path, bus)
    assert sorted(host.name for host in hosts.hosts.values()) == ["Fold", "iPad"]
    assert hosts.hosts[PHONE].blocked


@pytest.mark.asyncio
async def test_use_unblocks_the_choice_first_and_blocks_the_rest(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad"), PHONE: device("8C:A3:EC:B6:17:1D", "Fold", True)})
    hosts = make_hosts(tmp_path, bus)
    await hosts.use(PHONE)
    assert bus.sets == [(PHONE, "Blocked", False), (IPAD, "Blocked", True)]
    assert hosts.hosts[IPAD].blocked and not hosts.hosts[PHONE].blocked


def test_signals_update_known_hosts_only(tmp_path: Path) -> None:
    bus = FakeBus({IPAD: device("34:EE:16:4F:E0:BD", "iPad")})
    hosts = make_hosts(tmp_path, bus)
    changes: list[None] = []
    hosts.on_change = lambda: changes.append(None)
    hosts._on_signal(changed(IPAD, Connected=Variant("b", False)))
    hosts._on_signal(changed(HEADPHONES, Connected=Variant("b", False)))
    assert not hosts.hosts[IPAD].connected
    assert HEADPHONES not in hosts.hosts
    assert len(changes) == 1


@pytest.mark.asyncio
async def test_new_pairing_is_stored_and_used(tmp_path: Path) -> None:
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
    assert bus.sets == [(PHONE, "Blocked", False), (IPAD, "Blocked", True)]
