#!/usr/bin/env python3
"""Process ONE song: find the first song without .gp5 and convert it.

Designed to be called repeatedly (via cron or loop). Each invocation is
independent — if one crashes, the next invocation just picks up the next song.

Usage:
    python3 scripts/convert_one.py
"""

from __future__ import annotations

import base64, io, json, os, subprocess, sys, tempfile, time
from datetime import datetime
from pathlib import Path

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
from pic2guitarpro.ascii_parser import parse_tab
from pic2guitarpro.roman import sort_by_roman
from pic2guitarpro.song_builder import write_gp5

API_KEY = os.environ.get("BAILIAN_API_KEY", "sk-f99e08d3fa024d3ba7d5ee27e8d4700a")
MODEL = "qwen3.6-plus"
BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
INPUT_DIR = Path("input/pdfs")
OUTPUT_DIR = Path("output")

SUPPORTED_EXTS = {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tiff", ".tif", ".webp"}

SYSTEM_PROMPT = """\
You are an expert guitar tablature transcriber. You are given an image of \
printed/engraved guitar tablature (six-line tab staves, possibly with a \
standard-music-notation staff above, chord names, lyrics, and header text).

Your job: transcribe ONLY the six-line TAB staves into clean ASCII tab. \
Ignore the standard notation staff, chord-name labels, lyrics, titles, and \
any other text — output nothing but the tab.

OUTPUT FORMAT (exact):
- Each tab system is six lines, top to bottom, labelled e B G D A E \
  (high-e string on top, low-E on the bottom). Each line starts with the \
  letter then | then the fret digits, dashes, and closing |.
- Fret numbers are decimal digits 0-24. A two-digit fret (10-24) is written \
  as two adjacent digits with no dash between them.
- Use - (dash) as the rail/spacer between fret numbers within a measure.
- Use | as the barline between measures. Every line of a system must have \
  barlines at the same columns so the six strings stay aligned.
- Notes struck simultaneously (a chord) must be at the same character column \
  across the six strings.
- Leave a blank line between consecutive tab systems.
- Output ONLY the ASCII tab. No prose, no explanations, no chord names, no \
  markdown fences.

If a part of the image is unreadable, transcribe what you can confidently \
read and omit the unclear bars rather than guessing frets.

Example output:
e|-----------------|
B|-----------------|
G|-----------------|
D|--------5--------|
A|------5----5-----|
E|---3----------3--|
"""


def transcribe(image_path: Path, timeout: int = 600) -> str | None:
    """Call Bailian API via curl. Returns ASCII tab or None."""
    im = Image.open(image_path).convert("RGB")
    im.thumbnail((800, 800))
    buf = io.BytesIO()
    im.save(buf, format="JPEG", quality=85)
    b64 = base64.standard_b64encode(buf.getvalue()).decode()
    data_uri = f"data:image/jpeg;base64,{b64}"

    payload = json.dumps({
        "model": MODEL,
        "max_tokens": 8192,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": data_uri}},
                {"type": "text", "text": "Transcribe every guitar tab stave in this image to ASCII tab, following the format in the system prompt. Output only the ASCII tab."},
            ]},
        ],
    })

    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        f.write(payload)
        pf = f.name

    try:
        for attempt in range(2):
            try:
                result = subprocess.run(
                    ["curl", "-s", "--max-time", str(timeout),
                     "-X", "POST", BASE_URL,
                     "-H", "Content-Type: application/json",
                     "-H", f"Authorization: Bearer {API_KEY}",
                     "-d", f"@{pf}"],
                    capture_output=True, text=True, timeout=timeout + 30,
                )
                if result.returncode != 0:
                    print(f"    curl error (attempt {attempt+1}): {result.stderr[:100]}")
                    time.sleep(5)
                    continue
                data = json.loads(result.stdout)
                if "choices" in data:
                    return data["choices"][0]["message"]["content"].strip()
                else:
                    print(f"    API error: {data.get('error', str(data)[:100])}")
                    return None
            except subprocess.TimeoutExpired:
                print(f"    timeout (attempt {attempt+1})")
                time.sleep(5)
            except Exception as e:
                print(f"    exception (attempt {attempt+1}): {e}")
                time.sleep(5)
        return None
    finally:
        try:
            os.unlink(pf)
        except OSError:
            pass


def find_next_song() -> tuple[str, Path, list[Path]] | None:
    """Find the first song dir that doesn't have a .gp5 file."""
    for d in sorted(INPUT_DIR.iterdir()):
        if not d.is_dir():
            continue
        gp5 = OUTPUT_DIR / d.name / f"{d.name}.gp5"
        if gp5.exists():
            continue
        images = [p for p in d.iterdir()
                  if p.suffix.lower() in SUPPORTED_EXTS and p.is_file()]
        if images:
            return d.name, d, sorted(images)
    return None


def main():
    ts = datetime.now().strftime("%H:%M:%S")
    next_song = find_next_song()
    if next_song is None:
        print(f"[{ts}] All songs have .gp5 files. Done!")
        return 0

    name, song_dir, images = next_song
    total_gp5 = len(list(OUTPUT_DIR.glob("*.gp5"))) + len(list(OUTPUT_DIR.glob("*/*.gp5")))
    print(f"[{ts}] [{total_gp5} gp5] Processing: {name} ({len(images)} images)")

    song_out = OUTPUT_DIR / name
    song_out.mkdir(parents=True, exist_ok=True)

    ascii_chunks = []
    errors = []
    for img in images:
        print(f"  Transcribing {img.name}...", end=" ", flush=True)
        t0 = time.time()
        text = transcribe(img)
        elapsed = time.time() - t0
        if text:
            ascii_chunks.append(text)
            print(f"{elapsed:.0f}s, {len(text)} chars")
        else:
            errors.append(img.name)
            print(f"FAILED ({elapsed:.0f}s)")

    if not ascii_chunks:
        print(f"  FAIL: no output from any image")
        return 1

    ascii_text = "\n".join(ascii_chunks)
    txt_path = song_out / f"{name}.txt"
    txt_content = ascii_text
    if errors:
        txt_content += f"\n\n# Errors on: {errors}"
    txt_path.write_text(txt_content)

    try:
        tab = parse_tab(ascii_text)
        gp5_path = song_out / f"{name}.gp5"
        write_gp5(tab, gp5_path, title=name)
        print(f"  OK: {len(tab.measures)} measures → {gp5_path}")
        return 0
    except Exception as e:
        print(f"  FAIL: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
