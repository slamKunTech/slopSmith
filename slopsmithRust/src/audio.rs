//! Audio extraction and conversion for Rocksmith CDLC.
//!
//! Translated from `lib/audio.py`. Finds WEM audio files and converts them to
//! a browser-playable format by shelling out to external decoders
//! (vgmstream-cli, ffmpeg, ww2ogg).

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use walkdir::WalkDir;

/// Manual PATH search for an executable (stand-in for `shutil.which`).
fn which(cmd: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Return true if `path` exists and is a non-empty file.
fn nonempty(path: &str) -> bool {
    fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

/// Run a command, returning true only if it launched and exited successfully.
/// Output is captured (and discarded) to keep stdout/stderr quiet.
fn run_ok(prog: &str, args: &[&str]) -> bool {
    match Command::new(prog).args(args).output() {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Find WEM audio files, sorted largest first (full song before preview).
pub fn find_wem_files(extracted_dir: &str) -> Vec<String> {
    let mut wem_files: Vec<PathBuf> = WalkDir::new(extracted_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wem"))
                .unwrap_or(false)
        })
        .collect();

    // Sort by file size, largest first.
    wem_files.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| m.len()).unwrap_or(0)));

    wem_files
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Convert a WEM file to a playable format.
///
/// Returns the path to the converted audio file. Tries, in order:
/// vgmstream-cli -> WAV (-> MP3 via ffmpeg), then ffmpeg directly, then ww2ogg.
pub fn convert_wem(wem_path: &str, output_base: &str) -> Result<String, Box<dyn Error>> {
    // Try vgmstream-cli -> WAV -> MP3 (best browser compatibility).
    if which("vgmstream-cli").is_some() {
        let wav = format!("{}.wav", output_base);
        if run_ok("vgmstream-cli", &["-o", &wav, wem_path]) && nonempty(&wav) {
            if which("ffmpeg").is_some() {
                let mp3 = format!("{}.mp3", output_base);
                if run_ok("ffmpeg", &["-y", "-i", &wav, "-b:a", "192k", &mp3])
                    && std::path::Path::new(&mp3).exists()
                {
                    let _ = fs::remove_file(&wav);
                    return Ok(mp3);
                }
            }
            return Ok(wav);
        }
    }

    // Try ffmpeg directly (some builds handle Wwise).
    if which("ffmpeg").is_some() {
        let mp3 = format!("{}.mp3", output_base);
        if run_ok("ffmpeg", &["-y", "-i", wem_path, "-b:a", "192k", &mp3]) && nonempty(&mp3) {
            return Ok(mp3);
        }

        // Try WAV output as fallback.
        let wav = format!("{}.wav", output_base);
        if run_ok("ffmpeg", &["-y", "-i", wem_path, &wav]) && nonempty(&wav) {
            return Ok(wav);
        }
    }

    // Try ww2ogg.
    if which("ww2ogg").is_some() {
        let ogg = format!("{}.ogg", output_base);
        if run_ok("ww2ogg", &[wem_path, "-o", &ogg]) && nonempty(&ogg) {
            return Ok(ogg);
        }
    }

    Err("No WEM audio decoder found. Install vgmstream-cli:\n\
         \x20 Manjaro/Arch:  yay -S vgmstream-cli-bin\n\
         \x20 Or build from: github.com/vgmstream/vgmstream"
        .into())
}
