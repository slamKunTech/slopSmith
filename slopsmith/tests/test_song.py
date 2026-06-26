"""Tests for lib/song.py wire-format serialization (pure, no fixtures)."""

import json

import pytest

from song import (
    Anchor,
    Arrangement,
    Chord,
    ChordTemplate,
    HandShape,
    Note,
    Phrase,
    PhraseLevel,
    arrangement_from_wire,
    arrangement_string_count,
    arrangement_to_wire,
    chord_from_wire,
    chord_to_wire,
    note_from_wire,
    note_to_wire,
    phrase_from_wire,
    phrase_to_wire,
)


# ── Note round-trip ──────────────────────────────────────────────────────────

def test_note_minimal_round_trip():
    n = Note(time=1.0, string=2, fret=5)
    assert note_from_wire(note_to_wire(n)) == n


def test_note_with_every_technique_round_trip():
    n = Note(
        time=0.5, string=0, fret=3,
        sustain=0.25,
        slide_to=7,
        slide_unpitch_to=9,
        bend=1.0,
        hammer_on=True, pull_off=True,
        harmonic=True, harmonic_pinch=True,
        palm_mute=True, mute=True,
        tremolo=True, accent=True,
        tap=True,
    )
    assert note_from_wire(note_to_wire(n)) == n


def test_note_link_next_is_not_round_tripped():
    """link_next is deliberately omitted from the wire format.

    Acknowledged behavior, not a regression — chord linking is derived elsewhere.
    Pinning this so nobody "fixes" it accidentally and breaks downstream assumptions.
    """
    n = Note(time=0.0, string=0, fret=0, link_next=True)
    assert note_to_wire(n) == {
        "t": 0.0, "s": 0, "f": 0, "sus": 0.0,
        "sl": -1, "slu": -1, "bn": 0,
        "ho": False, "po": False,
        "hm": False, "hp": False,
        "pm": False, "mt": False,
        "tr": False, "ac": False, "tp": False,
    }
    assert note_from_wire(note_to_wire(n)).link_next is False


def test_note_time_rounded_to_three_decimals():
    n = Note(time=1.23456789, string=0, fret=0)
    assert note_to_wire(n)["t"] == 1.235


def test_note_bend_zero_serializes_as_integer_zero():
    # note_to_wire uses `round(bend, 1) if bend else 0` — the else branch returns int 0.
    # from_wire then float()s it back. Pin this quirk so a refactor doesn't surprise callers.
    wire = note_to_wire(Note(time=0.0, string=0, fret=0, bend=0.0))
    assert wire["bn"] == 0
    assert isinstance(wire["bn"], int)


def test_note_bend_nonzero_rounded_to_one_decimal():
    n = Note(time=0.0, string=0, fret=0, bend=1.75)
    assert note_to_wire(n)["bn"] == 1.8


# ── Chord round-trip ─────────────────────────────────────────────────────────

def test_chord_with_multiple_notes_round_trip():
    c = Chord(
        time=2.0,
        chord_id=5,
        high_density=False,
        notes=[
            Note(time=2.0, string=0, fret=3),
            Note(time=2.0, string=1, fret=5),
            Note(time=2.0, string=2, fret=5),
        ],
    )
    assert chord_from_wire(chord_to_wire(c)) == c


def test_chord_high_density_round_trip():
    c = Chord(
        time=1.5, chord_id=2, high_density=True,
        notes=[Note(time=1.5, string=0, fret=0)],
    )
    assert chord_from_wire(chord_to_wire(c)) == c


def test_chord_notes_inherit_chord_time_on_deserialization():
    """chord_note_to_wire strips each note's time; chord_from_wire replays the chord time.

    So notes constructed with mismatched times are normalized by the round-trip.
    """
    c = Chord(
        time=3.0, chord_id=0,
        notes=[
            Note(time=99.0, string=0, fret=0),  # will be normalized to 3.0
            Note(time=42.5, string=1, fret=1),  # will be normalized to 3.0
        ],
    )
    result = chord_from_wire(chord_to_wire(c))
    assert all(n.time == 3.0 for n in result.notes)


# ── Arrangement round-trip ───────────────────────────────────────────────────

def test_arrangement_empty_round_trip():
    arr = Arrangement(name="Lead")
    assert arrangement_from_wire(arrangement_to_wire(arr)) == arr


def test_arrangement_full_round_trip():
    arr = Arrangement(
        name="Rhythm",
        tuning=[-2, 0, 0, 0, 0, 0],
        capo=2,
        notes=[
            Note(time=1.0, string=0, fret=3, palm_mute=True),
            Note(time=1.5, string=1, fret=5, hammer_on=True),
        ],
        chords=[
            Chord(
                time=2.0, chord_id=1, high_density=True,
                notes=[
                    Note(time=2.0, string=0, fret=0),
                    Note(time=2.0, string=1, fret=2),
                ],
            ),
        ],
        anchors=[
            Anchor(time=0.0, fret=1, width=4),
            Anchor(time=10.0, fret=7, width=5),
        ],
        hand_shapes=[
            HandShape(chord_id=1, start_time=2.0, end_time=2.5),
        ],
        chord_templates=[
            ChordTemplate(
                name="Em",
                fingers=[-1, -1, 2, 3, -1, -1],
                frets=[0, 2, 2, 0, 0, 0],
            ),
        ],
    )
    assert arrangement_from_wire(arrangement_to_wire(arr)) == arr


def test_arrangement_default_tuning_is_six_zeros():
    arr = Arrangement(name="Bass")
    assert arr.tuning == [0, 0, 0, 0, 0, 0]


def test_arrangement_from_wire_missing_fields_use_defaults():
    # Minimal wire dict — every list field defaults to empty, capo to 0,
    # tuning to six zeros.
    arr = arrangement_from_wire({"name": "Lead"})
    assert arr.name == "Lead"
    assert arr.tuning == [0, 0, 0, 0, 0, 0]
    assert arr.capo == 0
    assert arr.notes == []
    assert arr.chords == []
    assert arr.anchors == []
    assert arr.hand_shapes == []
    assert arr.chord_templates == []
    # phrases is the "slider disabled" sentinel — absent key → None, NOT [].
    assert arr.phrases is None


# ── Phrase / master-difficulty round-trip (slopsmith#48) ─────────────────────

def test_phrase_empty_round_trip():
    p = Phrase(start_time=0.0, end_time=10.0, max_difficulty=0, levels=[])
    assert phrase_from_wire(phrase_to_wire(p)) == p


def test_phrase_times_rounded_to_three_decimals():
    # Pin the rounding behaviour for start_time / end_time so accidental
    # precision changes (which would shift frontend event timing or break
    # sloppak round-trips) are caught by the suite.
    p = Phrase(start_time=1.234567, end_time=9.876543, max_difficulty=0, levels=[])
    wire = phrase_to_wire(p)
    assert wire["start_time"] == 1.235
    assert wire["end_time"] == 9.877


def test_phrase_with_multiple_levels_round_trip():
    p = Phrase(
        start_time=4.5, end_time=12.25, max_difficulty=2,
        levels=[
            PhraseLevel(
                difficulty=0,
                notes=[Note(time=5.0, string=0, fret=3)],
                chords=[],
                anchors=[Anchor(time=5.0, fret=3, width=4)],
                hand_shapes=[],
            ),
            PhraseLevel(
                difficulty=1,
                notes=[
                    Note(time=5.0, string=0, fret=3),
                    Note(time=6.5, string=1, fret=5, palm_mute=True),
                ],
                chords=[],
                anchors=[Anchor(time=5.0, fret=3, width=4)],
                hand_shapes=[],
            ),
            PhraseLevel(
                difficulty=2,
                notes=[
                    Note(time=5.0, string=0, fret=3),
                    Note(time=6.5, string=1, fret=5, palm_mute=True),
                ],
                chords=[
                    Chord(
                        time=8.0, chord_id=1,
                        notes=[
                            Note(time=8.0, string=0, fret=0),
                            Note(time=8.0, string=1, fret=2),
                        ],
                    ),
                ],
                anchors=[Anchor(time=5.0, fret=3, width=4)],
                hand_shapes=[HandShape(chord_id=1, start_time=8.0, end_time=8.5)],
            ),
        ],
    )
    assert phrase_from_wire(phrase_to_wire(p)) == p


def test_arrangement_with_phrases_round_trip():
    arr = Arrangement(
        name="Lead",
        phrases=[
            Phrase(
                start_time=0.0, end_time=8.0, max_difficulty=1,
                levels=[
                    PhraseLevel(difficulty=0, notes=[Note(time=1.0, string=0, fret=0)]),
                    PhraseLevel(difficulty=1, notes=[
                        Note(time=1.0, string=0, fret=0),
                        Note(time=2.0, string=0, fret=2),
                    ]),
                ],
            ),
        ],
    )
    assert arrangement_from_wire(arrangement_to_wire(arr)) == arr


def test_arrangement_wire_omits_phrases_when_none():
    # Slider-disabled sentinel: arrangements without phrase data must NOT
    # emit a "phrases" key. Frontends distinguish by presence, not value.
    arr = Arrangement(name="Bass")
    wire = arrangement_to_wire(arr)
    assert "phrases" not in wire


def test_arrangement_wire_emits_phrases_when_set():
    arr = Arrangement(
        name="Lead",
        phrases=[Phrase(start_time=0.0, end_time=4.0, max_difficulty=0, levels=[])],
    )
    wire = arrangement_to_wire(arr)
    assert "phrases" in wire
    assert wire["phrases"] == [{
        "start_time": 0.0, "end_time": 4.0,
        "max_difficulty": 0, "levels": [],
    }]


def test_arrangement_wire_omits_phrases_when_empty_list():
    # An empty list means "no phrase data" just like None — emitting
    # `"phrases": []` would signal slider-enabled-but-no-ladder, which
    # is an invalid state for consumers. Normalize at the wire boundary.
    arr = Arrangement(name="Rhythm", phrases=[])
    wire = arrangement_to_wire(arr)
    assert "phrases" not in wire


def test_arrangement_from_wire_empty_phrases_list_becomes_none():
    # Symmetric: an explicit `"phrases": []` on the wire must deserialize
    # to the None sentinel so the slider-disabled signal is preserved.
    arr = arrangement_from_wire({"name": "X", "phrases": []})
    assert arr.phrases is None


def test_phrase_wire_is_json_safe():
    p = Phrase(
        start_time=1.234, end_time=5.678, max_difficulty=1,
        levels=[
            PhraseLevel(
                difficulty=1,
                notes=[Note(time=2.0, string=0, fret=5, sustain=0.5, tap=True)],
                chords=[
                    Chord(time=3.0, chord_id=2, high_density=True,
                          notes=[Note(time=3.0, string=0, fret=0)]),
                ],
                anchors=[Anchor(time=2.0, fret=5, width=4)],
                hand_shapes=[HandShape(chord_id=2, start_time=3.0, end_time=3.5)],
            ),
        ],
    )
    wire = phrase_to_wire(p)
    # allow_nan=False rejects Infinity/NaN — which JS JSON.parse
    # also rejects. Keeps the wire strictly browser-compatible.
    assert json.loads(json.dumps(wire, allow_nan=False)) == wire


# ── Dataclass defaults ───────────────────────────────────────────────────────

def test_note_defaults():
    n = Note(time=0.0, string=0, fret=0)
    assert n.sustain == 0.0
    assert n.slide_to == -1
    assert n.slide_unpitch_to == -1
    assert n.bend == 0.0
    assert n.hammer_on is False
    assert n.pull_off is False
    assert n.harmonic is False
    assert n.harmonic_pinch is False
    assert n.palm_mute is False
    assert n.mute is False
    assert n.tremolo is False
    assert n.accent is False
    assert n.link_next is False
    assert n.tap is False


def test_anchor_default_width_is_four():
    a = Anchor(time=0.0, fret=1)
    assert a.width == 4


def test_chord_default_high_density_is_false():
    c = Chord(time=0.0, chord_id=0)
    assert c.high_density is False
    assert c.notes == []


# ── JSON-safety (#41) ────────────────────────────────────────────────────────
# The *_to_wire functions are documented as producing "JSON-ready" dicts that
# the highway WebSocket streams to the client. These tests catch things the
# Python-level round-trip tests above don't: non-JSON-native values (Path,
# Decimal, dataclass, set), and tuples (which JSON coerces to lists, failing
# the round-trip equality check).

def test_note_to_wire_is_json_safe():
    n = Note(
        time=0.5, string=0, fret=3,
        sustain=0.25, slide_to=7, slide_unpitch_to=9, bend=1.0,
        hammer_on=True, pull_off=True,
        harmonic=True, harmonic_pinch=True,
        palm_mute=True, mute=True,
        tremolo=True, accent=True, tap=True,
    )
    wire = note_to_wire(n)
    # allow_nan=False rejects Infinity/NaN — which JS JSON.parse
    # also rejects. Keeps the wire strictly browser-compatible.
    assert json.loads(json.dumps(wire, allow_nan=False)) == wire


def test_chord_to_wire_is_json_safe():
    c = Chord(
        time=2.0, chord_id=5, high_density=True,
        notes=[
            Note(time=2.0, string=0, fret=3, palm_mute=True),
            Note(time=2.0, string=1, fret=5),
            Note(time=2.0, string=2, fret=5),
        ],
    )
    wire = chord_to_wire(c)
    # allow_nan=False rejects Infinity/NaN — which JS JSON.parse
    # also rejects. Keeps the wire strictly browser-compatible.
    assert json.loads(json.dumps(wire, allow_nan=False)) == wire


def test_arrangement_to_wire_is_json_safe():
    # Same shape as test_arrangement_full_round_trip — exercises every nested
    # list / dict / int / str / bool path the wire format emits.
    arr = Arrangement(
        name="Rhythm",
        tuning=[-2, 0, 0, 0, 0, 0],
        capo=2,
        notes=[
            Note(time=1.0, string=0, fret=3, palm_mute=True),
            Note(time=1.5, string=1, fret=5, hammer_on=True),
        ],
        chords=[
            Chord(
                time=2.0, chord_id=1, high_density=True,
                notes=[
                    Note(time=2.0, string=0, fret=0),
                    Note(time=2.0, string=1, fret=2),
                ],
            ),
        ],
        anchors=[
            Anchor(time=0.0, fret=1, width=4),
            Anchor(time=10.0, fret=7, width=5),
        ],
        hand_shapes=[
            HandShape(chord_id=1, start_time=2.0, end_time=2.5),
        ],
        chord_templates=[
            ChordTemplate(
                name="Em",
                fingers=[-1, -1, 2, 3, -1, -1],
                frets=[0, 2, 2, 0, 0, 0],
            ),
        ],
    )
    wire = arrangement_to_wire(arr)
    # allow_nan=False rejects Infinity/NaN — which JS JSON.parse
    # also rejects. Keeps the wire strictly browser-compatible.
    assert json.loads(json.dumps(wire, allow_nan=False)) == wire


# ── Wire-format default-value fallbacks (#44) ────────────────────────────────
# Pin the fallback values embedded in arrangement_from_wire() so future
# refactors can't silently change what a sparse wire dict deserializes to.

def test_anchor_missing_width_defaults_to_four():
    # arrangement_from_wire: `width=int(a.get("width", 4))` at song.py:198
    arr = arrangement_from_wire({
        "name": "Lead",
        "anchors": [{"time": 0.0, "fret": 1}],  # no "width" key
    })
    assert len(arr.anchors) == 1
    assert arr.anchors[0].width == 4


def test_chord_template_missing_fingers_frets_defaults_to_negative_ones():
    # arrangement_from_wire: fingers/frets default to `[-1] * 6` at song.py:209-210
    arr = arrangement_from_wire({
        "name": "Rhythm",
        "templates": [{"name": "Em"}],  # no "fingers" or "frets" keys
    })
    assert len(arr.chord_templates) == 1
    ct = arr.chord_templates[0]
    assert ct.name == "Em"
    assert ct.fingers == [-1, -1, -1, -1, -1, -1]
    assert ct.frets == [-1, -1, -1, -1, -1, -1]


def test_chord_with_empty_notes_list_round_trips():
    # A chord with no notes (unusual but valid input) should survive round-trip.
    c = Chord(time=1.0, chord_id=3, notes=[])
    assert chord_from_wire(chord_to_wire(c)) == c


# ── arrangement_string_count (slopsmith-plugin-3dhighway#7) ──────────────────

def test_string_count_4_for_bass_arrangement_with_full_string_usage():
    # 4-string bass: notes reference strings 0..3.
    arr = Arrangement(
        name="Bass",
        notes=[
            Note(time=0.0, string=0, fret=3),
            Note(time=1.0, string=2, fret=5),
            Note(time=2.0, string=3, fret=0),
        ],
    )
    assert arrangement_string_count(arr) == 4


def test_string_count_4_for_bass_with_sparse_string_usage():
    # 4-string bass with notes only on strings 0..2. Notes-derived
    # gives 3, but the name-based fallback bumps it to 4. This is
    # the case codex flagged as broken under the pure notes-derived
    # approach — a real-world bass line that doesn't touch the high
    # G string still has 4 strings on the instrument.
    arr = Arrangement(
        name="Bass",
        notes=[
            Note(time=0.0, string=0, fret=3),
            Note(time=1.0, string=1, fret=5),
            Note(time=2.0, string=2, fret=0),
        ],
    )
    assert arrangement_string_count(arr) == 4


def test_string_count_6_for_standard_guitar_with_full_string_usage():
    # Notes spread across all 6 strings.
    arr = Arrangement(
        name="Lead",
        notes=[Note(time=float(i), string=i, fret=0) for i in range(6)],
    )
    assert arrangement_string_count(arr) == 6


def test_string_count_6_for_guitar_with_sparse_string_usage():
    # 6-string lead chart with notes only on strings 0..4 (never
    # touches string 5, the highest-index string in RS indexing).
    # Notes-derived gives 5; name-based fallback (anything-not-bass
    # = 6) bumps to the correct 6.
    arr = Arrangement(
        name="Lead",
        notes=[Note(time=float(i), string=i, fret=0) for i in range(5)],
    )
    assert arrangement_string_count(arr) == 6


def test_string_count_uses_chord_notes_when_higher_than_single_notes():
    # Single notes only touch strings 0–2; the chord touches string 5.
    arr = Arrangement(
        name="Rhythm",
        notes=[Note(time=0.0, string=0, fret=0), Note(time=1.0, string=2, fret=3)],
        chords=[Chord(time=2.0, chord_id=0, notes=[
            Note(time=2.0, string=4, fret=0),
            Note(time=2.0, string=5, fret=0),
        ])],
    )
    assert arrangement_string_count(arr) == 6


def test_string_count_empty_bass_arrangement_returns_4():
    # Empty arrangement named "Bass" — name-based fallback wins.
    arr = Arrangement(name="Bass")
    assert arrangement_string_count(arr) == 4


def test_string_count_empty_non_bass_arrangement_returns_6():
    # Empty non-bass arrangement defaults to the canonical 6.
    arr = Arrangement(name="Lead")
    assert arrangement_string_count(arr) == 6


def test_string_count_7_for_extended_range_guitar():
    # 7-string guitar (GP-imported sources may carry these). Notes
    # span 0..6, so the notes-derived count is 7. The name-based
    # fallback gives 6, but max() picks the higher value — extended-
    # range arrangements are correctly handled WITHOUT having to
    # special-case "7-string" in the name.
    arr = Arrangement(
        name="Lead",
        notes=[Note(time=float(i), string=i, fret=0) for i in range(7)],
    )
    assert arrangement_string_count(arr) == 7


def test_string_count_5_for_extended_range_bass():
    # 5-string bass via GP import — notes span 0..4. Notes-derived
    # gives 5; name-based gives 4; max picks 5. No special-casing
    # for "5-string" in the arrangement name needed.
    arr = Arrangement(
        name="Bass",
        notes=[Note(time=float(i), string=i, fret=0) for i in range(5)],
    )
    assert arrangement_string_count(arr) == 5


def test_string_count_name_match_is_case_insensitive():
    arr_lower = Arrangement(name="bass")
    arr_upper = Arrangement(name="BASS")
    arr_mixed = Arrangement(name="Combo Bass")  # substring match
    assert arrangement_string_count(arr_lower) == 4
    assert arrangement_string_count(arr_upper) == 4
    assert arrangement_string_count(arr_mixed) == 4


def test_string_count_uses_tuning_length_for_sparse_extended_range_bass():
    # A sloppak / GP-imported 5-string bass may encode the
    # instrument range in tuning even if the chart never touches
    # the highest string index. tuning_count (5) wins over
    # notes_count (4) AND name_based (4) — extended-range bass
    # without name-based hints still resolves correctly.
    arr = Arrangement(
        name="Bass",
        tuning=[0, 0, 0, 0, 0],
        notes=[Note(time=float(i), string=i, fret=0) for i in range(4)],
    )
    assert arrangement_string_count(arr) == 5


def test_string_count_uses_tuning_length_for_sparse_7_string_guitar():
    # 7-string GP-imported guitar where the chart only uses
    # strings 0..5 (sparse top-string usage). tuning_count (7) is
    # the only reliable signal; notes_count gives 6 and name_based
    # gives 6.
    arr = Arrangement(
        name="Lead",
        tuning=[0, 0, 0, 0, 0, 0, 0],
        notes=[Note(time=float(i), string=i, fret=0) for i in range(6)],
    )
    assert arrangement_string_count(arr) == 7


def test_string_count_ignores_rs_padded_tuning_for_bass():
    # RS-XML bass: tuning is padded to length 6 with zeros at
    # indices 4-5. Even though len(tuning) == 6, we MUST NOT use
    # that as a 6-string signal (would mis-classify bass as
    # guitar). arrangement_string_count's `tuning_count = 0 if
    # tuning_len == 6 else tuning_len` rule takes care of this.
    arr = Arrangement(
        name="Bass",
        tuning=[0, -5, -10, -15, 0, 0],  # bass with RS XML padding
        notes=[Note(time=float(i), string=i, fret=0) for i in range(4)],
    )
    assert arrangement_string_count(arr) == 4
