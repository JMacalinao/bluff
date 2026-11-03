<img src="brandkit/logo/bluff-logo-readme.svg" alt="bluff" width="226">

# Your laptop's keyboard and touchpad, on your tablet or phone

Use a Linux or Windows laptop's keyboard and touchpad on an iPad or any other
device that pairs with Bluetooth LE keyboards. The laptop pretends to be a Bluetooth
LE keyboard and mouse (HID over GATT), so the device needs no app; it pairs
with bluff like any other keyboard.

Built for Wayland compositors with no input-capture portal (Niri): bluff reads
`/dev/input` directly and takes an exclusive grab while the device has control.

## Requirements

- BlueZ 5.56+ with an adapter that supports the LE peripheral role
  (`bluetoothctl show` lists `peripheral` under `Roles`)
- Membership in the `input` group, for `/dev/input/event*`
- Rust 1.85 or later, to build from source

## Use

Download the package for your distribution from the
[releases page](https://git.macalinao.org/jj/bluff/releases) and install it:

```sh
sudo apt install ./bluff_*_amd64.deb          # Debian, Ubuntu
sudo dnf install ./bluff-*.x86_64.rpm         # Fedora
sudo pacman -U bluff-*-x86_64.pkg.tar.zst     # Arch
```

Then add yourself to the `input` group (log out and back in after) and start
bluff as a user service tied to the graphical session:

```sh
sudo usermod -aG input "$USER"
systemctl --user enable --now bluff
```

On another distribution, unpack `bluff-*-x86_64-linux.tar.gz` and put
`bluff` in `~/.local/bin` and `bluff.service` in `~/.config/systemd/user`.
The release binaries need glibc 2.36 or later.

To build from source instead:

```sh
cargo install --path . --root ~/.local
install -Dm644 contrib/bluff.service ~/.config/systemd/user/bluff.service
systemctl --user enable --now bluff
```

bluff puts an icon in the system tray (any StatusNotifierItem tray: KDE,
Waybar, DankMaterialShell and others; GNOME needs the AppIndicator
extension).

1. Pick "Pair a new device…" in the tray menu. bluff becomes discoverable for
   two minutes.
2. On the device, open its Bluetooth settings and pick `bluff`. iPadOS may
   list it under the laptop's Bluetooth name instead.
3. A notification shows the six-digit code. Accept it if the device shows the
   same code.
4. Click the tray icon, or press and release Ctrl+Alt+Super together, to
   send input to the device. Press the hotkey again to take it back.

iPadOS does not repeat keys held on bluff, so bluff repeats them itself at the
laptop's key repeat rate. Held letters repeat too, where an Apple keyboard
would show the accent picker.

Outside pairing mode bluff is not discoverable and refuses every pairing
request; a bonded device reconnects on its own. Without a tray, stop the
service and run `bluff --pair` in a terminal to pair there instead.

With more than one device paired, the tray menu lists them and input goes to
the checked one only. All of them stay connected, so picking another device
switches over at once. A newly paired device becomes the checked one. When a
device reconnects, bluff may re-register its keyboard with BlueZ so the device
subscribes again; any other connected device then pauses for a second or two. bluff keeps the addresses of the devices it paired in
`~/.local/state/bluff/hosts`; devices paired before that file existed need
their address added there by hand.

After pulling changes to a source build:
`cargo install --path . --root ~/.local --force` and
`systemctl --user restart bluff`. Logs: `journalctl --user -u bluff -f`.

If the device disconnects, input returns to the laptop on its own. Stopping
bluff releases the grab too.

Options: `--hotkey LEFTCTRL+LEFTALT+LEFTMETA`, `--name`, `--adapter`,
`--pixels-per-mm` (touchpad speed), `--mm-per-tick` (scroll speed). The
device's own pointer speed and natural-scrolling settings apply on top.

## Windows

Needs Windows 11 24H2 or later and a Bluetooth adapter that supports the LE
peripheral role. Download `bluff-*-x86_64-windows.zip` from the
[releases page](https://git.macalinao.org/jj/bluff/releases) and extract
`bluff.exe` from it.

To build it yourself, install [rustup](https://rustup.rs) and unpack
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw/releases) (the
`ucrt-x86_64` zip) with its `bin` folder on `PATH`, then from a clone:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnullvm
cargo +stable-x86_64-pc-windows-gnullvm build --release
```

That gives a single `target\release\bluff.exe`.

Copy `bluff.exe` anywhere. Run it from a terminal, or put a shortcut to it in
`shell:startup` to start it at sign-in. bluff opens no window; quit it from the
tray menu. Run from a terminal, it logs there.

The tray icon and hotkey work as on Linux, with these differences:

- Windows runs pairing. Pick "Pair a new device…" in the tray menu, then pick
  this computer's name (not `bluff`) on the device, and confirm the pairing in
  the Windows prompt. Outside pairing mode bluff is not discoverable; paired
  devices still reconnect.
- The tray menu lists only the devices connected right now.
- The pointer follows Windows' own pointer speed and acceleration, so
  `--pixels-per-mm` has no effect, and `--name` and `--adapter` are ignored.
  Two-finger scrolling on a precision touchpad does follow `--mm-per-tick`.
- While input goes to the device, the touchpad's own two-finger scrolling is
  off. If bluff is killed at that moment, scrolling stays off until you sign
  out, or toggle "Drag two fingers to scroll" in the touchpad settings.
- Ctrl+Alt+Del always reaches Windows, and input to an elevated (admin)
  window stays on the laptop unless bluff runs elevated too.
- Remembered devices and the chosen one live in `%LOCALAPPDATA%\bluff`.

## Uninstall

On Linux, stop the service, remove the package, and delete the list of paired
devices:

```sh
systemctl --user disable --now bluff
sudo apt remove bluff        # Debian, Ubuntu
sudo dnf remove bluff        # Fedora
sudo pacman -R bluff         # Arch
rm -r ~/.local/state/bluff
```

From the tarball or a source build, delete `~/.local/bin/bluff` and
`~/.config/systemd/user/bluff.service` instead of removing a package.

On Windows, pick "Quit bluff" in the tray menu, delete its shortcut from
`shell:startup`, then delete `bluff.exe` and `%LOCALAPPDATA%\bluff`.

Then forget bluff in the device's Bluetooth settings, and remove the device
from the laptop's.

## If pairing fails

If the device shows bluff as connected and then drops the pairing a moment
later, the laptop's Bluetooth controller may still hold keys from an earlier
pairing with that device. This can happen after the device forgot bluff while
BlueZ failed to clear the device from the controller. Turn the adapter off and
on, then pair again:

```sh
bluetoothctl power off
bluetoothctl power on
```

Other Bluetooth devices, such as headphones, disconnect for a moment. To
confirm the cause first, run `sudo btmon` during a pairing attempt: it shows
`SMP: Pairing Failed` with `Reason: DHKey check failed (0x0b)`.

## Limits

- One finger moves, two fingers scroll, click with two fingers to right-click.
  There is no tap-to-click and there are no iPadOS trackpad gestures: those
  need Apple's Magic Trackpad protocol. Use the Globe-key shortcuts instead.
- Keyboards and mice plugged in after start are not picked up (Linux).
- A device paired with bluff cannot also play audio through the laptop
  (Linux). bluff refuses its Bluetooth audio connections, because a device
  that comes back on audio after the laptop resumes does not reconnect to
  bluff.
- No clipboard sharing.

## Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.2.0
git push origin main v0.2.0
```

CI builds the Linux packages and the Windows zip and attaches them to a release
for the tag. The tag must match the version in `Cargo.toml`.

## License

Apache License 2.0; see [`LICENSE`](LICENSE). The fonts in `brandkit/fonts` keep
their own licenses, next to each font.
