import asyncio
import os
import socket

import pytest
from dbus_fast import DBusError

from bluff.ble import Agent, Peripheral
from bluff.hosts import Host


@pytest.mark.asyncio
async def test_agent_refuses_pairing_outside_pairing_mode() -> None:
    with pytest.raises(DBusError):
        await Agent()._confirm("Pair with test?")


@pytest.mark.asyncio
async def test_agent_asks_confirm_in_pairing_mode() -> None:
    agent = Agent()
    questions: list[str] = []

    async def no(question: str) -> bool:
        questions.append(question)
        return False

    agent.confirm = no
    with pytest.raises(DBusError):
        await agent._confirm("Pair with test?")
    assert questions == ["Pair with test?"]


@pytest.mark.asyncio
async def test_reports_go_only_to_the_chosen_host() -> None:
    peripheral = Peripheral("test")
    peripheral.hosts.hosts = {"/a": Host("/a", "A", "iPad", True), "/b": Host("/b", "B", "Fold", True)}
    peripheral.hosts.chosen_address = "B"
    ipad, ipad_bluez = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    fold, fold_bluez = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    peripheral.keyboard.sockets = {"/a": ipad, "/b": fold}
    ipad_bluez.setblocking(False)

    peripheral.keyboard.notify(b"\x01")
    assert fold_bluez.recv(8) == b"\x01"
    with pytest.raises(BlockingIOError):
        ipad_bluez.recv(8)


@pytest.mark.asyncio
async def test_acquire_notify_hands_out_a_socket_until_bluez_hangs_up() -> None:
    keyboard = Peripheral("test").keyboard
    fd, mtu = keyboard.acquire("/a", 64)
    assert mtu == 64
    bluez = socket.socket(fileno=os.dup(fd))
    keyboard.notify(b"\x02")
    assert bluez.recv(8) == b"\x02"

    bluez.close()
    os.close(fd)
    await asyncio.sleep(0.05)
    assert keyboard.sockets == {}


@pytest.mark.asyncio
async def test_a_failed_send_drops_the_host_after_the_send_returns() -> None:
    keyboard = Peripheral("test").keyboard
    changes: list[int] = []
    keyboard.listener = lambda: changes.append(len(keyboard.sockets))
    ours, bluez = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    keyboard.sockets["/a"] = ours
    bluez.close()

    keyboard.notify(b"\x01")
    assert "/a" in keyboard.sockets and changes == []
    await asyncio.sleep(0)
    assert keyboard.sockets == {} and changes == [0]
