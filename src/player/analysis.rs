// src/player/analysis.rs
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use crate::mixer::mute;
use crate::player::{Project, Transport};
use crate::player::notes::{split_notes, value_at};
use crate::formats::halotext::hz_to_note;
use super::analysis_mir::{self, N, Features};
use super::motion;
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
    pub motion_anchor: bool,
    pub motion_leap: i32,
    pub motion_register: String,
    pub motion_highlight: Vec<(usize, i64)>,
}

/// Static per-line motion markers used by note rendering. Keep this separate
/// from the full theory report so ordinary tracker redraws don't rebuild a
/// 17-measure analysis every frame.
pub fn motion_highlights(project: &Project, line_idx: usize) -> Vec<(usize, i64)> {
    let t = std::time::Instant::now();
    let out = motion_highlights_inner(project, line_idx);
    crate::player::perf::record_motion(t.elapsed().as_nanos() as u64);
    out
}

fn motion_highlights_inner(project: &Project, line_idx: usize) -> Vec<(usize, i64)> {
    let snap = mute::snapshot();
    let mut by_part: std::collections::BTreeMap<String, Vec<N>> = std::collections::BTreeMap::new();
    for call in project.line_calls(line_idx) {
        let Some(path) = project.part_path(&call.name) else { continue };
        if snap.soloed_part.as_deref().is_some_and(|solo| solo != path)
            || snap.muted_parts.contains(path) { continue; }
        let subs = project.part_subs_per_beat(&call.name);
        let start = call.pattern_measure_start;
        let end = start + call.pattern_measure_len;
        for note in split_notes(project.part_events(&call.name), subs,
            project.part_sample_offset_u32(&call.name)) {
            if note.start_row < start || note.start_row >= end { continue; }
            by_part.entry(call.name.clone()).or_default().push(N {
                start: note.start_row - start,
                end: note.end_row - start,
                pitch: hz_to_note(value_at(&note.pitch_points, note.start_row).unwrap_or(440.0)),
                beats: (note.end_row - note.start_row) / subs.max(1) as f64,
            });
        }
    }
    let upper = by_part.iter()
        .filter(|(name, notes)| !is_rhythm_part(name) && notes.len() >= 3)
        .max_by(|a, b| mean_pitch(a.1).partial_cmp(&mean_pitch(b.1)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, notes)| analysis_mir::top_line(notes)).unwrap_or_default();
    let bass = by_part.iter()
        .filter(|(name, notes)| !is_rhythm_part(name) && notes.len() >= 2)
        .min_by(|a, b| mean_pitch(a.1).partial_cmp(&mean_pitch(b.1)).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    for pair in upper.windows(2) {
        if (pair[1].pitch - pair[0].pitch).abs() >= 7 {
            out.push((line_idx, pair[0].start as i64));
            out.push((line_idx, pair[1].start as i64));
        }
    }
    if let Some((_, notes)) = bass.filter(|(_, notes)| motion::calculate_motion(notes).is_pedal) {
        let lowest = notes.iter().map(|n| n.pitch).min().unwrap_or(0);
        out.extend(notes.iter().filter(|n| n.pitch <= lowest + 2).map(|n| (line_idx, n.start as i64)));
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn midi_to_name(midi: i32) -> String {
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = midi / 12;
    format!("{}{}", names[midi.rem_euclid(12) as usize], octave)
}


fn part_label(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name).split('_').next().unwrap_or(name)
}

fn is_rhythm_part(name: &str) -> bool {
    let n = part_label(name).to_ascii_lowercase();
    n.starts_with("dac") || n.starts_with("noise") || n.contains("drum")
        || n.contains("kick") || n.contains("snare") || n.contains("perc")
}

fn mean_pitch(notes: &[N]) -> f64 {
    if notes.is_empty() { return 0.0; }
    notes.iter().map(|n| n.pitch as f64).sum::<f64>() / notes.len() as f64
}

pub fn analyze(project: &Project, tp: &Transport, line_idx: usize) -> AnalysisReport {
    let t = std::time::Instant::now();
    let out = analyze_inner(project, tp, line_idx);
    crate::player::perf::record_analysis(t.elapsed().as_nanos() as u64);
    out
}

fn analyze_inner(project: &Project, tp: &Transport, line_idx: usize) -> AnalysisReport {
    let snap = mute::snapshot();
    let mut active_parts_map: std::collections::HashMap<String, Vec<i32>> = std::collections::HashMap::new();
    let mut all_pitches = Vec::new();
    let mut window_pitches = Vec::new();
    let mut project_notes = Vec::new();
    let mut measure_notes_by_part: std::collections::BTreeMap<String, Vec<N>> = std::collections::BTreeMap::new();
    let mut prev_measure_notes_by_part: std::collections::BTreeMap<String, Vec<N>> = std::collections::BTreeMap::new();
    let mut part_subs: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut slide_observations: Vec<(f64, String)> = Vec::new();
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
                project.part_sample_offset_u32(&call.name),
            );
            if k == 0 { part_subs.insert(call.name.clone(), subs.max(1) as f64); }

            for note in notes {
                if note.start_row < measure_start || note.start_row >= measure_end { continue; }

                let local_start = note.start_row - measure_start;
                let local_end = note.end_row - measure_start;
                let beats = (note.end_row - note.start_row) / subs as f64;
                let pitch = hz_to_note(value_at(&note.pitch_points, note.start_row).unwrap_or(440.0));
                let n_struct = N { start: local_start, end: local_end, pitch, beats };

                if k == 0 {
                    if let Some(base) = note.pitch_points.first().map(|p| p.value).filter(|v| *v > 0.0) {
                        let mut largest = 0.0_f64;
                        let mut bend = None;
                        for point in note.pitch_points.iter().skip(1) {
                            if point.value <= 0.0 { continue; }
                            let semitones = 12.0 * (point.value / base).log2();
                            if semitones.abs() > largest {
                                largest = semitones.abs();
                                bend = Some((point, semitones));
                            }
                        }
                        if let Some((point, semitones)) = bend.filter(|_| largest >= 0.35) {
                            let beat = (point.row - measure_start) / subs.max(1) as f64 + 1.0;
                            let ramp = point.ramp_rows / subs.max(1) as f64;
                            let direction = if semitones > 0.0 { "up" } else { "down" };
                            slide_observations.push((largest, format!(
                                "{} bends {} {:.1} semitones near beat {:.2} over {:.2} beats.",
                                part_label(&call.name), direction, largest, beat, ramp)));
                        }
                    }
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
                    if row_diff <= 1.0 { window_pitches.push(pitch); }
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

    // Use the highest-register pitched part as a clearly labelled upper line.
    // Halotext does not carry reliable lead/bass role metadata, so do not
    // pretend that a filename heuristic identifies the actual lead.
    let upper_part = measure_notes_by_part.iter()
        .filter(|(name, notes)| !is_rhythm_part(name) && notes.len() >= 3)
        .max_by(|a, b| mean_pitch(a.1).partial_cmp(&mean_pitch(b.1)).unwrap_or(std::cmp::Ordering::Equal));
    let upper_name = upper_part.map(|(name, _)| name.clone());
    let upper_source = upper_part.map(|(_, notes)| notes.as_slice()).unwrap_or(&[]);
    let upper_notes = analysis_mir::top_line(upper_source);
    let beat_scale = upper_name.as_ref().and_then(|name| part_subs.get(name).copied()).unwrap_or(1.0).max(1.0);
    let upper_notes_beats: Vec<N> = upper_notes.iter().map(|n| N {
        start: n.start / beat_scale, end: n.end / beat_scale, pitch: n.pitch, beats: n.beats,
    }).collect();
    let prev_upper_notes = upper_name.as_ref()
        .and_then(|name| prev_measure_notes_by_part.get(name))
        .map(|notes| analysis_mir::top_line(notes)).unwrap_or_default();

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
    let motif = analysis_mir::find_motif(&upper_notes_beats);

    let contour = if upper_notes.is_empty() {
        "No clear upper line (fewer than three pitched attacks)".to_string()
    } else {
        format!("{}: {}", upper_name.as_deref().map(part_label).unwrap_or("upper part"),
            analysis_mir::melody_contour(&upper_notes_beats))
    };
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
    let melody_mean = if upper_notes.is_empty() { None } else { Some(mean_pitch(&upper_notes)) };
    let prev_melody_mean = if prev_upper_notes.is_empty() { None } else { Some(mean_pitch(&prev_upper_notes)) };

    let onsets_per_beat = total_measure_notes as f64 / rpmeasure;
    let prev_onsets_per_beat = if prev_measure_notes_by_part.is_empty() { None } else {
        Some(prev_measure_notes_by_part.values().map(|v| v.len()).sum::<usize>() as f64 / rpmeasure)
    };

    let has_bends = !slide_observations.is_empty();

    let mut motion_feats = motion::calculate_motion(&upper_notes);
    let bass_part = measure_notes_by_part.iter()
        .filter(|(name, notes)| !is_rhythm_part(name) && notes.len() >= 2)
        .min_by(|a, b| mean_pitch(a.1).partial_cmp(&mean_pitch(b.1)).unwrap_or(std::cmp::Ordering::Equal));
    if let Some((_, bass_notes)) = bass_part { motion_feats.is_pedal = motion::calculate_motion(bass_notes).is_pedal; }
    let prev_motion_feats = upper_name.as_ref()
        .and_then(|name| prev_measure_notes_by_part.get(name))
        .map(|notes| motion::calculate_motion(notes)).unwrap_or_default();

    // Mark leaps on the selected upper line only; combining simultaneous
    // pitches from different parts creates false octave jumps.
    let mut motion_highlight: Vec<(usize, i64)> = Vec::new();
    let mut ordered_upper = upper_notes.clone();
    ordered_upper.sort_by(|a, b| a.start.partial_cmp(&b.start)
        .unwrap_or(std::cmp::Ordering::Equal).then(a.pitch.cmp(&b.pitch)));
    for pair in ordered_upper.windows(2) {
        if (pair[1].pitch - pair[0].pitch).abs() >= 7 {
            motion_highlight.push((line_idx, pair[0].start as i64));
            motion_highlight.push((line_idx, pair[1].start as i64));
        }
    }
    if motion_feats.is_pedal {
        if let Some((_, bass_notes)) = bass_part {
            let lowest = bass_notes.iter().map(|n| n.pitch).min().unwrap_or(0);
            for n in bass_notes.iter().filter(|n| n.pitch <= lowest + 2) {
                motion_highlight.push((line_idx, n.start as i64));
            }
        }
    }
    motion_highlight.sort_unstable();
    motion_highlight.dedup();

    let features = Features {
        chord: chord_obj.clone(), roman, contour: contour.clone(),
        melody_mean, prev_melody_mean, onsets_per_beat, prev_onsets_per_beat,
        has_motif: motif.is_some(), has_bends, unique_pc_count: unique_pcs.len(),
        active_pitches: all_pitches.len(), average_pitch,
        motion: motion_feats.clone(), prev_motion: prev_motion_feats,
        outside_scale_notes: unique_pcs.iter().filter(|&&pc| {
            let scale: &[i32] = if major { &[0, 2, 4, 5, 7, 9, 11] } else { &[0, 2, 3, 5, 7, 8, 10] };
            !scale.contains(&(pc - tonic).rem_euclid(12))
        }).map(|&pc| analysis_mir::NAMES[pc as usize].to_string()).collect(),
    };

    let mut explanations = Vec::new();
    if let Some(motif) = &motif {
        explanations.push(format!("{}: {}.", upper_name.as_deref().map(part_label).unwrap_or("Upper line"), motif));
    }
    if let Some((_, slide)) = slide_observations.iter().max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)) {
        explanations.push(slide.clone());
    }
    if upper_part.is_some() {
        let upper_onsets: Vec<f64> = upper_notes_beats.iter().map(|n| n.start).collect();
        let mut drum_onsets: Vec<f64> = measure_notes_by_part.iter()
            .filter(|(part, _)| is_rhythm_part(part))
            .flat_map(|(part, notes)| {
                let subs = part_subs.get(part).copied().unwrap_or(1.0);
                notes.iter().map(move |n| n.start / subs)
            }).collect();
        drum_onsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        drum_onsets.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        if upper_onsets.len() >= 3 && drum_onsets.len() >= 2 {
            let aligned = upper_onsets.iter().filter(|&&beat| drum_onsets.iter().any(|d| (d - beat).abs() <= 0.125)).count();
            explanations.push(format!("Percussion aligns with {}/{} upper-line attacks within 1/8 beat.",
                aligned, upper_onsets.len()));
        }
    }
    explanations.extend(theory::explain_observations(&features));
    explanations.truncate(4);
    if explanations.is_empty() { explanations.push("No clear repeated figure, pitch bend, or large upper-line leap in this measure.".into()); }
    let mut intervals = Vec::new();
    let mut chromatic_grid = "·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·".to_string();

    if !all_pitches.is_empty() {
        let mut grid = vec!['·'; 12];
        for &p in &all_pitches { grid[p.rem_euclid(12) as usize] = '●'; }
        chromatic_grid = grid.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("  ");
        let root_pc = chord_obj.as_ref().map_or_else(
            || all_pitches[0].rem_euclid(12), |c| c.root_pc.rem_euclid(12));
        let mut seen_intervals = BTreeSet::new();
        for &p in &all_pitches {
            let diff = (p.rem_euclid(12) - root_pc).rem_euclid(12);
            let name = match diff {
                1 => "Minor 2nd", 2 => "Major 2nd", 3 => "Minor 3rd", 4 => "Major 3rd",
                5 => "Perfect 4th", 6 => "Tritone", 7 => "Perfect 5th", 8 => "Minor 6th",
                9 => "Major 6th", 10 => "Minor 7th", 11 => "Major 7th", _ => continue,
            };
            if seen_intervals.insert(diff) { intervals.push(name.to_string()); }
        }
    }

    let motion_register = if motion_feats.highest_pitch > 72 {
        "High".to_string()
    } else if motion_feats.lowest_pitch < 48 {
        "Low".to_string()
    } else {
        "Mid".to_string()
    };

    AnalysisReport {
        active_parts, chord: chord_obj.as_ref().map_or("None".into(), |c| c.name.clone()),
        root: chord_obj.as_ref().map_or_else(
            || all_pitches.first().map_or("-".into(), |&p| midi_to_name(p)),
            |c| analysis_mir::NAMES[c.root_pc.rem_euclid(12) as usize].to_string()),
        intervals, chromatic_grid, scale, contour, motif, explanations,
        motion_anchor: motion_feats.is_pedal, motion_leap: motion_feats.biggest_leap,
        motion_register, motion_highlight,
    }
}