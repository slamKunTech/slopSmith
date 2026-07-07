//! slopsmith — Rust port of the slopsmith Python library.
//!
//! Browse and practice Rocksmith 2014 Custom DLC. This crate exposes the core
//! library modules (song parsing, PSARC extraction, audio conversion, the
//! sloppak open format, and the various converters) so both the `slopsmith`
//! server binary and the standalone CLI tools (`psarc-to-sloppak`,
//! `split-stems`) can share one implementation.

#![allow(dead_code)]

pub mod audio;
pub mod cdlc_builder;
pub mod gp2midi;
pub mod gp2rs;
pub mod midi_import;
pub mod patcher;
pub mod psarc;
pub mod retune;
pub mod sloppak;
pub mod sng_vocals;
pub mod song;
pub mod tunings;
pub mod wem_decode;
