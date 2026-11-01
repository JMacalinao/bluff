<img src="brandkit/logo/bluff-logo-readme.svg" alt="bluff" width="226">

# Your laptop's keyboard and touchpad, on your tablet or phone

Use a Linux laptop's keyboard and touchpad on an iPad or any other device
that pairs with Bluetooth LE keyboards. The laptop pretends to be a Bluetooth
LE keyboard and mouse (HID over GATT), so the device needs no app; it pairs
with bluff like any other keyboard.

Built for Wayland compositors with no input-capture portal (Niri): bluff reads
`/dev/input` directly and takes an exclusive grab while the device has control.

## Requirements

- BlueZ 5.56+ with an adapter that supports the LE peripheral role
  (`bluetoothctl show` lists `peripheral` under `Roles`)
- Membership in the `input` group, for `/dev/input/event*`
- Python 3.14 and uv

## Use

Install and run bluff as a user service tied to the graphical session:

```sh
uv tool install .
install -Dm644 contrib/bluff.service ~/.config/systemd/user/bluff.service
systemctl --user enable --now bluff
```

bluff puts an icon in the system tray (any StatusNotifierItem tray: KDE,
Waybar, DankMaterialShell and others; GNOME needs the AppIndicator
extension).

1. Pick "Pair a new device…" in the tray menu. bluff becomes discoverable for
   two minutes.
2. On the device, open its Bluetooth settings and pick `bluff`.
3. A notification shows the six-digit code. Accept it if the device shows the
   same code.
4. Click the tray icon, or press and release Ctrl+Alt+Super together, to
   send input to the device. Press the hotkey again to take it back.

Outside pairing mode bluff is not discoverable and refuses every pairing
request; a bonded device reconnects on its own. Without a tray, stop the
service and run `bluff --pair` in a terminal to pair there instead.

With more than one device paired, the tray menu lists them and only the
checked one may connect. bluff blocks the others in BlueZ, which keeps their
pairing, so picking another device switches over by itself: about a second for
an iPad, up to a minute for an Android phone. A newly paired device becomes
the checked one. bluff keeps the addresses of the devices it paired in
`~/.local/state/bluff/hosts`; devices paired before that file existed need
their address added there by hand.

After pulling changes: `uv tool install --reinstall .` and
`systemctl --user restart bluff`. Logs: `journalctl --user -u bluff -f`.

If the device disconnects, input returns to the laptop on its own. Stopping
bluff releases the grab too.

Options: `--hotkey LEFTCTRL+LEFTALT+LEFTMETA`, `--name`, `--adapter`,
`--pixels-per-mm` (touchpad speed), `--mm-per-tick` (scroll speed). The
device's own pointer speed and natural-scrolling settings apply on top.

## Limits

- One finger moves, two fingers scroll, click with two fingers to right-click.
  There is no tap-to-click and there are no iPadOS trackpad gestures: those
  need Apple's Magic Trackpad protocol. Use the Globe-key shortcuts instead.
- Keyboards and mice plugged in after start are not picked up.
- No clipboard sharing.
