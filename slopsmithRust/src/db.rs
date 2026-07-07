//! SQLite metadata cache — Rust port of `MetadataDB` from `server.py`.
//!
//! Stores extracted song metadata, favorites, and practice loops in a
//! WAL-mode SQLite database. All access is serialized through an internal
//! `std::sync::Mutex<Connection>` so the type is `Sync` and can be shared
//! across the Axum handler tasks and the background scanner thread.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, params_from_iter, types::Value, Connection};
use serde::{Deserialize, Serialize};

// ── Serde-serializable row / result types ────────────────────────────────────

/// One arrangement descriptor as stored inside the `arrangements` JSON blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrangementInfo {
    pub index: i64,
    pub name: String,
    pub notes: i64,
}

/// Cached metadata for a single song (matches the dict `get()` returns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub duration: f64,
    pub tuning: String,
    pub arrangements: Vec<ArrangementInfo>,
    pub has_lyrics: bool,
    pub format: String,
    pub stem_count: i64,
}

impl Default for SongMeta {
    fn default() -> Self {
        SongMeta {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            year: String::new(),
            duration: 0.0,
            tuning: "E Standard".to_string(),
            arrangements: Vec::new(),
            has_lyrics: false,
            format: "psarc".to_string(),
            stem_count: 0,
        }
    }
}

/// A song row as returned by the paginated library query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongEntry {
    pub filename: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub duration: f64,
    pub tuning: String,
    pub arrangements: Vec<ArrangementInfo>,
    pub has_lyrics: bool,
    /// Only serialized for the flat library view; omitted in the artist tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<f64>,
    pub format: String,
    pub stem_count: i64,
    pub has_estd: bool,
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumEntry {
    pub name: String,
    pub songs: Vec<SongEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistEntry {
    pub name: String,
    pub album_count: usize,
    pub song_count: usize,
    pub albums: Vec<AlbumEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsEntry {
    pub total_songs: i64,
    pub total_artists: i64,
    pub letters: HashMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopEntry {
    pub id: i64,
    pub name: String,
    pub start: f64,
    pub end: f64,
}

// ── Database ──────────────────────────────────────────────────────────────────

pub struct MetadataDB {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: String,
}

impl MetadataDB {
    /// Initialize the database, creating the schema (songs / favorites / loops)
    /// and running idempotent migrations. Mirrors `MetadataDB.__init__`.
    pub fn new(config_dir: &Path) -> Self {
        std::fs::create_dir_all(config_dir).ok();
        let db_path = config_dir.join("web_library.db").to_string_lossy().to_string();
        let conn = Connection::open(&db_path).expect("failed to open web_library.db");

        conn.pragma_update(None, "journal_mode", "WAL").ok();

        conn.execute_batch(
            r#"
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
            );
            "#,
        )
        .expect("failed to create songs table");

        // Idempotent migrations for installs predating these columns.
        let _ = conn.execute("ALTER TABLE songs ADD COLUMN format TEXT DEFAULT 'psarc'", []);
        let _ = conn.execute("ALTER TABLE songs ADD COLUMN stem_count INTEGER DEFAULT 0", []);

        conn.execute_batch(
            r#"
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
            );
            "#,
        )
        .expect("failed to create supporting tables");

        MetadataDB {
            conn: Mutex::new(conn),
            db_path,
        }
    }

    // ── Favorites ────────────────────────────────────────────────────────────

    pub fn is_favorite(&self, filename: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        Self::is_fav_conn(&conn, filename)
    }

    fn is_fav_conn(conn: &Connection, filename: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM favorites WHERE filename = ?",
            params![filename],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Toggle favorite status; returns the new state.
    pub fn toggle_favorite(&self, filename: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        if Self::is_fav_conn(&conn, filename) {
            conn.execute("DELETE FROM favorites WHERE filename = ?", params![filename])
                .ok();
            false
        } else {
            conn.execute("INSERT OR IGNORE INTO favorites VALUES (?)", params![filename])
                .ok();
            true
        }
    }

    pub fn favorite_set(&self) -> HashSet<String> {
        let conn = self.conn.lock().unwrap();
        Self::fav_set_conn(&conn)
    }

    fn fav_set_conn(conn: &Connection) -> HashSet<String> {
        let mut set = HashSet::new();
        if let Ok(mut stmt) = conn.prepare("SELECT filename FROM favorites") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for r in rows.flatten() {
                    set.insert(r);
                }
            }
        }
        set
    }

    /// Set of original filenames that have a retuned (`_EStd_` / `_DropD_`) variant.
    fn estd_set_conn(conn: &Connection) -> HashSet<String> {
        let mut set = HashSet::new();
        let sql = "SELECT filename FROM songs WHERE filename LIKE '%\\_EStd\\_%' ESCAPE '\\' \
                   OR filename LIKE '%\\_DropD\\_%' ESCAPE '\\'";
        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for fname in rows.flatten() {
                    let orig = fname.replace("_EStd_", "_").replace("_DropD_", "_");
                    set.insert(orig);
                }
            }
        }
        set
    }

    // ── Cache get / put ────────────────────────────────────────────────────────

    /// Return cached metadata if the on-disk mtime + size still match and a
    /// title is present, otherwise `None` (stale / never scanned).
    pub fn get(&self, filename: &str, mtime: f64, size: i64) -> Option<SongMeta> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT mtime, size, title, artist, album, year, duration, tuning, arrangements, \
             has_lyrics, format, stem_count FROM songs WHERE filename = ?",
            params![filename],
            |row| {
                let row_mtime: Option<f64> = row.get(0)?;
                let row_size: Option<i64> = row.get(1)?;
                let title: Option<String> = row.get(2)?;
                Ok((row_mtime, row_size, title, row_to_meta(row)?))
            },
        )
        .ok()
        .and_then(|(row_mtime, row_size, title, meta)| {
            let title_ok = title.map(|t| !t.is_empty()).unwrap_or(false);
            if row_mtime == Some(mtime) && row_size == Some(size) && title_ok {
                Some(meta)
            } else {
                None
            }
        })
    }

    /// Insert or replace cached metadata for a file.
    pub fn put(&self, filename: &str, mtime: f64, size: i64, meta: &SongMeta) {
        let conn = self.conn.lock().unwrap();
        let arrangements = serde_json::to_string(&meta.arrangements).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT OR REPLACE INTO songs \
             (filename, mtime, size, title, artist, album, year, duration, tuning, arrangements, \
              has_lyrics, format, stem_count) \
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
                if meta.has_lyrics { 1 } else { 0 },
                meta.format,
                meta.stem_count,
            ],
        )
        .ok();
    }

    pub fn count(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM songs WHERE title != ''", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as usize
    }

    /// Remove DB rows for files no longer on disk. Returns number removed.
    pub fn delete_missing(&self, current_filenames: &HashSet<String>) -> usize {
        let conn = self.conn.lock().unwrap();
        let mut db_files: Vec<String> = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT filename FROM songs") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                db_files.extend(rows.flatten());
            }
        }
        let stale: Vec<String> = db_files
            .into_iter()
            .filter(|f| !current_filenames.contains(f))
            .collect();
        for f in &stale {
            conn.execute("DELETE FROM songs WHERE filename = ?", params![f]).ok();
        }
        stale.len()
    }

    /// Delete every song row (used by the full-rescan endpoint).
    pub fn clear_songs(&self) {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM songs", []).ok();
    }

    // ── Paginated queries ──────────────────────────────────────────────────────

    /// Server-side paginated search. Returns `(songs, total_count)`.
    #[allow(clippy::too_many_arguments)]
    pub fn query_page(
        &self,
        q: &str,
        page: i64,
        size: i64,
        sort: &str,
        direction: &str,
        favorites_only: bool,
        format_filter: &str,
    ) -> (Vec<SongEntry>, i64) {
        let conn = self.conn.lock().unwrap();

        let mut where_clause = String::from("WHERE title != ''");
        let mut bind: Vec<Value> = Vec::new();
        if favorites_only {
            where_clause.push_str(" AND filename IN (SELECT filename FROM favorites)");
        }
        if !format_filter.is_empty() {
            where_clause.push_str(" AND format = ?");
            bind.push(Value::Text(format_filter.to_string()));
        }
        if !q.is_empty() {
            where_clause.push_str(
                " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE \
                 OR album LIKE ? COLLATE NOCASE)",
            );
            let like = format!("%{}%", q);
            for _ in 0..3 {
                bind.push(Value::Text(like.clone()));
            }
        }

        let mut order = match sort {
            "artist" => "artist COLLATE NOCASE",
            "artist-desc" => "artist COLLATE NOCASE DESC",
            "title" => "title COLLATE NOCASE",
            "title-desc" => "title COLLATE NOCASE DESC",
            "recent" => "mtime DESC",
            "tuning" => "tuning COLLATE NOCASE",
            _ => "artist COLLATE NOCASE",
        }
        .to_string();
        if direction == "desc" && !order.contains("DESC") {
            order.push_str(" DESC");
        }

        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM songs {}", where_clause),
                params_from_iter(bind.iter()),
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut row_bind = bind.clone();
        row_bind.push(Value::Integer(size));
        row_bind.push(Value::Integer(page * size));

        let sql = format!(
            "SELECT filename, title, artist, album, year, duration, tuning, arrangements, \
             has_lyrics, mtime, format, stem_count FROM songs {} ORDER BY {} LIMIT ? OFFSET ?",
            where_clause, order
        );

        let estd = Self::estd_set_conn(&conn);
        let favs = Self::fav_set_conn(&conn);

        let mut songs = Vec::new();
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map(params_from_iter(row_bind.iter()), |row| {
                Ok(page_row_to_entry(row)?)
            }) {
                for mut entry in rows.flatten() {
                    entry.has_estd = estd.contains(&entry.filename);
                    entry.favorite = favs.contains(&entry.filename);
                    songs.push(entry);
                }
            }
        }

        (songs, total)
    }

    /// Artists grouped by letter with their albums and songs. Returns
    /// `(artists, total_artists)`.
    #[allow(clippy::too_many_arguments)]
    pub fn query_artists(
        &self,
        letter: &str,
        q: &str,
        favorites_only: bool,
        page: i64,
        size: i64,
        format_filter: &str,
    ) -> (Vec<ArtistEntry>, i64) {
        let conn = self.conn.lock().unwrap();

        let mut where_clause = String::from("WHERE title != ''");
        let mut bind: Vec<Value> = Vec::new();
        if favorites_only {
            where_clause.push_str(" AND filename IN (SELECT filename FROM favorites)");
        }
        if !format_filter.is_empty() {
            where_clause.push_str(" AND format = ?");
            bind.push(Value::Text(format_filter.to_string()));
        }
        if letter == "#" {
            where_clause.push_str(" AND artist NOT GLOB '[A-Za-z]*'");
        } else if !letter.is_empty() {
            where_clause.push_str(" AND UPPER(SUBSTR(artist, 1, 1)) = ?");
            bind.push(Value::Text(letter.to_uppercase()));
        }
        if !q.is_empty() {
            where_clause.push_str(
                " AND (title LIKE ? COLLATE NOCASE OR artist LIKE ? COLLATE NOCASE \
                 OR album LIKE ? COLLATE NOCASE)",
            );
            let like = format!("%{}%", q);
            for _ in 0..3 {
                bind.push(Value::Text(like.clone()));
            }
        }

        let total_artists: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT artist COLLATE NOCASE) FROM songs {}",
                    where_clause
                ),
                params_from_iter(bind.iter()),
                |r| r.get(0),
            )
            .unwrap_or(0);

        // Paginated distinct artist names.
        let mut artist_bind = bind.clone();
        artist_bind.push(Value::Integer(size));
        artist_bind.push(Value::Integer(page * size));
        let artist_sql = format!(
            "SELECT DISTINCT artist COLLATE NOCASE as a FROM songs {} ORDER BY a LIMIT ? OFFSET ?",
            where_clause
        );
        let mut artist_names: Vec<String> = Vec::new();
        if let Ok(mut stmt) = conn.prepare(&artist_sql) {
            if let Ok(rows) =
                stmt.query_map(params_from_iter(artist_bind.iter()), |r| r.get::<_, String>(0))
            {
                artist_names.extend(rows.flatten());
            }
        }
        if artist_names.is_empty() {
            return (Vec::new(), total_artists);
        }

        // Songs for just those artists.
        let placeholders = vec!["?"; artist_names.len()].join(",");
        let song_where = format!(
            "{} AND artist COLLATE NOCASE IN ({})",
            where_clause, placeholders
        );
        let mut song_bind = bind.clone();
        for name in &artist_names {
            song_bind.push(Value::Text(name.clone()));
        }
        let song_sql = format!(
            "SELECT filename, title, artist, album, year, duration, tuning, arrangements, \
             has_lyrics, format, stem_count FROM songs {} \
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, title COLLATE NOCASE",
            song_where
        );

        let estd = Self::estd_set_conn(&conn);
        let favs = Self::fav_set_conn(&conn);

        // Group: artist_key -> (display_name, album_key -> (display, songs))
        use indexmap::IndexMap;
        let mut artists: IndexMap<String, (String, IndexMap<String, AlbumEntry>)> = IndexMap::new();

        if let Ok(mut stmt) = conn.prepare(&song_sql) {
            if let Ok(rows) =
                stmt.query_map(params_from_iter(song_bind.iter()), |row| Ok(artist_row_to_entry(row)?))
            {
                for mut entry in rows.flatten() {
                    entry.has_estd = estd.contains(&entry.filename);
                    entry.favorite = favs.contains(&entry.filename);
                    let artist = if entry.artist.is_empty() {
                        "Unknown Artist".to_string()
                    } else {
                        entry.artist.clone()
                    };
                    let album = if entry.album.is_empty() {
                        "Unknown Album".to_string()
                    } else {
                        entry.album.clone()
                    };
                    let akey = artist.to_lowercase();
                    let bkey = album.to_lowercase();
                    let a = artists
                        .entry(akey)
                        .or_insert_with(|| (artist.clone(), IndexMap::new()));
                    let alb = a.1.entry(bkey).or_insert_with(|| AlbumEntry {
                        name: album.clone(),
                        songs: Vec::new(),
                    });
                    alb.songs.push(entry);
                }
            }
        }

        let mut result = Vec::new();
        for (_akey, (name, albums_map)) in artists {
            let albums: Vec<AlbumEntry> = albums_map.into_values().collect();
            let song_count: usize = albums.iter().map(|a| a.songs.len()).sum();
            result.push(ArtistEntry {
                name,
                album_count: albums.len(),
                song_count,
                albums,
            });
        }
        (result, total_artists)
    }

    /// Aggregate stats for the letter bar.
    pub fn query_stats(&self, favorites_only: bool) -> StatsEntry {
        let conn = self.conn.lock().unwrap();
        let filt = if favorites_only {
            " AND filename IN (SELECT filename FROM favorites)"
        } else {
            ""
        };
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM songs WHERE title != ''{}", filt),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let artist_count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT artist) FROM songs WHERE title != ''{}",
                    filt
                ),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut letters: HashMap<String, i64> = HashMap::new();
        let sql = format!(
            "SELECT UPPER(SUBSTR(artist, 1, 1)) as letter, COUNT(DISTINCT artist COLLATE NOCASE) \
             FROM songs WHERE title != ''{} GROUP BY letter",
            filt
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| {
                let letter: Option<String> = r.get(0)?;
                let count: i64 = r.get(1)?;
                Ok((letter, count))
            }) {
                for (letter, count) in rows.flatten() {
                    match letter {
                        Some(l) if l.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) => {
                            *letters.entry(l).or_insert(0) += count;
                        }
                        _ => {
                            *letters.entry("#".to_string()).or_insert(0) += count;
                        }
                    }
                }
            }
        }

        StatsEntry {
            total_songs: total,
            total_artists: artist_count,
            letters,
        }
    }

    // ── Loops ──────────────────────────────────────────────────────────────────

    pub fn loops_for(&self, filename: &str) -> Vec<LoopEntry> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, name, start_time, end_time FROM loops WHERE filename = ? ORDER BY start_time",
        ) {
            if let Ok(rows) = stmt.query_map(params![filename], |r| {
                Ok(LoopEntry {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    start: r.get(2)?,
                    end: r.get(3)?,
                })
            }) {
                out.extend(rows.flatten());
            }
        }
        out
    }

    /// Save a loop; auto-generates a name when blank. Returns the final name.
    pub fn add_loop(&self, filename: &str, name: &str, start: f64, end: f64) -> String {
        let conn = self.conn.lock().unwrap();
        let final_name = if name.trim().is_empty() {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM loops WHERE filename = ?",
                    params![filename],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            format!("Loop {}", count + 1)
        } else {
            name.trim().to_string()
        };
        conn.execute(
            "INSERT INTO loops (filename, name, start_time, end_time) VALUES (?, ?, ?, ?)",
            params![filename, final_name, start, end],
        )
        .ok();
        final_name
    }

    pub fn delete_loop(&self, loop_id: i64) {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM loops WHERE id = ?", params![loop_id]).ok();
    }

    /// Update editable metadata fields (title / artist / album / year).
    pub fn update_song_fields(&self, filename: &str, fields: &[(&str, String)]) -> bool {
        if fields.is_empty() {
            return false;
        }
        let conn = self.conn.lock().unwrap();
        let assignments = fields
            .iter()
            .map(|(k, _)| format!("{} = ?", k))
            .collect::<Vec<_>>()
            .join(", ");
        let mut bind: Vec<Value> = fields.iter().map(|(_, v)| Value::Text(v.clone())).collect();
        bind.push(Value::Text(filename.to_string()));
        let sql = format!("UPDATE songs SET {} WHERE filename = ?", assignments);
        conn.execute(&sql, params_from_iter(bind.iter())).is_ok()
    }
}

// ── Row → struct helpers ──────────────────────────────────────────────────────

fn parse_arrangements(text: Option<String>) -> Vec<ArrangementInfo> {
    text.and_then(|s| {
        if s.is_empty() {
            None
        } else {
            serde_json::from_str::<Vec<ArrangementInfo>>(&s).ok()
        }
    })
    .unwrap_or_default()
}

/// Build a `SongMeta` from a `get()` row (columns 2..=11).
fn row_to_meta(row: &rusqlite::Row) -> rusqlite::Result<SongMeta> {
    Ok(SongMeta {
        title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        artist: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        album: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        year: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        duration: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
        tuning: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        arrangements: parse_arrangements(row.get::<_, Option<String>>(8)?),
        has_lyrics: row.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
        format: row
            .get::<_, Option<String>>(10)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "psarc".to_string()),
        stem_count: row.get::<_, Option<i64>>(11)?.unwrap_or(0),
    })
}

/// Columns: filename,title,artist,album,year,duration,tuning,arrangements,
/// has_lyrics,mtime,format,stem_count
fn page_row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<SongEntry> {
    Ok(SongEntry {
        filename: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        artist: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        album: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        year: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        duration: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
        tuning: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        arrangements: parse_arrangements(row.get::<_, Option<String>>(7)?),
        has_lyrics: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
        mtime: row.get::<_, Option<f64>>(9)?,
        format: row
            .get::<_, Option<String>>(10)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "psarc".to_string()),
        stem_count: row.get::<_, Option<i64>>(11)?.unwrap_or(0),
        has_estd: false,
        favorite: false,
    })
}

/// Columns: filename,title,artist,album,year,duration,tuning,arrangements,
/// has_lyrics,format,stem_count (no mtime — omitted in the artist tree).
fn artist_row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<SongEntry> {
    Ok(SongEntry {
        filename: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        artist: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        album: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        year: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        duration: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
        tuning: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        arrangements: parse_arrangements(row.get::<_, Option<String>>(7)?),
        has_lyrics: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
        mtime: None,
        format: row
            .get::<_, Option<String>>(9)?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "psarc".to_string()),
        stem_count: row.get::<_, Option<i64>>(10)?.unwrap_or(0),
        has_estd: false,
        favorite: false,
    })
}
