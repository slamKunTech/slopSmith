//! Pitch-shift a CDLC's audio to E standard tuning.
//!
//! Translated from `lib/retune.py`.
//!
//! Only works for uniform tunings (all strings shifted by the same amount),
//! e.g. Eb standard (-1), D standard (-2), C# standard (-3).

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use regex::Regex;
use tempfile::Builder;
use walkdir::WalkDir;

use crate::patcher::{pack_psarc, unpack_psarc};

type BoxErr = Box<dyn Error>;

/// Location of the RsCli binary (env `RSCLI_PATH`, else bundled default).
fn rscli_path() -> PathBuf {
    if let Ok(p) = env::var("RSCLI_PATH") {
        return PathBuf::from(p);
    }
    // Mirrors Python's `Path(__file__).parent / "tools" / "rscli" / "RsCli"`.
    PathBuf::from("lib")
        .join("tools")
        .join("rscli")
        .join("RsCli")
}

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

/// Recursively collect files under `root` whose extension equals `ext`
/// (case-insensitive), sorted by path.
fn find_by_ext(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

/// Parsed shape of a Rocksmith arrangement XML.
struct SongXml {
    root: String,
    arrangement: Option<String>,
    tuning: Option<Vec<i32>>,
}

/// Read the root tag, arrangement text, and tuning offsets from a song XML.
fn parse_song_xml(path: &Path) -> Option<SongXml> {
    let text = fs::read_to_string(path).ok()?;
    let mut reader = Reader::from_str(&text);
    reader.trim_text(true);

    let mut root: Option<String> = None;
    let mut arrangement: Option<String> = None;
    let mut tuning: Option<Vec<i32>> = None;
    let mut in_arrangement = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if root.is_none() {
                    root = Some(name.clone());
                }
                if name == "tuning" && tuning.is_none() {
                    let mut offs = vec![0i32; 6];
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
                        if let Some(idx) = key.strip_prefix("string") {
                            if let Ok(i) = idx.parse::<usize>() {
                                if i < 6 {
                                    let val = String::from_utf8_lossy(&attr.value);
                                    offs[i] = val.trim().parse::<i32>().unwrap_or(0);
                                }
                            }
                        }
                    }
                    tuning = Some(offs);
                }
                if name == "arrangement" {
                    in_arrangement = true;
                }
            }
            Ok(Event::Text(t)) => {
                if in_arrangement && arrangement.is_none() {
                    let s = t.unescape().unwrap_or_default().trim().to_string();
                    if !s.is_empty() {
                        arrangement = Some(s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"arrangement" {
                    in_arrangement = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }

    Some(SongXml {
        root: root?,
        arrangement,
        tuning,
    })
}

/// Return the name of the first (root) element in an XML document, skipping
/// declarations (`<?...?>`) and doctype/comments (`<!...>`).
fn xml_root_tag(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if i + 1 < bytes.len() && (bytes[i + 1] == b'?' || bytes[i + 1] == b'!') {
                while i < bytes.len() && bytes[i] != b'>' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            while j < bytes.len()
                && !bytes[j].is_ascii_whitespace()
                && bytes[j] != b'>'
                && bytes[j] != b'/'
            {
                j += 1;
            }
            return Some(String::from_utf8_lossy(&bytes[start..j]).into_owned());
        }
        i += 1;
    }
    None
}

/// Set every `stringN` attribute on the `<tuning>` element to `"0"`.
/// Returns true if the file was a `song` and its tuning was updated.
fn set_tuning_zero(path: &Path) -> bool {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return false,
    };

    match xml_root_tag(&text) {
        Some(r) if r == "song" => {}
        _ => return false,
    }

    let re_tag = match Regex::new(r"(?s)<tuning\b[^>]*?/?>") {
        Ok(r) => r,
        Err(_) => return false,
    };
    let tag_match = match re_tag.find(&text) {
        Some(m) => m,
        None => return false,
    };

    let orig = tag_match.as_str().to_string();
    let mut new_tag = orig.clone();
    for i in 0..6 {
        if let Ok(re_attr) = Regex::new(&format!(r#"string{}\s*=\s*"[^"]*""#, i)) {
            let replacement = format!("string{}=\"0\"", i);
            new_tag = re_attr.replace(&new_tag, replacement.as_str()).into_owned();
        }
    }

    if new_tag != orig {
        let new_text = text.replacen(&orig, &new_tag, 1);
        return fs::write(path, new_text).is_ok();
    }

    false
}

/// Extract tuning from a PSARC. Returns (offsets, is_uniform).
/// Prefers guitar (Lead/Rhythm/Combo) arrangements over Bass.
pub fn get_tuning(psarc_path: &str) -> Result<(Vec<i32>, bool), BoxErr> {
    let tmp_dir = Builder::new().prefix("rs_tune_").tempdir()?;
    let tmp = tmp_dir.path().to_path_buf();

    unpack_psarc(Path::new(psarc_path), &tmp).map_err(|e| format!("{e}"))?;

    let mut guitar_tuning: Option<Vec<i32>> = None;
    let mut fallback_tuning: Option<Vec<i32>> = None;

    // Check manifest JSON first (works for SNG-only files).
    for jf in find_by_ext(&tmp, "json") {
        let text = match fs::read_to_string(&jf) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let data: serde_json::Value = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if let Some(entries) = data.get("Entries").and_then(|e| e.as_object()) {
            for v in entries.values() {
                let attrs = match v.get("Attributes").and_then(|a| a.as_object()) {
                    Some(a) => a,
                    None => continue,
                };
                let arr_name = attrs
                    .get("ArrangementName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let tun = attrs.get("Tuning").and_then(|t| t.as_object());
                let tun = match tun {
                    Some(t) if !t.is_empty() => t,
                    _ => continue,
                };
                if matches!(arr_name, "Vocals" | "ShowLights" | "JVocals") {
                    continue;
                }
                let offsets: Vec<i32> = (0..6)
                    .map(|i| {
                        tun.get(&format!("string{}", i))
                            .and_then(|x| x.as_i64())
                            .unwrap_or(0) as i32
                    })
                    .collect();
                if matches!(arr_name, "Lead" | "Rhythm" | "Combo") {
                    if guitar_tuning.is_none() {
                        guitar_tuning = Some(offsets);
                    }
                } else if fallback_tuning.is_none() {
                    fallback_tuning = Some(offsets);
                }
            }
        }
    }

    // Check XMLs as fallback.
    if guitar_tuning.is_none() && fallback_tuning.is_none() {
        for xml_path in find_by_ext(&tmp, "xml") {
            let info = match parse_song_xml(&xml_path) {
                Some(i) => i,
                None => continue,
            };
            if info.root != "song" {
                continue;
            }
            if let Some(a) = &info.arrangement {
                let low = a.to_lowercase();
                if matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                    continue;
                }
            }
            if let Some(offsets) = info.tuning {
                let fname = xml_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if fname.contains("lead") || fname.contains("rhythm") || fname.contains("combo") {
                    if guitar_tuning.is_none() {
                        guitar_tuning = Some(offsets);
                    }
                } else if fallback_tuning.is_none() {
                    fallback_tuning = Some(offsets);
                }
            }
        }
    }

    let best = guitar_tuning
        .or(fallback_tuning)
        .unwrap_or_else(|| vec![0; 6]);
    let is_uniform = best.iter().all(|&x| x == best[0]);
    Ok((best, is_uniform))
    // tmp_dir dropped here -> temp directory removed (mirrors finally rmtree).
}

/// Decode a WEM, pitch-shift it, and replace the original file in place.
/// Returns true if successful.
fn pitch_shift_wem(wem_path: &Path, semitones: i32) -> bool {
    let wav_decoded = wem_path.with_extension("decoded.wav");
    let ogg_out = wem_path.with_extension("shifted.ogg");

    let cleanup = |paths: &[&Path]| {
        for p in paths {
            if p.exists() {
                let _ = fs::remove_file(p);
            }
        }
    };

    // Step 1: Decode WEM to WAV.
    let mut decoded = false;
    if which("vgmstream-cli").is_some() {
        let ok = Command::new("vgmstream-cli")
            .arg("-o")
            .arg(&wav_decoded)
            .arg(wem_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            if let Ok(m) = fs::metadata(&wav_decoded) {
                if m.len() > 100 {
                    decoded = true;
                    println!("    Decoded with vgmstream ({} bytes)", m.len());
                }
            }
        }
    }

    if !decoded && which("ffmpeg").is_some() {
        let ok = Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(wem_path)
            .arg(&wav_decoded)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            if let Ok(m) = fs::metadata(&wav_decoded) {
                if m.len() > 100 {
                    decoded = true;
                    println!("    Decoded with ffmpeg ({} bytes)", m.len());
                }
            }
        }
    }

    if !decoded {
        println!(
            "    FAILED to decode {}",
            wem_path.file_name().unwrap_or_default().to_string_lossy()
        );
        cleanup(&[wav_decoded.as_path(), ogg_out.as_path()]);
        return false;
    }

    // Step 2: Pitch shift (rubberband preserves tempo, only shifts pitch).
    // Detect original sample rate to preserve it.
    let sample_rate = match Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&wav_decoded)
        .output()
    {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("44100")
                .to_string()
        }
        Err(_) => "44100".to_string(),
    };

    let factor = 2f64.powf(semitones as f64 / 12.0);
    let shift_result = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(&wav_decoded)
        .arg("-af")
        .arg(format!("rubberband=pitch={}", factor))
        .arg("-ar")
        .arg(&sample_rate)
        .arg("-q:a")
        .arg("6")
        .arg(&ogg_out)
        .output();
    let _ = fs::remove_file(&wav_decoded);

    let (ok, stderr) = match shift_result {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(_) => (false, String::new()),
    };

    if !ok || !ogg_out.exists() {
        let tail: String = stderr.chars().rev().take(200).collect::<String>().chars().rev().collect();
        println!("    FAILED to pitch-shift: {}", tail);
        cleanup(&[ogg_out.as_path()]);
        return false;
    }

    let size = fs::metadata(&ogg_out).map(|m| m.len()).unwrap_or(0);
    println!("    Shifted {:+} semitones ({} bytes)", semitones, size);

    // Step 3: Replace original WEM with shifted OGG.
    // (Rocksmith accepts OGG files with a .wem extension.)
    let _ = fs::remove_file(wem_path);
    if fs::rename(&ogg_out, wem_path).is_err() {
        // Fallback across filesystems.
        if fs::copy(&ogg_out, wem_path).is_err() {
            return false;
        }
        let _ = fs::remove_file(&ogg_out);
    }
    true
}

/// Set every arrangement's `Tuning` in JSON manifests to zeros. Returns true
/// if any manifest was changed for that file.
fn zero_manifest_tuning(json_path: &Path) -> bool {
    let text = match fs::read_to_string(json_path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let mut data: serde_json::Value = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let mut changed = false;
    if let Some(entries) = data.get_mut("Entries").and_then(|e| e.as_object_mut()) {
        for entry in entries.values_mut() {
            if let Some(attrs) = entry.get_mut("Attributes").and_then(|a| a.as_object_mut()) {
                if attrs.contains_key("Tuning") {
                    let mut zeros = serde_json::Map::new();
                    for i in 0..6 {
                        zeros.insert(
                            format!("string{}", i),
                            serde_json::Value::from(0),
                        );
                    }
                    attrs.insert("Tuning".to_string(), serde_json::Value::Object(zeros));
                    changed = true;
                }
            }
        }
    }

    if changed {
        if let Ok(out) = serde_json::to_string_pretty(&data) {
            let _ = fs::write(json_path, out);
        }
    }
    changed
}

/// Pitch-shift a CDLC to E standard tuning.
///
/// * `psarc_path`: input `.psarc` file.
/// * `output_path`: output path; if empty, uses the input name with an
///   `_EStd` suffix.
///
/// Returns the path to the new `.psarc` file. Errors if the tuning is
/// non-uniform or already E standard.
pub fn retune_to_standard(psarc_path: &str, output_path: &str) -> Result<String, BoxErr> {
    let (offsets, is_uniform) = get_tuning(psarc_path)?;

    if offsets.iter().all(|&o| o == 0) {
        return Err("Already in E standard tuning".into());
    }

    if !is_uniform {
        return Err(format!(
            "Non-uniform tuning {:?} — only uniform tunings supported. \
             E.g. Eb standard [-1,-1,-1,-1,-1,-1]",
            offsets
        )
        .into());
    }

    let semitones = -offsets[0]; // e.g. offset=-1 (Eb) -> shift up by 1
    println!("Tuning: {:?} → shifting {:+} semitone(s)", offsets, semitones);

    let tmp_dir = Builder::new().prefix("rs_retune_").tempdir()?;
    let tmp = tmp_dir.path().to_path_buf();

    // Extract.
    println!("Extracting PSARC...");
    unpack_psarc(Path::new(psarc_path), &tmp).map_err(|e| format!("{e}"))?;

    // Pitch-shift all audio files.
    let mut shifted_count = 0usize;
    for wem in find_by_ext(&tmp, "wem") {
        println!(
            "Processing: {}",
            wem.file_name().unwrap_or_default().to_string_lossy()
        );
        if pitch_shift_wem(&wem, semitones) {
            shifted_count += 1;
        }
    }

    if shifted_count == 0 {
        return Err("No audio files were successfully pitch-shifted".into());
    }

    println!("Shifted {} audio file(s)", shifted_count);

    // Update arrangement XMLs: set tuning to E standard.
    for xml_path in find_by_ext(&tmp, "xml") {
        if set_tuning_zero(&xml_path) {
            println!(
                "Updated tuning: {}",
                xml_path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }

    // Recompile SNGs from updated XMLs.
    let rscli = rscli_path();
    if rscli.exists() {
        for xml_path in find_by_ext(&tmp, "xml") {
            // Only songs/arr/*.xml.
            let is_arr = xml_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                == Some("arr")
                && xml_path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    == Some("songs");
            if !is_arr {
                continue;
            }

            let info = match parse_song_xml(&xml_path) {
                Some(i) => i,
                None => continue,
            };
            if info.root != "song" {
                continue;
            }
            if let Some(a) = &info.arrangement {
                let low = a.to_lowercase();
                if matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                    continue;
                }
            }

            let stem = xml_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let sng_path = tmp
                .join("songs")
                .join("bin")
                .join("generic")
                .join(format!("{}.sng", stem));
            if sng_path.exists() {
                println!("Recompiling SNG: {}", stem);
                let _ = Command::new(&rscli)
                    .arg("xml2sng")
                    .arg(&xml_path)
                    .arg(&sng_path)
                    .output();
            }
        }
    }

    // Update JSON manifests.
    for json_path in find_by_ext(&tmp, "json") {
        zero_manifest_tuning(&json_path);
    }

    // Repack.
    println!("Repacking PSARC...");
    let final_output = if output_path.is_empty() {
        let p = Path::new(psarc_path);
        let mut stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        if let Some(base) = stem.strip_suffix("_p") {
            stem = base.to_string();
        }
        let parent = p.parent().unwrap_or_else(|| Path::new("."));
        parent
            .join(format!("{}_EStd_p.psarc", stem))
            .to_string_lossy()
            .into_owned()
    } else {
        output_path.to_string()
    };

    pack_psarc(&tmp, Path::new(&final_output)).map_err(|e| format!("{e}"))?;
    println!("Created: {}", final_output);
    Ok(final_output)
    // tmp_dir dropped here -> temp directory removed (mirrors finally rmtree).
}
