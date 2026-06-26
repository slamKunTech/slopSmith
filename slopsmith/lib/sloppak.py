"""Sloppak — open song format loader.

A `.sloppak` is an open, hand-editable song package. It exists in two
interchangeable forms:

1. **Zip archive** — a `.sloppak` file containing a `manifest.yaml`,
   arrangement JSONs, stem OGGs, optional cover/lyrics. Distribution form.
2. **Directory** — a directory whose name ends in `.sloppak/` containing the
   same files. Authoring form.

See the format spec in the project's sloppak plan for the full layout.
"""

from __future__ import annotations

import json
import shutil
import threading
import zipfile
from dataclasses import dataclass
from pathlib import Path

import yaml

from song import (
    Song,
    Beat,
    Section,
    arrangement_from_wire,
)


# ── Format detection ──────────────────────────────────────────────────────────

def is_sloppak(path: Path) -> bool:
    """True if path looks like a sloppak (zip file or directory)."""
    return path.name.lower().endswith(".sloppak")


# ── Source resolution (zip unpack cache + directory passthrough) ──────────────

# Maps sloppak filename (relative to DLC_DIR) → (source_dir, mtime, size).
# For directory-form sloppaks, source_dir is the original path and we only
# track it so serving can locate it by filename.
# For zipped sloppaks, source_dir is a cache dir under the unpack root.
_source_cache: dict[str, tuple[Path, float, int]] = {}
_source_lock = threading.Lock()


def _unpack_zip(zip_path: Path, dest: Path) -> None:
    """Extract a sloppak zip archive into dest, replacing any previous contents."""
    if dest.exists():
        shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(str(zip_path), "r") as zf:
        zf.extractall(str(dest))


def _safe_id(filename: str) -> str:
    """Turn a filename into a filesystem-safe cache key (no path separators)."""
    return filename.replace("/", "__").replace("\\", "__").replace(" ", "_")


def resolve_source_dir(
    filename: str,
    dlc_root: Path,
    unpack_cache_root: Path,
) -> Path:
    """Return the on-disk directory containing a sloppak's files.

    - Directory-form: returns the sloppak dir itself (no copy).
    - Zip-form:       unpacks to ``unpack_cache_root/{id}/`` on first use,
                      re-unpacks if mtime/size changed, then returns that dir.

    Caches the resolution so subsequent calls are ~free.
    """
    path = dlc_root / filename
    stat = path.stat()
    mtime, size = stat.st_mtime, stat.st_size

    with _source_lock:
        cached = _source_cache.get(filename)
        if cached:
            cached_dir, cached_mtime, cached_size = cached
            if (
                cached_mtime == mtime
                and cached_size == size
                and cached_dir.exists()
            ):
                return cached_dir

    if path.is_dir():
        resolved = path
    else:
        # Zip form — unpack to the cache.
        dest = unpack_cache_root / _safe_id(filename)
        _unpack_zip(path, dest)
        resolved = dest

    with _source_lock:
        _source_cache[filename] = (resolved, mtime, size)
    return resolved


def get_cached_source_dir(filename: str) -> Path | None:
    """Return the cached source dir for a sloppak if one is known."""
    with _source_lock:
        cached = _source_cache.get(filename)
        return cached[0] if cached else None


# ── Manifest + song loading ───────────────────────────────────────────────────

def _read_manifest(source_dir: Path) -> dict:
    mf = source_dir / "manifest.yaml"
    if not mf.exists():
        mf = source_dir / "manifest.yml"
    if not mf.exists():
        raise FileNotFoundError(f"manifest.yaml not found in {source_dir}")
    with mf.open("r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh)
    if not isinstance(data, dict):
        raise ValueError("manifest.yaml must contain a mapping at the top level")
    return data


def _read_manifest_from_zip(zip_path: Path) -> dict:
    """Read just manifest.yaml from a zipped sloppak without unpacking stems."""
    with zipfile.ZipFile(str(zip_path), "r") as zf:
        for name in ("manifest.yaml", "manifest.yml"):
            try:
                with zf.open(name) as fh:
                    data = yaml.safe_load(fh.read().decode("utf-8"))
                    if isinstance(data, dict):
                        return data
            except KeyError:
                continue
    raise FileNotFoundError(f"manifest.yaml not found in zip {zip_path}")


def load_manifest(path: Path) -> dict:
    """Return the parsed manifest dict for a sloppak (dir or zip)."""
    if path.is_dir():
        return _read_manifest(path)
    return _read_manifest_from_zip(path)


@dataclass
class LoadedSloppak:
    """Result of loading a sloppak: the Song object plus stem descriptors."""
    song: Song
    stems: list[dict]           # [{"id": str, "file": str, "default": bool}]
    source_dir: Path
    manifest: dict


def load_song(
    filename: str,
    dlc_root: Path,
    unpack_cache_root: Path,
) -> LoadedSloppak:
    """Fully load a sloppak: resolve its source dir, parse manifest + all
    arrangements + optional lyrics, and return a ready-to-stream Song."""
    source_dir = resolve_source_dir(filename, dlc_root, unpack_cache_root)
    manifest = _read_manifest(source_dir)

    song = Song(
        title=str(manifest.get("title", "")),
        artist=str(manifest.get("artist", "")),
        album=str(manifest.get("album", "")),
        year=int(manifest.get("year", 0) or 0),
        song_length=float(manifest.get("duration", 0.0) or 0.0),
    )

    # Load each arrangement from its JSON file.
    for entry in manifest.get("arrangements", []) or []:
        rel = entry.get("file")
        if not rel:
            continue
        arr_path = source_dir / rel
        if not arr_path.exists():
            continue
        try:
            data = json.loads(arr_path.read_text(encoding="utf-8"))
        except Exception:
            continue
        arr = arrangement_from_wire(data)
        # Manifest-level overrides take precedence over anything embedded in
        # the arrangement JSON (name, tuning, capo).
        if entry.get("name"):
            arr.name = str(entry["name"])
        if "tuning" in entry:
            arr.tuning = list(entry["tuning"])
        if "capo" in entry:
            arr.capo = int(entry["capo"])

        # Beats/sections can live on the arrangement itself in the wire format.
        # If the manifest-level arrangement JSON carries them, pull them onto
        # the song object the first time we see them.
        if not song.beats:
            for b in data.get("beats", []) or []:
                song.beats.append(
                    Beat(time=float(b.get("time", 0)), measure=int(b.get("measure", -1)))
                )
        if not song.sections:
            for s in data.get("sections", []) or []:
                song.sections.append(
                    Section(
                        name=str(s.get("name", "")),
                        number=int(s.get("number", 0)),
                        start_time=float(s.get("time", s.get("start_time", 0))),
                    )
                )
        song.arrangements.append(arr)

    # Optional shared lyrics file.
    lyrics_rel = manifest.get("lyrics")
    if lyrics_rel:
        lyr_path = source_dir / str(lyrics_rel)
        if lyr_path.exists():
            try:
                song.lyrics = json.loads(lyr_path.read_text(encoding="utf-8"))
            except Exception:
                pass

    # Stem descriptors — normalized for callers. File paths are resolved but
    # returned as ``file`` relative strings so URL construction stays caller-side.
    stems: list[dict] = []
    for s in manifest.get("stems", []) or []:
        if not isinstance(s, dict):
            continue
        sid = str(s.get("id", ""))
        sfile = str(s.get("file", ""))
        if not sid or not sfile:
            continue
        default_val = s.get("default", True)
        if isinstance(default_val, str):
            default_on = default_val.lower() not in ("off", "false", "0", "no")
        else:
            default_on = bool(default_val)
        stems.append({"id": sid, "file": sfile, "default": default_on})

    return LoadedSloppak(song=song, stems=stems, source_dir=source_dir, manifest=manifest)


# ── Fast metadata extractor (scanner path) ────────────────────────────────────

def _tuning_for_meta(arrangements_manifest: list[dict]) -> list[int]:
    """Best-effort guitar-first tuning for the library index."""
    for entry in arrangements_manifest:
        name = str(entry.get("name", "")).lower()
        tun = entry.get("tuning")
        if tun and isinstance(tun, list) and name in ("lead", "rhythm", "combo"):
            return list(tun)
    # Fallback: first arrangement with a tuning
    for entry in arrangements_manifest:
        tun = entry.get("tuning")
        if tun and isinstance(tun, list):
            return list(tun)
    return [0] * 6


def extract_meta(path: Path) -> dict:
    """Fast metadata for the library scanner. Reads only the manifest."""
    manifest = load_manifest(path)
    arr_list = manifest.get("arrangements", []) or []

    arrangements = []
    for i, entry in enumerate(arr_list):
        arrangements.append(
            {
                "index": i,
                "name": str(entry.get("name", entry.get("id", f"Arr{i}"))),
                "notes": 0,  # unknown without loading; fine for the index
            }
        )
    # Sort like PSARC path: Lead > Combo > Rhythm > Bass
    priority = {"Lead": 0, "Combo": 1, "Rhythm": 2, "Bass": 3}
    arrangements.sort(key=lambda a: priority.get(a["name"], 99))
    for i, a in enumerate(arrangements):
        a["index"] = i

    has_lyrics = bool(manifest.get("lyrics"))
    tuning_offsets = _tuning_for_meta(arr_list)

    stems_list = manifest.get("stems", []) or []
    stem_count = sum(1 for s in stems_list if isinstance(s, dict) and s.get("id"))

    return {
        "title": str(manifest.get("title", "")),
        "artist": str(manifest.get("artist", "")),
        "album": str(manifest.get("album", "")),
        "year": str(manifest.get("year", "") or ""),
        "duration": float(manifest.get("duration", 0) or 0),
        "tuning_offsets": tuning_offsets,  # caller maps to a name via tunings.tuning_name
        "arrangements": arrangements,
        "has_lyrics": has_lyrics,
        "stem_count": stem_count,
    }
