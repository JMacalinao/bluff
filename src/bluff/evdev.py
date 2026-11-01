"""Minimal evdev access: device discovery, nonblocking reads and exclusive grab."""

import asyncio
import fcntl
import logging
import os
import struct
from collections.abc import Callable
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path

logger = logging.getLogger(__name__)

EV_SYN, EV_KEY, EV_REL, EV_ABS = 0, 1, 2, 3
EV_REP = 0x14
SYN_REPORT = 0
REL_X, REL_Y, REL_HWHEEL, REL_WHEEL = 0, 1, 6, 8
ABS_X, ABS_Y = 0, 1
KEY_A = 30
BTN_LEFT = 0x110
BTN_TOOL_FINGER, BTN_TOUCH = 0x145, 0x14A
BTN_TOOL_DOUBLETAP, BTN_TOOL_TRIPLETAP, BTN_TOOL_QUADTAP = 0x14D, 0x14E, 0x14F

_EVENT = struct.Struct("llHHi")
_EVIOCGRAB = 0x40044590
_EVIOCGABS_BASE = 0x80184540  # _IOR('E', 0x40 + axis, struct input_absinfo)
_ABSINFO = struct.Struct("6i")

type EventHandler = Callable[[Device, int, int, int], None]


class Kind(StrEnum):
    KEYBOARD = "keyboard"
    MOUSE = "mouse"
    TOUCHPAD = "touchpad"


@dataclass(frozen=True)
class DeviceInfo:
    name: str
    path: Path
    kind: Kind


def _bitmask(words: str) -> int:
    """Join a /proc bitmap (64-bit hex words, most significant first) into one int."""
    return int("".join(w.rjust(16, "0") for w in words.split()), 16)


def _has(mask: int, *bits: int) -> bool:
    return all(mask >> bit & 1 for bit in bits)


def _classify(fields: dict[str, str]) -> Kind | None:
    ev = _bitmask(fields.get("EV", "0"))
    key = _bitmask(fields.get("KEY", "0"))
    if _has(ev, EV_REL) and _has(_bitmask(fields.get("REL", "0")), REL_X, REL_Y):
        return Kind.MOUSE
    if _has(ev, EV_ABS) and _has(_bitmask(fields.get("ABS", "0")), ABS_X, ABS_Y):
        return Kind.TOUCHPAD if _has(key, BTN_TOOL_FINGER, BTN_LEFT) else None
    if _has(ev, EV_KEY, EV_REP) and _has(key, KEY_A):
        return Kind.KEYBOARD
    return None


def parse_devices(text: str) -> list[DeviceInfo]:
    """Classify the input devices listed in /proc/bus/input/devices."""
    devices = []
    for block in text.strip().split("\n\n"):
        fields: dict[str, str] = {}
        name = ""
        for line in block.splitlines():
            tag, _, rest = line.partition(": ")
            if tag == "N":
                name = rest.removeprefix('Name="').removesuffix('"')
            elif tag in ("H", "B"):
                k, _, v = rest.partition("=")
                fields[k] = v
        event = next((h for h in fields.get("Handlers", "").split() if h.startswith("event")), None)
        kind = _classify(fields)
        if event and kind:
            devices.append(DeviceInfo(name, Path("/dev/input") / event, kind))
    return devices


def discover() -> list[DeviceInfo]:
    return parse_devices(Path("/proc/bus/input/devices").read_text())


class Device:
    """An open evdev node that forwards events to a handler from the event loop."""

    def __init__(self, info: DeviceInfo, handler: EventHandler) -> None:
        self.info = info
        self._handler = handler
        self._fd = os.open(info.path, os.O_RDONLY | os.O_NONBLOCK)
        asyncio.get_running_loop().add_reader(self._fd, self._read)

    def resolution(self, axis: int) -> int:
        """Units per millimeter of an absolute axis, 0 if the device does not say."""
        buf = fcntl.ioctl(self._fd, _EVIOCGABS_BASE + axis, bytes(_ABSINFO.size))
        return _ABSINFO.unpack(buf)[5]

    def grab(self, on: bool) -> None:
        fcntl.ioctl(self._fd, _EVIOCGRAB, int(on))

    def close(self) -> None:
        asyncio.get_running_loop().remove_reader(self._fd)
        os.close(self._fd)

    def _read(self) -> None:
        try:
            data = os.read(self._fd, _EVENT.size * 64)
        except BlockingIOError:
            return
        except OSError:
            logger.warning("%s went away", self.info.name)
            self.close()
            return
        for _, _, type_, code, value in _EVENT.iter_unpack(data):
            self._handler(self, type_, code, value)
