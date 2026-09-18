#!/usr/bin/env python3
"""Batch resume: convert remaining tab images to .gp5 via Bailian (Qwen) vision.

Uses the DashScope OpenAI-compatible endpoint to transcribe tab images to ASCII
tab, then parses and writes .gp5 files via the existing pic2guitarpro pipeline.

Set env:
  BAILIAN_API_KEY=sk-f99e08d3fa024d3ba7d5ee27e8d4700a  (default used)
  BAILIAN_MODEL=qwen3.6-plus                           (default)
"""

from __future__ import annotations

import base64
import io
import json
import os
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

from PIL import Image

# Add project src to path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
from pic2guitarpro.ascii_parser import parse_tab
from pic2guitarpro.roman import sort_by_roman
from pic2guitarpro.song_builder import write_gp5

# ── config ──────────────────────────────────────────────────────────────────
API_KEY = os.environ.get("BAILIAN_API_KEY", "sk-f99e08d3fa024d3ba7d5ee27e8d4700a")
MODEL = os.environ.get("BAILIAN_MODEL", "qwen3.6-plus")
BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
INPUT_DIR = Path("input/pdfs")
OUTPUT_DIR = Path("output")
LOG_PATH = OUTPUT_DIR / f"_batch_bailian_{datetime.now().strftime('%Y%m%d_%H%M')}.log"

IMAGE_MAX_EDGE = 800
IMAGE_QUALITY = 85
MAX_TOKENS = 8192
API_TIMEOUT = 600  # seconds per image (qwen3.6-plus uses heavy reasoning)
MAX_RETRIES = 2
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

USER_INSTRUCTION = (
    "Transcribe every guitar tab stave in this image to ASCII tab, following "
    "the format in the system prompt. Output only the ASCII tab."
)


# ── helpers ──────────────────────────────────────────────────────────────────
def log(msg: str) -> None:
    timestamp = datetime.now().strftime("%H:%M:%S")
    line = f"[{timestamp}] {msg}"
    print(line, flush=True)
    with open(LOG_PATH, "a") as f:
        f.write(line + "\n")
        f.flush()


def encode_image(image_path: Path) -> str:
    """Encode image as base64 data-URI for OpenAI-compatible API."""
    im = Image.open(image_path).convert("RGB")
    im.thumbnail((IMAGE_MAX_EDGE, IMAGE_MAX_EDGE))
    buf = io.BytesIO()
    im.save(buf, format="JPEG", quality=IMAGE_QUALITY)
    b64 = base64.standard_b64encode(buf.getvalue()).decode("utf-8")
    return f"data:image/jpeg;base64,{b64}"


def call_api(image_path: Path, retries: int = MAX_RETRIES) -> str | None:
    """Transcribe one image via Bailian Qwen vision. Returns ASCII tab or None.

    Uses curl via subprocess for reliable long-lived connections on WSL.
    """
    data_uri = encode_image(image_path)

    payload = json.dumps({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": data_uri}},
                {"type": "text", "text": USER_INSTRUCTION},
            ]},
        ],
    })

    # Write payload to temp file to avoid shell escaping issues with large base64
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", delete=False, prefix="bailian_payload_"
    ) as f:
        f.write(payload)
        payload_file = f.name

    try:
        for attempt in range(retries):
            try:
                result = subprocess.run(
                    [
                        "curl", "-s", "--max-time", str(API_TIMEOUT),
                        "-X", "POST", BASE_URL,
                        "-H", "Content-Type: application/json",
                        "-H", f"Authorization: Bearer {API_KEY}",
                        "-d", f"@{payload_file}",
                    ],
                    capture_output=True,
                    text=True,
                    timeout=API_TIMEOUT + 30,  # extra buffer for curl startup
                )

                if result.returncode != 0:
                    log(f"     curl error for {image_path.name} (attempt {attempt+1}): {result.stderr[:200]}")
                    time.sleep(2 ** attempt)
                    continue

                data = json.loads(result.stdout)
                if "choices" in data and len(data["choices"]) > 0:
                    content = data["choices"][0]["message"].get("content", "")
                    return content.strip()
                else:
                    err = data.get("error", {}).get("message", str(data)[:200])
                    log(f"     API error for {image_path.name}: {err}")
                    return None

            except subprocess.TimeoutExpired:
                log(f"     curl timeout for {image_path.name} (attempt {attempt+1})")
                time.sleep(2 ** attempt)
            except json.JSONDecodeError as e:
                log(f"     JSON parse error for {image_path.name} (attempt {attempt+1}): {e}")
                time.sleep(2 ** attempt)
            except Exception as e:
                log(f"     Exception for {image_path.name} (attempt {attempt+1}): {e}")
                time.sleep(2 ** attempt)

        return None
    finally:
        try:
            os.unlink(payload_file)
        except OSError:
            pass


def _is_image(p: Path) -> bool:
    return p.suffix.lower() in SUPPORTED_EXTS and p.is_file()


# ── main logic ───────────────────────────────────────────────────────────────
def process_song(name: str, song_dir: Path) -> bool:
    """Transcribe, parse, write one song. Returns True on success."""
    song_out = OUTPUT_DIR / name
    song_out.mkdir(parents=True, exist_ok=True)

    # Skip if already converted
    gp5_path = song_out / f"{name}.gp5"
    if gp5_path.exists():
        return True  # log is in main loop

    images = [p for p in song_dir.iterdir() if _is_image(p)]
    if not images:
        log(f"  ✗ no images in {song_dir}")
        return False

    images_sorted = [Path(p) for p in sort_by_roman([str(p) for p in images])]

    # Transcribe images sequentially (avoids thread-pool issues on WSL)
    ascii_chunks: list[str] = []
    ocr_errors: list[str] = []

    for img in images_sorted:
        try:
            sys.stdout.flush()
            result = call_api(img)
            if result:
                ascii_chunks.append(result)
            else:
                ocr_errors.append(f"{img.name}: no output")
        except Exception as e:
            ocr_errors.append(f"{img.name}: {e}")
            log(f"  ⚠ error on {img.name}: {e}")

    ascii_text = "\n".join(ascii_chunks)

    # Save intermediate ASCII for debugging
    txt_path = song_out / f"{name}.txt"
    txt_content = ascii_text
    if ocr_errors:
        txt_content += "\n\n# Transcription errors:\n" + "\n".join(ocr_errors)
    txt_path.write_text(txt_content)

    if not ascii_text.strip():
        log(f"  ✗ failed: no tab text extracted from images")
        return False

    # Parse and write .gp5
    try:
        tab = parse_tab(ascii_text)
        write_gp5(tab, gp5_path, title=name)
        log(f"  ✓ {len(images_sorted)} images → {len(tab.measures)} measures → {gp5_path.name}")
        return True
    except Exception as e:
        log(f"  ✗ failed: {e}")
        return False


def main():
    input_dir = INPUT_DIR.resolve()
    output_dir = OUTPUT_DIR.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    if not input_dir.is_dir():
        log(f"ERROR: input dir not found: {input_dir}")
        return 1

    # Collect all song dirs
    songs: list[tuple[str, Path]] = []
    for d in sorted(input_dir.iterdir(), key=lambda p: p.name):
        if d.is_dir() and any(_is_image(p) for p in d.iterdir()):
            songs.append((d.name, d))

    total = len(songs)
    log(f"Starting batch: {total} song dirs found")
    log(f"Model: {MODEL}")
    log(f"Log: {LOG_PATH}")

    ok = 0
    skipped = 0
    failed = 0

    for idx, (name, song_dir) in enumerate(songs, 1):
        gp5_path = output_dir / name / f"{name}.gp5"

        if gp5_path.exists():
            skipped += 1
            if idx <= 5 or idx % 50 == 0:
                log(f"[{idx}/{total}] skip (exists): {name}")
            continue

        log(f"[{idx}/{total}] Processing: {name}")
        try:
            success = process_song(name, song_dir)
            if success:
                ok += 1
            else:
                failed += 1
        except Exception as e:
            log(f"  ✗ CRASH processing song: {e}")
            failed += 1

        # Periodic summary
        if idx % 20 == 0:
            log(f"--- Progress: {ok} ok, {failed} failed, {skipped} skipped, {total - idx} remaining ---")

    log(f"\n{'='*60}")
    log(f"DONE: {ok} converted, {failed} failed, {skipped} skipped, {total} total")
    log(f"Output: {output_dir}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
