#!/usr/bin/env python3
"""Transcribe a standard-notation score PDF/image (弹唱谱, NO six-line tab)
into a two-track .gp5: melody (staff → ABC → notes) + chord accompaniment
(chord symbols → strum pattern). Uses Bailian (Qwen-VL) via curl like
convert_one.py, but with staff-notation prompts.

Usage (from the pic2guitarpro repo root):
    python scripts/score_to_gp5.py <image_or_pdf> <out_dir> [--title TITLE] [--artist ARTIST]

Outputs <out_dir>/<stem>/<stem>.gp5 plus <stem>.txt (the raw ABC + chord
transcription, for inspection). The .gp5 title/artist are left EMPTY on
purpose: slopsmith's convert_gp_to_sloppak.py falls back to the filename
stem, so name the file `<title>-<artist>.gp5` to get proper library metadata.
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))
import guitarpro  # noqa: E402  (PyGuitarPro, editable install)
from guitarpro import models as m  # noqa: E402

API_KEY = os.environ.get("BAILIAN_API_KEY", "sk-f99e08d3fa024d3ba7d5ee27e8d4700a")
MODEL = os.environ.get("BAILIAN_MODEL", "qwen3.6-plus")
BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"

_QUARTER = m.Duration.quarterTime  # 960
_BAR = _QUARTER * 4
_EIGHTH = _QUARTER // 2
# ABC length digits are MULTIPLIERS of the default L:1/8, not denominators:
# 1=eighth, 2=quarter, 3=dotted quarter, 4=half, 8=whole.
_DURATION_TICKS = {1: _EIGHTH, 2: _QUARTER, 3: _QUARTER + _EIGHTH,
                   4: _QUARTER * 2, 6: _QUARTER * 3, 8: _BAR, 16: _BAR * 2,
                   32: _BAR * 4}

# String index (1..6) -> open-string MIDI pitch, standard tuning.
_STRING_OPEN = {1: 64, 2: 59, 3: 55, 4: 50, 5: 45, 6: 40}

NOTE_PC = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}

MELODY_SYSTEM = """\
You are an expert music engraving transcriber. You are given an image of a \
printed Chinese pop-song score: standard five-line staff notation carrying \
the melody, chord symbols printed above the staff, and lyrics below.

Transcribe ONLY the melody from the staff into ABC notation. Output ONLY the \
ABC text — no prose, no markdown fences, no commentary.

Rules:
- Begin with the header lines: X:1, K:C, M:4/4, L:1/8, Q:1/4=<tempo bpm> \
  (your best estimate of the song tempo).
- C D E F G A B = middle octave; c d e f g a b = one octave above; letters \
  followed by a comma = one octave below; apostrophe = two octaves above.
- Length digits are MULTIPLIERS of L:1/8: 2 = quarter note, 3 = dotted \
  quarter, 4 = half note, 8 = whole note. Write the digit after a note or \
  rest when its duration differs from L:1/8. Dotted notes are allowed \
  (e.g. c2.).
- One measure per | pair, ending every line with |, including the last.
- Use z rests (with a length digit when longer than an eighth).
- Ignore lyrics, chord symbols, title text, and repeats; the melody only.
- Transcribe EVERY measure of every system from top to bottom — never \
  truncate, never summarize, never skip a system.
- The image always contains clearly readable staff notation. Never output \
  rests for an entire section. If an individual note is unclear, use your \
  best judgment.
"""

MELODY_USER = ("Transcribe the COMPLETE melody visible in this image to ABC "
               "notation, following the system prompt. Output only the ABC "
               "text.")

MELODY_USER_V2 = ("Look carefully at every staff system in this image, top to "
                  "bottom, and transcribe the melody note-by-note into ABC "
                  "notation, following the system prompt. Do not skip any "
                  "system. Output only the ABC text.")

CHORD_SYSTEM = """\
You are an expert chord chart reader. You are given an image of a printed \
Chinese pop-song score with chord symbols printed above the staff (for \
example Em, C, G, D, Bm7, Am7).

List the chord progression in order, ONE chord per measure, reading left to \
right and top to bottom, as a single line of plain text separated by spaces, \
for example:

Em C G D Am7 Bm7

Rules:
- The chord symbols are the small bold letters printed ABOVE each system's \
  top staff line (like Em, C, G, D, Bm7, Am7). Read each one carefully — \
  they are the only letters above the staff.
- Keep the chord spelling exactly as printed (m, 7, m7, maj7, sus2, sus4, \
  add9, and slash chords like C/G).
- If two chord symbols are printed inside one measure, keep only the first.
- If a measure has no printed chord, output - for it.
- Cover EVERY measure of every system; never skip a system.
- Output ONLY that one line — no prose, no fences, no explanations.
"""

CHORD_USER = ("Output the chord progression of this score, one chord per "
              "measure, plain text only.")


def call_qwen(system: str, user: str, image_path: Path,
              max_edge: int = 1600, timeout: int = 600) -> str | None:
    """One vision call to the Bailian qwen endpoint. Returns text or None."""
    im = Image.open(image_path).convert("RGB")
    im.thumbnail((max_edge, max_edge))
    buf = io.BytesIO()
    im.save(buf, format="JPEG", quality=90)
    data_uri = "data:image/jpeg;base64," + base64.standard_b64encode(buf.getvalue()).decode()

    payload = json.dumps({
        "model": MODEL,
        "max_tokens": 8192,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": data_uri}},
                {"type": "text", "text": user},
            ]},
        ],
    })

    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        f.write(payload)
        pf = f.name
    try:
        for attempt in range(2):
            try:
                result = subprocess.run(
                    ["curl", "-s", "--max-time", str(timeout), "-X", "POST",
                     BASE_URL, "-H", "Content-Type: application/json",
                     "-H", f"Authorization: Bearer {API_KEY}", "-d", f"@{pf}"],
                    capture_output=True, text=True, timeout=timeout + 30,
                )
                if result.returncode != 0:
                    print(f"    curl error (attempt {attempt+1}): {result.stderr[:100]}")
                    continue
                data = json.loads(result.stdout)
                if "choices" in data:
                    return data["choices"][0]["message"]["content"].strip()
                print(f"    API error: {data.get('error', str(data)[:120])}")
                return None
            except (subprocess.TimeoutExpired, Exception) as e:
                print(f"    attempt {attempt+1} failed: {e}")
        return None
    finally:
        try:
            os.unlink(pf)
        except OSError:
            pass


# ── ABC parsing ─────────────────────────────────────────────────────────────

def _abc_pitch(token: str) -> int | None:
    """ABC pitch token ('c', "C'", 'A,') → MIDI pitch, or None."""
    mch = re.match(r"([A-Ga-g])([',]*)$", token)
    if not mch:
        return None
    # lowercase = C5 (72), uppercase = C4 (60); , → octave down, ' → up.
    pitch = NOTE_PC[mch.group(1).upper()] + (72 if mch.group(1).islower() else 60)
    for ch in mch.group(2):
        pitch += 12 if ch == "'" else -12
    return pitch


def _has_notes(text: str) -> bool:
    """True when the ABC contains at least one actual note (model responses
    that are all rests/z mean the transcription gave up)."""
    return bool(re.search(r"[A-Ga-g][',]*\d?", re.sub(r"^[A-Za-z]:.*$", "", text, flags=re.M)))


def parse_abc(text: str):
    """Parse ABC melody text → (bars, tempo). Each bar is a list of
    (midi_pitch_or_None, ticks); None is a rest."""
    bars: list[list[tuple[int | None, int]]] = []
    tempo = 72
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith(("%", "```", "X:", "K:", "M:", "L:",
                                        "T:", "C:", "H:", "O:", "w:", "W:")):
            continue
        if line.startswith("Q:"):
            mm = re.search(r"=\s*(\d+)", line)
            if mm:
                tempo = int(mm.group(1))
            continue
        cur: list[tuple[int | None, int]] = []
        i = 0
        while i < len(line):
            ch = line[i]
            if ch in " \t":
                i += 1
                continue
            if ch == "|":
                bars.append(cur)
                cur = []
                i += 1
                continue
            mm = re.match(r"(?:\^|_|=)?([A-Ga-gzZ])([',]*)(\d{1,2})?(/?\d{1,2})?(\.?)(-?)", line[i:])
            if not mm:
                i += 1  # skip unparseable char
                continue
            i += mm.end()
            pitch = None if mm.group(1).lower() == "z" else _abc_pitch(mm.group(1) + mm.group(2))
            # No digit = ×1 of L:1/8 = one eighth (NOT the L: value itself).
            length = int(mm.group(3)) if mm.group(3) else 1
            ticks = _DURATION_TICKS.get(length, _EIGHTH)
            if mm.group(4):  # A/2 = halved value (eighth → sixteenth)
                ticks //= int(mm.group(4)[1:])
            if mm.group(5):
                ticks += ticks // 2
            cur.append((pitch, ticks))
        if cur:
            bars.append(cur)
    return bars, tempo


# ── Chord handling ──────────────────────────────────────────────────────────

_CHORD_TOKEN_RE = re.compile(
    r"^[A-G](?:#|b)?(?:maj7|m7b5|m7|maj|m6|m|7|6|9|add9|sus2|sus4|dim|aug)?"
    r"(?:/[A-G](?:#|b)?)?$", re.IGNORECASE)

_QUALITY_INTERVALS = {
    "": (0, 4, 7), "m": (0, 3, 7), "maj": (0, 4, 7), "maj7": (0, 4, 7, 11),
    "7": (0, 4, 7, 10), "m7": (0, 3, 7, 10), "m6": (0, 3, 7, 9),
    "6": (0, 4, 7, 9), "9": (0, 4, 7, 10, 14), "add9": (0, 4, 7, 14),
    "sus2": (0, 2, 7), "sus4": (0, 5, 7), "dim": (0, 3, 6),
    "aug": (0, 4, 8), "m7b5": (0, 3, 6, 10),
}


def parse_chord(symbol: str) -> tuple[list[int], int | None] | None:
    """Chord symbol → (pitch classes, bass pc or None)."""
    mm = re.match(r"^([A-G])(#|b)?(maj7|m7b5|m7|maj|m6|m|7|6|9|add9|sus2|sus4|dim|aug)?(?:/([A-G])(#|b)?)?$",
                  symbol, re.IGNORECASE)
    if not mm:
        return None
    root = NOTE_PC[mm.group(1).upper()]
    if mm.group(2) == "#":
        root += 1
    elif mm.group(2) == "b":
        root -= 1
    quality = (mm.group(3) or "").lower()
    intervals = _QUALITY_INTERVALS.get(quality)
    if intervals is None:
        return None
    bass = None
    if mm.group(4):
        bass = NOTE_PC[mm.group(4).upper()]
        if mm.group(5) == "#":
            bass += 1
        elif mm.group(5) == "b":
            bass -= 1
    return sorted({(root + iv) % 12 for iv in intervals}), bass


def chord_shape(pcs: list[int], bass: int | None = None) -> tuple[list[int], int]:
    """Greedy fretted shape (strings 6→1, frets 0-11); -1 = muted string.
    Returns (strings, firstFret)."""
    shape = []
    chosen = []
    for s in range(6, 0, -1):
        open_pc = _STRING_OPEN[s] % 12
        fret = None
        for f in range(0, 12):
            if (open_pc + f) % 12 in pcs:
                fret = f
                break
        if fret is not None and (s == 6 or fret <= 4 or (chosen and fret - max(chosen) <= 4)):
            shape.append(fret)
            chosen.append(fret)
        else:
            shape.append(-1)
    # Slash chord: force the bass pitch class onto the lowest sounding string.
    if bass is not None:
        for s in range(6, 0, -1):
            if shape[6 - s] >= 0:
                open_pc = _STRING_OPEN[s] % 12
                for f in range(0, 12):
                    if (open_pc + f) % 12 == bass:
                        shape[6 - s] = f
                        break
                break
    sounding = [f for f in shape if f >= 0]
    return shape, (min(sounding) if sounding else 1)


# ── Song building ───────────────────────────────────────────────────────────

def melody_fret(pitch: int) -> tuple[int, int]:
    """Pick (string, fret) for a melody pitch: lowest fret on strings 4→1."""
    best = None
    for s in (4, 3, 2, 1):
        fret = pitch - _STRING_OPEN[s]
        if 0 <= fret <= 17 and (best is None or fret < best[1]):
            best = (s, fret)
    if best is None:  # very low: fall back to strings 6/5
        for s in (6, 5):
            fret = pitch - _STRING_OPEN[s]
            if 0 <= fret <= 17:
                return s, fret
        return 1, 0
    return best


def build_song(melody_bars: list[list[tuple[int | None, int]]],
               chord_syms: list[str],
               chord_parsed: list[tuple[list[int], int | None] | None],
               tempo: int) -> m.Song:
    n = max(len(melody_bars), len(chord_syms))
    song = m.Song()
    song.measureHeaders.clear()

    start = _QUARTER
    for i in range(n):
        header = m.MeasureHeader()
        header.number = i + 1
        header.start = start
        song.addMeasureHeader(header)
        start += _BAR

    song.tempo = tempo

    def make_track(number: int, name: str) -> m.Track:
        track = m.Track(song, number=number)
        track.name = name
        track.measures.clear()
        for header in song.measureHeaders:
            measure = m.Measure(track, header)
            voice = measure.voices[0]
            idx = header.number - 1

            if number == 1:  # melody
                used = 0
                for pitch, ticks in melody_bars[idx] if idx < len(melody_bars) else []:
                    if used + ticks > _BAR:
                        break  # drop overflow (model over-filled the bar)
                    beat = m.Beat(voice)
                    beat.duration = m.Duration.fromTime(ticks)
                    if pitch is None:
                        beat.status = m.BeatStatus.rest
                    else:
                        beat.status = m.BeatStatus.normal
                        s, fret = melody_fret(pitch)
                        note = m.Note(beat)
                        note.type = m.NoteType.normal
                        note.string = s
                        note.value = fret
                        beat.notes.append(note)
                    voice.beats.append(beat)
                    used += ticks
                if used < _BAR:
                    # Pad the bar with rests; the remainder may not be a
                    # single duration, so decompose greedily.
                    remaining = _BAR - used
                    for dur in (m.Duration.whole, m.Duration.half,
                                m.Duration.quarter, m.Duration.eighth,
                                m.Duration.sixteenth, m.Duration.thirtySecond):
                        t = int(dur)
                        while remaining >= t:
                            beat = m.Beat(voice)
                            beat.status = m.BeatStatus.rest
                            beat.duration = m.Duration(dur)
                            voice.beats.append(beat)
                            remaining -= t
            else:  # chord accompaniment: 8 eighth-note strums per bar
                sym = chord_syms[idx] if idx < len(chord_syms) else "-"
                chord = chord_parsed[idx] if idx < len(chord_parsed) else None
                if (sym == "-" or chord is None) and idx > 0:
                    sym, chord = chord_syms[idx - 1], chord_parsed[idx - 1]
                elif sym == "-" and chord is None:
                    # Leading '-' with nothing to carry: find the first real chord.
                    for s, c in zip(chord_syms, chord_parsed):
                        if s != "-":
                            sym, chord = s, c
                            break
                shape = chord_shape(*chord) if chord is not None else None
                for b in range(8):
                    beat = m.Beat(voice)
                    beat.status = m.BeatStatus.normal
                    beat.duration = m.Duration(m.Duration.eighth)
                    beat.effect.pickStroke = (
                        m.BeatStrokeDirection.down if b % 2 == 0
                        else m.BeatStrokeDirection.up)
                    if b == 0 and chord is not None:
                        c = m.Chord(length=6)
                        c.name = sym
                        c.strings = shape[0]
                        c.firstFret = shape[1]
                        beat.effect.chord = c
                    if shape is not None:
                        for s, fret in enumerate(shape[0], start=1):
                            if fret >= 0:
                                note = m.Note(beat)
                                note.type = m.NoteType.normal
                                note.string = s
                                note.value = fret
                                beat.notes.append(note)
                    voice.beats.append(beat)
            track.measures.append(measure)
        return track

    melody_track = make_track(1, "Melody")
    chord_track = make_track(2, "Chords")
    song.tracks[0].measures.clear()  # default track replaced below
    song.tracks = [melody_track, chord_track]
    return song


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("src", help="score image or PDF (PDFs are rendered at 200 DPI)")
    ap.add_argument("out_dir", help="output dir for <stem>/<stem>.gp5")
    ap.add_argument("--title", default="", help="kept in the sidecar txt only")
    ap.add_argument("--artist", default="", help="kept in the sidecar txt only")
    ap.add_argument("--chords", default="",
                    help="manual chord override, one per measure, e.g. 'Em C G D';"
                         " '-' repeats the previous. Skips the chord API call.")
    ap.add_argument("--tempo", type=int, default=0,
                    help="override the model's tempo estimate (bpm)")
    args = ap.parse_args()

    src = Path(args.src).resolve()
    if not src.is_file():
        print(f"✗ not found: {src}", flush=True)
        return 1

    if src.suffix.lower() == ".pdf":
        import pymupdf
        doc = pymupdf.open(src)
        pages = []
        for i, page in enumerate(doc):
            p = src.with_name(f"{src.stem}_p{i+1:02d}.png")
            page.get_pixmap(dpi=200).save(p)
            pages.append(p)
        print(f"rendered {len(pages)} PDF page(s) → PNG", flush=True)
    else:
        pages = [src]

    # Tall pages (whole song on one page) get split into horizontal sections:
    # the model truncates long thin scores, and section crops also render the
    # staff notation larger. Each section is transcribed separately and the
    # ABC bodies are concatenated (header lines are skipped by parse_abc).
    sections: list[Path] = []
    for p in pages:
        im = Image.open(p)
        w, h = im.size
        if h > 1.2 * w:
            n = 4
            for i in range(n):
                a = max(0, h * i // n - 40)
                b = min(h, h * (i + 1) // n + 40)
                sec = p.with_name(f"{p.stem}_sec{i}.png")
                im.crop((0, a, w, b)).save(sec)
                sections.append(sec)
        else:
            sections.append(p)
    if len(sections) > 1:
        print(f"split tall page into {len(sections)} sections", flush=True)

    # 1. Transcribe (melody ABC + chord line). The chord symbols are small
    # letters above the staff — send a larger image for that call.
    abc_parts = []
    for sec in sections:
        # Skip blank sections: a score on one half of a tall page leaves the
        # other sections empty, and the model correctly returns rests there.
        im_sec = Image.open(sec).convert("L")
        sx, sy = im_sec.size
        dark = sum(1 for y in range(sy) for x in range(sx)
                   if im_sec.getpixel((x, y)) < 200)
        if dark < sx * sy * 0.005:
            print(f"melody section {sec.stem}: blank, skipping", flush=True)
            continue
        # Per-section cache: successful transcriptions are saved next to the
        # section image so re-runs (and API failures) resume cheaply.
        cache = sec.with_suffix(sec.suffix + ".abc")
        if cache.is_file() and _has_notes(cache.read_text(encoding="utf-8")):
            print(f"melody section {sec.stem}: using cached ABC", flush=True)
            abc_parts.append(cache.read_text(encoding="utf-8"))
            continue
        print(f"transcribing melody section {sec.stem} → ABC ...", flush=True)
        part = None
        for attempt, edge in enumerate((2200, 1800, 1500)):
            user = MELODY_USER if attempt % 2 == 0 else MELODY_USER_V2
            candidate = call_qwen(MELODY_SYSTEM, user, sec, max_edge=edge)
            # Reject "gave up" responses: nothing but rests means the model
            # did not read the staff, not that the section is blank.
            if candidate and _has_notes(candidate):
                part = candidate
                break
            print(f"    attempt {attempt + 1} gave no notes "
                  f"(reply: {candidate[:80]!r}), retrying...", flush=True)
        if not part:
            print(f"✗ melody transcription failed for {sec.name}", flush=True)
            return 1
        cache.write_text(part, encoding="utf-8")
        abc_parts.append(part)
    abc = "\n".join(abc_parts)
    if args.chords:
        chord_text = args.chords
        print("using manual chord override", flush=True)
    else:
        print("transcribing chords ...", flush=True)
        chord_text = call_qwen(CHORD_SYSTEM, CHORD_USER, sections[0], max_edge=2600)
        if not chord_text:
            print("✗ chord transcription failed", flush=True)
            return 1

    # 2. Parse.
    bars, tempo = parse_abc(abc)
    if not any(b for b in bars):
        print("✗ ABC produced no notes", flush=True)
        return 1
    if args.tempo:
        tempo = args.tempo
    chord_tokens = []
    for t in chord_text.split():
        tok = t.strip(" ,;.、，()[]")
        if tok == "-":
            chord_tokens.append("-")
        elif _CHORD_TOKEN_RE.match(tok):
            chord_tokens.append(tok)
    chord_parsed = [parse_chord(t) for t in chord_tokens]
    print(f"melody: {len(bars)} bars, tempo {tempo} bpm; chords: {len(chord_parsed)}", flush=True)

    # 3. Build and write.
    song = build_song(bars, chord_tokens, chord_parsed, tempo)
    song.title = ""          # convert_gp_to_sloppak falls back to filename stem
    song.artist = ""
    song.subtitle = "Transcribed from score images (pic2guitarpro score_to_gp5)"
    song.copyright = "2026"

    stem = src.stem
    out = Path(args.out_dir).resolve() / stem
    out.mkdir(parents=True, exist_ok=True)
    gp5_path = out / f"{stem}.gp5"
    guitarpro.write(song, str(gp5_path))
    (out / f"{stem}.txt").write_text(
        f"# {args.title or stem}{' - ' + args.artist if args.artist else ''}\n"
        f"# tempo: {tempo} bpm\n\n===== ABC (melody) =====\n{abc}\n\n"
        f"===== chords =====\n{chord_text}\n", encoding="utf-8")
    print(f"OK: {gp5_path} ({len(song.measureHeaders)} measures, 2 tracks)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
