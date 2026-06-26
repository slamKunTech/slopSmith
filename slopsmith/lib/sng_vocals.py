"""Decrypt + parse Rocksmith 2014 vocals SNG files.

RsCli's sng2xml only handles instrumental arrangements, so official DLC
(which ships SNG-only) has no lyrics path. This module decodes the vocals
SNG directly so lyrics can be served for both official DLC and CDLC.

SNG vocals file format (little-endian unless noted)
────────────────────────────────────────────────────
Top-level envelope (what `_decrypt_sng` strips):

    offset  size  field
    ──────  ────  ──────────────────────────────────────
     0      4     magic (u32)        # ignored by the decoder
     4      4     version (u32)      # ignored by the decoder
     8     16     iv                 # AES-CTR initial counter
    24     N      encrypted_payload  # AES-CTR ciphertext
   -56    56     signature footer   # ignored by the decoder

`encrypted_payload`, after AES-CTR decryption with the platform key
(_PC_KEY or _MAC_KEY), starts with:

    +0   4     uncompressed_size (big-endian u32)
    +4   ...   zlib stream

Decompressed body for a vocals arrangement:

    +0   16    four u32 zeros (section counts: beats / phrases /
                  chord_templates / chord_notes — all 0 for vocals)
    +16   4    vocal_count (u32)
    +20   N*60 vocal entries

Each 60-byte vocal entry:

    +0    4     time   (float32)
    +4    4     note   (int32; unused — vocals aren't pitched here)
    +8    4     length (float32)
    +12  48     lyric  (utf-8, null-terminated, zero-padded)

A minimal round-trip encoder lives in `tests/test_sng_vocals.py` —
the test-only inverse is the spec's source of truth when reading
this alongside the parser.
"""

import struct
import zlib

try:
    from Crypto.Cipher import AES
    from Crypto.Util import Counter
except ImportError:
    AES = None  # type: ignore
    Counter = None  # type: ignore

# Well-known Rocksmith 2014 SNG AES keys (public, used by sng2014HSL et al).
_PC_KEY = bytes.fromhex(
    "CB648DF3D12A16BF71701414E69619EC171CCA5D2A142E3E59DE7ADDA18A3A30"
)
_MAC_KEY = bytes.fromhex(
    "9821330E34B91F70D0A48CBD62599312" "6970CEA09192C0E6CDA676CC9838289D"
)


def _decrypt_sng(data: bytes, platform: str) -> bytes:
    if AES is None:
        raise RuntimeError("pycryptodome not available")
    # Header: u32 magic, u32 version, 16-byte IV, payload..., 56-byte signature
    if len(data) < 24 + 56:
        raise ValueError("SNG too small")
    iv = data[8:24]
    encrypted = data[24:-56]
    key = _MAC_KEY if platform == "mac" else _PC_KEY
    ctr = Counter.new(128, initial_value=int.from_bytes(iv, "big"))
    cipher = AES.new(key, AES.MODE_CTR, counter=ctr)
    decrypted = cipher.decrypt(encrypted)
    # First 4 bytes big-endian uncompressed size, then zlib stream.
    return zlib.decompress(decrypted[4:])


def parse_vocals_sng(path: str, platform: str = "pc") -> list[dict]:
    """Return lyrics in the same wire shape the highway WS expects:
    [{"t": float, "d": float, "w": str}, ...]"""
    with open(path, "rb") as f:
        raw = f.read()
    try:
        body = _decrypt_sng(raw, platform)
    except Exception:
        return []

    # Vocals SNG layout: four empty u32 section counts (beats/phrases/
    # chord_templates/chord_notes, all zero for a vocals-only track), then the
    # vocals section itself: u32 count followed by N × 60-byte entries.
    entry_size = 60
    header_skip = 16  # four zero u32s preceding the vocal count
    if len(body) < header_skip + 4:
        return []
    count = struct.unpack_from("<I", body, header_skip)[0]
    if count == 0 or len(body) < header_skip + 4 + count * entry_size:
        return []

    out: list[dict] = []
    off = header_skip + 4
    for _ in range(count):
        time, _note, length = struct.unpack_from("<fif", body, off)
        lyric_raw = body[off + 12 : off + 60]
        nul = lyric_raw.find(b"\x00")
        if nul >= 0:
            lyric_raw = lyric_raw[:nul]
        try:
            lyric = lyric_raw.decode("utf-8")
        except UnicodeDecodeError:
            lyric = lyric_raw.decode("latin-1", errors="replace")
        out.append({"t": round(float(time), 3), "d": round(float(length), 3), "w": lyric})
        off += entry_size
    return out
