"""Sort image filenames by their Roman-numeral suffix.

Song pages are named with a Roman-numeral suffix to indicate order, e.g.
``riff_I.png``, ``riff_II.png``, ``riff_IV.png``. Plain lexicographic sort
would put ``X`` before ``V`` etc., so we extract the trailing Roman numeral
and sort by its integer value.
"""

from __future__ import annotations

import re
from functools import cmp_to_key

# Matches a Roman numeral token (1..3999). We delimit by non-letters (not \b)
# so that suffixes like ``song_I.png`` match: ``_`` is a word char but not a
# letter, and the numeral must not be part of a larger alphabetic word.
_ROMAN_RE = re.compile(
    r"(?<![A-Za-z])M{0,3}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3})(?![A-Za-z])",
    re.IGNORECASE,
)

# Single-letter tunings also look like Roman numerals (e, B, G, D, A, E, c, ...).
# We only treat a token as a numeral if it is purely Roman-uppercase/lowercase
# letters from {I,V,X,L,C,D,M}; the regex already enforces that.


def roman_to_int(roman: str) -> int:
    """Convert a Roman numeral string to an int. Returns 0 for empty/invalid."""
    values = {"I": 1, "V": 5, "X": 10, "L": 50, "C": 100, "D": 500, "M": 1000}
    s = roman.upper()
    total, prev = 0, 0
    for ch in reversed(s):
        if ch not in values:
            return 0
        cur = values[ch]
        total += -cur if cur < prev else cur
        prev = cur
    return total


def roman_suffix(filename: str) -> int | None:
    """Return the integer value of the *last* Roman numeral in ``filename``.

    Returns ``None`` if no Roman numeral is found.
    """
    matches = _ROMAN_RE.findall(filename)
    # findall with groups returns tuples; re-find non-overlapping instead.
    tokens = _ROMAN_RE.finditer(filename)
    last: str | None = None
    for m in tokens:
        last = m.group(0)
    if last is None:
        return None
    value = roman_to_int(last)
    return value if value > 0 else None


def sort_by_roman(paths: list[str]) -> list[str]:
    """Stable-sort ``paths`` by Roman-numeral suffix.

    Paths with a Roman suffix come first, ordered by value; paths without one
    follow in their original (alphabetical) order.
    """

    def cmp(a: str, b: str) -> int:
        ra, rb = roman_suffix(a), roman_suffix(b)
        if ra is not None and rb is not None:
            return (ra > rb) - (ra < rb)
        if ra is not None:
            return -1
        if rb is not None:
            return 1
        return (a > b) - (a < b)

    return sorted(paths, key=cmp_to_key(cmp))
