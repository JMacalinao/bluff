import pytest
from dbus_fast import DBusError

from bluff.ble import Agent


@pytest.mark.asyncio
async def test_agent_refuses_pairing_outside_pairing_mode() -> None:
    with pytest.raises(DBusError):
        await Agent(pairing=False)._confirm("Pair with test?")
