#!/usr/bin/env python3
"""midi -> gp5 -> sloppak chained pipeline.

Stage A (MIDI -> GP5):
    batch_convert.py running under the "midi" venv (pretty_midi + PyGuitarPro,
    default /Users/mac/glbpy14/glbpy14). Mirrors the MIDI tree into --gp5-dir.

Stage B (GP5 -> .sloppak):
    GuitarProFiles2sloppak_convertor.py running under the slopsmith venv
    (gp2rs + fluidsynth audio render). Incremental by default: GP5 files that
    already have a manifest under --sloppak-out are skipped.

Usage:
    # full chain from a MIDI collection:
    python midi2sloppak.py --midi-src "/path/to/songs" \
        --gp5-dir "/path/to/gp5_out" \
        --sloppak-out "/path/to/sloppak_out"

    # GP5 files already exist -> only stage B:
    python midi2sloppak.py --skip-midi --gp5-dir ... --sloppak-out ...

    # only regenerate GP5 tabs -> stop after stage A:
    python midi2sloppak.py --midi-src ... --gp5-dir ... --skip-sloppak

Notable flags: --overwrite-midi (redo stage A outputs), --force-sloppak
(reconvert existing sloppaks), --jobs/--sloppak-workers, --timeout.
"""
import argparse
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BATCH_CONVERT = HERE / "batch_convert.py"
GP2SLOPPAK = HERE / "GuitarProFiles2sloppak_convertor.py"

MIDI_PY = "/Users/mac/glbpy14/glbpy14/bin/python3"      # stage A venv
SLOPPOSMITH_PY = "/Users/mac/codes/slopSmith/slopsmith/.venv/bin/python"  # stage B venv


def run_stream(cmd: list[str]) -> int:
    """Run a subprocess, streaming its stdout/stderr through. Returns rc."""
    print("$ " + " ".join(cmd), flush=True)
    return subprocess.run(cmd).returncode


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--midi-src", type=Path, help="root dir of .mid files (stage A input)")
    ap.add_argument("--gp5-dir", type=Path, required=True,
                    help="dir holding (or to receive) the mirrored GP5 tree")
    ap.add_argument("--sloppak-out", type=Path,
                    help="output dir for .sloppak packages (stage B output)")
    ap.add_argument("--skip-midi", action="store_true", help="skip stage A")
    ap.add_argument("--skip-sloppak", action="store_true", help="skip stage B")
    ap.add_argument("--overwrite-midi", action="store_true",
                    help="stage A: regenerate GP5 files that already exist")
    ap.add_argument("--force-sloppak", action="store_true",
                    help="stage B: reconvert songs that already have a sloppak")
    ap.add_argument("--jobs", type=int, default=6, help="stage A parallel workers")
    ap.add_argument("--timeout", type=int, default=180, help="stage A per-file timeout (s)")
    ap.add_argument("--sloppak-workers", type=int, default=4, help="stage B parallel workers")
    ap.add_argument("--midi-python", default=MIDI_PY)
    ap.add_argument("--sloppak-python", default=SLOPPOSMITH_PY)
    args = ap.parse_args()

    if not args.skip_midi and not args.midi_src:
        ap.error("--midi-src is required unless --skip-midi")
    if not args.skip_sloppak and not args.sloppak_out:
        ap.error("--sloppak-out is required unless --skip-sloppak")

    # ── Stage A: MIDI -> GP5 ──────────────────────────────────────────────
    if not args.skip_midi:
        cmd = [args.midi_python, str(BATCH_CONVERT),
               str(args.midi_src), str(args.gp5_dir),
               "--jobs", str(args.jobs), "--timeout", str(args.timeout)]
        if args.overwrite_midi:
            cmd.append("--overwrite")
        rc = run_stream(cmd)
        if rc != 0:
            print("stage A had failures; report:",
                  args.gp5_dir / "_convert_report.json", file=sys.stderr)
            # continue is intentional? no — stop so the user sees the report.
            return rc

    # ── Stage B: GP5 -> sloppak ───────────────────────────────────────────
    if not args.skip_sloppak:
        cmd = [args.sloppak_python, str(GP2SLOPPAK),
               "--src", str(args.gp5_dir), "--out", str(args.sloppak_out),
               "--workers", str(args.sloppak_workers)]
        if args.force_sloppak:
            cmd.append("--force")
        rc = run_stream(cmd)
        if rc != 0:
            print("stage B reported failures (see log above)", file=sys.stderr)
        return rc

    return 0


if __name__ == "__main__":
    sys.exit(main())
