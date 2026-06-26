# Slopsmith — AI Agent Guide

Slopsmith is a self-hosted web app for browsing, playing, and practicing Rocksmith 2014 Custom DLC. It runs as a Docker container with a FastAPI backend (`server.py`), vanilla JavaScript frontend (`static/`), shared Python libraries (`lib/`), and an extensive plugin system (`plugins/`). There are no frontend frameworks — everything is plain JS, HTML, and Tailwind CSS.

## Architecture Quick Reference

```
server.py              FastAPI app — library API, WebSocket highway, plugin loading
static/
  app.js               Main frontend — screens, library views, player, plugin loader
  highway.js           Canvas note highway renderer (createHighway factory)
  index.html           Single-page app shell
  style.css            Custom CSS loaded alongside Tailwind
lib/
  song.py              Core data models (Note, Chord, Arrangement, Song)
  psarc.py             PSARC archive reading and extraction
  sloppak.py           Sloppak format support
  audio.py             WEM/OGG/MP3 audio handling
  retune.py            Pitch-shifting logic
  tunings.py           Tuning name/offset utilities
  gp2rs.py             Guitar Pro to Rocksmith XML conversion
  gp2midi.py           Guitar Pro to MIDI
plugins/
  __init__.py           Plugin discovery, loading, requirements install
  <plugin_name>/        Each plugin is its own directory (often a git submodule)
tests/
  test_*.py             pytest test suite
```

## Plugin System

Plugins are the primary extension point. Each plugin lives in `plugins/<name>/` with a `plugin.json` manifest:

```json
{
  "id": "my_plugin",
  "name": "My Plugin",
  "version": "1.0.0",
  "private": false,
  "type": "visualization",
  "nav": { "label": "My Plugin", "screen": "plugin-my_plugin" },
  "screen": "screen.html",
  "script": "screen.js",
  "routes": "routes.py",
  "settings": { "html": "settings.html" }
}
```

All fields except `id` and `name` are optional. Plugins can have any combination of frontend (screen/script), backend (routes), and settings.

`version` and `private` are advisory metadata — the plugin loader does not currently consume them, but plugins commonly include them for publishing/tooling purposes.

`type` is an optional role hint (slopsmith#36). Supported values:
- `"visualization"` — plugin provides a highway renderer. Declaring this makes the plugin eligible for the main-player viz picker AND splitscreen's per-panel picker. Must pair with a `window.slopsmithViz_<id>` factory exporting the setRenderer contract below.
- Absent → no declared role; plugin is loaded and its script runs, but it doesn't appear in role-specific UIs.

**Backend routes** — `routes.py` must export a `setup(app, context)` function. The `context` dict provides:
- `config_dir` — persistent config path
- `get_dlc_dir()` — returns the DLC folder Path
- `extract_meta()` — metadata extraction callable
- `meta_db` — shared MetadataDB instance
- `get_sloppak_cache_dir()` — sloppak cache path
- `load_sibling(name)` — loads a sibling module from this plugin's directory under a unique, namespaced module name. See "Sibling imports" below.

**Sibling imports — use `load_sibling`, not bare imports** (slopsmith#33). The plugin loader inserts each plugin's directory onto `sys.path` so `from extractor import X` works, but Python caches imports by **module name** in `sys.modules`. Two plugins that each ship a top-level `extractor.py` (or any other generic name — `util.py`, `client.py`, `parser.py`, `config.py`, …) collide: whichever loads first wins, and the other plugin's `from extractor import X` either gets the wrong module or fails with `cannot import name 'X' from 'extractor'`.

The fix is `context["load_sibling"](name)`, which loads the sibling under a namespaced module name (`plugin_<id>.<name>`, where plugin_id is bijectively encoded so reverse-DNS-style ids like `com.example.foo` work without colliding: `_` -> `_5f_`, `.` -> `_2e_`) so each plugin gets its own copy:

```python
def setup(app, context):
    extractor = context["load_sibling"]("extractor")
    PsarcReader = extractor.PsarcReader
    # …
```

Notes:
- `name` is a bare module name — no `.py` suffix, no slashes, no `.`. The helper raises `ValueError` for path traversal / format issues and `ImportError` for missing files.
- Both single-file siblings (`extractor.py`) and package-form siblings (`extractor/__init__.py`) work. Package form wins when both exist (matches CPython's import-resolution precedence).
- Relative imports between siblings work — `from .shared import X` in a top-level helper, `from ..shared import X` from inside a sibling package. The synthetic parent package `plugin_<id>` carries the plugin directory in its `__path__`.
- `from . import sibling` (attribute-style) also resolves: loaded children are exposed as attributes on the parent package.
- Repeat calls return the cached module. Concurrent first-time calls are serialized via per-module locks so no caller observes a half-initialized module.
- Bare `import sibling` from `routes.py` still works during the transition period, but the loader prints a startup warning when it detects two plugins shipping a same-named top-level module — covering both `.py` files and package directories. Migrate to `load_sibling` to silence the warning and immunize your plugin from future ecosystem collisions. (Don't mix bare imports and `load_sibling` for the same module — they'd execute the file twice and split module-level state.)

**Frontend scripts** — `screen.js` runs in the global scope via a `<script>` tag. It can access `window.playSong`, `window.showScreen`, `window.createHighway`, the `<audio>` element, and the `window.slopsmith` event emitter.

**The playSong wrapper chain** — Plugins commonly wrap `window.playSong` to hook into song playback. Plugins load alphabetically, so the last-loaded (alphabetically later) wrapper runs first, while the alphabetically first plugin runs closest to the original. Be aware that `await` calls in inner wrappers yield to the event loop — WebSocket messages can arrive before outer wrappers finish setup.

## Plugin Best Practices

### Visualization plugins — two complementary contracts

Slopsmith supports two ways for a plugin to participate in the main player's visuals. They coexist; the setRenderer contract is the default for any viz that draws a highway-shaped surface, and overlays handle layered decorations on top.

**Pick the right shape:**
- Replacing the whole highway drawing on the existing highway canvas (your renderer owns its rendering context / resources; `createHighway()` still owns the canvas element and the rAF loop)? → **setRenderer** (section 1). Enters the viz picker. Works in both the main player and per-panel under splitscreen.
- Adding a layer on top of whichever viz is active? → **Overlay** (section 2). Navbar toggle, not in the picker.

#### 1. setRenderer contract (slopsmith#36) — preferred

Plugins that want to replace the main highway's draw function (per panel, per session) export a renderer factory on `window.slopsmithViz_<id>` where `<id>` matches the `id` in `plugin.json` (`type: "visualization"` required). The factory returns an object matching this shape:

```js
window.slopsmithViz_my_viz = function () {
    return {
        init(canvas, bundle) {
            // One-time setup. Own your getContext() call here —
            // acquire '2d' or 'webgl' depending on the renderer.
            this.ctx = canvas.getContext('2d');
        },
        draw(bundle) {
            // Called each requestAnimationFrame tick by the factory.
            // `bundle` is a snapshot with: currentTime, songInfo, isReady,
            // notes, chords, anchors (all difficulty-filter-aware),
            // beats, sections, chordTemplates, stringCount, lyrics,
            // toneChanges, toneBase, mastery, hasPhraseData, inverted,
            // lefty, renderScale, lyricsVisible, plus the 2D coordinate
            // helpers project and fretX. `stringCount` is the active
            // arrangement's string count (4 for bass, 6 for guitar,
            // 7+ for extended-range GP imports — size string-indexed
            // geometry against this, not a hardcoded 6). If your
            // renderer needs lefty-aware text rendering, check
            // bundle.lefty and apply the mirror transform yourself —
            // a bundle-level helper isn't provided because it would
            // need your renderer's own context, not the factory's.
        },
        resize(w, h) {
            // Optional. Canvas dims already updated; re-create WebGL
            // framebuffers / reset 2D transforms here.
        },
        destroy() {
            // Optional. Release resources, remove DOM nodes, null refs.
            // Called before setRenderer() swaps to another renderer
            // and on highway.stop().
        },
    };
};
```

Selecting this plugin in the main-player viz picker — or in splitscreen's per-panel picker — calls `highway.setRenderer(factory())` on the existing highway instance. The built-in 2D highway is the default renderer and is restored by passing nullish — `setRenderer(null)` and `setRenderer(undefined)` both work (the implementation gates on `r == null`). Splitscreen panels create one `createHighway()` per panel and each independently consults the picker, so N panels can run different renderers (or N copies of the same renderer with different arrangements) without coordination.

**Lifecycle contract.** The factory returns a single renderer instance that may go through multiple `init() → ... → destroy()` cycles as the user navigates between songs or screens. Specifically:

- `init(canvas, bundle)` runs when the highway has a canvas and the renderer takes over drawing. This is when to acquire `getContext()`, build shaders / meshes / DOM nodes, and register listeners.
- `draw(bundle)` runs on every rAF frame once the WebSocket `ready` message has fired and until the renderer is replaced or the highway stops. It is **not** called during the loading / reconnect window (between `api.init()` + `stop()` and the next `ready`) — that would hand the renderer half-populated chart arrays. Renderers that want to show a "loading" state can read `bundle.isReady` inside a future-widened contract, but today the factory gates `draw` behind the ready flag and `isReady` is only informational once it does fire.
- `destroy()` runs when the renderer is replaced via another `setRenderer(...)` call, OR when `highway.stop()` is called (e.g. the user navigates away from the player). It releases everything `init()` acquired.
- **After `destroy()`, the same instance may receive another `init()` call** — this happens on `playSong()` which does `stop()` → `init()` to reuse the same canvas element for the next song. Renderers must tolerate `init()` being called again on an instance that was previously destroyed. Practically: null your refs in destroy, re-acquire them in init.
- `destroy()` is skipped when it would run on an un-init'd renderer — if a caller does `setRenderer(x)` before the highway ever init'd (possible when restoring a saved picker selection at page load), `x.destroy()` is not called until `x.init()` has run at least once.
- `resize(w, h)` is optional; runs after init and whenever the canvas dimensions change.

**Key rules:**
- The factory **returns a fresh object on each call** — important for splitscreen, where multiple panels will each get an independent instance.
- The renderer **owns its own rendering context** (2D or WebGL). Factory will not call getContext for you.
- **Canvas context caveat — "first context wins".** Browsers lock a canvas element to the first context type successfully acquired for its lifetime: once `getContext('2d')` succeeds, `getContext('webgl')` on that same canvas returns `null`, and vice versa. This produces two asymmetric cases with the renderer picker:
  - A WebGL renderer *can* work if it is selected **before** `createHighway().init()` has a chance to acquire a 2D context — the restore-saved-selection path in app.js calls `setRenderer` at page load, which stashes the choice; the first `highway.init()` that follows will then install the WebGL renderer directly, and `_defaultRenderer` never grabs a 2D context on that canvas.
  - But if the default renderer already owns the canvas (the usual case — user picks WebGL from the dropdown mid-session), switching to WebGL on that same canvas fails. The reverse fails too: a canvas that started with a WebGL renderer can't switch back to the default 2D.
  Supporting arbitrary swaps between 2D and WebGL therefore requires recreating or replacing the canvas element when the context type changes — out of scope for Wave A, but the restore-at-load path is a viable escape hatch for WebGL viz authors today.
- `draw(bundle)` receives difficulty-filtered arrays — never read from `_filteredNotes` or other internals.
- `_drawHooks` fire only for the default 2D renderer. Custom renderers handling their own compositing should not expect them.

**Auto mode — `matchesArrangement(songInfo)` (optional).**

The viz picker prepends an "Auto (match arrangement)" entry that is the default selection on fresh installs. When Auto is active, core evaluates registered viz factories on every `song:ready` and swaps the renderer to the first factory whose `matchesArrangement(songInfo)` predicate returns truthy. No match → the built-in 2D highway.

Declare the predicate as a static on the factory (not the instance) so core can evaluate it without constructing a throwaway renderer:

```js
window.slopsmithViz_piano = function () { /* ... */ };
window.slopsmithViz_piano.matchesArrangement = function (songInfo) {
    return /keys|piano|synth/i.test((songInfo && songInfo.arrangement) || '');
};
```

- `songInfo` is the highway's live song_info snapshot — `arrangement`, `tuning`, `capo`, `arrangement_index`, `filename`, `artist`, `title`, etc. May be `{}` before the first song loads.
- Factories without `matchesArrangement` are skipped during auto-selection — the correct default for arrangement-agnostic viz (tabview, jumpingtab) that only make sense as manual picks.
- Explicit picker selections override Auto and are persisted to `localStorage.vizSelection`, so the pinned choice survives page reloads until the user switches back to "Auto" (which also persists). Picking "Auto" re-evaluates against the current song immediately. In contexts where `localStorage` is unavailable (private mode, sandboxed iframes, some test runners) persistence falls back to the current picker `<option>` value, which still overrides Auto for as long as the page stays loaded.
- When an Auto-selected renderer fails and core emits `viz:reverted`, the picker falls back to the built-in default and disables auto-switching until the user re-selects Auto.
- First match wins (picker order), so the registration order of plugins is the tiebreaker. Keep predicates narrow to avoid stealing songs from more specialized viz.

**Known limitation — WebGL viz in Auto mode.** Auto's evaluation happens on `song:ready`, which fires AFTER `highway.init()` has already given the canvas to the default 2D renderer. Installing a WebGL viz at that point fails because the canvas is locked to 2D (see the "first context wins" caveat above). Conversely, reverting from a WebGL-active Auto pick to the default 2D on a no-match song will silently blank the player. Both cases are the same canvas-lock limitation manual picker swaps already have. A future wave will teach highway to recreate the canvas on context-type change. For now, WebGL viz should either be pinned explicitly via the picker (which stores the choice pre-`highway.init()` on reload) or skip `matchesArrangement`.

#### 2. Overlay contract — for add-on layers

Plugins that add a layer on top of whichever visualization is active — HUDs, fretboard diagrams, chord labels, practice feedback — don't replace the renderer. They manage their own canvas, their own rAF loop, and a toggle button somewhere visible (typically a navbar pill), reading public highway state via the getters:

- `highway.getTime()` / `highway.getBeats()` — current playback position
- `highway.getNotes()` / `highway.getChords()` — difficulty-filter-aware arrays
- `highway.getChordTemplates()` — chord shape lookup table; index by `chord.id` from `getChords()` to get `{ name, fingers, frets }`. `fingers` and `frets` are per-string arrays (length matches the tuning's string count); within `fingers`, `-1` = unused, `0` = open string, `n > 0` = finger number. RS XML sources populate real fingerings; GP imports currently emit all `-1` since pre-import sources don't carry finger data. Not filter-aware: templates are static metadata, every `chord_id` referenced by `getChords()` is guaranteed valid
- `highway.getSongInfo()` — tuning, arrangement, capo
- `highway.getStringCount()` — number of strings on the active arrangement (4 for bass, 6 for guitar, 7+ for extended-range GP imports). Derived server-side as `max(notes-max-string + 1, name-based fallback, len(tuning))` where the tuning length only contributes when it isn't the RS-XML padded 6-string form (sloppak / GP-imported sources carry trimmed tuning lengths). The name-based fallback is 4 for arrangements containing "bass" (case-insensitive) and 6 otherwise. This combination handles partial-string-usage charts (a 6-string lead that never plays string 5), extended-range GP imports (5-string bass, 7-string guitar), and sloppaks that explicitly encode the instrument range — without requiring plugins to do their own arrangement-name matching
- `highway.getLefty()` / `highway.getInverted()` — mirror + invert state

Overlays do NOT appear in the viz picker and do NOT declare `"type": "visualization"` in `plugin.json`. They coexist with whichever renderer (default 2D, 3D highway, piano, ...) the user has picked.

**Key rules:**
- **Own your rAF + canvas** — don't piggyback on `_drawHooks` (those only fire for the default 2D renderer) or on `createHighway`'s rendering context.
- **Re-read state every frame** — overlay output must track whatever the current renderer is drawing. Don't cache note positions across frames.
- **Respect lefty + invert toggles** — if the overlay depicts strings or frets, mirror using the same transforms the active renderer would.
- **Clean up on toggle-off** — cancel rAF and remove/hide the overlay canvas so inactive overlays aren't wasting frames.

Reference: [fretboard plugin](https://github.com/byrongamatos/slopsmith-plugin-fretboard) — canonical overlay implementation (navbar toggle, own canvas, 80ms active-note window).

**Why two?** setRenderer plugs into an existing highway — main-player or splitscreen-panel — reusing its WebSocket and data parsing, so the common "I want a different look for the same data" case is zero boilerplate AND multi-instance for free. Overlays compose with whatever renderer is active — they decorate rather than replace, so multiple can stack (fretboard + chord labels + practice feedback) without fighting over the canvas.

A previous standalone-pane contract (`window.createMyVisualization({ container })` with its own WebSocket per pane) was used by splitscreen pre-Wave-C. It's been retired now that splitscreen calls `setRenderer` on per-panel `createHighway()` instances; if you find references in older plugin docs or external integration guides, those describe the legacy path.

### General plugin guidelines

- Wrap your plugin code in an IIFE: `(function () { 'use strict'; ... })();`
- Use `localStorage` for user-facing settings, prefixed with your plugin id
- If hooking `window.playSong`, always call the original and `await` it
- If hooking `window.showScreen`, clean up your state when leaving the player screen
- Use `window.slopsmith.emit()` / `window.slopsmith.on()` for inter-plugin communication

## Song Formats

Slopsmith supports two song formats:

### PSARC (Rocksmith native)
The original Rocksmith 2014 archive format. Contains encrypted SNG note data, WEM audio, album art, and tone presets. Read-only — Slopsmith does fast metadata scanning in-memory via `lib/psarc.py` (`read_psarc_entries`) without fully unpacking the archive, but playback and conversion paths extract the PSARC to a temporary directory via `unpack_psarc()` before loading note/audio assets. Audio is decoded via `vgmstream-cli` + `ffmpeg`.

### Sloppak (open format)
An open, hand-editable song package designed for Slopsmith. Exists in two interchangeable forms:
- **Zip archive** (`.sloppak` file) — distribution form
- **Directory** (`.sloppak/` folder) — authoring form

**Contents:**
```
manifest.yaml          Song metadata (title, artist, album, duration, tuning, arrangement IDs, ...)
arrangements/
  lead.json            Note/chord/anchor data in wire format (see song.py)
  rhythm.json          Files here are driven by manifest.yaml arrangement entries
  ...                  (e.g. arrangements/<arrangement-id>.json)
stems/
  full.ogg             Mixed audio (always present)
  guitar.ogg           Individual stems (optional, from Demucs split)
  bass.ogg
  drums.ogg
  vocals.ogg
  piano.ogg
  other.ogg
cover.jpg              Album art (optional)
lyrics.json            Syllable-level lyrics (optional)
```

Sloppak is the preferred format for new features. The [Sloppak Converter plugin](https://github.com/topkoa/slopsmith-plugin-sloppak-converter) converts PSARCs to sloppak, and the [Stems plugin](https://github.com/topkoa/slopsmith-plugin-stems) provides live stem mixing for sloppak songs.

**Full developer reference:** [docs/sloppak-spec.md](docs/sloppak-spec.md) — manifest schema, arrangement wire format, and how to extend the format with new data types (drum tab, key/scale annotations, etc.).

**Key code:**
- `lib/sloppak.py` — format detection, zip/directory resolution, metadata extraction, song loading
- `lib/sloppak_convert.py` — PSARC to sloppak conversion pipeline, Demucs stem splitting
- `lib/song.py` — shared data models (`Note`, `Chord`, `Arrangement`, `Song`) and wire format serialization used by both formats

## Frontend Conventions

- **No frameworks** — vanilla JS, fetch API, DOM manipulation
- **Globals** — `highway`, `audio`, `playSong()`, `showScreen()`, `createHighway()`, `window.slopsmith`
- **Storage** — `localStorage` for all user preferences
- **Styling** — Tailwind CSS utility classes, dark theme (`bg-dark-600`, `text-gray-300`, accent `#4080e0`, gold `#e8c040`)
- **Naming** — camelCase for JS functions, kebab-case for CSS classes, snake_case for plugin IDs
- **Player layout** — `#player` is `display:flex; flex-direction:column; position:fixed; inset:0`. `#highway` is `flex:1`. `#player-controls` sits at the bottom. Hiding the highway collapses the layout — use `margin-top: auto` on controls if you need to hide it.

## Backend Conventions

- **Framework** — FastAPI with uvicorn
- **Imports** — flat imports from `lib/` (no package `__init__.py`): `from song import Song`
- **Database** — SQLite via MetadataDB class with `threading.Lock` for thread safety
- **WebSocket** — JSON frames, try/except `WebSocketDisconnect`
- **Error handling** — graceful fallbacks (audio conversion errors don't crash the song, missing art returns placeholder)
- **Type hints** — used sparingly (`Path | None`, `dict`, `list`)
- **Docstrings** — minimal; code is self-documenting

## Testing

```bash
pytest                         # Run all tests
pytest tests/test_song.py -v   # Specific file
pytest -k "round_trip" -v      # Pattern match
```

- Framework: pytest
- Config: `pyproject.toml` sets `pythonpath = [".", "lib"]` and `testpaths = ["tests"]`
- CI: GitHub Actions runs pytest on push/PR to main (Python 3.12)
- Test dependencies: `requirements-test.txt`

## Versioning

- **`VERSION`** (repo root) — single source of truth; plain semver string (e.g. `0.2.4`). Bind-mounted into the container and copied by the Dockerfile so it's always available at `/app/VERSION`.
- **`GET /api/version`** — returns `{"version": "<contents of VERSION>"}`. Displayed as a badge in the navbar.
- **Auto-sync** — `.github/workflows/sync-version.yml` rewrites `VERSION` via a `repository_dispatch` (`desktop-released`) fired from `slopsmith-desktop`'s release job. As an explicit automation-only exception to the "Never push directly to main" rule in Git Workflow below, the sync job commits straight to `main` as `github-actions[bot]` (version bumps are mechanical; the PR round-trip adds no signal). Human contributors must still go through feature branches + PRs. No manual VERSION edits needed. Use the workflow's `workflow_dispatch` trigger with `version: X.Y.Z` for manual runs (recovery / out-of-band bumps).
- **`CHANGELOG.md`** — follows [Keep a Changelog](https://keepachangelog.com/) format. Update the `[Unreleased]` section with each PR; when `slopsmith-desktop` cuts a release, rename `[Unreleased]` to the new version + date (the VERSION bump itself is automated).

## Git Workflow

- **Never push directly to main** — always create a feature branch and open a PR
- **Upstream remote** — set `upstream` to the canonical Slopsmith repository; `origin` is your fork
- **Plugins are gitlinks** — each plugin in `plugins/` is typically its own git repo (submodule or clone). Branch switches on the main repo can clobber plugin directories. Use `git update-index --assume-unchanged` for plugin dirs if needed.
- **Commit style** — short imperative subject line, blank line, then body explaining *why*

## WebSocket Protocol Reference

The highway WebSocket at `/ws/highway/{filename}?arrangement={index}` streams these messages in order:

| Message | Shape | Description |
|---------|-------|-------------|
| `loading` | `{ type: 'loading', stage }` | Status/progress message during extraction or conversion |
| `song_info` | `{ type, title, artist, arrangement, arrangement_index, arrangements, duration, tuning, capo, format, audio_url, audio_error, stems }` | Song metadata. `arrangements` is the full list for the switcher. `audio_url` is `null` when audio is unavailable, in which case `audio_error` is non-null; otherwise `audio_error` is `null`. `stems` is always present — an empty array for non-sloppak songs or sloppak songs with no split stems. `tuning` is an array (6 for guitar, 4 for bass). |
| `beats` | `{ type, data: [{ time, measure }] }` | Beat timestamps with measure numbers |
| `sections` | `{ type, data: [{ time, name }] }` | Named sections (Intro, Verse, Chorus, etc.) |
| `anchors` | `{ type, data: [{ time, fret, width }] }` | Fret zoom anchors |
| `chord_templates` | `{ type, data: [{ name, frets: [6] }] }` | Named chord shapes |
| `lyrics` | `{ type, data: [{ w, t, d }] }` | Syllables: `w`=word, `t`=time, `d`=duration. `-` joins to previous, `+` = line break |
| `tone_changes` | `{ type: 'tone_changes', base, data: [{ time, name }] }` | Optional — tone change events relative to the arrangement base tone; only sent if tones were found |
| `notes` | `{ type, data: [{ t, s, f, sus, ho, po, sl, bn, ... }] }` | Single notes |
| `chords` | `{ type, data: [{ t, notes: [{ s, f, sus, ... }] }] }` | Chord events |
| `phrases` | `{ type, data: [{ start_time, end_time, max_difficulty, levels: [{ difficulty, notes, chords, anchors, handshapes }] }], total }` | Optional — per-phrase difficulty ladder for master-difficulty slider (slopsmith#48). Only sent when the source chart carries multi-level phrase data (PSARC / phrase-aware sloppak). Sent in chunks (`data` is a batch, `total` is the full count across messages) to avoid multi-MB single frames. Absent for GP imports and legacy sloppak; consumers must treat missing message as "single fixed difficulty — slider disabled". |
| `ready` | `{ type: 'ready' }` | All data sent — safe to finalize and start rendering |

Message delivery is incremental. You may receive `loading` updates and `lyrics` before note/chord payloads; `tone_changes` comes after `lyrics` when present and may be omitted entirely. Do not finalize rendering until you receive `ready`.

## Common Pitfalls

1. **playSong wrapper race condition** — The wrapper chain runs outermost-first (last-loaded wrapper runs first). If an inner plugin (e.g. `3dhighway`) does `await import(CDN)`, it yields to the event loop. WebSocket messages (`song_info`, `ready`) can arrive before outer plugins set their callbacks. Use `getSongInfo()` as a fallback rather than relying solely on `_onReady`.

2. **Plugin gitlinks** — Plugins are separate git repos cloned into `plugins/`. Switching branches on the main repo can delete or clobber these directories. Be careful with `git checkout` and `git clean`.

3. **Highway flex layout** — `#highway` has `flex:1` in the player. Hiding it with `display:none` removes the flex child, causing `#player-controls` to float to the top. If you hide the highway, add `margin-top: auto` to the controls div to keep it at the bottom.

4. **Multiple WebSocket connections** — The server supports many simultaneous WebSocket connections to the same song. Split screen panels, lyrics panes, and jumping tab panes each open their own. This is by design — don't try to multiplex.

5. **Plugin load order** — Plugins load alphabetically by directory name. This determines the `playSong` wrapper chain order and which plugin's UI elements appear first. If your plugin depends on another's globals, check at runtime (`typeof window.X === 'function'`), not at load time.
