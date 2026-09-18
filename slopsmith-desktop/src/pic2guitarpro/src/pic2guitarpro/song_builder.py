"""Build a PyGuitarPro ``Song`` from a :class:`ParsedTab` and write ``.gp5``.

PyGuitarPro writes GP3/GP4/GP5 files (``.gp5``). Guitar Pro 8 opens ``.gp5``
natively, so the output is editable in GP8 even though it is not the native
``.gp`` (XML) format — there is no open-source writer for that format.

Rhythm: ASCII tab does not encode reliable timing, so each measure's beats are
given a duration that divides the measure evenly when possible (e.g. 8 beats in
4/4 → eighths); otherwise each beat defaults to an eighth note. Empty measures
get a single whole-measure rest.
"""

from __future__ import annotations

from pathlib import Path

import guitarpro
from guitarpro import models as m

from .ascii_parser import ParsedTab

# Default 4/4 measure length in MIDI ticks (Duration.quarterTime * 4).
_QUARTER = m.Duration.quarterTime  # 960


def _safe_title(title: str) -> str:
    """GP5 stores strings as cp1252, which can't encode CJK. Drop unencodable
    characters (so "Beyond《海阔天空》" -> "Beyond") and fall back to
    "Untitled" when nothing encodes. The UTF-8 song name is still used for the
    output filename, so songs stay distinguishable on disk."""
    if not title:
        return "Untitled"
    try:
        title.encode("cp1252")
        return title
    except UnicodeEncodeError:
        ascii_part = title.encode("cp1252", errors="ignore").decode("cp1252").strip()
        return ascii_part or "Untitled"


def _beat_duration(measure_len: int, n_beats: int) -> m.Duration:
    """Pick a duration so ``n_beats`` of it sum to ``measure_len`` if possible."""
    if n_beats <= 0:
        return m.Duration(m.Duration.whole)
    per = measure_len // n_beats
    try:
        return m.Duration.fromTime(per)
    except ValueError:
        # Uneven split (e.g. 3 beats in 4/4); fall back to an eighth note.
        return m.Duration(m.Duration.eighth)


def build_song(tab: ParsedTab, title: str = "") -> m.Song:
    """Construct a PyGuitarPro ``Song`` from a parsed tab."""
    song = m.Song()
    song.title = _safe_title(title or "Untitled")
    song.subtitle = "Converted from tab images by pic2guitarpro"

    # Reuse the default first track (a 6-string guitar in standard tuning).
    track = song.tracks[0]
    track.name = _safe_title(title or "Guitar")

    # The default Song ships with one MeasureHeader; replace it with our own so
    # measure numbers and start positions are contiguous.
    song.measureHeaders.clear()
    track.measures.clear()

    measure_len = _QUARTER * 4  # 4/4 default
    start = _QUARTER
    for idx, pmeasure in enumerate(tab.measures):
        header = m.MeasureHeader()
        header.number = idx + 1
        header.start = start
        # addMeasureHeader wires up song reference + repeat-group bookkeeping.
        song.addMeasureHeader(header)
        start += header.length

        measure = m.Measure(track, header)
        voice = measure.voices[0]

        n_beats = len(pmeasure.beats)
        if n_beats == 0:
            # Empty measure: a single rest that fills the bar.
            beat = m.Beat(voice)
            beat.status = m.BeatStatus.rest
            beat.duration = m.Duration(m.Duration.whole)
            voice.beats.append(beat)
        else:
            duration = _beat_duration(measure_len, n_beats)
            for pbeat in pmeasure.beats:
                beat = m.Beat(voice)
                beat.status = m.BeatStatus.normal
                beat.duration = duration
                for pnote in pbeat.notes:
                    note = m.Note(beat)
                    note.string = pnote.string
                    note.value = pnote.fret  # Note.value is the FRET, not MIDI pitch.
                    note.type = m.NoteType.normal
                    beat.notes.append(note)
                voice.beats.append(beat)
        track.measures.append(measure)

    return song


def write_gp5(tab: ParsedTab, dest: Path, title: str = "") -> Path:
    """Build a Song from ``tab`` and write it to ``dest`` (``.gp5``)."""
    song = build_song(tab, title=title)
    dest = Path(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    guitarpro.write(song, str(dest))
    return dest
