//! Song assembly: measure placement, part creation, output string.

use crate::spec;
use super::super::song::{Call, MeasureLine, Song};
use super::super::{fmt, note_name, pos_str};
use super::{
    BEATS_PER_MEASURE, BPM, CH_DAC, LABELS, N_CH, SOURCE,
};
use super::analyze::Analysis;
use super::regs::TimbreSnapshot;
use super::tracker::{PitchPoint, VolumePoint};
use super::timing::Timing;

fn new_song(title: &str, source_name: &str, subs: u32, bpm: f64, beats: usize) -> Song {
    let sig = if beats == 3 { (3, 4) } else { (4, 4) };
    Song::new(title, source_name, "vgm2trackscript", bpm, subs, sig, 100.0, "equal_power", "names")
}

fn pitch_text(pitch: f64) -> String {
    let nearest = pitch.round();
    if (pitch - nearest).abs() < 0.025 {
        // VGM semitones use MIDI numbering (C4=60), while Trackscript
        // names map C5 to 60. Shift the displayed octave down so parsing
        // the name preserves the original frequency.
        return note_name(nearest as i32 - 12);
    }
    let hz = 440.0 * 2f64.powf((pitch - 69.0) / 12.0);
    format!("{:.3}", hz)
}

fn note_piece(
    n_pitch: f64,
    points: &[PitchPoint],
    n_vol: f64,
    vol_points: &[VolumePoint],
    measure: usize,
    start_beat: f64,
    end_beat: f64,
    measure_beats: f64,
    tm: &Timing,
    subs: u32,
    pos: &str,
    dur: &str,
) -> String {
    let mut pitch = n_pitch;
    for p in points.iter().filter(|p| tm.beats_at(p.sample) <= start_beat) { pitch = p.pitch; }
    let mut volume = n_vol;
    for p in vol_points.iter().filter(|p| tm.beats_at(p.sample) <= start_beat) { volume = p.volume; }
    let mut text = spec::note(pos, &pitch_text(pitch), dur, &fmt(volume.clamp(0.0, 100.0), 1), "").to_string();
    let mut changes: Vec<(f64, Option<f64>, Option<f64>)> = Vec::new();
    for p in points {
        let at = tm.beats_at(p.sample);
        if at > start_beat && at < end_beat { changes.push((at, Some(p.pitch), None)); }
    }
    for p in vol_points {
        let at = tm.beats_at(p.sample);
        if at > start_beat && at < end_beat { changes.push((at, None, Some(p.volume))); }
    }
    changes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut grouped: Vec<(f64, Option<f64>, Option<f64>)> = Vec::new();
    for (at, p, v) in changes {
        if let Some(last) = grouped.last_mut().filter(|x| (x.0 - at).abs() < 1e-8) {
            if p.is_some() { last.1 = p; }
            if v.is_some() { last.2 = v; }
        } else { grouped.push((at, p, v)); }
    }
    for (i, (at, target, target_vol)) in grouped.iter().enumerate() {
        let next = grouped.get(i + 1).map(|x| x.0).unwrap_or(end_beat);
        let ramp = (next - *at).max(0.0);
        let local_row = (*at - measure as f64 * measure_beats).max(0.0) * subs as f64;
        text.push_str(&format!("&<{}", pos_str(local_row, subs)));
        if let Some(target) = target { text.push_str(&format!(",#{}", pitch_text(*target))); }
        if let Some(target_vol) = target_vol { text.push_str(&format!(",%{}", fmt(target_vol.clamp(0.0, 100.0), 1))); }
        if ramp > 0.0005 { text.push_str(&format!(",~{}", fmt(ramp, 3))); }
        text.push('>');
    }
    text
}

pub(crate) fn build_trackscript(a: &Analysis, title: &str, source_name: &str, subs: u32, tm: &Timing) -> String {
    let bpmeas = tm.beats as f64;
    let measures = ((tm.beats_at(a.total) / bpmeas).ceil() as usize).max(1);
    let mut song = new_song(title, source_name, subs, tm.bpm, tm.beats);

    use std::collections::HashMap;
    let mut timbres: HashMap<(usize, u64), TimbreSnapshot> = HashMap::new();
    for ch in 0..N_CH {
        for n in &a.notes[ch] {
            timbres.entry((ch, n.timbre_hash)).or_insert_with(|| n.timbre.clone());
        }
    }
    let mut timbre_name: HashMap<(usize, u64), String> = HashMap::new();
    for ((ch, hash), _) in &timbres {
        timbre_name.insert((*ch, *hash), format!("{}_{:08x}", LABELS[*ch], (*hash as u32)));
    }
    let mut order: Vec<(usize, u64)> = timbres.keys().cloned().collect();
    order.sort();

    let mut by: HashMap<(usize, u64), Vec<Vec<(f64, String)>>> = HashMap::new();
    for key in &order {
        by.insert(*key, vec![Vec::new(); measures]);
    }
    for ch in 0..N_CH {
        for n in &a.notes[ch] {
            let key = (ch, n.timbre_hash);
            let Some(bucket) = by.get_mut(&key) else { continue };
            let mut sb = tm.start_beats(n.start).max(0.0);
            let eb = tm.end_beats(n.end).max(sb);
            while sb < eb - 1e-6 {
                let m = ((sb / bpmeas).floor().max(0.0) as usize).min(measures - 1);
                let barline = (m as f64 + 1.0) * bpmeas;
                let clipped = m < measures - 1 && eb > barline && eb - barline < 0.05;
                let piece_end = if m == measures - 1 { eb } else { eb.min(barline) };
                let local = (sb - m as f64 * bpmeas).max(0.0);
                let pos = pos_str(local * subs as f64, subs);
                let dur = fmt((piece_end - sb).max(0.001), 3);
                let text = note_piece(
                    n.pitch, &n.pitch_points, n.vol, &n.vol_points, m, sb, piece_end,
                    bpmeas, tm, subs, &pos, &dur,
                );
                bucket[m].push((sb, text));
                if clipped || piece_end <= sb { break; }
                sb = piece_end;
            }
        }
    }

    let mut part_idx: HashMap<(usize, u64), usize> = HashMap::new();
    for key in &order {
        let name = &timbre_name[key];
        let src = format!("banks/{}/{}.patch", title, name);
        let pi = song.get_or_create_part(name, spec::source_arg(&src, None));
        part_idx.insert(*key, pi);
    }

    for mi in 0..measures {
        let mut line = MeasureLine { calls: Vec::new(), extra: Vec::new() };
        for key in &order {
            let Some(pi) = part_idx.get(key).copied() else { continue };
            let evs = &mut by.get_mut(key).unwrap()[mi];
            if evs.is_empty() { continue; }
            evs.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            let body = evs.iter().map(|e| e.1.as_str()).collect::<Vec<_>>().join(",");
            let section_idx = song.add_section(pi, body);
            line.calls.push(Call { part_idx: pi, section_idx, overrides: None });
        }
        song.measures.push(line);
    }

    song.post_comments.push("// ------------------------------------------------------------".to_string());
    song.post_comments.push(format!("// {}", title));
    song.post_comments.push("// converted from VGM by vgm2trackscript".to_string());
    song.post_comments.push(format!("// source: {source_name}"));
    song.post_comments.push(format!("// {} measure(s); {} timbre part(s)", measures, order.len()));
    song.post_comments.push(format!(
        "// tempo {:.3} BPM, {}/4, grid {:.2} samples/row{}",
        tm.bpm, tm.beats, tm.tick, if tm.snap { " (detected)" } else { " (fallback 120 BPM)" }
    ));
    let _ = CH_DAC;
    song.write()
}

pub(crate) fn shadow_trackscript(total_seconds: f64, title: &str, source_name: &str, subs: u32) -> String {
    let beats = (total_seconds * BPM / 60.0).ceil().max(1.0);
    let measures = ((beats / BEATS_PER_MEASURE).ceil() as usize).max(1);
    let mut song = new_song(title, source_name, subs, BPM, BEATS_PER_MEASURE as usize);

    let part_idx = song.get_or_create_part("VGMChip", spec::source_arg(SOURCE, None));
    let body = spec::note(&pos_str(0.0, subs), "C5", "4", "100", "").to_string();
    let section_idx = song.add_section(part_idx, body);
    for _ in 0..measures {
        let mut line = MeasureLine { calls: Vec::new(), extra: Vec::new() };
        line.calls.push(Call { part_idx, section_idx, overrides: None });
        song.measures.push(line);
    }

    song.post_comments.push(format!("// {}", title));
    song.post_comments.push("// converted from VGM by vgm2trackscript (no notes extracted)".to_string());
    song.post_comments.push(format!("// source: {source_name}"));
    song.write()
}
