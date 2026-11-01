//! A deterministic music theory and motion engine.

use crate::player::analysis_mir::Features;

pub fn explain_observations(f: &Features) -> Vec<String> {
    let mut out = Vec::new();

    if let (Some(current), Some(previous)) = (f.melody_mean, f.prev_melody_mean) {
        let shift = (current - previous).round() as i32;
        if shift.abs() >= 5 {
            let direction = if shift > 0 { "up" } else { "down" };
            out.push(format!("The upper line's average register moves {} {} semitones from the previous line.", direction, shift.abs()));
        }
    }
    if f.motion.biggest_leap.abs() >= 7 {
        let d = f.motion.biggest_leap;
        out.push(format!("The upper line has a {}-semitone {} leap.", d.abs(), if d > 0 { "upward" } else { "downward" }));
    }
    if f.motion.is_pedal {
        out.push("The lowest pitched part repeats within a two-semitone range, forming a pedal-like anchor.".into());
    }
    if let Some(c) = &f.chord {
        if c.missing_third {
            out.push(format!("{} has no third, so its major/minor quality is unspecified.", c.name));
        }
    }
    if !f.outside_scale_notes.is_empty() {
        out.push(format!("Pitch classes outside the estimated scale: {}.", f.outside_scale_notes.join(", ")));
    }
    if f.onsets_per_beat > 2.5 {
        out.push(format!("The line contains {:.1} note onsets per beat.", f.onsets_per_beat));
    } else if f.onsets_per_beat > 0.0 && f.onsets_per_beat < 0.3 {
        out.push(format!("The line is sparse: {:.2} note onsets per beat.", f.onsets_per_beat));
    }

    out.truncate(4);
    out
}
