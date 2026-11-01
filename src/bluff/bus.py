"""Plain D-Bus method calls without introspected proxies."""

from typing import Any

from dbus_fast import DBusError, Message, MessageType
from dbus_fast.aio import MessageBus


async def call(
    bus: MessageBus,
    destination: str,
    path: str,
    interface: str,
    member: str,
    signature: str = "",
    body: list[Any] | None = None,
) -> list[Any]:
    """Call a method and return the reply body; raise DBusError on an error reply."""
    message = Message(
        destination=destination, path=path, interface=interface, member=member, signature=signature, body=body or []
    )
    reply = await bus.call(message)
    if reply is not None and reply.message_type == MessageType.ERROR:
        raise DBusError(reply.error_name or "", f"{member}: {reply.body[0] if reply.body else ''}")
    return reply.body if reply else []
