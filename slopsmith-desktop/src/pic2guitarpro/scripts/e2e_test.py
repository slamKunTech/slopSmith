"""End-to-end test: render ASCII tab to PNG images, run the full pipeline,
and verify the resulting .gp5 round-trips through PyGuitarPro.

Run:  .venv/bin/python scripts/e2e_test.py
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from pic2guitarpro.pipeline import process_folder  # noqa: E402

PAGE_I = """\
e|-3---6---3---2-|
B|-3---8---4---3-|
G|-3---8---5---2-|
D|-5---8---5---0-|
A|-5---6---3-----|
E|---------------|
"""

PAGE_II = """\
e|-------------------------|
B|-------------------------|
G|-------------------------|
D|------------------10-----|
A|-------3---5-------------|
E|-0-3-5---------5----3-0--|
"""


def render_tab_image(text: str, dest: Path) -> None:
    """Render tab text to a PNG using a monospace font (no Tesseract-friendly
    preprocessing here — we feed clean text so OCR has an easy job)."""
    lines = text.splitlines()
    width = 720
    line_h = 26
    height = line_h * (len(lines) + 2)
    img = Image.new("RGB", (width, height), "white")
    draw = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 22)
    except OSError:
        font = ImageFont.load_default()
    y = 10
    for line in lines:
        draw.text((15, y), line, fill="black", font=font)
        y += line_h
    dest.parent.mkdir(parents=True, exist_ok=True)
    img.save(dest)


def main() -> int:
    test_input = ROOT / "test_input"
    test_output = ROOT / "output"
    # Clean previous demo runs (keeps real outputs from other runs untouched
    # only if test_input's song names don't collide — they're demo names).
    if test_input.exists():
        shutil.rmtree(test_input)
    for f in test_output.glob("Smoke on the Water.*"):
        f.unlink(missing_ok=True)
    for f in test_output.glob("Riff Two.*"):
        f.unlink(missing_ok=True)

    song_dir = test_input / "Smoke on the Water"
    render_tab_image(PAGE_I, song_dir / "page_I.png")
    render_tab_image(PAGE_II, song_dir / "page_II.png")

    # Also add a second song to confirm multi-song handling.
    song2 = test_input / "Riff Two"
    render_tab_image(PAGE_II, song2 / "riff_I.png")
    render_tab_image(PAGE_I, song2 / "riff_II.png")

    results = process_folder(test_input, test_output)

    import guitarpro  # noqa: E402

    ok = True
    for r in results:
        if not r.gp5_path or not r.gp5_path.exists():
            print(f"FAIL: {r.name} produced no .gp5 ({r.error})")
            ok = False
            continue
        song = guitarpro.parse(str(r.gp5_path))
        n_meas = len(song.tracks[0].measures)
        n_notes = sum(
            len(b.notes)
            for meas in song.tracks[0].measures
            for b in meas.voices[0].beats
        )
        print(f"OK: {r.name} → {r.gp5_path.name} | {n_meas} measures, {n_notes} notes | title={song.title!r}")
        if n_notes == 0:
            print("  WARN: zero notes — OCR may have failed to read the image.")
            ok = False

    print("\nRESULT:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
