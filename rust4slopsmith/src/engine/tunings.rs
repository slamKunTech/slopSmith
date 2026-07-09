//! Map a Rocksmith per-string semitone-offset array to a human-readable name.
//! Direct port of `lib/tunings.py`. Kept separate from the server so the
//! naming conventions are 6-string-specific (7+ strings fall through to a
//! numeric fallback).

/// `tuning_name(offsets)` — mirrors lib/tunings.py:8.
pub fn tuning_name(offsets: &[i64]) -> String {
    // Standard tunings (all six strings same offset)
    let standard: &[(i64, &str)] = &[
        (0, "E Standard"),
        (-1, "Eb Standard"),
        (-2, "D Standard"),
        (-3, "C# Standard"),
        (-4, "C Standard"),
        (-5, "B Standard"),
        (-6, "Bb Standard"),
        (-7, "A Standard"),
        (1, "F Standard"),
        (2, "F# Standard"),
    ];
    if offsets.len() == 6 && offsets.iter().all(|&o| o == offsets[0]) {
        if let Some(&(_, name)) = standard.iter().find(|(k, _)| *k == offsets[0]) {
            return name.to_string();
        }
    }

    // Drop tunings (low string 2 semitones below the rest)
    if offsets.len() == 6
        && offsets[0] == offsets[1] - 2
        && offsets[1..].iter().all(|&o| o == offsets[1])
    {
        let note_names = [
            "E", "F", "F#", "G", "Ab", "A", "Bb", "B", "C", "C#", "D", "Eb",
        ];
        let low_note = note_names[((offsets[0] % 12 + 12) % 12) as usize];
        return format!("Drop {low_note}");
    }

    // Common named tunings
    let named: &[(&[i64], &str)] = &[
        (&[-2, 0, 0, 0, 0, 0], "Drop D"),
        (&[-4, -2, -2, -2, -2, -2], "Drop C"),
        (&[-2, -2, 0, 0, 0, 0], "Double Drop D"),
        (&[0, 0, 0, -1, 0, 0], "Open G"),
        (&[-2, -2, 0, 0, -2, -2], "Open D"),
        (&[-2, 0, 0, 0, -2, 0], "DADGAD"),
        (&[0, 2, 2, 1, 0, 0], "Open E"),
        (&[-2, 0, 0, 2, 3, 2], "Open D (alt)"),
    ];
    if offsets.len() == 6 {
        if let Some(&(_, name)) = named.iter().find(|(k, _)| k == &offsets) {
            return name.to_string();
        }
    }

    if offsets.is_empty() {
        return "Unknown".to_string();
    }
    offsets
        .iter()
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_and_drop() {
        assert_eq!(tuning_name(&[0, 0, 0, 0, 0, 0]), "E Standard");
        assert_eq!(tuning_name(&[-2, 0, 0, 0, 0, 0]), "Drop D");
        assert_eq!(tuning_name(&[-4, -2, -2, -2, -2, -2]), "Drop C");
        assert_eq!(tuning_name(&[-2, 0, 0, 0, -2, 0]), "DADGAD");
    }

    #[test]
    fn non_six_string_falls_through() {
        // 7-string all-zeros is NOT "E Standard".
        assert_eq!(tuning_name(&[0, 0, 0, 0, 0, 0, 0]), "0 0 0 0 0 0 0");
    }
}
