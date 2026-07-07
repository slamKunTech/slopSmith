//! MIDI file import — list tracks and convert a track to a Keys arrangement.
//!
//! Rust port of `lib/midi_import.py`. Mirrors the shape of `gp2rs` so the
//! editor's track-picker UI can use the same flow for both GP and MIDI files.
//! Drum tracks (GM channel 9) are filtered out of the listing entirely.
//!
//! Uses the [`midly`] crate for MIDI parsing.

use std::collections::{HashMap, VecDeque};

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use serde_json::{json, Value};

/// General MIDI piano-family programs (0-7) plus chromatic percussion + organ.
fn is_key_program(program: i32) -> bool {
    (0..24).contains(&program)
}

const KEYBOARD_NAME_HINTS: [&str; 12] = [
    "piano",
    "keys",
    "keyboard",
    "synth",
    "organ",
    "rhodes",
    "harpsichord",
    "clavinet",
    "wurlitzer",
    "ep ",
    "epiano",
    "electric piano",
];

/// A picker-UI track descriptor. `channel_filter` is set for format-0 split
/// entries; `None` means "use every non-drum channel".
#[derive(Debug, Clone, serde::Serialize)]
pub struct MidiTrackInfo {
    pub index: usize,
    pub channel_filter: Option<i32>,
    pub name: String,
    pub instrument: i32,
    pub notes: usize,
    pub channel: i32,
    pub is_piano: bool,
    pub is_drums: bool,
    pub strings: usize,
    pub is_percussion: bool,
}

#[derive(Default, Clone, Copy)]
struct ChannelStats {
    program: i32,
    notes: usize,
}

fn midi_type(format: Format) -> u8 {
    match format {
        Format::SingleTrack => 0,
        Format::Parallel => 1,
        Format::Sequential => 2,
    }
}

/// Return a list of track descriptors suitable for the picker UI.
///
/// Format-0 files (a single track holding every channel) are split into one
/// virtual entry per non-drum channel. Type-1/2 files keep their
/// one-entry-per-track shape. Channel-9 (drums) is dropped entirely.
pub fn list_midi_tracks(midi_path: &str) -> Result<Vec<MidiTrackInfo>, String> {
    let data = std::fs::read(midi_path).map_err(|e| format!("failed to read {midi_path}: {e}"))?;
    let smf = Smf::parse(&data).map_err(|e| format!("failed to parse MIDI: {e}"))?;

    let mtype = midi_type(smf.header.format);
    let split_format = mtype == 0;

    let mut tracks: Vec<MidiTrackInfo> = Vec::new();

    for (i, track) in smf.tracks.iter().enumerate() {
        let mut name = String::new();
        let mut per_channel: HashMap<i32, ChannelStats> = HashMap::new();

        for ev in track {
            match &ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(bytes)) => {
                    if name.is_empty() {
                        name = String::from_utf8_lossy(bytes).into_owned();
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int() as i32;
                    match message {
                        MidiMessage::ProgramChange { program } => {
                            let slot = per_channel.entry(ch).or_insert(ChannelStats {
                                program: -1,
                                notes: 0,
                            });
                            if slot.program < 0 {
                                slot.program = program.as_int() as i32;
                            }
                        }
                        MidiMessage::NoteOn { vel, .. } if vel.as_int() > 0 => {
                            let slot = per_channel.entry(ch).or_insert(ChannelStats {
                                program: -1,
                                notes: 0,
                            });
                            slot.notes += 1;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Drop channels with no notes and drop the drum channel (9).
        let mut active_channels: Vec<i32> = per_channel
            .iter()
            .filter(|(&ch, info)| info.notes > 0 && ch != 9)
            .map(|(&ch, _)| ch)
            .collect();
        active_channels.sort_unstable();

        if active_channels.is_empty() {
            continue;
        }

        let split = split_format && active_channels.len() > 1;

        let iter_channels: Vec<i32> = if split {
            active_channels.clone()
        } else {
            vec![active_channels[0]]
        };

        for ch in iter_channels {
            let info = per_channel[&ch];
            let program = info.program;
            let note_count = if split {
                info.notes
            } else {
                active_channels
                    .iter()
                    .map(|c| per_channel[c].notes)
                    .sum::<usize>()
            };

            let entry_name = if split {
                let base = if name.is_empty() {
                    format!("Track {i}")
                } else {
                    name.clone()
                };
                format!("{base} — Ch{}", ch + 1)
            } else if name.is_empty() {
                format!("Track {i}")
            } else {
                name.clone()
            };

            // Classify on the per-channel program first; fall back to the
            // name heuristic only for single-channel tracks with no program.
            let is_piano = if is_key_program(program) {
                true
            } else if program < 0 && !split {
                let name_lower = entry_name.to_lowercase();
                KEYBOARD_NAME_HINTS.iter().any(|h| name_lower.contains(h))
            } else {
                false
            };

            tracks.push(MidiTrackInfo {
                index: i,
                channel_filter: if split { Some(ch) } else { None },
                name: entry_name,
                instrument: program,
                notes: note_count,
                channel: ch,
                is_piano,
                is_drums: false,
                strings: 0,
                is_percussion: false,
            });
        }
    }

    Ok(tracks)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// O(log N) tempo-aware tick→seconds via a cumulative table + binary search.
fn tick_to_seconds(
    tick: i64,
    tempo_table: &[(i64, f64, i64)],
    tempo_ticks: &[i64],
    ticks_per_beat: i64,
) -> f64 {
    // bisect_right(tempo_ticks, tick) - 1
    let mut i = tempo_ticks.partition_point(|&t| t <= tick);
    if i == 0 {
        i = 0;
    } else {
        i -= 1;
    }
    let (base_tick, base_seconds, tempo) = tempo_table[i];
    base_seconds + (tick - base_tick) as f64 * (tempo as f64 / 1_000_000.0) / ticks_per_beat as f64
}

fn emit_note(
    notes_out: &mut Vec<(f64, Value)>,
    tempo_table: &[(i64, f64, i64)],
    tempo_ticks: &[i64],
    ticks_per_beat: i64,
    audio_offset: f64,
    pitch: i32,
    start_tick: i64,
    end_tick: i64,
) {
    let t = tick_to_seconds(start_tick, tempo_table, tempo_ticks, ticks_per_beat) + audio_offset;
    let end = tick_to_seconds(end_tick, tempo_table, tempo_ticks, ticks_per_beat) + audio_offset;
    let sus = (end - t).max(0.0);
    let v = json!({
        "t": round3(t),
        "s": pitch.div_euclid(24),
        "f": pitch.rem_euclid(24),
        "sus": round3(sus),
        "sl": -1, "slu": -1, "bn": 0,
        "ho": false, "po": false, "hm": false, "hp": false,
        "pm": false, "mt": false, "tr": false, "ac": false, "tp": false,
    });
    notes_out.push((t, v));
}

/// Convert a single MIDI track into a sloppak-format keys arrangement.
///
/// Encodes each MIDI note as `string = pitch // 24`, `fret = pitch % 24`.
/// Honors CC64 (sustain pedal): notes released while the pedal is held are
/// extended to the pedal-up event on the same channel. Channel 9 (percussion)
/// is skipped.
pub fn convert_midi_track_to_keys_wire(
    midi_path: &str,
    track_index: usize,
    audio_offset: f64,
    name: &str,
    channel_filter: Option<i32>,
) -> Result<Value, String> {
    let data = std::fs::read(midi_path).map_err(|e| format!("failed to read {midi_path}: {e}"))?;
    let smf = Smf::parse(&data).map_err(|e| format!("failed to parse MIDI: {e}"))?;

    if track_index >= smf.tracks.len() {
        return Err(format!("track_index {track_index} out of range"));
    }

    let ticks_per_beat: i64 = match smf.header.timing {
        Timing::Metrical(n) => n.as_int() as i64,
        _ => 480,
    };
    let mtype = midi_type(smf.header.format);

    // Build a tempo map with a format-aware scope.
    let mut raw_events: Vec<(i64, i64)> = vec![(0, 500_000)]; // default 120 BPM

    let tempo_source_indices: Vec<usize> = if mtype == 2 {
        vec![track_index]
    } else {
        (0..smf.tracks.len()).collect()
    };
    for &ti in &tempo_source_indices {
        let mut abs_tick = 0i64;
        for ev in &smf.tracks[ti] {
            abs_tick += ev.delta.as_int() as i64;
            if let TrackEventKind::Meta(MetaMessage::Tempo(tempo)) = &ev.kind {
                raw_events.push((abs_tick, tempo.as_int() as i64));
            }
        }
    }
    raw_events.sort_by_key(|e| e.0);

    // Deduplicate at same tick (keep the last one written).
    let mut deduped: Vec<(i64, i64)> = Vec::new();
    for ev in raw_events {
        if let Some(last) = deduped.last_mut() {
            if last.0 == ev.0 {
                *last = ev;
                continue;
            }
        }
        deduped.push(ev);
    }

    // Precompute (tick, cumulative_seconds, micros_per_beat).
    let mut tempo_table: Vec<(i64, f64, i64)> = Vec::new();
    let mut cum_seconds = 0.0f64;
    let mut prev_tick = 0i64;
    let mut prev_tempo = deduped[0].1;
    for (ev_tick, ev_tempo) in &deduped {
        cum_seconds +=
            (ev_tick - prev_tick) as f64 * (prev_tempo as f64 / 1_000_000.0) / ticks_per_beat as f64;
        tempo_table.push((*ev_tick, cum_seconds, *ev_tempo));
        prev_tick = *ev_tick;
        prev_tempo = *ev_tempo;
    }
    let tempo_ticks: Vec<i64> = tempo_table.iter().map(|r| r.0).collect();

    // Walk the requested track.
    let mut abs_tick = 0i64;
    let mut active: HashMap<(i32, i32), VecDeque<i64>> = HashMap::new();
    let mut pedal_pending: HashMap<i32, Vec<(i32, i64)>> = HashMap::new();
    let mut pedal_down: HashMap<i32, bool> = HashMap::new();
    let mut notes_out: Vec<(f64, Value)> = Vec::new();

    for ev in &smf.tracks[track_index] {
        abs_tick += ev.delta.as_int() as i64;

        let (msg_ch, message) = match &ev.kind {
            TrackEventKind::Midi { channel, message } => (channel.as_int() as i32, Some(message)),
            _ => (-1, None),
        };

        // Channel filter for format-0 split entries. Channel-less events
        // (channel == -1) pass through.
        if let Some(cf) = channel_filter {
            if msg_ch != -1 && msg_ch != cf {
                continue;
            }
        }

        let message = match message {
            Some(m) => m,
            None => continue,
        };

        match message {
            MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                if msg_ch == 9 {
                    continue; // skip percussion
                }
                let pitch = key.as_int() as i32;
                active
                    .entry((msg_ch, pitch))
                    .or_default()
                    .push_back(abs_tick);
            }
            MidiMessage::NoteOff { key, .. } | MidiMessage::NoteOn { key, .. } => {
                // note_off, or note_on with velocity 0.
                let pitch = key.as_int() as i32;
                let start_tick = {
                    let stack = match active.get_mut(&(msg_ch, pitch)) {
                        Some(s) => s,
                        None => continue,
                    };
                    let st = stack.pop_front();
                    if stack.is_empty() {
                        active.remove(&(msg_ch, pitch));
                    }
                    match st {
                        Some(v) => v,
                        None => continue,
                    }
                };
                if *pedal_down.get(&msg_ch).unwrap_or(&false) {
                    pedal_pending
                        .entry(msg_ch)
                        .or_default()
                        .push((pitch, start_tick));
                } else {
                    emit_note(
                        &mut notes_out,
                        &tempo_table,
                        &tempo_ticks,
                        ticks_per_beat,
                        audio_offset,
                        pitch,
                        start_tick,
                        abs_tick,
                    );
                }
            }
            MidiMessage::Controller { controller, value } if controller.as_int() == 64 => {
                let was_down = *pedal_down.get(&msg_ch).unwrap_or(&false);
                let now_down = value.as_int() >= 64;
                pedal_down.insert(msg_ch, now_down);
                if was_down && !now_down {
                    if let Some(pending) = pedal_pending.remove(&msg_ch) {
                        for (pitch, start_tick) in pending {
                            emit_note(
                                &mut notes_out,
                                &tempo_table,
                                &tempo_ticks,
                                ticks_per_beat,
                                audio_offset,
                                pitch,
                                start_tick,
                                abs_tick,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // End-of-track: close anything still active or held by the pedal.
    let active_snapshot: Vec<((i32, i32), Vec<i64>)> = active
        .iter()
        .map(|(k, v)| (*k, v.iter().copied().collect()))
        .collect();
    for ((_ch, pitch), starts) in active_snapshot {
        for start_tick in starts {
            emit_note(
                &mut notes_out,
                &tempo_table,
                &tempo_ticks,
                ticks_per_beat,
                audio_offset,
                pitch,
                start_tick,
                abs_tick,
            );
        }
    }
    active.clear();

    let pending_snapshot: Vec<(i32, i64)> = pedal_pending
        .values()
        .flat_map(|v| v.iter().copied())
        .collect();
    for (pitch, start_tick) in pending_snapshot {
        emit_note(
            &mut notes_out,
            &tempo_table,
            &tempo_ticks,
            ticks_per_beat,
            audio_offset,
            pitch,
            start_tick,
            abs_tick,
        );
    }
    pedal_pending.clear();

    notes_out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let notes: Vec<Value> = notes_out.into_iter().map(|(_, v)| v).collect();

    Ok(json!({
        "name": name,
        "tuning": [0, 0, 0, 0, 0, 0],
        "capo": 0,
        "notes": notes,
        "chords": [],
        "anchors": [],
        "handshapes": [],
        "templates": [],
    }))
}
