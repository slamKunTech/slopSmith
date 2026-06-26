#!/usr/bin/env python3
"""Convert a Rocksmith PSARC into a `.sloppak` package.

Usage:
    python scripts/psarc_to_sloppak.py path/to/song.psarc [-o OUT] [--dir]

Produces a single-stem sloppak (stems/full.ogg with default=on). Run
`scripts/split_stems.py` afterwards to replace it with real stems.

- Default output form is a zipped `.sloppak` file.
- Pass `--dir` to emit the directory form instead (for hand-editing).
- Pass `-o PATH` to override the default output location.

Reuses the existing slopsmith library code — no format logic is duplicated:
  * lib/patcher.py    — unpack_psarc
  * lib/song.py       — load_song + arrangement_to_wire
  * lib/audio.py      — WEM→WAV via vgmstream-cli (then WAV→OGG via ffmpeg)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

# Make `lib/` importable regardless of CWD.
_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
sys.path.insert(0, str(_ROOT / "lib"))

import yaml  # noqa: E402

from patcher import unpack_psarc  # noqa: E402
from song import load_song, arrangement_to_wire  # noqa: E402
from audio import find_wem_files, _vgmstream_cmd, _ffmpeg_cmd  # noqa: E402


# ── Helpers ──────────────────────────────────────────────────────────────────

def _sanitize(name: str) -> str:
    """Make a string filesystem-safe for use as a sloppak stem."""
    s = re.sub(r"[^A-Za-z0-9._-]+", "_", name).strip("_")
    return s or "song"


def _arrangement_id(name: str, used: set[str]) -> str:
    """Stable lowercase id for an arrangement, deduped within a song."""
    base = re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_") or "arr"
    candidate = base
    i = 2
    while candidate in used:
        candidate = f"{base}{i}"
        i += 1
    used.add(candidate)
    return candidate


def _wem_to_ogg(wem_path: str, out_ogg: Path) -> None:
    """Decode a WEM to OGG/Vorbis via vgmstream-cli → WAV → ffmpeg."""
    vgmstream = _vgmstream_cmd()
    ffmpeg = _ffmpeg_cmd()
    if not vgmstream:
        raise RuntimeError("vgmstream-cli not found on PATH (needed to decode WEM)")
    if not ffmpeg:
        raise RuntimeError("ffmpeg not found on PATH (needed to encode OGG)")

    with tempfile.TemporaryDirectory(prefix="psarc2slop_") as td:
        wav = Path(td) / "full.wav"
        r = subprocess.run(
            [vgmstream, "-o", str(wav), wem_path],
            capture_output=True,
        )
        if r.returncode != 0 or not wav.exists() or wav.stat().st_size < 100:
            raise RuntimeError(
                f"vgmstream-cli failed for {wem_path}: {r.stderr.decode(errors='replace')}"
            )

        out_ogg.parent.mkdir(parents=True, exist_ok=True)
        r2 = subprocess.run(
            [ffmpeg, "-y", "-i", str(wav), "-c:a", "libvorbis", "-q:a", "5", str(out_ogg)],
            capture_output=True,
        )
        if r2.returncode != 0 or not out_ogg.exists() or out_ogg.stat().st_size < 100:
            raise RuntimeError(
                f"ffmpeg OGG encode failed: {r2.stderr.decode(errors='replace')}"
            )


def _parse_lyrics(extracted_dir: Path) -> list[dict]:
    """Return compact-wire lyric tokens from any vocals XML in the extract."""
    for xml_path in sorted(extracted_dir.rglob("*.xml")):
        try:
            root = ET.parse(xml_path).getroot()
        except Exception:
            continue
        if root.tag != "vocals":
            continue
        out: list[dict] = []
        for v in root.findall("vocal"):
            out.append({
                "t": round(float(v.get("time", "0")), 3),
                "d": round(float(v.get("length", "0")), 3),
                "w": v.get("lyric", ""),
            })
        return out
    return []


def _extract_cover(extracted_dir: Path, out_jpg: Path) -> bool:
    """Convert the largest DDS album art into out_jpg. Returns True on success."""
    dds_files = sorted(
        extracted_dir.rglob("*.dds"), key=lambda p: p.stat().st_size, reverse=True
    )
    if not dds_files:
        return False
    try:
        from PIL import Image
    except ImportError:
        print("[warn] Pillow not installed — skipping cover art", file=sys.stderr)
        return False
    try:
        img = Image.open(dds_files[0]).convert("RGB")
        out_jpg.parent.mkdir(parents=True, exist_ok=True)
        img.save(str(out_jpg), "JPEG", quality=88)
        return True
    except Exception as e:
        print(f"[warn] cover art extraction failed: {e}", file=sys.stderr)
        return False


def _zip_dir(src_dir: Path, out_zip: Path) -> None:
    """Write src_dir's contents into out_zip (flat at root, not nested)."""
    out_zip.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(str(out_zip), "w", zipfile.ZIP_DEFLATED) as zf:
        for f in src_dir.rglob("*"):
            if f.is_file():
                zf.write(f, f.relative_to(src_dir).as_posix())


# ── Main conversion ──────────────────────────────────────────────────────────

def convert(psarc_path: Path, out_path: Path, as_dir: bool) -> Path:
    print(f"[*] Unpacking {psarc_path.name}")
    tmp_extract = Path(tempfile.mkdtemp(prefix="psarc2slop_extract_"))
    work_dir = Path(tempfile.mkdtemp(prefix="psarc2slop_work_"))
    try:
        unpack_psarc(str(psarc_path), str(tmp_extract))

        print("[*] Parsing song data")
        song = load_song(str(tmp_extract))
        if not song.arrangements:
            raise RuntimeError("no playable arrangements found in PSARC")

        # Arrangements → JSON files.
        used_ids: set[str] = set()
        arr_manifest: list[dict] = []
        first = True
        for arr in song.arrangements:
            aid = _arrangement_id(arr.name, used_ids)
            wire = arrangement_to_wire(arr)
            # Embed beats/sections on the first arrangement so sloppak.load_song
            # picks them up onto the Song object.
            if first:
                wire["beats"] = [
                    {"time": round(b.time, 3), "measure": b.measure} for b in song.beats
                ]
                wire["sections"] = [
                    {"name": s.name, "number": s.number, "time": round(s.start_time, 3)}
                    for s in song.sections
                ]
                first = False

            arr_file = work_dir / "arrangements" / f"{aid}.json"
            arr_file.parent.mkdir(parents=True, exist_ok=True)
            arr_file.write_text(json.dumps(wire, separators=(",", ":")), encoding="utf-8")

            arr_manifest.append({
                "id": aid,
                "name": arr.name,
                "file": f"arrangements/{aid}.json",
                "tuning": list(arr.tuning),
                "capo": arr.capo,
            })

        # Audio: biggest WEM → stems/full.ogg.
        print("[*] Converting audio (WEM → OGG)")
        wems = find_wem_files(str(tmp_extract))
        if not wems:
            raise RuntimeError("no WEM audio found in PSARC")
        _wem_to_ogg(wems[0], work_dir / "stems" / "full.ogg")

        stems_manifest = [
            {"id": "full", "file": "stems/full.ogg", "default": "on"},
        ]

        # Lyrics.
        lyrics = _parse_lyrics(tmp_extract)
        lyrics_rel = None
        if lyrics:
            print(f"[*] Writing {len(lyrics)} lyric tokens")
            (work_dir / "lyrics.json").write_text(
                json.dumps(lyrics, separators=(",", ":")), encoding="utf-8"
            )
            lyrics_rel = "lyrics.json"

        # Cover art.
        cover_rel = None
        if _extract_cover(tmp_extract, work_dir / "cover.jpg"):
            cover_rel = "cover.jpg"
            print("[*] Extracted cover art")

        # Manifest.
        manifest: dict = {
            "title": song.title or psarc_path.stem,
            "artist": song.artist or "",
            "album": song.album or "",
            "year": int(song.year or 0),
            "duration": round(float(song.song_length or 0.0), 3),
        }
        if cover_rel:
            manifest["cover"] = cover_rel
        manifest["stems"] = stems_manifest
        manifest["arrangements"] = arr_manifest
        if lyrics_rel:
            manifest["lyrics"] = lyrics_rel

        (work_dir / "manifest.yaml").write_text(
            yaml.safe_dump(manifest, sort_keys=False, allow_unicode=True),
            encoding="utf-8",
        )

        # Emit output.
        if as_dir:
            if out_path.exists():
                shutil.rmtree(out_path)
            shutil.copytree(work_dir, out_path)
        else:
            _zip_dir(work_dir, out_path)

        return out_path
    finally:
        shutil.rmtree(tmp_extract, ignore_errors=True)
        shutil.rmtree(work_dir, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description="Convert a PSARC to a .sloppak")
    ap.add_argument("psarc", type=Path, help="input .psarc file")
    ap.add_argument("-o", "--output", type=Path, default=None,
                    help="output path (default: alongside the PSARC)")
    ap.add_argument("--dir", action="store_true",
                    help="emit directory form instead of a zip")
    args = ap.parse_args()

    psarc = args.psarc
    if not psarc.exists():
        print(f"error: {psarc} does not exist", file=sys.stderr)
        return 2

    stem = _sanitize(psarc.stem.replace("_p", "").replace("_m", ""))
    default_name = f"{stem}.sloppak"
    out = args.output or (psarc.parent / default_name)
    # If user passed a directory as -o, drop the file inside it.
    if out.exists() and out.is_dir() and not args.dir:
        out = out / default_name

    try:
        result = convert(psarc, out, as_dir=args.dir)
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    print(f"[✓] Wrote {result}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
