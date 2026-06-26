"""Convert Guitar Pro files (.gp5/.gp4/.gp3) to Rocksmith 2014 arrangement XML."""

import xml.etree.ElementTree as ET
from xml.dom import minidom
from dataclasses import dataclass, field
from pathlib import Path

import guitarpro

# Standard tuning MIDI values (high e to low E, GP string order 1-6)
STANDARD_TUNING_6 = [64, 59, 55, 50, 45, 40]
STANDARD_TUNING_4 = [43, 38, 33, 28]  # Bass: G D A E

GP_TICKS_PER_QUARTER = 960


@dataclass
class TempoEvent:
    tick: int
    tempo: float  # BPM


@dataclass
class RsNote:
    time: float
    string: int
    fret: int
    sustain: float = 0.0
    bend: float = 0.0
    slide_to: int = -1
    slide_unpitch_to: int = -1
    hammer_on: bool = False
    pull_off: bool = False
    harmonic: bool = False
    harmonic_pinch: bool = False
    palm_mute: bool = False
    mute: bool = False
    accent: bool = False
    tremolo: bool = False
    tap: bool = False
    link_next: bool = False


@dataclass
class RsChord:
    time: float
    template_idx: int
    notes: list[RsNote] = field(default_factory=list)


@dataclass
class RsAnchor:
    time: float
    fret: int
    width: int = 4


@dataclass
class RsBeat:
    time: float
    measure: int  # -1 for non-downbeats


@dataclass
class RsSection:
    name: str
    time: float
    number: int = 1


@dataclass
class ChordTemplate:
    name: str
    frets: list[int]  # per string, -1 = unused
    fingers: list[int]  # per string, -1 = unused


def _build_tempo_map(song: guitarpro.Song) -> list[TempoEvent]:
    """Build a list of (tick, tempo) events from the song."""
    events = [TempoEvent(tick=0, tempo=float(song.tempo))]

    for track in song.tracks:
        for measure in track.measures:
            for voice in measure.voices:
                for beat in voice.beats:
                    if beat.effect and beat.effect.mixTableChange:
                        mtc = beat.effect.mixTableChange
                        if mtc.tempo and mtc.tempo.value > 0:
                            events.append(TempoEvent(
                                tick=beat.start, tempo=float(mtc.tempo.value)
                            ))

    events.sort(key=lambda e: e.tick)
    # Deduplicate by tick
    seen = set()
    unique = []
    for e in events:
        if e.tick not in seen:
            seen.add(e.tick)
            unique.append(e)
    return unique


def _tick_to_seconds(tick: int, tempo_map: list[TempoEvent]) -> float:
    """Convert a GP tick position to seconds using the tempo map."""
    seconds = 0.0
    prev_tick = 0
    prev_tempo = tempo_map[0].tempo

    for event in tempo_map:
        if event.tick >= tick:
            break
        # Accumulate time from prev_tick to event.tick at prev_tempo
        dt = (event.tick - prev_tick) / GP_TICKS_PER_QUARTER * (60.0 / prev_tempo)
        seconds += dt
        prev_tick = event.tick
        prev_tempo = event.tempo

    # Remaining ticks from last tempo event to target tick
    dt = (tick - prev_tick) / GP_TICKS_PER_QUARTER * (60.0 / prev_tempo)
    seconds += dt
    return seconds


def _duration_to_seconds(duration: guitarpro.Duration, tempo: float) -> float:
    """Convert a GP Duration to seconds at a given tempo."""
    # duration.value: 1=whole, 2=half, 4=quarter, 8=eighth, etc.
    beats = 4.0 / duration.value
    if duration.isDotted:
        beats *= 1.5
    if duration.tuplet.enters > 0 and duration.tuplet.times > 0:
        beats *= duration.tuplet.times / duration.tuplet.enters
    return beats * (60.0 / tempo)


def _tempo_at_tick(tick: int, tempo_map: list[TempoEvent]) -> float:
    """Get the tempo at a given tick."""
    result = tempo_map[0].tempo
    for event in tempo_map:
        if event.tick > tick:
            break
        result = event.tempo
    return result


def _gp_string_to_rs(gp_string: int, num_strings: int) -> int:
    """Convert GP string number (1=high) to RS string index (0=low)."""
    return num_strings - gp_string


def _compute_tuning(track: guitarpro.Track) -> list[int]:
    """Compute RS tuning offsets from GP string MIDI values."""
    num = len(track.strings)
    if num == 4:
        standard = STANDARD_TUNING_4
    else:
        standard = STANDARD_TUNING_6[:num]

    # GP strings are ordered high to low (string 1 = highest)
    # RS tuning is ordered low to high (index 0 = lowest)
    offsets = [0] * num
    for gp_str in track.strings:
        rs_idx = _gp_string_to_rs(gp_str.number, num)
        std_midi = standard[gp_str.number - 1]
        offsets[rs_idx] = gp_str.value - std_midi
    return offsets


def convert_track(
    song: guitarpro.Song,
    track_index: int,
    audio_offset: float = 0.0,
    arrangement_name: str = "",
    force_standard_tuning: bool = False,
) -> str:
    """Convert a GP track to Rocksmith 2014 arrangement XML string.

    Args:
        song: Parsed Guitar Pro song
        track_index: Which track to convert (0-based)
        audio_offset: Seconds to add to all times (for sync with audio)
        arrangement_name: "Lead", "Rhythm", "Bass", etc.
        force_standard_tuning: If True, set tuning to E standard (frets unchanged)

    Returns:
        XML string of the Rocksmith arrangement
    """
    track = song.tracks[track_index]
    num_strings = len(track.strings)
    is_bass = num_strings == 4
    tempo_map = _build_tempo_map(song)
    if force_standard_tuning:
        tuning = [0] * num_strings
    else:
        tuning = _compute_tuning(track)

    if not arrangement_name:
        name = track.name.strip()
        low = name.lower()
        if is_bass or "bass" in low:
            arrangement_name = "Bass"
        elif "rhythm" in low or "rhy" in low:
            arrangement_name = "Rhythm"
        else:
            arrangement_name = "Lead"

    # ── Collect beats (ebeats) ────────────────────────────────────────────
    beats = []
    for mh in song.measureHeaders:
        t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
        beats.append(RsBeat(time=t, measure=mh.number))
        # Subdivisions within the measure
        tempo = _tempo_at_tick(mh.start, tempo_map)
        ticks_per_beat = GP_TICKS_PER_QUARTER
        num_beats_in_measure = mh.timeSignature.numerator
        for b in range(1, num_beats_in_measure):
            sub_tick = mh.start + b * ticks_per_beat
            sub_t = _tick_to_seconds(sub_tick, tempo_map) + audio_offset
            beats.append(RsBeat(time=sub_t, measure=-1))
    beats.sort(key=lambda b: b.time)

    # ── Collect sections from markers ─────────────────────────────────────
    sections = []
    section_counts = {}
    for mh in song.measureHeaders:
        if mh.marker and mh.marker.title:
            name = mh.marker.title.strip().lower().replace(" ", "")
            section_counts[name] = section_counts.get(name, 0) + 1
            t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
            sections.append(RsSection(name=name, time=t, number=section_counts[name]))

    if not sections:
        # Default: one section for the whole song
        sections.append(RsSection(name="default", time=audio_offset, number=1))

    # ── Collect notes and chords ──────────────────────────────────────────
    rs_notes = []
    rs_chords = []
    chord_templates: list[ChordTemplate] = []
    chord_template_map: dict[tuple, int] = {}  # fret tuple → index

    for measure in track.measures:
        for voice in measure.voices:
            for beat in voice.beats:
                if not beat.notes:
                    continue

                t = _tick_to_seconds(beat.start, tempo_map) + audio_offset
                tempo = _tempo_at_tick(beat.start, tempo_map)
                dur = _duration_to_seconds(beat.duration, tempo)

                beat_notes = []
                for note in beat.notes:
                    if note.type == guitarpro.NoteType.rest:
                        continue

                    rs_str = _gp_string_to_rs(note.string, num_strings)
                    fret = note.value
                    if note.type == guitarpro.NoteType.dead:
                        fret = max(fret, 0)

                    rn = RsNote(
                        time=t,
                        string=rs_str,
                        fret=fret,
                        sustain=dur if dur > 0.2 else 0.0,
                        mute=note.type == guitarpro.NoteType.dead,
                    )

                    # Techniques
                    eff = note.effect
                    if eff.bend and eff.bend.points:
                        max_bend = max(p.value for p in eff.bend.points)
                        rn.bend = max_bend / 100.0  # GP uses 100 = 1 semitone

                    if eff.hammer:
                        # Determine H vs P from fret context
                        rn.hammer_on = True  # simplified; ideally check prev note

                    if eff.slides:
                        for slide in eff.slides:
                            if slide in (
                                guitarpro.SlideType.shiftSlideTo,
                                guitarpro.SlideType.legatoSlideTo,
                            ):
                                rn.link_next = True
                                # slide target fret determined from next note

                    if eff.harmonic:
                        if isinstance(eff.harmonic, guitarpro.PinchHarmonic):
                            rn.harmonic_pinch = True
                        else:
                            rn.harmonic = True

                    if eff.palmMute:
                        rn.palm_mute = True
                    if eff.accentuatedNote or eff.heavyAccentuatedNote:
                        rn.accent = True
                    if eff.ghostNote:
                        rn.mute = True
                    if eff.tremoloPicking:
                        rn.tremolo = True

                    beat_notes.append(rn)

                if not beat_notes:
                    continue

                if len(beat_notes) == 1:
                    rs_notes.append(beat_notes[0])
                else:
                    # Chord: create/reuse a chord template
                    frets = [-1] * max(6, num_strings)
                    for n in beat_notes:
                        if 0 <= n.string < len(frets):
                            frets[n.string] = n.fret
                    fret_key = tuple(frets)

                    if fret_key not in chord_template_map:
                        # Try to get chord name from GP
                        chord_name = ""
                        if beat.effect and beat.effect.chord:
                            chord_name = beat.effect.chord.name or ""
                        idx = len(chord_templates)
                        chord_templates.append(ChordTemplate(
                            name=chord_name,
                            frets=list(frets),
                            fingers=[-1] * len(frets),
                        ))
                        chord_template_map[fret_key] = idx

                    rs_chords.append(RsChord(
                        time=t,
                        template_idx=chord_template_map[fret_key],
                        notes=beat_notes,
                    ))

    rs_notes.sort(key=lambda n: n.time)
    rs_chords.sort(key=lambda c: c.time)

    # ── Compute anchors ───────────────────────────────────────────────────
    # Exclude open strings (fret 0) — they span the full highway and
    # shouldn't cause the fret range to shift
    anchors = []
    all_timed_frets = [(n.time, n.fret) for n in rs_notes if n.fret > 0]
    for c in rs_chords:
        for cn in c.notes:
            if cn.fret > 0:
                all_timed_frets.append((cn.time, cn.fret))
    all_timed_frets.sort()

    # Always start with an anchor at the beginning
    first_fret = all_timed_frets[0][1] if all_timed_frets else 1
    anchors.append(RsAnchor(time=audio_offset, fret=max(1, first_fret - 1), width=4))

    for t, fret in all_timed_frets:
        anchor_lo = anchors[-1].fret
        anchor_hi = anchor_lo + anchors[-1].width
        if fret < anchor_lo or fret > anchor_hi:
            new_fret = max(1, fret - 1)
            if new_fret != anchors[-1].fret:
                anchors.append(RsAnchor(time=t, fret=new_fret, width=4))

    # ── Compute song length ───────────────────────────────────────────────
    last_mh = song.measureHeaders[-1]
    song_length = _tick_to_seconds(
        last_mh.start + last_mh.timeSignature.numerator * GP_TICKS_PER_QUARTER,
        tempo_map,
    ) + audio_offset

    # ── Build XML ─────────────────────────────────────────────────────────
    return _build_xml(
        title=song.title or "Untitled",
        artist=song.artist or "Unknown",
        album=song.album or "",
        year=str(song.copyright) if song.copyright else "",
        arrangement=arrangement_name,
        tuning=tuning,
        num_strings=num_strings,
        song_length=song_length,
        audio_offset=audio_offset,
        beats=beats,
        sections=sections,
        notes=rs_notes,
        chords=rs_chords,
        chord_templates=chord_templates,
        anchors=anchors,
        tempo=song.tempo,
    )


def _build_xml(
    title, artist, album, year, arrangement, tuning, num_strings,
    song_length, audio_offset, beats, sections, notes, chords,
    chord_templates, anchors, tempo,
) -> str:
    root = ET.Element("song", version="7")

    ET.SubElement(root, "title").text = title
    ET.SubElement(root, "arrangement").text = arrangement
    ET.SubElement(root, "offset").text = f"{audio_offset:.3f}"
    ET.SubElement(root, "songLength").text = f"{song_length:.3f}"
    ET.SubElement(root, "startBeat").text = f"{beats[0].time:.3f}" if beats else "0.000"
    ET.SubElement(root, "averageTempo").text = str(tempo)
    ET.SubElement(root, "artistName").text = artist
    ET.SubElement(root, "albumName").text = album
    ET.SubElement(root, "albumYear").text = year

    # Tuning
    tuning_el = ET.SubElement(root, "tuning")
    for i in range(6):
        tuning_el.set(f"string{i}", str(tuning[i] if i < len(tuning) else 0))
    ET.SubElement(root, "capo").text = "0"

    # Ebeats
    ebeats = ET.SubElement(root, "ebeats", count=str(len(beats)))
    for b in beats:
        ET.SubElement(ebeats, "ebeat", time=f"{b.time:.3f}", measure=str(b.measure))

    # Sections
    sections_el = ET.SubElement(root, "sections", count=str(len(sections)))
    for s in sections:
        ET.SubElement(sections_el, "section",
                      name=s.name, number=str(s.number),
                      startTime=f"{s.time:.3f}")

    # Phrases — one per section
    phrases_el = ET.SubElement(root, "phrases", count=str(len(sections)))
    for i, s in enumerate(sections):
        ET.SubElement(phrases_el, "phrase",
                      disparity="0", ignore="0", maxDifficulty="0",
                      name=s.name, solo="0")

    phrase_iters = ET.SubElement(root, "phraseIterations", count=str(len(sections)))
    for i, s in enumerate(sections):
        ET.SubElement(phrase_iters, "phraseIteration",
                      time=f"{s.time:.3f}", phraseId=str(i))

    # Chord templates
    ct_el = ET.SubElement(root, "chordTemplates", count=str(len(chord_templates)))
    for ct in chord_templates:
        attrs = {"chordName": ct.name}
        for i in range(6):
            attrs[f"fret{i}"] = str(ct.frets[i] if i < len(ct.frets) else -1)
            attrs[f"finger{i}"] = str(ct.fingers[i] if i < len(ct.fingers) else -1)
        ET.SubElement(ct_el, "chordTemplate", **attrs)

    # Single difficulty level with all notes
    levels_el = ET.SubElement(root, "levels", count="1")
    level = ET.SubElement(levels_el, "level", difficulty="0")

    # Notes
    notes_el = ET.SubElement(level, "notes", count=str(len(notes)))
    for n in notes:
        attrs = {
            "time": f"{n.time:.3f}",
            "string": str(n.string),
            "fret": str(n.fret),
            "sustain": f"{n.sustain:.3f}",
            "bend": f"{n.bend:.1f}" if n.bend else "0",
            "hammerOn": "1" if n.hammer_on else "0",
            "pullOff": "1" if n.pull_off else "0",
            "slideTo": str(n.slide_to),
            "slideUnpitchTo": str(n.slide_unpitch_to),
            "harmonic": "1" if n.harmonic else "0",
            "harmonicPinch": "1" if n.harmonic_pinch else "0",
            "palmMute": "1" if n.palm_mute else "0",
            "mute": "1" if n.mute else "0",
            "tremolo": "1" if n.tremolo else "0",
            "accent": "1" if n.accent else "0",
            "linkNext": "1" if n.link_next else "0",
            "tap": "1" if n.tap else "0",
            "ignore": "0",
        }
        ET.SubElement(notes_el, "note", **attrs)

    # Chords
    chords_el = ET.SubElement(level, "chords", count=str(len(chords)))
    for ch in chords:
        chord_el = ET.SubElement(chords_el, "chord",
                                 time=f"{ch.time:.3f}",
                                 chordId=str(ch.template_idx),
                                 highDensity="0", strum="down")
        for cn in ch.notes:
            ET.SubElement(chord_el, "chordNote",
                          time=f"{cn.time:.3f}",
                          string=str(cn.string),
                          fret=str(cn.fret),
                          sustain=f"{cn.sustain:.3f}",
                          bend="0",
                          hammerOn="0", pullOff="0",
                          slideTo="-1", slideUnpitchTo="-1",
                          harmonic="0", harmonicPinch="0",
                          palmMute="1" if cn.palm_mute else "0",
                          mute="1" if cn.mute else "0",
                          tremolo="0", accent="0",
                          linkNext="0", tap="0", ignore="0")

    # Anchors
    anchors_el = ET.SubElement(level, "anchors", count=str(len(anchors)))
    for a in anchors:
        ET.SubElement(anchors_el, "anchor",
                      time=f"{a.time:.3f}",
                      fret=str(a.fret),
                      width=str(a.width))

    # Hand shapes (empty for now)
    ET.SubElement(level, "handShapes", count="0")

    # Pretty print
    xml_str = ET.tostring(root, encoding="unicode")
    dom = minidom.parseString(xml_str)
    return dom.toprettyxml(indent="  ", encoding=None)


PIANO_INSTRUMENTS = set(range(0, 8))  # MIDI instruments 0-7 = piano family
KEYS_INSTRUMENTS = PIANO_INSTRUMENTS | set(range(16, 24)) | {80, 81, 82, 83}  # + organs + synth leads
KEYS_NAME_KEYWORDS = {"piano", "keys", "keyboard", "synth", "organ", "rhodes", "wurlitzer", "clav", "epiano"}

# GM drum mapping: MIDI note -> drum piece name
GM_DRUM_MAP = {
    35: "Kick", 36: "Kick",
    38: "Snare", 40: "Snare",
    42: "HiHat", 44: "HiHat", 46: "HiHat",
    48: "Tom1", 50: "Tom1",
    45: "Tom2", 47: "Tom2",
    41: "Tom3", 43: "Tom3",
    49: "Crash", 57: "Crash",
    51: "Ride", 59: "Ride",
}
DRUMS_NAME_KEYWORDS = {"drums", "drum", "percussion", "drum kit", "drumkit"}


def is_piano_track(track: guitarpro.Track) -> bool:
    """Detect if a GP track is a piano/keyboard instrument."""
    if track.isPercussionTrack:
        return False
    # Check MIDI instrument
    if hasattr(track, 'channel') and track.channel:
        inst = getattr(track.channel, 'instrument', -1)
        if inst in KEYS_INSTRUMENTS:
            return True
    # Check name
    name_low = track.name.lower()
    if any(kw in name_low for kw in KEYS_NAME_KEYWORDS):
        return True
    return False


def is_drum_track(track: guitarpro.Track) -> bool:
    """Detect if a GP track is a percussion/drum track."""
    if track.isPercussionTrack:
        return True
    # Check MIDI channel 10 (index 9)
    if hasattr(track, 'channel') and track.channel:
        ch = getattr(track.channel, 'channel', -1)
        if ch == 9:  # MIDI channel 10 (0-indexed)
            return True
    # Check name
    name_low = track.name.lower()
    if any(kw in name_low for kw in DRUMS_NAME_KEYWORDS):
        return True
    return False


def list_tracks(gp_path: str) -> list[dict]:
    """List all tracks in a Guitar Pro file with basic info."""
    song = guitarpro.parse(gp_path)
    tracks = []
    for i, track in enumerate(song.tracks):
        note_count = 0
        for measure in track.measures:
            for voice in measure.voices:
                for beat in voice.beats:
                    note_count += len(beat.notes)
        instrument = -1
        if hasattr(track, 'channel') and track.channel:
            instrument = getattr(track.channel, 'instrument', -1)
        tracks.append({
            "index": i,
            "name": track.name,
            "strings": len(track.strings),
            "is_percussion": track.isPercussionTrack,
            "is_piano": is_piano_track(track),
            "is_drums": is_drum_track(track),
            "instrument": instrument,
            "notes": note_count,
        })
    return tracks


def auto_select_tracks(gp_path: str) -> tuple[list[int], dict[int, str]]:
    """Auto-select guitar/bass/keys tracks and assign Rocksmith arrangement names.

    Includes piano/keyboard tracks as "Keys" arrangements alongside
    guitar and bass tracks.

    Returns:
        (track_indices, name_map) — indices to include and their arrangement names
    """
    tracks = list_tracks(gp_path)
    guitar_keywords = {"guitar", "gtr", "lead", "rhythm", "rhy", "solo", "clean", "distort", "acoustic", "elec"}
    bass_keywords = {"bass"}
    skip_keywords = {"string", "choir", "brass", "brite", "flute", "violin", "cello", "horn"}

    selected = []
    for t in tracks:
        if t["notes"] == 0:
            continue

        # Drum/percussion tracks → Drums
        if t["is_drums"]:
            selected.append((t["index"], "drums"))
            continue

        # Piano/keyboard tracks → Keys
        if t["is_piano"]:
            selected.append((t["index"], "keys"))
            continue

        name_low = t["name"].lower()

        # 4-string = bass
        if t["strings"] == 4:
            selected.append((t["index"], "bass"))
            continue

        # Check name for skip keywords
        if any(kw in name_low for kw in skip_keywords):
            continue

        # Check name for guitar/bass keywords
        if any(kw in name_low for kw in bass_keywords):
            selected.append((t["index"], "bass"))
        elif any(kw in name_low for kw in guitar_keywords):
            selected.append((t["index"], "guitar"))
        elif t["strings"] == 6:
            # Generic 6-string, assume guitar
            selected.append((t["index"], "guitar"))

    if not selected:
        # Fallback: take all non-percussion non-empty tracks
        for t in tracks:
            if not t["is_percussion"] and t["notes"] > 0:
                role = "bass" if t["strings"] == 4 else "guitar"
                selected.append((t["index"], role))

    # Assign Rocksmith names: Lead, Rhythm, Combo, Bass, Keys, Drums
    track_indices = []
    name_map = {}
    lead_count = 0
    rhythm_count = 0
    bass_count = 0
    keys_count = 0
    drums_count = 0

    for idx, role in selected:
        track_indices.append(idx)
        if role == "drums":
            drums_count += 1
            name_map[idx] = "Drums" if drums_count == 1 else f"Drums {drums_count}"
        elif role == "keys":
            keys_count += 1
            name_map[idx] = "Keys" if keys_count == 1 else f"Keys {keys_count}"
        elif role == "bass":
            bass_count += 1
            name_map[idx] = "Bass" if bass_count == 1 else f"Bass {bass_count}"
        elif lead_count == 0:
            lead_count += 1
            name_map[idx] = "Lead"
        else:
            rhythm_count += 1
            name_map[idx] = "Rhythm" if rhythm_count == 1 else f"Combo"

    return track_indices, name_map


def convert_piano_track(
    song: guitarpro.Song,
    track_index: int,
    audio_offset: float = 0.0,
    arrangement_name: str = "Keys",
) -> str:
    """Convert a GP piano/keyboard track to Rocksmith XML using MIDI encoding.

    Encodes MIDI notes into Rocksmith's string+fret format:
        string = midi_note // 24
        fret   = midi_note % 24

    This gives a range of 0-143, covering the full piano range within
    Rocksmith's 6-string x 24-fret structure. The piano highway plugin
    decodes back via: midi = string * 24 + fret.
    """
    track = song.tracks[track_index]
    tempo_map = _build_tempo_map(song)

    # ── Collect beats ────────────────────────────────────────────────
    beats = []
    for mh in song.measureHeaders:
        t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
        beats.append(RsBeat(time=t, measure=mh.number))
        tempo = _tempo_at_tick(mh.start, tempo_map)
        num_beats_in_measure = mh.timeSignature.numerator
        for b in range(1, num_beats_in_measure):
            sub_tick = mh.start + b * GP_TICKS_PER_QUARTER
            sub_t = _tick_to_seconds(sub_tick, tempo_map) + audio_offset
            beats.append(RsBeat(time=sub_t, measure=-1))
    beats.sort(key=lambda b: b.time)

    # ── Collect sections from markers ────────────────────────────────
    sections = []
    section_counts = {}
    for mh in song.measureHeaders:
        if mh.marker and mh.marker.title:
            name = mh.marker.title.strip().lower().replace(" ", "")
            section_counts[name] = section_counts.get(name, 0) + 1
            t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
            sections.append(RsSection(name=name, time=t, number=section_counts[name]))
    if not sections:
        sections.append(RsSection(name="default", time=audio_offset, number=1))

    # ── Collect notes ────────────────────────────────────────────────
    rs_notes = []
    rs_chords = []
    chord_templates: list[ChordTemplate] = []
    chord_template_map: dict[tuple, int] = {}

    for measure in track.measures:
        for voice in measure.voices:
            for beat in voice.beats:
                if not beat.notes:
                    continue

                t = _tick_to_seconds(beat.start, tempo_map) + audio_offset
                tempo = _tempo_at_tick(beat.start, tempo_map)
                dur = _duration_to_seconds(beat.duration, tempo)

                beat_notes = []
                for note in beat.notes:
                    if note.type == guitarpro.NoteType.rest:
                        continue

                    # Get MIDI note value from the GP note
                    # In GP, note.value is the fret, and the string tuning
                    # gives the base MIDI value
                    gp_str_idx = note.string  # 1-based in GP
                    if gp_str_idx <= len(track.strings):
                        base_midi = track.strings[gp_str_idx - 1].value
                    else:
                        base_midi = 60  # fallback to middle C
                    midi_note = base_midi + note.value

                    # Encode into Rocksmith string+fret
                    rs_string = midi_note // 24
                    rs_fret = midi_note % 24

                    rn = RsNote(
                        time=t,
                        string=rs_string,
                        fret=rs_fret,
                        sustain=dur if dur > 0.15 else 0.0,
                        mute=note.type == guitarpro.NoteType.dead,
                    )

                    # Accent from velocity
                    eff = note.effect
                    if eff.accentuatedNote or eff.heavyAccentuatedNote:
                        rn.accent = True

                    beat_notes.append(rn)

                if not beat_notes:
                    continue

                if len(beat_notes) == 1:
                    rs_notes.append(beat_notes[0])
                else:
                    # Piano chord: create template from MIDI-encoded positions
                    frets = [-1] * 6
                    for n in beat_notes:
                        if 0 <= n.string < 6:
                            frets[n.string] = n.fret
                    fret_key = tuple(frets)

                    if fret_key not in chord_template_map:
                        chord_name = ""
                        if beat.effect and beat.effect.chord:
                            chord_name = beat.effect.chord.name or ""
                        idx = len(chord_templates)
                        chord_templates.append(ChordTemplate(
                            name=chord_name,
                            frets=list(frets),
                            fingers=[-1] * 6,
                        ))
                        chord_template_map[fret_key] = idx

                    rs_chords.append(RsChord(
                        time=t,
                        template_idx=chord_template_map[fret_key],
                        notes=beat_notes,
                    ))

    rs_notes.sort(key=lambda n: n.time)
    rs_chords.sort(key=lambda c: c.time)

    # ── Anchors (simplified for piano — just cover the range) ────────
    anchors = [RsAnchor(time=audio_offset, fret=1, width=24)]

    # ── Song length ──────────────────────────────────────────────────
    last_mh = song.measureHeaders[-1]
    song_length = _tick_to_seconds(
        last_mh.start + last_mh.timeSignature.numerator * GP_TICKS_PER_QUARTER,
        tempo_map,
    ) + audio_offset

    # ── Build XML ────────────────────────────────────────────────────
    # Use all-zero tuning (piano has no tuning concept)
    return _build_xml(
        title=song.title or "Untitled",
        artist=song.artist or "Unknown",
        album=song.album or "",
        year=str(song.copyright) if song.copyright else "",
        arrangement=arrangement_name,
        tuning=[0] * 6,
        num_strings=6,
        song_length=song_length,
        audio_offset=audio_offset,
        beats=beats,
        sections=sections,
        notes=rs_notes,
        chords=rs_chords,
        chord_templates=chord_templates,
        anchors=anchors,
        tempo=song.tempo,
    )


def convert_drum_track(
    song: guitarpro.Song,
    track_index: int,
    audio_offset: float = 0.0,
    arrangement_name: str = "Drums",
) -> str:
    """Convert a GP drum/percussion track to Rocksmith XML using MIDI encoding.

    Encodes MIDI drum note numbers into Rocksmith's string+fret format:
        string = midi_note // 24
        fret   = midi_note % 24

    The drum highway plugin decodes back via: midi = string * 24 + fret
    and maps to the appropriate drum lane (kick, snare, hi-hat, etc.).
    """
    track = song.tracks[track_index]
    tempo_map = _build_tempo_map(song)

    # ── Collect beats ────────────────────────────────────────────────
    beats = []
    for mh in song.measureHeaders:
        t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
        beats.append(RsBeat(time=t, measure=mh.number))
        num_beats_in_measure = mh.timeSignature.numerator
        for b in range(1, num_beats_in_measure):
            sub_tick = mh.start + b * GP_TICKS_PER_QUARTER
            sub_t = _tick_to_seconds(sub_tick, tempo_map) + audio_offset
            beats.append(RsBeat(time=sub_t, measure=-1))
    beats.sort(key=lambda b: b.time)

    # ── Collect sections from markers ────────────────────────────────
    sections = []
    section_counts = {}
    for mh in song.measureHeaders:
        if mh.marker and mh.marker.title:
            name = mh.marker.title.strip().lower().replace(" ", "")
            section_counts[name] = section_counts.get(name, 0) + 1
            t = _tick_to_seconds(mh.start, tempo_map) + audio_offset
            sections.append(RsSection(name=name, time=t, number=section_counts[name]))
    if not sections:
        sections.append(RsSection(name="default", time=audio_offset, number=1))

    # ── Collect drum notes ───────────────────────────────────────────
    rs_notes = []
    rs_chords = []
    chord_templates: list[ChordTemplate] = []
    chord_template_map: dict[tuple, int] = {}

    for measure in track.measures:
        for voice in measure.voices:
            for beat in voice.beats:
                if not beat.notes:
                    continue

                t = _tick_to_seconds(beat.start, tempo_map) + audio_offset

                beat_notes = []
                for note in beat.notes:
                    if note.type == guitarpro.NoteType.rest:
                        continue

                    # For percussion tracks, the MIDI note comes from the
                    # string tuning value (each "string" = a drum piece).
                    # note.value is the fret (usually 0 for drums).
                    gp_str_idx = note.string  # 1-based
                    if gp_str_idx <= len(track.strings):
                        midi_note = track.strings[gp_str_idx - 1].value + note.value
                    else:
                        midi_note = note.value
                    if midi_note not in GM_DRUM_MAP:
                        continue  # Skip unknown percussion sounds

                    # Encode into Rocksmith string+fret
                    rs_string = midi_note // 24
                    rs_fret = midi_note % 24

                    rn = RsNote(
                        time=t,
                        string=rs_string,
                        fret=rs_fret,
                        sustain=0.0,  # Drums have no sustain
                    )

                    # Accent from velocity/effect
                    eff = note.effect
                    if eff.accentuatedNote or eff.heavyAccentuatedNote:
                        rn.accent = True
                    # Ghost notes: mark as mute (low velocity)
                    if eff.ghostNote:
                        rn.mute = True

                    beat_notes.append(rn)

                if not beat_notes:
                    continue

                if len(beat_notes) == 1:
                    rs_notes.append(beat_notes[0])
                else:
                    # Multiple drum hits at same time → chord
                    frets = [-1] * 6
                    for n in beat_notes:
                        if 0 <= n.string < 6:
                            frets[n.string] = n.fret
                    fret_key = tuple(frets)

                    if fret_key not in chord_template_map:
                        idx = len(chord_templates)
                        chord_templates.append(ChordTemplate(
                            name="",
                            frets=list(frets),
                            fingers=[-1] * 6,
                        ))
                        chord_template_map[fret_key] = idx

                    rs_chords.append(RsChord(
                        time=t,
                        template_idx=chord_template_map[fret_key],
                        notes=beat_notes,
                    ))

    rs_notes.sort(key=lambda n: n.time)
    rs_chords.sort(key=lambda c: c.time)

    # ── Anchors (simplified for drums) ───────────────────────────────
    anchors = [RsAnchor(time=audio_offset, fret=1, width=24)]

    # ── Song length ──────────────────────────────────────────────────
    last_mh = song.measureHeaders[-1]
    song_length = _tick_to_seconds(
        last_mh.start + last_mh.timeSignature.numerator * GP_TICKS_PER_QUARTER,
        tempo_map,
    ) + audio_offset

    # ── Build XML ────────────────────────────────────────────────────
    return _build_xml(
        title=song.title or "Untitled",
        artist=song.artist or "Unknown",
        album=song.album or "",
        year=str(song.copyright) if song.copyright else "",
        arrangement=arrangement_name,
        tuning=[0] * 6,
        num_strings=6,
        song_length=song_length,
        audio_offset=audio_offset,
        beats=beats,
        sections=sections,
        notes=rs_notes,
        chords=rs_chords,
        chord_templates=chord_templates,
        anchors=anchors,
        tempo=song.tempo,
    )


def convert_file(
    gp_path: str,
    output_dir: str,
    track_indices: list[int] | None = None,
    audio_offset: float = 0.0,
    arrangement_names: dict[int, str] | None = None,
    force_standard_tuning: bool = False,
) -> list[str]:
    """Convert a GP file to Rocksmith XMLs.

    Args:
        gp_path: Path to .gp5/.gp4/.gp3 file
        output_dir: Directory to write XML files
        track_indices: Which tracks to convert (None = auto-select)
        audio_offset: Seconds to add for audio sync
        arrangement_names: Override arrangement names {track_idx: name}
        force_standard_tuning: Force E standard tuning (frets unchanged)

    Returns:
        List of output XML file paths
    """
    song = guitarpro.parse(gp_path)
    out = Path(output_dir)
    out.mkdir(parents=True, exist_ok=True)

    if track_indices is None:
        # Auto-select: include all tracks that auto_select_tracks would pick
        track_indices, auto_names = auto_select_tracks(gp_path)
        if not arrangement_names:
            arrangement_names = auto_names

    names = arrangement_names or {}
    output_files = []

    for idx in track_indices:
        track = song.tracks[idx]
        arr_name = names.get(idx, "")

        # Route drum/percussion tracks through drum converter
        if is_drum_track(track) or (arr_name and arr_name.lower().startswith("drums")):
            xml_str = convert_drum_track(
                song, idx, audio_offset, arr_name or "Drums"
            )
        # Route piano/keyboard tracks through the MIDI-encoding converter
        elif is_piano_track(track) or (arr_name and arr_name.lower().startswith("keys")):
            xml_str = convert_piano_track(
                song, idx, audio_offset, arr_name or "Keys"
            )
        else:
            xml_str = convert_track(
                song, idx, audio_offset, arr_name, force_standard_tuning
            )

        safe_name = track.name.strip().replace(" ", "_").replace("/", "_")
        filename = f"{safe_name}_{arr_name or 'arr'}.xml"
        filepath = out / filename
        filepath.write_text(xml_str)
        output_files.append(str(filepath))

    return output_files
