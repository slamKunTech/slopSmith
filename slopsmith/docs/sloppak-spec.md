# Sloppak Format — Developer Guide

Sloppak is Slopsmith's open, hand-editable song format. This guide is for developers who want to **read**, **write**, or **extend** the format — including adding new data types like drum tabs, vocal pitches, lighting cues, key/scale annotations, or anything else a future visualization plugin might need.

The authoritative format reference lives in code (`lib/sloppak.py`, `lib/song.py`); this doc explains the why, the how, and the conventions you should follow when adding to it.

---

## 1. Format at a glance

A sloppak exists in **two interchangeable forms**:

| Form | What it is | Used for |
|---|---|---|
| **Directory** | A folder named `*.sloppak/` containing the files below | Authoring, hand editing, plugin development |
| **Zip archive** | A `.sloppak` file (zip with the same files inside) | Distribution |

Both forms hold identical contents. Slopsmith resolves either transparently — zip files are unpacked to a cache the first time they're opened (see `resolve_source_dir()` in [lib/sloppak.py](../lib/sloppak.py)).

### Directory layout

```
my-song.sloppak/
├── manifest.yaml             # Required — all metadata + file index
├── arrangements/
│   ├── lead.json             # One JSON per playable arrangement
│   ├── rhythm.json
│   └── bass.json
├── stems/
│   ├── full.ogg              # Mixed audio (initial single-stem output; may be absent after stem splitting)
│   ├── guitar.ogg            # Optional individual stems
│   ├── bass.ogg
│   ├── drums.ogg
│   ├── vocals.ogg
│   └── other.ogg
├── lyrics.json               # Optional — syllable-level lyrics
└── cover.jpg                 # Optional — album art
```

Three rules to remember:

1. **`manifest.yaml` is the index.** Nothing inside the sloppak is auto-discovered — every file path is listed in the manifest. This makes the format predictable: no scanning, no guessing. (One historical exception: the cover-art handler in `server.py` falls back to `cover.jpg` when `manifest.cover` is missing. New code should not add similar filename fallbacks.)
2. **Filenames in `manifest.yaml` are POSIX paths**, relative to the sloppak root (forward slashes, no leading `/`).
3. **YAML for the manifest, JSON for everything else.** YAML is hand-editable for users; JSON is fast-parsed and easy to round-trip in code.

---

## 2. `manifest.yaml` reference

Minimal valid manifest:

```yaml
title: "Black Hole Sun"
artist: "Soundgarden"
duration: 320.5
arrangements:
  - id: lead
    name: Lead
    file: arrangements/lead.json
    tuning: [0, 0, 0, 0, 0, 0]
    capo: 0
stems:
  - id: full
    file: stems/full.ogg
    default: true
```

Full set of currently-recognized top-level keys:

| Key | Type | Required | Description |
|---|---|---|---|
| `title` | string | yes | Song title |
| `artist` | string | yes | Artist name |
| `album` | string | no | Album |
| `year` | int | no | Release year |
| `duration` | float | yes | Song length in seconds |
| `arrangements` | list | yes | Playable arrangements (see §2.1) |
| `stems` | list | yes | Audio stems (see §2.2) |
| `lyrics` | string | no | Path to lyrics JSON |
| `cover` | string | no | Path to cover image |

Unknown keys are **silently ignored** by the loader. This is deliberate — it's the extensibility hook (see §5).

### 2.1. `arrangements[]`

Each entry describes one playable arrangement and points at its JSON file:

```yaml
arrangements:
  - id: lead              # filesystem-safe stable ID, used for filenames
    name: Lead            # display name (Lead/Rhythm/Bass/Combo are sorted first)
    file: arrangements/lead.json
    tuning: [0, 0, 0, 0, 0, 0]   # six semitone offsets from E A D G B E
    capo: 0
```

- `tuning` is a list of semitone offsets from standard `E2 A2 D2 G3 B3 E4`. **Six elements is the Rocksmith convention** and the only length `lib/tunings.py` produces friendly names for; 5- and 7-string content is accepted by the loader and falls through to a numeric label. For bass, the four bass strings are at indices 0–3; the other two slots are `0`. Consumers should not hard-code `len(tuning) == 6`.
- `name` controls the sort order in the UI: `Lead > Combo > Rhythm > Bass > everything else`.
- Manifest-level `tuning` and `capo` **override** anything embedded in the arrangement JSON. The arrangement JSON's own values are fallbacks.

### 2.2. `stems[]`

```yaml
stems:
  - id: full
    file: stems/full.ogg
    default: true        # plays by default when the song opens
  - id: guitar
    file: stems/guitar.ogg
    default: true
  - id: drums
    file: stems/drums.ogg
    default: false
```

- `id` is referenced by the Stems plugin and any other consumer; keep it stable.
- `default` accepts `true`/`false`, or strings (`"on"`/`"off"`/`"true"`/etc.) for hand-edited manifests.
- A freshly converted sloppak from `lib/sloppak_convert.py` starts with a single `{id: full, file: stems/full.ogg, ...}` entry. After stem-splitting (Demucs), `full.ogg` is removed and the manifest is rewritten with per-instrument entries (`guitar`, `bass`, `drums`, `vocals`, `other`). The format requires only that `stems` is non-empty — there's no specific filename or id that must always be present.

### 2.3. `lyrics`

If present, points at a JSON file containing a flat list of syllable objects:

```json
[
  {"t": 12.34, "d": 0.18, "w": "Hel"},
  {"t": 12.52, "d": 0.22, "w": "lo-"},
  {"t": 13.10, "d": 0.30, "w": "world"}
]
```

| Field | Meaning |
|---|---|
| `t` | Time in seconds |
| `d` | Duration in seconds |
| `w` | Syllable text. `-` suffix joins to next word; `+` is a line break sentinel |

---

## 3. Arrangement JSON — the wire format

Arrangement JSON files use the **wire format** produced by `arrangement_to_wire()` — the on-disk representation of a complete arrangement. Slopsmith's `/ws/highway/{filename}` endpoint transports similar data as a sequence of typed messages (`notes`, `chords`, `anchors`, `chord_templates`, `phrases`, …) rather than as one identical top-level JSON object, and individual frames may drop fields the on-disk format keeps (e.g. WS `chord_templates` currently omits `fingers`). In practice, the WebSocket stream reuses the same per-object field names where applicable, but it should not be treated as a byte-for-byte match for `arrangements/*.json`.

The authoritative serializer/deserializer is in [lib/song.py](../lib/song.py):

- `arrangement_to_wire(arr) → dict` — write
- `arrangement_from_wire(dict) → Arrangement` — read

### 3.1. Top-level shape

```json
{
  "name": "Lead",
  "tuning": [0, 0, 0, 0, 0, 0],
  "capo": 0,
  "notes":      [ /* see 3.2 */ ],
  "chords":     [ /* see 3.3 */ ],
  "anchors":    [ /* see 3.4 */ ],
  "handshapes": [ /* see 3.5 */ ],
  "templates":  [ /* see 3.6 */ ],
  "phrases":    [ /* optional, see 3.7 */ ],
  "beats":      [ /* see 3.8, only on first arrangement */ ],
  "sections":   [ /* see 3.8, only on first arrangement */ ]
}
```

`beats` and `sections` are **song-level** but live on the first arrangement's JSON for convenience — `lib/sloppak.py` hoists them to the `Song` object on load. If you author multiple arrangements, only put them in one file.

### 3.2. Notes

Field names are short on purpose — these get streamed thousands of times per song. Don't expand them.

```json
{
  "t": 12.345,    // time (s)
  "s": 2,         // string (0 = lowest)
  "f": 7,         // fret (0 = open, 24 = max)
  "sus": 0.5,     // sustain (s, 0 = none)
  "sl": 9,        // pitched slide-to fret (-1 = no slide)
  "slu": -1,      // unpitched slide-to fret (-1 = no slide)
  "bn": 1.0,      // bend amount in semitones
  "ho": false,    // hammer-on
  "po": false,    // pull-off
  "hm": false,    // natural harmonic
  "hp": false,    // pinch harmonic
  "pm": false,    // palm mute
  "mt": false,    // string mute
  "tr": false,    // tremolo
  "ac": false,    // accent
  "tp": false     // tap
}
```

Default values: numbers → `0` or `-1` (slides), bools → `false`. Omit fields equal to their default if you're authoring by hand — the parser fills them in.

### 3.3. Chords

A chord groups note-shaped objects under a single time:

```json
{
  "t": 30.0,
  "id": 12,           // index into templates[]
  "hd": false,        // high-density flag
  "notes": [
    {"s": 0, "f": 3, "sus": 0.0, ...},
    {"s": 1, "f": 5, "sus": 0.0, ...}
  ]
}
```

Chord notes use the same field set as standalone notes, **except `t` is omitted** (the chord carries the time). The fingering / shape lookup is `chord.id → templates[id]`.

### 3.4. Anchors

Where the fretting hand sits. Drives the highway zoom box.

```json
{"time": 12.0, "fret": 5, "width": 4}
```

### 3.5. Hand shapes

Spans during which a chord shape is held:

```json
{"chord_id": 12, "start_time": 30.0, "end_time": 31.5}
```

### 3.6. Chord templates

Named shapes referenced by `chord.id` and `handshape.chord_id`:

```json
{
  "name": "Em7",
  "fingers": [-1,  2,  1, -1, -1, -1],
  "frets":   [ 0,  2,  2,  0,  0,  0]
}
```

Both arrays are 6-long, lowest string first. `-1` = unused string. `frets[s] = 0` is open string.

### 3.7. Phrases (optional, multi-difficulty data)

Sources that carry per-phrase difficulty ladders (Rocksmith XML) include this. GP imports and legacy sloppaks omit it:

```json
"phrases": [
  {
    "start_time": 0.0,
    "end_time":   12.5,
    "max_difficulty": 4,
    "levels": [
      { "difficulty": 0, "notes": [...], "chords": [...], "anchors": [...], "handshapes": [...] },
      { "difficulty": 1, "notes": [...], "chords": [...], "anchors": [...], "handshapes": [...] },
      ...
    ]
  }
]
```

If you're writing a converter that doesn't have multi-difficulty data, **omit the `phrases` key entirely** (don't emit `"phrases": []`). A missing key signals "no ladder, disable the master-difficulty slider"; an empty list is the same in current code but reads ambiguously.

### 3.8. Beats and sections

```json
"beats":    [{"time": 0.5, "measure": 1}, {"time": 1.0, "measure": -1}, ...],
"sections": [{"name": "verse", "number": 1, "time": 12.5}, ...]
```

`measure: -1` = sub-beat (not a downbeat). Section `name` follows Rocksmith conventions (`intro`, `verse`, `chorus`, `bridge`, `solo`, `outro`, …).

---

## 4. Reading and writing sloppaks programmatically

### 4.1. Reading (Python, server-side)

```python
from pathlib import Path
from sloppak import load_song, load_manifest

# Quick metadata only (parses manifest, skips arrangement JSONs)
manifest = load_manifest(Path("song.sloppak"))

# Full song load (manifest + all arrangements + lyrics)
loaded = load_song("song.sloppak", dlc_root=Path("/dlc"), unpack_cache_root=Path("/cache"))
print(loaded.song.title, len(loaded.song.arrangements))
print(loaded.stems)        # [{"id": "full", "file": "stems/full.ogg", "default": True}]
print(loaded.manifest)     # raw dict — read your custom keys here
```

### 4.2. Writing (Python, server-side)

There's no general-purpose writer in `lib/` yet. The current writer lives in [lib/sloppak_convert.py](../lib/sloppak_convert.py) inside `convert_psarc_to_sloppak()` — it's the single source of truth for "how a sloppak gets built." If you need to write sloppaks from a new source, copy the structure of that function:

1. Build a `work_dir/` in temp.
2. Write `arrangements/{id}.json` per arrangement using `arrangement_to_wire()`.
3. Encode audio to OGG into `stems/`.
4. Optionally write `lyrics.json`, `cover.jpg`.
5. Compose the `manifest` dict and dump as YAML with `yaml.safe_dump(manifest, sort_keys=False, allow_unicode=True)`.
6. Either `shutil.copytree(work_dir, out)` for directory form, or `_zip_dir(work_dir, out)` for zip form.

Always use `yaml.safe_dump` (not `yaml.dump`) and pass `sort_keys=False` so the human-readable order is preserved.

### 4.3. Reading (JavaScript, plugin-side)

Plugins typically don't read the sloppak file directly — they consume the `/ws/highway/{filename}` WebSocket stream (see `CLAUDE.md` for the message protocol), which produces the same shapes. If you specifically need raw manifest access from the browser, expose it through a custom backend route in your plugin's `routes.py` and fetch it.

---

## 5. Extending the format — adding new data

Sloppak is designed to be extended without breaking older readers. The conventions below come from how `lyrics`, `stems`, and the optional `phrases` ladder were each added.

### 5.1. The golden rule: **manifest opt-in, file off to the side**

New data types should follow this pattern:

1. **Drop a new file** alongside the standard ones (e.g., `drums.json`, `keys.json`, `lighting.json`).
2. **Add a manifest key** that *points at* that file (e.g., `drum_tab: drums.json`).
3. **Make consumers gate on the manifest key**: if the key is absent, do nothing. Never auto-discover by filename — that breaks the "manifest is the index" rule.

So a sloppak with drum tabs would look like:

```yaml
# manifest.yaml
title: "Song"
artist: "Band"
duration: 240.0
arrangements: [...]
stems: [...]
drum_tab: drum_tab.json     # ← new key
```

```
my-song.sloppak/
├── manifest.yaml
├── arrangements/...
├── stems/...
└── drum_tab.json           # ← new file
```

Older Slopsmith readers ignore the unknown `drum_tab` key (the loader uses `manifest.get("drum_tab")` / unknown keys pass through). Your plugin checks for it and renders accordingly. **Zero coordination needed with core.**

### 5.2. Naming conventions for new keys and files

- **Manifest keys**: `snake_case`, descriptive, singular when the value is one thing (`lyrics`, `cover`, `drum_tab`), plural when it's a list (`stems`, `arrangements`).
- **File names**: lowercase, hyphenated or underscored, JSON for structured data, OGG for audio, JPG/PNG for images.
- **Inside JSON**: short field names for hot-path data that gets streamed thousands of times (`t`, `s`, `f` — see §3.2). Long names are fine for one-off metadata.
- **Time fields**: always `t` or `time` (not `start`, not `timestamp`) — and always **seconds as floats**, not ms or ticks. Be consistent with the existing wire format.
- **Indexes / IDs**: stable, filesystem-safe, lowercase. Don't reuse Rocksmith's internal numeric IDs unless you have to.

### 5.3. Worked examples for the kinds of additions you mentioned

#### Drum tab

`drum_tab.json` containing per-piece hits:

```json
{
  "version": 1,
  "kit": [
    {"id": "kick",    "name": "Kick"},
    {"id": "snare",   "name": "Snare"},
    {"id": "hh_open", "name": "Hi-hat (open)"},
    {"id": "ride",    "name": "Ride"}
  ],
  "hits": [
    {"t": 0.500, "p": "kick",  "v": 100},
    {"t": 0.750, "p": "snare", "v":  88},
    {"t": 1.000, "p": "hh_open"}
  ]
}
```

Manifest:

```yaml
drum_tab: drum_tab.json
```

Notes on the design:
- `kit[]` is the legend (which piece IDs exist) — separates fixed metadata from hot-path data.
- `hits[]` uses short field names (`t`, `p`, `v`) since this list can be thousands long.
- `v` (velocity) is optional, defaults to 100 — keeps simple charts terse.

#### Key / scale annotations (for theory-aware visualizations)

`keys.json` mirroring the `sections[]` shape:

```json
{
  "version": 1,
  "events": [
    {"t":   0.0, "key": "Em",     "scale": "natural_minor"},
    {"t":  64.5, "key": "G",      "scale": "major"},
    {"t": 142.0, "key": "Em",     "scale": "natural_minor"}
  ]
}
```

Manifest:

```yaml
keys: keys.json
```

Each entry implicitly applies until the next event. Same model as `sections[]`.

#### Vocal pitch contour

If you want a vocal-pitch overlay separate from the lyrics karaoke layer:

```yaml
vocal_pitch: vocal_pitch.json
```

```json
{
  "version": 1,
  "samples": [
    {"t": 0.000, "hz": 220.5},
    {"t": 0.020, "hz": 222.1}
  ]
}
```

Or — if your data comes as windows of sustained notes — use the same shape as Rocksmith's `vocals.xml`:

```json
{
  "version": 1,
  "notes": [
    {"t": 12.34, "d": 0.4, "midi": 64},
    {"t": 12.74, "d": 0.6, "midi": 67}
  ]
}
```

### 5.4. `version` field — always include it

Every new file should have `"version": 1` at the top. It's free insurance: when you change the schema later, `version: 2` consumers can branch on it. Old consumers without that branch ignore the file (or fall back gracefully).

### 5.5. Stay backward-compatible

If you change a field that already shipped:

- **Adding fields** is always safe (older readers ignore them).
- **Removing fields** breaks older readers. Don't.
- **Repurposing fields** (changing meaning or units) is the worst — bump `version` and branch.

If you're tempted to remove or repurpose: leave the old field, add a new one, and sunset the old one over a release or two.

### 5.6. When to put data inside an arrangement vs. its own file

- **Inside arrangement JSON** (`arrangements/lead.json`):
  - Data that is *per-arrangement* and *per-instrument* (notes, chords, anchors, hand-shapes — guitar specifics).
  - Data that meaningfully differs between Lead and Rhythm versions of the same song.
- **Its own file** (and pointed-at via manifest key):
  - Data that is *song-wide* (lyrics, beats, sections, tempo map, drum tab, lighting, key/scale changes).
  - Data that may be authored or generated independently of the playable arrangement (a stem split, an AI-generated drum tab).

Beats and sections currently live inside the first arrangement JSON for legacy reasons (Rocksmith XML put them there). New song-wide data should be its own file.

### 5.7. Don't break the manifest contract

A few things that should *not* end up in `manifest.yaml`:

- **Per-machine settings** (DMX universes, IPs, output device picks) — those go in `${CONFIG_DIR}/...json`, not the sloppak.
- **UI state** (last zoom level, panel sizes) — `localStorage` only.
- **User progress / play counts** — Slopsmith stores these in its metadata DB, not in the sloppak.

The sloppak holds **the song's authored data**. Anything that varies by user or by machine is out.

---

## 6. Quick reference — file types you'll touch

| File | Format | Schema lives in | Authority |
|---|---|---|---|
| `manifest.yaml` | YAML | `lib/sloppak.py` (`load_manifest`, `extract_meta`) | This doc + the loader |
| `arrangements/*.json` | JSON | `lib/song.py` (`arrangement_to_wire`, `arrangement_from_wire`) | The wire-format functions |
| `lyrics.json` | JSON (flat list) | `lib/sloppak.py` (passed through to `Song.lyrics`) | This doc §2.3 |
| `stems/*.ogg` | OGG Vorbis | — | Convention: `q:a 5` for size/quality balance |
| `cover.jpg` | JPEG | — | Convention: square, 500–1500 px on a side |
| Your new file | JSON (preferred) | Your plugin's spec doc | You |

---

## 7. Testing your extension

If you add a new file type or manifest key:

1. **Round-trip test**: write a sample, load it, write it back, compare. Add to `tests/test_sloppak.py`.
2. **Backward-compat test**: load a sloppak that *doesn't* have your new key — your code must not crash, and the song must still play.
3. **Hand-edit test**: open the directory form in a text editor, change a field by hand, reload Slopsmith. The format is meant to be hand-editable; your additions should preserve that.
4. **Both forms**: test with both the directory form and the zipped form. The unpack cache is invalidated based on mtime and size, so you can repackage and reload without restarting the server.

The full pytest suite (`pytest`) must stay green before any PR.

---

## 8. Where to look in the code

| For… | Read |
|---|---|
| Format detection, source resolution, zip unpacking | [lib/sloppak.py](../lib/sloppak.py) |
| Data classes (`Note`, `Chord`, `Arrangement`, `Song`, `Phrase`) | [lib/song.py](../lib/song.py) |
| Wire-format helpers (`*_to_wire` / `*_from_wire`) | [lib/song.py](../lib/song.py) |
| The reference writer (PSARC → sloppak) | [lib/sloppak_convert.py](../lib/sloppak_convert.py) |
| Live streaming over WebSocket (consumes the same shapes) | `server.py` (`/ws/highway/{filename}`) |
| The plugin system (where new viz consumers go) | [CLAUDE.md](../CLAUDE.md) — Plugin System section |
| Tests | [tests/test_sloppak.py](../tests/test_sloppak.py), [tests/test_sloppak_convert.py](../tests/test_sloppak_convert.py) |
