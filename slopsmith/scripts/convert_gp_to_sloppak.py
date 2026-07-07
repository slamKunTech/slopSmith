#!/usr/bin/env python3
"""Convert Guitar Pro (.gp3/.gp4/.gp5) files to slopsmith .sloppak DLC.

Assembles existing slopsmith pieces into one pipeline:
  GP --gp2rs.convert_file--> Rocksmith XML (guitar tab, string+fret)
     --song.parse_arrangement--> Arrangement --arrangement_to_wire--> JSON
  GP --gp2midi.gp_to_audio--> audio (fluidsynth MIDI render -> .wav)

Audio is a synthesized MIDI render, NOT the original record (GP files are
notation, they carry no real audio). Guitar tab (string/fret) is preserved.
Requires fluidsynth + a soundfont (slopsmith's gp2midi locates them).

Note on metadata: the `guitarpro` library often decodes CJK metadata bytes
with the wrong codec, producing mojibake titles. This script derives the
sloppak slug from the source filename (proper UTF-8, unique via a path
hash) so there are no collisions; the manifest title falls back to the
filename stem when the decoded title is unusable.

Usage (run from anywhere):
    python scripts/convert_gp_to_sloppak.py <gp_file_or_dir> <out_dlc_dir>
"""
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

# Make lib/ importable regardless of CWD.
_HERE = Path(__file__).resolve().parent
_ROOT = _HERE.parent
sys.path.insert(0, str(_ROOT / "lib"))
import yaml  # noqa: E402
import guitarpro  # noqa: E402
import gp2rs  # noqa: E402
import gp2midi  # noqa: E402
from song import parse_arrangement, arrangement_to_wire  # noqa: E402
from tunings import tuning_name  # noqa: E402

def _safe(s: str) -> str:
    """Filesystem-safe slug. Keeps CJK (macOS handles UTF-8 paths fine);
    replaces path-unsafe / control chars with _."""
    if not s:
        return "song"
    out = []
    for ch in s:
        if ch in '/\\:*?"<>|\n\r\t':
            out.append("_")
        else:
            out.append(ch)
    return "".join(out).strip().strip(".")[:80] or "song"


def _decode_mojibake(s: str) -> str:
    """The `guitarpro` library decodes CJK metadata bytes as latin-1/cp1252,
    producing mojibake like '¶}¤£¤F¤f' for '開不了口'. Re-decode: if the
    string contains Latin-1 supplement chars (>= U+0080), assume the original
    bytes were CJK and try big5/gb18030/gbk/utf-8 in order."""
    if not s:
        return s
    if not any("\x80" <= ch <= "\xff" for ch in s):
        return s  # clean ASCII / proper Unicode already
    try:
        raw = s.encode("latin-1", errors="ignore")
    except Exception:
        return s
    for enc in ("big5", "gb18030", "gbk", "utf-8"):
        try:
            return raw.decode(enc)
        except Exception:
            continue
    return s


def _beats_for(duration: float, bpm: float = 120.0):
    """Fallback beats if the XML doesn't carry them."""
    spb = 60.0 / bpm
    out = []
    t = 0.0
    measure = 0
    beat_in = 0
    while t < duration:
        out.append({"time": round(t, 3), "measure": (measure if beat_in == 0 else -1)})
        beat_in = (beat_in + 1) % 4
        if beat_in == 0:
            measure += 1
        t += spb
    return out


def convert_one(gp_path: Path, out_dir: Path) -> str | None:
    name = gp_path.name
    try:
        gp = guitarpro.parse(str(gp_path))
    except Exception as e:
        print(f"  ✗ {name}: parse failed: {e}", flush=True)
        return None

    title = _decode_mojibake((gp.title or "").strip()) or gp_path.stem
    artist = _decode_mojibake((gp.artist or "").strip()) or "Unknown"
    album = _decode_mojibake((gp.album or "").strip())
    year = ""
    if gp.copyright:
        import re
        m = re.search(r"(19|20)\d{2}", gp.copyright)
        if m:
            year = m.group(0)

    # Slug from the source filename stem (proper UTF-8, unique per file) +
    # a short path hash to guarantee no collisions across subdirs.
    import hashlib
    path_hash = hashlib.md5(str(gp_path).encode("utf-8", "replace")).hexdigest()[:6]
    slug = _safe(f"{gp_path.stem}-{path_hash}")
    spath = out_dir / f"{slug}.sloppak"
    if spath.exists():
        shutil.rmtree(spath)
    (spath / "arrangements").mkdir(parents=True, exist_ok=True)
    (spath / "stems").mkdir(exist_ok=True)

    # 1. GP -> RS XML (one per arrangement track; auto-selects guitar/bass/keys)
    with tempfile.TemporaryDirectory() as tmp:
        try:
            xml_files = gp2rs.convert_file(str(gp_path), tmp)
        except Exception as e:
            print(f"  ✗ {name}: gp2rs failed: {e}", flush=True)
            return None
        if not xml_files:
            print(f"  ✗ {name}: gp2rs produced no arrangements", flush=True)
            return None
        # Copy XMLs out of the temp dir before it's removed.
        xml_copy = []
        for xf in xml_files:
            dst = spath / "_xml" / Path(xf).name
            dst.parent.mkdir(exist_ok=True)
            shutil.copy(xf, dst)
            xml_copy.append(str(dst))

    # 2. RS XML -> Arrangement -> wire JSON
    manifest_arrs = []
    wire_arrs = []
    max_dur = 0.0
    for i, xf in enumerate(xml_copy):
        try:
            arr = parse_arrangement(xf)
        except Exception as e:
            print(f"  ! {name}: parse_arrangement failed for {Path(xf).name}: {e}", flush=True)
            continue
        wire = arrangement_to_wire(arr)
        # Synthesize beats on the first arrangement if the XML didn't carry them.
        if i == 0 and not wire.get("beats"):
            wire["beats"] = _beats_for(60.0, 120.0)
        if i == 0 and not wire.get("sections"):
            wire["sections"] = [{"name": "Verse", "number": 1, "start_time": 0.0}]
        af = spath / "arrangements" / f"{arr.name.lower()}.json"
        af.write_text(json.dumps(wire, separators=(",", ":")))
        manifest_arrs.append({
            "id": arr.name.lower(), "name": arr.name,
            "file": f"arrangements/{arr.name.lower()}.json",
            "tuning": arr.tuning, "capo": arr.capo,
        })
        wire_arrs.append(arr)
        # Approx duration from last note time.
        last = max((n.time for n in arr.notes), default=0.0)
        last_c = max((c.time for c in arr.chords), default=0.0)
        max_dur = max(max_dur, last, last_c)

    if not manifest_arrs:
        print(f"  ✗ {name}: no arrangements parsed", flush=True)
        return None

    # 3. GP -> audio (fluidsynth render). gp_to_audio returns .wav when ogg fails.
    audio_base = str(spath / "stems" / "full")
    try:
        audio_path = gp2midi.gp_to_audio(str(gp_path), audio_base)
    except Exception as e:
        print(f"  ! {name}: audio render failed: {e} (sloppak written without audio)", flush=True)
        audio_path = None

    # gp_to_audio may write .wav and (failed) .ogg; normalize to full.wav
    stem_file = None
    if audio_path and os.path.exists(audio_path):
        target = spath / "stems" / "full.wav"
        if audio_path != str(target):
            if target.exists():
                target.unlink()
            os.replace(audio_path, target)
        stem_file = "stems/full.wav"
        # Clean stray .ogg
        for ext in (".ogg",):
            stray = spath / "stems" / f"full{ext}"
            if stray.exists():
                stray.unlink()

    duration = float(max(max_dur + 2.0, getattr(gp, "length", 0) or 0) or max_dur + 2.0)

    manifest = {
        "title": title, "artist": artist, "album": album, "year": int(year) if year else 0,
        "duration": duration,
        "arrangements": manifest_arrs,
        "stems": ([{"id": "full", "file": stem_file, "default": True}]
                  if stem_file else []),
    }
    (spath / "manifest.yaml").write_text(yaml.safe_dump(manifest, sort_keys=False))
    # Remove temp XML staging.
    shutil.rmtree(spath / "_xml", ignore_errors=True)
    print(f"  ✓ {name} -> {spath.name}  ({len(manifest_arrs)} arr, "
          f"{sum(len(a.notes)+sum(len(c.notes) for c in a.chords) for a in wire_arrs)} notes, "
          f"{'audio' if stem_file else 'NO audio'})", flush=True)
    return str(spath)


def main():
    if len(sys.argv) != 3:
        print("Usage: convert_gp_to_sloppak.py <gp_file_or_dir> <out_dlc_dir>")
        sys.exit(2)
    src = Path(sys.argv[1])
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    if src.is_file():
        files = [src]
    else:
        files = [p for p in sorted(src.rglob("*"))
                 if p.is_file() and p.suffix.lower() in (".gp3", ".gp4", ".gp5")]
    print(f"Converting {len(files)} GP file(s) -> {out}")
    ok = 0
    for f in files:
        if convert_one(f, out):
            ok += 1
    print(f"Done: {ok}/{len(files)} converted.")


if __name__ == "__main__":
    main()
