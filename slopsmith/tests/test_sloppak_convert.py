"""Tests for lib/sloppak_convert.py pure helpers (sanitize_stem, _arrangement_id).

Both are regex-based string helpers with zero fixture cost. See issue #45.
"""

import pytest

from sloppak_convert import sanitize_stem, _arrangement_id


# ── sanitize_stem ────────────────────────────────────────────────────────────
# Regex replaces [^A-Za-z0-9._-]+ with "_", strips leading/trailing "_",
# falls back to "song" for empty result.

SANITIZE_CASES = [
    ("cleanname", "cleanname"),                          # passthrough
    ("song_v2.mp3", "song_v2.mp3"),                      # dot and underscore preserved
    ("safe-name.ogg", "safe-name.ogg"),                  # hyphen preserved
    ("my track", "my_track"),                            # space -> underscore
    ("my   track", "my_track"),                          # run of spaces collapses to one _
    ("path/to/file", "path_to_file"),                    # slashes -> underscore
    ("weird!@#name", "weird_name"),                      # punctuation run collapses to one _
    ("  spaced  ", "spaced"),                            # leading/trailing _ stripped
    ("__both__", "both"),                                # chained _ stripped at ends
    ("___", "song"),                                     # all-underscore input -> fallback
    ("", "song"),                                        # empty input -> fallback
    ("!!!", "song"),                                     # all-forbidden-chars -> fallback
    ("ünicöde", "nic_de"),                               # non-ASCII chars collapse to "_" via [^A-Za-z0-9._-]
]


@pytest.mark.parametrize("raw,expected", SANITIZE_CASES)
def test_sanitize_stem(raw, expected):
    assert sanitize_stem(raw) == expected


# ── _arrangement_id ──────────────────────────────────────────────────────────
# Regex replaces [^a-z0-9]+ with "_" in the lowercased input, strips "_",
# falls back to "arr" for empty. On collision, appends 2/3/… and mutates
# the `used` set in place.

def test_arrangement_id_first_call_passes_through():
    used = set()
    assert _arrangement_id("Lead", used) == "lead"
    assert used == {"lead"}


def test_arrangement_id_deduplicates_on_collision():
    used = {"lead"}
    assert _arrangement_id("Lead", used) == "lead2"
    assert used == {"lead", "lead2"}


def test_arrangement_id_chains_through_multiple_collisions():
    used = {"lead", "lead2"}
    assert _arrangement_id("Lead", used) == "lead3"
    assert used == {"lead", "lead2", "lead3"}


def test_arrangement_id_lowercases_and_strips_punctuation():
    used = set()
    assert _arrangement_id("Part Bass-01", used) == "part_bass_01"


def test_arrangement_id_empty_input_falls_back_to_arr():
    used = set()
    assert _arrangement_id("", used) == "arr"
    assert used == {"arr"}


def test_arrangement_id_all_punctuation_falls_back_to_arr():
    used = set()
    assert _arrangement_id("!!!", used) == "arr"
    assert used == {"arr"}


def test_arrangement_id_mutates_the_used_set_in_place():
    used = set()
    _arrangement_id("Rhythm", used)
    _arrangement_id("Rhythm", used)
    _arrangement_id("Rhythm", used)
    assert used == {"rhythm", "rhythm2", "rhythm3"}
