"""PSARC file extractor for Rocksmith 2014."""

import fnmatch
import struct
import zlib
from pathlib import Path

try:
    from Crypto.Cipher import AES
except ImportError:
    # Pure-Python fallback for iOS/platforms without pycryptodome
    import aes_fallback as AES

MAGIC = b"PSAR"
BLOCK_SIZE = 65536
ENTRY_SIZE = 30

ARC_KEY = bytes.fromhex(
    "C53DB23870A1A2F71CAE64061FDD0E1157309DC85204D4C5BFDF25090DF2572C"
)
ARC_IV = bytes.fromhex("E915AA018FEF71FC508132E4BB4CEB42")


def _decrypt_toc(data: bytes) -> bytes:
    aes = AES.new(ARC_KEY, AES.MODE_CFB, iv=ARC_IV, segment_size=128)
    return aes.decrypt(data)


def _extract_entry(f, entry: dict, block_sizes: list, block_size: int) -> bytes:
    f.seek(entry["offset"])
    if entry["length"] == 0:
        return b""

    num_blocks = (entry["length"] + block_size - 1) // block_size
    result = b""

    for i in range(num_blocks):
        bi = entry["z_index"] + i
        compressed_size = block_sizes[bi] if bi < len(block_sizes) else 0

        if compressed_size == 0:
            remaining = entry["length"] - len(result)
            result += f.read(min(block_size, remaining))
        else:
            block_data = f.read(compressed_size)
            try:
                result += zlib.decompress(block_data)
            except zlib.error:
                result += block_data

    return result[: entry["length"]]


def _parse_toc(f):
    """Parse PSARC header, TOC, and file listing. Returns (entries, filenames, block_sizes, block_size)."""
    magic = f.read(4)
    if magic != MAGIC:
        raise ValueError("Not a PSARC file")

    _version = struct.unpack(">I", f.read(4))[0]
    _compression = f.read(4)
    toc_length = struct.unpack(">I", f.read(4))[0]
    toc_entry_size = struct.unpack(">I", f.read(4))[0]
    toc_entries = struct.unpack(">I", f.read(4))[0]
    block_size = struct.unpack(">I", f.read(4))[0]
    archive_flags = struct.unpack(">I", f.read(4))[0]

    toc_region_size = toc_length - 32
    toc_region_raw = f.read(toc_region_size)

    if archive_flags == 4:
        toc_region = _decrypt_toc(toc_region_raw)
    else:
        toc_region = toc_region_raw

    toc_data_size = toc_entry_size * toc_entries
    toc_data = toc_region[:toc_data_size]
    bt_data = toc_region[toc_data_size:]

    entries = []
    for i in range(toc_entries):
        off = i * toc_entry_size
        ed = toc_data[off : off + toc_entry_size]
        z_index = struct.unpack(">I", ed[16:20])[0]
        length = int.from_bytes(ed[20:25], "big")
        offset = int.from_bytes(ed[25:30], "big")
        entries.append({"z_index": z_index, "length": length, "offset": offset})

    block_sizes = []
    for i in range(len(bt_data) // 2):
        block_sizes.append(int.from_bytes(bt_data[i * 2 : i * 2 + 2], "big"))

    file_list_data = _extract_entry(f, entries[0], block_sizes, block_size)
    filenames = (
        file_list_data.decode("utf-8", errors="ignore")
        .replace("\r\n", "\n")
        .strip()
        .split("\n")
    )
    return entries, filenames, block_sizes, block_size


def read_psarc_entries(filepath: str, patterns: list[str] | None = None) -> dict[str, bytes]:
    """Read specific files from a PSARC archive directly into memory.

    Args:
        filepath: Path to the PSARC file.
        patterns: Optional list of glob patterns to match (e.g. ["*.json", "*.xml"]).
                  If None, reads all entries.

    Returns:
        Dict mapping internal paths to their raw bytes content.
    """
    result = {}
    with open(filepath, "rb") as f:
        entries, filenames, block_sizes, block_size = _parse_toc(f)

        for entry, filename in zip(entries[1:], filenames):
            filename = filename.strip()
            if not filename:
                continue
            if patterns is not None:
                if not any(fnmatch.fnmatch(filename.lower(), p.lower()) for p in patterns):
                    continue
            try:
                data = _extract_entry(f, entry, block_sizes, block_size)
                result[filename] = data
            except Exception:
                pass
    return result


def unpack_psarc(filepath: str, output_dir: str) -> list[str]:
    """Extract a PSARC archive. Returns list of extracted file paths."""
    extracted = []

    with open(filepath, "rb") as f:
        entries, filenames, block_sizes, block_size = _parse_toc(f)

        out = Path(output_dir)
        for entry, filename in zip(entries[1:], filenames):
            filename = filename.strip()
            if not filename:
                continue
            outpath = out / filename
            outpath.parent.mkdir(parents=True, exist_ok=True)
            try:
                data = _extract_entry(f, entry, block_sizes, block_size)
                outpath.write_bytes(data)
                extracted.append(str(outpath))
            except Exception:
                outpath.write_bytes(b"")

    return extracted
