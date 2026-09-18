# midi2gp5 — MIDI → Guitar Pro 5 → .sloppak pipeline

Turns DAW-exported piano/pop MIDI files into playable single-track acoustic
guitar GP5 tabs, then into `.sloppak` DLC packages for Slopsmith (Rocksmith-style
arrangement JSON + synthesized audio stem).

## Layout

| Path | Role |
|---|---|
| `midi-to-guitar-tab/` | Upstream converter (git repo). `midi_to_gp5.py` is the note-level MIDI→GP5 engine used here. Carries 3 local fixes: GP5 cp1252 title sanitization, unplaceable-note `(None, None)` contract, and `NoteType.normal` (PyGuitarPro defaults to `rest`, which made every downstream reader drop all notes). |
| `convert_one.py` | Stage A worker: one MIDI → one GP5, BPM auto-detected from the MIDI tempo map (clamped 40–220). |
| `batch_convert.py` | Stage A batch: mirrors the source tree, parallel subprocesses, per-file timeout, writes `_convert_report.json`. |
| `GuitarProFiles2sloppak_convertor.py` | Stage B batch: GP5 → `.sloppak` (incremental via slug manifest check). Calls slopsmith's `scripts/convert_gp_to_sloppak.py` (gp2rs → RS XML → arrangement JSON; fluidsynth render → `stems/full.wav`). Slug = filename stem + 6-char path hash. |
| `midi2sloppak.py` | Orchestrator chaining A → B with pass-through flags. |

## Environments

Both stages run in **different venvs** (the orchestrator launches them as subprocesses):

- Stage A — `/Users/mac/glbpy14/glbpy14` (Python 3.14): `pretty_midi`, `PyGuitarPro`
  (plus `music21`, `pychord`, `mingus` for the chord-pipeline scripts inside
  `midi-to-guitar-tab/`). PyPI mirror note: the Tsinghua mirror lacks music21 /
  PyGuitarPro builds — install those from official PyPI. mingus installs from a
  GitHub zip (`codeload`) when `git clone` fails.
- Stage B — `/Users/mac/codes/slopSmith/slopsmith/.venv`: needs `guitarpro`,
  `gp2rs`, `gp2midi`, fluidsynth (`/opt/homebrew/bin/fluidsynth`) + soundfont.

## Usage

```bash
# Full chain: MIDI collection -> GP5 tree -> sloppak DLCs
python3 midi2sloppak.py \
    --midi-src   "/path/to/midi_collection" \
    --gp5-dir    "/path/to/gp5_output" \
    --sloppak-out "/path/to/sloppak_converted"

# Only regenerate GP5 tabs (stop after stage A)
python3 midi2sloppak.py --midi-src ... --gp5-dir ... --skip-sloppak --overwrite-midi

# GP5 files already exist — only produce sloppaks
python3 midi2sloppak.py --skip-midi --gp5-dir ... --sloppak-out ...

# Single MIDI file, direct:
python3 convert_one.py song.mid song.gp5
```

Useful flags: `--jobs 6` (stage A parallelism), `--timeout 180` (per-MIDI kill),
`--sloppak-workers 4` (stage B parallelism), `--overwrite-midi` / `--force-sloppak`
(redo existing outputs), `--midi-python` / `--sloppak-python` (venv overrides).

## Known behavior / caveats

- GP5 metadata is cp1252-only: CJK song titles are stored as `?` inside the file,
  but filenames stay UTF-8 and stage B derives the sloppak name/manifest title
  from the filename, so Chinese titles survive the full chain.
- Notes outside guitar range (MIDI <40 or >79) are dropped by design; dense
  keyboard clusters >6 simultaneous notes keep the best 6.
- Stage B "gp2rs produced no arrangements" on third-party GP files usually means
  the source tab uses instruments gp2rs skips — unrelated to this pipeline.
- Tabs are first-draft quality (see `midi-to-guitar-tab/README.md`) — expect
  manual polish in Guitar Pro for awkward arpeggio string assignments.
