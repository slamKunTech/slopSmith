"""Tests for lib/tunings.py: semitone-offset → human-readable tuning name."""

import pytest

from tunings import tuning_name


# ── Standard tunings (all six strings share the same offset) ─────────────────

STANDARD_CASES = [
    ([0, 0, 0, 0, 0, 0], "E Standard"),
    ([-1, -1, -1, -1, -1, -1], "Eb Standard"),
    ([-2, -2, -2, -2, -2, -2], "D Standard"),
    ([-3, -3, -3, -3, -3, -3], "C# Standard"),
    ([-4, -4, -4, -4, -4, -4], "C Standard"),
    ([-5, -5, -5, -5, -5, -5], "B Standard"),
    ([-6, -6, -6, -6, -6, -6], "Bb Standard"),
    ([-7, -7, -7, -7, -7, -7], "A Standard"),
    ([1, 1, 1, 1, 1, 1], "F Standard"),
    ([2, 2, 2, 2, 2, 2], "F# Standard"),
]


@pytest.mark.parametrize("offsets,expected", STANDARD_CASES)
def test_standard_tunings(offsets, expected):
    assert tuning_name(offsets) == expected


# ── Drop tunings (low string 2 semitones below the rest) ─────────────────────
# The auto-generator handles these; the explicit "Drop D" / "Drop C" entries in
# the named-tunings dict are effectively dead code because the auto-generator
# fires first and produces the same string.

DROP_CASES = [
    ([-2, 0, 0, 0, 0, 0], "Drop D"),
    ([-4, -2, -2, -2, -2, -2], "Drop C"),
    ([-3, -1, -1, -1, -1, -1], "Drop C#"),
    ([-5, -3, -3, -3, -3, -3], "Drop B"),
    ([-7, -5, -5, -5, -5, -5], "Drop A"),
    ([-8, -6, -6, -6, -6, -6], "Drop Ab"),
]


@pytest.mark.parametrize("offsets,expected", DROP_CASES)
def test_drop_tunings_auto_generated(offsets, expected):
    assert tuning_name(offsets) == expected


# ── Named tunings (non-drop patterns the auto-generator doesn't catch) ───────

NAMED_CASES = [
    ([-2, -2, 0, 0, 0, 0], "Double Drop D"),
    ([0, 0, 0, -1, 0, 0], "Open G"),
    ([-2, -2, 0, 0, -2, -2], "Open D"),
    ([-2, 0, 0, 0, -2, 0], "DADGAD"),
    ([0, 2, 2, 1, 0, 0], "Open E"),
    ([-2, 0, 0, 2, 3, 2], "Open D (alt)"),
]


@pytest.mark.parametrize("offsets,expected", NAMED_CASES)
def test_named_tunings(offsets, expected):
    assert tuning_name(offsets) == expected


# ── Fallback: unrecognized offsets stringify as space-joined numbers ─────────

def test_fallback_unrecognized_offsets():
    assert tuning_name([-3, -1, 0, 1, 2, 3]) == "-3 -1 0 1 2 3"


def test_fallback_with_seven_strings():
    # Seven-string mixed offsets — neither standard nor drop nor named match,
    # so we get the numeric fallback.
    assert tuning_name([-5, 0, 0, 0, 0, 0, 0]) == "-5 0 0 0 0 0 0"


# ── 7+-string regression tests (#43) ─────────────────────────────────────────
# The 6-string naming conventions (E Standard, Drop D, Double Drop D, etc.)
# don't generalize — a 7-string all-zeros has a low B, not an E. All three
# pattern checks are gated on len == 6; 7+ falls through to the numeric fallback.

SEVEN_STRING_FALLBACK_CASES = [
    # Previously mislabeled "E Standard" because len >= 6 + all-same matched.
    ([0, 0, 0, 0, 0, 0, 0], "0 0 0 0 0 0 0"),
    # Previously mislabeled "Eb Standard".
    ([-1, -1, -1, -1, -1, -1, -1], "-1 -1 -1 -1 -1 -1 -1"),
    # Previously mislabeled "Drop D" because the drop auto-generator matched
    # (offsets[0] == offsets[1] - 2, rest all equal).
    ([-2, 0, 0, 0, 0, 0, 0], "-2 0 0 0 0 0 0"),
    # Previously mislabeled "Drop C" similarly.
    ([-4, -2, -2, -2, -2, -2, -2], "-4 -2 -2 -2 -2 -2 -2"),
    # Previously mislabeled "Double Drop D" because the named-dict lookup used
    # tuple(offsets[:6]) which silently truncated the seventh offset.
    ([-2, -2, 0, 0, 0, 0, 0], "-2 -2 0 0 0 0 0"),
]


@pytest.mark.parametrize("offsets,expected", SEVEN_STRING_FALLBACK_CASES)
def test_seven_string_falls_through_to_fallback(offsets, expected):
    assert tuning_name(offsets) == expected


def test_five_string_falls_through_to_fallback():
    # Completeness: non-6 lengths on the low side fall through too.
    assert tuning_name([0, 0, 0, 0, 0]) == "0 0 0 0 0"


# ── Edge cases ───────────────────────────────────────────────────────────────

def test_empty_list_returns_unknown():
    # Empty offsets is the one case where the numeric fallback is useless —
    # `" ".join(str(o) for o in [])` is `""`, which used to flow downstream
    # as a blank badge. `or "Unknown"` kicks in only for empty input.
    assert tuning_name([]) == "Unknown"


def test_too_short_list_falls_through_to_fallback():
    # Fewer than 6 offsets — neither standard, drop, nor named match; fallback stringifies.
    assert tuning_name([-2, 0, 0]) == "-2 0 0"


def test_standard_dict_takes_precedence_over_numeric_fallback():
    # A list of 6 zeros could theoretically also hit the named-tunings tuple lookup
    # (if (0,0,0,0,0,0) were in there), but the standard-tuning branch runs first.
    # This test pins the priority.
    assert tuning_name([0, 0, 0, 0, 0, 0]) == "E Standard"


def test_drop_pattern_takes_precedence_over_named_dict():
    # [-2, 0, 0, 0, 0, 0] is in the named dict as "Drop D", but the drop-pattern
    # auto-generator fires first and produces the same string. The named dict entry
    # is effectively dead code for this case — this test documents the behavior.
    assert tuning_name([-2, 0, 0, 0, 0, 0]) == "Drop D"
