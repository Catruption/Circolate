use std::collections::HashSet;
use std::sync::atomic::Ordering;
use crate::mixer::mute;
use crate::player::{Project, Transport};
use crate::player::notes::{split_notes, value_at}; // Added imports
use crate::formats::trackscript::hz_to_note;       // Added import

pub struct AnalysisReport {
    pub active_parts: Vec<(String, Vec<String>)>,
    pub chord: String,
    pub root: String,
    pub intervals: Vec<String>,
    pub chromatic_grid: String,
    pub scale: String,
    pub contour: String,
    pub motif: Option<String>,
    pub explanations: Vec<String>,
}

fn midi_to_name(midi: i32) -> String {
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = midi / 12 - 1;
    format!("{}{}", names[(midi % 12) as usize], octave)
}

pub fn analyze(project: &Project, tp: &Transport, line_idx: usize) -> AnalysisReport {
    let snap = mute::snapshot();
    let mut active_parts = Vec::new();
    let mut all_pitches = Vec::new();
    let mut measure_pitches = Vec::new();
    let mut chrono_pitches = Vec::new();
    let mut total_dur = 0.0;
    let mut note_count = 0;
    let mut unique_rhythms = HashSet::new();
    let mut has_bends = false;

    let pos = tp.position.load(Ordering::Relaxed).max(0);
    let line_start = project.line_start_sample(line_idx);
    let measure_samples = project.line_measure_samples(line_idx).max(1);
    let current_frac = ((pos - line_start) as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = current_frac * project.rows_per_measure();

    // 1. Gather notes using split_notes to respect waypoints/bends
    for call in project.line_calls(line_idx) {
        let Some(path) = project.part_path(&call.name) else { continue };

        if let Some(solo) = &snap.soloed_part {
            if solo != path { continue; }
        }
        if snap.muted_parts.contains(path) { continue; }

        let mut part_pitches = Vec::new();
        let subs = project.part_subs_per_beat(&call.name);
        let measure_start = call.pattern_measure_start;
        let measure_end = measure_start + call.pattern_measure_len;

        // split_notes gives us the true start/end of the note, incorporating waypoints
        let notes = split_notes(
            project.part_events(&call.name),
                                subs,
                                project.part_sample_offset_u32(&call.name)
        );

        for note in notes {
            if note.start_row < measure_start || note.start_row >= measure_end {
                continue;
            }

            let local_start = note.start_row - measure_start;
            let local_end = note.end_row - measure_start;

            // If note has multiple pitch points, it bends!
            if note.pitch_points.len() > 1 {
                has_bends = true;
            }

            // Check if this note is active at the playhead
            if current_row >= local_start && current_row < local_end {
                // Get the pitch AT THE PLAYHEAD (this captures the bend!)
                let hz = value_at(&note.pitch_points, current_row + measure_start).unwrap_or(440.0);
                let midi = hz_to_note(hz);
                part_pitches.push(midi);
                all_pitches.push(midi);
            }

            // For measure context, sample the pitch at the start and end of the note
            let start_hz = value_at(&note.pitch_points, note.start_row).unwrap_or(440.0);
            let end_hz = value_at(&note.pitch_points, note.end_row).unwrap_or(start_hz);
            measure_pitches.push(hz_to_note(start_hz));
            measure_pitches.push(hz_to_note(end_hz));
            chrono_pitches.push(hz_to_note(start_hz));

            let dur = note.dur_beats.unwrap_or(1.0);
            total_dur += dur;
            note_count += 1;
            unique_rhythms.insert((dur * 100.0) as i32);
        }

        if !part_pitches.is_empty() {
            part_pitches.sort_unstable();
            part_pitches.dedup();
            let names: Vec<String> = part_pitches.iter().map(|&p| midi_to_name(p)).collect();
            active_parts.push((call.name.clone(), names));
        }
    }

    // 2. Active Harmony
    all_pitches.sort_unstable();
    all_pitches.dedup();

    let mut intervals = Vec::new();
    let mut chord = "None".to_string();
    let mut root = "-".to_string();
    let mut chromatic_grid = "·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·".to_string();
    let mut has_min3 = false;
    let mut has_min2 = false;
    let mut has_tritone = false;

    if !all_pitches.is_empty() {
        let root_midi = all_pitches[0];
        let root_pc = root_midi % 12;
        root = midi_to_name(root_midi);

        let mut grid = vec!['·'; 12];
        for &p in &all_pitches {
            grid[(p % 12) as usize] = '●';
        }
        chromatic_grid = grid.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("  ");

        if all_pitches.len() >= 2 {
            let mut pcs = Vec::new();
            for &p in &all_pitches {
                let pc = p % 12;
                pcs.push(pc);
                let diff = (pc - root_pc + 12) % 12;

                let name = match diff {
                    1 => { has_min2 = true; "Minor 2nd (Harsh)" },
                    2 => "Major 2nd",
                    3 => { has_min3 = true; "Minor 3rd (Dark)" },
                    4 => "Major 3rd (Bright)",
                    5 => "Perfect 4th",
                    6 => { has_tritone = true; "Tritone (Tense)" },
                    7 => "Perfect 5th (Stable)",
                    8 => "Minor 6th",
                    9 => "Major 6th",
                    10 => "Minor 7th",
                    11 => "Major 7th",
                    _ => continue,
                };
                intervals.push(name.to_string());
            }

            let maj3 = pcs.contains(&((root_pc + 4) % 12));
            let p5 = pcs.contains(&((root_pc + 7) % 12));

            if has_tritone { chord = "Diminished / Altered".into(); }
            else if maj3 && p5 { chord = "Major".into(); }
            else if has_min3 && p5 { chord = "Minor".into(); }
            else if maj3 || has_min3 { chord = "(No 5th)".into(); }
            else if p5 { chord = "Power / Suspended".into(); }
            else { chord = "Extended / Cluster".into(); }
        } else {
            chord = "Single Note".into();
        }
    }

    // 3. Measure Context
    let mut scale = "Empty".to_string();
    let mut contour = "Flat".to_string();
    let motif = None;

    if !measure_pitches.is_empty() {
        measure_pitches.sort_unstable();
        let unique_pcs: HashSet<i32> = measure_pitches.iter().map(|p| p % 12).collect();
        scale = match unique_pcs.len() {
            0 => "Empty".into(),
            1..=5 => format!("Pentatonic ({} notes)", unique_pcs.len()),
            6..=7 => format!("Diatonic ({} notes)", unique_pcs.len()),
            _ => format!("Chromatic ({} notes)", unique_pcs.len()),
        };

        let mut max_pitch = chrono_pitches[0];
        let mut min_pitch = chrono_pitches[0];
        for &p in &chrono_pitches {
            if p > max_pitch { max_pitch = p; }
            if p < min_pitch { min_pitch = p; }
        }
        let first = chrono_pitches[0];
        let last = *chrono_pitches.last().unwrap();

        if max_pitch - min_pitch > 12 {
            contour = "Leaping (Octave+ jumps)".into();
        } else if last > first + 2 {
            contour = "Ascending (Rising tension)".into();
        } else if last < first - 2 {
            contour = "Descending (Falling resolution)".into();
        } else {
            contour = "Wavy (Static center)".into();
        }
    }

    // 4. Synthesize Explanations (The "Why")
    let mut explanations = Vec::new();

    if has_bends {
        explanations.push("Pitch bends / slides detected (Glissando automation)".into());
    }

    if !all_pitches.is_empty() {
        if has_min3 { explanations.push("Minor 3rd interval (Core of sadness)".into()); }
        if has_min2 { explanations.push("Minor 2nd dissonance (Sighing tension)".into()); }
        if has_tritone { explanations.push("Tritone clash (Highly unstable)".into()); }
        if contour.contains("Descending") { explanations.push("Descending melody (Falling resolution)".into()); }
        if contour.contains("Leaping") { explanations.push("Wide leaps (Cinematic drama)".into()); }
    } else {
        explanations.push("Playhead is resting in a rhythmic gap".into());

        if note_count >= 8 {
            let avg_dur = total_dur / note_count as f64;
            if avg_dur < 0.4 && unique_rhythms.len() > 1 {
                explanations.push("Staccato, syncopated rhythm (Mechanical, driving)".into());
            } else if avg_dur >= 1.5 {
                explanations.push("Long, sustained notes (Reflective, lingering)".into());
            }
        }
    }

    if explanations.is_empty() {
        explanations.push("Balanced intervals (Neutral, ambient)".into());
    }

    AnalysisReport {
        active_parts,
        chord,
        root,
        intervals,
        chromatic_grid,
        scale,
        contour,
        motif,
        explanations,
    }
}
