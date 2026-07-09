//! Library modules ported from `slopsmith/lib/`. Each submodule mirrors a
//! Python file; see the per-module docs. Only the modules in the core server's
//! transitive closure are ported here — the rest (gp2rs, gp2midi, midi_import,
//! cdlc_builder, sloppak_convert, wem_decode) stay in the Python sidecar.

pub mod audio;
pub mod patcher;
pub mod psarc;
pub mod retune;
pub mod sloppak;
pub mod song;
pub mod sng_vocals;
pub mod tunings;
