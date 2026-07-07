//! Split a sloppak's full-mix stem into per-instrument stems via Demucs.
//!
//! Usage:
//!     split-stems path/to/song.sloppak [--model htdemucs_6s]
//!
//! Takes a sloppak whose only stem is `stems/full.ogg`, runs Demucs to split it
//! into per-instrument stems, replaces `full.ogg` with the results, and rewrites
//! `manifest.yaml`.
//!
//! Accepts both forms:
//! - Directory-form sloppak: edited in place.
//! - Zip-form sloppak:       unpacked to a temp dir, edited, re-zipped atomically.
//!
//! Requires `demucs` to be importable by the Python interpreter on PATH:
//!     pip install demucs
//!
//! Default model is `htdemucs_6s` which produces 6 stems:
//!     vocals, drums, bass, guitar, piano, other
//! Override with `--model htdemucs` (4 stems: vocals, drums, bass, other).

use std::error::Error;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use serde::Serialize;
use serde_yaml::Value as YamlValue;
use walkdir::WalkDir;
use zip::write::FileOptions;

type Res<T> = Result<T, Box<dyn Error>>;

// Demucs outputs WAVs named {stem}.wav in a per-track subfolder. We re-encode
// them to OGG/Vorbis with ffmpeg to match the rest of the sloppak format.
const STEM_ORDER: [&str; 6] = ["guitar", "bass", "drums", "vocals", "piano", "other"];

// ── CLI ──────────────────────────────────────────────────────────────────────

/// Split a sloppak's full-mix into per-instrument stems via Demucs
#[derive(Parser, Debug)]
#[command(
    name = "split-stems",
    about = "Split a sloppak's full-mix into per-instrument stems via Demucs"
)]
struct Args {
    /// input .sloppak (file or directory)
    sloppak: PathBuf,

    /// demucs model (default: htdemucs_6s = 6 stems inc. guitar + piano;
    /// htdemucs = 4 stems without guitar)
    #[arg(long, default_value = "htdemucs_6s")]
    model: String,
}

#[derive(Serialize, Clone)]
struct StemEntry {
    id: String,
    file: String,
    default: String,
}

// ── Python interpreter discovery ─────────────────────────────────────────────

/// Return an executable path on PATH, if present.
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

/// Locate the Python interpreter to drive Demucs (equivalent to sys.executable).
fn python_exe() -> Option<PathBuf> {
    if let Some(env) = std::env::var_os("PYTHON") {
        let p = PathBuf::from(env);
        if p.is_file() {
            return Some(p);
        }
    }
    which("python3").or_else(|| which("python"))
}

/// Check that `demucs` is importable by the interpreter.
fn demucs_available(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", "import demucs"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Demucs pipeline ──────────────────────────────────────────────────────────

/// Run demucs on `full_ogg`, return the directory containing the split stems.
fn run_demucs(python: &Path, full_ogg: &Path, out_dir: &Path, model: &str) -> Res<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let cmd_display = format!(
        "{} -m demucs -n {} -o {} {}",
        python.display(),
        model,
        out_dir.display(),
        full_ogg.display()
    );
    println!("[*] Running: {cmd_display}");

    let status = Command::new(python)
        .args(["-m", "demucs", "-n", model, "-o"])
        .arg(out_dir)
        .arg(full_ogg)
        .status()?;
    if !status.success() {
        return Err(format!(
            "demucs exited with code {}",
            status.code().unwrap_or(-1)
        )
        .into());
    }

    // Demucs writes to {out_dir}/{model}/{track_stem}/*.wav
    let track_stem = full_ogg
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut result_dir = out_dir.join(model).join(&track_stem);
    if !result_dir.exists() {
        // Some demucs versions use the track stem with spaces replaced, etc.
        let model_dir = out_dir.join(model);
        let candidates: Vec<PathBuf> = if model_dir.exists() {
            fs::read_dir(&model_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect()
        } else {
            Vec::new()
        };
        if candidates.len() == 1 && candidates[0].is_dir() {
            result_dir = candidates[0].clone();
        } else {
            return Err(format!(
                "demucs output dir not found under {}",
                out_dir.join(model).display()
            )
            .into());
        }
    }
    Ok(result_dir)
}

/// Re-encode a WAV stem to OGG/Vorbis via ffmpeg.
fn encode_ogg(wav_path: &Path, ogg_path: &Path) -> Res<()> {
    if let Some(parent) = ogg_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let r = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(wav_path)
        .args(["-c:a", "libvorbis", "-q:a", "5"])
        .arg(ogg_path)
        .output()?;
    if !r.status.success() || !ogg_path.exists() {
        let name = wav_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        return Err(format!(
            "ffmpeg OGG encode failed for {name}: {}",
            String::from_utf8_lossy(&r.stderr)
        )
        .into());
    }
    Ok(())
}

/// Rewrite `manifest.yaml`'s `stems` key with the new per-instrument entries,
/// preserving the rest of the manifest and its field order.
fn rewrite_manifest(source_dir: &Path, new_stems: &[StemEntry]) -> Res<()> {
    let mut mf = source_dir.join("manifest.yaml");
    if !mf.exists() {
        mf = source_dir.join("manifest.yml");
    }
    if !mf.exists() {
        return Err(format!("manifest.yaml not found in {}", source_dir.display()).into());
    }

    let text = fs::read_to_string(&mf)?;
    let mut data: YamlValue = serde_yaml::from_str(&text).unwrap_or(YamlValue::Null);
    if !matches!(data, YamlValue::Mapping(_)) {
        data = YamlValue::Mapping(serde_yaml::Mapping::new());
    }
    if let YamlValue::Mapping(ref mut map) = data {
        map.insert(
            YamlValue::String("stems".to_string()),
            serde_yaml::to_value(new_stems)?,
        );
    }

    fs::write(&mf, serde_yaml::to_string(&data)?)?;
    Ok(())
}

/// Collect `*.wav` files in a directory, sorted by name.
fn collect_wavs_sorted(dir: &Path) -> Res<Vec<PathBuf>> {
    let mut wavs: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false)
        })
        .collect();
    wavs.sort();
    Ok(wavs)
}

/// Do the split-and-rewrite work inside an unpacked sloppak directory.
fn split_in_dir(python: &Path, source_dir: &Path, model: &str) -> Res<()> {
    let full_ogg = source_dir.join("stems").join("full.ogg");
    if !full_ogg.exists() {
        return Err(format!(
            "{} not found — nothing to split. Run psarc-to-sloppak first, or manually add stems/full.ogg.",
            full_ogg.display()
        )
        .into());
    }

    let td = tempfile::Builder::new().prefix("split_stems_").tempdir()?;
    let result_dir = run_demucs(python, &full_ogg, td.path(), model)?;

    println!("[*] Encoding split stems to OGG");
    let stems_dir = source_dir.join("stems");
    let mut produced: Vec<StemEntry> = Vec::new();
    for wav in collect_wavs_sorted(&result_dir)? {
        let name = wav
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default(); // e.g. "guitar", "vocals"
        let out_ogg = stems_dir.join(format!("{name}.ogg"));
        encode_ogg(&wav, &out_ogg)?;
        produced.push(StemEntry {
            id: name.clone(),
            file: format!("stems/{name}.ogg"),
            default: "on".to_string(),
        });
    }

    if produced.is_empty() {
        return Err("demucs produced no output stems".into());
    }

    // Sort in a sensible mixer order, with unknown names at the end.
    produced.sort_by(|a, b| {
        let ka = STEM_ORDER.iter().position(|s| *s == a.id);
        let kb = STEM_ORDER.iter().position(|s| *s == b.id);
        let ka = (ka.unwrap_or(STEM_ORDER.len()), a.id.clone());
        let kb = (kb.unwrap_or(STEM_ORDER.len()), b.id.clone());
        ka.cmp(&kb)
    });

    // Remove the now-redundant full mix and update the manifest.
    if full_ogg.exists() {
        let _ = fs::remove_file(&full_ogg);
    }
    rewrite_manifest(source_dir, &produced)?;

    println!(
        "[✓] {} stems written to {}",
        produced.len(),
        stems_dir.display()
    );
    for s in &produced {
        println!("    - {}", s.id);
    }
    Ok(())
}

/// Extract a zip archive into `dest`.
fn unzip_into(zip_path: &Path, dest: &Path) -> Res<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let out_path = match entry.enclosed_name() {
            Some(p) => dest.join(p),
            None => continue,
        };
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    Ok(())
}

/// Write `src_dir`'s contents into `out_zip` (flat at root, not nested).
fn zip_dir(src_dir: &Path, out_zip: &Path) -> Res<()> {
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

fn split(python: &Path, sloppak_path: &Path, model: &str) -> Res<()> {
    if sloppak_path.is_dir() {
        split_in_dir(python, sloppak_path, model)?;
        return Ok(());
    }

    // Zip form: unpack, split, rezip in place (atomic via temp file).
    let name = sloppak_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    println!("[*] Unpacking {name}");

    let td = tempfile::Builder::new()
        .prefix("split_stems_zip_")
        .tempdir()?;
    let work = td.path().join("sloppak");
    fs::create_dir_all(&work)?;
    unzip_into(sloppak_path, &work)?;

    split_in_dir(python, &work, model)?;

    println!("[*] Repacking {name}");
    let mut tmp_out = sloppak_path.as_os_str().to_os_string();
    tmp_out.push(".tmp");
    let tmp_out = PathBuf::from(tmp_out);
    zip_dir(&work, &tmp_out)?;
    fs::rename(&tmp_out, sloppak_path)?;
    Ok(())
}

fn run() -> Res<i32> {
    let args = Args::parse();

    if !args.sloppak.exists() {
        eprintln!("error: {} does not exist", args.sloppak.display());
        return Ok(2);
    }

    let python = match python_exe() {
        Some(p) => p,
        None => {
            eprintln!("error: no Python interpreter found on PATH (needed to run demucs)");
            return Ok(2);
        }
    };

    if !demucs_available(&python) {
        eprintln!("error: demucs not installed. Run: pip install demucs");
        return Ok(2);
    }

    match split(&python, &args.sloppak, &args.model) {
        Ok(()) => Ok(0),
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
