#!/usr/bin/env python3
"""Split a sloppak's full-mix stem into per-instrument stems via Demucs.

Usage:
    python scripts/split_stems.py path/to/song.sloppak [--model htdemucs_6s]

Takes a sloppak whose only stem is `stems/full.ogg`, runs Demucs to split it
into per-instrument stems, replaces `full.ogg` with the results, and rewrites
`manifest.yaml`.

Accepts both forms:
- Directory-form sloppak: edited in place.
- Zip-form sloppak:       unpacked to a temp dir, edited, re-zipped atomically.

Requires `demucs` to be importable in the current interpreter:
    pip install demucs

Default model is `htdemucs_6s` which produces 6 stems:
    vocals, drums, bass, guitar, piano, other
Override with `--model htdemucs` (4 stems: vocals, drums, bass, other).
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

import yaml


# Demucs outputs WAVs named {stem}.wav in a per-track subfolder. We re-encode
# them to OGG/Vorbis with ffmpeg to match the rest of the sloppak format.
_STEM_ORDER = ["guitar", "bass", "drums", "vocals", "piano", "other"]


def _run_demucs(full_ogg: Path, out_dir: Path, model: str) -> Path:
    """Run demucs on full_ogg, return the directory containing the split stems."""
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        sys.executable, "-m", "demucs",
        "-n", model,
        "-o", str(out_dir),
        str(full_ogg),
    ]
    print(f"[*] Running: {' '.join(cmd)}")
    r = subprocess.run(cmd)
    if r.returncode != 0:
        raise RuntimeError(f"demucs exited with code {r.returncode}")

    # Demucs writes to {out_dir}/{model}/{track_stem}/*.wav
    track_stem = full_ogg.stem
    result_dir = out_dir / model / track_stem
    if not result_dir.exists():
        # Some demucs versions use the track stem with spaces replaced, etc.
        candidates = list((out_dir / model).iterdir()) if (out_dir / model).exists() else []
        if len(candidates) == 1 and candidates[0].is_dir():
            result_dir = candidates[0]
        else:
            raise RuntimeError(f"demucs output dir not found under {out_dir}/{model}")
    return result_dir


def _encode_ogg(wav_path: Path, ogg_path: Path) -> None:
    ogg_path.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        ["ffmpeg", "-y", "-i", str(wav_path),
         "-c:a", "libvorbis", "-q:a", "5", str(ogg_path)],
        capture_output=True,
    )
    if r.returncode != 0 or not ogg_path.exists():
        raise RuntimeError(
            f"ffmpeg OGG encode failed for {wav_path.name}: "
            f"{r.stderr.decode(errors='replace')}"
        )


def _rewrite_manifest(source_dir: Path, new_stems: list[dict]) -> None:
    mf = source_dir / "manifest.yaml"
    if not mf.exists():
        mf = source_dir / "manifest.yml"
    if not mf.exists():
        raise FileNotFoundError(f"manifest.yaml not found in {source_dir}")
    data = yaml.safe_load(mf.read_text(encoding="utf-8")) or {}
    data["stems"] = new_stems
    mf.write_text(
        yaml.safe_dump(data, sort_keys=False, allow_unicode=True),
        encoding="utf-8",
    )


def _split_in_dir(source_dir: Path, model: str) -> None:
    """Do the split-and-rewrite work inside an unpacked sloppak directory."""
    full_ogg = source_dir / "stems" / "full.ogg"
    if not full_ogg.exists():
        raise FileNotFoundError(
            f"{full_ogg} not found — nothing to split. "
            f"Run psarc_to_sloppak.py first, or manually add stems/full.ogg."
        )

    with tempfile.TemporaryDirectory(prefix="split_stems_") as td:
        td_path = Path(td)
        result_dir = _run_demucs(full_ogg, td_path, model)

        print(f"[*] Encoding split stems to OGG")
        produced: list[dict] = []
        stems_dir = source_dir / "stems"
        for wav in sorted(result_dir.glob("*.wav")):
            name = wav.stem.lower()  # e.g. "guitar", "vocals"
            out_ogg = stems_dir / f"{name}.ogg"
            _encode_ogg(wav, out_ogg)
            produced.append({
                "id": name,
                "file": f"stems/{name}.ogg",
                "default": "on",
            })

    if not produced:
        raise RuntimeError("demucs produced no output stems")

    # Sort in a sensible mixer order, with unknown names at the end.
    def _order_key(s: dict) -> tuple[int, str]:
        try:
            return (_STEM_ORDER.index(s["id"]), s["id"])
        except ValueError:
            return (len(_STEM_ORDER), s["id"])
    produced.sort(key=_order_key)

    # Remove the now-redundant full mix and update manifest.
    full_ogg.unlink(missing_ok=True)
    _rewrite_manifest(source_dir, produced)

    print(f"[✓] {len(produced)} stems written to {source_dir / 'stems'}")
    for s in produced:
        print(f"    - {s['id']}")


def split(sloppak_path: Path, model: str) -> None:
    if sloppak_path.is_dir():
        _split_in_dir(sloppak_path, model)
        return

    # Zip form: unpack, split, rezip in place (atomic via temp file).
    print(f"[*] Unpacking {sloppak_path.name}")
    with tempfile.TemporaryDirectory(prefix="split_stems_zip_") as td:
        work = Path(td) / "sloppak"
        work.mkdir()
        with zipfile.ZipFile(str(sloppak_path), "r") as zf:
            zf.extractall(work)

        _split_in_dir(work, model)

        print(f"[*] Repacking {sloppak_path.name}")
        tmp_out = sloppak_path.with_suffix(sloppak_path.suffix + ".tmp")
        with zipfile.ZipFile(str(tmp_out), "w", zipfile.ZIP_DEFLATED) as zf:
            for f in work.rglob("*"):
                if f.is_file():
                    zf.write(f, f.relative_to(work).as_posix())
        tmp_out.replace(sloppak_path)


def main() -> int:
    ap = argparse.ArgumentParser(description="Split a sloppak's full-mix into per-instrument stems via Demucs")
    ap.add_argument("sloppak", type=Path, help="input .sloppak (file or directory)")
    ap.add_argument("--model", default="htdemucs_6s",
                    help="demucs model (default: htdemucs_6s = 6 stems inc. guitar + piano; "
                         "htdemucs = 4 stems without guitar)")
    args = ap.parse_args()

    if not args.sloppak.exists():
        print(f"error: {args.sloppak} does not exist", file=sys.stderr)
        return 2

    try:
        import demucs  # noqa: F401
    except ImportError:
        print("error: demucs not installed. Run: pip install demucs", file=sys.stderr)
        return 2

    try:
        split(args.sloppak, args.model)
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
