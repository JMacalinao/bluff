from typing import cast

import pytest
from dbus_fast import DBusError, Message
from dbus_fast.aio import MessageBus

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
async def test_advertises_only_while_no_known_host_is_connected() -> None:
    calls: list[str] = []

    class Bus:
        async def call(self, message: Message) -> None:
            calls.append(message.member or "")

        def export(self, path: str, interface: object) -> None:
            pass

        def unexport(self, path: str) -> None:
            pass

    peripheral = Peripheral("test")
    peripheral._bus = cast(MessageBus, Bus())
    await peripheral._update_advertising()
    assert calls == ["RegisterAdvertisement"]

    peripheral.hosts.hosts = {"/a": Host("/a", "iPad", blocked=False, connected=True)}
    await peripheral._update_advertising()
    await peripheral._update_advertising()
    assert calls == ["RegisterAdvertisement", "UnregisterAdvertisement"]

    peripheral.hosts.hosts["/a"].connected = False
    await peripheral._update_advertising()
    assert calls[-1] == "RegisterAdvertisement"
