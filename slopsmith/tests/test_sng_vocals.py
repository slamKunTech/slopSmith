"""Tests for lib/sng_vocals.py — Rocksmith 2014 vocals SNG parser.

We can't ship real Rocksmith SNG bytes (copyright), so each test builds
a fresh SNG in-memory via `_encode_vocals_sng()` — a minimal writer that
inverts the parser. The encoder doubles as executable documentation of
the format described in lib/sng_vocals.py's top-of-file docstring; if
the parser ever changes, the encoder has to move with it, which keeps
the spec honest.

Covers the round-trip path (encode → decrypt → decompress → parse) and
the failure modes `parse_vocals_sng` swallows (bad ciphertext, truncated
header, zero count) where it returns [].
"""

import struct
import zlib

import pytest

from Crypto.Cipher import AES
from Crypto.Util import Counter

from sng_vocals import parse_vocals_sng, _PC_KEY, _MAC_KEY


# ── Encoder (inverse of parser) ──────────────────────────────────────────────

def _encode_entry(time: float, length: float, word: str, note: int = 0) -> bytes:
    """Build one 60-byte vocal entry: time(f32) + note(i32) + length(f32) +
    48-byte null-terminated utf-8 lyric."""
    lyric_bytes = word.encode("utf-8")
    if len(lyric_bytes) > 47:
        raise ValueError("lyric too long for 48-byte slot")
    padded = lyric_bytes + b"\x00" * (48 - len(lyric_bytes))
    return struct.pack("<fif", time, note, length) + padded


def _encode_vocals_sng(
    entries: list[tuple[float, float, str]],
    platform: str = "pc",
    iv: bytes = b"\x00" * 16,
) -> bytes:
    """Build an encrypted SNG file carrying the given vocal entries.

    Mirrors the spec in lib/sng_vocals.py — if this writer and
    _decrypt_sng + parse_vocals_sng ever disagree, the round-trip tests
    below will fail loudly.
    """
    # Decompressed body: four zero u32 section counts, then vocal count
    # + entries.
    body = b"\x00" * 16
    body += struct.pack("<I", len(entries))
    for t, d, w in entries:
        body += _encode_entry(t, d, w)

    # zlib-compress and prepend the 4-byte big-endian uncompressed size.
    compressed = zlib.compress(body)
    payload = struct.pack(">I", len(body)) + compressed

    # AES-CTR encrypt with the platform key, using `iv` as the initial
    # counter. Pycryptodome's CTR needs an integer counter; we mirror
    # what _decrypt_sng does on the read side.
    key = _MAC_KEY if platform == "mac" else _PC_KEY
    ctr = Counter.new(128, initial_value=int.from_bytes(iv, "big"))
    cipher = AES.new(key, AES.MODE_CTR, counter=ctr)
    encrypted = cipher.encrypt(payload)

    # Wrap in 8-byte magic+version header + IV + encrypted + 56-byte
    # signature footer. Parser doesn't validate magic/version/signature;
    # zero-fill both so the test input is fully synthetic.
    header = b"\x00" * 8 + iv
    footer = b"\x00" * 56
    return header + encrypted + footer


def _write_sng(tmp_path, data: bytes) -> str:
    p = tmp_path / "vocals.sng"
    p.write_bytes(data)
    return str(p)


# ── Round-trip: entries in, entries out ─────────────────────────────────────

def test_single_entry_round_trips(tmp_path):
    sng = _encode_vocals_sng([(1.0, 0.5, "hello")])
    assert parse_vocals_sng(_write_sng(tmp_path, sng)) == [
        {"t": 1.0, "d": 0.5, "w": "hello"},
    ]


def test_multiple_entries_preserve_order_and_values(tmp_path):
    entries = [
        (0.5, 0.2, "la"),
        (1.25, 0.3, "la"),
        (2.0, 0.8, "la-la"),
        (4.5, 1.0, "end"),
    ]
    sng = _encode_vocals_sng(entries)
    got = parse_vocals_sng(_write_sng(tmp_path, sng))
    assert got == [
        {"t": round(t, 3), "d": round(d, 3), "w": w} for (t, d, w) in entries
    ]


def test_empty_vocals_section_returns_empty_list(tmp_path):
    # Valid file, zero entries — parser returns [] via the count==0 guard.
    sng = _encode_vocals_sng([])
    assert parse_vocals_sng(_write_sng(tmp_path, sng)) == []


# ── Text handling ───────────────────────────────────────────────────────────

def test_utf8_lyric_round_trips(tmp_path):
    # Multi-byte characters have to survive the 48-byte null-padded slot
    # + the parser's utf-8 decode.
    sng = _encode_vocals_sng([(1.0, 0.25, "niño"), (2.0, 0.25, "café")])
    got = parse_vocals_sng(_write_sng(tmp_path, sng))
    assert [e["w"] for e in got] == ["niño", "café"]


def test_lyric_padding_does_not_leak_into_decoded_word(tmp_path):
    # The 48-byte slot is zero-padded. Parser stops at the first null;
    # padding bytes must not appear in the output.
    sng = _encode_vocals_sng([(0.0, 0.1, "x")])
    got = parse_vocals_sng(_write_sng(tmp_path, sng))
    assert got[0]["w"] == "x"
    assert "\x00" not in got[0]["w"]


def test_float_precision_rounded_to_three_decimals(tmp_path):
    # Parser explicitly rounds via `round(float(...), 3)` — pin that
    # precision so an accidental change in rounding would fail here
    # before it silently shifts every downstream lyric timestamp.
    sng = _encode_vocals_sng([(1.234567, 0.876543, "hi")])
    got = parse_vocals_sng(_write_sng(tmp_path, sng))
    # float32 storage will introduce a tiny quantization error; round to
    # 3 decimals matches the parser's output contract.
    assert got[0]["t"] == pytest.approx(1.235, abs=5e-4)
    assert got[0]["d"] == pytest.approx(0.877, abs=5e-4)


# ── Platform key routing ────────────────────────────────────────────────────

def test_pc_key_parses_pc_encoded_file(tmp_path):
    sng = _encode_vocals_sng([(1.0, 0.5, "hi")], platform="pc")
    got = parse_vocals_sng(_write_sng(tmp_path, sng), platform="pc")
    assert got == [{"t": 1.0, "d": 0.5, "w": "hi"}]


def test_mac_key_parses_mac_encoded_file(tmp_path):
    sng = _encode_vocals_sng([(1.0, 0.5, "hi")], platform="mac")
    got = parse_vocals_sng(_write_sng(tmp_path, sng), platform="mac")
    assert got == [{"t": 1.0, "d": 0.5, "w": "hi"}]


def test_pc_file_parsed_with_mac_key_returns_empty(tmp_path):
    # Wrong key → decrypted bytes are garbage, zlib.decompress raises,
    # parser's try/except returns []. A must-have because slopsmith
    # auto-detects platform; a mismatch shouldn't crash the server.
    sng = _encode_vocals_sng([(1.0, 0.5, "hi")], platform="pc")
    got = parse_vocals_sng(_write_sng(tmp_path, sng), platform="mac")
    assert got == []


# ── Failure modes the parser swallows ───────────────────────────────────────

def test_file_smaller_than_header_returns_empty(tmp_path):
    # The decrypt layer rejects < 80 bytes (header + footer, no payload).
    assert parse_vocals_sng(_write_sng(tmp_path, b"too short")) == []


def test_corrupt_ciphertext_returns_empty(tmp_path):
    # Valid-looking envelope but random bytes inside. AES-CTR decrypts
    # without error but zlib fails; parser's try/except swallows it.
    junk = b"\x00" * 24 + b"\xde\xad\xbe\xef" * 64 + b"\x00" * 56
    assert parse_vocals_sng(_write_sng(tmp_path, junk)) == []


def test_truncated_body_returns_empty(tmp_path):
    # Build an encrypted file whose decompressed body is only 10 bytes —
    # not enough room for the 16-byte zero-count block. Parser's
    # len-check returns [] without crashing.
    short_body = b"\x00" * 10
    compressed = zlib.compress(short_body)
    payload = struct.pack(">I", len(short_body)) + compressed
    ctr = Counter.new(128, initial_value=0)
    cipher = AES.new(_PC_KEY, AES.MODE_CTR, counter=ctr)
    encrypted = cipher.encrypt(payload)
    sng = b"\x00" * 24 + encrypted + b"\x00" * 56
    assert parse_vocals_sng(_write_sng(tmp_path, sng)) == []


def test_count_exceeds_remaining_bytes_returns_empty(tmp_path):
    # Claim N entries but don't actually supply that many bytes — guards
    # against struct.unpack_from overrunning and returning junk data
    # for non-existent entries. Parser's arithmetic len-check catches
    # this before any entry is unpacked.
    body = b"\x00" * 16 + struct.pack("<I", 10)  # claims 10 entries, 0 bytes
    compressed = zlib.compress(body)
    payload = struct.pack(">I", len(body)) + compressed
    ctr = Counter.new(128, initial_value=0)
    cipher = AES.new(_PC_KEY, AES.MODE_CTR, counter=ctr)
    encrypted = cipher.encrypt(payload)
    sng = b"\x00" * 24 + encrypted + b"\x00" * 56
    assert parse_vocals_sng(_write_sng(tmp_path, sng)) == []
