"""Tray icons shipped with the package."""

import struct
from functools import cache
from importlib.resources import files
from typing import Any


@cache
def pixmaps(state: str) -> list[list[Any]]:
    """Tray pixmaps for a state, as built by brandkit/build.py: frames of (width, height, ARGB32)."""
    data = files("bluff").joinpath("icons", f"tray-{state}.argb").read_bytes()
    frames, offset = [], 0
    while offset < len(data):
        width, height = struct.unpack_from(">II", data, offset)
        size = width * height * 4
        frames.append([width, height, data[offset + 8 : offset + 8 + size]])
        offset += 8 + size
    return frames
