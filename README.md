# bluff

Use a Linux laptop's keyboard and touchpad on an iPad. The laptop pretends
to be a Bluetooth LE keyboard and mouse (HID over GATT), so the iPad needs no
app; it pairs with bluff like any other keyboard.

Built for Wayland compositors with no input-capture portal (Niri): bluff reads
`/dev/input` directly and takes an exclusive grab while the iPad has control.

## Requirements

- BlueZ 5.56+ with an adapter that supports the LE peripheral role
  (`bluetoothctl show` lists `peripheral` under `Roles`)
- Membership in the `input` group, for `/dev/input/event*`
- Python 3.14 and uv

## Use

Pair once, in the foreground:

```sh
uv run bluff --pair
```

1. On the iPad, open Settings > Bluetooth and pick `bluff`.
2. Compare the six-digit code in the terminal with the iPad's and answer `y`.
3. Press and release Ctrl+Alt+Super together to send input to the iPad.
   Do it again to take it back.

Then stop it and run bluff as a user service. Outside `--pair` it is not
discoverable and refuses every pairing request; the bonded iPad reconnects
on its own.

```sh
uv tool install .
install -Dm644 contrib/bluff.service ~/.config/systemd/user/bluff.service
systemctl --user enable --now bluff
journalctl --user -u bluff -f
```

Stop the service before running `bluff --pair` again; only one instance can
hold the HID service. After pulling changes, `uv tool install --reinstall .`
and `systemctl --user restart bluff`.

If the iPad disconnects, input returns to the laptop on its own. Stopping
bluff releases the grab too.

Options: `--hotkey LEFTCTRL+LEFTALT+LEFTMETA`, `--name`, `--adapter`,
`--pixels-per-mm` (touchpad speed), `--mm-per-tick` (scroll speed). The
iPad's own pointer speed and natural-scrolling settings apply on top.

## Limits

- One finger moves, two fingers scroll, click with two fingers to right-click.
  There is no tap-to-click and there are no iPadOS trackpad gestures: those
  need Apple's Magic Trackpad protocol. Use the Globe-key shortcuts instead.
- Devices plugged in after start are not picked up.
- No clipboard sharing.
