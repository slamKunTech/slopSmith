"""Rocksmith 2014 arrangement XML parser and song data models."""

from dataclasses import dataclass, field
from pathlib import Path
import bisect
import json
import os
import subprocess
import xml.etree.ElementTree as ET


@dataclass
class Note:
    time: float
    string: int
    fret: int
    sustain: float = 0.0
    slide_to: int = -1
    slide_unpitch_to: int = -1
    bend: float = 0.0
    hammer_on: bool = False
    pull_off: bool = False
    harmonic: bool = False
    harmonic_pinch: bool = False
    palm_mute: bool = False
    mute: bool = False
    tremolo: bool = False
    accent: bool = False
    link_next: bool = False
    tap: bool = False


@dataclass
class ChordTemplate:
    name: str
    fingers: list[int]
    frets: list[int]


@dataclass
class Chord:
    time: float
    chord_id: int
    notes: list[Note] = field(default_factory=list)
    high_density: bool = False


@dataclass
class Anchor:
    time: float
    fret: int
    width: int = 4


@dataclass
class Beat:
    time: float
    measure: int  # -1 for non-downbeat


@dataclass
class Section:
    name: str
    number: int
    start_time: float


@dataclass
class HandShape:
    chord_id: int
    start_time: float
    end_time: float


@dataclass
class PhraseLevel:
    """One difficulty tier's worth of note/chord/anchor/hand-shape data for a
    single phrase iteration. Rocksmith's XML stores these as `<level
    difficulty="N">` blocks that repeat for every difficulty tier the chart
    author wrote; slopsmith used to collapse them to the phrase's
    maxDifficulty and throw the rest away. Keeping them around lets the
    highway render a "master difficulty" slider that picks a per-phrase
    difficulty tier at render time (slopsmith#48)."""

    difficulty: int
    notes: list[Note] = field(default_factory=list)
    chords: list[Chord] = field(default_factory=list)
    anchors: list[Anchor] = field(default_factory=list)
    hand_shapes: list[HandShape] = field(default_factory=list)


@dataclass
class Phrase:
    """One phrase iteration with every difficulty tier the source chart
    provided, scoped to the iteration's time range. `max_difficulty` is the
    phrase's authored cap — `levels` may contain entries at or below that
    cap (zero-indexed). The full-chart flat arrangement.notes/chords list
    is built from max-difficulty levels and is unchanged by this addition;
    phrases are additive metadata for the difficulty-slider consumer."""

    start_time: float
    end_time: float
    max_difficulty: int
    levels: list[PhraseLevel] = field(default_factory=list)


@dataclass
class Arrangement:
    name: str
    tuning: list[int] = field(default_factory=lambda: [0] * 6)
    capo: int = 0
    notes: list[Note] = field(default_factory=list)
    chords: list[Chord] = field(default_factory=list)
    anchors: list[Anchor] = field(default_factory=list)
    hand_shapes: list[HandShape] = field(default_factory=list)
    chord_templates: list[ChordTemplate] = field(default_factory=list)
    # None for single-level sources (GP converter, old sloppaks) — frontends
    # should treat a missing `phrases` as "no per-phrase difficulty data
    # available, disable the slider". Populated from Rocksmith XML when
    # multiple `<level>` tiers exist.
    phrases: list[Phrase] | None = None


@dataclass
class Song:
    title: str = ""
    artist: str = ""
    album: str = ""
    year: int = 0
    song_length: float = 0.0
    offset: float = 0.0
    beats: list[Beat] = field(default_factory=list)
    sections: list[Section] = field(default_factory=list)
    arrangements: list[Arrangement] = field(default_factory=list)
    audio_path: str = ""
    # Optional lyrics, one entry per syllable: {"t": float, "d": float, "w": str}
    lyrics: list[dict] = field(default_factory=list)


# ── Wire format serialization (shared between highway_ws and sloppak loader) ──
#
# These helpers produce/consume the same JSON shape the highway WebSocket streams
# to the client. They are the authoritative definition of the `.sloppak`
# arrangement file format — see `arrangements/*.json` inside a sloppak.

def note_to_wire(n: Note) -> dict:
    return {
        "t": round(n.time, 3), "s": n.string, "f": n.fret,
        "sus": round(n.sustain, 3),
        "sl": n.slide_to, "slu": n.slide_unpitch_to,
        "bn": round(n.bend, 1) if n.bend else 0,
        "ho": n.hammer_on, "po": n.pull_off,
        "hm": n.harmonic, "hp": n.harmonic_pinch,
        "pm": n.palm_mute, "mt": n.mute,
        "tr": n.tremolo, "ac": n.accent, "tp": n.tap,
    }


def chord_note_to_wire(cn: Note) -> dict:
    # Chord notes omit their own time (the chord carries it).
    d = note_to_wire(cn)
    d.pop("t", None)
    return d


def chord_to_wire(c: Chord) -> dict:
    return {
        "t": round(c.time, 3),
        "id": c.chord_id,
        "hd": c.high_density,
        "notes": [chord_note_to_wire(cn) for cn in c.notes],
    }


def note_from_wire(d: dict, time: float | None = None) -> Note:
    return Note(
        time=float(d.get("t", time if time is not None else 0.0)),
        string=int(d.get("s", 0)),
        fret=int(d.get("f", 0)),
        sustain=float(d.get("sus", 0.0)),
        slide_to=int(d.get("sl", -1)),
        slide_unpitch_to=int(d.get("slu", -1)),
        bend=float(d.get("bn", 0.0)),
        hammer_on=bool(d.get("ho", False)),
        pull_off=bool(d.get("po", False)),
        harmonic=bool(d.get("hm", False)),
        harmonic_pinch=bool(d.get("hp", False)),
        palm_mute=bool(d.get("pm", False)),
        mute=bool(d.get("mt", False)),
        tremolo=bool(d.get("tr", False)),
        accent=bool(d.get("ac", False)),
        tap=bool(d.get("tp", False)),
    )


def chord_from_wire(d: dict) -> Chord:
    t = float(d.get("t", 0.0))
    return Chord(
        time=t,
        chord_id=int(d.get("id", 0)),
        high_density=bool(d.get("hd", False)),
        notes=[note_from_wire(cn, time=t) for cn in d.get("notes", [])],
    )


def phrase_level_to_wire(pl: PhraseLevel) -> dict:
    return {
        "difficulty": pl.difficulty,
        "notes": [note_to_wire(n) for n in pl.notes],
        "chords": [chord_to_wire(c) for c in pl.chords],
        "anchors": [{"time": a.time, "fret": a.fret, "width": a.width} for a in pl.anchors],
        "handshapes": [
            {"chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time}
            for h in pl.hand_shapes
        ],
    }


def phrase_to_wire(p: Phrase) -> dict:
    return {
        "start_time": round(p.start_time, 3),
        "end_time": round(p.end_time, 3),
        "max_difficulty": p.max_difficulty,
        "levels": [phrase_level_to_wire(lv) for lv in p.levels],
    }


def phrase_level_from_wire(d: dict) -> PhraseLevel:
    return PhraseLevel(
        difficulty=int(d.get("difficulty", 0)),
        notes=[note_from_wire(n) for n in d.get("notes", [])],
        chords=[chord_from_wire(c) for c in d.get("chords", [])],
        anchors=[
            Anchor(time=float(a.get("time", 0)), fret=int(a.get("fret", 0)),
                   width=int(a.get("width", 4)))
            for a in d.get("anchors", [])
        ],
        hand_shapes=[
            HandShape(chord_id=int(h.get("chord_id", 0)),
                      start_time=float(h.get("start_time", 0)),
                      end_time=float(h.get("end_time", 0)))
            for h in d.get("handshapes", [])
        ],
    )


def phrase_from_wire(d: dict) -> Phrase:
    return Phrase(
        start_time=float(d.get("start_time", 0.0)),
        end_time=float(d.get("end_time", 0.0)),
        max_difficulty=int(d.get("max_difficulty", 0)),
        levels=[phrase_level_from_wire(lv) for lv in d.get("levels", [])],
    )


def arrangement_string_count(arr: Arrangement) -> int:
    """Derive the active arrangement's string count.

    Used by the server to emit ``stringCount`` in the song_info
    WebSocket payload (slopsmith-plugin-3dhighway#7).

    The RS XML schema always emits 6 ``<tuning>`` slots regardless
    of instrument (bass charts populate `string0`–`string3` and pad
    `string4`/`string5` with zeros), so ``len(arr.tuning)`` is not
    a reliable signal. Two independent signals get combined:

    1. **Notes-derived lower bound.** The highest string index
       referenced anywhere in notes + chord-notes, +1. A GP-imported
       7-string guitar with notes on strings 0..6 reports 7 here.
       But this is a LOWER BOUND only — a 6-string lead chart that
       never plays string 5 reports 5, undercounting by 1.

    2. **Name-based fallback.** Arrangements named "Bass" (case-
       insensitive substring match) default to 4; everything else
       defaults to 6. This catches the partial-string-usage case
       where notes don't span all the instrument's strings.

    A third signal — ``len(arr.tuning)`` when it isn't the RS-XML
    padded value of 6 — folds in for sloppak / GP-imported sources
    where the tuning array is explicitly trimmed (4 for bass, 5 for
    5-string bass, 7 for 7-string guitar, etc.). RS-XML / PSARC
    sources always emit length 6 regardless of instrument, so we
    deliberately ignore that exact value to avoid mis-classifying
    bass arrangements as guitar. ``< 6`` and ``> 6`` are both
    trustworthy signals.

    The result is ``max(notes_count, name_based, tuning_count)``
    where ``tuning_count`` is ``len(arr.tuning)`` when ``!= 6``,
    else 0. Worked examples:

    * RS XML 4-string bass, full usage (tuning len 6, notes 0..3) →
      max(4, 4, 0) = 4
    * RS XML 4-string bass, sparse usage (tuning len 6, notes 0..2) →
      max(3, 4, 0) = 4
    * RS XML 6-string lead, full usage (tuning len 6, notes 0..5) →
      max(6, 6, 0) = 6
    * RS XML 6-string lead, sparse usage (tuning len 6, notes 0..4) →
      max(5, 6, 0) = 6
    * Sloppak 5-string bass, sparse usage (tuning len 5, notes 0..3) →
      max(4, 4, 5) = 5
    * GP 7-string guitar (tuning len 7, notes 0..6) → max(7, 6, 7) = 7
    * GP 5-string bass (tuning len 5, notes 0..4) → max(5, 4, 5) = 5
    * Empty arrangement named "Bass" (tuning len 6) →
      max(0, 4, 0) = 4
    * Empty arrangement named "Lead" (tuning len 6) →
      max(0, 6, 0) = 6

    Topkoa's issue argues plugins shouldn't do arrangement-name
    matching; server-side fallback IS the right place for it
    because it gives plugins a single reliable ``stringCount`` to
    consume.
    """
    max_s = -1
    for n in arr.notes:
        if n.string > max_s:
            max_s = n.string
    for ch in arr.chords:
        for cn in ch.notes:
            if cn.string > max_s:
                max_s = cn.string
    notes_count = max_s + 1 if max_s >= 0 else 0
    name_based = 4 if "bass" in arr.name.lower() else 6
    # Tuning-length signal — only trustworthy when NOT the RS-XML
    # padded value of 6. Length 4/5 indicates explicit bass / 5-string
    # bass; length 7/8 indicates an extended-range guitar from GP.
    tuning_len = len(arr.tuning)
    tuning_count = tuning_len if tuning_len != 6 else 0
    return max(notes_count, name_based, tuning_count)


def arrangement_to_wire(arr: Arrangement) -> dict:
    """Serialize an Arrangement into a JSON-ready dict matching the wire format."""
    out = {
        "name": arr.name,
        "tuning": list(arr.tuning),
        "capo": arr.capo,
        "notes": [note_to_wire(n) for n in arr.notes],
        "chords": [chord_to_wire(c) for c in arr.chords],
        "anchors": [{"time": a.time, "fret": a.fret, "width": a.width} for a in arr.anchors],
        "handshapes": [
            {"chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time}
            for h in arr.hand_shapes
        ],
        "templates": [
            {"name": ct.name, "fingers": list(ct.fingers), "frets": list(ct.frets)}
            for ct in arr.chord_templates
        ],
    }
    # phrases is additive — only include the key when the source had
    # multi-level data. Treat an empty list the same as None ("slider
    # disabled"): emitting `"phrases": []` would otherwise signal
    # "slider enabled but with no ladder" to consumers. Sloppak readers
    # / old consumers that don't know about phrases just continue to
    # see the flat-merge arrangement.
    if arr.phrases:
        out["phrases"] = [phrase_to_wire(p) for p in arr.phrases]
    return out


def arrangement_from_wire(d: dict) -> Arrangement:
    """Parse a wire-format arrangement dict back into an Arrangement dataclass."""
    return Arrangement(
        name=d.get("name", ""),
        tuning=list(d.get("tuning", [0] * 6)),
        capo=int(d.get("capo", 0)),
        notes=[note_from_wire(n) for n in d.get("notes", [])],
        chords=[chord_from_wire(c) for c in d.get("chords", [])],
        anchors=[
            Anchor(time=float(a.get("time", 0)), fret=int(a.get("fret", 0)),
                   width=int(a.get("width", 4)))
            for a in d.get("anchors", [])
        ],
        hand_shapes=[
            HandShape(chord_id=int(h.get("chord_id", 0)),
                      start_time=float(h.get("start_time", 0)),
                      end_time=float(h.get("end_time", 0)))
            for h in d.get("handshapes", [])
        ],
        chord_templates=[
            ChordTemplate(name=ct.get("name", ""),
                          fingers=list(ct.get("fingers", [-1] * 6)),
                          frets=list(ct.get("frets", [-1] * 6)))
            for ct in d.get("templates", [])
        ],
        # `phrases` is optional — absent on single-level sources / older
        # sloppaks. Preserve None (rather than []) to preserve the
        # "slider disabled" signal downstream; an explicit empty list on
        # the wire is treated the same as absent.
        phrases=(
            [phrase_from_wire(p) for p in d["phrases"]]
            if d.get("phrases") else None
        ),
    )


def _float(elem, attr, default=0.0):
    v = elem.get(attr)
    return float(v) if v is not None else default


def _int(elem, attr, default=0):
    v = elem.get(attr)
    if v is None:
        return default
    try:
        return int(v)
    except ValueError:
        return int(float(v))


def _bool(elem, attr):
    v = elem.get(attr)
    return v is not None and v != "0"


def _parse_note(n) -> Note:
    return Note(
        time=_float(n, "time"),
        string=_int(n, "string"),
        fret=_int(n, "fret"),
        sustain=_float(n, "sustain"),
        slide_to=_int(n, "slideTo", -1),
        slide_unpitch_to=_int(n, "slideUnpitchTo", -1),
        bend=_float(n, "bend"),
        hammer_on=_bool(n, "hammerOn"),
        pull_off=_bool(n, "pullOff"),
        harmonic=_bool(n, "harmonic"),
        harmonic_pinch=_bool(n, "harmonicPinch"),
        palm_mute=_bool(n, "palmMute"),
        mute=_bool(n, "mute"),
        tremolo=_bool(n, "tremolo"),
        accent=_bool(n, "accent"),
        link_next=_bool(n, "linkNext"),
        tap=_bool(n, "tap"),
    )


def parse_arrangement(xml_path: str) -> Arrangement:
    """Parse a Rocksmith arrangement XML file."""
    tree = ET.parse(xml_path)
    root = tree.getroot()

    # Name
    arr_name = ""
    el = root.find("arrangement")
    if el is not None and el.text:
        arr_name = el.text

    # Tuning
    tuning = [0] * 6
    el = root.find("tuning")
    if el is not None:
        for i in range(6):
            tuning[i] = _int(el, f"string{i}")

    # Capo
    capo = 0
    el = root.find("capo")
    if el is not None and el.text:
        try:
            capo = int(el.text)
        except ValueError:
            pass

    # Chord templates
    chord_templates = []
    container = root.find("chordTemplates")
    if container is not None:
        for ct in container.findall("chordTemplate"):
            chord_templates.append(
                ChordTemplate(
                    name=ct.get("chordName", ""),
                    fingers=[_int(ct, f"finger{i}", -1) for i in range(6)],
                    frets=[_int(ct, f"fret{i}", -1) for i in range(6)],
                )
            )

    # Merge notes per-phrase: each phrase has its own maxDifficulty, and the full
    # chart is built by taking each phrase's notes from its max difficulty level.
    # For single-level XMLs (e.g. from GP converter), skip merging and use the one level directly.
    levels_el = root.find("levels")
    phrases_el = root.find("phrases")
    phrase_iters_el = root.find("phraseIterations")

    all_levels = {}
    if levels_el is not None:
        for level in levels_el.findall("level"):
            all_levels[_int(level, "difficulty")] = level

    notes = []
    chords = []
    anchors = []
    hand_shapes = []

    # Pre-parse each `<level>` once into time-sorted arrays plus a
    # parallel list of times for bisect. The phrase merge below slices
    # these per (phraseIteration × difficulty) pair; doing the XML walk
    # once per level turns that work from
    # O(phrases × levels × level_size) into
    # O(levels × level_size + phrases × levels × log(level_size)).
    # On long songs with deep ladders this is a big win.
    def _parse_level_fully(level):
        lv_notes = []
        container = level.find("notes")
        if container is not None:
            for n in container.findall("note"):
                lv_notes.append(_parse_note(n))
        lv_notes.sort(key=lambda n: n.time)

        lv_chords = []
        container = level.find("chords")
        if container is not None:
            for c in container.findall("chord"):
                t = _float(c, "time")
                chord_notes = [_parse_note(cn) for cn in c.findall("chordNote")]
                cid = _int(c, "chordId")
                if not chord_notes and cid < len(chord_templates):
                    ct = chord_templates[cid]
                    for s in range(6):
                        if ct.frets[s] >= 0:
                            chord_notes.append(Note(time=t, string=s, fret=ct.frets[s]))
                lv_chords.append(Chord(
                    time=t, chord_id=cid, notes=chord_notes,
                    high_density=_bool(c, "highDensity"),
                ))
        lv_chords.sort(key=lambda c: c.time)

        lv_anchors = []
        container = level.find("anchors")
        if container is not None:
            for a in container.findall("anchor"):
                lv_anchors.append(Anchor(
                    time=_float(a, "time"), fret=_int(a, "fret"),
                    width=_int(a, "width", 4),
                ))
        lv_anchors.sort(key=lambda a: a.time)

        lv_hand_shapes = []
        container = level.find("handShapes")
        if container is not None:
            for hs in container.findall("handShape"):
                lv_hand_shapes.append(HandShape(
                    chord_id=_int(hs, "chordId"),
                    start_time=_float(hs, "startTime"),
                    end_time=_float(hs, "endTime"),
                ))
        lv_hand_shapes.sort(key=lambda h: h.start_time)

        return {
            "notes": lv_notes,
            "note_times": [n.time for n in lv_notes],
            "chords": lv_chords,
            "chord_times": [c.time for c in lv_chords],
            "anchors": lv_anchors,
            "anchor_times": [a.time for a in lv_anchors],
            "hand_shapes": lv_hand_shapes,
            "hs_times": [h.start_time for h in lv_hand_shapes],
        }

    parsed_levels = {diff: _parse_level_fully(el) for diff, el in all_levels.items()}

    def _extract_level_slice(parsed, t_start, t_end):
        """Return (notes, chords, anchors, hand_shapes) for one pre-parsed level,
        clipped to [t_start, t_end). Uses bisect on the parallel time arrays —
        much cheaper than re-scanning XML when called per phrase-iteration."""
        def _slice(items, times):
            i0 = bisect.bisect_left(times, t_start)
            i1 = bisect.bisect_left(times, t_end)
            return items[i0:i1]
        return (
            _slice(parsed["notes"], parsed["note_times"]),
            _slice(parsed["chords"], parsed["chord_times"]),
            _slice(parsed["anchors"], parsed["anchor_times"]),
            _slice(parsed["hand_shapes"], parsed["hs_times"]),
        )

    def _collect_from_parsed(parsed, t_start, t_end):
        """Append a pre-parsed level's time-clipped slice to the flat
        arrangement lists. Used for the max-mastery merge that preserves
        the pre-slopsmith#48 behaviour for existing consumers."""
        lv_notes, lv_chords, lv_anchors, lv_hand_shapes = _extract_level_slice(
            parsed, t_start, t_end
        )
        notes.extend(lv_notes)
        chords.extend(lv_chords)
        anchors.extend(lv_anchors)
        hand_shapes.extend(lv_hand_shapes)

    def _collect_best_level_fallback():
        """Fallback merge when no usable phrase metadata is available: pick
        the level with the most notes+chords and flatten it."""
        best = max(
            parsed_levels.values(),
            key=lambda pl: len(pl["notes"]) + len(pl["chords"]),
        )
        _collect_from_parsed(best, 0.0, float("inf"))

    # Per-phrase difficulty data for the master-difficulty slider
    # (slopsmith#48). Only populated when the XML has multiple levels AND
    # phrase data — left as None for single-level sources so the frontend
    # knows to disable the slider.
    phrases: list[Phrase] | None = None

    # If there's only one level, use it directly (no per-phrase merge needed)
    if len(parsed_levels) == 1:
        _collect_from_parsed(next(iter(parsed_levels.values())), 0.0, float("inf"))
    # Merge per-phrase if we have phrase data and multiple levels
    elif phrases_el is not None and phrase_iters_el is not None and parsed_levels:
        phrase_list = phrases_el.findall("phrase")
        iterations = phrase_iters_el.findall("phraseIteration")

        # The last phrase iteration has no "next" to take its end time
        # from, so derive one from the last real event across all parsed
        # levels. Using a finite value here (instead of float('inf'))
        # matters because this ends up in Phrase.end_time on the wire,
        # and JSON has no Infinity literal — JS JSON.parse would reject
        # it. Include all event types (note start + sustain end, chord
        # start, anchor time, hand shape end_time) so the last phrase
        # window covers the whole authored content even when the final
        # event isn't a note/chord start. +1s pad ensures the final
        # event itself falls inside the bisect_left < t_end window.
        last_event = 0.0
        for pl in parsed_levels.values():
            for n in pl["notes"]:
                last_event = max(last_event, n.time + n.sustain)
            if pl["chord_times"]:
                last_event = max(last_event, pl["chord_times"][-1])
            if pl["anchor_times"]:
                last_event = max(last_event, pl["anchor_times"][-1])
            for h in pl["hand_shapes"]:
                last_event = max(last_event, h.end_time)
        # Also bound by the last phrase iteration's start time — some
        # charts place a trailing-silence phrase marker past every
        # authored event. Without this, the last phrase could end up
        # with end_time < start_time (invalid window, empty slice).
        for it in iterations:
            last_event = max(last_event, _float(it, "time"))
        song_end = last_event + 1.0

        phrases = []
        for i, it in enumerate(iterations):
            pid = _int(it, "phraseId")
            if pid >= len(phrase_list):
                continue
            max_diff = _int(phrase_list[pid], "maxDifficulty")
            t_start = _float(it, "time")
            t_end = _float(iterations[i + 1], "time") if i + 1 < len(iterations) else song_end

            # Build a PhraseLevel for every difficulty tier the author
            # wrote at or below this phrase's max — these are what the
            # master-difficulty slider selects between at render time.
            # Tiers above max_diff exist in some XMLs (authoring leftovers)
            # and are skipped to match Rocksmith's in-game behaviour.
            # Capture the extracted slices so the flat max-mastery merge
            # below can reuse one of them.
            phrase_levels: list[PhraseLevel] = []
            slices_by_diff: dict[int, tuple[list, list, list, list]] = {}
            for diff in sorted(parsed_levels.keys()):
                if diff > max_diff:
                    continue
                slc = _extract_level_slice(parsed_levels[diff], t_start, t_end)
                slices_by_diff[diff] = slc
                lv_notes, lv_chords, lv_anchors, lv_hand_shapes = slc
                phrase_levels.append(PhraseLevel(
                    difficulty=diff,
                    notes=lv_notes,
                    chords=lv_chords,
                    anchors=lv_anchors,
                    hand_shapes=lv_hand_shapes,
                ))

            # If every authored level was above this phrase's max_diff
            # (unusual but possible — e.g., the phrase block declares a
            # max_diff lower than any <level> that was actually written),
            # we have no ladder and no slice to flat-merge. Skip the
            # iteration entirely so the later `if not phrases:` fallback
            # can trigger a best-level merge for the whole arrangement.
            if not phrase_levels:
                continue

            phrases.append(Phrase(
                start_time=t_start,
                end_time=t_end,
                max_difficulty=max_diff,
                levels=phrase_levels,
            ))

            # Populate the flat max-mastery merge for existing consumers
            # (today's highway, sloppak converter's fallback). Reuse the
            # slice we just extracted for max_diff — or the closest tier
            # below it if max_diff itself wasn't authored.
            flat_diff = max_diff if max_diff in slices_by_diff else max(slices_by_diff)
            lv_notes, lv_chords, lv_anchors, lv_hand_shapes = slices_by_diff[flat_diff]
            notes.extend(lv_notes)
            chords.extend(lv_chords)
            anchors.extend(lv_anchors)
            hand_shapes.extend(lv_hand_shapes)

        # If the `<phraseIterations>` element was present but yielded
        # no usable iterations (empty element, or every iteration had
        # an out-of-range phraseId), revert to the "no phrase data"
        # sentinel and run the best-level fallback inline so we don't
        # ship an empty arrangement with the slider incorrectly enabled.
        if not phrases:
            phrases = None
            _collect_best_level_fallback()
    elif parsed_levels:
        _collect_best_level_fallback()

    notes.sort(key=lambda n: n.time)
    chords.sort(key=lambda c: c.time)
    anchors.sort(key=lambda a: a.time)
    hand_shapes.sort(key=lambda h: h.start_time)

    return Arrangement(
        name=arr_name,
        tuning=tuning,
        capo=capo,
        notes=notes,
        chords=chords,
        anchors=anchors,
        hand_shapes=hand_shapes,
        chord_templates=chord_templates,
        phrases=phrases,
    )


def _convert_sng_to_xml(extracted_dir: str):
    """If no arrangement XMLs exist but SNG files do, convert them via RsCli.
    Also converts vocals SNG → XML when no vocals XML is present, so lyrics
    are available for official DLC (which ships SNG-only)."""
    d = Path(extracted_dir)
    # Check if we already have arrangement XMLs (not just showlights/vocals)
    xml_files = list(d.rglob("*.xml"))
    has_arrangement_xml = False
    has_vocals_xml = False
    for xf in xml_files:
        try:
            root = ET.parse(xf).getroot()
            if root.tag == "vocals":
                has_vocals_xml = True
                continue
            if root.tag == "song":
                el = root.find("arrangement")
                if el is not None and el.text:
                    low = el.text.lower().strip()
                    if low not in ("vocals", "showlights", "jvocals"):
                        has_arrangement_xml = True
                    elif low == "vocals":
                        has_vocals_xml = True
                else:
                    has_arrangement_xml = True
        except Exception:
            continue

    if has_arrangement_xml and has_vocals_xml:
        return  # Already have everything

    # Find SNG files
    sng_files = list(d.rglob("*.sng"))
    if not sng_files:
        return

    rscli = os.environ.get("RSCLI_PATH", "")
    if not rscli or not Path(rscli).exists():
        # Try common locations (bundled, system, local)
        candidates = [
            Path(__file__).parent.parent / "tools" / "rscli" / "RsCli",
            Path(os.environ.get("PATH_BIN", "")) / "rscli" / "RsCli",
            Path("/opt/rscli/RsCli"),
            Path("./rscli/RsCli"),
        ]
        # Also check electron app's resources/bin/rscli
        if "RESOURCESPATH" in os.environ:
            candidates.insert(0, Path(os.environ["RESOURCESPATH"]) / "bin" / "rscli" / "RsCli")
        for p in candidates:
            if p.exists():
                rscli = str(p)
                break
    if not rscli:
        print("RsCli not found, cannot convert SNG to XML")
        return

    # Detect platform from directory structure
    platform = "pc"
    for sng in sng_files:
        parts = str(sng).lower()
        if "/macos/" in parts or "/mac/" in parts:
            platform = "mac"
            break

    arr_dir = d / "songs" / "arr"
    arr_dir.mkdir(parents=True, exist_ok=True)

    for sng_path in sng_files:
        stem = sng_path.stem
        # Vocals SNGs are not decoded via RsCli (unsupported) — they're parsed
        # directly in server.py via lib/sng_vocals.parse_vocals_sng().
        if "vocals" in stem.lower():
            continue
        if has_arrangement_xml:
            continue
        xml_out = arr_dir / f"{stem}.xml"
        try:
            result = subprocess.run(
                [rscli, "sng2xml", str(sng_path), str(xml_out), platform],
                capture_output=True, text=True, timeout=30,
            )
            if result.returncode != 0:
                print(f"sng2xml failed for {stem}: {result.stderr}")
        except Exception as e:
            print(f"sng2xml error for {stem}: {e}")


def load_song(extracted_dir: str) -> Song:
    """Load a song from an extracted PSARC directory."""
    # Convert SNG files to XML if needed (official DLC)
    _convert_sng_to_xml(extracted_dir)

    song = Song()
    xml_files = sorted(Path(extracted_dir).rglob("*.xml"))

    # Build manifest lookup: xml_stem (lowercase) -> ArrangementName
    _manifest_names = {}
    for jf in Path(extracted_dir).rglob("*.json"):
        try:
            data = json.loads(jf.read_text())
            entries = data.get("Entries") or {}
            for k, v in entries.items():
                attrs = v.get("Attributes") or {}
                arr_name = attrs.get("ArrangementName", "")
                if arr_name and arr_name not in ("Vocals", "ShowLights", "JVocals"):
                    # Match by JSON filename stem (same as XML stem)
                    _manifest_names[jf.stem.lower()] = arr_name
        except Exception:
            continue

    metadata_loaded = False
    for xml_path in xml_files:
        try:
            tree = ET.parse(xml_path)
            root = tree.getroot()
        except ET.ParseError:
            continue

        if root.tag != "song":
            continue

        # Skip vocals and showlights
        el = root.find("arrangement")
        if el is not None and el.text:
            low = el.text.lower().strip()
            if low in ("vocals", "showlights", "jvocals"):
                continue

        # Metadata from first valid arrangement
        if not metadata_loaded:
            for tag, attr in [
                ("title", "title"),
                ("artistName", "artist"),
                ("albumName", "album"),
            ]:
                el = root.find(tag)
                if el is not None and el.text:
                    setattr(song, attr, el.text)

            el = root.find("albumYear")
            if el is not None and el.text:
                try:
                    song.year = int(el.text)
                except ValueError:
                    pass

            el = root.find("songLength")
            if el is not None and el.text:
                song.song_length = float(el.text)

            el = root.find("offset")
            if el is not None and el.text:
                song.offset = float(el.text)

            # Beats
            container = root.find("ebeats")
            if container is not None:
                for eb in container.findall("ebeat"):
                    song.beats.append(
                        Beat(time=_float(eb, "time"), measure=_int(eb, "measure", -1))
                    )

            # Sections
            container = root.find("sections")
            if container is not None:
                for s in container.findall("section"):
                    song.sections.append(
                        Section(
                            name=s.get("name", ""),
                            number=_int(s, "number"),
                            start_time=_float(s, "startTime"),
                        )
                    )

            metadata_loaded = True

        # Parse arrangement
        arrangement = parse_arrangement(str(xml_path))

        # Try to get the correct name from the manifest JSON
        manifest_name = _manifest_names.get(xml_path.stem.lower())
        if manifest_name:
            arrangement.name = manifest_name
        else:
            # Fallback: map internal XML names to display names
            _name_map = {
                "part real_guitar": "Lead",
                "part real_guitar_22": "Rhythm",
                "part real_bass": "Bass",
                "part real_guitar_bonus": "Bonus Lead",
                "part real_bass_22": "Bass 2",
            }
            low = arrangement.name.lower().strip()
            if low in _name_map:
                arrangement.name = _name_map[low]
            elif not arrangement.name or low.startswith("part "):
                # Infer from filename
                fname = xml_path.stem.lower()
                if "lead" in fname:
                    arrangement.name = "Lead"
                elif "rhythm" in fname:
                    arrangement.name = "Rhythm"
                elif "bass" in fname:
                    arrangement.name = "Bass"
                elif "combo" in fname:
                    arrangement.name = "Combo"
                else:
                    arrangement.name = xml_path.stem

        song.arrangements.append(arrangement)

    # Sort: Lead > Combo > Rhythm > Bass > other
    priority = {"lead": 0, "combo": 1, "rhythm": 2, "bass": 3}
    song.arrangements.sort(key=lambda a: priority.get(a.name.lower(), 99))

    # Fallback: read metadata from manifest JSON files (official DLC)
    if not song.title or not song.artist:
        _load_manifest_metadata(song, extracted_dir)

    return song


def _load_manifest_metadata(song: Song, extracted_dir: str):
    """Read song metadata from manifest JSON files (used for official DLC)."""
    d = Path(extracted_dir)
    for jf in d.rglob("*.json"):
        try:
            data = json.loads(jf.read_text())
            # Manifest JSON has: Entries -> {key} -> Attributes
            entries = data.get("Entries") or data.get("entries") or {}
            if entries:
                for key, val in entries.items():
                    attrs = val.get("Attributes") or val.get("attributes") or {}
                    if not song.title and attrs.get("SongName"):
                        song.title = attrs["SongName"]
                    if not song.artist and attrs.get("ArtistName"):
                        song.artist = attrs["ArtistName"]
                    if not song.album and attrs.get("AlbumName"):
                        song.album = attrs["AlbumName"]
                    if not song.year and attrs.get("SongYear"):
                        try:
                            song.year = int(attrs["SongYear"])
                        except (ValueError, TypeError):
                            pass
                    if not song.song_length and attrs.get("SongLength"):
                        try:
                            song.song_length = float(attrs["SongLength"])
                        except (ValueError, TypeError):
                            pass
                    if song.title and song.artist:
                        return
            # Also check flat structure (individual arrangement manifests)
            attrs = data.get("Attributes") or data.get("attributes") or {}
            if attrs:
                if not song.title and attrs.get("SongName"):
                    song.title = attrs["SongName"]
                if not song.artist and attrs.get("ArtistName"):
                    song.artist = attrs["ArtistName"]
                if not song.album and attrs.get("AlbumName"):
                    song.album = attrs["AlbumName"]
                if not song.year and attrs.get("SongYear"):
                    try:
                        song.year = int(attrs["SongYear"])
                    except (ValueError, TypeError):
                        pass
                if not song.song_length and attrs.get("SongLength"):
                    try:
                        song.song_length = float(attrs["SongLength"])
                    except (ValueError, TypeError):
                        pass
                if song.title and song.artist:
                    return
        except Exception:
            continue
