//! Rocksmith 2014 arrangement XML parser and song data models.
//!
//! Direct port of `slopsmith/lib/song.py`. Contains the core data model
//! (`Note`, `Chord`, `Arrangement`, `Song`, ...), the wire-format
//! (JSON) serialization helpers shared with the sloppak loader, the
//! Rocksmith XML arrangement parser, and the extracted-PSARC loader.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─────────────────────────────── Data models ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub time: f64,
    pub string: i32,
    pub fret: i32,
    pub sustain: f64,
    pub slide_to: i32,
    pub slide_unpitch_to: i32,
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

impl Note {
    /// Construct a note with only position fields set (matches the Python
    /// `Note(time=..., string=..., fret=...)` default-field behaviour).
    pub fn with_pos(time: f64, string: i32, fret: i32) -> Note {
        Note {
            time,
            string,
            fret,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChordTemplate {
    pub name: String,
    pub fingers: Vec<i32>,
    pub frets: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chord {
    pub time: f64,
    pub chord_id: i32,
    pub notes: Vec<Note>,
    pub high_density: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub time: f64,
    pub fret: i32,
    pub width: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beat {
    pub time: f64,
    /// -1 for non-downbeat.
    pub measure: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub number: i32,
    pub start_time: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandShape {
    pub chord_id: i32,
    pub start_time: f64,
    pub end_time: f64,
}

/// One difficulty tier's worth of note/chord/anchor/hand-shape data for a
/// single phrase iteration. Rocksmith's XML stores these as
/// `<level difficulty="N">` blocks that repeat for every difficulty tier the
/// chart author wrote. Keeping them around lets the highway render a
/// "master difficulty" slider that picks a per-phrase difficulty tier at
/// render time (slopsmith#48).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhraseLevel {
    pub difficulty: i32,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
}

/// One phrase iteration with every difficulty tier the source chart provided,
/// scoped to the iteration's time range. `max_difficulty` is the phrase's
/// authored cap — `levels` may contain entries at or below that cap
/// (zero-indexed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phrase {
    pub start_time: f64,
    pub end_time: f64,
    pub max_difficulty: i32,
    pub levels: Vec<PhraseLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arrangement {
    pub name: String,
    pub tuning: Vec<i32>,
    pub capo: i32,
    pub notes: Vec<Note>,
    pub chords: Vec<Chord>,
    pub anchors: Vec<Anchor>,
    pub hand_shapes: Vec<HandShape>,
    pub chord_templates: Vec<ChordTemplate>,
    /// `None` for single-level sources (GP converter, old sloppaks) —
    /// frontends should treat a missing `phrases` as "no per-phrase
    /// difficulty data available, disable the slider". Populated from
    /// Rocksmith XML when multiple `<level>` tiers exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phrases: Option<Vec<Phrase>>,
}

impl Default for Arrangement {
    fn default() -> Self {
        Arrangement {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: i32,
    pub song_length: f64,
    pub offset: f64,
    pub beats: Vec<Beat>,
    pub sections: Vec<Section>,
    pub arrangements: Vec<Arrangement>,
    pub audio_path: String,
    /// Optional lyrics, one entry per syllable: {"t": float, "d": float, "w": str}.
    pub lyrics: Vec<Value>,
}

// ───────────────────────────── Small utilities ─────────────────────────────

/// Round `x` to `decimals` decimal places (mirrors Python `round(x, n)`
/// closely enough for wire serialization).
fn round_to(x: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (x * f).round() / f
}

// ── Wire format serialization (shared between highway_ws and sloppak loader) ──
//
// These helpers produce/consume the same JSON shape the highway WebSocket
// streams to the client. They are the authoritative definition of the
// `.sloppak` arrangement file format.

fn note_wire_map(n: &Note, include_time: bool) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if include_time {
        m.insert("t".into(), json!(round_to(n.time, 3)));
    }
    m.insert("s".into(), json!(n.string));
    m.insert("f".into(), json!(n.fret));
    m.insert("sus".into(), json!(round_to(n.sustain, 3)));
    m.insert("sl".into(), json!(n.slide_to));
    m.insert("slu".into(), json!(n.slide_unpitch_to));
    // Python: `round(n.bend, 1) if n.bend else 0` — an integer 0 when unset.
    if n.bend != 0.0 {
        m.insert("bn".into(), json!(round_to(n.bend, 1)));
    } else {
        m.insert("bn".into(), json!(0));
    }
    m.insert("ho".into(), json!(n.hammer_on));
    m.insert("po".into(), json!(n.pull_off));
    m.insert("hm".into(), json!(n.harmonic));
    m.insert("hp".into(), json!(n.harmonic_pinch));
    m.insert("pm".into(), json!(n.palm_mute));
    m.insert("mt".into(), json!(n.mute));
    m.insert("tr".into(), json!(n.tremolo));
    m.insert("ac".into(), json!(n.accent));
    m.insert("tp".into(), json!(n.tap));
    m
}

pub fn note_to_wire(n: &Note) -> Value {
    Value::Object(note_wire_map(n, true))
}

/// Chord notes omit their own time (the chord carries it).
pub fn chord_note_to_wire(cn: &Note) -> Value {
    Value::Object(note_wire_map(cn, false))
}

pub fn chord_to_wire(c: &Chord) -> Value {
    json!({
        "t": round_to(c.time, 3),
        "id": c.chord_id,
        "hd": c.high_density,
        "notes": c.notes.iter().map(chord_note_to_wire).collect::<Vec<_>>(),
    })
}

// ── Value extraction helpers (from-wire) ──

fn v_f64(d: &Value, k: &str, default: f64) -> f64 {
    d.get(k).and_then(|v| v.as_f64()).unwrap_or(default)
}

fn v_i32(d: &Value, k: &str, default: i32) -> i32 {
    d.get(k)
        .and_then(|v| {
            v.as_i64()
                .map(|n| n as i32)
                .or_else(|| v.as_f64().map(|f| f as i32))
        })
        .unwrap_or(default)
}

fn v_bool(d: &Value, k: &str, default: bool) -> bool {
    match d.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(default),
        Some(Value::String(s)) => !s.is_empty(),
        _ => default,
    }
}

fn v_i32_list(d: &Value, k: &str, default: Vec<i32>) -> Vec<i32> {
    match d.get(k).and_then(|v| v.as_array()) {
        Some(arr) => arr
            .iter()
            .map(|x| {
                x.as_i64()
                    .map(|n| n as i32)
                    .or_else(|| x.as_f64().map(|f| f as i32))
                    .unwrap_or(0)
            })
            .collect(),
        None => default,
    }
}

fn v_str(d: &Value, k: &str, default: &str) -> String {
    d.get(k)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn v_arr<'a>(d: &'a Value, k: &str) -> Vec<&'a Value> {
    d.get(k)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

pub fn note_from_wire(d: &Value, time: Option<f64>) -> Note {
    let t = match d.get("t") {
        Some(v) => v.as_f64().unwrap_or(0.0),
        None => time.unwrap_or(0.0),
    };
    Note {
        time: t,
        string: v_i32(d, "s", 0),
        fret: v_i32(d, "f", 0),
        sustain: v_f64(d, "sus", 0.0),
        slide_to: v_i32(d, "sl", -1),
        slide_unpitch_to: v_i32(d, "slu", -1),
        bend: v_f64(d, "bn", 0.0),
        hammer_on: v_bool(d, "ho", false),
        pull_off: v_bool(d, "po", false),
        harmonic: v_bool(d, "hm", false),
        harmonic_pinch: v_bool(d, "hp", false),
        palm_mute: v_bool(d, "pm", false),
        mute: v_bool(d, "mt", false),
        tremolo: v_bool(d, "tr", false),
        accent: v_bool(d, "ac", false),
        link_next: false,
        tap: v_bool(d, "tp", false),
    }
}

pub fn chord_from_wire(d: &Value) -> Chord {
    let t = v_f64(d, "t", 0.0);
    Chord {
        time: t,
        chord_id: v_i32(d, "id", 0),
        high_density: v_bool(d, "hd", false),
        notes: v_arr(d, "notes")
            .iter()
            .map(|cn| note_from_wire(cn, Some(t)))
            .collect(),
    }
}

pub fn phrase_level_to_wire(pl: &PhraseLevel) -> Value {
    json!({
        "difficulty": pl.difficulty,
        "notes": pl.notes.iter().map(note_to_wire).collect::<Vec<_>>(),
        "chords": pl.chords.iter().map(chord_to_wire).collect::<Vec<_>>(),
        "anchors": pl.anchors.iter().map(|a| json!({
            "time": a.time, "fret": a.fret, "width": a.width
        })).collect::<Vec<_>>(),
        "handshapes": pl.hand_shapes.iter().map(|h| json!({
            "chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time
        })).collect::<Vec<_>>(),
    })
}

pub fn phrase_to_wire(p: &Phrase) -> Value {
    json!({
        "start_time": round_to(p.start_time, 3),
        "end_time": round_to(p.end_time, 3),
        "max_difficulty": p.max_difficulty,
        "levels": p.levels.iter().map(phrase_level_to_wire).collect::<Vec<_>>(),
    })
}

pub fn phrase_level_from_wire(d: &Value) -> PhraseLevel {
    PhraseLevel {
        difficulty: v_i32(d, "difficulty", 0),
        notes: v_arr(d, "notes")
            .iter()
            .map(|n| note_from_wire(n, None))
            .collect(),
        chords: v_arr(d, "chords").iter().map(|c| chord_from_wire(c)).collect(),
        anchors: v_arr(d, "anchors")
            .iter()
            .map(|a| Anchor {
                time: v_f64(a, "time", 0.0),
                fret: v_i32(a, "fret", 0),
                width: v_i32(a, "width", 4),
            })
            .collect(),
        hand_shapes: v_arr(d, "handshapes")
            .iter()
            .map(|h| HandShape {
                chord_id: v_i32(h, "chord_id", 0),
                start_time: v_f64(h, "start_time", 0.0),
                end_time: v_f64(h, "end_time", 0.0),
            })
            .collect(),
    }
}

pub fn phrase_from_wire(d: &Value) -> Phrase {
    Phrase {
        start_time: v_f64(d, "start_time", 0.0),
        end_time: v_f64(d, "end_time", 0.0),
        max_difficulty: v_i32(d, "max_difficulty", 0),
        levels: v_arr(d, "levels")
            .iter()
            .map(|lv| phrase_level_from_wire(lv))
            .collect(),
    }
}

/// Derive the active arrangement's string count.
///
/// Combines three signals: the highest referenced string index +1
/// (notes-derived lower bound), a name-based fallback (arrangements named
/// "bass" default to 4, else 6), and the tuning-array length when it is not
/// the RS-XML padded value of 6. Returns the maximum of the three.
pub fn arrangement_string_count(arr: &Arrangement) -> i32 {
    let mut max_s: i32 = -1;
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
    let name_based = if arr.name.to_lowercase().contains("bass") {
        4
    } else {
        6
    };
    // Tuning-length signal — only trustworthy when NOT the RS-XML padded
    // value of 6. Length 4/5 indicates explicit bass / 5-string bass;
    // length 7/8 indicates an extended-range guitar from GP.
    let tuning_len = arr.tuning.len() as i32;
    let tuning_count = if tuning_len != 6 { tuning_len } else { 0 };
    notes_count.max(name_based).max(tuning_count)
}

/// Serialize an Arrangement into a JSON-ready value matching the wire format.
pub fn arrangement_to_wire(arr: &Arrangement) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("name".into(), json!(arr.name));
    out.insert("tuning".into(), json!(arr.tuning));
    out.insert("capo".into(), json!(arr.capo));
    out.insert(
        "notes".into(),
        json!(arr.notes.iter().map(note_to_wire).collect::<Vec<_>>()),
    );
    out.insert(
        "chords".into(),
        json!(arr.chords.iter().map(chord_to_wire).collect::<Vec<_>>()),
    );
    out.insert(
        "anchors".into(),
        json!(arr
            .anchors
            .iter()
            .map(|a| json!({"time": a.time, "fret": a.fret, "width": a.width}))
            .collect::<Vec<_>>()),
    );
    out.insert(
        "handshapes".into(),
        json!(arr
            .hand_shapes
            .iter()
            .map(|h| json!({
                "chord_id": h.chord_id, "start_time": h.start_time, "end_time": h.end_time
            }))
            .collect::<Vec<_>>()),
    );
    out.insert(
        "templates".into(),
        json!(arr
            .chord_templates
            .iter()
            .map(|ct| json!({
                "name": ct.name, "fingers": ct.fingers, "frets": ct.frets
            }))
            .collect::<Vec<_>>()),
    );
    // phrases is additive — only include the key when the source had
    // multi-level data. An empty list is treated the same as None.
    if let Some(phrases) = &arr.phrases {
        if !phrases.is_empty() {
            out.insert(
                "phrases".into(),
                json!(phrases.iter().map(phrase_to_wire).collect::<Vec<_>>()),
            );
        }
    }
    Value::Object(out)
}

/// Parse a wire-format arrangement value back into an Arrangement.
pub fn arrangement_from_wire(d: &Value) -> Arrangement {
    let phrases_present = d
        .get("phrases")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    Arrangement {
        name: v_str(d, "name", ""),
        tuning: v_i32_list(d, "tuning", vec![0; 6]),
        capo: v_i32(d, "capo", 0),
        notes: v_arr(d, "notes")
            .iter()
            .map(|n| note_from_wire(n, None))
            .collect(),
        chords: v_arr(d, "chords").iter().map(|c| chord_from_wire(c)).collect(),
        anchors: v_arr(d, "anchors")
            .iter()
            .map(|a| Anchor {
                time: v_f64(a, "time", 0.0),
                fret: v_i32(a, "fret", 0),
                width: v_i32(a, "width", 4),
            })
            .collect(),
        hand_shapes: v_arr(d, "handshapes")
            .iter()
            .map(|h| HandShape {
                chord_id: v_i32(h, "chord_id", 0),
                start_time: v_f64(h, "start_time", 0.0),
                end_time: v_f64(h, "end_time", 0.0),
            })
            .collect(),
        chord_templates: v_arr(d, "templates")
            .iter()
            .map(|ct| ChordTemplate {
                name: v_str(ct, "name", ""),
                fingers: v_i32_list(ct, "fingers", vec![-1; 6]),
                frets: v_i32_list(ct, "frets", vec![-1; 6]),
            })
            .collect(),
        phrases: if phrases_present {
            Some(
                v_arr(d, "phrases")
                    .iter()
                    .map(|p| phrase_from_wire(p))
                    .collect(),
            )
        } else {
            None
        },
    }
}

// ──────────────────────────── Minimal XML DOM ─────────────────────────────
//
// quick_xml is event-based; the Python code relied on ElementTree's tree
// navigation (find/findall/get/text), so we build a lightweight DOM once and
// navigate it the same way.

#[derive(Debug, Clone)]
struct XmlElement {
    tag: String,
    attrs: HashMap<String, String>,
    text: String,
    children: Vec<XmlElement>,
}

impl XmlElement {
    fn find(&self, tag: &str) -> Option<&XmlElement> {
        self.children.iter().find(|c| c.tag == tag)
    }

    fn findall(&self, tag: &str) -> Vec<&XmlElement> {
        self.children.iter().filter(|c| c.tag == tag).collect()
    }

    fn get(&self, attr: &str) -> Option<&str> {
        self.attrs.get(attr).map(|s| s.as_str())
    }

    /// The element's text if it is non-empty (mirrors Python's `el.text`
    /// truthiness for the leaf-value elements we read).
    fn text_opt(&self) -> Option<&str> {
        if self.text.is_empty() {
            None
        } else {
            Some(self.text.as_str())
        }
    }
}

fn element_from_start(e: &quick_xml::events::BytesStart) -> Result<XmlElement, String> {
    let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = HashMap::new();
    for a in e.attributes() {
        let a = a.map_err(|err| err.to_string())?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let val = a
            .unescape_value()
            .map(|c| c.into_owned())
            .unwrap_or_default();
        attrs.insert(key, val);
    }
    Ok(XmlElement {
        tag,
        attrs,
        text: String::new(),
        children: Vec::new(),
    })
}

fn parse_xml_str(content: &str) -> Result<XmlElement, String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(content);
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut root: Option<XmlElement> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let el = element_from_start(&e)?;
                stack.push(el);
            }
            Ok(Event::Empty(e)) => {
                let el = element_from_start(&e)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => root = Some(el),
                }
            }
            Ok(Event::End(_)) => {
                if let Some(el) = stack.pop() {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => root = Some(el),
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let txt = t.unescape().map(|c| c.into_owned()).unwrap_or_default();
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&txt);
                }
            }
            Ok(Event::CData(t)) => {
                let txt = String::from_utf8_lossy(t.as_ref()).into_owned();
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&txt);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("xml error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    root.ok_or_else(|| "no root element".to_string())
}

fn parse_xml_file(path: &Path) -> Result<XmlElement, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_xml_str(&content)
}

// ── XML attribute helpers (port of `_float`, `_int`, `_bool`) ──

fn xml_float(el: &XmlElement, attr: &str, default: f64) -> f64 {
    match el.get(attr) {
        Some(v) => v.parse::<f64>().unwrap_or(default),
        None => default,
    }
}

fn xml_int(el: &XmlElement, attr: &str, default: i32) -> i32 {
    match el.get(attr) {
        None => default,
        Some(v) => v
            .parse::<i32>()
            .unwrap_or_else(|_| v.parse::<f64>().map(|f| f as i32).unwrap_or(default)),
    }
}

fn xml_bool(el: &XmlElement, attr: &str) -> bool {
    match el.get(attr) {
        Some(v) => v != "0",
        None => false,
    }
}

fn parse_note(n: &XmlElement) -> Note {
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

// ─────────────────────────── Arrangement parsing ───────────────────────────

/// A single `<level>` pre-parsed into time-sorted arrays plus parallel time
/// arrays for bisection.
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

fn cmp_f64(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// Left-bisect: index of the first element in `times` that is `>= target`.
fn bisect_left(times: &[f64], target: f64) -> usize {
    let mut lo = 0usize;
    let mut hi = times.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if times[mid] < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn parse_level_fully(level: &XmlElement, chord_templates: &[ChordTemplate]) -> ParsedLevel {
    let mut lv_notes: Vec<Note> = Vec::new();
    if let Some(container) = level.find("notes") {
        for n in container.findall("note") {
            lv_notes.push(parse_note(n));
        }
    }
    lv_notes.sort_by(|a, b| cmp_f64(a.time, b.time));

    let mut lv_chords: Vec<Chord> = Vec::new();
    if let Some(container) = level.find("chords") {
        for c in container.findall("chord") {
            let t = xml_float(c, "time", 0.0);
            let mut chord_notes: Vec<Note> =
                c.findall("chordNote").iter().map(|cn| parse_note(cn)).collect();
            let cid = xml_int(c, "chordId", 0);
            if chord_notes.is_empty() && cid >= 0 && (cid as usize) < chord_templates.len() {
                let ct = &chord_templates[cid as usize];
                for s in 0..6usize {
                    if s < ct.frets.len() && ct.frets[s] >= 0 {
                        chord_notes.push(Note::with_pos(t, s as i32, ct.frets[s]));
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
    lv_chords.sort_by(|a, b| cmp_f64(a.time, b.time));

    let mut lv_anchors: Vec<Anchor> = Vec::new();
    if let Some(container) = level.find("anchors") {
        for a in container.findall("anchor") {
            lv_anchors.push(Anchor {
                time: xml_float(a, "time", 0.0),
                fret: xml_int(a, "fret", 0),
                width: xml_int(a, "width", 4),
            });
        }
    }
    lv_anchors.sort_by(|a, b| cmp_f64(a.time, b.time));

    let mut lv_hand_shapes: Vec<HandShape> = Vec::new();
    if let Some(container) = level.find("handShapes") {
        for hs in container.findall("handShape") {
            lv_hand_shapes.push(HandShape {
                chord_id: xml_int(hs, "chordId", 0),
                start_time: xml_float(hs, "startTime", 0.0),
                end_time: xml_float(hs, "endTime", 0.0),
            });
        }
    }
    lv_hand_shapes.sort_by(|a, b| cmp_f64(a.start_time, b.start_time));

    let note_times = lv_notes.iter().map(|n| n.time).collect();
    let chord_times = lv_chords.iter().map(|c| c.time).collect();
    let anchor_times = lv_anchors.iter().map(|a| a.time).collect();
    let hs_times = lv_hand_shapes.iter().map(|h| h.start_time).collect();

    ParsedLevel {
        notes: lv_notes,
        note_times,
        chords: lv_chords,
        chord_times,
        anchors: lv_anchors,
        anchor_times,
        hand_shapes: lv_hand_shapes,
        hs_times,
    }
}

type LevelSlice = (Vec<Note>, Vec<Chord>, Vec<Anchor>, Vec<HandShape>);

/// Return (notes, chords, anchors, hand_shapes) for one pre-parsed level,
/// clipped to [t_start, t_end).
fn extract_level_slice(parsed: &ParsedLevel, t_start: f64, t_end: f64) -> LevelSlice {
    let slice_notes = {
        let i0 = bisect_left(&parsed.note_times, t_start);
        let i1 = bisect_left(&parsed.note_times, t_end).max(i0);
        parsed.notes[i0..i1].to_vec()
    };
    let slice_chords = {
        let i0 = bisect_left(&parsed.chord_times, t_start);
        let i1 = bisect_left(&parsed.chord_times, t_end).max(i0);
        parsed.chords[i0..i1].to_vec()
    };
    let slice_anchors = {
        let i0 = bisect_left(&parsed.anchor_times, t_start);
        let i1 = bisect_left(&parsed.anchor_times, t_end).max(i0);
        parsed.anchors[i0..i1].to_vec()
    };
    let slice_hs = {
        let i0 = bisect_left(&parsed.hs_times, t_start);
        let i1 = bisect_left(&parsed.hs_times, t_end).max(i0);
        parsed.hand_shapes[i0..i1].to_vec()
    };
    (slice_notes, slice_chords, slice_anchors, slice_hs)
}

/// Fallback merge when no usable phrase metadata is available: pick the level
/// with the most notes+chords and flatten it. Preserves first-max ordering
/// (document order) on ties, matching Python's `max`.
fn collect_best_level_fallback(
    parsed_levels: &[(i32, ParsedLevel)],
    notes: &mut Vec<Note>,
    chords: &mut Vec<Chord>,
    anchors: &mut Vec<Anchor>,
    hand_shapes: &mut Vec<HandShape>,
) {
    let mut best: Option<&ParsedLevel> = None;
    let mut best_score: i64 = -1;
    for (_, pl) in parsed_levels {
        let score = (pl.notes.len() + pl.chords.len()) as i64;
        if score > best_score {
            best_score = score;
            best = Some(pl);
        }
    }
    if let Some(pl) = best {
        let (n, c, a, h) = extract_level_slice(pl, 0.0, f64::INFINITY);
        notes.extend(n);
        chords.extend(c);
        anchors.extend(a);
        hand_shapes.extend(h);
    }
}

/// Parse a Rocksmith arrangement XML file.
pub fn parse_arrangement(xml_path: &str) -> Arrangement {
    let root = match parse_xml_file(Path::new(xml_path)) {
        Ok(r) => r,
        Err(_) => return Arrangement::default(),
    };

    // Name
    let mut arr_name = String::new();
    if let Some(el) = root.find("arrangement") {
        if let Some(t) = el.text_opt() {
            arr_name = t.to_string();
        }
    }

    // Tuning
    let mut tuning = vec![0; 6];
    if let Some(el) = root.find("tuning") {
        for (i, slot) in tuning.iter_mut().enumerate() {
            *slot = xml_int(el, &format!("string{}", i), 0);
        }
    }

    // Capo
    let mut capo = 0;
    if let Some(el) = root.find("capo") {
        if let Some(t) = el.text_opt() {
            if let Ok(v) = t.parse::<i32>() {
                capo = v;
            }
        }
    }

    // Chord templates
    let mut chord_templates: Vec<ChordTemplate> = Vec::new();
    if let Some(container) = root.find("chordTemplates") {
        for ct in container.findall("chordTemplate") {
            chord_templates.push(ChordTemplate {
                name: ct.get("chordName").unwrap_or("").to_string(),
                fingers: (0..6).map(|i| xml_int(ct, &format!("finger{}", i), -1)).collect(),
                frets: (0..6).map(|i| xml_int(ct, &format!("fret{}", i), -1)).collect(),
            });
        }
    }

    // Pre-parse each `<level>` once. Preserve document order for tie-breaking
    // while deduplicating on difficulty (last write wins, first position).
    let mut parsed_levels: Vec<(i32, ParsedLevel)> = Vec::new();
    if let Some(levels_el) = root.find("levels") {
        for level in levels_el.findall("level") {
            let diff = xml_int(level, "difficulty", 0);
            let pl = parse_level_fully(level, &chord_templates);
            if let Some(entry) = parsed_levels.iter_mut().find(|(d, _)| *d == diff) {
                entry.1 = pl;
            } else {
                parsed_levels.push((diff, pl));
            }
        }
    }

    let phrases_el = root.find("phrases");
    let phrase_iters_el = root.find("phraseIterations");

    let mut notes: Vec<Note> = Vec::new();
    let mut chords: Vec<Chord> = Vec::new();
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut hand_shapes: Vec<HandShape> = Vec::new();

    // Per-phrase difficulty data for the master-difficulty slider
    // (slopsmith#48). Only populated when the XML has multiple levels AND
    // phrase data — left as None for single-level sources.
    let mut phrases: Option<Vec<Phrase>> = None;

    if parsed_levels.len() == 1 {
        // Single level — use it directly.
        let (n, c, a, h) = extract_level_slice(&parsed_levels[0].1, 0.0, f64::INFINITY);
        notes.extend(n);
        chords.extend(c);
        anchors.extend(a);
        hand_shapes.extend(h);
    } else if phrases_el.is_some() && phrase_iters_el.is_some() && !parsed_levels.is_empty() {
        let phrases_el = phrases_el.unwrap();
        let phrase_iters_el = phrase_iters_el.unwrap();
        let phrase_list = phrases_el.findall("phrase");
        let iterations = phrase_iters_el.findall("phraseIteration");

        // Derive a finite end time for the last phrase iteration from the last
        // real event across all parsed levels (JSON has no Infinity literal).
        let mut last_event = 0.0f64;
        for (_, pl) in &parsed_levels {
            for n in &pl.notes {
                last_event = last_event.max(n.time + n.sustain);
            }
            if let Some(&last) = pl.chord_times.last() {
                last_event = last_event.max(last);
            }
            if let Some(&last) = pl.anchor_times.last() {
                last_event = last_event.max(last);
            }
            for h in &pl.hand_shapes {
                last_event = last_event.max(h.end_time);
            }
        }
        for it in &iterations {
            last_event = last_event.max(xml_float(it, "time", 0.0));
        }
        let song_end = last_event + 1.0;

        // Difficulties sorted ascending — the order the slider ladder uses.
        let mut sorted_diffs: Vec<i32> = parsed_levels.iter().map(|(d, _)| *d).collect();
        sorted_diffs.sort();

        let mut ph: Vec<Phrase> = Vec::new();
        for (i, it) in iterations.iter().enumerate() {
            let pid = xml_int(it, "phraseId", 0);
            if pid < 0 || (pid as usize) >= phrase_list.len() {
                continue;
            }
            let max_diff = xml_int(phrase_list[pid as usize], "maxDifficulty", 0);
            let t_start = xml_float(it, "time", 0.0);
            let t_end = if i + 1 < iterations.len() {
                xml_float(iterations[i + 1], "time", 0.0)
            } else {
                song_end
            };

            // Build a PhraseLevel for every difficulty tier at or below the
            // phrase's max. Capture slices for reuse in the flat merge below.
            let mut phrase_levels: Vec<PhraseLevel> = Vec::new();
            let mut slices_by_diff: Vec<(i32, LevelSlice)> = Vec::new();
            for &diff in &sorted_diffs {
                if diff > max_diff {
                    continue;
                }
                let pl = parsed_levels
                    .iter()
                    .find(|(d, _)| *d == diff)
                    .map(|(_, p)| p)
                    .unwrap();
                let slc = extract_level_slice(pl, t_start, t_end);
                phrase_levels.push(PhraseLevel {
                    difficulty: diff,
                    notes: slc.0.clone(),
                    chords: slc.1.clone(),
                    anchors: slc.2.clone(),
                    hand_shapes: slc.3.clone(),
                });
                slices_by_diff.push((diff, slc));
            }

            // No ladder for this phrase — skip so the fallback can trigger.
            if phrase_levels.is_empty() {
                continue;
            }

            ph.push(Phrase {
                start_time: t_start,
                end_time: t_end,
                max_difficulty: max_diff,
                levels: phrase_levels,
            });

            // Populate the flat max-mastery merge for existing consumers.
            let has_max = slices_by_diff.iter().any(|(d, _)| *d == max_diff);
            let flat_diff = if has_max {
                max_diff
            } else {
                slices_by_diff.iter().map(|(d, _)| *d).max().unwrap()
            };
            let slc = &slices_by_diff
                .iter()
                .find(|(d, _)| *d == flat_diff)
                .unwrap()
                .1;
            notes.extend(slc.0.iter().cloned());
            chords.extend(slc.1.iter().cloned());
            anchors.extend(slc.2.iter().cloned());
            hand_shapes.extend(slc.3.iter().cloned());
        }

        // If no usable iterations were produced, revert to the "no phrase
        // data" sentinel and run the best-level fallback inline.
        if ph.is_empty() {
            phrases = None;
            collect_best_level_fallback(
                &parsed_levels,
                &mut notes,
                &mut chords,
                &mut anchors,
                &mut hand_shapes,
            );
        } else {
            phrases = Some(ph);
        }
    } else if !parsed_levels.is_empty() {
        collect_best_level_fallback(
            &parsed_levels,
            &mut notes,
            &mut chords,
            &mut anchors,
            &mut hand_shapes,
        );
    }

    notes.sort_by(|a, b| cmp_f64(a.time, b.time));
    chords.sort_by(|a, b| cmp_f64(a.time, b.time));
    anchors.sort_by(|a, b| cmp_f64(a.time, b.time));
    hand_shapes.sort_by(|a, b| cmp_f64(a.start_time, b.start_time));

    Arrangement {
        name: arr_name,
        tuning,
        capo,
        notes,
        chords,
        anchors,
        hand_shapes,
        chord_templates,
        phrases,
    }
}

// ────────────────────────── PSARC directory loading ────────────────────────

/// Recursively collect files under `dir` whose extension matches `ext`
/// (case-insensitive), equivalent to `Path.rglob("*.{ext}")`.
fn rglob(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.extend(rglob(&p, ext));
            } else if p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out
}

fn stem_lower(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// If no arrangement XMLs exist but SNG files do, convert them via RsCli.
/// Also converts vocals SNG → XML when no vocals XML is present, so lyrics
/// are available for official DLC (which ships SNG-only).
///
/// Note: unlike the Python original, the external RsCli process is not run
/// with a timeout (Rust's std has no built-in process timeout).
pub fn convert_sng_to_xml(extracted_dir: &str) {
    let d = Path::new(extracted_dir);

    // Check if we already have arrangement XMLs (not just showlights/vocals).
    let xml_files = rglob(d, "xml");
    let mut has_arrangement_xml = false;
    let mut has_vocals_xml = false;
    for xf in &xml_files {
        let root = match parse_xml_file(xf) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if root.tag == "vocals" {
            has_vocals_xml = true;
            continue;
        }
        if root.tag == "song" {
            if let Some(el) = root.find("arrangement") {
                if let Some(t) = el.text_opt() {
                    let low = t.to_lowercase();
                    let low = low.trim();
                    if !matches!(low, "vocals" | "showlights" | "jvocals") {
                        has_arrangement_xml = true;
                    } else if low == "vocals" {
                        has_vocals_xml = true;
                    }
                    continue;
                }
            }
            has_arrangement_xml = true;
        }
    }

    if has_arrangement_xml && has_vocals_xml {
        return; // Already have everything.
    }

    // Find SNG files.
    let sng_files = rglob(d, "sng");
    if sng_files.is_empty() {
        return;
    }

    // Resolve RsCli path.
    let mut rscli = std::env::var("RSCLI_PATH").unwrap_or_default();
    if rscli.is_empty() || !Path::new(&rscli).exists() {
        let mut candidates: Vec<PathBuf> = Vec::new();
        // Path relative to the executable (bundled tools/rscli/RsCli).
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
                candidates.push(parent.join("tools").join("rscli").join("RsCli"));
            }
        }
        if let Ok(path_bin) = std::env::var("PATH_BIN") {
            candidates.push(Path::new(&path_bin).join("rscli").join("RsCli"));
        }
        candidates.push(PathBuf::from("/opt/rscli/RsCli"));
        candidates.push(PathBuf::from("./rscli/RsCli"));
        // Electron app's resources/bin/rscli takes priority when present.
        if let Ok(resources) = std::env::var("RESOURCESPATH") {
            candidates.insert(0, Path::new(&resources).join("bin").join("rscli").join("RsCli"));
        }
        for p in candidates {
            if p.exists() {
                rscli = p.to_string_lossy().into_owned();
                break;
            }
        }
    }
    if rscli.is_empty() {
        println!("RsCli not found, cannot convert SNG to XML");
        return;
    }

    // Detect platform from directory structure.
    let mut platform = "pc";
    for sng in &sng_files {
        let parts = sng.to_string_lossy().to_lowercase();
        if parts.contains("/macos/") || parts.contains("/mac/") {
            platform = "mac";
            break;
        }
    }

    let arr_dir = d.join("songs").join("arr");
    if std::fs::create_dir_all(&arr_dir).is_err() {
        return;
    }

    for sng_path in &sng_files {
        let stem = sng_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Vocals SNGs are not decoded via RsCli (unsupported).
        if stem.to_lowercase().contains("vocals") {
            continue;
        }
        if has_arrangement_xml {
            continue;
        }
        let xml_out = arr_dir.join(format!("{}.xml", stem));
        match std::process::Command::new(&rscli)
            .arg("sng2xml")
            .arg(sng_path)
            .arg(&xml_out)
            .arg(platform)
            .output()
        {
            Ok(result) => {
                if !result.status.success() {
                    println!(
                        "sng2xml failed for {}: {}",
                        stem,
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
            }
            Err(e) => {
                println!("sng2xml error for {}: {}", stem, e);
            }
        }
    }
}

/// Load a song from an extracted PSARC directory.
pub fn load_song(extracted_dir: &str) -> Song {
    // Convert SNG files to XML if needed (official DLC).
    convert_sng_to_xml(extracted_dir);

    let mut song = Song::default();
    let mut xml_files = rglob(Path::new(extracted_dir), "xml");
    xml_files.sort();

    // Build manifest lookup: xml_stem (lowercase) -> ArrangementName.
    let mut manifest_names: HashMap<String, String> = HashMap::new();
    for jf in rglob(Path::new(extracted_dir), "json") {
        let text = match std::fs::read_to_string(&jf) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(entries) = data.get("Entries").and_then(|e| e.as_object()) {
            for (_, v) in entries {
                let arr_name = v
                    .get("Attributes")
                    .and_then(|a| a.get("ArrangementName"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                if !arr_name.is_empty()
                    && !matches!(arr_name, "Vocals" | "ShowLights" | "JVocals")
                {
                    manifest_names.insert(stem_lower(&jf), arr_name.to_string());
                }
            }
        }
    }

    let mut metadata_loaded = false;
    for xml_path in &xml_files {
        let root = match parse_xml_file(xml_path) {
            Ok(r) => r,
            Err(_) => continue,
        };

        if root.tag != "song" {
            continue;
        }

        // Skip vocals and showlights.
        if let Some(el) = root.find("arrangement") {
            if let Some(t) = el.text_opt() {
                let low = t.to_lowercase();
                let low = low.trim();
                if matches!(low, "vocals" | "showlights" | "jvocals") {
                    continue;
                }
            }
        }

        // Metadata from first valid arrangement.
        if !metadata_loaded {
            if let Some(el) = root.find("title") {
                if let Some(t) = el.text_opt() {
                    song.title = t.to_string();
                }
            }
            if let Some(el) = root.find("artistName") {
                if let Some(t) = el.text_opt() {
                    song.artist = t.to_string();
                }
            }
            if let Some(el) = root.find("albumName") {
                if let Some(t) = el.text_opt() {
                    song.album = t.to_string();
                }
            }

            if let Some(el) = root.find("albumYear") {
                if let Some(t) = el.text_opt() {
                    if let Ok(v) = t.parse::<i32>() {
                        song.year = v;
                    }
                }
            }

            if let Some(el) = root.find("songLength") {
                if let Some(t) = el.text_opt() {
                    if let Ok(v) = t.parse::<f64>() {
                        song.song_length = v;
                    }
                }
            }

            if let Some(el) = root.find("offset") {
                if let Some(t) = el.text_opt() {
                    if let Ok(v) = t.parse::<f64>() {
                        song.offset = v;
                    }
                }
            }

            // Beats
            if let Some(container) = root.find("ebeats") {
                for eb in container.findall("ebeat") {
                    song.beats.push(Beat {
                        time: xml_float(eb, "time", 0.0),
                        measure: xml_int(eb, "measure", -1),
                    });
                }
            }

            // Sections
            if let Some(container) = root.find("sections") {
                for s in container.findall("section") {
                    song.sections.push(Section {
                        name: s.get("name").unwrap_or("").to_string(),
                        number: xml_int(s, "number", 0),
                        start_time: xml_float(s, "startTime", 0.0),
                    });
                }
            }

            metadata_loaded = true;
        }

        // Parse arrangement.
        let mut arrangement = parse_arrangement(&xml_path.to_string_lossy());

        // Try to get the correct name from the manifest JSON.
        if let Some(manifest_name) = manifest_names.get(&stem_lower(xml_path)) {
            arrangement.name = manifest_name.clone();
        } else {
            // Fallback: map internal XML names to display names.
            let low = arrangement.name.to_lowercase();
            let low = low.trim().to_string();
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
                // Infer from filename.
                let fname = stem_lower(xml_path);
                arrangement.name = if fname.contains("lead") {
                    "Lead".to_string()
                } else if fname.contains("rhythm") {
                    "Rhythm".to_string()
                } else if fname.contains("bass") {
                    "Bass".to_string()
                } else if fname.contains("combo") {
                    "Combo".to_string()
                } else {
                    xml_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string()
                };
            }
        }

        song.arrangements.push(arrangement);
    }

    // Sort: Lead > Combo > Rhythm > Bass > other (stable).
    song.arrangements.sort_by_key(|a| match a.name.to_lowercase().as_str() {
        "lead" => 0,
        "combo" => 1,
        "rhythm" => 2,
        "bass" => 3,
        _ => 99,
    });

    // Fallback: read metadata from manifest JSON files (official DLC).
    if song.title.is_empty() || song.artist.is_empty() {
        load_manifest_metadata(&mut song, extracted_dir);
    }

    song
}

/// Read song metadata from manifest JSON files (used for official DLC).
fn load_manifest_metadata(song: &mut Song, extracted_dir: &str) {
    let d = Path::new(extracted_dir);
    for jf in rglob(d, "json") {
        let text = match std::fs::read_to_string(&jf) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Manifest JSON has: Entries -> {key} -> Attributes.
        let entries = data
            .get("Entries")
            .or_else(|| data.get("entries"))
            .and_then(|e| e.as_object());
        if let Some(entries) = entries {
            if !entries.is_empty() {
                for (_, val) in entries {
                    let attrs = val
                        .get("Attributes")
                        .or_else(|| val.get("attributes"));
                    if let Some(attrs) = attrs {
                        if apply_manifest_attrs(song, attrs) {
                            return;
                        }
                    }
                }
            }
        }

        // Also check flat structure (individual arrangement manifests).
        let attrs = data.get("Attributes").or_else(|| data.get("attributes"));
        if let Some(attrs) = attrs {
            if attrs.is_object() {
                if apply_manifest_attrs(song, attrs) {
                    return;
                }
            }
        }
    }
}

/// Apply a manifest `Attributes` object to `song`, only filling empty fields.
/// Returns `true` when both title and artist are now populated (mirrors the
/// Python early-return).
fn apply_manifest_attrs(song: &mut Song, attrs: &Value) -> bool {
    if song.title.is_empty() {
        if let Some(v) = attrs.get("SongName").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                song.title = v.to_string();
            }
        }
    }
    if song.artist.is_empty() {
        if let Some(v) = attrs.get("ArtistName").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                song.artist = v.to_string();
            }
        }
    }
    if song.album.is_empty() {
        if let Some(v) = attrs.get("AlbumName").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                song.album = v.to_string();
            }
        }
    }
    if song.year == 0 {
        if let Some(v) = attrs.get("SongYear") {
            if let Some(n) = v.as_i64() {
                song.year = n as i32;
            } else if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<i32>() {
                    song.year = n;
                }
            }
        }
    }
    if song.song_length == 0.0 {
        if let Some(v) = attrs.get("SongLength") {
            if let Some(n) = v.as_f64() {
                song.song_length = n;
            } else if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<f64>() {
                    song.song_length = n;
                }
            }
        }
    }
    !song.title.is_empty() && !song.artist.is_empty()
}
