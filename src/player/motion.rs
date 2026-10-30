//! Motion primitives: leaps, pedals, and registers.

use super::analysis_mir::N;

#[derive(Debug, Clone, Default)]
pub struct MotionFeatures {
    pub lowest_pitch: i32,
    pub highest_pitch: i32,
    pub biggest_leap: i32,
    pub is_pedal: bool,
}

pub fn calculate_motion(notes: &[N]) -> MotionFeatures {
    if notes.is_empty() {
        return MotionFeatures::default();
    }

    let mut sorted = notes.to_vec();
    sorted.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());

    let lowest_pitch = sorted.iter().map(|n| n.pitch).min().unwrap_or(0);
    let highest_pitch = sorted.iter().map(|n| n.pitch).max().unwrap_or(0);

    // To avoid flagging chords as "leaps", we only measure the leap between
    // consecutive *melodic* notes (the highest note playing at each moment).
    let mut melody: Vec<i32> = Vec::new();
    let mut last_time = -1.0;

    for n in &sorted {
        if n.start > last_time + 0.1 {
            // New onset, likely a new melodic note
            melody.push(n.pitch);
            last_time = n.start;
        } else if let Some(last) = melody.last_mut() {
            // Same onset, keep the highest note
            if n.pitch > *last {
                *last = n.pitch;
            }
        }
    }

    let mut biggest_leap: i32 = 0;
    for w in melody.windows(2) {
        let diff = w[1] - w[0];
        if diff.abs() > biggest_leap.abs() {
            biggest_leap = diff;
        }
    }

    let lowest_notes: Vec<i32> = sorted
    .iter()
    .map(|n| n.pitch)
    .filter(|&p| p <= lowest_pitch + 2)
    .collect();

    let is_pedal = if lowest_notes.len() >= 2 {
        let min = *lowest_notes.iter().min().unwrap();
        let max = *lowest_notes.iter().max().unwrap();
        max - min <= 2
    } else {
        false
    };

    MotionFeatures {
        lowest_pitch,
        highest_pitch,
        biggest_leap,
        is_pedal,
    }
}
