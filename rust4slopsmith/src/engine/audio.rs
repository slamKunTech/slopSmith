//! Audio extraction/conversion for Rocksmith CDLC. Port of `lib/audio.py` —
//! pure subprocess orchestration (ffmpeg / vgmstream-cli / ww2ogg), no binary
//! parsing of its own.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `shutil.which` — find a binary on PATH.
fn which(cmd: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn ffmpeg_cmd() -> Option<PathBuf> {
    which("ffmpeg")
}

/// Encode a WAV (or any ffmpeg-decodable audio) to Ogg/Vorbis. Prefers
/// `libvorbis`; falls back to the experimental native `vorbis` encoder when
/// the ffmpeg build doesn't link libvorbis (e.g. stock Homebrew). Mirrors
/// `encode_wav_to_ogg` (audio.py:19-56).
pub fn encode_wav_to_ogg(wav_path: &Path, ogg_path: &Path, quality: i32, ffmpeg: Option<&Path>) -> anyhow::Result<()> {
    let ff = ffmpeg
        .map(|p| p.to_path_buf())
        .or_else(|| ffmpeg_cmd())
        .unwrap_or_else(|| PathBuf::from("ffmpeg"));
    if let Some(parent) = ogg_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let q = quality.to_string();

    // Primary: libvorbis.
    let r = Command::new(&ff)
        .args(["-y", "-loglevel", "error"])
        .arg("-i").arg(wav_path)
        .args(["-c:a", "libvorbis"])
        .arg("-q:a").arg(&q)
        .arg(ogg_path)
        .output();
    if let Ok(out) = &r {
        if out.status.success()
            && ogg_path.exists()
            && ogg_path.metadata().map(|m| m.len() > 0).unwrap_or(false)
        {
            return Ok(());
        }
    }

    // Fallback: native vorbis (experimental → -strict -2).
    if ogg_path.exists() && ogg_path.metadata().map(|m| m.len() == 0).unwrap_or(false) {
        std::fs::remove_file(ogg_path).ok();
    }
    let r2 = Command::new(&ff)
        .args(["-y", "-loglevel", "error"])
        .arg("-i").arg(wav_path)
        .args(["-c:a", "vorbis", "-strict", "-2"])
        .arg("-q:a").arg(&q)
        .arg(ogg_path)
        .output()?;
    let ok = r2.status.success()
        && ogg_path.exists()
        && ogg_path.metadata().map(|m| m.len() >= 100).unwrap_or(false);
    if !ok {
        let stderr = String::from_utf8_lossy(&r2.stderr);
        anyhow::bail!(
            "ffmpeg OGG/Vorbis encode failed for {}: {}",
            wav_path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            stderr.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>()
        );
    }
    Ok(())
}

/// Find WEM audio files, sorted largest first (full song before preview).
/// Mirrors `find_wem_files` (audio.py:59-63).
pub fn find_wem_files(extracted_dir: &Path) -> Vec<PathBuf> {
    let mut wem: Vec<(u64, PathBuf)> = Vec::new();
    for e in walkdir::WalkDir::new(extracted_dir).into_iter().filter_map(|e| e.ok()) {
        if e.path().extension().and_then(|x| x.to_str()) == Some("wem") {
            if let Ok(meta) = e.metadata() {
                wem.push((meta.len(), e.path().to_path_buf()));
            }
        }
    }
    wem.sort_by(|a, b| b.0.cmp(&a.0));
    wem.into_iter().map(|(_, p)| p).collect()
}

/// Convert a WEM file to a playable format. Returns the converted file path.
/// Tries vgmstream-cli → WAV → MP3, then ffmpeg, then ww2ogg. Mirrors
/// `convert_wem` (audio.py:66-121).
pub fn convert_wem(wem_path: &Path, output_base: &Path) -> anyhow::Result<PathBuf> {
    let wav = output_base.with_extension("wav");
    let mp3 = output_base.with_extension("mp3");
    let ogg = output_base.with_extension("ogg");

    if let Some(vgm) = which("vgmstream-cli") {
        let r = Command::new(&vgm).arg("-o").arg(&wav).arg(wem_path).output();
        if let Ok(out) = &r {
            if out.status.success() && wav.exists() && wav.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                if let Some(ff) = ffmpeg_cmd() {
                    let r2 = Command::new(&ff).args(["-y", "-i"]).arg(&wav).args(["-b:a", "192k"]).arg(&mp3).output();
                    if let Ok(out2) = &r2 {
                        if out2.status.success() && mp3.exists() {
                            std::fs::remove_file(&wav).ok();
                            return Ok(mp3);
                        }
                    }
                }
                return Ok(wav);
            }
        }
    }

    if let Some(ff) = ffmpeg_cmd() {
        let r = Command::new(&ff).args(["-y", "-i"]).arg(wem_path).args(["-b:a", "192k"]).arg(&mp3).output();
        if let Ok(out) = &r {
            if out.status.success() && mp3.exists() && mp3.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                return Ok(mp3);
            }
        }
        let r = Command::new(&ff).args(["-y", "-i"]).arg(wem_path).arg(&wav).output();
        if let Ok(out) = &r {
            if out.status.success() && wav.exists() && wav.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                return Ok(wav);
            }
        }
    }

    if let Some(ww) = which("ww2ogg") {
        let r = Command::new(&ww).arg(wem_path).arg("-o").arg(&ogg).output();
        if let Ok(out) = &r {
            if out.status.success() && ogg.exists() && ogg.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                return Ok(ogg);
            }
        }
    }

    anyhow::bail!(
        "No WEM audio decoder found. Install vgmstream-cli:\n  \
         Manjaro/Arch: yay -S vgmstream-cli-bin\n  \
         Or build from: github.com/vgmstream/vgmstream"
    )
}
