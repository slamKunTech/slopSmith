#!/usr/bin/env python3
"""Generate cover art (title + artist text on a dark gradient) for sloppaks missing one.

Sloppak covers are served by /api/song/{id}/art: it serves manifest.cover,
falling back to cover.jpg in the sloppak dir. Converted songs often ship
without art, so this renders a text cover from manifest.yaml's title/artist.
Colours are deterministic per-title (hash → hue), so re-runs are stable.

Usage:
  python3 scripts/gen_sloppak_covers.py              # all sloppaks in the DLC dir
  python3 scripts/gen_sloppak_covers.py --force      # regenerate even if a cover exists
  python3 scripts/gen_sloppak_covers.py --dry-run    # list what would change
  python3 scripts/gen_sloppak_covers.py --dlc-dir /path/to/DLCs
"""
import argparse
import colorsys
import hashlib
import os
import sys
from pathlib import Path

import yaml
from PIL import Image, ImageDraw, ImageFilter, ImageFont

SIZE = 600
DEFAULT_DLC_DIR = "/Users/mac/Nutstore Files/donemidi/DLCs"
FONT_CANDIDATES = [
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/PingFang.ttc",
]
COVER_EXTENSIONS = (".jpg", ".jpeg", ".png", ".webp")


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if Path(path).exists():
            try:
                return ImageFont.truetype(path, size, index=0)
            except OSError:
                continue
    raise SystemExit("no CJK-capable font found")


def wrap_text(draw, text, font, max_width):
    """Greedy character wrap (CJK-safe)."""
    lines, line = [], ""
    for ch in text:
        if not line or draw.textbbox((0, 0), line + ch, font=font)[2] <= max_width:
            line += ch
        else:
            lines.append(line)
            line = ch
    if line:
        lines.append(line)
    return lines


def fit_text(draw, text, font_size, max_width, max_lines):
    """Shrink font until text wraps into at most max_lines."""
    while font_size > 14:
        font = load_font(font_size)
        lines = wrap_text(draw, text, font, max_width)
        if len(lines) <= max_lines:
            return font, lines
        font_size -= 4
    font = load_font(14)
    return font, wrap_text(draw, text, font, max_width)[:max_lines]


def gradient_rows(hue, steps):
    top = colorsys.hls_to_rgb(hue, 0.17, 0.55)
    bottom = colorsys.hls_to_rgb((hue + 0.06) % 1.0, 0.06, 0.45)
    rows = []
    for i in range(steps):
        t = i / (steps - 1)
        rows.append(tuple(round((a + (b - a) * t) * 255) for a, b in zip(top, bottom)))
    return rows


def render_cover(title: str, artist: str) -> Image.Image:
    hue = int.from_bytes(hashlib.md5(title.encode("utf-8")).digest()[:2], "big") / 65535.0

    img = Image.new("RGB", (SIZE, SIZE))
    draw = ImageDraw.Draw(img)
    for y, color in enumerate(gradient_rows(hue, SIZE)):
        draw.line([(0, y), (SIZE, y)], fill=color)

    # Soft radial glow behind the text block.
    glow = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(glow).ellipse(
        [SIZE * 0.10, SIZE * 0.26, SIZE * 0.90, SIZE * 0.74], fill=255
    )
    glow = glow.filter(ImageFilter.GaussianBlur(80))
    r, g, b = (round(v * 255) for v in colorsys.hls_to_rgb(hue, 0.45, 0.55))
    tint = Image.new("RGBA", (SIZE, SIZE), (r, g, b, 0))
    tint.putalpha(glow.point(lambda v: min(v, 60)))
    img.paste(tint, (0, 0), tint)

    draw = ImageDraw.Draw(img, "RGBA")

    # Faint guitar-string motif near the bottom.
    for i in range(6):
        y = int(SIZE * 0.64) + i * 12
        draw.line([(SIZE * 0.16, y), (SIZE * 0.84, y)], fill=(255, 255, 255, 26), width=1)

    max_w = int(SIZE * 0.84)

    # Title: wrapped, up to 3 lines, vertically centred around 42% height.
    tfont, tlines = fit_text(draw, title, 64, max_w, 3)
    line_h = int(tfont.size * 1.35)
    block_h = line_h * len(tlines)
    y0 = int(SIZE * 0.42) - block_h // 2
    for i, line in enumerate(tlines):
        bb = draw.textbbox((0, 0), line, font=tfont)
        x = (SIZE - bb[2]) / 2
        draw.text((x + 3, y0 + i * line_h + 3), line, font=tfont, fill=(0, 0, 0, 150))
        draw.text((x, y0 + i * line_h), line, font=tfont, fill=(245, 245, 245, 255))

    # Artist: one line, ellipsised to fit, with a small accent line above.
    if artist:
        ay = y0 + block_h + int(line_h * 0.55)
        draw.line([(SIZE * 0.42, ay - 12), (SIZE * 0.58, ay - 12)], fill=(255, 255, 255, 110), width=2)
        afont = load_font(30)
        text = artist
        while len(text) > 1 and draw.textbbox((0, 0), text + "…", font=afont)[2] > max_w:
            text = text[:-1]
        if text != artist:
            text += "…"
        bb = draw.textbbox((0, 0), text, font=afont)
        x = (SIZE - bb[2]) / 2
        draw.text((x + 2, ay + 2), text, font=afont, fill=(0, 0, 0, 150))
        draw.text((x, ay), text, font=afont, fill=(185, 185, 195, 255))

    # Vignette: darken the corners slightly.
    vig = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(vig).ellipse(
        [-SIZE * 0.2, -SIZE * 0.2, SIZE * 1.2, SIZE * 1.2], fill=255
    )
    vig = vig.filter(ImageFilter.GaussianBlur(50))
    img = Image.composite(img, Image.new("RGB", (SIZE, SIZE), (0, 0, 0)), vig)

    return img


def has_cover(sloppak_dir: Path, manifest: dict) -> bool:
    if manifest.get("cover"):
        return True
    return any((sloppak_dir / f"cover{ext}").exists() for ext in COVER_EXTENSIONS)


def main() -> int:
    ap = argparse.ArgumentParser(description="Generate text covers for sloppaks missing art")
    ap.add_argument("--dlc-dir", default=os.environ.get("DLC_DIR") or DEFAULT_DLC_DIR)
    ap.add_argument("--force", action="store_true", help="regenerate even if a cover exists")
    ap.add_argument("--dry-run", action="store_true", help="list only, write nothing")
    args = ap.parse_args()

    dlc = Path(args.dlc_dir)
    if not dlc.is_dir():
        print(f"error: DLC dir not found: {dlc}")
        return 1

    generated = skipped = failed = 0
    for sp in sorted(dlc.glob("*.sloppak")):
        if sp.is_file():
            print(f"skip (zip-form, needs re-zip): {sp.name}")
            skipped += 1
            continue
        manifest_path = sp / "manifest.yaml"
        try:
            manifest = yaml.safe_load(manifest_path.read_text()) or {}
        except Exception as e:
            print(f"fail (manifest unreadable): {sp.name}: {e}")
            failed += 1
            continue
        if not args.force and has_cover(sp, manifest):
            skipped += 1
            continue

        title = str(manifest.get("title") or sp.name).strip()
        artist = str(manifest.get("artist") or "").strip()
        if artist.lower() in ("", "unknown"):
            artist = ""  # skip the artist line on placeholder metadata
        out = sp / "cover.jpg"

        if args.dry_run:
            print(f"would generate: {out.name} in {sp.name}  ({title} — {artist})")
            generated += 1
            continue
        try:
            render_cover(title, artist).save(out, "JPEG", quality=90)
            print(f"generated: {sp.name}/cover.jpg  ({title} — {artist})")
            generated += 1
        except Exception as e:
            print(f"fail: {sp.name}: {e}")
            failed += 1

    print(f"\ndone: {generated} generated, {skipped} skipped, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
