"""Build every brand asset from geometry and the bundled fonts.

uv run --no-project --with fonttools --with resvg-py --with pillow --with shapely python brandkit/build.py
"""

import io
import json
import struct
from collections.abc import Iterable
from pathlib import Path

import resvg_py
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from PIL import Image
from shapely import affinity
from shapely.geometry import Point, Polygon, box
from shapely.geometry.base import BaseGeometry

KIT = Path(__file__).parent
LOGO = KIT / "logo"
FONTS = KIT / "fonts"
TRAY = KIT.parent / "src" / "icons"
EXE_ICON = KIT.parent / "src" / "windows" / "bluff.ico"
COLORS = json.loads((KIT / "colors.json").read_text())["colors"]
FELT, BAIZE = COLORS["felt"], COLORS["baize"]
GRAPHITE, CHARCOAL, SILVER, PAPER = COLORS["graphite"], COLORS["charcoal"], COLORS["silver"], COLORS["paper"]
RULE, GRAY = COLORS["rule"], COLORS["gray"]

# Mark geometry on a 64-unit grid: a fanned pair of cards, the front one carrying a pointer.
CARD_W, CARD_H, CARD_R = 28, 38, 4.5
FRONT = (24.5, 35.5, -14)
BACK = (39.5, 28.5, 8)
GAP = 2.8
POINTER = [(-5.2, -9.5), (-5.2, 7.2), (-0.9, 3.2), (2.3, 10.1), (5.4, 8.7), (2.4, 1.8), (7.5, 1.8)]
WORDMARK_X, BASELINE, FONT_SIZE = 93, 47, 45  # matches Kascoord: cap height 31.5 on the same baseline


def _place(shape: BaseGeometry, x: float, y: float, angle: float) -> BaseGeometry:
    return affinity.translate(affinity.rotate(shape, angle, origin=(0, 0)), x, y)


def _card() -> BaseGeometry:
    inner = box(-CARD_W / 2 + CARD_R, -CARD_H / 2 + CARD_R, CARD_W / 2 - CARD_R, CARD_H / 2 - CARD_R)
    return inner.buffer(CARD_R, quad_segs=16)


def mark_shapes() -> tuple[BaseGeometry, BaseGeometry]:
    """Return (cards with the pointer cut out, pointer)."""
    front = _place(_card(), *FRONT)
    back = _place(_card(), *BACK).difference(front.buffer(GAP, quad_segs=16))
    pointer = _place(Polygon(POINTER), *FRONT)
    return back.union(front.difference(pointer)), pointer


def path_d(shape: BaseGeometry) -> str:
    def ring(coords: Iterable[tuple[float, float]]) -> str:
        return "M" + "L".join(f"{x:.2f} {y:.2f}" for x, y in list(coords)[:-1]) + "Z"

    polygons = getattr(shape, "geoms", [shape])
    return "".join(ring(p.exterior.coords) + "".join(ring(i.coords) for i in p.interiors) for p in polygons)


def wordmark(text: str = "bluff") -> tuple[str, float]:
    """Outline the wordmark in Space Grotesk 600; return (path data, right edge of the ink)."""
    font = instantiateVariableFont(TTFont(FONTS / "space-grotesk.ttf"), {"wght": 600})
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = FONT_SIZE / font["head"].unitsPerEm
    first = cmap[ord(text[0])]
    x = WORDMARK_X - font["hmtx"][first][1] * scale
    pen = SVGPathPen(glyphs, ntos=lambda n: f"{n:.2f}")
    right = 0.0
    for char in text:
        name = cmap[ord(char)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x, BASELINE)))
        advance, lsb = font["hmtx"][name]
        right = max(right, x + (lsb + _ink_width(font, name)) * scale)
        x += advance * scale
    return pen.getCommands(), right


def _ink_width(font: TTFont, name: str) -> float:
    glyph = font["glyf"][name]
    return glyph.xMax - glyph.xMin if hasattr(glyph, "xMax") else 0


def svg(view: str, body: str, label: str) -> str:
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}" role="img" aria-label="{label}">{body}</svg>\n'


def write(name: str, content: str, png_width: int | None = None) -> None:
    (LOGO / f"{name}.svg").write_text(content)
    if png_width:
        (LOGO / f"{name}.png").write_bytes(render(content, png_width))


def render(content: str, width: int, height: int | None = None) -> bytes:
    fonts = [str(FONTS / "space-grotesk.ttf"), str(FONTS / "ibm-plex-mono.ttf")]
    return bytes(resvg_py.svg_to_bytes(svg_string=content, width=width, height=height, font_files=fonts))


def logos(cards: str, text: str, text_right: float) -> None:
    width = round(text_right + 10, 2)
    variants = {"color": (BAIZE, CHARCOAL), "reverse": (FELT, SILVER), "black": (CHARCOAL,) * 2, "white": (PAPER,) * 2}
    for variant, (mark_color, text_color) in variants.items():
        write(f"bluff-mark-{variant}", svg("0 0 64 64", f'<path fill="{mark_color}" d="{cards}"/>', "bluff mark"), 512)
        body = f'<path fill="{mark_color}" d="{cards}"/><path fill="{text_color}" d="{text}"/>'
        write(f"bluff-logo-{variant}", svg(f"0 0 {width} 64", body, "bluff"), round(width * 4))
    readme = f'<rect x="-16" y="-12" width="{width + 32}" height="88" rx="10" fill="{GRAPHITE}"/>'
    readme += f'<path fill="{FELT}" d="{cards}"/><path fill="{SILVER}" d="{text}"/>'
    write("bluff-logo-readme", svg(f"-16 -12 {width + 32} 88", readme, "bluff"))
    icon = f'<rect width="512" height="512" rx="104" fill="{GRAPHITE}"/>'
    icon += f'<g transform="translate(64 64) scale(6)"><path fill="{FELT}" d="{cards}"/></g>'
    write("bluff-icon", svg("0 0 512 512", icon, "bluff app icon"), 512)
    # Pillow scales the 256 px render down for the smaller sizes.
    exe_icon = Image.open(io.BytesIO(render(svg("0 0 512 512", icon, "bluff app icon"), 256)))
    exe_icon.save(EXE_ICON, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])


def tray_icons(cards: str, pointer: str) -> None:
    """Pre-rendered StatusNotifierItem pixmaps: per state, frames of (width, height, ARGB32 big-endian)."""
    states = {
        "disconnected": f'<path fill="{GRAY}" d="{cards}"/>',
        "laptop": f'<path fill="{SILVER}" d="{cards}"/>',
        "ipad": f'<path fill="{FELT}" d="{cards}"/>',
        "pairing": f'<path fill="{SILVER}" d="{cards}"/><path fill="{FELT}" d="{pointer}"/>',
    }
    TRAY.mkdir(exist_ok=True)
    for state, body in states.items():
        frames = b""
        for size in (16, 22, 24, 32, 48, 64):
            image = Image.open(io.BytesIO(render(svg("0 0 64 64", body, state), size, size))).convert("RGBA")
            red, green, blue, alpha = image.split()
            argb = Image.merge("RGBA", (alpha, red, green, blue)).tobytes()
            frames += struct.pack(">II", size, size) + argb
        (TRAY / f"tray-{state}.argb").write_bytes(frames)


def board(cards: str, text: str, text_right: float) -> None:
    """A 2 x 3 brand board, 1600 x 1000."""
    gutter, margin, cols, rows = 24, 48, 3, 2
    pw = (1600 - 2 * margin - (cols - 1) * gutter) / cols
    ph = (1000 - 2 * margin - (rows - 1) * gutter) / rows
    origin = [[(margin + c * (pw + gutter), margin + r * (ph + gutter)) for c in range(cols)] for r in range(rows)]
    mono = 'font-family="IBM Plex Mono" font-size="13" letter-spacing="0.8"'
    tagline_font = instantiateVariableFont(TTFont(FONTS / "space-grotesk.ttf"), {"wght": 600})

    def outline(line: str, x: float, y: float, size: float, color: str) -> str:
        glyphs, cmap = tagline_font.getGlyphSet(), tagline_font.getBestCmap()
        scale = size / tagline_font["head"].unitsPerEm
        pen = SVGPathPen(glyphs, ntos=lambda n: f"{n:.2f}")
        for char in line:
            name = cmap[ord(char)]
            glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x, y)))
            x += tagline_font["hmtx"][name][0] * scale
        return f'<path fill="{color}" d="{pen.getCommands()}"/>'

    def panel(r: int, c: int, number: str, label: str, body: str, fill: str = GRAPHITE) -> str:
        x, y = origin[r][c]
        frame = (
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{pw:.1f}" height="{ph:.1f}" rx="16" fill="{fill}" stroke="{RULE}"/>'
        )
        caption = f'<text x="{x + 28:.1f}" y="{y + ph - 26:.1f}" {mono} fill="{GRAY}">{number} / {label}</text>'
        return frame + f'<g transform="translate({x:.1f} {y:.1f})">{body}</g>' + caption

    logo_w = text_right + 10
    scale = 300 / logo_w
    cover = (
        f'<text x="28" y="44" {mono} fill="{GRAY}">BLUFF / VISUAL IDENTITY</text>'
        f'<g transform="translate({(pw - 300) / 2:.1f} {(ph - 64 * scale) / 2:.1f}) scale({scale:.4f})">'
        f'<path fill="{FELT}" d="{cards}"/><path fill="{SILVER}" d="{text}"/></g>'
    )

    menu_items = [
        ("iPad connected", GRAY),
        ("Switch to iPad", SILVER),
        ("Pair a new iPad…", SILVER),
        ("Quit bluff", SILVER),
    ]
    menu = "".join(
        f'<text x="{pw - 300:.1f}" y="{186 + i * 40}" font-family="IBM Plex Mono" font-size="15" fill="{color}">{item}</text>'
        for i, (item, color) in enumerate(menu_items)
    )
    tray = (
        f'<rect x="24" y="28" width="{pw - 48:.1f}" height="48" rx="12" fill="{CHARCOAL}" stroke="{RULE}"/>'
        + "".join(
            f'<circle cx="{52 + i * 22}" cy="52" r="{5 if i == 0 else 4}" fill="{SILVER if i == 0 else RULE}"/>'
            for i in range(4)
        )
        + f'<text x="{pw - 88:.1f}" y="57" font-family="IBM Plex Mono" font-size="14" fill="{SILVER}">20:16</text>'
        + "".join(f'<circle cx="{pw - 190 + i * 22:.1f}" cy="52" r="6" fill="{RULE}"/>' for i in range(3))
        + f'<g transform="translate({pw - 124:.1f} 40) scale(0.375)"><path fill="{FELT}" d="{cards}"/></g>'
        + f'<rect x="{pw - 324:.1f}" y="96" width="292" height="244" rx="12" fill="{CHARCOAL}" stroke="{RULE}"/>'
        + f'<text x="{pw - 300:.1f}" y="134" {mono} fill="{GRAY}">BLUFF</text>'
        + f'<line x1="{pw - 308:.1f}" x2="{pw - 48:.1f}" y1="150" y2="150" stroke="{RULE}"/>'
        + menu
    )

    lines = [
        ("$ systemctl --user enable --now bluff", SILVER),
        ("advertising as 'bluff' on hci0", GRAY),
        ("tray icon registered", GRAY),
        ("host subscribed to keyboard report", GRAY),
        ("input now goes to the iPad", FELT),
    ]
    terminal = (
        f'<rect x="24" y="28" width="{pw - 48:.1f}" height="{ph - 110:.1f}" rx="12" fill="{CHARCOAL}" stroke="{RULE}"/>'
        + "".join(f'<circle cx="{48 + i * 18}" cy="52" r="5" fill="{RULE}"/>' for i in range(3))
        + f'<line x1="24" x2="{pw - 24:.1f}" y1="74" y2="74" stroke="{RULE}"/>'
        + "".join(
            f'<text x="48" y="{118 + i * 34}" font-family="IBM Plex Mono" font-size="15" fill="{color}">{line}</text>'
            for i, (line, color) in enumerate(lines)
        )
    )

    shape, _ = mark_shapes()
    big = affinity.translate(affinity.scale(shape, 5.2, 5.2, origin=(32, 32)), pw / 2 - 32, ph / 2 - 40)
    dots = []
    step = 13
    for gx in range(int(pw // step) + 1):
        for gy in range(int((ph - 60) // step) + 1):
            cx, cy = 10 + gx * step, 10 + gy * step
            distance = big.boundary.distance(Point(cx, cy))
            inside = big.contains(Point(cx, cy))
            radius = 4.6 if inside else max(0.7, 3.2 - distance / 22)
            color = FELT if inside else RULE
            dots.append(f'<circle cx="{cx}" cy="{cy}" r="{radius:.2f}" fill="{color}"/>')
    halftone = f'<g clip-path="url(#p4)">{"".join(dots)}</g>'

    grid = "".join(
        f'<line x1="{x0}" x2="{x0}" y1="0" y2="64" stroke="{RULE}" stroke-width="0.12"/>'
        f'<line x1="0" x2="64" y1="{x0}" y2="{x0}" stroke="{RULE}" stroke-width="0.12"/>'
        for x0 in range(0, 65, 4)
    )
    guides = "".join(
        f'<g transform="translate({cx} {cy}) rotate({angle})"><line x1="0" y1="-24" x2="0" y2="24" stroke="{GRAY}" '
        f'stroke-width="0.15" stroke-dasharray="0.8 0.8"/></g>'
        for cx, cy, angle in (FRONT, BACK)
    )
    chips = "".join(
        f'<rect x="{28 + i * 108}" y="{ph - 110:.1f}" width="92" height="36" rx="8" fill="{color}" stroke="{RULE}"/>'
        f'<text x="{28 + i * 108}" y="{ph - 56:.1f}" font-family="IBM Plex Mono" font-size="11" fill="{GRAY}">{color}</text>'
        for i, color in enumerate((FELT, BAIZE, GRAPHITE, SILVER))
    )
    construction = (
        f'<g transform="translate({pw / 2 - 128:.1f} 24) scale(4)">{grid}<path fill="{FELT}" fill-opacity="0.92" d="{cards}"/>{guides}</g>'
        f'<text x="{pw - 150:.1f}" y="60" {mono} fill="{GRAY}">−14° / +8°</text>' + chips
    )

    essence = (
        outline("Plays the part", 28, ph / 2 - 20, 54, SILVER)
        + outline("of a ", 28, ph / 2 + 44, 54, SILVER)
        + outline("keyboard.", 28 + _advance(tagline_font, "of a ", 54), ph / 2 + 44, 54, FELT)
    )

    body = (
        f'<rect width="1600" height="1000" fill="#0B0C0E"/>'
        f'<defs><clipPath id="p4"><rect width="{pw:.1f}" height="{ph:.1f}" rx="16"/></clipPath></defs>'
        + panel(0, 0, "01", "MARK", cover)
        + panel(0, 1, "02", "TRAY", tray)
        + panel(0, 2, "03", "SERVICE", terminal)
        + panel(1, 0, "04", "HALFTONE", halftone)
        + panel(1, 1, "05", "CONSTRUCTION", construction)
        + panel(1, 2, "06", "ESSENCE", essence)
    )
    content = svg("0 0 1600 1000", body, "bluff brand board")
    (KIT / "board.svg").write_text(content)
    (KIT / "board.png").write_bytes(render(content, 1600, 1000))


def _advance(font: TTFont, text: str, size: float) -> float:
    cmap = font.getBestCmap()
    return sum(font["hmtx"][cmap[ord(c)]][0] for c in text) * size / font["head"].unitsPerEm


def main() -> None:
    LOGO.mkdir(exist_ok=True)
    shape, pointer = mark_shapes()
    cards, pointer_d = path_d(shape), path_d(pointer)
    text, text_right = wordmark()
    logos(cards, text, text_right)
    tray_icons(cards, pointer_d)
    board(cards, text, text_right)
    print(f"mark bounds {tuple(round(v, 1) for v in shape.bounds)}, wordmark ends at {text_right:.1f}")


if __name__ == "__main__":
    main()
