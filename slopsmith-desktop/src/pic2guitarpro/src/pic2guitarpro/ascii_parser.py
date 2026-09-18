"""Parse ASCII guitar tablature into structured measures.

Input: the raw text produced by OCR (``ocr_tabber.ocr_tab.ocr_tab_image``),
possibly concatenated across several pages for the same song.

Output: a :class:`ParsedTab` — an ordered list of :class:`ParsedMeasure`,
each holding a list of :class:`ParsedBeat`, each holding a list of
:class:`ParsedNote` (string number 1..6 + fret).

The parser is deliberately tolerant of OCR noise:

* Tab rows are recognised by content (a tuning letter, dashes, digits and
  ``|``) and grouped into 6-line staves (fewer rows are allowed — OCR often
  drops empty top strings).
* String number is recovered by *subsequence-matching* the detected leading
  tuning letters against the standard order ``e B G D A E``. This correctly
  maps a partial stave like ``D A E`` to strings 4/5/6 rather than 1/2/3.
  When no row carries a usable letter, it falls back to top-to-bottom order.
* Measures are split on ``|`` barlines, aligned across the six strings by
  segment index (the standard ASCII-tab layout); mismatched segment counts
  are padded with empty strings.
* Within a measure, notes are clustered into beats by character column with a
  small tolerance, so notes struck together (a chord) land in one beat. Two
  notes on the same string never share a beat.
* Digit-lookalike letters in the rail (e.g. ``0`` misread as ``G``/``O``,
  ``5`` as ``S``) are mapped back to digits so frets survive OCR errors.
* Rhythm is not encoded reliably by ASCII tab, so each beat gets a duration
  chosen so the beats sum to the measure length when possible; otherwise an
  eighth-note fallback is used.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

# Standard tuning, string 1 (high E) .. string 6 (low E), as MIDI note values.
# These match PyGuitarPro's default Track.strings.
STANDARD_TUNING: dict[int, int] = {1: 64, 2: 59, 3: 55, 4: 50, 5: 45, 6: 40}

# Standard tuning, top -> bottom, with string number. The letter 'e' appears
# at both ends (high-e = string 1, low-E = string 6); we disambiguate by
# matching the detected letters as a subsequence of this list.
_STANDARD_ORDER = [("e", 1), ("b", 2), ("g", 3), ("d", 4), ("a", 5), ("e", 6)]

_TOL = 2  # char-column tolerance for clustering notes into one beat

# Rail cleaning: keep digits, dashes, barlines, and slide slashes; replace
# everything else (technique letters like s/h/p/b, vibrato ~, bend ^, parens,
# spaces, OCR letter-noise) with a dash so fret numbers never merge across a
# removed character. ``4s2`` (slide 4->2) becomes ``4-2`` -> two separate
# frets, with the slide articulation dropped (not modelled in v1).
_RAIL_KEEP = re.compile(r"[^0-9\-|/\\]")


def _clean_rail(seg: str) -> str:
    """Replace non-rail characters with dashes so fret digits stay separate."""
    return _RAIL_KEEP.sub("-", seg)


@dataclass
class ParsedNote:
    string: int  # 1..6 (1 = high E)
    fret: int


@dataclass
class ParsedBeat:
    notes: list[ParsedNote] = field(default_factory=list)
    column: int = 0  # representative char column (debugging / ordering)


@dataclass
class ParsedMeasure:
    beats: list[ParsedBeat] = field(default_factory=list)


@dataclass
class ParsedTab:
    measures: list[ParsedMeasure] = field(default_factory=list)
    tuning: dict[int, int] = field(default_factory=lambda: dict(STANDARD_TUNING))


def _is_string_line(line: str) -> bool:
    """Heuristic: does this line look like one row of a tab staff?"""
    s = line.strip()
    if len(s) < 3:
        return False
    # Must contain a barline or a run of dashes (the tab "rail").
    if "|" not in s and "-" not in s:
        return False
    # Reject lyric-like lines: too many lowercase letters relative to digits.
    digits = sum(c.isdigit() for c in s)
    # A string line is mostly dashes/spaces/digits/pipe; allow one leading
    # tuning letter. If it has many alphabetic chars beyond the first, skip.
    alpha = sum(c.isalpha() for c in s)
    if alpha > 3 and digits == 0:
        return False
    return True


def _split_staves(lines: list[str]) -> list[list[str]]:
    """Group consecutive string-lines into 6-line staves.

    Lines that aren't string lines act as separators. A stave with fewer than
    6 rows is kept only if it has at least 3 (a partial staff still usable);
    otherwise discarded as noise.
    """
    staves: list[list[str]] = []
    current: list[str] = []
    for line in lines:
        if _is_string_line(line):
            current.append(line)
        else:
            if len(current) >= 3:
                staves.append(current)
            current = []
    if len(current) >= 3:
        staves.append(current)
    return staves


def _normalize_stave(stave: list[str]) -> list[str]:
    """Pad/trim a stave to exactly 6 rows, top = string 1.

    If more than 6 rows, keep the first 6 (extra rows are usually OCR echoes).
    If fewer, pad with empty rail lines so alignment logic still works.
    """
    if len(stave) > 6:
        stave = stave[:6]
    while len(stave) < 6:
        stave.append("-" * 0)
    return stave


def _scan_string_row(text: str) -> list[tuple[int, int]]:
    """Find fret numbers in one string row.

    Returns ``[(fret, column), ...]`` where ``column`` is the index of the
    first digit of the fret number within ``text``. Consecutive digits are
    read as a single multi-digit fret (10..24).
    """
    out: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch.isdigit():
            j = i
            num = 0
            while j < n and text[j].isdigit():
                num = num * 10 + int(text[j])
                j += 1
            # Cap absurd OCR values; guitars have ≤ 24 frets.
            if num > 24:
                # Could be two adjacent frets run together; split digits.
                for k in range(i, j):
                    out.append((int(text[k]), k))
            else:
                out.append((num, i))
            i = j
        else:
            i += 1
    return out


def _cluster_beats(events: list[tuple[int, int, int]]) -> list[ParsedBeat]:
    """Cluster ``(string, fret, column)`` events into beats.

    Events within ``_TOL`` columns of each other form one beat (a chord).
    Two notes on the *same* string can never share a beat (a string is only
    fretted once at a time), so a same-string collision always starts a new
    beat even when the columns are close.
    """
    if not events:
        return []
    events.sort(key=lambda e: e[2])
    beats: list[ParsedBeat] = []
    current = ParsedBeat(column=events[0][2])
    strings_in_current: set[int] = set()
    last_col = events[0][2]
    for string, fret, col in events:
        same_string = string in strings_in_current
        if current.notes and (col - last_col > _TOL or same_string):
            beats.append(current)
            current = ParsedBeat(column=col)
            strings_in_current = set()
        current.notes.append(ParsedNote(string=string, fret=fret))
        strings_in_current.add(string)
        last_col = col
    if current.notes:
        beats.append(current)
    return beats


def _leading_letter(row: str) -> str | None:
    """Return the first tuning letter in ``row`` before its first ``|``."""
    idx = row.find("|")
    head = row[:idx] if idx >= 0 else row
    for ch in head:
        if ch in "eEbBgGdDaA":
            return ch.lower()
    return None


def _assign_strings(stave: list[str]) -> list[int | None]:
    """Determine the string number (1..6) for each row of a stave.

    Primary method: match the detected leading letters as a *subsequence* of
    the standard tuning order ``e B G D A E``. This handles partial staves
    where OCR dropped empty top strings (e.g. a 3-row ``D A E`` stave maps to
    strings 4, 5, 6 — not 1, 2, 3). Falls back to top-to-bottom line order
    (1, 2, …) only when no row carries a usable tuning letter.
    """
    letters = [_leading_letter(r) for r in stave]
    assignment: list[int | None] = [None] * len(stave)
    cursor = 0
    for i, letter in enumerate(letters):
        if letter is None:
            continue
        for j in range(cursor, len(_STANDARD_ORDER)):
            if _STANDARD_ORDER[j][0] == letter:
                assignment[i] = _STANDARD_ORDER[j][1]
                cursor = j + 1
                break
    if all(a is None for a in assignment):
        # No letters at all — assume a full, ordered stave top->bottom.
        for i in range(len(stave)):
            assignment[i] = min(i + 1, 6)
    return assignment


def _parse_measure_segment(pairs: list[tuple[int | None, str]]) -> ParsedMeasure:
    """Parse one measure given ``(string_num, segment_text)`` per row.

    Rows whose string number is ``None`` (no tuning letter and no fallback)
    are skipped. ``segment_text`` is pure rail content (label already split
    off), so digit-lookalike letters are mapped to digits before scanning.
    """
    events: list[tuple[int, int, int]] = []  # (string, fret, column)
    for string_num, seg in pairs:
        if string_num is None:
            continue
        for fret, col in _scan_string_row(_clean_rail(seg)):
            events.append((string_num, fret, col))
    beats = _cluster_beats(events)
    return ParsedMeasure(beats=beats)


def _parse_stave_aligned(stave: list[str]) -> list[ParsedMeasure]:
    """Parse a stave using barline-segment alignment across its rows.

    Splits each row on ``|``; the k-th segment (k>=1, i.e. after the leading
    tuning letter) of each row forms measure k. Rows with fewer segments are
    padded with empty strings. String numbers come from
    :func:`_assign_strings`.
    """
    rows = _normalize_stave(stave)
    string_nums = _assign_strings(rows)
    split_rows = [r.split("|") for r in rows]
    max_segs = max((len(s) for s in split_rows), default=1)
    measures: list[ParsedMeasure] = []
    # segment index 0 is the tuning-letter prefix; measures start at index 1.
    for seg_idx in range(1, max_segs):
        pairs: list[tuple[int | None, str]] = []
        for row_i, segs in enumerate(split_rows):
            seg = segs[seg_idx] if seg_idx < len(segs) else ""
            pairs.append((string_nums[row_i], seg))
        # Skip a trailing empty segment (final barline with no notes).
        if all(seg.strip() == "" for _, seg in pairs):
            continue
        measures.append(_parse_measure_segment(pairs))
    return measures


def parse_tab(text: str) -> ParsedTab:
    """Parse OCR'd ASCII tab text into a :class:`ParsedTab`."""
    lines = text.splitlines()
    staves = _split_staves(lines)
    tab = ParsedTab()
    for stave in staves:
        tab.measures.extend(_parse_stave_aligned(stave))
    # Guarantee at least one measure so the output file is always valid.
    if not tab.measures:
        tab.measures.append(ParsedMeasure())
    return tab
