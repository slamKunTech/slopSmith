//! Extract cache for PSARC highway sessions. Mirrors `_extract_cache` /
//! `_get_or_extract` (server.py:1174-1205): unpacked PSARC temp dirs + parsed
//! `Song`s are cached for 5 minutes (max 10 entries) so switching arrangements
//! doesn't re-extract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::psarc;
use crate::engine::song::{load_song, Song};

pub struct ExtractEntry {
    pub tmp_dir: PathBuf,
    pub song: Arc<Song>,
    pub ts: Instant,
}

/// 5-minute cache TTL.
const TTL: Duration = Duration::from_secs(300);
/// Max cached extractions.
const MAX_ENTRIES: usize = 10;

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Return a cached extraction or extract fresh. Mirrors `_get_or_extract`.
/// Returns `(tmp_dir, song, is_new)`.
pub fn get_or_extract(
    cache: &Mutex<HashMap<String, ExtractEntry>>,
    filename: &str,
    psarc_path: &Path,
) -> std::io::Result<(PathBuf, Arc<Song>, bool)> {
    // Check cache (hold lock only for the check).
    {
        let mut map = cache.lock().unwrap();
        if let Some(entry) = map.get(filename) {
            if entry.tmp_dir.exists() && entry.ts.elapsed() < TTL {
                return Ok((entry.tmp_dir.clone(), Arc::clone(&entry.song), false));
            }
            // Stale — drop the entry and clean its dir.
            let removed = map.remove(filename);
            drop(map);
            if let Some(r) = removed {
                std::fs::remove_dir_all(&r.tmp_dir).ok();
            }
        }
    }

    // Extract fresh (no lock held during the long unpack + parse).
    let tmp = mkdtemp_rs();
    psarc::unpack_psarc(psarc_path, &tmp)?;
    let song = load_song(&tmp);
    let song = Arc::new(song);

    {
        let mut map = cache.lock().unwrap();
        // Evict oldest if over capacity.
        if map.len() >= MAX_ENTRIES {
            if let Some((oldest_key, _)) = map
                .iter()
                .min_by_key(|(_, e)| e.ts)
                .map(|(k, _)| (k.clone(), ()))
            {
                if let Some(removed) = map.remove(&oldest_key) {
                    drop(map);
                    std::fs::remove_dir_all(&removed.tmp_dir).ok();
                } else {
                    drop(map);
                }
            } else {
                drop(map);
            }
        } else {
            drop(map);
        }
        let mut map = cache.lock().unwrap();
        map.insert(
            filename.to_string(),
            ExtractEntry {
                tmp_dir: tmp.clone(),
                song: Arc::clone(&song),
                ts: Instant::now(),
            },
        );
    }

    Ok((tmp, song, true))
}

fn mkdtemp_rs() -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("rs_web_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).ok();
    dir
}
