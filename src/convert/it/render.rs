// src/convert/it/render.rs
//! Voices → per-measure sections, and the shared `Song` builder.

use std::collections::HashMap;

use super::helpers::*;
use super::sim::{Converter, Voice};
use crate::convert::song::{Call, MeasureLine, Song};
use crate::convert::PitchMode;
use crate::sources::it::Module;

// ── position / pitch / pan text ────────────────────────────────────────

pub(super) fn pos_str(r: f64) -> String {
    let mut b = (r / RPB).floor() as i64 + 1;

    let s = pyround(r - RPB * (b - 1) as f64 + 1.0, 2);

    if (s - round_even(s)).abs() < 1e-9 {
        let mut si = round_even(s) as i64;

        if si > ROWS_PER_BEAT as i64 {
            b += 1;
            si = 1;
        }

        return format!("§{}/{}", b, si);
    }

    format!("§{}/{}", b, fmt(s, 2))
}

/// Note name when the pitch is exactly a note (within 2 cents), otherwise Hz.
pub(super) fn pitch_text(hz_value: f64, mode: PitchMode) -> String {
    if matches!(mode, PitchMode::Names) {
        let semis = 60.0 + 12.0 * (hz_value / C5_HZ).log2();
        let n = round_even(semis);
        if (semis - n).abs() < 0.02 {
            return note_name(n as i64);
        }
    }
    fmt(hz_value, 2)
}

pub(super) fn pan_str(p: f64) -> String {
    fmt(p.clamp(-1.0, 1.0), 2)
}

// ── structural helpers ─────────────────────────────────────────────────

/// One pattern span per measure.
pub(super) fn build_safe_spans(total_rows: f64) -> Vec<(f64, f64)> {
    if total_rows <= 0.0 {
        return Vec::new();
    }
    let total = total_rows as usize;
    let step = MEASURE_ROWS as usize;
    (0..total)
        .step_by(step)
        .map(|s| (s as f64, (s as f64 + MEASURE_ROWS).min(total_rows)))
        .collect()
}

pub(super) fn sample_base_name(module: &Module, sample_index: u32) -> String {
    let s = &module.samples[sample_index as usize - 1];

    if !s.name.is_empty() {
        return identifier(&s.name);
    }

    format!("Sample{:02}", sample_index)
}

/// Tempo events as (row, bpm, ramp_rows).
pub(super) fn tempo_lane(conv: &Converter) -> Vec<(f64, f64, f64)> {
    let b: Vec<f64> = conv.rowlog.iter().map(|r| r.4).collect();
    if b.is_empty() {
        return Vec::new();
    }

    let mut wps: Vec<(f64, f64, f64)> = Vec::new();
    let mut slide_rows: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (row, tick_bpms) in &conv.tempo_ramps {
        let n = tick_bpms.len();
        for k in 1..n {
            if (tick_bpms[k] - tick_bpms[k - 1]).abs() > 1e-9 {
                wps.push((row + k as f64 / n as f64, tick_bpms[k], 0.0));
            }
        }
        let end_row = *row as usize + 1;
        if end_row < b.len() && (b[end_row] - pyround(tick_bpms[n - 1], 2)).abs() < 0.01 {
            slide_rows.insert(end_row);
        }
    }

    for row in 1..b.len() {
        if !slide_rows.contains(&row) && b[row] != b[row - 1] {
            wps.push((row as f64, b[row], 0.0));
        }
    }

    wps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    wps
}

/// Global BPM runs as (start_row, end_row, bpm).  Kept for the report.
pub(super) fn build_sections(conv: &Converter) -> Vec<(f64, f64, f64)> {
    let mut runs: Vec<(f64, f64, f64)> = Vec::new();

    for &(t, _oi, _pat, _row, bpm) in &conv.rowlog {
        match runs.last_mut() {
            Some(last) if last.2 == bpm => last.1 = t + 1.0,
            _ => runs.push((t, t + 1.0, bpm)),
        }
    }

    runs
}

// ── rendering ──────────────────────────────────────────────────────────

impl<'a> Converter<'a> {
    /// Voices starting inside the span, grouped by source sample.
    pub(super) fn group_span_voices(
        &self,
        start: f64,
        end: f64,
    ) -> std::collections::BTreeMap<u32, Vec<usize>> {
        let mut groups: std::collections::BTreeMap<u32, Vec<usize>> =
            std::collections::BTreeMap::new();

        for (vi, v) in self.voices.iter().enumerate() {
            if start <= v.start && v.start < end {
                groups.entry(v.sample).or_default().push(vi);
            }
        }

        for voices in groups.values_mut() {
            voices.sort_by(|&a, &b| {
                let (va, vb) = (&self.voices[a], &self.voices[b]);
                va.start
                    .partial_cmp(&vb.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        va.end
                            .unwrap_or(1e18)
                            .partial_cmp(&vb.end.unwrap_or(1e18))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
        }

        groups
    }

    pub(super) fn render_voice_event(
        &self,
        v: &Voice,
        local_start: f64,
        sample_files: &HashMap<u32, String>,
    ) -> Option<String> {
        let mode = self.pitch_mode;
        let f = sample_files.get(&v.sample)?;
        if f.is_empty() {
            return None;
        }

        let dur_rows = (v.end.unwrap_or(v.start) - v.start).max(0.05);
        let dur_beats = dur_rows / RPB;

        let mut fields = vec![
            pos_str(v.start - local_start),
            format!("#{}", pitch_text(v.init["pitch"], mode)),
            format!("~{}", fmt(dur_beats, 2)),
        ];

        if (v.init["vol"] - 100.0).abs() > 1e-9 {
            fields.push(format!("%{}", fmt(v.init["vol"], 1)));
        }

        if v.offset != 0 {
            fields.push(format!("ƒ'{}'({})", f, v.offset));
        }

        if v.init["pan"].abs() > 1e-9 {
            fields.push(format!("¶{}", pan_str(v.init["pan"])));
        }

        let ev = format!("<{}>", fields.join(","));

        // Group waypoints sharing (position, duration), in first-seen order.
        let mut order: Vec<(i64, i64)> = Vec::new();
        let mut groups: HashMap<(i64, i64), Vec<(&'static str, f64)>> = HashMap::new();

        for &(pt, prop, val, d) in &v.points {
            let key = (key3(pt), key3(d));
            groups
                .entry(key)
                .or_insert_with(|| {
                    order.push(key);
                    Vec::new()
                })
                .push((prop, val));
        }

        order.sort_by_key(|k| k.0);

        let mut transitions = String::new();

        for key in order {
            let pt = key.0 as f64 / 1000.0;
            let d = key.1 as f64 / 1000.0;

            let (mut pitch, mut vol, mut pan) = (None, None, None);
            for &(prop, val) in &groups[&key] {
                match prop {
                    "pitch" => pitch = Some(val),
                    "vol" => vol = Some(val),
                    "pan" => pan = Some(val),
                    _ => {}
                }
            }

            if pt < local_start {
                continue;
            }

            let mut fields = vec![pos_str(pt - local_start)];

            if let Some(p) = pitch {
                fields.push(format!("#{}", pitch_text(p, mode)));
            }
            if let Some(vv) = vol {
                fields.push(format!("%{}", fmt(vv, 1)));
            }
            if let Some(pp) = pan {
                fields.push(format!("¶{}", pan_str(pp)));
            }
            if d > 1e-9 {
                fields.push(format!("~{}", fmt(d / RPB, 2)));
            }

            transitions.push_str(&format!("&<{}>", fields.join(",")));
        }

        Some(ev + &transitions)
    }

    /// Render a part independently of its original channel.
    pub(super) fn render_part(
        &mut self,
        voices: &[usize],
        start: f64,
        sample_files: &HashMap<u32, String>,
    ) -> String {
        let mut events: Vec<(i64, String)> = Vec::new();
        let mut missing = 0;

        for &vi in voices {
            let v = &self.voices[vi];
            match self.render_voice_event(v, start, sample_files) {
                Some(ev) => {
                    let rel = key3(v.start - start);
                    events.push((rel, ev));
                }
                None => missing += 1,
            }
        }
        for _ in 0..missing {
            self.note_warn("voice references missing sample output");
        }

        events.sort_by_key(|e| e.0);

        let all: Vec<String> = events.into_iter().map(|e| e.1).collect();
        all.join(",")
    }

    /// Populate a shared `Song` with one part per sample, one section
    /// per unique measure.
    pub(super) fn extract_structure(
        &mut self,
        song: &mut Song,
        sample_files: &HashMap<u32, String>,
    ) -> HashMap<u32, usize> {
        let spans = build_safe_spans(self.total_rows);

        let mut first_start: HashMap<u32, f64> = HashMap::new();
        for v in &self.voices {
            first_start
                .entry(v.sample)
                .and_modify(|e| *e = e.min(v.start))
                .or_insert(v.start);
        }

        let mut part_for_sample: HashMap<u32, usize> = HashMap::new();
        let mut part_name_used: HashMap<String, u32> = HashMap::new();

        for (start, end) in spans {
            let groups = self.group_span_voices(start, end);

            let mut ordered: Vec<(u32, Vec<usize>)> = groups.into_iter().collect();
            ordered.sort_by(|a, b| {
                let ra = first_start.get(&a.0).copied().unwrap_or(f64::INFINITY);
                let rb = first_start.get(&b.0).copied().unwrap_or(f64::INFINITY);
                ra.partial_cmp(&rb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });

            let mut line = MeasureLine { calls: Vec::new(), extra: Vec::new() };

            for (sample_index, voices) in ordered {
                let body = self.render_part(&voices, start, sample_files);

                let part_idx = match part_for_sample.get(&sample_index) {
                    Some(&idx) => idx,
                    None => {
                        let base = sample_base_name(self.m, sample_index);
                        let count = {
                            let e = part_name_used.entry(base.clone()).or_insert(0);
                            *e += 1;
                            *e
                        };
                        let name = if count == 1 {
                            base
                        } else {
                            format!("{}{:02}", base, count)
                        };
                        let file = sample_files
                            .get(&sample_index)
                            .map(String::as_str)
                            .unwrap_or("");
                        let source_arg = crate::spec::source_arg(file, None);
                        let idx = song.get_or_create_part(&name, source_arg);
                        part_for_sample.insert(sample_index, idx);
                        idx
                    }
                };

                let section_idx = song.add_section(part_idx, body);
                line.calls.push(Call {
                    part_idx,
                    section_idx,
                    overrides: None,
                });
            }

            song.measures.push(line);
        }

        part_for_sample
    }
}

// ── Song builder ───────────────────────────────────────────────────────

pub(super) fn build_song(
    conv: &mut Converter,
    base: &str,
    sample_files: &HashMap<u32, String>,
    source_name: &str,
    with_message: bool,
) -> Song {
    let m = conv.m;
    let runs = build_sections(conv);
    let first_bpm = runs.first().map_or(120.0, |r| r.2);

    let title = if m.title.is_empty() { base } else { &m.title };

    let mut song = Song::new(
        title,
        source_name,
        "it2trackscript",
        first_bpm,
        ROWS_PER_BEAT,
        (4, 4),
        m.mv as f64 * 100.0 / 512.0,
        "linear",
        "hz",
    );

    conv.extract_structure(&mut song, sample_files);

    // ── tempo shims ───────────────────────────────────────────────
    let wps = tempo_lane(conv);
    song.measure_line(0)
        .extra
        .push(crate::spec::tempo_shim("§1/1", &fmt(first_bpm, 2)));

    for &(row, bpm, _ramp) in &wps {
        let measure = (row / MEASURE_ROWS).floor() as usize;
        let local = row - measure as f64 * MEASURE_ROWS;
        if measure == 0 && local < 1e-9 {
            continue;
        }
        song.measure_line(measure)
            .extra
            .push(crate::spec::tempo_shim(&pos_str(local), &fmt(bpm, 2)));
    }

    // ── post-comments ─────────────────────────────────────────────
    song.post_comments
        .push("// ------------------------------------------------------------".to_string());
    song.post_comments.push(format!("// {}", title));
    song.post_comments.push(format!(
        "// Converted from {} by it2trackscript",
        source_name
    ));
    song.post_comments.push("//".to_string());
    song.post_comments.push(
        "// The generated structure is based on reusable musical parts rather than IT channels."
            .to_string(),
    );

    if !wps.is_empty() {
        song.post_comments.push(String::new());
        song.post_comments.push(
            "// Tempo: one tempo line per measure; § positions are local to that line."
                .to_string(),
        );
    }

    if with_message && !m.message.trim().is_empty() {
        song.post_comments.push(String::new());
        song.post_comments
            .push("// ---- original song message ----".to_string());
        for line in m.message.split('\n') {
            song.post_comments.push(format!("// {}", line.trim_end()));
        }
        song.post_comments
            .push("// ---- end song message ----".to_string());
    }

    if !sample_files.is_empty() {
        song.post_comments.push(String::new());
        song.post_comments.push(
            "// Samples (WAV files are written at each sample's C5Speed):".to_string(),
        );
        for (idx, s) in m.samples.iter().enumerate() {
            let idx = idx as u32 + 1;
            if let Some(file) = sample_files.get(&idx) {
                let lp = if s.r#loop {
                    format!(", loop {}-{}", s.loop_beg, s.loop_end)
                } else {
                    String::new()
                };
                song.post_comments.push(format!(
                    "//   {:02}  {}  (C5Speed {}{})",
                    idx, file, s.c5, lp
                ));
            }
        }
    }

    if let Some((oi, row, _)) = conv.loop_info {
        song.post_comments.push(String::new());
        song.post_comments.push(format!(
            "// Original song loops back to order {:02} row {}",
            oi, row
        ));
    }

    song
}