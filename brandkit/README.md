# bluff brand kit

Open `index.html` in a browser for the visual guide, or `board.png` for the one-page overview. Fonts and images load locally.

## Files

| Path | Contents |
| --- | --- |
| `index.html` | Visual guide with logos, colors, type, and size examples |
| `board.svg`, `board.png` | 1600 × 1000 brand board |
| `logo/` | Horizontal logos, transparent marks, README logo and app icon; SVG and PNG |
| `colors.json` | Palette and role restrictions |
| `fonts/` | Space Grotesk, Geist, and IBM Plex Mono with SIL OFL notices |
| `build.py` | Generates every asset above, plus the tray pixmaps in `../src/bluff/icons/` |

## Direction

bluff makes a laptop pass itself off as a Bluetooth keyboard. The mark is two fanned playing cards: a bluff is a card game, and two cards are two screens. The front card carries the pointer as negative space. A gap separates the cards, the same way Kascoord's K keeps its stem apart from its arms.

The identity shares Kascoord's system so the apps read as a family: monochrome Graphite and Silver surfaces, the README logo on a Graphite surface with 10-unit rounded corners, the app icon on a Graphite tile with 104/512 corners, Space Grotesk 600 outlined for the wordmark at the same cap height, Geist for body text and IBM Plex Mono for labels.

The accent is Felt (`#7ED9A6`), the green of a card table, for the mark and small accents on dark backgrounds. Baize (`#2E7A55`) replaces it on light backgrounds, where Felt is too pale for the mark.

## Usage

| Asset | Use |
| --- | --- |
| `bluff-logo-reverse.svg` | Felt mark, Silver wordmark, dark backgrounds |
| `bluff-logo-color.svg` | Baize mark, Charcoal wordmark, light backgrounds |
| `bluff-logo-white.svg`, `bluff-logo-black.svg` | Monochrome |
| `bluff-logo-readme.svg` | README header: reverse logo on its own rounded Graphite surface |
| `bluff-mark-*.svg` | Standalone mark in the same variants |
| `bluff-icon.svg` | Felt mark on a Graphite app tile |

- Full logo: at least 120 CSS pixels wide. Standalone mark: at least 16 pixels; the tray uses it at 16 to 64.
- Leave one mark-height of clear space around standalone logos.
- Use the SVG geometry as supplied. The cards' tilt is part of the mark; do not rotate, stretch, recolor outside the palette, or add shadows and gradients.
- Tray states use the mark alone: Gray when no iPad is connected, Silver while input stays on the laptop, Felt while it goes to the iPad, and Silver with a Felt pointer while pairing.

## Rebuilding

Edit the geometry or palette, then run from the repository root:

```sh
uv run --no-project --with fonttools --with resvg-py --with pillow --with shapely python brandkit/build.py
```

The kit follows the `brandkit` skill (`npx skills add https://github.com/Leonxlnx/imagegen-skills --skill brandkit`). That skill targets image generators; here its strategy and 2 × 3 board layout are drawn as vector geometry instead, so every asset stays editable and reproducible.

Fonts are unmodified upstream files from [Google Fonts](https://github.com/google/fonts/tree/main/ofl), shared with Kascoord, each with its license notice.
