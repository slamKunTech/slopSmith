"""Rocksmith Web — FastAPI backend serving highway viewer + library."""

import asyncio
import json
import os
import sys
import tempfile
import shutil
from pathlib import Path

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, UploadFile, File
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse

from psarc import unpack_psarc, read_psarc_entries
from song import load_song, phrase_to_wire, arrangement_string_count
from audio import find_wem_files, convert_wem
from tunings import tuning_name
import sloppak as sloppak_mod

import concurrent.futures
import sqlite3
import threading
import xml.etree.ElementTree as ET

app = FastAPI(title="Rocksmith Web")

STATIC_DIR = Path(__file__).parent / "static"
try:
    STATIC_DIR.mkdir(exist_ok=True)
except OSError:
    pass  # Read-only in packaged installs

# Distinguish "env not set / empty" from "explicitly set". Path("") collapses
# to Path(".") so we can't recover that signal after the cast — capture the
# raw env-var string up front and let _get_dlc_dir() consult both. This way
# `DLC_DIR=.` remains a valid opt-in for cwd while `DLC_DIR=""` (or unset)
# falls through to the config.json fallback.
_DLC_DIR_ENV = os.environ.get("DLC_DIR", "").strip()
DLC_DIR = Path(_DLC_DIR_ENV) if _DLC_DIR_ENV else Path("")
CONFIG_DIR = Path(os.environ.get("CONFIG_DIR", str(Path.home() / ".local" / "share" / "rocksmith-cdlc")))

# Writable cache directories (use CONFIG_DIR, not STATIC_DIR which may be read-only)
ART_CACHE_DIR = CONFIG_DIR / "art_cache"
AUDIO_CACHE_DIR = CONFIG_DIR / "audio_cache"
SLOPPAK_CACHE_DIR = CONFIG_DIR / "sloppak_cache"


# ── SQLite metadata cache ─────────────────────────────────────────────────────

class MetadataDB:
    def __init__(self):
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        self.db_path = str(CONFIG_DIR / "web_library.db")
        self.conn = sqlite3.connect(self.db_path, check_same_thread=False)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("""
            CREATE TABLE IF NOT EXISTS songs (
                filename TEXT PRIMARY KEY,
                mtime REAL,
                size INTEGER,
                title TEXT,
                artist TEXT,
                album TEXT,
                year TEXT,
                duration REAL,
                tuning TEXT,
                arrangements TEXT,
                has_lyrics INTEGER DEFAULT 0,
                format TEXT DEFAULT 'psarc',
                stem_count INTEGER DEFAULT 0
            )
        """)
        # Idempotent migration for installs that predate the format column.
        try:
            self.conn.execute("ALTER TABLE songs ADD COLUMN format TEXT DEFAULT 'psarc'")
        except sqlite3.OperationalError:
            pass
        try:
            self.conn.execute("ALTER TABLE songs ADD COLUMN stem_count INTEGER DEFAULT 0")
        except sqlite3.OperationalError:
            pass
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_songs_artist ON songs(artist COLLATE NOCASE)")
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_songs_title ON songs(title COLLATE NOCASE)")
        self.conn.execute("CREATE TABLE IF NOT EXISTS favorites (filename TEXT PRIMARY KEY)")
        self.conn.execute("""
            CREATE TABLE IF NOT EXISTS loops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                filename TEXT NOT NULL,
                name TEXT NOT NULL,
                start_time REAL NOT NULL,
                end_time REAL NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            )
        """)
        self.conn.commit()
        self._lock = threading.Lock()

    def is_favorite(self, filename: str) -> bool:
        return self.conn.execute("SELECT 1 FROM favorites WHERE filename = ?", (filename,)).fetchone() is not None

    def toggle_favorite(self, filename: str) -> bool:
        """Toggle favorite status. Returns new state."""
        with self._lock:
            if self.is_favorite(filename):
                self.conn.execute("DELETE FROM favorites WHERE filename = ?", (filename,))
                self.conn.commit()
                return False
            else:
                self.conn.execute("INSERT OR IGNORE INTO favorites VALUES (?)", (filename,))
                self.conn.commit()
                return True

    def favorite_set(self) -> set[str]:
        return {r[0] for r in self.conn.execute("SELECT filename FROM favorites").fetchall()}

    def get(self, filename: str, mtime: float, size: int) -> dict | None:
        row = self.conn.execute(
            "SELECT mtime, size, title, artist, album, year, duration, tuning, arrangements, has_lyrics, format, stem_count "
            "FROM songs WHERE filename = ?", (filename,)
        ).fetchone()
        if row and row[0] == mtime and row[1] == size and row[2]:
            return {
                "title": row[2], "artist": row[3], "album": row[4],
                "year": row[5], "duration": row[6], "tuning": row[7],
                "arrangements": json.loads(row[8]) if row[8] else [],
                "has_lyrics": bool(row[9]),
                "format": row[10] or "psarc",
                "stem_count": int(row[11] or 0),
            }
        return None

    def put(self, filename: str, mtime: float, size: int, meta: dict):
        with self._lock:
            self.conn.execute(
                "INSERT OR REPLACE INTO songs "
                "(filename, mtime, size, title, artist, album, year, duration, tuning, arrangements, has_lyrics, format, stem_count) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (filename, mtime, size, meta.get("title", ""), meta.get("artist", ""),
                 meta.get("album", ""), meta.get("year", ""), meta.get("duration", 0),
                 meta.get("tuning", ""), json.dumps(meta.get("arrangements", [])),
                 1 if meta.get("has_lyrics") else 0,
                 meta.get("format", "psarc"),
                 int(meta.get("stem_count", 0) or 0)),
            )
            self.conn.commit()

    def count(self) -> int:
        return self.conn.execute("SELECT COUNT(*) FROM songs WHERE title != ''").fetchone()[0]

    def delete_missing(self, current_filenames: set[str]):
        """Remove DB entries for files no longer on disk."""
        with self._lock:
            db_files = {r[0] for r in self.conn.execute("SELECT filename FROM songs").fetchall()}
            stale = db_files - current_filenames
            if stale:
                self.conn.executemany("DELETE FROM songs WHERE filename = ?", [(f,) for f in stale])
                self.conn.commit()
            return len(stale)

    def _estd_set(self) -> set[str]:
        """Get set of filenames that have a retuned variant (_EStd_ or _DropD_) in the DB."""
        rows = self.conn.execute(
            "SELECT filename FROM songs WHERE filename LIKE '%\\_EStd\\_%' ESCAPE '\\' "
            "OR filename LIKE '%\\_DropD\\_%' ESCAPE '\\'"
        ).fetchall()
        originals = set()
        for (fname,) in rows:
            originals.add(fname.replace("_EStd_", "_").replace("_DropD_", "_"))
        return originals

    def query_page(self, q: str = "", page: int = 0, size: int = 24,
                   sort: str = "artist", direction: str = "asc",
                   favorites_only: bool = False,
                   format_filter: str = "") -> tuple[list[dict], int]:
        """Server-side paginated search. Returns (songs, total_count)."""
        where = "WHERE title != ''"
        params = []
        if favorites_only:
            where += " AND filename IN (SELECT filename FROM favorites)"
        if format_filter:
            where += " AND format = ?"
            params.append(format_filter)
        if q:
            where += " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE OR album LIKE ? COLLATE NOCASE)"
            params += [f"%{q}%"] * 3

        sort_map = {
            "artist": "artist COLLATE NOCASE", "artist-desc": "artist COLLATE NOCASE DESC",
            "title": "title COLLATE NOCASE", "title-desc": "title COLLATE NOCASE DESC",
            "recent": "mtime DESC", "tuning": "tuning COLLATE NOCASE",
        }
        order = sort_map.get(sort, "artist COLLATE NOCASE")
        if direction == "desc" and "DESC" not in order:
            order += " DESC"

        total = self.conn.execute(f"SELECT COUNT(*) FROM songs {where}", params).fetchone()[0]
        rows = self.conn.execute(
            f"SELECT filename, title, artist, album, year, duration, tuning, arrangements, has_lyrics, mtime, format, stem_count "
            f"FROM songs {where} ORDER BY {order} LIMIT ? OFFSET ?",
            params + [size, page * size]
        ).fetchall()

        estd = self._estd_set()
        favs = self.favorite_set()
        songs = []
        for r in rows:
            songs.append({
                "filename": r[0], "title": r[1], "artist": r[2], "album": r[3],
                "year": r[4], "duration": r[5], "tuning": r[6],
                "arrangements": json.loads(r[7]) if r[7] else [],
                "has_lyrics": bool(r[8]), "mtime": r[9],
                "format": r[10] or "psarc",
                "stem_count": int(r[11] or 0),
                "has_estd": r[0] in estd, "favorite": r[0] in favs,
            })
        return songs, total

    def query_artists(self, letter: str = "", q: str = "",
                      favorites_only: bool = False,
                      page: int = 0, size: int = 50,
                      format_filter: str = "") -> tuple[list[dict], int]:
        """Get artists grouped by letter with their albums and songs. Returns (artists, total_artists)."""
        where = "WHERE title != ''"
        params = []
        if favorites_only:
            where += " AND filename IN (SELECT filename FROM favorites)"
        if format_filter:
            where += " AND format = ?"
            params.append(format_filter)
        if letter == "#":
            where += " AND artist NOT GLOB '[A-Za-z]*'"
        elif letter:
            where += " AND UPPER(SUBSTR(artist, 1, 1)) = ?"
            params.append(letter.upper())
        if q:
            where += " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE OR album LIKE ? COLLATE NOCASE)"
            params += [f"%{q}%"] * 3

        # Get paginated distinct artists
        total_artists = self.conn.execute(
            f"SELECT COUNT(DISTINCT artist COLLATE NOCASE) FROM songs {where}", params
        ).fetchone()[0]

        artist_rows = self.conn.execute(
            f"SELECT DISTINCT artist COLLATE NOCASE as a FROM songs {where} ORDER BY a LIMIT ? OFFSET ?",
            params + [size, page * size]
        ).fetchall()
        artist_names = [r[0] for r in artist_rows]

        if not artist_names:
            return [], total_artists

        # Fetch songs for these artists only
        placeholders = ",".join(["?"] * len(artist_names))
        song_where = f"{where} AND artist COLLATE NOCASE IN ({placeholders})"
        song_params = params + artist_names

        rows = self.conn.execute(
            f"SELECT filename, title, artist, album, year, duration, tuning, arrangements, has_lyrics, format, stem_count "
            f"FROM songs {song_where} ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, title COLLATE NOCASE",
            song_params
        ).fetchall()

        # Group into artist -> album -> songs
        from collections import OrderedDict
        estd = self._estd_set()
        favs = self.favorite_set()
        artists = OrderedDict()
        for r in rows:
            artist = r[2] or "Unknown Artist"
            album = r[3] or "Unknown Album"
            akey = artist.lower()
            if akey not in artists:
                artists[akey] = {"name": artist, "albums": OrderedDict()}
            bkey = album.lower()
            if bkey not in artists[akey]["albums"]:
                artists[akey]["albums"][bkey] = {"name": album, "songs": []}
            artists[akey]["albums"][bkey]["songs"].append({
                "filename": r[0], "title": r[1], "artist": r[2], "album": r[3],
                "year": r[4], "duration": r[5], "tuning": r[6],
                "arrangements": json.loads(r[7]) if r[7] else [],
                "has_lyrics": bool(r[8]),
                "format": r[9] or "psarc",
                "stem_count": int(r[10] or 0),
                "has_estd": r[0] in estd,
                "favorite": r[0] in favs,
            })

        # Pick most common name variant per artist/album
        result = []
        for akey, aval in artists.items():
            albums = []
            for bkey, bval in aval["albums"].items():
                albums.append({"name": bval["name"], "songs": bval["songs"]})
            result.append({"name": aval["name"], "album_count": len(albums),
                           "song_count": sum(len(a["songs"]) for a in albums), "albums": albums})
        return result, total_artists

    def query_stats(self, favorites_only: bool = False) -> dict:
        """Aggregate stats for the letter bar."""
        filt = " AND filename IN (SELECT filename FROM favorites)" if favorites_only else ""
        total = self.conn.execute(f"SELECT COUNT(*) FROM songs WHERE title != ''{filt}").fetchone()[0]
        artist_count = self.conn.execute(f"SELECT COUNT(DISTINCT artist) FROM songs WHERE title != ''{filt}").fetchone()[0]
        rows = self.conn.execute(
            f"SELECT UPPER(SUBSTR(artist, 1, 1)) as letter, COUNT(DISTINCT artist COLLATE NOCASE) "
            f"FROM songs WHERE title != ''{filt} GROUP BY letter"
        ).fetchall()
        letters = {}
        for letter, count in rows:
            if letter and letter.isalpha():
                letters[letter] = count
            else:
                letters["#"] = letters.get("#", 0) + count
        return {"total_songs": total, "total_artists": artist_count, "letters": letters}


meta_db = MetadataDB()


def _get_dlc_dir() -> Path | None:
    # Only consider DLC_DIR if the env var was non-empty. `Path("")` collapses
    # to `.` and reports `.is_dir() == True`, which would silently shadow the
    # config.json fallback. Checking the raw env string preserves
    # `DLC_DIR=.` as a valid opt-in for cwd while keeping unset/empty out.
    if _DLC_DIR_ENV and DLC_DIR.is_dir():
        return DLC_DIR
    config_file = CONFIG_DIR / "config.json"
    if config_file.exists():
        try:
            cfg = json.loads(config_file.read_text())
            raw = str(cfg.get("dlc_dir", "")).strip()
            if raw:
                p = Path(raw)
                if p.is_dir():
                    return p
        except Exception:
            pass
    return None


# ── Background metadata scan ──────────────────────────────────────────────────

def _extract_meta_fast(psarc_path: Path) -> dict:
    """Extract metadata from a PSARC using in-memory reading (no disk I/O)."""
    files = read_psarc_entries(str(psarc_path), ["*.json", "*.xml", "*vocals*.sng"])

    title = artist = album = year = ""
    duration = 0.0
    tuning = "E Standard"
    _tuning_from_guitar = False
    arrangements = []
    has_lyrics = False
    arr_index = 0

    # Parse manifest JSONs for metadata + arrangement info
    for path, data in sorted(files.items()):
        if not path.lower().endswith(".json"):
            continue
        try:
            jdata = json.loads(data)
            entries = jdata.get("Entries") or {}
            for k, v in entries.items():
                attrs = v.get("Attributes") or {}
                arr_name = attrs.get("ArrangementName", "")
                if arr_name in ("Vocals", "ShowLights", "JVocals"):
                    continue
                if not title:
                    title = attrs.get("SongName", "")
                    artist = attrs.get("ArtistName", "")
                    album = attrs.get("AlbumName", "")
                    yr = attrs.get("SongYear")
                    year = str(yr) if yr else ""
                    sl = attrs.get("SongLength")
                    if sl:
                        try: duration = float(sl)
                        except (ValueError, TypeError): pass
                if arr_name:
                    # Get tuning - prefer guitar arrangements over bass
                    tun = attrs.get("Tuning")
                    if tun and isinstance(tun, dict):
                        offsets = [tun.get(f"string{i}", 0) for i in range(6)]
                        tun_name = tuning_name(offsets)
                        is_guitar = arr_name in ("Lead", "Rhythm", "Combo")
                        if tuning == "E Standard" or (is_guitar and not _tuning_from_guitar):
                            tuning = tun_name
                            if is_guitar:
                                _tuning_from_guitar = True
                    notes = attrs.get("NotesHard", 0) or attrs.get("NotesMedium", 0) or 0
                    arrangements.append({"index": arr_index, "name": arr_name, "notes": notes})
                    arr_index += 1
        except Exception:
            continue

    # Check XMLs for vocals (CDLC), or fall back to vocals SNG (official DLC)
    for path, data in files.items():
        if path.lower().endswith(".xml"):
            try:
                root = ET.fromstring(data)
                if root.tag == "vocals":
                    has_lyrics = True
                    break
            except Exception:
                continue
        elif path.lower().endswith(".sng") and "vocals" in path.lower():
            has_lyrics = True
            break

    # Sort arrangements: Lead > Combo > Rhythm > Bass
    priority = {"Lead": 0, "Combo": 1, "Rhythm": 2, "Bass": 3}
    arrangements.sort(key=lambda a: priority.get(a["name"], 99))
    for i, a in enumerate(arrangements):
        a["index"] = i

    return {
        "title": title, "artist": artist, "album": album, "year": year,
        "duration": duration, "tuning": tuning,
        "arrangements": arrangements, "has_lyrics": has_lyrics,
    }


def _extract_meta_sloppak(path: Path) -> dict:
    """Extract metadata for a sloppak (file or directory)."""
    meta = sloppak_mod.extract_meta(path)
    offsets = meta.pop("tuning_offsets", None) or [0] * 6
    meta["tuning"] = tuning_name(offsets)
    meta["format"] = "sloppak"
    return meta


def _extract_meta_for_file(psarc_path: Path) -> dict:
    """Extract metadata — dispatches on extension; PSARC path tries fast then falls back."""
    if sloppak_mod.is_sloppak(psarc_path):
        return _extract_meta_sloppak(psarc_path)
    try:
        meta = _extract_meta_fast(psarc_path)
        if meta["title"]:
            return meta
    except Exception:
        pass
    # Fallback: full extraction (handles SNG-only official DLC etc.)
    tmp = tempfile.mkdtemp(prefix="rs_scan_")
    try:
        unpack_psarc(str(psarc_path), tmp)
        song = load_song(tmp)
        tuning = "E Standard"
        if song.arrangements and song.arrangements[0].tuning:
            tuning = tuning_name(song.arrangements[0].tuning)
        arrangements = [
            {"index": i, "name": a.name,
             "notes": len(a.notes) + sum(len(c.notes) for c in a.chords)}
            for i, a in enumerate(song.arrangements)
        ]
        has_lyrics = False
        for xf in Path(tmp).rglob("*.xml"):
            try:
                if ET.parse(str(xf)).getroot().tag == "vocals":
                    has_lyrics = True
                    break
            except Exception:
                pass
        return {
            "title": song.title, "artist": song.artist,
            "album": song.album, "year": str(song.year) if song.year else "",
            "duration": song.song_length, "tuning": tuning,
            "arrangements": arrangements, "has_lyrics": has_lyrics,
        }
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


_SCAN_STATUS_INIT = {"running": False, "stage": "idle", "total": 0, "done": 0, "current": "", "error": None}
_scan_status = dict(_SCAN_STATUS_INIT)


def _background_scan():
    """Scan all PSARCs and cache metadata on startup. Uses thread pool for parallelism."""
    global _scan_status
    _scan_status = {**_SCAN_STATUS_INIT, "running": True, "stage": "listing"}

    dlc = _get_dlc_dir()
    if not dlc:
        _scan_status = {**_SCAN_STATUS_INIT, "stage": "idle", "error": "DLC folder not configured"}
        print("Scan: no DLC folder configured", flush=True)
        return

    # Listing can fail on macOS without Full Disk Access, or on Docker if the
    # path isn't shared. Report the failure explicitly rather than silently
    # appearing to scan nothing.
    try:
        # Skip RS1 compatibility mega-PSARCs (multi-song, not individually playable)
        psarcs = [f for f in sorted(dlc.rglob("*.psarc"))
                  if f.is_file()
                  and "rs1compatibility" not in f.name.lower()]
        # Sloppaks: match both file (zip) and directory form by suffix.
        sloppaks = [f for f in sorted(dlc.rglob("*.sloppak"))
                    if sloppak_mod.is_sloppak(f)]
    except PermissionError as e:
        msg = (f"Permission denied reading {dlc}. "
               "On macOS: grant Full Disk Access to the app in System Settings → Privacy & Security. "
               "With Docker: share this path in Docker Desktop → Settings → Resources → File Sharing.")
        print(f"Scan failed: {msg} ({e})", flush=True)
        _scan_status = {**_SCAN_STATUS_INIT, "stage": "error", "error": msg}
        return
    except OSError as e:
        print(f"Scan failed listing {dlc}: {e}", flush=True)
        _scan_status = {**_SCAN_STATUS_INIT, "stage": "error", "error": f"Unable to list {dlc}: {e}"}
        return

    all_songs = psarcs + sloppaks
    print(f"Scan: listed {len(psarcs)} PSARCs and {len(sloppaks)} sloppaks in {dlc}", flush=True)

    def _rel(f: Path) -> str:
        # Store the path relative to the DLC root so sub-folders (e.g.
        # dlc/sloppak/foo.sloppak produced by the converter) resolve back
        # correctly later. PSARCs always live directly in dlc/, so this
        # reduces to f.name for them.
        try:
            return f.relative_to(dlc).as_posix()
        except ValueError:
            return f.name

    current_files = {_rel(f) for f in all_songs}

    # Clean up stale DB entries
    stale = meta_db.delete_missing(current_files)
    if stale:
        print(f"Removed {stale} stale DB entries", flush=True)

    # Figure out which need scanning
    to_scan = []
    for f in all_songs:
        stat = f.stat()
        if not meta_db.get(_rel(f), stat.st_mtime, stat.st_size):
            to_scan.append((f, stat))

    if not to_scan:
        _scan_status = {**_SCAN_STATUS_INIT, "stage": "complete"}
        print(f"Scan: nothing new to scan ({len(all_songs)} songs, all cached)", flush=True)
        return

    _scan_status = {**_SCAN_STATUS_INIT, "running": True, "stage": "scanning", "total": len(to_scan)}
    print(f"Library: {len(psarcs)} PSARCs + {len(sloppaks)} sloppaks, {len(all_songs) - len(to_scan)} cached, {len(to_scan)} to scan", flush=True)

    def _scan_one(item):
        f, stat = item
        # Per-file log so users running the server / desktop can see live
        # activity and distinguish a stuck scan from a slow one.
        print(f"  scanning {f.name}", flush=True)
        meta = _extract_meta_for_file(f)
        return _rel(f), stat.st_mtime, stat.st_size, meta

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        futures = {executor.submit(_scan_one, item): item[0].name for item in to_scan}
        for future in concurrent.futures.as_completed(futures):
            fname = futures[future]
            try:
                name, mtime, size, meta = future.result()
                meta_db.put(name, mtime, size, meta)
            except Exception as e:
                print(f"  Failed: {fname}: {e}", flush=True)
            _scan_status["done"] += 1
            _scan_status["current"] = fname

    print(f"Scan complete: {len(to_scan)} songs cached", flush=True)
    _scan_status = {**_SCAN_STATUS_INIT, "stage": "complete"}


# ── Register plugin API endpoints (lightweight, before app starts) ───────────
from plugins import load_plugins, register_plugin_api
register_plugin_api(app)

# Plugin loading deferred to startup event (see below) to avoid blocking
# server startup when many plugins are installed.


@app.on_event("startup")
def startup_events():
    # Load plugins in background after server starts
    load_plugins(app, {
        "config_dir": CONFIG_DIR,
        "get_dlc_dir": _get_dlc_dir,
        "extract_meta": _extract_meta_for_file,
        "meta_db": meta_db,
        "get_sloppak_cache_dir": lambda: SLOPPAK_CACHE_DIR,
    })
    # Start background metadata scan
    startup_scan()


def startup_scan():
    """Start background metadata scan and periodic rescan on server start."""
    thread = threading.Thread(target=_background_scan, daemon=True)
    thread.start()
    # Periodic rescan every 5 minutes
    rescan_thread = threading.Thread(target=_periodic_rescan, daemon=True)
    rescan_thread.start()


def _periodic_rescan():
    """Check for new files every 5 minutes."""
    import time
    time.sleep(300)  # Wait 5 minutes after startup
    while True:
        if not _scan_status["running"]:
            _background_scan()
        time.sleep(300)


@app.get("/api/version")
def get_version():
    env_version = os.environ.get("APP_VERSION", "").strip()
    if env_version:
        return {"version": env_version}
    version_file = Path(__file__).parent / "VERSION"
    version = "unknown"
    if version_file.exists():
        try:
            version = version_file.read_text().strip()
        except (OSError, UnicodeDecodeError):
            pass
    return {"version": version}


@app.get("/api/scan-status")
def scan_status():
    return _scan_status


@app.post("/api/rescan")
def trigger_rescan():
    """Manually trigger a library rescan."""
    if _scan_status["running"]:
        return {"message": "Scan already in progress"}
    thread = threading.Thread(target=_background_scan, daemon=True)
    thread.start()
    return {"message": "Rescan started"}


@app.post("/api/rescan/full")
def trigger_full_rescan():
    """Clear cache and rescan everything."""
    if _scan_status["running"]:
        return {"message": "Scan already in progress"}
    with meta_db._lock:
        meta_db.conn.execute("DELETE FROM songs")
        meta_db.conn.commit()
    thread = threading.Thread(target=_background_scan, daemon=True)
    thread.start()
    return {"message": "Full rescan started"}


# ── Library API ───────────────────────────────────────────────────────────────

@app.get("/api/library")
def list_library(q: str = "", page: int = 0, size: int = 24, sort: str = "artist",
                 dir: str = "asc", favorites: int = 0, format: str = ""):
    """Paginated library search, queried from SQLite."""
    size = min(size, 100)
    fmt = format if format in ("psarc", "sloppak") else ""
    songs, total = meta_db.query_page(q=q, page=page, size=size, sort=sort,
                                       direction=dir, favorites_only=bool(favorites),
                                       format_filter=fmt)
    return {"songs": songs, "total": total, "page": page, "size": size}


@app.get("/api/library/artists")
def list_artists(letter: str = "", q: str = "", favorites: int = 0, page: int = 0, size: int = 50,
                 format: str = ""):
    """Get artists grouped by letter with albums and songs (for tree view)."""
    fmt = format if format in ("psarc", "sloppak") else ""
    artists, total = meta_db.query_artists(letter=letter, q=q, favorites_only=bool(favorites),
                                           page=page, size=min(size, 100), format_filter=fmt)
    return {"artists": artists, "total_artists": total, "page": page, "size": size}


@app.get("/api/library/stats")
def library_stats(favorites: int = 0):
    """Aggregate stats for the UI."""
    return meta_db.query_stats(favorites_only=bool(favorites))


@app.post("/api/favorites/toggle")
def toggle_favorite(data: dict):
    """Toggle a song's favorite status."""
    filename = data.get("filename", "")
    if not filename:
        return {"error": "No filename"}
    new_state = meta_db.toggle_favorite(filename)
    return {"favorite": new_state}


# ── Loops API ────────────────────────────────────────────────────────────────

@app.get("/api/loops")
def list_loops(filename: str):
    rows = meta_db.conn.execute(
        "SELECT id, name, start_time, end_time FROM loops WHERE filename = ? ORDER BY start_time",
        (filename,)
    ).fetchall()
    return [{"id": r[0], "name": r[1], "start": r[2], "end": r[3]} for r in rows]


@app.post("/api/loops")
def save_loop(data: dict):
    filename = data.get("filename", "")
    name = data.get("name", "").strip()
    start = data.get("start")
    end = data.get("end")
    if not filename or start is None or end is None:
        return {"error": "Missing fields"}
    if not name:
        count = meta_db.conn.execute(
            "SELECT COUNT(*) FROM loops WHERE filename = ?", (filename,)
        ).fetchone()[0]
        name = f"Loop {count + 1}"
    with meta_db._lock:
        meta_db.conn.execute(
            "INSERT INTO loops (filename, name, start_time, end_time) VALUES (?, ?, ?, ?)",
            (filename, name, float(start), float(end))
        )
        meta_db.conn.commit()
    return {"ok": True, "name": name}


@app.delete("/api/loops/{loop_id}")
def delete_loop(loop_id: int):
    with meta_db._lock:
        meta_db.conn.execute("DELETE FROM loops WHERE id = ?", (loop_id,))
        meta_db.conn.commit()
    return {"ok": True}


# ── Settings API ──────────────────────────────────────────────────────────────

def _default_settings():
    """Fallback settings returned when config.json is missing or
    unreadable. Also used to seed a fresh cfg on first-run POSTs so a
    single-key write (e.g. the difficulty slider) can't silently wipe
    defaults that subsequent GETs would have exposed."""
    # Same `_DLC_DIR_ENV` truthy check as `_get_dlc_dir`: an empty env
    # var collapses to `Path(".")` whose `.is_dir()` is True, so without
    # the explicit guard we'd surface `"."` to /api/settings — and any
    # partial-update POST would then persist that into config.json,
    # silently undoing the env-var fix on the next load.
    return {"dlc_dir": str(DLC_DIR) if (_DLC_DIR_ENV and DLC_DIR.is_dir()) else ""}


def _load_config(config_file):
    """Read and parse config.json. Returns the parsed dict, or None if
    the file is missing, unreadable, invalid JSON, or parses to a
    non-dict (e.g. the file contains `[]` or `42`). Callers treat None
    as "fall back to defaults". Shared between GET and POST so both
    handle bad files the same way."""
    if not config_file.exists():
        return None
    try:
        parsed = json.loads(config_file.read_text())
    except Exception:
        return None
    return parsed if isinstance(parsed, dict) else None


@app.get("/api/settings")
def get_settings():
    cfg = _load_config(CONFIG_DIR / "config.json")
    return cfg if cfg is not None else _default_settings()


@app.post("/api/settings")
def save_settings(data: dict):
    # Partial-update: merge only keys present in the request body so
    # single-key POSTs (like the difficulty slider's oninput) don't
    # clobber unrelated settings on disk.
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    config_file = CONFIG_DIR / "config.json"
    # Seed defaults when config.json is missing, unreadable, or parses
    # to a non-dict (e.g. `[]`, `42`). Without the non-dict guard, the
    # next `cfg["..."] = ...` assignment would raise TypeError and 500
    # the public endpoint. Seeding also ensures single-key POSTs (the
    # difficulty slider's fire-and-forget write) don't produce a config
    # file missing the dlc_dir fallback GET would have surfaced.
    cfg = _load_config(config_file)
    if cfg is None:
        cfg = _default_settings()

    messages = []
    if "dlc_dir" in data:
        dlc_path = data["dlc_dir"]
        # null / missing is no-op (preserve on-disk value). Only an
        # explicit empty string means "clear". Non-string values are
        # rejected so Path(...) can't be surprised by non-str JSON.
        if dlc_path is None:
            pass
        elif not isinstance(dlc_path, str):
            return {"error": "dlc_dir must be a string path or empty"}
        elif dlc_path == "":
            cfg["dlc_dir"] = ""
        else:
            if Path(dlc_path).is_dir():
                cfg["dlc_dir"] = dlc_path
                count = sum(1 for f in Path(dlc_path).iterdir() if f.suffix == ".psarc")
                messages.append(f"DLC folder: {count} .psarc files found")
            else:
                return {"error": f"DLC directory not found: {dlc_path}"}

    # Both of these are consumed downstream as strings (e.g.
    # demucs_server_url.rstrip('/') in lib/sloppak_convert.py), so
    # reject non-string shapes here. Matches the dlc_dir pattern above:
    # null is no-op, empty string clears, non-string is a structured
    # error that preserves the on-disk value.
    for key in ("default_arrangement", "demucs_server_url"):
        if key in data:
            raw = data[key]
            if raw is None:
                pass
            elif not isinstance(raw, str):
                return {"error": f"{key} must be a string or empty"}
            else:
                cfg[key] = raw
    if "master_difficulty" in data:
        # Coerce defensively — public endpoint, so `null`, `""`, or a
        # non-numeric string shouldn't 500 the request. float() accepts
        # both integer and float-shaped strings; anything else returns
        # a structured error like the dlc_dir branch above.
        raw = data["master_difficulty"]
        # Reject bool explicitly: Python makes bool a subclass of int, so
        # True/False would otherwise coerce to 1/0 and persist as a valid
        # difficulty. Caller almost certainly means "bad input".
        if isinstance(raw, bool):
            return {"error": "master_difficulty must be a number between 0 and 100"}
        try:
            cfg["master_difficulty"] = max(0, min(100, int(float(raw))))
        except (TypeError, ValueError, OverflowError):
            # OverflowError covers int(float("inf")) / int(float("1e309"))
            # which Python raises distinctly from ValueError.
            return {"error": "master_difficulty must be a number between 0 and 100"}

    if "av_offset_ms" in data:
        # Audio-output pipeline latency compensation. Positive values
        # mean audio is running ahead of visuals; the highway adds
        # this to its render clock to catch the visuals up. Clamped
        # to ±1000 ms to mirror the client-side slider — a direct
        # POST shouldn't be able to persist `1e9`. Same defensive
        # coercion shape as master_difficulty above (reject bool,
        # cover OverflowError, structured 4xx-style return on bad
        # input rather than 500).
        raw = data["av_offset_ms"]
        if isinstance(raw, bool):
            return {"error": "av_offset_ms must be a number between -1000 and 1000"}
        try:
            cfg["av_offset_ms"] = max(-1000.0, min(1000.0, float(raw)))
        except (TypeError, ValueError, OverflowError):
            return {"error": "av_offset_ms must be a number between -1000 and 1000"}

    config_file.write_text(json.dumps(cfg, indent=2))
    return {"message": ". ".join(messages) if messages else "Settings saved"}


# ── Plugin-provided routes are registered at startup via plugins/__init__.py ─
# (CustomsForge, Ultimate Guitar, etc. are loaded from plugins/ directory)



@app.websocket("/ws/retune")
async def ws_retune(websocket: WebSocket, filename: str, target: str = "E Standard"):
    """Retune a song to a target tuning with real-time progress."""
    import asyncio
    await websocket.accept()

    dlc = _get_dlc_dir()
    if not dlc:
        await websocket.send_json({"error": "DLC folder not configured"})
        await websocket.close()
        return

    psarc_path = dlc / filename
    if not psarc_path.exists():
        await websocket.send_json({"error": "File not found"})
        await websocket.close()
        return

    # Retune only operates on PSARC containers — sloppak is an open format
    # and doesn't share the SNG/encryption pipeline retune.py depends on.
    if filename.lower().endswith(".sloppak") or sloppak_mod.is_sloppak(psarc_path):
        await websocket.send_json({"error": "Retune is not supported for .sloppak files"})
        await websocket.close()
        return

    progress_queue = asyncio.Queue()

    def _do_retune():
        from retune import retune_to_standard, get_tuning

        def report(stage, pct):
            progress_queue.put_nowait({"stage": stage, "progress": pct})

        try:
            report("Checking tuning...", 5)
            offsets, uniform = get_tuning(str(psarc_path))

            # Determine target offsets
            if target == "Drop D":
                target_offsets = [-2, 0, 0, 0, 0, 0]
            else:
                target_offsets = [0, 0, 0, 0, 0, 0]

            # Check if already at target
            if offsets == target_offsets:
                progress_queue.put_nowait({"error": f"Already in {target}"})
                return

            # For uniform tunings (all same offset), shift everything to 0
            # For drop tunings, check if the shift is uniform
            shift = [target_offsets[i] - offsets[i] for i in range(6)]
            if len(set(shift)) != 1:
                progress_queue.put_nowait({"error": f"Cannot uniformly retune {offsets} to {target} — shift varies per string"})
                return

            semitones = shift[0]
            report("Extracting PSARC...", 10)

            import builtins
            _orig_print = builtins.print
            def _progress_print(*args, **kwargs):
                msg = " ".join(str(a) for a in args)
                if "Processing" in msg: report(msg, 30)
                elif "Decoded" in msg: report(msg, 45)
                elif "Shifted" in msg: report(msg, 60)
                elif "Updated tuning" in msg: report(msg, 70)
                elif "Recompiling" in msg: report(msg, 80)
                elif "Repacking" in msg: report(msg, 90)
                elif "Created" in msg: report(msg, 95)
                _orig_print(*args, **kwargs)

            builtins.print = _progress_print
            try:
                # Set custom output path based on target
                suffix = "_EStd" if target == "E Standard" else "_DropD"
                p = Path(psarc_path)
                stem = p.stem.replace("_p", "")
                out_path = str(p.parent / f"{stem}{suffix}_p.psarc")
                result = retune_to_standard(str(psarc_path), output_path=out_path)
            finally:
                builtins.print = _orig_print

            # Cache metadata for new file
            new_path = Path(result)
            if new_path.exists():
                try:
                    meta = _extract_meta_for_file(new_path)
                    stat = new_path.stat()
                    meta_db.put(new_path.name, stat.st_mtime, stat.st_size, meta)
                except Exception:
                    pass

            progress_queue.put_nowait({
                "done": True, "progress": 100,
                "stage": "Complete!",
                "filename": new_path.name,
            })

        except Exception as e:
            import traceback
            traceback.print_exc()
            progress_queue.put_nowait({"error": str(e)})

    loop = asyncio.get_event_loop()
    build_task = loop.run_in_executor(None, _do_retune)

    try:
        while True:
            try:
                msg = await asyncio.wait_for(progress_queue.get(), timeout=1.0)
                await websocket.send_json(msg)
                if msg.get("done") or msg.get("error"):
                    break
            except asyncio.TimeoutError:
                if build_task.done():
                    break
    except WebSocketDisconnect:
        pass

    await websocket.close()


@app.get("/api/song/{filename:path}/art")
async def get_song_art(filename: str):
    """Extract and serve album art from a PSARC as PNG."""
    import asyncio
    dlc = _get_dlc_dir()
    if not dlc:
        return JSONResponse({"error": "not configured"}, 404)

    psarc_path = dlc / filename
    if not psarc_path.exists():
        return JSONResponse({"error": "not found"}, 404)

    # Sloppak path: pull cover.jpg from the source dir (manifest-declared or default).
    if sloppak_mod.is_sloppak(psarc_path):
        try:
            src = sloppak_mod.resolve_source_dir(filename, dlc, SLOPPAK_CACHE_DIR)
            manifest = sloppak_mod.load_manifest(psarc_path)
            cover_rel = str(manifest.get("cover") or "cover.jpg")
            cover_path = (src / cover_rel).resolve()
            # Prevent escape and fall back to default name if missing.
            try:
                cover_path.relative_to(src.resolve())
            except ValueError:
                return JSONResponse({"error": "forbidden"}, 403)
            if cover_path.exists() and cover_path.is_file():
                mt = {
                    ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
                    ".png": "image/png", ".webp": "image/webp",
                }.get(cover_path.suffix.lower(), "image/jpeg")
                return FileResponse(str(cover_path), media_type=mt)
        except Exception:
            pass
        return JSONResponse({"error": "no art"}, 404)

    # Check cache first
    art_cache = ART_CACHE_DIR
    art_cache.mkdir(parents=True, exist_ok=True)
    safe_name = filename.replace("/", "_").replace(" ", "_")
    cached = art_cache / f"{safe_name}.png"
    if cached.exists():
        return FileResponse(str(cached), media_type="image/png")

    def _extract_art():
        tmp = tempfile.mkdtemp(prefix="rs_art_")
        try:
            unpack_psarc(str(psarc_path), tmp)
            dds_files = sorted(Path(tmp).rglob("*.dds"), key=lambda p: p.stat().st_size, reverse=True)
            if not dds_files:
                return None
            from PIL import Image
            img = Image.open(dds_files[0]).convert("RGB")
            img.save(str(cached), "PNG")
            return str(cached)
        except Exception:
            return None
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    result = await asyncio.get_event_loop().run_in_executor(None, _extract_art)
    if result:
        return FileResponse(result, media_type="image/png")
    return JSONResponse({"error": "no art"}, 404)


@app.post("/api/song/{filename:path}/meta")
def update_song_meta(filename: str, data: dict):
    """Update song metadata in the cache."""
    with meta_db._lock:
        updates = []
        params = []
        for field in ("title", "artist", "album", "year"):
            if field in data:
                updates.append(f"{field} = ?")
                params.append(data[field])
        if not updates:
            return {"error": "No fields to update"}
        params.append(filename)
        meta_db.conn.execute(
            f"UPDATE songs SET {', '.join(updates)} WHERE filename = ?", params
        )
        meta_db.conn.commit()
    return {"ok": True}


@app.post("/api/song/{filename:path}/art/upload")
async def upload_song_art_b64(filename: str, data: dict):
    """Upload custom album art as base64 PNG/JPG."""
    import base64
    b64 = data.get("image", "")
    if not b64:
        return {"error": "No image data"}
    # Strip data URL prefix if present
    if "," in b64:
        b64 = b64.split(",", 1)[1]
    try:
        img_data = base64.b64decode(b64)
    except Exception:
        return {"error": "Invalid base64"}

    art_cache = ART_CACHE_DIR
    art_cache.mkdir(parents=True, exist_ok=True)
    safe_name = filename.replace("/", "_").replace(" ", "_")
    cached = art_cache / f"{safe_name}.png"

    # Convert to PNG if needed
    try:
        from PIL import Image
        import io
        img = Image.open(io.BytesIO(img_data)).convert("RGB")
        img.save(str(cached), "PNG")
    except Exception as e:
        return {"error": f"Invalid image: {e}"}

    return {"ok": True}


@app.get("/api/song/{filename:path}")
async def get_song_info(filename: str):
    """Return song metadata, from cache or by extracting PSARC."""
    import asyncio
    dlc = _get_dlc_dir()
    if not dlc:
        return JSONResponse({"error": "DLC folder not configured"}, 404)

    psarc_path = dlc / filename
    if not psarc_path.exists():
        return JSONResponse({"error": "File not found"}, 404)

    stat = psarc_path.stat()
    cached = meta_db.get(filename, stat.st_mtime, stat.st_size)
    if cached:
        return cached

    # Extract in thread pool
    def _extract():
        meta = _extract_meta_for_file(psarc_path)
        meta_db.put(filename, stat.st_mtime, stat.st_size, meta)
        return meta

    meta = await asyncio.get_event_loop().run_in_executor(None, _extract)
    return meta


# ── Highway WebSocket ─────────────────────────────────────────────────────────

# Cache extracted PSARCs to avoid re-extraction on arrangement switch
_extract_cache = {}  # filename -> (tmp_dir, song, timestamp)
_extract_cache_lock = threading.Lock()


def _get_or_extract(filename, psarc_path):
    """Return cached extraction or extract fresh."""
    import time
    with _extract_cache_lock:
        cached = _extract_cache.get(filename)
        if cached:
            tmp, song, ts = cached
            if Path(tmp).exists() and (time.time() - ts) < 300:  # 5 min cache
                return tmp, song, False  # False = not new
            else:
                shutil.rmtree(tmp, ignore_errors=True)
                del _extract_cache[filename]

    tmp = tempfile.mkdtemp(prefix="rs_web_")
    unpack_psarc(str(psarc_path), tmp)
    song = load_song(tmp)

    with _extract_cache_lock:
        # Clean old entries if cache gets too big
        if len(_extract_cache) > 10:
            oldest = min(_extract_cache, key=lambda k: _extract_cache[k][2])
            old_tmp = _extract_cache.pop(oldest)[0]
            shutil.rmtree(old_tmp, ignore_errors=True)
        import time as _t
        _extract_cache[filename] = (tmp, song, _t.time())

    return tmp, song, True  # True = freshly extracted


@app.get("/api/sloppak/{filename:path}/file/{rel_path:path}")
def serve_sloppak_file(filename: str, rel_path: str):
    """Serve a file from inside a sloppak (stems, cover, etc.)."""
    src = sloppak_mod.get_cached_source_dir(filename)
    if src is None:
        dlc = _get_dlc_dir()
        if not dlc:
            return JSONResponse({"error": "not configured"}, 404)
        try:
            src = sloppak_mod.resolve_source_dir(filename, dlc, SLOPPAK_CACHE_DIR)
        except Exception:
            return JSONResponse({"error": "not found"}, 404)
    # Prevent path traversal.
    target = (src / rel_path).resolve()
    try:
        target.relative_to(src.resolve())
    except ValueError:
        return JSONResponse({"error": "forbidden"}, 403)
    if not target.exists() or not target.is_file():
        return JSONResponse({"error": "not found"}, 404)
    ext = target.suffix.lower()
    mt = {
        ".ogg": "audio/ogg", ".opus": "audio/ogg", ".oga": "audio/ogg",
        ".mp3": "audio/mpeg", ".wav": "audio/wav", ".flac": "audio/flac",
        ".m4a": "audio/mp4",
        ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
        ".png": "image/png", ".webp": "image/webp",
        ".json": "application/json",
    }.get(ext)
    return FileResponse(str(target), media_type=mt) if mt else FileResponse(str(target))


@app.websocket("/ws/highway/{filename:path}")
async def highway_ws(websocket: WebSocket, filename: str, arrangement: int = -1):
    """Stream song data for the highway renderer over WebSocket."""
    await websocket.accept()

    dlc = _get_dlc_dir()
    if not dlc:
        await websocket.send_json({"error": "DLC folder not configured"})
        await websocket.close()
        return

    psarc_path = dlc / filename
    if not psarc_path.exists():
        await websocket.send_json({"error": "File not found"})
        await websocket.close()
        return

    is_slop = sloppak_mod.is_sloppak(psarc_path)
    tmp = None
    owns_tmp = False
    loaded_slop = None  # LoadedSloppak when is_slop
    _keepalive_active = True

    async def _send_keepalives():
        while _keepalive_active:
            try:
                await asyncio.sleep(3)
                if _keepalive_active:
                    await websocket.send_json({"type": "loading", "stage": "Loading..."})
            except Exception:
                break

    try:
        await websocket.send_json({"type": "loading", "stage": "Extracting..."})
        keepalive_task = asyncio.create_task(_send_keepalives())

        try:
            loop = asyncio.get_event_loop()
            if is_slop:
                SLOPPAK_CACHE_DIR.mkdir(parents=True, exist_ok=True)
                loaded_slop = await loop.run_in_executor(
                    None,
                    lambda: sloppak_mod.load_song(filename, dlc, SLOPPAK_CACHE_DIR),
                )
                song = loaded_slop.song
                tmp = str(loaded_slop.source_dir)
                owns_tmp = False
            else:
                tmp, song, owns_tmp = await loop.run_in_executor(None, lambda: _get_or_extract(filename, psarc_path))
        finally:
            _keepalive_active = False
            keepalive_task.cancel()

        if not song.arrangements:
            await websocket.send_json({"error": "No arrangements found"})
            await websocket.close()
            return

        # Pick arrangement: explicit request > user preference > most notes
        best = -1
        if 0 <= arrangement < len(song.arrangements):
            best = arrangement
        else:
            # Check user's default arrangement preference
            pref = ""
            config_file = CONFIG_DIR / "config.json"
            if config_file.exists():
                try:
                    pref = json.loads(config_file.read_text()).get("default_arrangement", "")
                except Exception:
                    pass
            if pref:
                for i, a in enumerate(song.arrangements):
                    if a.name == pref:
                        best = i
                        break
        if best < 0:
            # Fallback: most notes
            best = 0
            best_count = 0
            for i, a in enumerate(song.arrangements):
                c = len(a.notes) + sum(len(ch.notes) for ch in a.chords)
                if c > best_count:
                    best_count = c
                    best = i
        arr = song.arrangements[best]

        # Convert audio with unique filename (check cache first)
        audio_url = None
        audio_error: str | None = None  # Surfaced in song_info when audio_url is None
        stems_payload: list[dict] = []
        audio_id = Path(filename).stem.replace(" ", "_")

        if is_slop:
            # Stems are served via the sloppak file endpoint; the first stem
            # (or explicit default) is the core <audio> source. The stems
            # plugin replaces it with a mixed graph when active.
            from urllib.parse import quote
            q_fn = quote(filename, safe="")
            for s in loaded_slop.stems:
                url = f"/api/sloppak/{q_fn}/file/{quote(s['file'])}"
                stems_payload.append({"id": s["id"], "url": url, "default": s["default"]})
            if stems_payload:
                audio_url = stems_payload[0]["url"]
            else:
                audio_error = "This sloppak has no playable stems."
        else:
            AUDIO_CACHE_DIR.mkdir(parents=True, exist_ok=True)
            # Check if audio already cached (writable cache dir or legacy static dir)
            for ext in [".mp3", ".ogg", ".wav"]:
                for cache_dir in [AUDIO_CACHE_DIR, STATIC_DIR]:
                    cached_audio = cache_dir / f"audio_{audio_id}{ext}"
                    if cached_audio.exists() and cached_audio.stat().st_size > 1000:
                        audio_url = f"/audio/audio_{audio_id}{ext}"
                        break
                if audio_url:
                    break

        if not audio_url and not is_slop:
            await websocket.send_json({"type": "loading", "stage": "Converting audio..."})
            wem_files = find_wem_files(tmp)
            if not wem_files:
                audio_error = "No WEM audio files were found inside this PSARC."
            else:
                try:
                    audio_path = convert_wem(wem_files[0], os.path.join(tmp, "audio"))
                    ext = Path(audio_path).suffix
                    audio_dest = AUDIO_CACHE_DIR / f"audio_{audio_id}{ext}"
                    shutil.copy2(audio_path, audio_dest)
                    audio_url = f"/audio/audio_{audio_id}{ext}"
                except Exception as e:
                    print(f"Audio conversion failed: {e}")
                    import traceback
                    traceback.print_exc()
                    audio_error = f"Audio conversion failed: {e}"

            # Clean up old audio cache files (keep max 100)
            try:
                audio_files = [f for f in AUDIO_CACHE_DIR.iterdir()
                               if f.name.startswith("audio_") and f.suffix in (".mp3", ".ogg", ".wav")]
                if len(audio_files) > 100:
                    audio_files.sort(key=lambda f: f.stat().st_atime)
                    for f in audio_files[:len(audio_files) - 100]:
                        f.unlink(missing_ok=True)
            except Exception:
                pass

        # Send song metadata
        arr_list = [{"index": i, "name": a.name, "notes": len(a.notes) + sum(len(c.notes) for c in a.chords)}
                    for i, a in enumerate(song.arrangements)]
        await websocket.send_json({
            "type": "song_info",
            "title": song.title,
            "artist": song.artist,
            "duration": song.song_length,
            "arrangement": arr.name,
            "arrangement_index": best,
            "arrangements": arr_list,
            "audio_url": audio_url,
            "audio_error": audio_error,
            "tuning": arr.tuning,
            # Number of strings on the active arrangement
            # (slopsmith-plugin-3dhighway#7). RS XML / PSARC sources
            # always emit `tuning` as length 6 with zero-padding for
            # unused string slots, so `len(arr.tuning)` is unreliable
            # there; sloppak / GP-imported sources may instead carry
            # a trimmed list. arrangement_string_count() combines a
            # notes-derived lower bound, a name-based fallback (4 for
            # "bass" arrangements), and the tuning length (when it
            # disagrees with the RS-XML padded 6) into a single
            # reliable signal. Plugins should size string-indexed UI
            # / geometry against THIS rather than assuming 6 or
            # using `tuning.length` directly.
            "stringCount": arrangement_string_count(arr),
            "capo": arr.capo,
            "format": "sloppak" if is_slop else "psarc",
            "stems": stems_payload,
        })

        # Send beats
        beats = [{"time": b.time, "measure": b.measure} for b in song.beats]
        await websocket.send_json({"type": "beats", "data": beats})

        # Send sections
        sections = [{"name": s.name, "time": s.start_time} for s in song.sections]
        await websocket.send_json({"type": "sections", "data": sections})

        # Send anchors
        anchors = [{"time": a.time, "fret": a.fret, "width": a.width} for a in arr.anchors]
        await websocket.send_json({"type": "anchors", "data": anchors})

        # Send chord templates. Include `fingers` alongside `name` /
        # `frets` so plugin overlays consuming highway.getChordTemplates()
        # can render full chord boxes (Rocksmith-style fingering
        # diagrams), not just chord names. Each fingering entry is
        # per-string: -1 = unused, 0 = open string, n > 0 = finger
        # number. RS XML sources populate real values; GP imports
        # currently emit all -1 (no finger data available pre-import).
        templates = []
        for ct in arr.chord_templates:
            templates.append({
                "name": ct.name,
                "fingers": ct.fingers,
                "frets": ct.frets,
            })
        await websocket.send_json({"type": "chord_templates", "data": templates})

        # Send lyrics if available
        import xml.etree.ElementTree as ET
        lyrics = []
        if is_slop:
            lyrics = list(song.lyrics or [])
        else:
            for xml_path in sorted(Path(tmp).rglob("*.xml")):
                try:
                    root = ET.parse(xml_path).getroot()
                    if root.tag == "vocals":
                        for v in root.findall("vocal"):
                            lyrics.append({
                                "t": round(float(v.get("time", "0")), 3),
                                "d": round(float(v.get("length", "0")), 3),
                                "w": v.get("lyric", ""),
                            })
                        break
                except Exception:
                    pass
            if not lyrics:
                # SNG-only PSARC (official DLC) — decode vocals SNG directly.
                try:
                    from lib.sng_vocals import parse_vocals_sng
                    for sng_path in sorted(Path(tmp).rglob("*vocals*.sng")):
                        plat = "mac" if "/macos/" in str(sng_path).replace("\\", "/").lower() else "pc"
                        try:
                            lyrics = parse_vocals_sng(str(sng_path), plat)
                        except Exception:
                            lyrics = []
                        if lyrics:
                            break
                except ImportError:
                    pass
        if lyrics:
            await websocket.send_json({"type": "lyrics", "data": lyrics})

        # Send tone changes (PSARC only; sloppak has no tone XML)
        tone_changes = []
        if is_slop:
            xml_paths = []
        else:
            xml_paths = sorted(Path(tmp).rglob("*.xml"))

        # Build tone ID→name map from manifest JSON matching selected arrangement
        tone_id_map = {}  # {0: "Tone_A_name", 1: "Tone_B_name", ...}
        arr_name_lower = arr.name.lower() if arr else ""
        for jf in sorted(Path(tmp).rglob("*.json")):
            try:
                # Prefer manifest matching selected arrangement
                if arr_name_lower and arr_name_lower not in jf.stem.lower():
                    continue
                jdata = json.loads(jf.read_text())
                for entry in (jdata.get("Entries") or {}).values():
                    attrs = entry.get("Attributes") or {}
                    for idx, key in enumerate(["Tone_A", "Tone_B", "Tone_C", "Tone_D"]):
                        val = attrs.get(key, "")
                        if val:
                            tone_id_map[idx] = val
                    if tone_id_map:
                        break
            except Exception:
                continue
            if tone_id_map:
                break
        # Fallback: try any manifest if arrangement-specific one not found
        if not tone_id_map:
            for jf in sorted(Path(tmp).rglob("*.json")):
                try:
                    jdata = json.loads(jf.read_text())
                    for entry in (jdata.get("Entries") or {}).values():
                        attrs = entry.get("Attributes") or {}
                        for idx, key in enumerate(["Tone_A", "Tone_B", "Tone_C", "Tone_D"]):
                            val = attrs.get(key, "")
                            if val:
                                tone_id_map[idx] = val
                        if tone_id_map:
                            break
                except Exception:
                    continue
                if tone_id_map:
                    break

        # Parse XMLs — prefer the one matching selected arrangement, fall back to any
        # Try arrangement-matching XML first, then fall back to any
        def _xml_matches_arr(xp):
            return arr_name_lower and arr_name_lower in xp.stem.lower()
        sorted_xml = sorted(xml_paths, key=lambda xp: (0 if _xml_matches_arr(xp) else 1, xp.name))
        for xml_path in sorted_xml:
            try:
                root = ET.parse(xml_path).getroot()
                if root.tag != "song":
                    continue
                tones_el = root.find("tones")
                if tones_el is not None:
                    for t in tones_el.findall("tone"):
                        tc_time = t.get("time")
                        tc_name = t.get("name", "")
                        tc_id = t.get("id", "")
                        # Resolve "N/A" or empty names using tone ID map
                        if (not tc_name or tc_name == "N/A") and tc_id:
                            tc_name = tone_id_map.get(int(tc_id), f"Tone {tc_id}")
                        if tc_time and tc_name:
                            tone_changes.append({
                                "t": round(float(tc_time), 3),
                                "name": tc_name,
                            })
                    if tone_changes:
                        tonebase = root.find("tonebase")
                        base_name = tonebase.text if tonebase is not None and tonebase.text else ""
                        # If base name not in XML, use Tone_A from tone_id_map (same arrangement)
                        if not base_name:
                            base_name = tone_id_map.get(0, "")
                        await websocket.send_json({
                            "type": "tone_changes",
                            "base": base_name,
                            "data": sorted(tone_changes, key=lambda x: x["t"]),
                        })
                        break
            except Exception:
                pass

        # Send notes in chunks
        notes = []
        for n in arr.notes:
            notes.append({
                "t": round(n.time, 3), "s": n.string, "f": n.fret,
                "sus": round(n.sustain, 3),
                "sl": n.slide_to, "slu": n.slide_unpitch_to,
                "bn": round(n.bend, 1) if n.bend else 0,
                "ho": n.hammer_on, "po": n.pull_off,
                "hm": n.harmonic, "hp": n.harmonic_pinch,
                "pm": n.palm_mute, "mt": n.mute,
                "tr": n.tremolo, "ac": n.accent, "tp": n.tap,
            })
        # Send in chunks of 500
        for i in range(0, len(notes), 500):
            await websocket.send_json({
                "type": "notes",
                "data": notes[i:i+500],
                "total": len(notes),
            })

        # Send chords
        chords = []
        for c in arr.chords:
            chord_notes = [{
                "s": cn.string, "f": cn.fret,
                "sus": round(cn.sustain, 3),
                "bn": round(cn.bend, 1) if cn.bend else 0,
                "sl": cn.slide_to, "slu": cn.slide_unpitch_to,
                "ho": cn.hammer_on, "po": cn.pull_off,
                "hm": cn.harmonic, "hp": cn.harmonic_pinch,
                "pm": cn.palm_mute, "mt": cn.mute,
                "tr": cn.tremolo, "ac": cn.accent, "tp": cn.tap,
            } for cn in c.notes]
            chords.append({
                "t": round(c.time, 3),
                "id": c.chord_id,
                "hd": c.high_density,
                "notes": chord_notes,
            })
        for i in range(0, len(chords), 500):
            await websocket.send_json({
                "type": "chords",
                "data": chords[i:i+500],
                "total": len(chords),
            })

        # Per-phrase difficulty data for the master-difficulty slider
        # (slopsmith#48). Only sent when the source chart had multiple
        # `<level>` tiers — single-level charts (GP converter, older
        # sloppaks without phrase data) produce arr.phrases=None, and the
        # frontend treats the missing message as "slider disabled".
        # Consumers that don't know about this message type ignore it.
        #
        # Chunked at phrase granularity (20 phrases per frame) because
        # each phrase nests per-level note/chord lists — a single frame
        # could otherwise exceed proxy/WS size limits on large songs.
        # Chunk boundary is per-phrase (not per-level) so the frontend
        # reassembles whole phrase ladders.
        if arr.phrases:
            total = len(arr.phrases)
            for i in range(0, total, 20):
                await websocket.send_json({
                    "type": "phrases",
                    "data": [phrase_to_wire(p) for p in arr.phrases[i:i + 20]],
                    "total": total,
                })

        await websocket.send_json({"type": "ready"})

        # Keep connection alive for control messages
        try:
            while True:
                msg = await websocket.receive_text()
                data = json.loads(msg)
                if data.get("action") == "change_arrangement":
                    pass
        except WebSocketDisconnect:
            pass

    except Exception as e:
        import traceback
        traceback.print_exc()
        try:
            await websocket.send_json({"error": str(e)})
            await websocket.close()
        except Exception:
            pass

    finally:
        pass  # Don't clean up — cached for arrangement switching


# ── Audio serving ─────────────────────────────────────────────────────────────


@app.get("/audio/{filename:path}")
def serve_audio(filename: str):
    """Serve audio files from the writable audio cache directory."""
    for d in [AUDIO_CACHE_DIR, STATIC_DIR]:
        audio_file = d / filename
        if audio_file.exists():
            return FileResponse(str(audio_file))
    return JSONResponse({"error": "not found"}, status_code=404)


app.mount("/static", StaticFiles(directory=str(STATIC_DIR)), name="static")


@app.get("/")
def index():
    return FileResponse(str(Path(__file__).parent / "static" / "index.html"))
