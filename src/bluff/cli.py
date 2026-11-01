"""Command-line entry point."""

import argparse
import asyncio
import logging
import logging.config
from importlib.resources import files

import yaml

from bluff.ble import Peripheral
from bluff.bridge import Bridge
from bluff.evdev import discover
from bluff.keymap import KEYCODES
from bluff.tray import start_tray

logger = logging.getLogger(__name__)


def parse_hotkey(spec: str) -> frozenset[int]:
    """Turn ``LEFTCTRL+LEFTALT+LEFTMETA`` into Linux keycodes."""
    try:
        return frozenset(KEYCODES[name.strip().upper()] for name in spec.split("+"))
    except KeyError as missing:
        raise argparse.ArgumentTypeError(f"unknown key {missing}; use names like LEFTCTRL, A, F12") from None


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="bluff", description="Be a Bluetooth keyboard and mouse for an iPad.")
    parser.add_argument("--name", default="bluff", help="name the iPad shows when pairing")
    parser.add_argument("--adapter", default="hci0")
    parser.add_argument("--pair", action="store_true", help="be discoverable and confirm pairing on this terminal")
    parser.add_argument("--hotkey", type=parse_hotkey, default="LEFTCTRL+LEFTALT+LEFTMETA", help="toggle chord")
    parser.add_argument("--pixels-per-mm", type=float, default=10.0, help="touchpad pointer speed")
    parser.add_argument("--mm-per-tick", type=float, default=2.0, help="touchpad finger travel per scroll step")
    return parser.parse_args()


async def _run(args: argparse.Namespace) -> None:
    peripheral = Peripheral(args.name, args.adapter)
    if args.pair:
        await peripheral.set_pairing(_ask_terminal)
    await peripheral.start()
    bridge = Bridge(peripheral, args.hotkey, args.pixels_per_mm, args.mm_per_tick)
    bridge.open(discover())
    quit = asyncio.Event()
    await start_tray(bridge, quit)
    if args.pair:
        logger.info("pick %r in the iPad's Bluetooth settings", args.name)
    logger.info("press and release the hotkey to switch")
    await quit.wait()


async def _ask_terminal(question: str) -> bool:
    return (await asyncio.to_thread(input, f"{question} [y/N] ")).strip().lower() == "y"


def main() -> None:
    """Run bluff until interrupted.

    Registers the HID peripheral with BlueZ, opens every keyboard, mouse and
    touchpad, and toggles between laptop and iPad on the hotkey. Exiting
    releases all grabs because the kernel drops them with the file descriptors.
    """
    config = yaml.safe_load(files("bluff").joinpath("logging.yaml").read_text())
    logging.config.dictConfig(config)
    try:
        asyncio.run(_run(_arguments()))
    except KeyboardInterrupt:
        pass
