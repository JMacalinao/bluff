from pathlib import Path

from bluff.evdev import Kind, parse_devices

PROC = """I: Bus=0019 Vendor=0000 Product=0001 Version=0000
N: Name="Power Button"
H: Handlers=kbd event0
B: EV=3
B: KEY=8000 10000000000000 0

I: Bus=0003 Vendor=0b05 Product=19b6 Version=0110
N: Name="Asus Keyboard"
H: Handlers=sysrq kbd leds event3
B: EV=120013
B: KEY=1000000000007 ff9f207ac14057ff febeffdfffefffff fffffffffffffffe

I: Bus=0018 Vendor=04f3 Product=3282 Version=0100
N: Name="ASUP1208:00 093A:3011 Touchpad"
H: Handlers=mouse1 event7
B: EV=1b
B: KEY=e520 10000 0 0 0 0
B: ABS=2e0800000000003

I: Bus=0003 Vendor=046d Product=c52b Version=0111
N: Name="Logitech USB Receiver"
H: Handlers=mouse2 event9
B: EV=17
B: KEY=ffff0000 0 0 0 0
B: REL=1943
"""


def test_parse_devices_classifies_by_capability() -> None:
    found = {(d.name, d.path, d.kind) for d in parse_devices(PROC)}
    assert found == {
        ("Asus Keyboard", Path("/dev/input/event3"), Kind.KEYBOARD),
        ("ASUP1208:00 093A:3011 Touchpad", Path("/dev/input/event7"), Kind.TOUCHPAD),
        ("Logitech USB Receiver", Path("/dev/input/event9"), Kind.MOUSE),
    }
