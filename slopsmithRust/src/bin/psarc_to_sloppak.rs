//! Convert a Rocksmith PSARC into a `.sloppak` package.
//!
//! Usage:
//!     psarc-to-sloppak path/to/song.psarc [-o OUT] [--dir]
//!
//! Produces a single-stem sloppak (stems/full.ogg with default=on). Run
//! `split-stems` afterwards to replace it with real stems.
//!
//! - Default output form is a zipped `.sloppak` file.
//! - Pass `--dir` to emit the directory form instead (for hand-editing).
//! - Pass `-o PATH` to override the default output location.
//!
//! Reuses the existing slopsmith library code — no format logic is duplicated:
//!   * slopsmith::patcher — unpack_psarc
//!   * slopsmith::song     — load_song + arrangement_to_wire

use std::error::Error;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use serde::Serialize;
use serde_json::{json, Value};
use walkdir::WalkDir;
use zip::write::FileOptions;

use slopsmith::patcher::unpack_psarc;
use slopsmith::song::{arrangement_to_wire, load_song};

type Res<T> = Result<T, Box<dyn Error>>;

// ── CLI ──────────────────────────────────────────────────────────────────────

/// Convert a PSARC to a .sloppak
#[derive(Parser, Debug)]
#[command(name = "psarc-to-sloppak", about = "Convert a PSARC to a .sloppak")]
struct Args {
    /// input .psarc file
    psarc: PathBuf,

    /// output path (default: alongside the PSARC)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// emit directory form instead of a zip
    #[arg(long = "dir")]
    dir: bool,
}

// ── Manifest data model (serialized to YAML, field order preserved) ──────────

#[derive(Serialize)]
struct StemEntry {
    id: String,
    file: String,
    default: String,
}

#[derive(Serialize)]
struct ArrEntry {
    id: String,
    name: String,
    file: String,
    tuning: Vec<i32>,
    capo: i32,
}

#[derive(Serialize)]
struct Manifest {
    title: String,
    artist: String,
    album: String,
    year: i64,
    duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover: Option<String>,
    stems: Vec<StemEntry>,
    arrangements: Vec<ArrEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lyrics: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Round to 3 decimal places (matches Python's `round(x, 3)` for wire output).
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Make a string filesystem-safe for use as a sloppak stem.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_us = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "song".to_string()
    } else {
        trimmed
    }
}

/// Stable lowercase id for an arrangement, deduped within a song.
fn arrangement_id(name: &str, used: &mut Vec<String>) -> String {
    let lower = name.to_lowercase();
    let mut base = String::with_capacity(lower.len());
    let mut prev_us = false;
    for ch in lower.chars() {
        if ch.is_ascii_digit() || ch.is_ascii_lowercase() {
            base.push(ch);
            prev_us = false;
        } else if !prev_us {
            base.push('_');
            prev_us = true;
        }
    }
    let mut base = base.trim_matches('_').to_string();
    if base.is_empty() {
        base = "arr".to_string();
    }

    let mut candidate = base.clone();
    let mut i = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}{i}");
        i += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// Return the path to an executable on PATH (like `shutil.which`).
fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Find WEM audio files, sorted largest first (full song before preview).
fn find_wem_files(extracted_dir: &Path) -> Vec<PathBuf> {
    let mut wems: Vec<(u64, PathBuf)> = WalkDir::new(extracted_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wem"))
                .unwrap_or(false)
        })
        .map(|e| {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            (size, e.into_path())
        })
        .collect();
    wems.sort_by(|a, b| b.0.cmp(&a.0));
    wems.into_iter().map(|(_, p)| p).collect()
}

/// Decode a WEM to OGG/Vorbis via vgmstream-cli → WAV → ffmpeg.
fn wem_to_ogg(wem_path: &Path, out_ogg: &Path) -> Res<()> {
    let vgmstream = which("vgmstream-cli")
        .ok_or("vgmstream-cli not found on PATH (needed to decode WEM)")?;
    let ffmpeg = which("ffmpeg").ok_or("ffmpeg not found on PATH (needed to encode OGG)")?;

    let td = tempfile::Builder::new().prefix("psarc2slop_").tempdir()?;
    let wav = td.path().join("full.wav");

    let r = Command::new(&vgmstream)
        .arg("-o")
        .arg(&wav)
        .arg(wem_path)
        .output()?;
    let wav_ok = wav.exists() && fs::metadata(&wav).map(|m| m.len()).unwrap_or(0) >= 100;
    if !r.status.success() || !wav_ok {
        return Err(format!(
            "vgmstream-cli failed for {}: {}",
            wem_path.display(),
            String::from_utf8_lossy(&r.stderr)
        )
        .into());
    }

    if let Some(parent) = out_ogg.parent() {
        fs::create_dir_all(parent)?;
    }
    let r2 = Command::new(&ffmpeg)
        .args(["-y", "-i"])
        .arg(&wav)
        .args(["-c:a", "libvorbis", "-q:a", "5"])
        .arg(out_ogg)
        .output()?;
    let ogg_ok = out_ogg.exists() && fs::metadata(out_ogg).map(|m| m.len()).unwrap_or(0) >= 100;
    if !r2.status.success() || !ogg_ok {
        return Err(format!(
            "ffmpeg OGG encode failed: {}",
            String::from_utf8_lossy(&r2.stderr)
        )
        .into());
    }
    Ok(())
}

/// Collect every `*.xml` under `dir`, sorted by path (matches Python rglob+sorted).
fn collect_xml_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut xmls: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("xml"))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect();
    xmls.sort();
    xmls
}

/// Return compact-wire lyric tokens from any vocals XML in the extract.
fn parse_lyrics(extracted_dir: &Path) -> Vec<Value> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    for xml_path in collect_xml_sorted(extracted_dir) {
        let text = match fs::read_to_string(&xml_path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let mut reader = Reader::from_str(&text);
        reader.trim_text(true);

        // Determine whether the root element is <vocals>, and if so collect
        // every <vocal> element's attributes.
        let mut buf = Vec::new();
        let mut root_checked = false;
        let mut is_vocals = false;
        let mut out: Vec<Value> = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let name = e.name();
                    let tag = String::from_utf8_lossy(name.as_ref()).to_string();
                    if !root_checked {
                        root_checked = true;
                        is_vocals = tag == "vocals";
                        if !is_vocals {
                            break;
                        }
                        // A <vocals> root may itself be an empty element.
                        continue;
                    }
                    if is_vocals && tag == "vocal" {
                        let mut t = 0.0f64;
                        let mut d = 0.0f64;
                        let mut w = String::new();
                        for attr in e.attributes().flatten() {
                            let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                            let val = attr
                                .unescape_value()
                                .map(|c| c.into_owned())
                                .unwrap_or_default();
                            match key.as_str() {
                                "time" => t = val.parse().unwrap_or(0.0),
                                "length" => d = val.parse().unwrap_or(0.0),
                                "lyric" => w = val,
                                _ => {}
                            }
                        }
                        out.push(json!({
                            "t": round3(t),
                            "d": round3(d),
                            "w": w,
                        }));
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        if is_vocals {
            return out;
        }
    }
    Vec::new()
}

/// Convert the largest DDS album art into `out_jpg`. Returns true on success.
fn extract_cover(extracted_dir: &Path, out_jpg: &Path) -> bool {
    let mut dds_files: Vec<(u64, PathBuf)> = WalkDir::new(extracted_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("dds"))
                .unwrap_or(false)
        })
        .map(|e| {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            (size, e.into_path())
        })
        .collect();
    if dds_files.is_empty() {
        return false;
    }
    dds_files.sort_by(|a, b| b.0.cmp(&a.0));

    let src = &dds_files[0].1;
    let img = match image::open(src) {
        Ok(i) => i.to_rgb8(),
        Err(e) => {
            eprintln!("[warn] cover art extraction failed: {e}");
            return false;
        }
    };
    if let Some(parent) = out_jpg.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let file = match fs::File::create(out_jpg) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[warn] cover art extraction failed: {e}");
            return false;
        }
    };
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 88);
    match encoder.encode_image(&img) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("[warn] cover art extraction failed: {e}");
            false
        }
    }
}

/// Write `src_dir`'s contents into `out_zip` (flat at root, not nested).
fn zip_dir(src_dir: &Path, out_zip: &Path) -> Res<()> {
    if let Some(parent) = out_zip.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(out_zip)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for entry in WalkDir::new(src_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let rel = path.strip_prefix(src_dir)?;
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            zip.start_file(name, options)?;
            let data = fs::read(path)?;
            zip.write_all(&data)?;
        }
    }
    zip.finish()?;
    Ok(())
}

// ── Main conversion ──────────────────────────────────────────────────────────

fn convert(psarc_path: &Path, out_path: &Path, as_dir: bool) -> Res<PathBuf> {
    let psarc_name = psarc_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    println!("[*] Unpacking {psarc_name}");

    let tmp_extract = tempfile::Builder::new()
        .prefix("psarc2slop_extract_")
        .tempdir()?;
    let work = tempfile::Builder::new()
        .prefix("psarc2slop_work_")
        .tempdir()?;
    let tmp_extract_path = tmp_extract.path();
    let work_dir = work.path();

    unpack_psarc(psarc_path, tmp_extract_path)?;

    println!("[*] Parsing song data");
    let song = load_song(tmp_extract_path)?;
    if song.arrangements.is_empty() {
        return Err("no playable arrangements found in PSARC".into());
    }

    // Arrangements → JSON files.
    let mut used_ids: Vec<String> = Vec::new();
    let mut arr_manifest: Vec<ArrEntry> = Vec::new();
    let mut first = true;
    for arr in &song.arrangements {
        let aid = arrangement_id(&arr.name, &mut used_ids);
        let mut wire = arrangement_to_wire(arr);

        // Embed beats/sections on the first arrangement so the sloppak loader
        // picks them up onto the Song object.
        if first {
            if let Value::Object(ref mut map) = wire {
                let beats: Vec<Value> = song
                    .beats
                    .iter()
                    .map(|b| json!({"time": round3(b.time), "measure": b.measure}))
                    .collect();
                let sections: Vec<Value> = song
                    .sections
                    .iter()
                    .map(|s| {
                        json!({"name": s.name, "number": s.number, "time": round3(s.start_time)})
                    })
                    .collect();
                map.insert("beats".to_string(), Value::Array(beats));
                map.insert("sections".to_string(), Value::Array(sections));
            }
            first = false;
        }

        let arr_file = work_dir.join("arrangements").join(format!("{aid}.json"));
        if let Some(parent) = arr_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&arr_file, serde_json::to_string(&wire)?)?;

        arr_manifest.push(ArrEntry {
            id: aid.clone(),
            name: arr.name.clone(),
            file: format!("arrangements/{aid}.json"),
            tuning: arr.tuning.clone(),
            capo: arr.capo,
        });
    }

    // Audio: biggest WEM → stems/full.ogg.
    println!("[*] Converting audio (WEM → OGG)");
    let wems = find_wem_files(tmp_extract_path);
    if wems.is_empty() {
        return Err("no WEM audio found in PSARC".into());
    }
    wem_to_ogg(&wems[0], &work_dir.join("stems").join("full.ogg"))?;

    let stems_manifest = vec![StemEntry {
        id: "full".to_string(),
        file: "stems/full.ogg".to_string(),
        default: "on".to_string(),
    }];

    // Lyrics.
    let lyrics = parse_lyrics(tmp_extract_path);
    let mut lyrics_rel: Option<String> = None;
    if !lyrics.is_empty() {
        println!("[*] Writing {} lyric tokens", lyrics.len());
        fs::write(
            work_dir.join("lyrics.json"),
            serde_json::to_string(&lyrics)?,
        )?;
        lyrics_rel = Some("lyrics.json".to_string());
    }

    // Cover art.
    let mut cover_rel: Option<String> = None;
    if extract_cover(tmp_extract_path, &work_dir.join("cover.jpg")) {
        cover_rel = Some("cover.jpg".to_string());
        println!("[*] Extracted cover art");
    }

    // Manifest.
    let default_title = || {
        psarc_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let title = if song.title.is_empty() {
        default_title()
    } else {
        song.title.clone()
    };
    let manifest = Manifest {
        title,
        artist: song.artist.clone(),
        album: song.album.clone(),
        year: song.year as i64,
        duration: round3(song.song_length),
        cover: cover_rel,
        stems: stems_manifest,
        arrangements: arr_manifest,
        lyrics: lyrics_rel,
    };

    fs::write(
        work_dir.join("manifest.yaml"),
        serde_yaml::to_string(&manifest)?,
    )?;

    // Emit output.
    if as_dir {
        if out_path.exists() {
            fs::remove_dir_all(out_path)?;
        }
        copy_dir_all(work_dir, out_path)?;
    } else {
        zip_dir(work_dir, out_path)?;
    }

    Ok(out_path.to_path_buf())
}

/// Recursively copy a directory tree (equivalent to shutil.copytree).
fn copy_dir_all(src: &Path, dst: &Path) -> Res<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn run() -> Result<i32, Box<dyn Error>> {
    let args = Args::parse();

    let psarc = args.psarc.clone();
    if !psarc.exists() {
        eprintln!("error: {} does not exist", psarc.display());
        return Ok(2);
    }

    let raw_stem = psarc
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
        .replace("_p", "")
        .replace("_m", "");
    let stem = sanitize(&raw_stem);
    let default_name = format!("{stem}.sloppak");

    let mut out = match &args.output {
        Some(o) => o.clone(),
        None => psarc
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&default_name),
    };

    // If user passed a directory as -o, drop the file inside it.
    if out.exists() && out.is_dir() && !args.dir {
        out = out.join(&default_name);
    }

    match convert(&psarc, &out, args.dir) {
        Ok(result) => {
            println!("[✓] Wrote {}", result.display());
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
