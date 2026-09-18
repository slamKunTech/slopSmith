#!/usr/bin/env python3
"""Convert one MIDI file to a GP5 tab using the bundled midi-to-guitar-tab
converter, auto-detecting playback BPM from the MIDI's own tempo map.

Usage: python convert_one.py input.mid output.gp5

Exit codes: 0 ok, 1 bad args, 2 empty/no output, 3 conversion error.

Requires: pretty_midi + PyGuitarPro (stage-A venv in midi2sloppak.py;
verified with /Users/mac/glbpy14/glbpy14, Python 3.14).
"""
import sys
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "midi-to-guitar-tab"))

from midi_to_gp5 import build_gp5  # noqa: E402


def detect_bpm(midi_path: str) -> int:
    """GP playback tempo: the MIDI's first tempo change, clamped sanely."""
    import pretty_midi
    pm = pretty_midi.PrettyMIDI(midi_path)
    tempos = pm.get_tempo_changes()[1]
    bpm = int(tempos[0]) if len(tempos) > 0 else 72
    return max(40, min(bpm, 220))


def convert(midi_path: str, out_path: str) -> int:
    """Convert one file; returns the BPM used. Raises on failure."""
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    bpm = detect_bpm(midi_path)
    build_gp5(midi_path, out_path, bpm)
    out = Path(out_path)
    if not out.exists() or out.stat().st_size == 0:
        raise RuntimeError("output not written or empty")
    return bpm


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: convert_one.py input.mid output.gp5", file=sys.stderr)
        sys.exit(1)
    try:
        bpm = convert(sys.argv[1], sys.argv[2])
        print(f"OK bpm={bpm}")
    except SystemExit:
        raise
    except Exception:
        traceback.print_exc(file=sys.stderr)
        sys.exit(3)
