# pic2guitarpro

Convert a folder of guitar **six-string tablature images** into **Guitar Pro** scores.

Input layout:

```
<input_folder>/
  <Song Name 1>/          # subfolder name = song title
    ..._I.png             # pages, ordered by Roman-numeral suffix
    ..._II.png
    ..._III.png
  <Song Name 2>/
    ...
```

Each subfolder is one song. Its images are OCR'd in Roman-numeral order,
concatenated into one ASCII tab, parsed into measures/beats/notes, and written
as `output/<Song Name>.gp5`. The intermediate ASCII is also saved as
`output/<Song Name>.txt` for inspection.

## Output format: GP5 (openable in Guitar Pro 8)

The bridge uses [PyGuitarPro](PyGuitarPro), which reads/writes **GP3/GP4/GP5**
(`.gp5`) — **not** Guitar Pro 8's native `.gp` (XML-zip) format, for which no
open-source writer exists. Guitar Pro 8 opens `.gp5` files natively, so the
output is fully editable in GP8.

## Setup

A single root venv holds both subprojects plus the bridge code:

```bash
uv venv --python 3.14 .venv
. .venv/bin/activate
uv pip install -e ./OCR-tabber -e ./PyGuitarPro -e .
```

Tesseract OCR must be installed on the system (`tesseract --version`).
OCR language data ships in `OCR-tabber/data/tessdata/`.

## Usage

```bash
. .venv/bin/activate
pic2guitarpro <input_folder>            # writes to ./output
pic2guitarpro <input_folder> -o out/    # custom output dir
```

Or as a library:

```python
from pic2guitarpro.pipeline import process_folder
process_folder("path/to/tabs", output_dir="output")
```

## Pipeline / architecture

```
tab images ──OCR (ocr_tabber)──▶ ASCII tab ──▶ ascii_parser ──▶ ParsedTab ──▶ song_builder ──▶ .gp5
```

| Module | Role |
|---|---|
| `src/pic2guitarpro/roman.py` | Sort image filenames by Roman-numeral suffix. |
| `src/pic2guitarpro/ascii_parser.py` | Tolerant ASCII-tab parser: 6-line staves, barline→measure split, column→beat clustering, OCR-noise cleanup. |
| `src/pic2guitarpro/song_builder.py` | `ParsedTab` → PyGuitarPro `Song` → `.gp5`. |
| `src/pic2guitarpro/pipeline.py` | Walks the input folder, runs OCR + parse + write per song. |
| `src/pic2guitarpro/cli.py` | `pic2guitarpro` command. |

### How OCR noise is handled

- **Tuning letters are kept** in the OCR whitelist so the leading `e/B/G/D/A/E`
  per row survives and barline-segment alignment across the six strings stays
  intact. (A digit-only whitelist corrupts that alignment.)
- **Digit-lookalike letters are mapped back to digits** in the rail (e.g. `0`
  misread as `G`/`O`, `5` as `S`), so open-string and similar frets survive.
- **String number is recovered by subsequence-matching** the detected tuning
  letters against the standard order `e B G D A E`. This correctly handles
  partial staves where OCR dropped empty top strings (a 3-row `D A E` stave
  maps to strings 4/5/6, not 1/2/3).
- **Same-string notes never share a beat** (a string is fretted once at a
  time), so sequential notes on one string are always split into separate
  beats.

## Known limitations (v1)

- **Rhythm is approximate.** ASCII tab does not encode reliable timing; each
  measure's beats are given a duration that divides the bar evenly when
  possible (e.g. 8 beats in 4/4 → eighths), otherwise an eighth-note fallback.
  Time signature is fixed at 4/4.
- **2-digit frets** (10–24) depend on OCR reading both digits; if OCR splits
  them the note may be misread.
- **Articulations** (hammer-on `h`, pull-off `p`, slides `/\`, bends `b`,
  vibrato `~`) are not yet translated into GP effects — only frets and rhythm.
- **Tuning is fixed** to standard EADGBE. Alternate tunings are not detected.

## Test

```bash
. .venv/bin/activate
python scripts/e2e_test.py
```

Renders ASCII tab to PNG images, runs the full pipeline, and verifies the
`.gp5` round-trips through PyGuitarPro. Demo output lands in `output/`.

## Subprojects

- [`OCR-tabber/`](OCR-tabber) — image → ASCII tab OCR (pytesseract + Tesseract).
- [`PyGuitarPro/`](PyGuitarPro) — read/write/parse `.gp3/.gp4/.gp5` files.
# pic2guitarpro
# pic2guitarpro
