//! Generate MIDI and render audio from a Guitar Pro file.
//!
//! This is a Rust port of `lib/gp2midi.py`. The Python original uses
//! `pyguitarpro` for parsing and `midiutil` for MIDI output. In Rust we use
//! [`midly`] for MIDI file writing and shell out to `fluidsynth`/`ffmpeg` for
//! audio rendering.
//!
//! Because there is no mature pure-Rust Guitar Pro parser, the Guitar Pro data
//! model is defined here as a set of plain structs and [`parse`] is provided as
//! a stub. The important part translated faithfully is the MIDI-generation and
//! audio-rendering pipeline that operates on that data model.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

pub const GP_TICKS_PER_QUARTER: u32 = 960;

/// Standard tuning MIDI values (GP string order: 1 = high, 6 = low): e B G D A E
pub const STANDARD_6: [i32; 6] = [64, 59, 55, 50, 45, 40];
/// Standard bass tuning: G D A E
pub const STANDARD_4: [i32; 4] = [43, 38, 33, 28];

// ─── Guitar Pro data model ────────────────────────────────────────────────
//
// A minimal representation of the subset of the Guitar Pro object model that
// the MIDI exporter needs. In the Python original these come from
// `pyguitarpro`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteType {
    Rest,
    Normal,
    Tie,
    Dead,
}

#[derive(Debug, Clone, Default)]
pub struct Tuplet {
    pub enters: i32,
    pub times: i32,
}

#[derive(Debug, Clone)]
pub struct Duration {
    /// 1 = whole, 2 = half, 4 = quarter, 8 = eighth, ...
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

#[derive(Debug, Clone, Default)]
pub struct NoteEffect {
    pub ghost_note: bool,
    pub palm_mute: bool,
    pub accentuated_note: bool,
    pub heavy_accentuated_note: bool,
    pub tremolo_picking: bool,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub note_type: NoteType,
    /// 1-based GP string number (1 = highest).
    pub string: i32,
    /// Fret number.
    pub value: i32,
    pub velocity: i32,
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
pub struct BeatEffect {
    pub mix_table_change: Option<MixTableChange>,
}

#[derive(Debug, Clone)]
pub struct Beat {
    /// Absolute start position in GP ticks (960 per quarter).
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
    /// 1-based string number (1 = highest).
    pub number: i32,
    /// Open-string MIDI value.
    pub value: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Channel {
    pub instrument: i32,
    pub volume: i32,
    pub balance: i32,
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
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub copyright: String,
    pub tempo: f64,
    pub tracks: Vec<Track>,
}

/// Parse a Guitar Pro file into a [`Song`].
///
/// There is no mature pure-Rust Guitar Pro parser, so this is a stub. In a
/// full deployment this would either bind to a native GP library or implement
/// the GP3/GP4/GP5 binary format. The rest of this module operates on the
/// [`Song`] model and is fully functional.
pub fn parse(gp_path: &str) -> Result<Song, String> {
    let _ = gp_path;
    Err(
        "Guitar Pro parsing is not implemented in the Rust port yet; \
         provide a Song via the data model or wire up a GP parser."
            .to_string(),
    )
}

// ─── MIDI generation ───────────────────────────────────────────────────────

/// Compute the effective duration of a beat in quarter notes.
fn beat_duration_quarters(dur: &Duration) -> f64 {
    let mut dur_quarters = 4.0 / dur.value as f64;
    if dur.is_dotted {
        dur_quarters *= 1.5;
    }
    if dur.tuplet.enters > 0 && dur.tuplet.times > 0 {
        dur_quarters *= dur.tuplet.times as f64 / dur.tuplet.enters as f64;
    }
    dur_quarters
}

/// Convert a BPM tempo to microseconds-per-quarter-note (MIDI tempo unit).
fn bpm_to_mpq(bpm: f64) -> u32 {
    if bpm <= 0.0 {
        500_000 // 120 BPM default
    } else {
        (60_000_000.0 / bpm).round() as u32
    }
}

/// An event with an absolute tick position; `order` breaks ties at the same
/// tick so setup/meta events precede note events.
struct AbsEvent<'a> {
    tick: u32,
    order: u8,
    kind: TrackEventKind<'a>,
}

/// Convert a Guitar Pro file to a MIDI file.
///
/// * `track_indices` — which tracks to include (`None` = all tracks).
/// * `force_standard_tuning` — if true, use E standard tuning for all
///   instruments (fret numbers are kept, open-string pitches change).
///
/// Returns the path to the written MIDI file.
pub fn gp_to_midi(
    gp_path: &str,
    output_midi: &str,
    track_indices: Option<Vec<usize>>,
    force_standard_tuning: bool,
) -> Result<String, String> {
    let song = parse(gp_path)?;

    let track_indices: Vec<usize> =
        track_indices.unwrap_or_else(|| (0..song.tracks.len()).collect());

    // Track names are owned here so the borrowed MetaMessage::TrackName events
    // remain valid for the lifetime of the Smf.
    let track_names: Vec<Vec<u8>> = track_indices
        .iter()
        .map(|&i| song.tracks[i].name.clone().into_bytes())
        .collect();

    let mut midi_tracks: Vec<Vec<TrackEvent>> = Vec::with_capacity(track_indices.len());

    for (midi_track_idx, &gp_track_idx) in track_indices.iter().enumerate() {
        let track = &song.tracks[gp_track_idx];
        let is_perc = track.is_percussion_track;

        // MIDI channel: percussion must be 9, others avoid 9.
        let channel_num: u8 = if is_perc {
            9
        } else {
            let c = if midi_track_idx < 9 {
                midi_track_idx
            } else {
                midi_track_idx + 1
            };
            c.min(15) as u8
        };
        let channel = u4::from_int_lossy(channel_num);

        let mut events: Vec<AbsEvent> = Vec::new();

        // Track name.
        events.push(AbsEvent {
            tick: 0,
            order: 0,
            kind: TrackEventKind::Meta(MetaMessage::TrackName(&track_names[midi_track_idx])),
        });

        // Initial tempo.
        events.push(AbsEvent {
            tick: 0,
            order: 1,
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::from_int_lossy(bpm_to_mpq(
                song.tempo,
            )))),
        });

        // Instrument program change.
        if !is_perc {
            let program = match &track.channel {
                Some(ch) => ch.instrument,
                None => 29, // overdriven guitar
            };
            let program = program.clamp(0, 127) as u8;
            events.push(AbsEvent {
                tick: 0,
                order: 2,
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::ProgramChange {
                        program: u7::from_int_lossy(program),
                    },
                },
            });
        }

        // Volume (CC7) and pan (CC10).
        if let Some(ch) = &track.channel {
            let vol = ch.volume.min(127).max(0) as u8;
            let pan = ch.balance.min(127).max(0) as u8;
            events.push(AbsEvent {
                tick: 0,
                order: 3,
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::Controller {
                        controller: u7::from_int_lossy(7),
                        value: u7::from_int_lossy(vol),
                    },
                },
            });
            events.push(AbsEvent {
                tick: 0,
                order: 3,
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::Controller {
                        controller: u7::from_int_lossy(10),
                        value: u7::from_int_lossy(pan),
                    },
                },
            });
        }

        // Tempo changes from mix-table changes.
        let mut tempo_added: HashSet<i64> = HashSet::new();
        for measure in &track.measures {
            for voice in &measure.voices {
                for beat in &voice.beats {
                    if let Some(mtc) = beat
                        .effect
                        .mix_table_change
                        .as_ref()
                        .and_then(|m| m.tempo.as_ref())
                    {
                        if mtc.value > 0 && !tempo_added.contains(&beat.start) {
                            events.push(AbsEvent {
                                tick: beat.start.max(0) as u32,
                                order: 1,
                                kind: TrackEventKind::Meta(MetaMessage::Tempo(
                                    u24::from_int_lossy(bpm_to_mpq(mtc.value as f64)),
                                )),
                            });
                            tempo_added.insert(beat.start);
                        }
                    }
                }
            }
        }

        // Notes.
        for measure in &track.measures {
            for voice in &measure.voices {
                for beat in &voice.beats {
                    if beat.notes.is_empty() {
                        continue;
                    }

                    let beat_tick = beat.start.max(0) as u32;
                    let dur_quarters = beat_duration_quarters(&beat.duration);

                    for note in &beat.notes {
                        if note.note_type == NoteType::Rest {
                            continue;
                        }

                        let string_midi = if force_standard_tuning && !is_perc {
                            let num_strings = track.strings.len();
                            let idx = (note.string - 1) as usize;
                            if num_strings == 4 {
                                if idx < STANDARD_4.len() {
                                    STANDARD_4[idx]
                                } else {
                                    track.strings[idx].value
                                }
                            } else if idx < STANDARD_6.len() {
                                STANDARD_6[idx]
                            } else {
                                track.strings[idx].value
                            }
                        } else {
                            track.strings[(note.string - 1) as usize].value
                        };
                        let pitch = string_midi + note.value;

                        // Duration in quarters → ticks.
                        let dur_q = if note.note_type == NoteType::Dead {
                            0.05
                        } else if dur_quarters <= 0.0 {
                            0.05
                        } else {
                            dur_quarters
                        };
                        let dur_ticks =
                            ((dur_q * GP_TICKS_PER_QUARTER as f64).round() as u32).max(1);

                        let mut velocity = note.velocity;
                        if note.effect.ghost_note {
                            velocity = (velocity / 2).max(20);
                        }
                        if velocity <= 0 {
                            velocity = 1;
                        }
                        velocity = velocity.min(127);

                        // Skip out-of-range pitches (midiutil would crash).
                        if pitch < 0 || pitch > 127 {
                            continue;
                        }

                        let key = u7::from_int_lossy(pitch as u8);
                        events.push(AbsEvent {
                            tick: beat_tick,
                            order: 5,
                            kind: TrackEventKind::Midi {
                                channel,
                                message: MidiMessage::NoteOn {
                                    key,
                                    vel: u7::from_int_lossy(velocity as u8),
                                },
                            },
                        });
                        // Note-off ordered before note-ons (order 4) so a note
                        // ending exactly when another starts doesn't cut it.
                        events.push(AbsEvent {
                            tick: beat_tick + dur_ticks,
                            order: 4,
                            kind: TrackEventKind::Midi {
                                channel,
                                message: MidiMessage::NoteOff {
                                    key,
                                    vel: u7::from_int_lossy(0),
                                },
                            },
                        });
                    }
                }
            }
        }

        // Sort by (tick, order) — stable so identical keys keep insertion order.
        events.sort_by_key(|e| (e.tick, e.order));

        // Convert absolute ticks to delta times.
        let mut track_events: Vec<TrackEvent> = Vec::with_capacity(events.len() + 1);
        let mut prev_tick: u32 = 0;
        let mut last_tick: u32 = 0;
        for ev in events {
            let delta = ev.tick.saturating_sub(prev_tick);
            prev_tick = ev.tick;
            last_tick = ev.tick;
            track_events.push(TrackEvent {
                delta: u28::from_int_lossy(delta),
                kind: ev.kind,
            });
        }
        let _ = last_tick;
        track_events.push(TrackEvent {
            delta: u28::from_int_lossy(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });

        midi_tracks.push(track_events);
    }

    let header = Header::new(
        Format::Parallel,
        Timing::Metrical(u15::from_int_lossy(GP_TICKS_PER_QUARTER as u16)),
    );
    let smf = Smf {
        header,
        tracks: midi_tracks,
    };

    smf.save(output_midi)
        .map_err(|e| format!("failed to write MIDI file {output_midi}: {e}"))?;

    Ok(output_midi.to_string())
}

// ─── Soundfont discovery ────────────────────────────────────────────────────

/// Locate a `.sf2` soundfont for MIDI rendering.
///
/// Precedence:
///   1. `SLOPSMITH_SOUNDFONT` env var (user override / desktop-app supplied)
///   2. Bundled `<RESOURCESPATH>/soundfonts/*.sf2` (Electron desktop builds)
///   3. Common system locations per OS.
pub fn find_soundfont() -> Option<String> {
    if let Ok(override_path) = std::env::var("SLOPSMITH_SOUNDFONT") {
        if !override_path.is_empty() {
            if Path::new(&override_path).is_file() {
                return Some(override_path);
            }
            eprintln!(
                "[slopsmith] SLOPSMITH_SOUNDFONT is set to {override_path:?} but that file \
                 does not exist; falling back to other sources."
            );
        }
    }

    if let Ok(resources) = std::env::var("RESOURCESPATH") {
        let pattern = format!("{resources}/soundfonts/*.sf2");
        if let Ok(paths) = glob::glob(&pattern) {
            let mut matches: Vec<String> = paths
                .filter_map(|p| p.ok())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            matches.sort();
            if let Some(first) = matches.into_iter().next() {
                return Some(first);
            }
        }
    }

    let mut candidates: Vec<String> = Vec::new();
    if cfg!(target_os = "linux") {
        candidates.extend(
            [
                "/usr/share/soundfonts/FluidR3_GM.sf2",
                "/usr/share/soundfonts/FluidR3_GS.sf2",
                "/usr/share/soundfonts/default.sf2",
                "/usr/share/sounds/sf2/FluidR3_GM.sf2",
                "/usr/share/sounds/sf2/default-GM.sf2",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
    } else if cfg!(target_os = "macos") {
        candidates.extend(
            [
                "/opt/homebrew/share/sounds/sf2/FluidR3_GM.sf2",
                "/opt/homebrew/share/soundfonts/FluidR3_GM.sf2",
                "/usr/local/share/sounds/sf2/FluidR3_GM.sf2",
                "/usr/local/share/soundfonts/FluidR3_GM.sf2",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
    } else if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            for pattern in [
                format!("{appdata}/Slopsmith/soundfonts/*.sf2"),
                format!("{appdata}/SoundFonts/*.sf2"),
            ] {
                if let Ok(paths) = glob::glob(&pattern) {
                    let mut matches: Vec<String> = paths
                        .filter_map(|p| p.ok())
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    matches.sort();
                    candidates.extend(matches);
                }
            }
        }
    }

    candidates.into_iter().find(|p| Path::new(p).is_file())
}

fn soundfont_install_hint() -> String {
    if cfg!(target_os = "linux") {
        "Install a soundfont:\n  Arch/Manjaro:  sudo pacman -S soundfont-fluid\n  \
         Debian/Ubuntu: sudo apt install fluid-soundfont-gm\n  \
         Fedora:        sudo dnf install fluid-soundfont-gm"
            .to_string()
    } else if cfg!(target_os = "macos") {
        "Download a soundfont (e.g. GeneralUser GS from schristiancollins.com or FluidR3_GM \
         from musical-artifacts.com) and either place the .sf2 file in \
         /usr/local/share/sounds/sf2/ (Intel) or /opt/homebrew/share/sounds/sf2/ (Apple \
         Silicon), or set the SLOPSMITH_SOUNDFONT environment variable to its full path."
            .to_string()
    } else if cfg!(target_os = "windows") {
        "Download a soundfont (e.g. GeneralUser GS from schristiancollins.com or FluidR3_GM \
         from musical-artifacts.com) and either place the .sf2 file in \
         %APPDATA%\\Slopsmith\\soundfonts\\ or set the SLOPSMITH_SOUNDFONT environment \
         variable to its full path."
            .to_string()
    } else {
        "Set SLOPSMITH_SOUNDFONT to the full path of a .sf2 file.".to_string()
    }
}

fn fluidsynth_install_hint() -> String {
    if cfg!(target_os = "linux") {
        "Install fluidsynth:\n  Arch/Manjaro:  sudo pacman -S fluidsynth\n  \
         Debian/Ubuntu: sudo apt install fluidsynth\n  \
         Fedora:        sudo dnf install fluidsynth"
            .to_string()
    } else if cfg!(target_os = "macos") {
        "Install fluidsynth with Homebrew: brew install fluid-synth".to_string()
    } else if cfg!(target_os = "windows") {
        "Install fluidsynth (https://github.com/FluidSynth/fluidsynth/releases) and ensure \
         fluidsynth.exe is on your PATH."
            .to_string()
    } else {
        "Install fluidsynth and ensure it is on PATH.".to_string()
    }
}

/// Render a MIDI file to OGG audio using fluidsynth (falling back to WAV if the
/// ffmpeg conversion fails).
pub fn render_midi_to_audio(midi_path: &str, output_path: &str) -> Result<String, String> {
    let soundfont = find_soundfont()
        .ok_or_else(|| format!("No soundfont found. {}", soundfont_install_hint()))?;

    let wav_path = format!("{output_path}.wav");
    let ogg_path = format!("{output_path}.ogg");

    let result = Command::new("fluidsynth")
        .args([
            "-ni",
            "-T",
            "wav",
            "-F",
            &wav_path,
            "-r",
            "44100",
            &soundfont,
            midi_path,
        ])
        .output();

    let result = match result {
        Ok(o) => o,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Err(format!("fluidsynth not found. {}", fluidsynth_install_hint()));
            }
            return Err(format!("fluidsynth failed to launch: {e}"));
        }
    };

    if !result.status.success() || !Path::new(&wav_path).exists() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let tail: String = stderr.chars().rev().take(300).collect::<String>().chars().rev().collect();
        return Err(format!("fluidsynth failed: {tail}"));
    }

    let ff = Command::new("ffmpeg")
        .args(["-y", "-i", &wav_path, "-q:a", "6", &ogg_path])
        .output();

    if let Ok(ff) = ff {
        if ff.status.success() && Path::new(&ogg_path).exists() {
            let _ = std::fs::remove_file(&wav_path);
            return Ok(ogg_path);
        }
    }

    Ok(wav_path)
}

/// Convert a Guitar Pro file directly to audio.
///
/// Generates a temporary MIDI file and renders it with FluidSynth. Returns the
/// path to the produced audio file.
pub fn gp_to_audio(
    gp_path: &str,
    output_path: &str,
    track_indices: Option<Vec<usize>>,
    force_standard_tuning: bool,
) -> Result<String, String> {
    let mut tmp_midi = std::env::temp_dir();
    let unique = format!(
        "rs_midi_{}.mid",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    tmp_midi.push(unique);
    let tmp_midi = tmp_midi.to_string_lossy().into_owned();

    let tuning_label = if force_standard_tuning {
        " (E Standard)"
    } else {
        ""
    };
    let name = Path::new(gp_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| gp_path.to_string());
    println!("Generating MIDI from {name}{tuning_label}...");

    let result = (|| {
        gp_to_midi(gp_path, &tmp_midi, track_indices, force_standard_tuning)?;
        println!("Rendering audio with FluidSynth...");
        render_midi_to_audio(&tmp_midi, output_path)
    })();

    if Path::new(&tmp_midi).exists() {
        let _ = std::fs::remove_file(&tmp_midi);
    }

    result
}
