//! Pitch conversions: note names ⇄ MIDI ⇄ Hz.

pub const C5_HZ: f64 = 523.2511306;
#[allow(dead_code)]
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn note_to_hz(note: i32) -> f64 {
    C5_HZ * 2f64.powf((note as f64 - 60.0) / 12.0)
}

pub fn hz_to_note(hz: f64) -> i32 {
    (60.0 + 12.0 * (hz / C5_HZ).log2()).round() as i32
}

fn parse_note_name(text: &str) -> Option<i32> {
    let text = text.trim();
    let mut chars = text.chars();
    let first = chars.next()?.to_ascii_uppercase();
    let base = match first {
        'C' => 0, 'D' => 2, 'E' => 4, 'F' => 5,
        'G' => 7, 'A' => 9, 'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (accidental, octave_part) = if let Some(r) = rest.strip_prefix('#') {
        (1i32, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1i32, r)
    } else {
        (0i32, rest)
    };
    let digits = octave_part.strip_prefix('-').unwrap_or(octave_part);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let octave: i32 = octave_part.parse().ok()?;
    Some(octave * 12 + base + accidental)
}

pub fn parse_pitch_midi(text: &str) -> Option<i32> {
    if let Some(n) = parse_note_name(text) {
        return Some(n);
    }
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|h| *h > 0.0)
        .map(hz_to_note)
}

pub fn parse_pitch_hz(text: &str) -> Option<f64> {
    if let Some(n) = parse_note_name(text) {
        return Some(note_to_hz(n));
    }
    text.trim().parse::<f64>().ok().filter(|h| *h > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Halotext note names are scientific pitch: C4 is 261.63 Hz and
    /// A4 is 440 Hz.  The internal note number for C4 is 48, so the
    /// reference frequency paired with note 60 must be C5 (523.25 Hz).
    /// Changing that reference to C4 silently transposes every existing
    /// halotext down an octave — including VGM slide waypoints.
    #[test]
    fn note_names_parse_at_scientific_pitch() {
        for (name, hz) in [
            ("C4", 261.6255653005986),
            ("A4", 440.0),
            ("C5", 523.2511306),
        ] {
            let got = parse_pitch_hz(name).expect(name);
            assert!((got - hz).abs() < 0.01, "{name} -> {got}, expected {hz}");
        }
    }

    #[test]
    fn vgm_midi_round_trip_uses_legacy_note_names() {
        // The VGM converter emits note_name(midi - 12) so that standard
        // MIDI C4 (60) is written as halotext "C4" and parses back to
        // 261.63 Hz.  Lock that pairing down.
        let hz = parse_pitch_hz("C4").unwrap();
        assert!((hz - 261.6255653005986).abs() < 0.01, "C4 -> {hz}");
    }
}