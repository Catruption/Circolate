use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use crate::mixer::mute;
use crate::player::{Project, Transport};
use crate::player::notes::{split_notes, value_at};
use crate::formats::trackscript::hz_to_note;
use super::analysis_mir::{self, N, Features};
use super::motion; // <--- Import motion
use super::theory;

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
    // New motion fields for the UI
    pub motion_anchor: bool,
    pub motion_leap: i32,
    pub motion_register: String,
}

fn midi_to_name(midi: i32) -> String {
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = midi / 12;
    format!("{}{}", names[(midi % 12) as usize], octave)
}

pub fn analyze(project: &Project, tp: &Transport, line_idx: usize) -> AnalysisReport {
    let snap = mute::snapshot();
    let mut active_parts_map: std::collections::HashMap<String, Vec<i32>> = std::collections::HashMap::new();

    let mut all_pitches = Vec::new();
    let mut window_pitches = Vec::new();

    let mut project_notes = Vec::new();

    let mut measure_notes_by_part: std::collections::BTreeMap<String, Vec<N>> = std::collections::BTreeMap::new();
    let mut prev_measure_notes_by_part: std::collections::BTreeMap<String, Vec<N>> = std::collections::BTreeMap::new();
    let mut total_measure_notes = 0;

    let pos = tp.position.load(Ordering::Relaxed).max(0);
    let line_start = project.line_start_sample(line_idx);
    let measure_samples = project.line_measure_samples(line_idx).max(1);
    let current_frac = ((pos - line_start) as f64 / measure_samples as f64).clamp(0.0, 1.0);

    let rpmeasure = project.rows_per_measure().max(1.0);
    let current_row = current_frac * rpmeasure;

    let lookback = 16;

    for k in 0..=lookback {
        if line_idx < k { break; }
        let check_li = line_idx - k;
        let offset_rows = k as f64 * rpmeasure;

        for call in project.line_calls(check_li) {
            let Some(path) = project.part_path(&call.name) else { continue };

            if let Some(solo) = &snap.soloed_part {
                if solo != path { continue; }
            }
            if snap.muted_parts.contains(path) { continue; }

            let subs = project.part_subs_per_beat(&call.name);
            let measure_start = call.pattern_measure_start;
            let measure_end = measure_start + call.pattern_measure_len;

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
                let beats = (note.end_row - note.start_row) / subs as f64;
                let pitch = hz_to_note(value_at(&note.pitch_points, note.start_row).unwrap_or(440.0));

                let n_struct = N { start: local_start, end: local_end, pitch, beats };

                if k == 0 {
                    project_notes.push(N { start: note.start_row, end: note.end_row, pitch, beats });
                    measure_notes_by_part.entry(call.name.clone()).or_default().push(n_struct);
                    total_measure_notes += 1;
                } else if k == 1 {
                    prev_measure_notes_by_part.entry(call.name.clone()).or_default().push(n_struct);
                }

                let current_part_row = offset_rows + current_row;
                let absolute_row = measure_start + current_part_row;
                let is_active_now = current_part_row >= local_start && current_part_row < local_end;

                if k == 0 {
                    active_parts_map.entry(call.name.clone()).or_default().push(pitch);
                } else if is_active_now {
                    active_parts_map.entry(call.name.clone()).or_default().push(hz_to_note(value_at(&note.pitch_points, absolute_row).unwrap_or(440.0)));
                }

                if is_active_now {
                    let hz = value_at(&note.pitch_points, absolute_row).unwrap_or(440.0);
                    let pitch = hz_to_note(hz);
                    all_pitches.push(pitch);
                    window_pitches.push(pitch);
                } else if k == 0 {
                    let row_diff = (local_start - current_row).abs();
                    if row_diff <= 1.0 {
                        window_pitches.push(pitch);
                    }
                }
            }
        }
    }

    let mut active_parts = Vec::new();
    for (name, mut pitches) in active_parts_map {
        pitches.sort_unstable();
        pitches.dedup();
        let names: Vec<String> = pitches.iter().map(|&p| midi_to_name(p)).collect();
        active_parts.push((name, names));
    }
    active_parts.sort_by(|a, b| a.0.cmp(&b.0));

    let mut split = analysis_mir::Split { melody: vec![], harmony: vec![] };
    for (name, notes) in &measure_notes_by_part {
        let part_split = analysis_mir::split_roles(notes, name);
        split.melody.extend(part_split.melody);
        split.harmony.extend(part_split.harmony);
    }

    let mut prev_split = analysis_mir::Split { melody: vec![], harmony: vec![] };
    for (name, notes) in &prev_measure_notes_by_part {
        let part_split = analysis_mir::split_roles(notes, name);
        prev_split.melody.extend(part_split.melody);
        prev_split.harmony.extend(part_split.harmony);
    }

    all_pitches.sort_unstable();
    all_pitches.dedup();

    if all_pitches.is_empty() {
        all_pitches = window_pitches;
        all_pitches.sort_unstable();
        all_pitches.dedup();
    }

    let unique_pcs: BTreeSet<i32> = all_pitches.iter().map(|p| p.rem_euclid(12)).collect();

    let chord_obj = analysis_mir::identify(&all_pitches);

    let hist = analysis_mir::histogram(&project_notes);
    let (tonic, major) = analysis_mir::estimate_key(&hist);

    let roman = chord_obj.as_ref().map(|c| analysis_mir::roman(c, tonic));
    let motif = analysis_mir::find_motif(&split.melody);

    let contour_source: Vec<N> = if split.melody.is_empty() {
        measure_notes_by_part.values().flatten().cloned().collect()
    } else {
        split.melody.clone()
    };
    let contour = analysis_mir::melody_contour(&contour_source);

    let scale = if unique_pcs.is_empty() {
        "Empty".to_string()
    } else if unique_pcs.len() == 1 {
        format!("{} Drone (Monophonic)", midi_to_name(*all_pitches.first().unwrap()))
    } else {
        analysis_mir::scale_summary(&unique_pcs, tonic, major)
    };

    let average_pitch = if all_pitches.is_empty() { None } else {
        Some(all_pitches.iter().map(|&p| p as f64).sum::<f64>() / all_pitches.len() as f64)
    };

    let melody_mean = if split.melody.is_empty() { None } else {
        Some(split.melody.iter().map(|n| n.pitch as f64).sum::<f64>() / split.melody.len() as f64)
    };
    let prev_melody_mean = if prev_split.melody.is_empty() { None } else {
        Some(prev_split.melody.iter().map(|n| n.pitch as f64).sum::<f64>() / prev_split.melody.len() as f64)
    };

    let onsets_per_beat = total_measure_notes as f64 / rpmeasure;
    let prev_onsets_per_beat = if prev_measure_notes_by_part.is_empty() { None } else {
        Some(prev_measure_notes_by_part.values().map(|v| v.len()).sum::<usize>() as f64 / rpmeasure)
    };

    let mut has_bends = false;
    for call in project.line_calls(line_idx) {
        let subs = project.part_subs_per_beat(&call.name);
        let notes = split_notes(
            project.part_events(&call.name),
                                subs,
                                project.part_sample_offset_u32(&call.name)
        );
        for note in notes {
            if note.start_row <= current_row && current_row < note.end_row {
                if note.pitch_points.len() > 1 {
                    has_bends = true;
                    break;
                }
            }
        }
    }

    // --- CALCULATE MOTION ---
    let motion_feats = motion::calculate_motion(&measure_notes_by_part.values().flatten().cloned().collect::<Vec<_>>());

    let prev_motion_feats = motion::calculate_motion(&prev_measure_notes_by_part.values().flatten().cloned().collect::<Vec<_>>());

    let features = Features {
        chord: chord_obj.clone(),
        roman,
        contour: contour.clone(),
        melody_mean,
        prev_melody_mean,
        onsets_per_beat,
        prev_onsets_per_beat,
        has_motif: motif.is_some(),
        has_bends,
        unique_pc_count: unique_pcs.len(),
        active_pitches: all_pitches.len(),
        average_pitch,
        motion: motion_feats.clone(),
        prev_motion: prev_motion_feats,
    };

    let explanations = theory::explain_emotional(&features);

    let mut intervals = Vec::new();
    let mut chromatic_grid = "·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·".to_string();

    if !all_pitches.is_empty() {
        let mut grid = vec!['·'; 12];
        for &p in &all_pitches {
            grid[(p % 12) as usize] = '●';
        }
        chromatic_grid = grid.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("  ");

        let root_midi = all_pitches[0];
        let root_pc = root_midi % 12;
        for &p in &all_pitches {
            let diff = (p % 12 - root_pc + 12) % 12;
            let name = match diff {
                1 => "Minor 2nd", 2 => "Major 2nd", 3 => "Minor 3rd", 4 => "Major 3rd",
                5 => "Perfect 4th", 6 => "Tritone", 7 => "Perfect 5th", 8 => "Minor 6th",
                9 => "Major 6th", 10 => "Minor 7th", 11 => "Major 7th", _ => continue,
            };
            intervals.push(name.to_string());
        }
    }
    intervals.dedup();

    // Format the register for the UI
    let motion_register = if motion_feats.highest_pitch > 72 {
        "High".to_string()
    } else if motion_feats.lowest_pitch < 48 {
        "Low".to_string()
    } else {
        "Mid".to_string()
    };

    AnalysisReport {
        active_parts,
        chord: chord_obj.map_or("None".into(), |c| c.name),
        root: all_pitches.first().map_or("-".into(), |&p| midi_to_name(p)),
        intervals,
        chromatic_grid,
        scale,
        contour,
        motif,
        explanations,
        motion_anchor: motion_feats.is_pedal,
        motion_leap: motion_feats.biggest_leap,
        motion_register,
    }
}
