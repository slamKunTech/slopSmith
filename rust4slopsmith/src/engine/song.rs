//! Rocksmith 2014 arrangement data models, wire-format (de)serialization,
//! the arrangement XML parser, and `load_song`. Port of `lib/song.py` (1001
//! lines). This is the central data-structure module: the highway WebSocket
//! (Wave 4) and the sloppak loader both consume its wire format.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

// ── Data models ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Note {
    pub time: f64,
    pub string: i64,
    pub fret: i64,
    pub sustain: f64,
    pub slide_to: i64,
    pub slide_unpitch_to: i64,
    pub bend: f64,
    pub hammer_on: bool,
    pub pull_off: bool,
    pub harmonic: bool,
    pub harmonic_pinch: bool,
    pub palm_mute: bool,
    pub mute: bool,
    pub tremolo: bool,
    pub accent: bool,
    pub link_next: bool,
    pub tap: bool,
}

impl Default for Note {
    fn default() -> Self {
        Self {
            time: 0.0,
            string: 0,
            fret: 0,
            sustain: 0.0,
            slide_to: -1,
            slide_unpitch_to: -1,
            bend: 0.0,
            hammer_on: false,
            pull_off: false,
            harmonic: false,
            harmonic_pinch: false,
            palm_mute: false,
            mute: false,
            tremolo: false,
            accent: false,
            link_next: false,
            tap: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChordTemplate {
    pub name: String,
    pub fingers: Vec<i64>,
    pub frets: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct Chord {
    pub time: f64,
    pub chord_id: i64,
    pub notes: Vec<Note>,
    pub high_density: bool,
}

#[derive(Debug, Clone)]
pub struct Anchor {
    pub time: f64,
    pub fret: i64,
    pub width: i64,
}

impl Anchor {
    pub fn new(time: f64, fret: i64) -> Self {
        Self { time, fret, width: 4 }
    }
}

#[derive(Debug, Clone)]
pub struct Beat {
    pub time: f64,
    pub measure: i64, // -1 for non-downbeat
}

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub number: i64,
    pub start_time: f64,
}

#[derive(Debug, Clone)]
pub struct HandShape {
    pub chord_id: i64,
    pub start_time: f64,
    pub end_time: f64,
}

#[derive(Debug, Clone)]
pub struct PhraseLevel {
    pub difficulty: i64,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
}

#[derive(Debug, Clone)]
pub struct Phrase {
    pub start_time: f64,
    pub end_time: f64,
    pub max_difficulty: i64,
    pub levels: Vec<PhraseLevel>,
}

#[derive(Debug, Clone)]
pub struct Arrangement {
    pub name: String,
    pub tuning: Vec<i64>,
    pub capo: i64,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
    pub chord_templates: Vec<ChordTemplate>,
    /// `None` for single-level sources (GP converter, old sloppaks) — frontends
    /// treat missing `phrases` as "no per-phrase difficulty data, slider off".
    pub phrases: Option<Vec<Phrase>>,
}

impl Default for Arrangement {
    fn default() -> Self {
        Self {
            name: String::new(),
            tuning: vec![0; 6],
            capo: 0,
            notes: Vec::new(),
            chords: Vec::new(),
            anchors: Vec::new(),
            hand_shapes: Vec::new(),
            chord_templates: Vec::new(),
            phrases: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: i64,
    pub song_length: f64,
    pub offset: f64,
    pub beats: Vec<Beat>,
    pub sections: Vec<Section>,
    pub arrangements: Vec<Arrangement>,
    pub audio_path: String,
    /// One entry per syllable: `{t, d, w}`.
    pub lyrics: Vec<Value>,
}

// ── Wire format (de)serialization ────────────────────────────────────────────

/// Round to `dp` decimal places, half-to-even, matching Python's `round()` on
/// floats (Rust's `{:.dp}` formatting is also round-half-to-even on the binary
/// value, so this agrees with Python for the same input).
pub fn round_dp(x: f64, dp: usize) -> f64 {
    format!("{:.*}", dp, x).parse().unwrap_or(x)
}

pub fn note_to_wire(n: &Note) -> Value {
    let mut m = Map::new();
    m.insert("t".into(), json!(round_dp(n.time, 3)));
    m.insert("s".into(), json!(n.string));
    m.insert("f".into(), json!(n.fret));
    m.insert("sus".into(), json!(round_dp(n.sustain, 3)));
    m.insert("sl".into(), json!(n.slide_to));
    m.insert("slu".into(), json!(n.slide_unpitch_to));
    // Python: `round(bend,1) if bend else 0` — int 0 when zero, float otherwise.
    m.insert(
        "bn".into(),
        if n.bend != 0.0 { json!(round_dp(n.bend, 1)) } else { json!(0) },
    );
    m.insert("ho".into(), json!(n.hammer_on));
    m.insert("po".into(), json!(n.pull_off));
    m.insert("hm".into(), json!(n.harmonic));
    m.insert("hp".into(), json!(n.harmonic_pinch));
    m.insert("pm".into(), json!(n.palm_mute));
    m.insert("mt".into(), json!(n.mute));
    m.insert("tr".into(), json!(n.tremolo));
    m.insert("ac".into(), json!(n.accent));
    m.insert("tp".into(), json!(n.tap));
    Value::Object(m)
}

/// Chord notes omit their own `t` (the chord carries it). Mirrors
/// `chord_note_to_wire`. Built directly (not `note_to_wire` + `remove("t")`)
/// because `serde_json::Map::remove` uses swap-remove, which would move the
/// last key (`tp`) to the front — diverging from Python's order-preserving
/// `dict.pop("t")`.
pub fn chord_note_to_wire(cn: &Note) -> Value {
    let mut m = Map::new();
    m.insert("s".into(), json!(cn.string));
    m.insert("f".into(), json!(cn.fret));
    m.insert("sus".into(), json!(round_dp(cn.sustain, 3)));
    m.insert("sl".into(), json!(cn.slide_to));
    m.insert("slu".into(), json!(cn.slide_unpitch_to));
    m.insert(
        "bn".into(),
        if cn.bend != 0.0 { json!(round_dp(cn.bend, 1)) } else { json!(0) },
    );
    m.insert("ho".into(), json!(cn.hammer_on));
    m.insert("po".into(), json!(cn.pull_off));
    m.insert("hm".into(), json!(cn.harmonic));
    m.insert("hp".into(), json!(cn.harmonic_pinch));
    m.insert("pm".into(), json!(cn.palm_mute));
    m.insert("mt".into(), json!(cn.mute));
    m.insert("tr".into(), json!(cn.tremolo));
    m.insert("ac".into(), json!(cn.accent));
    m.insert("tp".into(), json!(cn.tap));
    Value::Object(m)
}

pub fn chord_to_wire(c: &Chord) -> Value {
    json!({
        "t": round_dp(c.time, 3),
        "id": c.chord_id,
        "hd": c.high_density,
        "notes": c.notes.iter().map(chord_note_to_wire).collect::<Vec<_>>(),
    })
}

pub fn note_from_wire(d: &Value, time: Option<f64>) -> Note {
    let g = |k: &str| d.get(k);
    let f = |k: &str, def: f64| g(k).and_then(|v| v.as_f64()).unwrap_or(def);
    let i = |k: &str, def: i64| g(k).and_then(|v| v.as_i64()).unwrap_or(def);
    let b = |k: &str| g(k).map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false);
    Note {
        time: f("t", time.unwrap_or(0.0)),
        string: i("s", 0),
        fret: i("f", 0),
        sustain: f("sus", 0.0),
        slide_to: i("sl", -1),
        slide_unpitch_to: i("slu", -1),
        bend: f("bn", 0.0),
        hammer_on: b("ho"),
        pull_off: b("po"),
        harmonic: b("hm"),
        harmonic_pinch: b("hp"),
        palm_mute: b("pm"),
        mute: b("mt"),
        tremolo: b("tr"),
        accent: b("ac"),
        tap: b("tp"),
        link_next: false,
    }
}

pub fn chord_from_wire(d: &Value) -> Chord {
    let t = d.get("t").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let notes = d
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|cn| note_from_wire(cn, Some(t))).collect())
        .unwrap_or_default();
    Chord {
        time: t,
        chord_id: d.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        high_density: d.get("hd").and_then(|v| v.as_bool()).unwrap_or(false),
        notes,
    }
}

pub fn phrase_level_to_wire(pl: &PhraseLevel) -> Value {
    json!({
        "difficulty": pl.difficulty,
        "notes": pl.notes.iter().map(note_to_wire).collect::<Vec<_>>(),
        "chords": pl.chords.iter().map(chord_to_wire).collect::<Vec<_>>(),
        "anchors": pl.anchors.iter().map(|a| json!({"time": a.time, "fret": a.fret, "width": a.width})).collect::<Vec<_>>(),
        "handshapes": pl.hand_shapes.iter().map(|h| json!({"chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time})).collect::<Vec<_>>(),
    })
}

pub fn phrase_to_wire(p: &Phrase) -> Value {
    json!({
        "start_time": round_dp(p.start_time, 3),
        "end_time": round_dp(p.end_time, 3),
        "max_difficulty": p.max_difficulty,
        "levels": p.levels.iter().map(phrase_level_to_wire).collect::<Vec<_>>(),
    })
}

pub fn phrase_level_from_wire(d: &Value) -> PhraseLevel {
    PhraseLevel {
        difficulty: d.get("difficulty").and_then(|v| v.as_i64()).unwrap_or(0),
        notes: d.get("notes").and_then(|v| v.as_array()).map(|a| a.iter().map(note_from_wire_default).collect()).unwrap_or_default(),
        chords: d.get("chords").and_then(|v| v.as_array()).map(|a| a.iter().map(chord_from_wire).collect()).unwrap_or_default(),
        anchors: d.get("anchors").and_then(|v| v.as_array()).map(|a| a.iter().map(anchor_from_wire).collect()).unwrap_or_default(),
        hand_shapes: d.get("handshapes").and_then(|v| v.as_array()).map(|a| a.iter().map(handshape_from_wire).collect()).unwrap_or_default(),
    }
}

fn note_from_wire_default(v: &Value) -> Note {
    note_from_wire(v, None)
}

fn anchor_from_wire(a: &Value) -> Anchor {
    Anchor {
        time: a.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0),
        fret: a.get("fret").and_then(|v| v.as_i64()).unwrap_or(0),
        width: a.get("width").and_then(|v| v.as_i64()).unwrap_or(4),
    }
}

fn handshape_from_wire(h: &Value) -> HandShape {
    HandShape {
        chord_id: h.get("chord_id").and_then(|v| v.as_i64()).unwrap_or(0),
        start_time: h.get("start_time").and_then(|v| v.as_f64()).unwrap_or(0.0),
        end_time: h.get("end_time").and_then(|v| v.as_f64()).unwrap_or(0.0),
    }
}

pub fn phrase_from_wire(d: &Value) -> Phrase {
    Phrase {
        start_time: d.get("start_time").and_then(|v| v.as_f64()).unwrap_or(0.0),
        end_time: d.get("end_time").and_then(|v| v.as_f64()).unwrap_or(0.0),
        max_difficulty: d.get("max_difficulty").and_then(|v| v.as_i64()).unwrap_or(0),
        levels: d.get("levels").and_then(|v| v.as_array()).map(|a| a.iter().map(phrase_level_from_wire).collect()).unwrap_or_default(),
    }
}

/// Derive the active arrangement's string count. Mirrors
/// `arrangement_string_count` (song.py:256-328).
pub fn arrangement_string_count(arr: &Arrangement) -> i64 {
    let mut max_s: i64 = -1;
    for n in &arr.notes {
        if n.string > max_s {
            max_s = n.string;
        }
    }
    for ch in &arr.chords {
        for cn in &ch.notes {
            if cn.string > max_s {
                max_s = cn.string;
            }
        }
    }
    let notes_count = if max_s >= 0 { max_s + 1 } else { 0 };
    let name_based = if arr.name.to_lowercase().contains("bass") { 4 } else { 6 };
    let tuning_len = arr.tuning.len() as i64;
    let tuning_count = if tuning_len != 6 { tuning_len } else { 0 };
    notes_count.max(name_based).max(tuning_count)
}

pub fn arrangement_to_wire(arr: &Arrangement) -> Value {
    let mut out = Map::new();
    out.insert("name".into(), json!(arr.name));
    out.insert("tuning".into(), json!(arr.tuning));
    out.insert("capo".into(), json!(arr.capo));
    out.insert("notes".into(), json!(arr.notes.iter().map(note_to_wire).collect::<Vec<_>>()));
    out.insert("chords".into(), json!(arr.chords.iter().map(chord_to_wire).collect::<Vec<_>>()));
    out.insert(
        "anchors".into(),
        json!(arr.anchors.iter().map(|a| json!({"time": a.time, "fret": a.fret, "width": a.width})).collect::<Vec<_>>()),
    );
    out.insert(
        "handshapes".into(),
        json!(arr.hand_shapes.iter().map(|h| json!({"chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time})).collect::<Vec<_>>()),
    );
    out.insert(
        "templates".into(),
        json!(arr.chord_templates.iter().map(|ct| json!({"name": ct.name, "fingers": ct.fingers, "frets": ct.frets})).collect::<Vec<_>>()),
    );
    // phrases is additive — only include when non-empty (None or [] both → absent).
    if let Some(phrases) = &arr.phrases {
        if !phrases.is_empty() {
            out.insert("phrases".into(), json!(phrases.iter().map(phrase_to_wire).collect::<Vec<_>>()));
        }
    }
    Value::Object(out)
}

pub fn arrangement_from_wire(d: &Value) -> Arrangement {
    let tuning = d
        .get("tuning")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_else(|| vec![0; 6]);
    let notes = d.get("notes").and_then(|v| v.as_array()).map(|a| a.iter().map(note_from_wire_default).collect()).unwrap_or_default();
    let chords = d.get("chords").and_then(|v| v.as_array()).map(|a| a.iter().map(chord_from_wire).collect()).unwrap_or_default();
    let anchors = d.get("anchors").and_then(|v| v.as_array()).map(|a| a.iter().map(anchor_from_wire).collect()).unwrap_or_default();
    let hand_shapes = d.get("handshapes").and_then(|v| v.as_array()).map(|a| a.iter().map(handshape_from_wire).collect()).unwrap_or_default();
    let chord_templates = d
        .get("templates")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|ct| ChordTemplate {
                    name: ct.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    fingers: ct.get("fingers").and_then(|v| v.as_array()).map(|f| f.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_else(|| vec![-1; 6]),
                    frets: ct.get("frets").and_then(|v| v.as_array()).map(|f| f.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_else(|| vec![-1; 6]),
                })
                .collect()
        })
        .unwrap_or_default();
    // phrases optional — absent or empty → None (preserves "slider disabled").
    let phrases = match d.get("phrases") {
        Some(Value::Array(a)) if !a.is_empty() => Some(a.iter().map(phrase_from_wire).collect()),
        _ => None,
    };
    Arrangement {
        name: d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        tuning,
        capo: d.get("capo").and_then(|v| v.as_i64()).unwrap_or(0),
        notes,
        chords,
        anchors,
        hand_shapes,
        chord_templates,
        phrases,
    }
}

// ── XML attribute helpers ────────────────────────────────────────────────────

type N<'a> = roxmltree::Node<'a, 'a>;

fn child<'a>(el: N<'a>, tag: &str) -> Option<N<'a>> {
    el.children().find(|c| c.is_element() && c.tag_name().name() == tag)
}

fn children<'a>(el: N<'a>, tag: &str) -> Vec<N<'a>> {
    el.children().filter(|c| c.is_element() && c.tag_name().name() == tag).collect()
}

fn xml_float(el: N, attr: &str, default: f64) -> f64 {
    el.attribute(attr).and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn xml_int(el: N, attr: &str, default: i64) -> i64 {
    match el.attribute(attr) {
        Some(s) => s.parse::<i64>().unwrap_or_else(|_| s.parse::<f64>().map(|f| f as i64).unwrap_or(default)),
        None => default,
    }
}

fn xml_bool(el: N, attr: &str) -> bool {
    el.attribute(attr).map(|s| s != "0").unwrap_or(false)
}

fn xml_text(el: N, tag: &str) -> Option<String> {
    child(el, tag).and_then(|c| c.text()).map(|s| s.to_string())
}

fn parse_note(n: N) -> Note {
    Note {
        time: xml_float(n, "time", 0.0),
        string: xml_int(n, "string", 0),
        fret: xml_int(n, "fret", 0),
        sustain: xml_float(n, "sustain", 0.0),
        slide_to: xml_int(n, "slideTo", -1),
        slide_unpitch_to: xml_int(n, "slideUnpitchTo", -1),
        bend: xml_float(n, "bend", 0.0),
        hammer_on: xml_bool(n, "hammerOn"),
        pull_off: xml_bool(n, "pullOff"),
        harmonic: xml_bool(n, "harmonic"),
        harmonic_pinch: xml_bool(n, "harmonicPinch"),
        palm_mute: xml_bool(n, "palmMute"),
        mute: xml_bool(n, "mute"),
        tremolo: xml_bool(n, "tremolo"),
        accent: xml_bool(n, "accent"),
        link_next: xml_bool(n, "linkNext"),
        tap: xml_bool(n, "tap"),
    }
}

/// A pre-parsed `<level>`: time-sorted arrays + parallel time arrays for
/// bisect-sliced phrase merging. Mirrors `_parse_level_fully`.
struct ParsedLevel {
    notes: Vec<Note>,
    note_times: Vec<f64>,
    chords: Vec<Chord>,
    chord_times: Vec<f64>,
    anchors: Vec<Anchor>,
    anchor_times: Vec<f64>,
    hand_shapes: Vec<HandShape>,
    hs_times: Vec<f64>,
}

fn parse_level_fully(level: N, chord_templates: &[ChordTemplate]) -> ParsedLevel {
    let mut lv_notes = Vec::new();
    if let Some(container) = child(level, "notes") {
        for n in children(container, "note") {
            lv_notes.push(parse_note(n));
        }
    }
    lv_notes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));

    let mut lv_chords = Vec::new();
    if let Some(container) = child(level, "chords") {
        for c in children(container, "chord") {
            let t = xml_float(c, "time", 0.0);
            let mut chord_notes: Vec<Note> = children(c, "chordNote").iter().map(|cn| parse_note(*cn)).collect();
            let cid = xml_int(c, "chordId", 0);
            if chord_notes.is_empty() && (cid as usize) < chord_templates.len() {
                let ct = &chord_templates[cid as usize];
                for s in 0..6 {
                    if ct.frets[s] >= 0 {
                        chord_notes.push(Note { time: t, string: s as i64, fret: ct.frets[s], ..Default::default() });
                    }
                }
            }
            lv_chords.push(Chord {
                time: t,
                chord_id: cid,
                notes: chord_notes,
                high_density: xml_bool(c, "highDensity"),
            });
        }
    }
    lv_chords.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));

    let mut lv_anchors = Vec::new();
    if let Some(container) = child(level, "anchors") {
        for a in children(container, "anchor") {
            lv_anchors.push(Anchor {
                time: xml_float(a, "time", 0.0),
                fret: xml_int(a, "fret", 0),
                width: xml_int(a, "width", 4),
            });
        }
    }
    lv_anchors.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));

    let mut lv_hand_shapes = Vec::new();
    if let Some(container) = child(level, "handShapes") {
        for hs in children(container, "handShape") {
            lv_hand_shapes.push(HandShape {
                chord_id: xml_int(hs, "chordId", 0),
                start_time: xml_float(hs, "startTime", 0.0),
                end_time: xml_float(hs, "endTime", 0.0),
            });
        }
    }
    lv_hand_shapes.sort_by(|a, b| a.start_time.partial_cmp(&b.start_time).unwrap_or(std::cmp::Ordering::Equal));

    ParsedLevel {
        note_times: lv_notes.iter().map(|n| n.time).collect(),
        chord_times: lv_chords.iter().map(|c| c.time).collect(),
        anchor_times: lv_anchors.iter().map(|a| a.time).collect(),
        hs_times: lv_hand_shapes.iter().map(|h| h.start_time).collect(),
        notes: lv_notes,
        chords: lv_chords,
        anchors: lv_anchors,
        hand_shapes: lv_hand_shapes,
    }
}

/// `bisect.bisect_left(times, target)` — count of elements `< target`.
fn bisect_left(times: &[f64], target: f64) -> usize {
    times.partition_point(|&t| t < target)
}

/// Slice a pre-parsed level to `[t_start, t_end)`. Mirrors `_extract_level_slice`.
fn extract_level_slice<'a>(parsed: &'a ParsedLevel, t_start: f64, t_end: f64) -> (&'a [Note], &'a [Chord], &'a [Anchor], &'a [HandShape]) {
    let n = slice_range(&parsed.note_times, t_start, t_end);
    let c = slice_range(&parsed.chord_times, t_start, t_end);
    let a = slice_range(&parsed.anchor_times, t_start, t_end);
    let h = slice_range(&parsed.hs_times, t_start, t_end);
    let ns = n.0..n.1;
    let cs = c.0..c.1;
    let an = a.0..a.1;
    let hs = h.0..h.1;
    (&parsed.notes[ns], &parsed.chords[cs], &parsed.anchors[an], &parsed.hand_shapes[hs])
}

fn slice_range(times: &[f64], t_start: f64, t_end: f64) -> (usize, usize) {
    let i0 = bisect_left(times, t_start);
    let i1 = bisect_left(times, t_end);
    (i0, i1)
}

/// Parse a Rocksmith arrangement XML file. Mirrors `parse_arrangement`
/// (song.py:438-725), including the per-phrase difficulty merge and the
/// master-difficulty ladder.
pub fn parse_arrangement(xml_path: &Path) -> anyhow::Result<Arrangement> {
    let text = std::fs::read_to_string(xml_path)?;
    let doc = roxmltree::Document::parse(&text)?;
    let root = doc.root_element();

    // Name
    let mut arr_name = String::new();
    if let Some(el) = child(root, "arrangement") {
        if let Some(t) = el.text() {
            arr_name = t.to_string();
        }
    }

    // Tuning
    let mut tuning = vec![0i64; 6];
    if let Some(el) = child(root, "tuning") {
        for i in 0..6 {
            tuning[i] = xml_int(el, &format!("string{i}"), 0);
        }
    }

    // Capo
    let mut capo = 0i64;
    if let Some(el) = child(root, "capo") {
        if let Some(t) = el.text() {
            capo = t.trim().parse().ok().unwrap_or(0);
        }
    }

    // Chord templates
    let mut chord_templates = Vec::new();
    if let Some(container) = child(root, "chordTemplates") {
        for ct in children(container, "chordTemplate") {
            chord_templates.push(ChordTemplate {
                name: ct.attribute("chordName").unwrap_or("").to_string(),
                fingers: (0..6).map(|i| xml_int(ct, &format!("finger{i}"), -1)).collect(),
                frets: (0..6).map(|i| xml_int(ct, &format!("fret{i}"), -1)).collect(),
            });
        }
    }

    let levels_el = child(root, "levels");
    let phrases_el = child(root, "phrases");
    let phrase_iters_el = child(root, "phraseIterations");

    // all_levels: difficulty -> level element
    let mut all_levels: BTreeMap<i64, N> = BTreeMap::new();
    if let Some(le) = levels_el {
        for level in children(le, "level") {
            all_levels.insert(xml_int(level, "difficulty", 0), level);
        }
    }

    let mut parsed_levels: BTreeMap<i64, ParsedLevel> = BTreeMap::new();
    for (diff, el) in &all_levels {
        parsed_levels.insert(*diff, parse_level_fully(*el, &chord_templates));
    }

    let mut notes: Vec<Note> = Vec::new();
    let mut chords: Vec<Chord> = Vec::new();
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut hand_shapes: Vec<HandShape> = Vec::new();

    // Collect a pre-parsed level's time-clipped slice into the flat lists.
    let mut collect_from = |parsed: &ParsedLevel, t_start: f64, t_end: f64,
                            notes: &mut Vec<Note>, chords: &mut Vec<Chord>,
                            anchors: &mut Vec<Anchor>, hand_shapes: &mut Vec<HandShape>| {
        let (n, c, a, h) = extract_level_slice(parsed, t_start, t_end);
        notes.extend_from_slice(n);
        chords.extend_from_slice(c);
        anchors.extend_from_slice(a);
        hand_shapes.extend_from_slice(h);
    };

    // Pick the level with the most notes+chords and flatten it.
    let best_level = || -> Option<&ParsedLevel> {
        parsed_levels.values().max_by_key(|pl| pl.notes.len() + pl.chords.len())
    };

    let mut phrases: Option<Vec<Phrase>> = None;

    if parsed_levels.len() == 1 {
        // Single level — use it directly.
        let pl = parsed_levels.values().next().unwrap();
        collect_from(pl, 0.0, f64::INFINITY, &mut notes, &mut chords, &mut anchors, &mut hand_shapes);
    } else if phrases_el.is_some() && phrase_iters_el.is_some() && !parsed_levels.is_empty() {
        let phrase_list = children(phrases_el.unwrap(), "phrase");
        let iterations = children(phrase_iters_el.unwrap(), "phraseIteration");

        // Derive a finite end time for the last phrase iteration.
        let mut last_event = 0.0f64;
        for pl in parsed_levels.values() {
            for n in &pl.notes {
                last_event = last_event.max(n.time + n.sustain);
            }
            if let Some(&t) = pl.chord_times.last() {
                last_event = last_event.max(t);
            }
            if let Some(&t) = pl.anchor_times.last() {
                last_event = last_event.max(t);
            }
            for h in &pl.hand_shapes {
                last_event = last_event.max(h.end_time);
            }
        }
        for it in &iterations {
            last_event = last_event.max(xml_float(*it, "time", 0.0));
        }
        let song_end = last_event + 1.0;

        let mut phrases_list: Vec<Phrase> = Vec::new();
        for (i, it) in iterations.iter().enumerate() {
            let pid = xml_int(*it, "phraseId", 0);
            if (pid as usize) >= phrase_list.len() {
                continue;
            }
            let max_diff = xml_int(phrase_list[pid as usize], "maxDifficulty", 0);
            let t_start = xml_float(*it, "time", 0.0);
            let t_end = if i + 1 < iterations.len() {
                xml_float(iterations[i + 1], "time", 0.0)
            } else {
                song_end
            };

            let mut phrase_levels: Vec<PhraseLevel> = Vec::new();
            let mut slices_by_diff: BTreeMap<i64, (Vec<Note>, Vec<Chord>, Vec<Anchor>, Vec<HandShape>)> = BTreeMap::new();
            for (&diff, parsed) in parsed_levels.iter() {
                if diff > max_diff {
                    continue;
                }
                let (n, c, a, h) = extract_level_slice(parsed, t_start, t_end);
                slices_by_diff.insert(diff, (n.to_vec(), c.to_vec(), a.to_vec(), h.to_vec()));
                phrase_levels.push(PhraseLevel {
                    difficulty: diff,
                    notes: n.to_vec(),
                    chords: c.to_vec(),
                    anchors: a.to_vec(),
                    hand_shapes: h.to_vec(),
                });
            }

            if phrase_levels.is_empty() {
                continue;
            }

            phrases_list.push(Phrase {
                start_time: t_start,
                end_time: t_end,
                max_difficulty: max_diff,
                levels: phrase_levels,
            });

            // Flat max-mastery merge: reuse the slice for max_diff, or the
            // closest tier below it.
            let flat_diff = if slices_by_diff.contains_key(&max_diff) {
                max_diff
            } else {
                *slices_by_diff.keys().max().unwrap()
            };
            let s = &slices_by_diff[&flat_diff];
            notes.extend_from_slice(&s.0);
            chords.extend_from_slice(&s.1);
            anchors.extend_from_slice(&s.2);
            hand_shapes.extend_from_slice(&s.3);
        }

        if phrases_list.is_empty() {
            // phraseIterations yielded nothing usable — best-level fallback.
            if let Some(best) = best_level() {
                collect_from(best, 0.0, f64::INFINITY, &mut notes, &mut chords, &mut anchors, &mut hand_shapes);
            }
        } else {
            phrases = Some(phrases_list);
        }
    } else if !parsed_levels.is_empty() {
        if let Some(best) = best_level() {
            collect_from(best, 0.0, f64::INFINITY, &mut notes, &mut chords, &mut anchors, &mut hand_shapes);
        }
    }

    notes.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    chords.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    anchors.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    hand_shapes.sort_by(|a, b| a.start_time.partial_cmp(&b.start_time).unwrap_or(std::cmp::Ordering::Equal));

    Ok(Arrangement {
        name: arr_name,
        tuning,
        capo,
        notes,
        chords,
        anchors,
        hand_shapes,
        chord_templates,
        phrases,
    })
}

// ── SNG → XML (RsCli) + song loading ─────────────────────────────────────────

/// Resolve the RsCli binary path: `RSCLI_PATH` env, then bundled/system
/// candidates. Mirrors the candidate list in `_convert_sng_to_xml`.
pub fn resolve_rscli() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RSCLI_PATH") {
        if !p.is_empty() && Path::new(&p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(r) = std::env::var("RESOURCESPATH") {
        candidates.push(PathBuf::from(r).join("bin").join("rscli").join("RsCli"));
    }
    // root_dir/tools/rscli/RsCli — root_dir is the exe's parent.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("tools").join("rscli").join("RsCli"));
        }
    }
    if let Ok(pb) = std::env::var("PATH_BIN") {
        candidates.push(PathBuf::from(pb).join("rscli").join("RsCli"));
    }
    candidates.push(PathBuf::from("/opt/rscli/RsCli"));
    candidates.push(PathBuf::from("./rscli/RsCli"));
    for p in candidates {
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// If no arrangement XMLs exist but SNG files do, convert them via RsCli.
/// Mirrors `_convert_sng_to_xml` (song.py:728-812).
pub fn convert_sng_to_xml(extracted_dir: &Path) {
    let mut has_arrangement_xml = false;
    let mut has_vocals_xml = false;
    for xf in rglob(extracted_dir, "xml") {
        let Ok(text) = std::fs::read_to_string(&xf) else { continue };
        let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
        let root = doc.root_element();
        if root.tag_name().name() == "vocals" {
            has_vocals_xml = true;
            continue;
        }
        if root.tag_name().name() == "song" {
            if let Some(el) = child(root, "arrangement") {
                if let Some(t) = el.text() {
                    let low = t.trim().to_lowercase();
                    if low == "vocals" {
                        has_vocals_xml = true;
                    } else if !matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                        has_arrangement_xml = true;
                    } else {
                        has_arrangement_xml = true;
                    }
                } else {
                    has_arrangement_xml = true;
                }
            } else {
                // no <arrangement> — treat as arrangement XML (matches Python's else branch)
                has_arrangement_xml = true;
            }
        }
    }

    if has_arrangement_xml && has_vocals_xml {
        return;
    }

    let sng_files = rglob(extracted_dir, "sng");
    if sng_files.is_empty() {
        return;
    }

    let Some(rscli) = resolve_rscli() else {
        tracing::warn!("RsCli not found, cannot convert SNG to XML");
        return;
    };

    // Detect platform from directory structure.
    let mut platform = "pc";
    for sng in &sng_files {
        let parts = sng.to_string_lossy().to_lowercase();
        if parts.contains("/macos/") || parts.contains("/mac/") {
            platform = "mac";
            break;
        }
    }

    let arr_dir = extracted_dir.join("songs").join("arr");
    let _ = std::fs::create_dir_all(&arr_dir);

    for sng_path in &sng_files {
        let stem = sng_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if stem.to_lowercase().contains("vocals") {
            continue;
        }
        if has_arrangement_xml {
            continue;
        }
        let xml_out = arr_dir.join(format!("{stem}.xml"));
        let result = std::process::Command::new(&rscli)
            .arg("sng2xml")
            .arg(sng_path)
            .arg(&xml_out)
            .arg(platform)
            .output();
        match result {
            Ok(out) if !out.status.success() => {
                tracing::warn!("sng2xml failed for {stem}: {}", String::from_utf8_lossy(&out.stderr));
            }
            Err(e) => tracing::warn!("sng2xml error for {stem}: {e}"),
            _ => {}
        }
    }
}

/// Load a song from an extracted PSARC directory. Mirrors `load_song`
/// (song.py:815-947).
pub fn load_song(extracted_dir: &Path) -> Song {
    convert_sng_to_xml(extracted_dir);

    let mut song = Song::default();
    let xml_files = {
        let mut v = rglob(extracted_dir, "xml");
        v.sort();
        v
    };

    // Build manifest lookup: xml_stem (lowercase) -> ArrangementName.
    let mut manifest_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for jf in rglob(extracted_dir, "json") {
        let Ok(text) = std::fs::read_to_string(&jf) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(&text) else { continue };
        if let Some(entries) = data.get("Entries").and_then(|v| v.as_object()) {
            for (_k, v) in entries.iter() {
                let attrs = v.get("Attributes");
                let arr_name = attrs.and_then(|a| a.get("ArrangementName")).and_then(|v| v.as_str()).unwrap_or("");
                if !arr_name.is_empty() && !matches!(arr_name, "Vocals" | "ShowLights" | "JVocals") {
                    if let Some(stem) = jf.file_stem().and_then(|s| s.to_str()) {
                        manifest_names.insert(stem.to_lowercase(), arr_name.to_string());
                    }
                }
            }
        }
    }

    let mut metadata_loaded = false;
    for xml_path in &xml_files {
        let Ok(text) = std::fs::read_to_string(xml_path) else { continue };
        let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
        let root = doc.root_element();
        if root.tag_name().name() != "song" {
            continue;
        }

        // Skip vocals and showlights.
        if let Some(el) = child(root, "arrangement") {
            if let Some(t) = el.text() {
                let low = t.trim().to_lowercase();
                if matches!(low.as_str(), "vocals" | "showlights" | "jvocals") {
                    continue;
                }
            }
        }

        // Metadata from first valid arrangement.
        if !metadata_loaded {
            for (tag, set) in [("title", true), ("artistName", false), ("albumName", false)] {
                if let Some(t) = xml_text(root, tag) {
                    match set {
                        true => song.title = t,
                        false if tag == "artistName" => song.artist = t,
                        false => song.album = t,
                    }
                }
            }
            if let Some(t) = xml_text(root, "albumYear") {
                if let Ok(y) = t.trim().parse::<i64>() {
                    song.year = y;
                }
            }
            if let Some(t) = xml_text(root, "songLength") {
                if let Ok(v) = t.trim().parse::<f64>() {
                    song.song_length = v;
                }
            }
            if let Some(t) = xml_text(root, "offset") {
                if let Ok(v) = t.trim().parse::<f64>() {
                    song.offset = v;
                }
            }

            if let Some(container) = child(root, "ebeats") {
                for eb in children(container, "ebeat") {
                    song.beats.push(Beat {
                        time: xml_float(eb, "time", 0.0),
                        measure: xml_int(eb, "measure", -1),
                    });
                }
            }
            if let Some(container) = child(root, "sections") {
                for s in children(container, "section") {
                    song.sections.push(Section {
                        name: s.attribute("name").unwrap_or("").to_string(),
                        number: xml_int(s, "number", 0),
                        start_time: xml_float(s, "startTime", 0.0),
                    });
                }
            }
            metadata_loaded = true;
        }

        let mut arrangement = match parse_arrangement(xml_path) {
            Ok(a) => a,
            Err(_) => continue,
        };

        // Correct name from manifest, else fallback mapping / filename inference.
        let stem_lower = xml_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        if let Some(manifest_name) = manifest_names.get(&stem_lower) {
            arrangement.name = manifest_name.clone();
        } else {
            let low = arrangement.name.trim().to_lowercase();
            let mapped = match low.as_str() {
                "part real_guitar" => Some("Lead"),
                "part real_guitar_22" => Some("Rhythm"),
                "part real_bass" => Some("Bass"),
                "part real_guitar_bonus" => Some("Bonus Lead"),
                "part real_bass_22" => Some("Bass 2"),
                _ => None,
            };
            if let Some(m) = mapped {
                arrangement.name = m.to_string();
            } else if arrangement.name.is_empty() || low.starts_with("part ") {
                arrangement.name = if stem_lower.contains("lead") {
                    "Lead".to_string()
                } else if stem_lower.contains("rhythm") {
                    "Rhythm".to_string()
                } else if stem_lower.contains("bass") {
                    "Bass".to_string()
                } else if stem_lower.contains("combo") {
                    "Combo".to_string()
                } else {
                    xml_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
                };
            }
        }

        song.arrangements.push(arrangement);
    }

    // Sort: Lead > Combo > Rhythm > Bass > other.
    let priority = |name: &str| match name.to_lowercase().as_str() {
        "lead" => 0,
        "combo" => 1,
        "rhythm" => 2,
        "bass" => 3,
        _ => 99,
    };
    song.arrangements.sort_by_key(|a| priority(&a.name));

    // Fallback: read metadata from manifest JSON files (official DLC).
    if song.title.is_empty() || song.artist.is_empty() {
        load_manifest_metadata(&mut song, extracted_dir);
    }

    song
}

/// Read song metadata from manifest JSON files. Mirrors `_load_manifest_metadata`
/// (song.py:950-1002).
fn load_manifest_metadata(song: &mut Song, extracted_dir: &Path) {
    for jf in rglob(extracted_dir, "json") {
        let Ok(text) = std::fs::read_to_string(&jf) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(&text) else { continue };

        // Entries -> {key} -> Attributes
        let entries_obj = data.get("Entries").and_then(|v| v.as_object()).or_else(|| data.get("entries").and_then(|v| v.as_object()));
        if let Some(entries) = entries_obj {
            for (_key, val) in entries.iter() {
                let attrs = val.get("Attributes").or_else(|| val.get("attributes"));
                let Some(attrs) = attrs else { continue };
                fill_from_attrs(song, attrs);
                if !song.title.is_empty() && !song.artist.is_empty() {
                    return;
                }
            }
        }
        // Flat structure (individual arrangement manifests).
        let attrs = data.get("Attributes").or_else(|| data.get("attributes"));
        if let Some(attrs) = attrs {
            fill_from_attrs(song, attrs);
            if !song.title.is_empty() && !song.artist.is_empty() {
                return;
            }
        }
    }
}

fn fill_from_attrs(song: &mut Song, attrs: &Value) {
    let s = |k: &str| attrs.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    if song.title.is_empty() {
        song.title = s("SongName");
    }
    if song.artist.is_empty() {
        song.artist = s("ArtistName");
    }
    if song.album.is_empty() {
        song.album = s("AlbumName");
    }
    if song.year == 0 {
        if let Some(y) = attrs.get("SongYear").and_then(|v| v.as_i64()) {
            song.year = y;
        } else if let Some(y) = attrs.get("SongYear").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
            song.year = y;
        }
    }
    if song.song_length == 0.0 {
        if let Some(l) = attrs.get("SongLength").and_then(|v| v.as_f64()) {
            song.song_length = l;
        } else if let Some(l) = attrs.get("SongLength").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
            song.song_length = l;
        }
    }
}

/// Recursive glob — all files under `dir` with the given extension.
fn rglob(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if e.path().extension().and_then(|x| x.to_str()) == Some(ext) {
            v.push(e.path().to_path_buf());
        }
    }
    v
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    /// Round-trip a fixture arrangement through arrangement_from_wire →
    /// arrangement_to_wire and compare the compact-JSON output to a
    /// Python-generated reference (SLOPSMITH_FIXTURE_REF). Skips if either env
    /// var is absent (so `cargo test` runs clean without the user's fixtures).
    #[test]
    fn wire_matches_python() {
        let Ok(fix) = std::env::var("SLOPSMITH_FIXTURE_ARR") else { return };
        let Ok(refp) = std::env::var("SLOPSMITH_FIXTURE_REF") else { return };
        let original = std::fs::read_to_string(&fix).unwrap();
        let v: serde_json::Value = serde_json::from_str(original.trim()).unwrap();
        let arr = arrangement_from_wire(&v);
        let out = arrangement_to_wire(&arr);
        let serialized = serde_json::to_string(&out).unwrap();
        let reference = std::fs::read_to_string(&refp).unwrap();
        assert_eq!(serialized, reference.trim_end());
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    /// Cross-validate parse_arrangement (incl. the phrase/level merge and all
    /// fallback paths) against Python-generated references: for each
    /// /tmp/arr_case_N.xml, parse + arrangement_to_wire and compare to
    /// /tmp/arr_case_N.json. Skips cases whose fixtures are absent.
    #[test]
    fn parse_arrangement_matches_python_all_cases() {
        for i in 0..9 {
            let xml = format!("/tmp/arr_case_{i}.xml");
            let refp = format!("/tmp/arr_case_{i}.json");
            if !std::path::Path::new(&xml).exists() || !std::path::Path::new(&refp).exists() {
                continue;
            }
            let arr = parse_arrangement(std::path::Path::new(&xml)).unwrap();
            let out = arrangement_to_wire(&arr);
            let serialized = serde_json::to_string(&out).unwrap();
            let reference = std::fs::read_to_string(&refp).unwrap();
            assert_eq!(serialized, reference.trim_end(), "parse_arrangement case {i} diverged");
        }
    }
}
