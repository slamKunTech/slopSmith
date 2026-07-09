//! SQLite metadata cache. Mirrors `MetadataDB` in server.py:49-318.
//!
//! A single `rusqlite::Connection` guarded by a `Mutex` — the same
//! serialization model as Python's `check_same_thread=False` + `threading.Lock`.
//! All query methods return `serde_json::Value` shaped to match the Python
//! dicts the frontend expects, so the HTTP layer can return them verbatim.

use std::collections::HashSet;
use std::sync::Mutex;

use rusqlite::{params, params_from_iter, Connection, OpenFlags};
use serde_json::{Map, Value};

pub struct MetadataDb {
    conn: Mutex<Connection>,
}

/// A song row's cached metadata, as stored in the `songs` table. Built by the
/// scanner (`_extract_meta_for_file`) and read back by the library endpoints.
#[derive(Debug, Clone)]
pub struct SongMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub duration: f64,
    pub tuning: String,
    pub arrangements: Value, // JSON array
    pub has_lyrics: bool,
    pub format: String,
    pub stem_count: i64,
}

impl Default for SongMeta {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            year: String::new(),
            duration: 0.0,
            tuning: String::new(),
            arrangements: Value::Array(vec![]),
            has_lyrics: false,
            format: "psarc".to_string(),
            stem_count: 0,
        }
    }
}

impl SongMeta {
    /// The JSON shape returned by `GET /api/song/{filename}` (the `db.get()`
    /// dict in Python). Keys mirror the Python dict exactly.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "title": self.title,
            "artist": self.artist,
            "album": self.album,
            "year": self.year,
            "duration": self.duration,
            "tuning": self.tuning,
            "arrangements": self.arrangements,
            "has_lyrics": self.has_lyrics,
            "format": self.format,
            "stem_count": self.stem_count,
        })
    }
}

impl MetadataDb {
    pub fn open(config_dir: &std::path::Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(config_dir)?;
        let db_path = config_dir.join("web_library.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS songs (
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
            );
            CREATE INDEX IF NOT EXISTS idx_songs_artist ON songs(artist COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_songs_title ON songs(title COLLATE NOCASE);
            CREATE TABLE IF NOT EXISTS favorites (filename TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS loops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                filename TEXT NOT NULL,
                name TEXT NOT NULL,
                start_time REAL NOT NULL,
                end_time REAL NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            );",
        )?;

        // Idempotent migrations for installs that predate these columns.
        for col in [
            "format TEXT DEFAULT 'psarc'",
            "stem_count INTEGER DEFAULT 0",
        ] {
            let sql = format!("ALTER TABLE songs ADD COLUMN {col}");
            // "duplicate column name" → already migrated; ignore.
            if let Err(e) = conn.execute(&sql, []) {
                if !e.to_string().contains("duplicate column") {
                    return Err(e.into());
                }
            }
        }

        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn is_favorite(&self, filename: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM favorites WHERE filename = ?",
            params![filename],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Toggle favorite. Returns the new state. Mirrors `toggle_favorite`.
    pub fn toggle_favorite(&self, filename: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let exists = conn
            .query_row("SELECT 1 FROM favorites WHERE filename = ?", params![filename], |_| Ok(()))
            .is_ok();
        if exists {
            let _ = conn.execute("DELETE FROM favorites WHERE filename = ?", params![filename]);
        } else {
            let _ = conn.execute("INSERT OR IGNORE INTO favorites VALUES (?)", params![filename]);
        }
        !exists
    }

    pub fn favorite_set(&self) -> HashSet<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT filename FROM favorites").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        let mut set = HashSet::new();
        for r in rows {
            if let Ok(f) = r {
                set.insert(f);
            }
        }
        set
    }

    /// Cached entry for a file if mtime+size match and title is non-empty.
    /// Mirrors `get()` (server.py:117-131).
    pub fn get(&self, filename: &str, mtime: f64, size: i64) -> Option<SongMeta> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT mtime, size, title, artist, album, year, duration, tuning,
                    arrangements, has_lyrics, format, stem_count
             FROM songs WHERE filename = ?",
        )
        .ok()?;
        let row = stmt.query_row(params![filename], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?,
            ))
        })
        .ok()?;
        let (rmtime, rsize, title, artist, album, year, duration, tuning, arrangements, has_lyrics, format, stem_count) = row;
        if rmtime != mtime || rsize != size || title.is_empty() {
            return None;
        }
        let arrangements = arrangements
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Array(vec![]));
        Some(SongMeta {
            title,
            artist,
            album,
            year,
            duration,
            tuning,
            arrangements,
            has_lyrics: has_lyrics != 0,
            format: format.unwrap_or_else(|| "psarc".to_string()),
            stem_count: stem_count.unwrap_or(0),
        })
    }

    pub fn put(&self, filename: &str, mtime: f64, size: i64, meta: &SongMeta) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let arrangements = serde_json::to_string(&meta.arrangements)?;
        conn.execute(
            "INSERT OR REPLACE INTO songs
             (filename, mtime, size, title, artist, album, year, duration, tuning,
              arrangements, has_lyrics, format, stem_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                filename,
                mtime,
                size,
                meta.title,
                meta.artist,
                meta.album,
                meta.year,
                meta.duration,
                meta.tuning,
                arrangements,
                meta.has_lyrics as i64,
                meta.format,
                meta.stem_count,
            ],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn count(&self) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM songs WHERE title != ''", [], |r| r.get(0))
            .unwrap_or(0)
    }

    /// Remove rows for files no longer on disk. Returns count removed.
    pub fn delete_missing(&self, current: &HashSet<String>) -> usize {
        let mut conn = self.conn.lock().unwrap();
        let db_files: HashSet<String> = {
            let mut stmt = conn.prepare("SELECT filename FROM songs").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            let mut s = HashSet::new();
            for r in rows.flatten() {
                s.insert(r);
            }
            s
        };
        let stale: Vec<&String> = db_files.difference(current).collect();
        if stale.is_empty() {
            return 0;
        }
        let tx = conn.transaction().unwrap();
        for f in &stale {
            let _ = tx.execute("DELETE FROM songs WHERE filename = ?", params![f]);
        }
        let _ = tx.commit();
        stale.len()
    }

    /// Filenames that have a retuned variant (`_EStd_`/`_DropD_`) in the DB,
    /// mapped back to the original filename. Mirrors `_estd_set()`.
    fn estd_set(&self) -> HashSet<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT filename FROM songs
             WHERE filename LIKE '%\\_EStd\\_%' ESCAPE '\\'
                OR filename LIKE '%\\_DropD\\_%' ESCAPE '\\'",
        )
        .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        let mut set = HashSet::new();
        for r in rows.flatten() {
            set.insert(r.replace("_EStd_", "_").replace("_DropD_", "_"));
        }
        set
    }

    /// DELETE FROM songs (full rescan). Mirrors `trigger_full_rescan`.
    pub fn clear_songs(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM songs", [])?;
        Ok(())
    }

    /// Run an arbitrary parameterized SQL statement (used by `POST
    /// /api/song/{filename}/meta` to UPDATE cached metadata fields).
    pub fn execute_sql(&self, sql: &str, params: &[rusqlite::types::Value]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(sql, params_from_iter(params.iter()))?;
        Ok(())
    }

    /// Paginated library search. Returns `(songs, total)`. Mirrors
    /// `query_page()` (server.py:172-217).
    pub fn query_page(
        &self,
        q: &str,
        page: i64,
        size: i64,
        sort: &str,
        direction: &str,
        favorites_only: bool,
        format_filter: &str,
    ) -> anyhow::Result<(Value, i64)> {
        let mut where_ = String::from("WHERE title != ''");
        // Use `rusqlite::types::Value` (Clone) so the trailing LIMIT/OFFSET
        // integers can be appended to the same params vec.
        let mut paramsv: Vec<rusqlite::types::Value> = Vec::new();
        if favorites_only {
            where_.push_str(" AND filename IN (SELECT filename FROM favorites)");
        }
        if !format_filter.is_empty() {
            where_.push_str(" AND format = ?");
            paramsv.push(rusqlite::types::Value::Text(format_filter.to_string()));
        }
        if !q.is_empty() {
            where_.push_str(
                " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE OR album LIKE ? COLLATE NOCASE)",
            );
            let pat = format!("%{q}%");
            paramsv.push(rusqlite::types::Value::Text(pat.clone()));
            paramsv.push(rusqlite::types::Value::Text(pat.clone()));
            paramsv.push(rusqlite::types::Value::Text(pat));
        }

        let order = match sort {
            "artist" => "artist COLLATE NOCASE",
            "artist-desc" => "artist COLLATE NOCASE DESC",
            "title" => "title COLLATE NOCASE",
            "title-desc" => "title COLLATE NOCASE DESC",
            "recent" => "mtime DESC",
            "tuning" => "tuning COLLATE NOCASE",
            _ => "artist COLLATE NOCASE",
        };
        let mut order = order.to_string();
        if direction.eq_ignore_ascii_case("desc") && !order.contains("DESC") {
            order.push_str(" DESC");
        }

        // Compute estd/favs BEFORE taking the connection lock — estd_set and
        // favorite_set lock the same Mutex, and std::sync::Mutex is not
        // reentrant (matches Python, where _estd_set/favorite_set don't take
        // self._lock).
        let estd = self.estd_set();
        let favs = self.favorite_set();

        let conn = self.conn.lock().unwrap();
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM songs {where_}"),
                params_from_iter(paramsv.iter()),
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut stmt = conn.prepare(&format!(
            "SELECT filename, title, artist, album, year, duration, tuning, arrangements,
                    has_lyrics, mtime, format, stem_count
             FROM songs {where_} ORDER BY {order} LIMIT ? OFFSET ?"
        ))?;
        let mut query_params = paramsv.clone();
        query_params.push(rusqlite::types::Value::Integer(size));
        query_params.push(rusqlite::types::Value::Integer(page * size));
        let rows = stmt.query_map(
            params_from_iter(query_params.iter()),
            row_to_song_value,
        )?;

        let mut songs = Vec::new();
        for r in rows {
            if let Ok(mut v) = r {
                if let Some(obj) = v.as_object_mut() {
                    let fname = obj
                        .get("filename")
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    obj.insert("has_estd".into(), Value::Bool(estd.contains(&fname)));
                    obj.insert("favorite".into(), Value::Bool(favs.contains(&fname)));
                }
                songs.push(v);
            }
        }
        Ok((Value::Array(songs), total))
    }

    /// Artists grouped by letter → album → songs. Mirrors `query_artists()`
    /// (server.py:219-298).
    pub fn query_artists(
        &self,
        letter: &str,
        q: &str,
        favorites_only: bool,
        page: i64,
        size: i64,
        format_filter: &str,
    ) -> anyhow::Result<(Value, i64)> {
        let mut where_ = String::from("WHERE title != ''");
        let mut paramsv: Vec<rusqlite::types::Value> = Vec::new();
        if favorites_only {
            where_.push_str(" AND filename IN (SELECT filename FROM favorites)");
        }
        if !format_filter.is_empty() {
            where_.push_str(" AND format = ?");
            paramsv.push(rusqlite::types::Value::Text(format_filter.to_string()));
        }
        if letter == "#" {
            where_.push_str(" AND artist NOT GLOB '[A-Za-z]*'");
        } else if !letter.is_empty() {
            where_.push_str(" AND UPPER(SUBSTR(artist, 1, 1)) = ?");
            paramsv.push(rusqlite::types::Value::Text(letter.to_uppercase()));
        }
        if !q.is_empty() {
            where_.push_str(
                " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE OR album LIKE ? COLLATE NOCASE)",
            );
            let pat = format!("%{q}%");
            paramsv.push(rusqlite::types::Value::Text(pat.clone()));
            paramsv.push(rusqlite::types::Value::Text(pat.clone()));
            paramsv.push(rusqlite::types::Value::Text(pat));
        }

        // Compute estd/favs before locking conn (see query_page for why).
        let estd = self.estd_set();
        let favs = self.favorite_set();

        let conn = self.conn.lock().unwrap();
        let total_artists: i64 = conn
            .query_row(
                &format!("SELECT COUNT(DISTINCT artist COLLATE NOCASE) FROM songs {where_}"),
                params_from_iter(paramsv.iter()),
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT artist COLLATE NOCASE as a FROM songs {where_} ORDER BY a LIMIT ? OFFSET ?"
        ))?;
        let mut artist_params = paramsv.clone();
        artist_params.push(rusqlite::types::Value::Integer(size));
        artist_params.push(rusqlite::types::Value::Integer(page * size));
        let artist_rows = stmt.query_map(
            params_from_iter(artist_params.iter()),
            |r| r.get::<_, String>(0),
        )?;
        let artist_names: Vec<String> = artist_rows.flatten().collect();
        if artist_names.is_empty() {
            return Ok((Value::Array(vec![]), total_artists));
        }

        // Fetch songs for these artists only.
        let placeholders = (0..artist_names.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let song_where = format!("{where_} AND artist COLLATE NOCASE IN ({placeholders})");
        let mut song_params: Vec<rusqlite::types::Value> = paramsv.clone();
        for n in &artist_names {
            song_params.push(rusqlite::types::Value::Text(n.clone()));
        }
        let mut stmt2 = conn.prepare(&format!(
            "SELECT filename, title, artist, album, year, duration, tuning, arrangements,
                    has_lyrics, format, stem_count
             FROM songs {song_where}
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, title COLLATE NOCASE"
        ))?;
        let rows = stmt2.query_map(
            params_from_iter(song_params.iter()),
            row_to_artist_song_value,
        )?;

        // Rows arrive ordered by artist/album/title (NOCASE), so group
        // sequentially: lowercased artist+album keys are adjacent.
        let mut artists: Vec<(String, Vec<(String, Vec<Value>)>)> = Vec::new();

        for r in rows {
            let mut v = r?;
            let fname = v["filename"].as_str().unwrap_or("").to_string();
            let artist = v["artist"].as_str().unwrap_or("Unknown Artist").to_string();
            let album = v["album"].as_str().unwrap_or("Unknown Album").to_string();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("has_estd".into(), Value::Bool(estd.contains(&fname)));
                obj.insert("favorite".into(), Value::Bool(favs.contains(&fname)));
            }
            let akey = artist.to_lowercase();
            let bkey = album.to_lowercase();

            // Advance artist group when the lowercased key changes.
            let need_new_artist = artists
                .last()
                .map(|(n, _)| n.to_lowercase() != akey)
                .unwrap_or(true);
            if need_new_artist {
                artists.push((artist.clone(), Vec::new()));
            }
            let albums = &mut artists.last_mut().unwrap().1;

            // Advance album group when the lowercased key changes.
            let need_new_album = albums
                .last()
                .map(|(n, _)| n.to_lowercase() != bkey)
                .unwrap_or(true);
            if need_new_album {
                albums.push((album.clone(), Vec::new()));
            }
            albums.last_mut().unwrap().1.push(v);
        }

        let mut result = Vec::new();
        for (display_name, albums) in &artists {
            let album_count = albums.len();
            let song_count: usize = albums.iter().map(|(_, s)| s.len()).sum();
            let albums_json: Vec<Value> = albums
                .iter()
                .map(|(name, songs)| serde_json::json!({ "name": name, "songs": songs.clone() }))
                .collect();
            result.push(serde_json::json!({
                "name": display_name,
                "album_count": album_count,
                "song_count": song_count,
                "albums": albums_json,
            }));
        }

        Ok((Value::Array(result), total_artists))
    }

    /// Aggregate stats for the letter bar. Mirrors `query_stats()`.
    pub fn query_stats(&self, favorites_only: bool) -> Value {
        let conn = self.conn.lock().unwrap();
        let filt = if favorites_only {
            " AND filename IN (SELECT filename FROM favorites)"
        } else {
            ""
        };
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM songs WHERE title != ''{filt}"),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let artist_count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(DISTINCT artist) FROM songs WHERE title != ''{filt}"),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT UPPER(SUBSTR(artist, 1, 1)) as letter, COUNT(DISTINCT artist COLLATE NOCASE)
                 FROM songs WHERE title != ''{filt} GROUP BY letter"
            ))
            .unwrap();
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        });
        let mut letters: Map<String, Value> = Map::new();
        if let Ok(rows) = rows {
            for r in rows.flatten() {
                let (letter, count) = r;
                match letter {
                    Some(l) if l.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) => {
                        letters.insert(l, Value::from(count));
                    }
                    _ => {
                        let entry = letters.entry("#".to_string()).or_insert(Value::from(0i64));
                        if let Some(n) = entry.as_i64() {
                            *entry = Value::from(n + count);
                        }
                    }
                }
            }
        }
        serde_json::json!({
            "total_songs": total,
            "total_artists": artist_count,
            "letters": letters,
        })
    }

    // ── Loops ───────────────────────────────────────────────────────────

    pub fn list_loops(&self, filename: &str) -> Vec<Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, name, start_time, end_time FROM loops WHERE filename = ? ORDER BY start_time",
            )
            .unwrap();
        let rows = stmt
            .query_map(params![filename], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "start": r.get::<_, f64>(2)?,
                    "end": r.get::<_, f64>(3)?,
                }))
            })
            .unwrap();
        rows.flatten().collect()
    }

    /// Insert a loop. `name` defaults to `Loop {n+1}` if empty. Mirrors
    /// `save_loop()` (server.py:746-765). Returns `(ok, name)` or an error
    /// message string.
    pub fn save_loop(&self, filename: &str, name: &str, start: f64, end: f64) -> Result<(bool, String), String> {
        if filename.is_empty() {
            return Err("Missing fields".into());
        }
        let name = name.trim();
        let name = if name.is_empty() {
            let conn = self.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM loops WHERE filename = ?",
                    params![filename],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            format!("Loop {}", count + 1)
        } else {
            name.to_string()
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO loops (filename, name, start_time, end_time) VALUES (?, ?, ?, ?)",
            params![filename, name, start, end],
        )
        .map_err(|e| e.to_string())?;
        Ok((true, name))
    }

    pub fn delete_loop(&self, loop_id: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM loops WHERE id = ?", params![loop_id]).is_ok()
    }
}

// Helper: build the per-song JSON object for `query_page` rows.
fn row_to_song_value(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let arrangements: Option<String> = r.get(7)?;
    let arrangements = arrangements
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Array(vec![]));
    Ok(serde_json::json!({
        "filename": r.get::<_, String>(0)?,
        "title": r.get::<_, String>(1)?,
        "artist": r.get::<_, String>(2)?,
        "album": r.get::<_, String>(3)?,
        "year": r.get::<_, String>(4)?,
        "duration": r.get::<_, f64>(5)?,
        "tuning": r.get::<_, String>(6)?,
        "arrangements": arrangements,
        "has_lyrics": r.get::<_, i64>(8)? != 0,
        "mtime": r.get::<_, f64>(9)?,
        "format": r.get::<_, Option<String>>(10)?.unwrap_or_else(|| "psarc".to_string()),
        "stem_count": r.get::<_, Option<i64>>(11)?.unwrap_or(0),
    }))
}

// Helper: build the per-song JSON object for `query_artists` rows (no mtime).
fn row_to_artist_song_value(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let arrangements: Option<String> = r.get(7)?;
    let arrangements = arrangements
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Array(vec![]));
    Ok(serde_json::json!({
        "filename": r.get::<_, String>(0)?,
        "title": r.get::<_, String>(1)?,
        "artist": r.get::<_, String>(2)?,
        "album": r.get::<_, String>(3)?,
        "year": r.get::<_, String>(4)?,
        "duration": r.get::<_, f64>(5)?,
        "tuning": r.get::<_, String>(6)?,
        "arrangements": arrangements,
        "has_lyrics": r.get::<_, i64>(8)? != 0,
        "format": r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "psarc".to_string()),
        "stem_count": r.get::<_, Option<i64>>(10)?.unwrap_or(0),
    }))
}
