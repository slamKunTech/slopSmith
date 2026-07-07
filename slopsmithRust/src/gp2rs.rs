//! Convert Guitar Pro files (.gp5/.gp4/.gp3) to Rocksmith 2014 arrangement XML.
//!
//! Rust port of `lib/gp2rs.py`. The Python original uses `pyguitarpro`; here we
//! define the required subset of the Guitar Pro object model as plain structs
//! and provide a [`parse`] stub. XML is generated with [`quick_xml`].

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::writer::Writer;

/// Standard tuning MIDI values (high e to low E, GP string order 1-6).
pub const STANDARD_TUNING_6: [i32; 6] = [64, 59, 55, 50, 45, 40];
/// Bass tuning: G D A E.
pub const STANDARD_TUNING_4: [i32; 4] = [43, 38, 33, 28];

pub const GP_TICKS_PER_QUARTER: i64 = 960;

// ─── Guitar Pro data model ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteType {
    Rest,
    Normal,
    Tie,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideType {
    ShiftSlideTo,
    LegatoSlideTo,
    OutDownwards,
    OutUpwards,
    IntoFromBelow,
    IntoFromAbove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarmonicKind {
    Natural,
    Artificial,
    Tapped,
    Pinch,
    Semi,
}

#[derive(Debug, Clone, Default)]
pub struct Tuplet {
    pub enters: i32,
    pub times: i32,
}

#[derive(Debug, Clone)]
pub struct Duration {
    pub value: i32,
    pub is_dotted: bool,
    pub tuplet: Tuplet,
}

impl Default for Duration {
    fn default() -> Self {
        Duration {
            value: 4,
            is_dotted: false,
            tuplet: Tuplet::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BendPoint {
    pub value: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Bend {
    pub points: Vec<BendPoint>,
}

#[derive(Debug, Clone, Default)]
pub struct NoteEffect {
    pub bend: Option<Bend>,
    pub hammer: bool,
    pub slides: Vec<SlideType>,
    pub harmonic: Option<HarmonicKind>,
    pub palm_mute: bool,
    pub accentuated_note: bool,
    pub heavy_accentuated_note: bool,
    pub ghost_note: bool,
    pub tremolo_picking: bool,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub note_type: NoteType,
    /// 1-based GP string number (1 = highest).
    pub string: i32,
    /// Fret number.
    pub value: i32,
    pub effect: NoteEffect,
}

#[derive(Debug, Clone, Default)]
pub struct MixTableItem {
    pub value: i32,
}

#[derive(Debug, Clone, Default)]
pub struct MixTableChange {
    pub tempo: Option<MixTableItem>,
}

#[derive(Debug, Clone, Default)]
pub struct Chord {
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct BeatEffect {
    pub mix_table_change: Option<MixTableChange>,
    pub chord: Option<Chord>,
}

#[derive(Debug, Clone)]
pub struct Beat {
    pub start: i64,
    pub duration: Duration,
    pub notes: Vec<Note>,
    pub effect: BeatEffect,
}

#[derive(Debug, Clone, Default)]
pub struct Voice {
    pub beats: Vec<Beat>,
}

#[derive(Debug, Clone, Default)]
pub struct Measure {
    pub voices: Vec<Voice>,
}

#[derive(Debug, Clone)]
pub struct GpString {
    pub number: i32,
    pub value: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Channel {
    pub instrument: i32,
    pub channel: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Track {
    pub name: String,
    pub is_percussion_track: bool,
    pub channel: Option<Channel>,
    pub strings: Vec<GpString>,
    pub measures: Vec<Measure>,
}

#[derive(Debug, Clone, Default)]
pub struct TimeSignature {
    pub numerator: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Marker {
    pub title: String,
}

#[derive(Debug, Clone, Default)]
pub struct MeasureHeader {
    pub start: i64,
    pub number: i32,
    pub time_signature: TimeSignature,
    pub marker: Option<Marker>,
}

#[derive(Debug, Clone, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub copyright: String,
    pub tempo: f64,
    pub tracks: Vec<Track>,
    pub measure_headers: Vec<MeasureHeader>,
}

/// Parse a Guitar Pro file into a [`Song`]. Stub — see module docs.
pub fn parse(gp_path: &str) -> Result<Song, String> {
    let _ = gp_path;
    Err(
        "Guitar Pro parsing is not implemented in the Rust port yet; \
         provide a Song via the data model or wire up a GP parser."
            .to_string(),
    )
}

// ─── Rocksmith intermediate types ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TempoEvent {
    pub tick: i64,
    pub tempo: f64, // BPM
}

#[derive(Debug, Clone, Default)]
pub struct RsNote {
    pub time: f64,
    pub string: i32,
    pub fret: i32,
    pub sustain: f64,
    pub bend: f64,
    pub slide_to: i32,
    pub slide_unpitch_to: i32,
    pub hammer_on: bool,
    pub pull_off: bool,
    pub harmonic: bool,
    pub harmonic_pinch: bool,
    pub palm_mute: bool,
    pub mute: bool,
    pub accent: bool,
    pub tremolo: bool,
    pub tap: bool,
    pub link_next: bool,
}

impl RsNote {
    fn new(time: f64, string: i32, fret: i32) -> Self {
        RsNote {
            time,
            string,
            fret,
            slide_to: -1,
            slide_unpitch_to: -1,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct RsChord {
    pub time: f64,
    pub template_idx: usize,
    pub notes: Vec<RsNote>,
}

#[derive(Debug, Clone)]
pub struct RsAnchor {
    pub time: f64,
    pub fret: i32,
    pub width: i32,
}

#[derive(Debug, Clone)]
pub struct RsBeat {
    pub time: f64,
    pub measure: i32, // -1 for non-downbeats
}

#[derive(Debug, Clone)]
pub struct RsSection {
    pub name: String,
    pub time: f64,
    pub number: i32,
}

#[derive(Debug, Clone)]
pub struct ChordTemplate {
    pub name: String,
    pub frets: Vec<i32>,
    pub fingers: Vec<i32>,
}

// ─── Tempo / timing helpers ─────────────────────────────────────────────────

fn build_tempo_map(song: &Song) -> Vec<TempoEvent> {
    let mut events = vec![TempoEvent {
        tick: 0,
        tempo: song.tempo,
    }];

    for track in &song.tracks {
        for measure in &track.measures {
            for voice in &measure.voices {
                for beat in &voice.beats {
                    if let Some(mtc) = beat
                        .effect
                        .mix_table_change
                        .as_ref()
                        .and_then(|m| m.tempo.as_ref())
                    {
                        if mtc.value > 0 {
                            events.push(TempoEvent {
                                tick: beat.start,
                                tempo: mtc.value as f64,
                            });
                        }
                    }
                }
            }
        }
    }

    events.sort_by_key(|e| e.tick);
    // Deduplicate by tick (keep first at each tick).
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for e in events {
        if seen.insert(e.tick) {
            unique.push(e);
        }
    }
    unique
}

fn tick_to_seconds(tick: i64, tempo_map: &[TempoEvent]) -> f64 {
    let mut seconds = 0.0;
    let mut prev_tick = 0i64;
    let mut prev_tempo = tempo_map[0].tempo;

    for event in tempo_map {
        if event.tick >= tick {
            break;
        }
        let dt = (event.tick - prev_tick) as f64 / GP_TICKS_PER_QUARTER as f64 * (60.0 / prev_tempo);
        seconds += dt;
        prev_tick = event.tick;
        prev_tempo = event.tempo;
    }

    let dt = (tick - prev_tick) as f64 / GP_TICKS_PER_QUARTER as f64 * (60.0 / prev_tempo);
    seconds + dt
}

fn duration_to_seconds(duration: &Duration, tempo: f64) -> f64 {
    let mut beats = 4.0 / duration.value as f64;
    if duration.is_dotted {
        beats *= 1.5;
    }
    if duration.tuplet.enters > 0 && duration.tuplet.times > 0 {
        beats *= duration.tuplet.times as f64 / duration.tuplet.enters as f64;
    }
    beats * (60.0 / tempo)
}

fn tempo_at_tick(tick: i64, tempo_map: &[TempoEvent]) -> f64 {
    let mut result = tempo_map[0].tempo;
    for event in tempo_map {
        if event.tick > tick {
            break;
        }
        result = event.tempo;
    }
    result
}

/// Convert GP string number (1 = high) to RS string index (0 = low).
fn gp_string_to_rs(gp_string: i32, num_strings: i32) -> i32 {
    num_strings - gp_string
}

fn compute_tuning(track: &Track) -> Vec<i32> {
    let num = track.strings.len();
    let standard: Vec<i32> = if num == 4 {
        STANDARD_TUNING_4.to_vec()
    } else {
        STANDARD_TUNING_6[..num.min(STANDARD_TUNING_6.len())].to_vec()
    };

    let mut offsets = vec![0i32; num];
    for gp_str in &track.strings {
        let rs_idx = gp_string_to_rs(gp_str.number, num as i32) as usize;
        let std_midi = standard
            .get((gp_str.number - 1) as usize)
            .copied()
            .unwrap_or(0);
        if rs_idx < offsets.len() {
            offsets[rs_idx] = gp_str.value - std_midi;
        }
    }
    offsets
}

fn f3(x: f64) -> String {
    format!("{:.3}", x)
}

// ─── Instrument classification ───────────────────────────────────────────────

fn is_keys_instrument(inst: i32) -> bool {
    (0..8).contains(&inst) || (16..24).contains(&inst) || (80..=83).contains(&inst)
}

const KEYS_NAME_KEYWORDS: [&str; 9] = [
    "piano", "keys", "keyboard", "synth", "organ", "rhodes", "wurlitzer", "clav", "epiano",
];
const DRUMS_NAME_KEYWORDS: [&str; 5] = ["drums", "drum", "percussion", "drum kit", "drumkit"];

/// GM drum mapping: MIDI note -> drum piece name.
pub fn gm_drum_map(note: i32) -> Option<&'static str> {
    match note {
        35 | 36 => Some("Kick"),
        38 | 40 => Some("Snare"),
        42 | 44 | 46 => Some("HiHat"),
        48 | 50 => Some("Tom1"),
        45 | 47 => Some("Tom2"),
        41 | 43 => Some("Tom3"),
        49 | 57 => Some("Crash"),
        51 | 59 => Some("Ride"),
        _ => None,
    }
}

pub fn is_piano_track(track: &Track) -> bool {
    if track.is_percussion_track {
        return false;
    }
    if let Some(ch) = &track.channel {
        if is_keys_instrument(ch.instrument) {
            return true;
        }
    }
    let name_low = track.name.to_lowercase();
    KEYS_NAME_KEYWORDS.iter().any(|kw| name_low.contains(kw))
}

pub fn is_drum_track(track: &Track) -> bool {
    if track.is_percussion_track {
        return true;
    }
    if let Some(ch) = &track.channel {
        if ch.channel == 9 {
            return true;
        }
    }
    let name_low = track.name.to_lowercase();
    DRUMS_NAME_KEYWORDS.iter().any(|kw| name_low.contains(kw))
}

// ─── Track listing / auto-selection ──────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackInfo {
    pub index: usize,
    pub name: String,
    pub strings: usize,
    pub is_percussion: bool,
    pub is_piano: bool,
    pub is_drums: bool,
    pub instrument: i32,
    pub notes: usize,
}

pub fn list_tracks(gp_path: &str) -> Result<Vec<TrackInfo>, String> {
    let song = parse(gp_path)?;
    Ok(list_tracks_from_song(&song))
}

fn list_tracks_from_song(song: &Song) -> Vec<TrackInfo> {
    let mut tracks = Vec::new();
    for (i, track) in song.tracks.iter().enumerate() {
        let mut note_count = 0usize;
        for measure in &track.measures {
            for voice in &measure.voices {
                for beat in &voice.beats {
                    note_count += beat.notes.len();
                }
            }
        }
        let instrument = track.channel.as_ref().map(|c| c.instrument).unwrap_or(-1);
        tracks.push(TrackInfo {
            index: i,
            name: track.name.clone(),
            strings: track.strings.len(),
            is_percussion: track.is_percussion_track,
            is_piano: is_piano_track(track),
            is_drums: is_drum_track(track),
            instrument,
            notes: note_count,
        });
    }
    tracks
}

/// Auto-select guitar/bass/keys/drums tracks and assign Rocksmith arrangement
/// names. Returns `(track_indices, name_map)`.
pub fn auto_select_tracks(gp_path: &str) -> Result<(Vec<usize>, HashMap<usize, String>), String> {
    let song = parse(gp_path)?;
    Ok(auto_select_from_song(&song))
}

fn auto_select_from_song(song: &Song) -> (Vec<usize>, HashMap<usize, String>) {
    let tracks = list_tracks_from_song(song);
    let guitar_keywords = [
        "guitar", "gtr", "lead", "rhythm", "rhy", "solo", "clean", "distort", "acoustic", "elec",
    ];
    let bass_keywords = ["bass"];
    let skip_keywords = [
        "string", "choir", "brass", "brite", "flute", "violin", "cello", "horn",
    ];

    let mut selected: Vec<(usize, &'static str)> = Vec::new();
    for t in &tracks {
        if t.notes == 0 {
            continue;
        }
        if t.is_drums {
            selected.push((t.index, "drums"));
            continue;
        }
        if t.is_piano {
            selected.push((t.index, "keys"));
            continue;
        }

        let name_low = t.name.to_lowercase();

        if t.strings == 4 {
            selected.push((t.index, "bass"));
            continue;
        }

        if skip_keywords.iter().any(|kw| name_low.contains(kw)) {
            continue;
        }

        if bass_keywords.iter().any(|kw| name_low.contains(kw)) {
            selected.push((t.index, "bass"));
        } else if guitar_keywords.iter().any(|kw| name_low.contains(kw)) {
            selected.push((t.index, "guitar"));
        } else if t.strings == 6 {
            selected.push((t.index, "guitar"));
        }
    }

    if selected.is_empty() {
        for t in &tracks {
            if !t.is_percussion && t.notes > 0 {
                let role = if t.strings == 4 { "bass" } else { "guitar" };
                selected.push((t.index, role));
            }
        }
    }

    let mut track_indices = Vec::new();
    let mut name_map = HashMap::new();
    let mut lead_count = 0;
    let mut rhythm_count = 0;
    let mut bass_count = 0;
    let mut keys_count = 0;
    let mut drums_count = 0;

    for (idx, role) in selected {
        track_indices.push(idx);
        match role {
            "drums" => {
                drums_count += 1;
                name_map.insert(
                    idx,
                    if drums_count == 1 {
                        "Drums".to_string()
                    } else {
                        format!("Drums {drums_count}")
                    },
                );
            }
            "keys" => {
                keys_count += 1;
                name_map.insert(
                    idx,
                    if keys_count == 1 {
                        "Keys".to_string()
                    } else {
                        format!("Keys {keys_count}")
                    },
                );
            }
            "bass" => {
                bass_count += 1;
                name_map.insert(
                    idx,
                    if bass_count == 1 {
                        "Bass".to_string()
                    } else {
                        format!("Bass {bass_count}")
                    },
                );
            }
            _ => {
                if lead_count == 0 {
                    lead_count += 1;
                    name_map.insert(idx, "Lead".to_string());
                } else {
                    rhythm_count += 1;
                    name_map.insert(
                        idx,
                        if rhythm_count == 1 {
                            "Rhythm".to_string()
                        } else {
                            "Combo".to_string()
                        },
                    );
                }
            }
        }
    }

    (track_indices, name_map)
}

// ─── Beat / section collection ──────────────────────────────────────────────

fn collect_beats(song: &Song, tempo_map: &[TempoEvent], audio_offset: f64) -> Vec<RsBeat> {
    let mut beats = Vec::new();
    for mh in &song.measure_headers {
        let t = tick_to_seconds(mh.start, tempo_map) + audio_offset;
        beats.push(RsBeat {
            time: t,
            measure: mh.number,
        });
        let num_beats = mh.time_signature.numerator.max(1);
        for b in 1..num_beats {
            let sub_tick = mh.start + b as i64 * GP_TICKS_PER_QUARTER;
            let sub_t = tick_to_seconds(sub_tick, tempo_map) + audio_offset;
            beats.push(RsBeat {
                time: sub_t,
                measure: -1,
            });
        }
    }
    beats.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    beats
}

fn collect_sections(song: &Song, tempo_map: &[TempoEvent], audio_offset: f64) -> Vec<RsSection> {
    let mut sections = Vec::new();
    let mut counts: HashMap<String, i32> = HashMap::new();
    for mh in &song.measure_headers {
        if let Some(marker) = &mh.marker {
            if !marker.title.is_empty() {
                let name = marker.title.trim().to_lowercase().replace(' ', "");
                let c = counts.entry(name.clone()).or_insert(0);
                *c += 1;
                let t = tick_to_seconds(mh.start, tempo_map) + audio_offset;
                sections.push(RsSection {
                    name,
                    time: t,
                    number: *c,
                });
            }
        }
    }
    if sections.is_empty() {
        sections.push(RsSection {
            name: "default".to_string(),
            time: audio_offset,
            number: 1,
        });
    }
    sections
}

fn song_length(song: &Song, tempo_map: &[TempoEvent], audio_offset: f64) -> f64 {
    if let Some(last_mh) = song.measure_headers.last() {
        tick_to_seconds(
            last_mh.start + last_mh.time_signature.numerator as i64 * GP_TICKS_PER_QUARTER,
            tempo_map,
        ) + audio_offset
    } else {
        audio_offset
    }
}

// ─── Main conversion ─────────────────────────────────────────────────────────

/// Convert a GP track to a Rocksmith 2014 arrangement XML string.
pub fn convert_track(
    song: &Song,
    track_index: usize,
    audio_offset: f64,
    arrangement_name: &str,
    force_standard_tuning: bool,
) -> Result<String, String> {
    let track = &song.tracks[track_index];
    let num_strings = track.strings.len() as i32;
    let is_bass = num_strings == 4;
    let tempo_map = build_tempo_map(song);
    let tuning = if force_standard_tuning {
        vec![0i32; num_strings as usize]
    } else {
        compute_tuning(track)
    };

    let arrangement_name = if !arrangement_name.is_empty() {
        arrangement_name.to_string()
    } else {
        let name = track.name.trim();
        let low = name.to_lowercase();
        if is_bass || low.contains("bass") {
            "Bass".to_string()
        } else if low.contains("rhythm") || low.contains("rhy") {
            "Rhythm".to_string()
        } else {
            "Lead".to_string()
        }
    };

    let beats = collect_beats(song, &tempo_map, audio_offset);
    let sections = collect_sections(song, &tempo_map, audio_offset);

    let mut rs_notes: Vec<RsNote> = Vec::new();
    let mut rs_chords: Vec<RsChord> = Vec::new();
    let mut chord_templates: Vec<ChordTemplate> = Vec::new();
    let mut chord_template_map: HashMap<Vec<i32>, usize> = HashMap::new();

    for measure in &track.measures {
        for voice in &measure.voices {
            for beat in &voice.beats {
                if beat.notes.is_empty() {
                    continue;
                }

                let t = tick_to_seconds(beat.start, &tempo_map) + audio_offset;
                let tempo = tempo_at_tick(beat.start, &tempo_map);
                let dur = duration_to_seconds(&beat.duration, tempo);

                let mut beat_notes: Vec<RsNote> = Vec::new();
                for note in &beat.notes {
                    if note.note_type == NoteType::Rest {
                        continue;
                    }

                    let rs_str = gp_string_to_rs(note.string, num_strings);
                    let mut fret = note.value;
                    if note.note_type == NoteType::Dead {
                        fret = fret.max(0);
                    }

                    let mut rn = RsNote::new(t, rs_str, fret);
                    rn.sustain = if dur > 0.2 { dur } else { 0.0 };
                    rn.mute = note.note_type == NoteType::Dead;

                    let eff = &note.effect;
                    if let Some(bend) = &eff.bend {
                        if !bend.points.is_empty() {
                            let max_bend =
                                bend.points.iter().map(|p| p.value).max().unwrap_or(0);
                            rn.bend = max_bend as f64 / 100.0;
                        }
                    }
                    if eff.hammer {
                        rn.hammer_on = true;
                    }
                    for slide in &eff.slides {
                        if matches!(slide, SlideType::ShiftSlideTo | SlideType::LegatoSlideTo) {
                            rn.link_next = true;
                        }
                    }
                    if let Some(h) = eff.harmonic {
                        if h == HarmonicKind::Pinch {
                            rn.harmonic_pinch = true;
                        } else {
                            rn.harmonic = true;
                        }
                    }
                    if eff.palm_mute {
                        rn.palm_mute = true;
                    }
                    if eff.accentuated_note || eff.heavy_accentuated_note {
                        rn.accent = true;
                    }
                    if eff.ghost_note {
                        rn.mute = true;
                    }
                    if eff.tremolo_picking {
                        rn.tremolo = true;
                    }

                    beat_notes.push(rn);
                }

                if beat_notes.is_empty() {
                    continue;
                }

                if beat_notes.len() == 1 {
                    rs_notes.push(beat_notes.into_iter().next().unwrap());
                } else {
                    let width = 6.max(num_strings as usize);
                    let mut frets = vec![-1i32; width];
                    for n in &beat_notes {
                        if n.string >= 0 && (n.string as usize) < frets.len() {
                            frets[n.string as usize] = n.fret;
                        }
                    }
                    let idx = *chord_template_map.entry(frets.clone()).or_insert_with(|| {
                        let chord_name = beat
                            .effect
                            .chord
                            .as_ref()
                            .map(|c| c.name.clone())
                            .unwrap_or_default();
                        let i = chord_templates.len();
                        chord_templates.push(ChordTemplate {
                            name: chord_name,
                            frets: frets.clone(),
                            fingers: vec![-1; frets.len()],
                        });
                        i
                    });
                    rs_chords.push(RsChord {
                        time: t,
                        template_idx: idx,
                        notes: beat_notes,
                    });
                }
            }
        }
    }

    rs_notes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    rs_chords.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());

    // ── Anchors ──────────────────────────────────────────────────────────
    let mut all_timed_frets: Vec<(f64, i32)> = rs_notes
        .iter()
        .filter(|n| n.fret > 0)
        .map(|n| (n.time, n.fret))
        .collect();
    for c in &rs_chords {
        for cn in &c.notes {
            if cn.fret > 0 {
                all_timed_frets.push((cn.time, cn.fret));
            }
        }
    }
    all_timed_frets.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap()
            .then(a.1.cmp(&b.1))
    });

    let mut anchors: Vec<RsAnchor> = Vec::new();
    let first_fret = all_timed_frets.first().map(|f| f.1).unwrap_or(1);
    anchors.push(RsAnchor {
        time: audio_offset,
        fret: 1.max(first_fret - 1),
        width: 4,
    });
    for (t, fret) in &all_timed_frets {
        let last = anchors.last().unwrap();
        let anchor_lo = last.fret;
        let anchor_hi = anchor_lo + last.width;
        if *fret < anchor_lo || *fret > anchor_hi {
            let new_fret = 1.max(fret - 1);
            if new_fret != anchors.last().unwrap().fret {
                anchors.push(RsAnchor {
                    time: *t,
                    fret: new_fret,
                    width: 4,
                });
            }
        }
    }

    let length = song_length(song, &tempo_map, audio_offset);

    build_xml(
        &pick(&song.title, "Untitled"),
        &pick(&song.artist, "Unknown"),
        &song.album,
        &song.copyright,
        &arrangement_name,
        &tuning,
        length,
        audio_offset,
        &beats,
        &sections,
        &rs_notes,
        &rs_chords,
        &chord_templates,
        &anchors,
        song.tempo,
    )
}

/// Convert a GP piano/keyboard track to Rocksmith XML using MIDI encoding.
///
/// Encodes MIDI notes as `string = midi // 24`, `fret = midi % 24`.
pub fn convert_piano_track(
    song: &Song,
    track_index: usize,
    audio_offset: f64,
    arrangement_name: &str,
) -> Result<String, String> {
    let track = &song.tracks[track_index];
    let tempo_map = build_tempo_map(song);

    let beats = collect_beats(song, &tempo_map, audio_offset);
    let sections = collect_sections(song, &tempo_map, audio_offset);

    let mut rs_notes: Vec<RsNote> = Vec::new();
    let mut rs_chords: Vec<RsChord> = Vec::new();
    let mut chord_templates: Vec<ChordTemplate> = Vec::new();
    let mut chord_template_map: HashMap<Vec<i32>, usize> = HashMap::new();

    for measure in &track.measures {
        for voice in &measure.voices {
            for beat in &voice.beats {
                if beat.notes.is_empty() {
                    continue;
                }
                let t = tick_to_seconds(beat.start, &tempo_map) + audio_offset;
                let tempo = tempo_at_tick(beat.start, &tempo_map);
                let dur = duration_to_seconds(&beat.duration, tempo);

                let mut beat_notes: Vec<RsNote> = Vec::new();
                for note in &beat.notes {
                    if note.note_type == NoteType::Rest {
                        continue;
                    }
                    let gp_str_idx = note.string;
                    let base_midi = if (gp_str_idx as usize) <= track.strings.len()
                        && gp_str_idx >= 1
                    {
                        track.strings[(gp_str_idx - 1) as usize].value
                    } else {
                        60
                    };
                    let midi_note = base_midi + note.value;
                    let rs_string = midi_note.div_euclid(24);
                    let rs_fret = midi_note.rem_euclid(24);

                    let mut rn = RsNote::new(t, rs_string, rs_fret);
                    rn.sustain = if dur > 0.15 { dur } else { 0.0 };
                    rn.mute = note.note_type == NoteType::Dead;
                    let eff = &note.effect;
                    if eff.accentuated_note || eff.heavy_accentuated_note {
                        rn.accent = true;
                    }
                    beat_notes.push(rn);
                }

                if beat_notes.is_empty() {
                    continue;
                }
                if beat_notes.len() == 1 {
                    rs_notes.push(beat_notes.into_iter().next().unwrap());
                } else {
                    let mut frets = vec![-1i32; 6];
                    for n in &beat_notes {
                        if n.string >= 0 && (n.string as usize) < 6 {
                            frets[n.string as usize] = n.fret;
                        }
                    }
                    let idx = *chord_template_map.entry(frets.clone()).or_insert_with(|| {
                        let chord_name = beat
                            .effect
                            .chord
                            .as_ref()
                            .map(|c| c.name.clone())
                            .unwrap_or_default();
                        let i = chord_templates.len();
                        chord_templates.push(ChordTemplate {
                            name: chord_name,
                            frets: frets.clone(),
                            fingers: vec![-1; 6],
                        });
                        i
                    });
                    rs_chords.push(RsChord {
                        time: t,
                        template_idx: idx,
                        notes: beat_notes,
                    });
                }
            }
        }
    }

    rs_notes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    rs_chords.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());

    let anchors = vec![RsAnchor {
        time: audio_offset,
        fret: 1,
        width: 24,
    }];

    let length = song_length(song, &tempo_map, audio_offset);

    build_xml(
        &pick(&song.title, "Untitled"),
        &pick(&song.artist, "Unknown"),
        &song.album,
        &song.copyright,
        arrangement_name,
        &vec![0i32; 6],
        length,
        audio_offset,
        &beats,
        &sections,
        &rs_notes,
        &rs_chords,
        &chord_templates,
        &anchors,
        song.tempo,
    )
}

/// Convert a GP drum/percussion track to Rocksmith XML using MIDI encoding.
pub fn convert_drum_track(
    song: &Song,
    track_index: usize,
    audio_offset: f64,
    arrangement_name: &str,
) -> Result<String, String> {
    let track = &song.tracks[track_index];
    let tempo_map = build_tempo_map(song);

    let beats = collect_beats(song, &tempo_map, audio_offset);
    let sections = collect_sections(song, &tempo_map, audio_offset);

    let mut rs_notes: Vec<RsNote> = Vec::new();
    let mut rs_chords: Vec<RsChord> = Vec::new();
    let mut chord_templates: Vec<ChordTemplate> = Vec::new();
    let mut chord_template_map: HashMap<Vec<i32>, usize> = HashMap::new();

    for measure in &track.measures {
        for voice in &measure.voices {
            for beat in &voice.beats {
                if beat.notes.is_empty() {
                    continue;
                }
                let t = tick_to_seconds(beat.start, &tempo_map) + audio_offset;

                let mut beat_notes: Vec<RsNote> = Vec::new();
                for note in &beat.notes {
                    if note.note_type == NoteType::Rest {
                        continue;
                    }
                    let gp_str_idx = note.string;
                    let midi_note = if (gp_str_idx as usize) <= track.strings.len()
                        && gp_str_idx >= 1
                    {
                        track.strings[(gp_str_idx - 1) as usize].value + note.value
                    } else {
                        note.value
                    };
                    if gm_drum_map(midi_note).is_none() {
                        continue;
                    }
                    let rs_string = midi_note.div_euclid(24);
                    let rs_fret = midi_note.rem_euclid(24);

                    let mut rn = RsNote::new(t, rs_string, rs_fret);
                    rn.sustain = 0.0;
                    let eff = &note.effect;
                    if eff.accentuated_note || eff.heavy_accentuated_note {
                        rn.accent = true;
                    }
                    if eff.ghost_note {
                        rn.mute = true;
                    }
                    beat_notes.push(rn);
                }

                if beat_notes.is_empty() {
                    continue;
                }
                if beat_notes.len() == 1 {
                    rs_notes.push(beat_notes.into_iter().next().unwrap());
                } else {
                    let mut frets = vec![-1i32; 6];
                    for n in &beat_notes {
                        if n.string >= 0 && (n.string as usize) < 6 {
                            frets[n.string as usize] = n.fret;
                        }
                    }
                    let idx = *chord_template_map.entry(frets.clone()).or_insert_with(|| {
                        let i = chord_templates.len();
                        chord_templates.push(ChordTemplate {
                            name: String::new(),
                            frets: frets.clone(),
                            fingers: vec![-1; 6],
                        });
                        i
                    });
                    rs_chords.push(RsChord {
                        time: t,
                        template_idx: idx,
                        notes: beat_notes,
                    });
                }
            }
        }
    }

    rs_notes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    rs_chords.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());

    let anchors = vec![RsAnchor {
        time: audio_offset,
        fret: 1,
        width: 24,
    }];

    let length = song_length(song, &tempo_map, audio_offset);

    build_xml(
        &pick(&song.title, "Untitled"),
        &pick(&song.artist, "Unknown"),
        &song.album,
        &song.copyright,
        arrangement_name,
        &vec![0i32; 6],
        length,
        audio_offset,
        &beats,
        &sections,
        &rs_notes,
        &rs_chords,
        &chord_templates,
        &anchors,
        song.tempo,
    )
}

fn pick(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

/// Convert a GP file to Rocksmith XMLs, writing them into `output_dir`.
pub fn convert_file(
    gp_path: &str,
    output_dir: &str,
    track_indices: Option<Vec<usize>>,
    audio_offset: f64,
    arrangement_names: Option<HashMap<usize, String>>,
    force_standard_tuning: bool,
) -> Result<Vec<String>, String> {
    let song = parse(gp_path)?;
    let out = PathBuf::from(output_dir);
    std::fs::create_dir_all(&out).map_err(|e| format!("failed to create {output_dir}: {e}"))?;

    let (track_indices, names): (Vec<usize>, HashMap<usize, String>) = match track_indices {
        Some(ti) => (ti, arrangement_names.unwrap_or_default()),
        None => {
            let (ti, auto_names) = auto_select_from_song(&song);
            let names = arrangement_names.unwrap_or(auto_names);
            (ti, names)
        }
    };

    let mut output_files = Vec::new();

    for idx in track_indices {
        let track = &song.tracks[idx];
        let arr_name = names.get(&idx).cloned().unwrap_or_default();
        let arr_lower = arr_name.to_lowercase();

        let xml_str = if is_drum_track(track) || arr_lower.starts_with("drums") {
            let name = if arr_name.is_empty() { "Drums" } else { arr_name.as_str() };
            convert_drum_track(&song, idx, audio_offset, name)?
        } else if is_piano_track(track) || arr_lower.starts_with("keys") {
            let name = if arr_name.is_empty() { "Keys" } else { arr_name.as_str() };
            convert_piano_track(&song, idx, audio_offset, name)?
        } else {
            convert_track(&song, idx, audio_offset, &arr_name, force_standard_tuning)?
        };

        let safe_name = track.name.trim().replace(' ', "_").replace('/', "_");
        let arr_part = if arr_name.is_empty() {
            "arr".to_string()
        } else {
            arr_name.clone()
        };
        let filename = format!("{safe_name}_{arr_part}.xml");
        let filepath = out.join(&filename);
        std::fs::write(&filepath, xml_str)
            .map_err(|e| format!("failed to write {}: {e}", filepath.display()))?;
        output_files.push(filepath.to_string_lossy().into_owned());
    }

    Ok(output_files)
}

// ─── XML generation ──────────────────────────────────────────────────────────

type Wr = Writer<Cursor<Vec<u8>>>;

fn el_text(w: &mut Wr, name: &str, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    w.write_event(Event::Start(BytesStart::new(name)))?;
    w.write_event(Event::Text(BytesText::new(text)))?;
    w.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn el_empty(
    w: &mut Wr,
    name: &str,
    attrs: &[(&str, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut e = BytesStart::new(name);
    for (k, v) in attrs {
        e.push_attribute((*k, v.as_str()));
    }
    w.write_event(Event::Empty(e))?;
    Ok(())
}

fn el_start(
    w: &mut Wr,
    name: &str,
    attrs: &[(&str, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut e = BytesStart::new(name);
    for (k, v) in attrs {
        e.push_attribute((*k, v.as_str()));
    }
    w.write_event(Event::Start(e))?;
    Ok(())
}

fn el_end(w: &mut Wr, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    w.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn b(v: bool) -> String {
    if v { "1".to_string() } else { "0".to_string() }
}

#[allow(clippy::too_many_arguments)]
fn build_xml(
    title: &str,
    artist: &str,
    album: &str,
    year: &str,
    arrangement: &str,
    tuning: &[i32],
    length: f64,
    audio_offset: f64,
    beats: &[RsBeat],
    sections: &[RsSection],
    notes: &[RsNote],
    chords: &[RsChord],
    chord_templates: &[ChordTemplate],
    anchors: &[RsAnchor],
    tempo: f64,
) -> Result<String, String> {
    build_xml_inner(
        title,
        artist,
        album,
        year,
        arrangement,
        tuning,
        length,
        audio_offset,
        beats,
        sections,
        notes,
        chords,
        chord_templates,
        anchors,
        tempo,
    )
    .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn build_xml_inner(
    title: &str,
    artist: &str,
    album: &str,
    year: &str,
    arrangement: &str,
    tuning: &[i32],
    length: f64,
    audio_offset: f64,
    beats: &[RsBeat],
    sections: &[RsSection],
    notes: &[RsNote],
    chords: &[RsChord],
    chord_templates: &[ChordTemplate],
    anchors: &[RsAnchor],
    tempo: f64,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut w: Wr = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);

    el_start(&mut w, "song", &[("version", "7".to_string())])?;

    el_text(&mut w, "title", title)?;
    el_text(&mut w, "arrangement", arrangement)?;
    el_text(&mut w, "offset", &f3(audio_offset))?;
    el_text(&mut w, "songLength", &f3(length))?;
    el_text(
        &mut w,
        "startBeat",
        &beats.first().map(|b| f3(b.time)).unwrap_or_else(|| "0.000".to_string()),
    )?;
    el_text(&mut w, "averageTempo", &format!("{}", tempo))?;
    el_text(&mut w, "artistName", artist)?;
    el_text(&mut w, "albumName", album)?;
    el_text(&mut w, "albumYear", year)?;

    // Tuning
    let tuning_attrs: Vec<(&str, String)> = (0..6)
        .map(|i| {
            let key: &str = match i {
                0 => "string0",
                1 => "string1",
                2 => "string2",
                3 => "string3",
                4 => "string4",
                _ => "string5",
            };
            (key, format!("{}", tuning.get(i).copied().unwrap_or(0)))
        })
        .collect();
    el_empty(&mut w, "tuning", &tuning_attrs)?;
    el_text(&mut w, "capo", "0")?;

    // Ebeats
    el_start(&mut w, "ebeats", &[("count", format!("{}", beats.len()))])?;
    for bt in beats {
        el_empty(
            &mut w,
            "ebeat",
            &[
                ("time", f3(bt.time)),
                ("measure", format!("{}", bt.measure)),
            ],
        )?;
    }
    el_end(&mut w, "ebeats")?;

    // Sections
    el_start(&mut w, "sections", &[("count", format!("{}", sections.len()))])?;
    for s in sections {
        el_empty(
            &mut w,
            "section",
            &[
                ("name", s.name.clone()),
                ("number", format!("{}", s.number)),
                ("startTime", f3(s.time)),
            ],
        )?;
    }
    el_end(&mut w, "sections")?;

    // Phrases
    el_start(&mut w, "phrases", &[("count", format!("{}", sections.len()))])?;
    for s in sections {
        el_empty(
            &mut w,
            "phrase",
            &[
                ("disparity", "0".to_string()),
                ("ignore", "0".to_string()),
                ("maxDifficulty", "0".to_string()),
                ("name", s.name.clone()),
                ("solo", "0".to_string()),
            ],
        )?;
    }
    el_end(&mut w, "phrases")?;

    // Phrase iterations
    el_start(
        &mut w,
        "phraseIterations",
        &[("count", format!("{}", sections.len()))],
    )?;
    for (i, s) in sections.iter().enumerate() {
        el_empty(
            &mut w,
            "phraseIteration",
            &[("time", f3(s.time)), ("phraseId", format!("{}", i))],
        )?;
    }
    el_end(&mut w, "phraseIterations")?;

    // Chord templates
    el_start(
        &mut w,
        "chordTemplates",
        &[("count", format!("{}", chord_templates.len()))],
    )?;
    for ct in chord_templates {
        let mut attrs: Vec<(&str, String)> = vec![("chordName", ct.name.clone())];
        const FRET_KEYS: [&str; 6] = ["fret0", "fret1", "fret2", "fret3", "fret4", "fret5"];
        const FINGER_KEYS: [&str; 6] =
            ["finger0", "finger1", "finger2", "finger3", "finger4", "finger5"];
        for i in 0..6 {
            attrs.push((FRET_KEYS[i], format!("{}", ct.frets.get(i).copied().unwrap_or(-1))));
            attrs.push((
                FINGER_KEYS[i],
                format!("{}", ct.fingers.get(i).copied().unwrap_or(-1)),
            ));
        }
        el_empty(&mut w, "chordTemplate", &attrs)?;
    }
    el_end(&mut w, "chordTemplates")?;

    // Levels
    el_start(&mut w, "levels", &[("count", "1".to_string())])?;
    el_start(&mut w, "level", &[("difficulty", "0".to_string())])?;

    // Notes
    el_start(&mut w, "notes", &[("count", format!("{}", notes.len()))])?;
    for n in notes {
        let bend_str = if n.bend != 0.0 {
            format!("{:.1}", n.bend)
        } else {
            "0".to_string()
        };
        el_empty(
            &mut w,
            "note",
            &[
                ("time", f3(n.time)),
                ("string", format!("{}", n.string)),
                ("fret", format!("{}", n.fret)),
                ("sustain", f3(n.sustain)),
                ("bend", bend_str),
                ("hammerOn", b(n.hammer_on)),
                ("pullOff", b(n.pull_off)),
                ("slideTo", format!("{}", n.slide_to)),
                ("slideUnpitchTo", format!("{}", n.slide_unpitch_to)),
                ("harmonic", b(n.harmonic)),
                ("harmonicPinch", b(n.harmonic_pinch)),
                ("palmMute", b(n.palm_mute)),
                ("mute", b(n.mute)),
                ("tremolo", b(n.tremolo)),
                ("accent", b(n.accent)),
                ("linkNext", b(n.link_next)),
                ("tap", b(n.tap)),
                ("ignore", "0".to_string()),
            ],
        )?;
    }
    el_end(&mut w, "notes")?;

    // Chords
    el_start(&mut w, "chords", &[("count", format!("{}", chords.len()))])?;
    for ch in chords {
        el_start(
            &mut w,
            "chord",
            &[
                ("time", f3(ch.time)),
                ("chordId", format!("{}", ch.template_idx)),
                ("highDensity", "0".to_string()),
                ("strum", "down".to_string()),
            ],
        )?;
        for cn in &ch.notes {
            el_empty(
                &mut w,
                "chordNote",
                &[
                    ("time", f3(cn.time)),
                    ("string", format!("{}", cn.string)),
                    ("fret", format!("{}", cn.fret)),
                    ("sustain", f3(cn.sustain)),
                    ("bend", "0".to_string()),
                    ("hammerOn", "0".to_string()),
                    ("pullOff", "0".to_string()),
                    ("slideTo", "-1".to_string()),
                    ("slideUnpitchTo", "-1".to_string()),
                    ("harmonic", "0".to_string()),
                    ("harmonicPinch", "0".to_string()),
                    ("palmMute", b(cn.palm_mute)),
                    ("mute", b(cn.mute)),
                    ("tremolo", "0".to_string()),
                    ("accent", "0".to_string()),
                    ("linkNext", "0".to_string()),
                    ("tap", "0".to_string()),
                    ("ignore", "0".to_string()),
                ],
            )?;
        }
        el_end(&mut w, "chord")?;
    }
    el_end(&mut w, "chords")?;

    // Anchors
    el_start(&mut w, "anchors", &[("count", format!("{}", anchors.len()))])?;
    for a in anchors {
        el_empty(
            &mut w,
            "anchor",
            &[
                ("time", f3(a.time)),
                ("fret", format!("{}", a.fret)),
                ("width", format!("{}", a.width)),
            ],
        )?;
    }
    el_end(&mut w, "anchors")?;

    // Empty hand shapes
    el_empty(&mut w, "handShapes", &[("count", "0".to_string())])?;

    el_end(&mut w, "level")?;
    el_end(&mut w, "levels")?;
    el_end(&mut w, "song")?;

    let bytes = w.into_inner().into_inner();
    Ok(String::from_utf8(bytes)?)
}
