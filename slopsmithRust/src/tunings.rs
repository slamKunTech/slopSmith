//! Map a Rocksmith per-string semitone offset array to a human-readable name.
//!
//! Kept as a small standalone module so tests / other modules can use it
//! without pulling in the rest of the song-parsing machinery. This is a
//! direct port of `slopsmith/lib/tunings.py`.

/// Return a human-readable tuning name for a per-string semitone offset array.
///
/// All three pattern checks below are gated on `offsets.len() == 6`. The
/// naming conventions here are 6-string-specific — e.g. a 7-string all-zeros
/// tuning has a low B, not an E, so labeling it "E Standard" would be wrong.
/// 7+-string community content falls through to the numeric fallback.
pub fn tuning_name(offsets: &[i32]) -> String {
    // Standard tunings (all six strings same offset).
    if offsets.len() == 6 && offsets.iter().all(|&o| o == offsets[0]) {
        let name = match offsets[0] {
            0 => Some("E Standard"),
            -1 => Some("Eb Standard"),
            -2 => Some("D Standard"),
            -3 => Some("C# Standard"),
            -4 => Some("C Standard"),
            -5 => Some("B Standard"),
            -6 => Some("Bb Standard"),
            -7 => Some("A Standard"),
            1 => Some("F Standard"),
            2 => Some("F# Standard"),
            _ => None,
        };
        if let Some(n) = name {
            return n.to_string();
        }
    }

    // Drop tunings (low string 2 semitones below the rest). Named after the
    // low string's note: e.g. offsets [-2,0,0,0,0,0] = Drop D (low E dropped
    // to D).
    if offsets.len() == 6
        && offsets[0] == offsets[1] - 2
        && offsets[1..].iter().all(|&o| o == offsets[1])
    {
        let note_names = [
            "E", "F", "F#", "G", "Ab", "A", "Bb", "B", "C", "C#", "D", "Eb",
        ];
        // Python's `%` yields a non-negative result for negative operands.
        let idx = (((offsets[0] % 12) + 12) % 12) as usize;
        return format!("Drop {}", note_names[idx]);
    }

    // Common named tunings.
    if offsets.len() == 6 {
        let t = (
            offsets[0], offsets[1], offsets[2], offsets[3], offsets[4], offsets[5],
        );
        let named = match t {
            (-2, 0, 0, 0, 0, 0) => Some("Drop D"),
            (-4, -2, -2, -2, -2, -2) => Some("Drop C"),
            (-2, -2, 0, 0, 0, 0) => Some("Double Drop D"),
            (0, 0, 0, -1, 0, 0) => Some("Open G"),
            (-2, -2, 0, 0, -2, -2) => Some("Open D"),
            (-2, 0, 0, 0, -2, 0) => Some("DADGAD"),
            (0, 2, 2, 1, 0, 0) => Some("Open E"),
            (-2, 0, 0, 2, 3, 2) => Some("Open D (alt)"),
            _ => None,
        };
        if let Some(n) = named {
            return n.to_string();
        }
    }

    // Numeric fallback — space-joined offsets, or "Unknown" when empty.
    if offsets.is_empty() {
        return "Unknown".to_string();
    }
    offsets
        .iter()
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}
