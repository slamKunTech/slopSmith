//! Pitch-shift a CDLC's audio to E standard. Port of `lib/retune.py`. Only
//! uniform tunings (all strings shifted equally) are supported. Depends on
//! `patcher` (pack/unpack), ffmpeg's `rubberband` filter, vgmstream-cli, and
//! RsCli (`xml2sng`). Progress is reported via a callback so the retune WS
//! can stream `{"stage","progress"}` frames.

use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;
use serde_json::Value;

use crate::engine::patcher;
use crate::engine::psarc;

/// Resolve the RsCli binary (env `RSCLI_PATH` or bundled candidates). Reuses
/// the same candidate list as [`crate::engine::song::resolve_rscli`].
fn rscli() -> Option<PathBuf> {
    crate::engine::song::resolve_rscli()
}

/// `shutil.which`.
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

/// Extract tuning from a PSARC. Returns `(offsets, is_uniform)`. Prefers
/// guitar (Lead/Rhythm/Combo) arrangements over Bass. Mirrors `get_tuning`
/// (retune.py:20-76).
pub fn get_tuning(psarc_path: &Path) -> std::io::Result<(Vec<i64>, bool)> {
    let tmp = mkdtemp("rs_tune_");
    let result = (|| -> std::io::Result<(Vec<i64>, bool)> {
        psarc::unpack_psarc(psarc_path, &tmp)?;
        let mut guitar_tuning: Option<Vec<i64>> = None;
        let mut fallback_tuning: Option<Vec<i64>> = None;

        // Manifest JSONs first (works for SNG-only files).
        let mut jsons = rglob(&tmp, "json");
        jsons.sort();
        for jf in &jsons {
            let Ok(text) = std::fs::read_to_string(jf) else { continue };
            let Ok(data) = serde_json::from_str::<Value>(&text) else { continue };
            let Some(entries) = data.get("Entries").and_then(|v| v.as_object()) else { continue };
            for (_k, v) in entries.iter() {
                let Some(attrs) = v.get("Attributes") else { continue };
                let arr_name = attrs.get("ArrangementName").and_then(|v| v.as_str()).unwrap_or("");
                if matches!(arr_name, "Vocals" | "ShowLights" | "JVocals") {
                    continue;
                }
                let Some(tun) = attrs.get("Tuning").and_then(|v| v.as_object()) else { continue };
                if tun.is_empty() {
                    continue;
                }
                let offsets: Vec<i64> = (0..6).map(|i| tun.get(&format!("string{i}")).and_then(|v| v.as_i64()).unwrap_or(0)).collect();
                if matches!(arr_name, "Lead" | "Rhythm" | "Combo") {
                    if guitar_tuning.is_none() {
                        guitar_tuning = Some(offsets);
                    }
                } else if fallback_tuning.is_none() {
                    fallback_tuning = Some(offsets);
                }
            }
        }

        // XML fallback.
        if guitar_tuning.is_none() && fallback_tuning.is_none() {
            let mut xmls = rglob(&tmp, "xml");
            xmls.sort();
            for xml_path in &xmls {
                let Ok(text) = std::fs::read_to_string(xml_path) else { continue };
                let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
                let root = doc.root_element();
                if root.tag_name().name() != "song" {
                    continue;
                }
                if let Some(arr) = root.children().find(|c| c.is_element() && c.tag_name().name() == "arrangement") {
                    if let Some(t) = arr.text() {
                        let low = t.trim().to_lowercase();
                        if matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                            continue;
                        }
                    }
                }
                let Some(tuning_el) = root.children().find(|c| c.is_element() && c.tag_name().name() == "tuning") else { continue };
                let offsets: Vec<i64> = (0..6).map(|i| tuning_el.attribute(format!("string{i}").as_str()).and_then(|s| s.parse().ok()).unwrap_or(0)).collect();
                let fname = xml_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let is_guitar = fname.contains("lead") || fname.contains("rhythm") || fname.contains("combo");
                if is_guitar {
                    if guitar_tuning.is_none() {
                        guitar_tuning = Some(offsets);
                    }
                } else if fallback_tuning.is_none() {
                    fallback_tuning = Some(offsets);
                }
            }
        }

        let best = guitar_tuning.or(fallback_tuning).unwrap_or_else(|| vec![0; 6]);
        let is_uniform = best.iter().all(|&x| x == best[0]);
        Ok((best, is_uniform))
    })();
    std::fs::remove_dir_all(&tmp).ok();
    result
}

/// Decode a WEM, pitch-shift it, replace the original file. Returns true on
/// success. Mirrors `_pitch_shift_wem` (retune.py:79-148).
pub fn pitch_shift_wem(wem_path: &Path, semitones: i64) -> bool {
    let wav_decoded = wem_path.with_extension("decoded.wav");
    let ogg_out = wem_path.with_extension("shifted.ogg");

    // Step 1: decode WEM → WAV.
    let mut decoded = false;
    if let Some(vgm) = which("vgmstream-cli") {
        let r = Command::new(&vgm).arg("-o").arg(&wav_decoded).arg(wem_path).output();
        if let Ok(out) = &r {
            if out.status.success() && wav_decoded.exists() && file_size(&wav_decoded) > 100 {
                decoded = true;
            }
        }
    }
    if !decoded {
        if let Some(ff) = which("ffmpeg") {
            let r = Command::new(&ff).args(["-y", "-i"]).arg(wem_path).arg(&wav_decoded).output();
            if let Ok(out) = &r {
                if out.status.success() && wav_decoded.exists() && file_size(&wav_decoded) > 100 {
                    decoded = true;
                }
            }
        }
    }
    if !decoded {
        let _ = std::fs::remove_file(&wav_decoded);
        let _ = std::fs::remove_file(&ogg_out);
        return false;
    }

    // Step 2: pitch-shift (rubberband preserves tempo).
    let sample_rate = if let Some(probe) = which("ffprobe") {
        Command::new(probe)
            .args(["-v", "error", "-show_entries", "stream=sample_rate", "-of", "default=noprint_wrappers=1:nokey=1"])
            .arg(&wav_decoded)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "44100".to_string())
    } else {
        "44100".to_string()
    };
    let factor = 2f64.powf(semitones as f64 / 12.0);
    let r = Command::new(which("ffmpeg").unwrap_or_else(|| PathBuf::from("ffmpeg")))
        .args(["-y", "-i"]).arg(&wav_decoded)
        .arg("-af").arg(format!("rubberband=pitch={factor}"))
        .arg("-ar").arg(&sample_rate)
        .args(["-q:a", "6"]).arg(&ogg_out)
        .output();
    let _ = std::fs::remove_file(&wav_decoded);
    let ok = match &r {
        Ok(out) => out.status.success() && ogg_out.exists(),
        _ => false,
    };
    if !ok {
        let _ = std::fs::remove_file(&ogg_out);
        return false;
    }

    // Step 3: replace the original WEM with the shifted OGG (Rocksmith accepts
    // OGG data with a .wem extension).
    let _ = std::fs::remove_file(wem_path);
    std::fs::rename(&ogg_out, wem_path).is_ok()
}

/// Pitch-shift a CDLC to E standard. `report` receives `(stage_message,
/// progress%)` at each step. Mirrors `retune_to_standard` (retune.py:151-267).
pub fn retune_to_standard<F: Fn(&str, i32)>(
    psarc_path: &Path,
    output_path: &Path,
    report: &F,
) -> anyhow::Result<()> {
    let (offsets, _is_uniform) = get_tuning(psarc_path)?;
    if offsets.iter().all(|&o| o == 0) {
        anyhow::bail!("Already in E standard tuning");
    }
    if !offsets.iter().all(|&x| x == offsets[0]) {
        anyhow::bail!(
            "Non-uniform tuning {:?} — only uniform tunings supported. E.g. Eb standard [-1,-1,-1,-1,-1,-1]",
            offsets
        );
    }
    let semitones = -offsets[0];

    let tmp = mkdtemp("rs_retune_");
    let res = (|| -> anyhow::Result<()> {
        report("Extracting PSARC...", 10);
        psarc::unpack_psarc(psarc_path, &tmp)?;

        // Pitch-shift all WEMs.
        let mut wems = rglob(&tmp, "wem");
        wems.sort();
        let mut shifted_count = 0;
        for wem in &wems {
            report(&format!("Processing: {}", wem.file_name().and_then(|s| s.to_str()).unwrap_or("")), 30);
            if pitch_shift_wem(wem, semitones) {
                shifted_count += 1;
            }
        }
        if shifted_count == 0 {
            anyhow::bail!("No audio files were successfully pitch-shifted");
        }

        // Update arrangement XMLs: zero the tuning.
        let re = Regex::new(r#"string(\d+)="[^"]*""#).unwrap();
        let mut xmls = rglob(&tmp, "xml");
        xmls.sort();
        for xml_path in &xmls {
            let Ok(text) = std::fs::read_to_string(xml_path) else { continue };
            let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
            if doc.root_element().tag_name().name() != "song" {
                continue;
            }
            // Only rewrite if a <tuning> element exists.
            let has_tuning = doc.root_element().children().any(|c| c.is_element() && c.tag_name().name() == "tuning");
            if !has_tuning {
                continue;
            }
            let new_text = re.replace_all(&text, "string${1}=\"0\"").into_owned();
            if new_text != text {
                std::fs::write(xml_path, new_text)?;
            }
        }

        // Recompile SNGs from updated XMLs.
        if let Some(rscli) = rscli() {
            let arr_dir = tmp.join("songs").join("arr");
            let mut arr_xmls = rglob(&arr_dir, "xml");
            arr_xmls.sort();
            for xml_path in &arr_xmls {
                let Ok(text) = std::fs::read_to_string(xml_path) else { continue };
                let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
                if doc.root_element().tag_name().name() != "song" {
                    continue;
                }
                if let Some(arr) = doc.root_element().children().find(|c| c.is_element() && c.tag_name().name() == "arrangement") {
                    if let Some(t) = arr.text() {
                        let low = t.trim().to_lowercase();
                        if matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                            continue;
                        }
                    }
                }
                let stem = xml_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let sng_path = tmp.join("songs").join("bin").join("generic").join(format!("{stem}.sng"));
                if sng_path.exists() {
                    let _ = Command::new(&rscli).arg("xml2sng").arg(xml_path).arg(&sng_path).output();
                }
            }
        }

        // Update JSON manifests: zero the Tuning.
        let mut jsons = rglob(&tmp, "json");
        jsons.sort();
        for json_path in &jsons {
            let Ok(text) = std::fs::read_to_string(json_path) else { continue };
            let Ok(mut data) = serde_json::from_str::<Value>(&text) else { continue };
            let mut changed = false;
            if let Some(entries) = data.get("Entries").and_then(|v| v.as_object()).cloned() {
                if let Some(obj) = data.as_object_mut() {
                    if let Some(entries_val) = obj.get_mut("Entries") {
                        if let Some(entries_map) = entries_val.as_object_mut() {
                            for (_k, entry) in entries_map.iter_mut() {
                                if let Some(attrs) = entry.get_mut("Attributes").and_then(|v| v.as_object_mut()) {
                                    if attrs.contains_key("Tuning") {
                                        let mut tuning = serde_json::Map::new();
                                        for i in 0..6 {
                                            tuning.insert(format!("string{i}"), Value::from(0));
                                        }
                                        attrs.insert("Tuning".into(), Value::Object(tuning));
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = entries;
            }
            if changed {
                let pretty = serde_json::to_string_pretty(&data)?;
                std::fs::write(json_path, pretty)?;
            }
        }

        // Repack.
        report("Repacking PSARC...", 90);
        patcher::pack_psarc(&tmp, output_path)?;
        report(&format!("Created: {}", output_path.display()), 95);
        Ok(())
    })();
    std::fs::remove_dir_all(&tmp).ok();
    res
}

fn mkdtemp(prefix: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("{prefix}{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn rglob(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if e.path().extension().and_then(|x| x.to_str()) == Some(ext) {
            v.push(e.path().to_path_buf());
        }
    }
    v
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}
