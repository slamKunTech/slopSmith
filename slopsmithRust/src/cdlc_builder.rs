//! Build a complete Rocksmith 2014 CDLC .psarc from arrangement XMLs + audio.
//!
//! Rust port of `lib/cdlc_builder.py`. Manifests are generated with
//! [`serde_json`], XML metadata is read with [`quick_xml`], and the final
//! archive is packed via [`crate::patcher::pack_psarc`].

use std::path::{Path, PathBuf};
use std::process::Command;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use serde_json::{json, Map, Value};
use uuid::Uuid;

pub const DEFAULT_APP_ID: &str = "248750";

/// Location of the RsCli tool used for XML→SNG conversion.
fn rscli_path() -> PathBuf {
    if let Ok(p) = std::env::var("RSCLI_PATH") {
        PathBuf::from(p)
    } else {
        PathBuf::from("tools/rscli/RsCli")
    }
}

#[derive(Debug, Clone)]
struct ArrangementInfo {
    name: String,
    persistent_id: String,
    #[allow(dead_code)]
    master_id: i64,
}

/// Generate a lowercase alphanumeric DLC key from artist + title.
pub fn sanitize_key(artist: &str, title: &str) -> String {
    let combined = format!("{artist}{title}").to_lowercase();
    combined
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect()
}

/// Generate a Rocksmith manifest JSON for a single arrangement.
#[allow(clippy::too_many_arguments)]
fn generate_manifest(
    dlc_key: &str,
    arrangement_name: &str,
    song_title: &str,
    artist: &str,
    album: &str,
    year: &str,
    song_length: f64,
    tuning: &[i32],
    persistent_id: &str,
    master_id: i64,
) -> Value {
    let arr_lower = arrangement_name.to_lowercase();
    let route_mask = if arr_lower == "bass" {
        4
    } else if arr_lower == "rhythm" {
        2
    } else {
        1
    };

    let song_year: i64 = if year.is_empty() {
        2024
    } else {
        year.parse::<i64>().unwrap_or(2024)
    };

    let mut tuning_map = Map::new();
    for (i, v) in tuning.iter().enumerate() {
        tuning_map.insert(format!("string{i}"), json!(v));
    }

    let attributes = json!({
        "ArrangementName": arrangement_name,
        "DLCKey": dlc_key,
        "LeaderboardChallengeRating": 0,
        "ManifestUrn": format!("urn:database:json-db:{dlc_key}_{arr_lower}"),
        "MasterID_RDV": master_id,
        "PersistentID": persistent_id,
        "SongKey": dlc_key,
        "SongLength": song_length,
        "SongName": song_title,
        "ArtistName": artist,
        "AlbumName": album,
        "SongYear": song_year,
        "Tuning": Value::Object(tuning_map),
        "ArrangementSort": 0,
        "RouteMask": route_mask,
        "CapoFret": 0,
        "CentOffset": 0.0,
        "DNA_Chords": 0.0,
        "DNA_Riffs": 0.0,
        "DNA_Solo": 0.0,
        "NotesEasy": 0.0,
        "NotesMedium": 0.0,
        "NotesHard": 0.0,
        "Tones": [],
        "Tone_Base": "Default",
        "Tone_Multiplayer": "",
        "Tone_A": "",
        "Tone_B": "",
        "Tone_C": "",
        "Tone_D": "",
    });

    let mut entries = Map::new();
    entries.insert(
        persistent_id.to_string(),
        json!({ "Attributes": attributes }),
    );

    json!({
        "Entries": Value::Object(entries),
        "ModelName": "RSEnumerable_Song",
        "IterationVersion": 2,
        "InsertRoot": format!("Static.Songs.Headers.{dlc_key}"),
    })
}

/// Generate the aggregate `.hsan` manifest from per-arrangement manifests.
fn generate_hsan(arrangements: &[Value]) -> Value {
    let mut entries = Map::new();
    for arr in arrangements {
        if let Some(Value::Object(arr_entries)) = arr.get("Entries") {
            for (pid, data) in arr_entries {
                entries.insert(pid.clone(), data.clone());
            }
        }
    }
    json!({
        "Entries": Value::Object(entries),
        "ModelName": "RSEnumerable_Song",
        "IterationVersion": 2,
        "InsertRoot": "Static.Songs.Headers",
    })
}

/// Generate the `.xblock` game entity XML.
fn generate_xblock(dlc_key: &str, arrangements_info: &[ArrangementInfo]) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(r#"<?xml version="1.0" encoding="utf-8"?>"#.to_string());
    lines.push("<game>".to_string());
    lines.push("  <entitySet>".to_string());

    for info in arrangements_info {
        let name = info.name.to_lowercase();
        lines.push(format!(
            r#"    <entity id="{}" modelName="RSEnumerable_Song" name="{dlc_key}_{name}" iterations="0">"#,
            info.persistent_id
        ));
        lines.push(r#"      <property name="Header">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:database:json-db:{dlc_key}_{name}" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="Manifest">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:database:json-db:{dlc_key}_{name}" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="SngAsset">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:application:musicgame-song:{dlc_key}_{name}" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="AlbumArtSmall">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:image:dds:album_{dlc_key}_64" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="AlbumArtMedium">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:image:dds:album_{dlc_key}_128" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="AlbumArtLarge">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:image:dds:album_{dlc_key}_256" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="LyricArt">"#.to_string());
        lines.push(r#"        <set value="" />"#.to_string());
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="ShowLightsXMLAsset">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:application:xml:{dlc_key}_showlights" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="SoundBank">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:audio:wwise-sound-bank:song_{dlc_key}" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push(r#"      <property name="PreviewSoundBank">"#.to_string());
        lines.push(format!(
            r#"        <set value="urn:audio:wwise-sound-bank:song_{dlc_key}_preview" />"#
        ));
        lines.push("      </property>".to_string());
        lines.push("    </entity>".to_string());
    }

    lines.push("  </entitySet>".to_string());
    lines.push("</game>".to_string());
    lines.join("\n")
}

/// Generate a minimal showlights XML. `song_length` is accepted for parity but
/// unused (the minimal file contains a single fixed showlight).
fn generate_showlights(_song_length: f64) -> String {
    concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
        "<showlights count=\"1\">\n",
        "  <showlight time=\"0.000\" note=\"44\" />\n",
        "</showlights>"
    )
    .to_string()
}

/// Generate the aggregate graph `.nt` file.
fn generate_aggregategraph(dlc_key: &str, arrangements_info: &[ArrangementInfo]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for info in arrangements_info {
        let name = info.name.to_lowercase();
        lines.push(format!("urn:application:musicgame-song:{dlc_key}_{name} {{"));
        lines.push("  a urn:application:musicgame-song ;".to_string());
        lines.push("}".to_string());
    }
    lines.join("\n")
}

/// Read `songLength` and `tuning` out of a Rocksmith arrangement XML.
fn parse_xml_meta(xml_path: &str) -> Result<(f64, [i32; 6]), String> {
    let content =
        std::fs::read_to_string(xml_path).map_err(|e| format!("failed to read {xml_path}: {e}"))?;
    let mut reader = Reader::from_str(&content);

    let mut song_length = 300.0f64;
    let mut tuning = [0i32; 6];
    let mut in_song_length = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"songLength" => in_song_length = true,
                b"tuning" => read_tuning(&e, &mut tuning),
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"tuning" {
                    read_tuning(&e, &mut tuning);
                }
            }
            Ok(Event::Text(t)) => {
                if in_song_length {
                    if let Ok(s) = t.unescape() {
                        if let Ok(v) = s.trim().parse::<f64>() {
                            song_length = v;
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"songLength" {
                    in_song_length = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error in {xml_path}: {e}")),
            _ => {}
        }
    }

    Ok((song_length, tuning))
}

fn read_tuning(e: &BytesStart, tuning: &mut [i32; 6]) {
    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        if key.starts_with(b"string") {
            if let Some(idx_byte) = key.last() {
                if let Some(idx) = (*idx_byte as char).to_digit(10) {
                    let idx = idx as usize;
                    if idx < 6 {
                        if let Ok(s) = std::str::from_utf8(&attr.value) {
                            if let Ok(v) = s.parse::<i32>() {
                                tuning[idx] = v;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect()
}

/// Build a complete CDLC .psarc file. Returns the path to the created archive.
#[allow(clippy::too_many_arguments)]
pub fn build_cdlc(
    xml_paths: &[String],
    arrangement_names: &[String],
    audio_path: &str,
    title: &str,
    artist: &str,
    album: &str,
    year: &str,
    output_path: &str,
    album_art_path: &str,
    on_progress: Option<&dyn Fn(&str, f64)>,
) -> Result<String, String> {
    let dlc_key = sanitize_key(artist, title);
    let tmp = tempfile::Builder::new()
        .prefix("cdlc_build_")
        .tempdir()
        .map_err(|e| format!("failed to create temp dir: {e}"))?;

    let progress = |msg: &str, pct: f64| {
        if let Some(cb) = on_progress {
            cb(msg, pct);
        }
        println!("  [{:.0}%] {}", pct, msg);
    };

    let build_dir = tmp.path().join(&dlc_key);
    std::fs::create_dir_all(&build_dir).map_err(|e| format!("failed to create build dir: {e}"))?;

    let mut arrangements_info: Vec<ArrangementInfo> = Vec::new();
    let mut manifests: Vec<Value> = Vec::new();
    let mut last_song_length = 300.0f64;

    // ── Convert XMLs to SNG + generate manifests ─────────────────────────
    for (i, (xml_path, arr_name)) in xml_paths.iter().zip(arrangement_names.iter()).enumerate() {
        progress(
            &format!("Converting {arr_name} XML to SNG..."),
            10.0 + i as f64 * 15.0,
        );

        let arr_lower = arr_name.to_lowercase();
        let persistent_id = Uuid::new_v4().to_string().to_uppercase();
        let master_id = 1000 + i as i64;

        let sng_dir = build_dir.join("songs").join("bin").join("generic");
        std::fs::create_dir_all(&sng_dir).map_err(|e| format!("mkdir failed: {e}"))?;
        let sng_path = sng_dir.join(format!("{dlc_key}_{arr_lower}.sng"));

        let result = Command::new(rscli_path())
            .arg("xml2sng")
            .arg(xml_path)
            .arg(&sng_path)
            .output()
            .map_err(|e| format!("failed to launch RsCli: {e}"))?;
        if !result.status.success() {
            return Err(format!(
                "SNG conversion failed for {arr_name}: {}",
                String::from_utf8_lossy(&result.stderr)
            ));
        }

        // Copy XML arrangement.
        let arr_dir = build_dir.join("songs").join("arr");
        std::fs::create_dir_all(&arr_dir).map_err(|e| format!("mkdir failed: {e}"))?;
        std::fs::copy(xml_path, arr_dir.join(format!("{dlc_key}_{arr_lower}.xml")))
            .map_err(|e| format!("failed to copy XML: {e}"))?;

        // Read metadata from XML.
        let (song_length, tuning) = parse_xml_meta(xml_path)?;
        last_song_length = song_length;

        // Generate manifest.
        let manifest = generate_manifest(
            &dlc_key,
            arr_name,
            title,
            artist,
            album,
            year,
            song_length,
            &tuning,
            &persistent_id,
            master_id,
        );
        manifests.push(manifest.clone());

        let manifest_dir = build_dir
            .join("manifests")
            .join(format!("songs_dlc_{dlc_key}"));
        std::fs::create_dir_all(&manifest_dir).map_err(|e| format!("mkdir failed: {e}"))?;
        std::fs::write(
            manifest_dir.join(format!("{dlc_key}_{arr_lower}.json")),
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("failed to write manifest: {e}"))?;

        arrangements_info.push(ArrangementInfo {
            name: arr_name.clone(),
            persistent_id,
            master_id,
        });
    }

    // ── HSAN ─────────────────────────────────────────────────────────────
    progress("Generating manifests...", 60.0);
    let hsan = generate_hsan(&manifests);
    let manifest_dir = build_dir
        .join("manifests")
        .join(format!("songs_dlc_{dlc_key}"));
    std::fs::write(
        manifest_dir.join(format!("songs_dlc_{dlc_key}.hsan")),
        serde_json::to_string_pretty(&hsan).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("failed to write hsan: {e}"))?;

    // ── Audio ──────────────────────────────────────────────────────────────
    progress("Processing audio...", 65.0);
    let audio_dir = build_dir.join("audio").join("windows");
    std::fs::create_dir_all(&audio_dir).map_err(|e| format!("mkdir failed: {e}"))?;

    if !Path::new(audio_path).exists() {
        return Err(format!("Audio file not found: {audio_path}"));
    }

    let audio_ext = Path::new(audio_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let wem_path = audio_dir.join(format!("song_{dlc_key}.wem"));

    match audio_ext.as_str() {
        "ogg" | "wem" => {
            std::fs::copy(audio_path, &wem_path).map_err(|e| format!("copy failed: {e}"))?;
        }
        "wav" => {
            let ogg_tmp = tmp.path().join("audio.ogg");
            let res = Command::new("ffmpeg")
                .args(["-y", "-i", audio_path, "-q:a", "6"])
                .arg(&ogg_tmp)
                .output();
            let ok = matches!(&res, Ok(o) if o.status.success()) && ogg_tmp.exists();
            if ok {
                std::fs::copy(&ogg_tmp, &wem_path).map_err(|e| format!("copy failed: {e}"))?;
            } else {
                progress("Warning: ffmpeg conversion failed, using WAV directly", 67.0);
                std::fs::copy(audio_path, &wem_path).map_err(|e| format!("copy failed: {e}"))?;
            }
        }
        _ => {
            let ogg_tmp = tmp.path().join("audio.ogg");
            let _ = Command::new("ffmpeg")
                .args(["-y", "-i", audio_path, "-q:a", "6"])
                .arg(&ogg_tmp)
                .output();
            if ogg_tmp.exists() {
                std::fs::copy(&ogg_tmp, &wem_path).map_err(|e| format!("copy failed: {e}"))?;
            } else {
                return Err(format!("Failed to convert audio: {audio_path}"));
            }
        }
    }

    // Minimal placeholder soundbanks.
    std::fs::write(audio_dir.join(format!("song_{dlc_key}.bnk")), [0u8; 64])
        .map_err(|e| format!("write bnk failed: {e}"))?;
    std::fs::write(
        audio_dir.join(format!("song_{dlc_key}_preview.bnk")),
        [0u8; 64],
    )
    .map_err(|e| format!("write bnk failed: {e}"))?;

    // ── Album art ──────────────────────────────────────────────────────────
    progress("Processing album art...", 75.0);
    let art_dir = build_dir.join("gfxassets").join("album_art");
    std::fs::create_dir_all(&art_dir).map_err(|e| format!("mkdir failed: {e}"))?;

    let sizes = [64u32, 128, 256];
    let have_art = !album_art_path.is_empty() && Path::new(album_art_path).exists();
    if have_art {
        let ext = Path::new(album_art_path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext == "dds" {
            for size in sizes {
                std::fs::copy(album_art_path, art_dir.join(format!("album_{dlc_key}_{size}.dds")))
                    .map_err(|e| format!("copy failed: {e}"))?;
            }
        } else {
            match image::open(album_art_path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    for size in sizes {
                        let resized = image::imageops::resize(
                            &rgba,
                            size,
                            size,
                            image::imageops::FilterType::Lanczos3,
                        );
                        write_image_dds(
                            &art_dir.join(format!("album_{dlc_key}_{size}.dds")),
                            size,
                            &resized,
                        )?;
                    }
                }
                Err(_) => {
                    for size in sizes {
                        write_placeholder_dds(
                            &art_dir.join(format!("album_{dlc_key}_{size}.dds")),
                            size,
                        )?;
                    }
                }
            }
        }
    } else {
        for size in sizes {
            write_placeholder_dds(&art_dir.join(format!("album_{dlc_key}_{size}.dds")), size)?;
        }
    }

    // ── Showlights ───────────────────────────────────────────────────────
    let arr_dir = build_dir.join("songs").join("arr");
    std::fs::write(
        arr_dir.join(format!("{dlc_key}_showlights.xml")),
        generate_showlights(last_song_length),
    )
    .map_err(|e| format!("write showlights failed: {e}"))?;

    // ── XBlock ─────────────────────────────────────────────────────────────
    let xblock_dir = build_dir.join("gamexblocks").join("nsongs");
    std::fs::create_dir_all(&xblock_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    std::fs::write(
        xblock_dir.join(format!("{dlc_key}.xblock")),
        generate_xblock(&dlc_key, &arrangements_info),
    )
    .map_err(|e| format!("write xblock failed: {e}"))?;

    // ── Aggregate graph ──────────────────────────────────────────────────
    std::fs::write(
        build_dir.join(format!("{dlc_key}_aggregategraph.nt")),
        generate_aggregategraph(&dlc_key, &arrangements_info),
    )
    .map_err(|e| format!("write aggregategraph failed: {e}"))?;

    // ── App ID + toolkit version ───────────────────────────────────────────
    std::fs::write(build_dir.join("appid.appid"), DEFAULT_APP_ID)
        .map_err(|e| format!("write appid failed: {e}"))?;
    std::fs::write(build_dir.join("toolkit.version"), "RsCli GP2RS 1.0")
        .map_err(|e| format!("write toolkit.version failed: {e}"))?;

    // ── Pack PSARC ─────────────────────────────────────────────────────────
    progress("Packing PSARC...", 90.0);
    let output_path = if output_path.is_empty() {
        format!(
            "{}_{}_p.psarc",
            sanitize_filename(title),
            sanitize_filename(artist)
        )
    } else {
        output_path.to_string()
    };

    crate::patcher::pack_psarc(&build_dir, Path::new(&output_path))
        .map_err(|e| format!("pack_psarc failed: {e}"))?;

    progress(&format!("Created: {output_path}"), 100.0);
    Ok(output_path)
}

/// Build a 128-byte uncompressed 32-bit RGBA DDS header (B,G,R,A byte order).
fn dds_header(size: u32) -> [u8; 128] {
    let mut header = [0u8; 128];
    let put = |h: &mut [u8; 128], off: usize, v: u32| {
        h[off..off + 4].copy_from_slice(&v.to_le_bytes());
    };
    header[0..4].copy_from_slice(b"DDS ");
    put(&mut header, 4, 124); // header size
    put(&mut header, 8, 0x1 | 0x2 | 0x4 | 0x1000); // flags
    put(&mut header, 12, size); // height
    put(&mut header, 16, size); // width
    put(&mut header, 20, size * 4); // pitch
    put(&mut header, 76, 32); // pixel format size
    put(&mut header, 80, 0x41); // DDPF_RGB | DDPF_ALPHAPIXELS
    put(&mut header, 88, 32); // RGB bit count
    put(&mut header, 92, 0x00FF_0000); // R mask
    put(&mut header, 96, 0x0000_FF00); // G mask
    put(&mut header, 100, 0x0000_00FF); // B mask
    put(&mut header, 104, 0xFF00_0000); // A mask
    header
}

/// Write a minimal uncompressed DDS file filled with dark gray.
fn write_placeholder_dds(path: &Path, size: u32) -> Result<(), String> {
    let header = dds_header(size);
    let pixel = [0x30u8, 0x30, 0x30, 0xFF]; // B, G, R, A → dark gray
    let mut data = Vec::with_capacity(128 + (size * size * 4) as usize);
    data.extend_from_slice(&header);
    for _ in 0..(size * size) {
        data.extend_from_slice(&pixel);
    }
    std::fs::write(path, data).map_err(|e| format!("failed to write DDS: {e}"))
}

/// Write an uncompressed DDS from an RGBA image (converted to B,G,R,A order).
fn write_image_dds(path: &Path, size: u32, img: &image::RgbaImage) -> Result<(), String> {
    let header = dds_header(size);
    let mut data = Vec::with_capacity(128 + (size * size * 4) as usize);
    data.extend_from_slice(&header);
    for pixel in img.pixels() {
        let [r, g, b, a] = pixel.0;
        data.extend_from_slice(&[b, g, r, a]);
    }
    std::fs::write(path, data).map_err(|e| format!("failed to write DDS: {e}"))
}
