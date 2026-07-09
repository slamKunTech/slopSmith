//! Sloppak open song-format loader. Port of `lib/sloppak.py` (read paths).
//!
//! Wave 2 ports format detection, source resolution (zip unpack cache +
//! directory passthrough), manifest loading, and the fast `extract_meta`
//! scanner path. Full `load_song` (which needs `arrangement_from_wire` from
//! [`crate::engine::song`]) lands in Wave 4.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::engine::song::{arrangement_from_wire, Arrangement, Beat, Section, Song};

/// Result of loading a sloppak: the Song + stem descriptors + source dir +
/// raw manifest. Mirrors `LoadedSloppak` (sloppak.py:148-154).
pub struct LoadedSloppak {
    pub song: Song,
    pub stems: Vec<SloppakStem>,
    pub source_dir: PathBuf,
    pub manifest: Value,
}

/// `{"id", "file", "default"}` — normalized stem descriptor.
#[derive(Clone)]
pub struct SloppakStem {
    pub id: String,
    pub file: String,
    pub default: bool,
}

/// Per-sloppak metadata returned by [`extract_meta`]. `tuning_offsets` is the
/// raw per-string array; the caller maps it to a name via
/// [`crate::engine::tunings::tuning_name`].
pub struct SloppakMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub duration: f64,
    pub tuning_offsets: Vec<i64>,
    pub arrangements: Vec<Value>, // [{index, name, notes}]
    pub has_lyrics: bool,
    pub stem_count: i64,
}

/// True if path looks like a sloppak (name ends in `.sloppak`).
pub fn is_sloppak(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().ends_with(".sloppak")
}

// ── Source resolution (zip unpack cache + directory passthrough) ─────────────

/// `filename → (source_dir, mtime, size)`. Mirrors `_source_cache`.
struct SourceCache {
    map: HashMap<String, (PathBuf, f64, u64)>,
}
static SOURCE_CACHE: Mutex<Option<SourceCache>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut SourceCache) -> R) -> R {
    let mut guard = SOURCE_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(|| SourceCache { map: HashMap::new() });
    f(cache)
}

fn safe_id(filename: &str) -> String {
    filename
        .replace('/', "__")
        .replace('\\', "__")
        .replace(' ', "_")
}

fn unpack_zip(zip_path: &Path, dest: &Path) -> std::io::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).ok();
    }
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let outpath = match entry.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut outfile = std::fs::File::create(&outpath)?;
            std::io::copy(&mut entry, &mut outfile)?;
        }
    }
    Ok(())
}

/// Return the on-disk directory containing a sloppak's files. Mirrors
/// `resolve_source_dir` (sloppak.py:64-102).
pub fn resolve_source_dir(
    filename: &str,
    dlc_root: &Path,
    unpack_cache_root: &Path,
) -> std::io::Result<PathBuf> {
    let path = dlc_root.join(filename);
    let meta = std::fs::metadata(&path)?;
    let mtime = file_mtime(&meta);
    let size = meta.len();

    // Check cache.
    let cached_dir = with_cache(|c| {
        if let Some((dir, cm, cs)) = c.map.get(filename) {
            if *cm == mtime && *cs == size && dir.exists() {
                return Some(dir.clone());
            }
        }
        None
    });
    if let Some(dir) = cached_dir {
        return Ok(dir);
    }

    let resolved = if meta.is_dir() {
        path
    } else {
        let dest = unpack_cache_root.join(safe_id(filename));
        unpack_zip(&path, &dest)?;
        dest
    };

    with_cache(|c| {
        c.map.insert(filename.to_string(), (resolved.clone(), mtime, size));
    });
    Ok(resolved)
}

/// Return the cached source dir for a sloppak, if known. Mirrors
/// `get_cached_source_dir`.
pub fn get_cached_source_dir(filename: &str) -> Option<PathBuf> {
    with_cache(|c| c.map.get(filename).map(|(d, _, _)| d.clone()))
}

// ── Manifest loading ─────────────────────────────────────────────────────────

fn read_manifest_from_dir(source_dir: &Path) -> std::io::Result<Value> {
    let mf = source_dir.join("manifest.yaml");
    let mf = if mf.exists() {
        mf
    } else {
        let alt = source_dir.join("manifest.yml");
        if alt.exists() {
            alt
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("manifest.yaml not found in {}", source_dir.display()),
            ));
        }
    };
    let text = std::fs::read_to_string(&mf)?;
    let data: Value = serde_yaml::from_str(&text).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad manifest: {e}"))
    })?;
    if !data.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "manifest.yaml must contain a mapping at the top level",
        ));
    }
    Ok(data)
}

fn read_manifest_from_zip(zip_path: &Path) -> std::io::Result<Value> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for name in ["manifest.yaml", "manifest.yml"] {
        if let Ok(mut entry) = archive.by_name(name) {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)?;
            if let Ok(data) = serde_yaml::from_str::<Value>(&text) {
                if data.is_object() {
                    return Ok(data);
                }
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("manifest.yaml not found in zip {}", zip_path.display()),
    ))
}

/// Return the parsed manifest for a sloppak (dir or zip). Mirrors
/// `load_manifest` (sloppak.py:141-145).
pub fn load_manifest(path: &Path) -> std::io::Result<Value> {
    if path.is_dir() {
        read_manifest_from_dir(path)
    } else {
        read_manifest_from_zip(path)
    }
}

// ── Fast metadata extractor (scanner path) ───────────────────────────────────

/// Best-effort guitar-first tuning for the library index. Mirrors
/// `_tuning_for_meta` (sloppak.py:248-260).
fn tuning_for_meta(arrangements: &[Value]) -> Vec<i64> {
    // First pass: a guitar arrangement (lead/rhythm/combo) with a tuning.
    for entry in arrangements {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(name.as_str(), "lead" | "rhythm" | "combo") {
            if let Some(t) = entry.get("tuning").and_then(|v| v.as_array()) {
                return t.iter().filter_map(|x| x.as_i64()).collect();
            }
        }
    }
    // Fallback: first arrangement with a tuning.
    for entry in arrangements {
        if let Some(t) = entry.get("tuning").and_then(|v| v.as_array()) {
            return t.iter().filter_map(|x| x.as_i64()).collect();
        }
    }
    vec![0, 0, 0, 0, 0, 0]
}

/// Fast metadata for the library scanner. Reads only the manifest. Mirrors
/// `extract_meta` (sloppak.py:263-299).
pub fn extract_meta(path: &Path) -> std::io::Result<SloppakMeta> {
    let manifest = load_manifest(path)?;
    let arr_list = manifest.get("arrangements").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let priority = |name: &str| match name {
        "Lead" => 0,
        "Combo" => 1,
        "Rhythm" => 2,
        "Bass" => 3,
        _ => 99,
    };
    let mut arrangements: Vec<Value> = arr_list
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    entry
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("Arr{i}"))
                });
            serde_json::json!({ "index": i, "name": name, "notes": 0 })
        })
        .collect();
    arrangements.sort_by_key(|a| {
        a.get("name").and_then(|v| v.as_str()).map(priority).unwrap_or(99)
    });
    for (i, a) in arrangements.iter_mut().enumerate() {
        a["index"] = Value::from(i as i64);
    }

    let has_lyrics = manifest.get("lyrics").is_some();
    let tuning_offsets = tuning_for_meta(&arr_list);
    let stem_count = manifest
        .get("stems")
        .and_then(|v| v.as_array())
        .map(|s| s.iter().filter(|s| s.get("id").is_some()).count() as i64)
        .unwrap_or(0);

    let s = |key: &str| -> String {
        manifest.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let year = manifest
        .get("year")
        .map(|v| match v.as_i64() {
            Some(n) => n.to_string(),
            None => v.as_str().unwrap_or("").to_string(),
        })
        .unwrap_or_default();
    let duration = manifest
        .get("duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    Ok(SloppakMeta {
        title: s("title"),
        artist: s("artist"),
        album: s("album"),
        year,
        duration,
        tuning_offsets,
        arrangements,
        has_lyrics,
        stem_count,
    })
}

// ── Full song load ────────────────────────────────────────────────────────────

/// Fully load a sloppak: resolve its source dir, parse manifest + all
/// arrangements + optional lyrics, return a ready-to-stream Song. Mirrors
/// `load_song` (sloppak.py:157-243).
pub fn load_song(
    filename: &str,
    dlc_root: &Path,
    unpack_cache_root: &Path,
) -> std::io::Result<LoadedSloppak> {
    let source_dir = resolve_source_dir(filename, dlc_root, unpack_cache_root)?;
    let manifest = read_manifest_from_dir(&source_dir)?;

    let mut song = Song {
        title: manifest.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        artist: manifest.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        album: manifest.get("album").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        year: manifest.get("year").and_then(|v| v.as_i64()).unwrap_or(0),
        song_length: manifest.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0),
        ..Default::default()
    };

    let arrangements = manifest.get("arrangements").and_then(|v| v.as_array());
    if let Some(arr_list) = arrangements {
        for entry in arr_list {
            let rel = match entry.get("file").and_then(|v| v.as_str()) {
                Some(r) if !r.is_empty() => r,
                _ => continue,
            };
            let arr_path = source_dir.join(rel);
            if !arr_path.exists() {
                continue;
            }
            let data = match std::fs::read_to_string(&arr_path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let parsed: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let mut arr: Arrangement = arrangement_from_wire(&parsed);
            // Manifest overrides take precedence over embedded values.
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    arr.name = name.to_string();
                }
            }
            if let Some(Value::Array(t)) = entry.get("tuning") {
                arr.tuning = t.iter().filter_map(|x| x.as_i64()).collect();
            }
            if let Some(c) = entry.get("capo").and_then(|v| v.as_i64()) {
                arr.capo = c;
            }

            // Beats/sections can live on the arrangement JSON; hoist onto the
            // song the first time we see them.
            if song.beats.is_empty() {
                if let Some(b) = parsed.get("beats").and_then(|v| v.as_array()) {
                    for beat in b {
                        song.beats.push(Beat {
                            time: beat.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0),
                            measure: beat.get("measure").and_then(|v| v.as_i64()).unwrap_or(-1),
                        });
                    }
                }
            }
            if song.sections.is_empty() {
                if let Some(s) = parsed.get("sections").and_then(|v| v.as_array()) {
                    for sec in s {
                        song.sections.push(Section {
                            name: sec.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            number: sec.get("number").and_then(|v| v.as_i64()).unwrap_or(0),
                            start_time: sec
                                .get("time")
                                .or_else(|| sec.get("start_time"))
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0),
                        });
                    }
                }
            }
            song.arrangements.push(arr);
        }
    }

    // Optional shared lyrics file.
    if let Some(lyrics_rel) = manifest.get("lyrics").and_then(|v| v.as_str()) {
        let lyr_path = source_dir.join(lyrics_rel);
        if lyr_path.exists() {
            if let Ok(text) = std::fs::read_to_string(&lyr_path) {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&text) {
                    song.lyrics = arr;
                }
            }
        }
    }

    // Stem descriptors, normalized.
    let mut stems: Vec<SloppakStem> = Vec::new();
    if let Some(s) = manifest.get("stems").and_then(|v| v.as_array()) {
        for entry in s {
            if !entry.is_object() {
                continue;
            }
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() || file.is_empty() {
                continue;
            }
            let default_on = match entry.get("default") {
                Some(Value::String(s)) => !matches!(s.to_lowercase().as_str(), "off" | "false" | "0" | "no"),
                Some(Value::Bool(b)) => *b,
                _ => true,
            };
            stems.push(SloppakStem { id, file, default: default_on });
        }
    }

    Ok(LoadedSloppak {
        song,
        stems,
        source_dir,
        manifest,
    })
}

/// Cross-platform mtime → unix seconds (f64).
fn file_mtime(meta: &std::fs::Metadata) -> f64 {
    use std::time::SystemTime;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
