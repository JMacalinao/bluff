import pytest
from dbus_fast import DBusError

from bluff.ble import Agent


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
