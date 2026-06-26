"""Pitch-shift a CDLC's audio to E standard tuning.

Only works for uniform tunings (all strings shifted by the same amount),
e.g. Eb standard (-1), D standard (-2), C# standard (-3).
"""

import json
import os
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

from patcher import unpack_psarc, pack_psarc

RSCLI = Path(os.environ.get("RSCLI_PATH", str(Path(__file__).parent / "tools" / "rscli" / "RsCli")))


def get_tuning(psarc_path: str) -> tuple[list[int], bool]:
    """Extract tuning from a PSARC. Returns (offsets, is_uniform).
    Prefers guitar (Lead/Rhythm/Combo) arrangements over Bass."""
    tmp = Path(tempfile.mkdtemp(prefix="rs_tune_"))
    try:
        unpack_psarc(psarc_path, str(tmp))
        # Also try reading from manifest JSON (works for SNG-only files)
        guitar_tuning = None
        fallback_tuning = None
        # Check manifests first
        for jf in sorted(tmp.rglob("*.json")):
            try:
                import json
                data = json.loads(jf.read_text())
                for k, v in data.get("Entries", {}).items():
                    attrs = v.get("Attributes", {})
                    arr_name = attrs.get("ArrangementName", "")
                    tun = attrs.get("Tuning", {})
                    if not tun or arr_name in ("Vocals", "ShowLights", "JVocals"):
                        continue
                    offsets = [tun.get(f"string{i}", 0) for i in range(6)]
                    if arr_name in ("Lead", "Rhythm", "Combo"):
                        if guitar_tuning is None:
                            guitar_tuning = offsets
                    elif fallback_tuning is None:
                        fallback_tuning = offsets
            except Exception:
                continue
        # Check XMLs as fallback
        if guitar_tuning is None and fallback_tuning is None:
            for xml_path in sorted(tmp.rglob("*.xml")):
                try:
                    tree = ET.parse(xml_path)
                    root = tree.getroot()
                except ET.ParseError:
                    continue
                if root.tag != "song":
                    continue
                arr = root.find("arrangement")
                if arr is not None and arr.text:
                    low = arr.text.lower().strip()
                    if low in ("vocals", "showlights", "jvocals"):
                        continue
                tuning = root.find("tuning")
                if tuning is not None:
                    offsets = [int(tuning.get(f"string{i}", "0")) for i in range(6)]
                    fname = xml_path.stem.lower()
                    if "lead" in fname or "rhythm" in fname or "combo" in fname:
                        if guitar_tuning is None:
                            guitar_tuning = offsets
                    elif fallback_tuning is None:
                        fallback_tuning = offsets
        best = guitar_tuning or fallback_tuning or [0] * 6
        is_uniform = len(set(best)) == 1
        return best, is_uniform
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _pitch_shift_wem(wem_path: Path, semitones: int) -> bool:
    """Decode a WEM, pitch-shift it, and replace the original file.

    Returns True if successful.
    """
    wav_decoded = wem_path.with_suffix(".decoded.wav")
    wav_shifted = wem_path.with_suffix(".shifted.wav")
    ogg_out = wem_path.with_suffix(".shifted.ogg")

    # Step 1: Decode WEM to WAV
    decoded = False
    if shutil.which("vgmstream-cli"):
        r = subprocess.run(
            ["vgmstream-cli", "-o", str(wav_decoded), str(wem_path)],
            capture_output=True, text=True,
        )
        if r.returncode == 0 and wav_decoded.exists() and wav_decoded.stat().st_size > 100:
            decoded = True
            print(f"    Decoded with vgmstream ({wav_decoded.stat().st_size} bytes)")

    if not decoded and shutil.which("ffmpeg"):
        r = subprocess.run(
            ["ffmpeg", "-y", "-i", str(wem_path), str(wav_decoded)],
            capture_output=True, text=True,
        )
        if r.returncode == 0 and wav_decoded.exists() and wav_decoded.stat().st_size > 100:
            decoded = True
            print(f"    Decoded with ffmpeg ({wav_decoded.stat().st_size} bytes)")

    if not decoded:
        print(f"    FAILED to decode {wem_path.name}")
        # Cleanup
        for f in [wav_decoded, wav_shifted, ogg_out]:
            if f.exists():
                f.unlink()
        return False

    # Step 2: Pitch shift (rubberband preserves tempo, only shifts pitch)
    # Detect original sample rate to preserve it
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=sample_rate",
         "-of", "default=noprint_wrappers=1:nokey=1", str(wav_decoded)],
        capture_output=True, text=True,
    )
    sample_rate = probe.stdout.strip() or "44100"

    factor = 2 ** (semitones / 12)
    r = subprocess.run(
        ["ffmpeg", "-y", "-i", str(wav_decoded),
         "-af", f"rubberband=pitch={factor}",
         "-ar", sample_rate,
         "-q:a", "6", str(ogg_out)],
        capture_output=True, text=True,
    )
    wav_decoded.unlink()

    if r.returncode != 0 or not ogg_out.exists():
        print(f"    FAILED to pitch-shift: {r.stderr[-200:]}")
        for f in [wav_shifted, ogg_out]:
            if f.exists():
                f.unlink()
        return False

    print(f"    Shifted {semitones:+d} semitones ({ogg_out.stat().st_size} bytes)")

    # Step 3: Replace original WEM with shifted OGG
    # (Rocksmith accepts OGG files with .wem extension)
    wem_path.unlink()
    shutil.move(str(ogg_out), str(wem_path))
    return True


def retune_to_standard(psarc_path: str, output_path: str = "") -> str:
    """Pitch-shift a CDLC to E standard tuning.

    Args:
        psarc_path: Input .psarc file
        output_path: Output path (default: same name with _EStd suffix)

    Returns:
        Path to the new .psarc file

    Raises:
        ValueError: If tuning is not uniform or already E standard
    """
    offsets, is_uniform = get_tuning(psarc_path)

    if all(o == 0 for o in offsets):
        raise ValueError("Already in E standard tuning")

    if not is_uniform:
        raise ValueError(
            f"Non-uniform tuning {offsets} — only uniform tunings supported. "
            f"E.g. Eb standard [-1,-1,-1,-1,-1,-1]"
        )

    semitones = -offsets[0]  # e.g. offset=-1 (Eb) → shift up by 1
    print(f"Tuning: {offsets} → shifting {semitones:+d} semitone(s)")

    tmp = Path(tempfile.mkdtemp(prefix="rs_retune_"))
    try:
        # Extract
        print("Extracting PSARC...")
        unpack_psarc(psarc_path, str(tmp))

        # Pitch-shift all audio files
        shifted_count = 0
        for wem in sorted(tmp.rglob("*.wem")):
            print(f"Processing: {wem.name}")
            if _pitch_shift_wem(wem, semitones):
                shifted_count += 1

        if shifted_count == 0:
            raise RuntimeError("No audio files were successfully pitch-shifted")

        print(f"Shifted {shifted_count} audio file(s)")

        # Update arrangement XMLs: set tuning to E standard
        for xml_path in sorted(tmp.rglob("*.xml")):
            try:
                tree = ET.parse(xml_path)
                root = tree.getroot()
            except ET.ParseError:
                continue
            if root.tag != "song":
                continue

            tuning_el = root.find("tuning")
            if tuning_el is not None:
                for i in range(6):
                    tuning_el.set(f"string{i}", "0")
                tree.write(xml_path, xml_declaration=True, encoding="UTF-8")
                print(f"Updated tuning: {xml_path.name}")

        # Recompile SNGs from updated XMLs
        if RSCLI.exists():
            for xml_path in sorted(tmp.rglob("songs/arr/*.xml")):
                try:
                    tree = ET.parse(xml_path)
                    root = tree.getroot()
                except ET.ParseError:
                    continue
                if root.tag != "song":
                    continue
                arr = root.find("arrangement")
                if arr is not None and arr.text:
                    low = arr.text.lower().strip()
                    if low in ("vocals", "showlights", "jvocals"):
                        continue

                stem = xml_path.stem
                sng_path = tmp / "songs" / "bin" / "generic" / f"{stem}.sng"
                if sng_path.exists():
                    print(f"Recompiling SNG: {stem}")
                    subprocess.run(
                        [str(RSCLI), "xml2sng", str(xml_path), str(sng_path)],
                        capture_output=True,
                    )

        # Update JSON manifests
        for json_path in sorted(tmp.rglob("*.json")):
            try:
                data = json.loads(json_path.read_text())
                changed = False
                for entry in data.get("Entries", {}).values():
                    attrs = entry.get("Attributes", {})
                    if "Tuning" in attrs:
                        attrs["Tuning"] = {f"string{i}": 0 for i in range(6)}
                        changed = True
                if changed:
                    json_path.write_text(json.dumps(data, indent=2))
            except (json.JSONDecodeError, KeyError):
                pass

        # Repack
        print("Repacking PSARC...")
        if not output_path:
            p = Path(psarc_path)
            stem = p.stem
            if stem.endswith("_p"):
                stem = stem[:-2]
            output_path = str(p.parent / f"{stem}_EStd_p.psarc")

        pack_psarc(str(tmp), output_path)
        print(f"Created: {output_path}")
        return output_path

    finally:
        shutil.rmtree(tmp, ignore_errors=True)
