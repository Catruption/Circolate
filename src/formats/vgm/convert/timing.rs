//! Musical grid detection from note starts.

use super::{
    BPM, BEATS_PER_MEASURE, VGM_RATE,
    MIN_ONSET_NOTE, TICK_MIN, TICK_MAX, MIN_COHERENCE,
    SNAP_START, SNAP_END, CH_DAC, N_CH, LABELS,
};
use super::analyze::Analysis;

pub(crate) struct Timing {
    pub(crate) tick: f64,
    pub(crate) rpb: f64,
    pub(crate) grid_off: f64,
    pub(crate) bpm: f64,
    pub(crate) beats: usize,
    pub(crate) snap: bool,
}

impl Timing {
    fn fallback(subs: u32) -> Self {
        Timing {
            tick: VGM_RATE * 60.0 / (BPM * subs as f64),
            rpb: subs as f64,
            grid_off: 0.0,
            bpm: BPM,
            beats: BEATS_PER_MEASURE as usize,
            snap: false,
        }
    }

    pub(crate) fn beats_at(&self, t: u32) -> f64 {
        (t as f64 - self.grid_off) / self.tick / self.rpb
    }

    fn snapped(&self, t: u32, tol: f64) -> f64 {
        let rows = (t as f64 - self.grid_off) / self.tick;
        if self.snap {
            let r = rows.round();
            if (rows - r).abs() <= tol { return r / self.rpb; }
        }
        rows / self.rpb
    }

    pub(crate) fn start_beats(&self, t: u32) -> f64 { self.snapped(t, SNAP_START) }
    pub(crate) fn end_beats(&self, t: u32) -> f64 { self.snapped(t, SNAP_END) }
}

fn coherence(sample: &[f64], tick: f64) -> f64 {
    let (mut c, mut s) = (0.0f64, 0.0f64);
    for &t in sample {
        let (sn, cs) = (std::f64::consts::TAU * t / tick).sin_cos();
        c += cs;
        s += sn;
    }
    c.hypot(s) / sample.len() as f64
}

fn find_tick(starts: &[f64]) -> Option<(f64, f64)> {
    let stride = (starts.len() / 800).max(1);
    let sample: Vec<f64> = starts.iter().step_by(stride).copied().collect();
    let co: Vec<f64> = (TICK_MIN..=TICK_MAX).map(|t| coherence(&sample, t as f64)).collect();
    let mx = co.iter().cloned().fold(0.0, f64::max);
    if mx < MIN_COHERENCE { return None; }
    let thr = 0.9 * mx;
    let mut best: Option<usize> = None;
    for i in 1..co.len() - 1 {
        if co[i] >= thr && co[i] >= co[i - 1] && co[i] >= co[i + 1] { best = Some(i); }
    }
    let mut tick = (TICK_MIN + best?) as f64;

    let (mut cs, mut sn) = (0.0f64, 0.0f64);
    for &t in starts {
        let (s, c) = (std::f64::consts::TAU * t / tick).sin_cos();
        cs += c;
        sn += s;
    }
    let mut off = (sn.atan2(cs) / std::f64::consts::TAU * tick).rem_euclid(tick);
    for _ in 0..4 {
        let (mut n, mut sk, mut sx, mut skk, mut skx) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for &x in starts {
            let kf = (x - off) / tick;
            let k = kf.round();
            if (kf - k).abs() < 0.25 {
                n += 1.0;
                sk += k;
                sx += x;
                skk += k * k;
                skx += k * x;
            }
        }
        let den = n * skk - sk * sk;
        if n < 8.0 || den.abs() < 1e-9 { break; }
        tick = (n * skx - sk * sx) / den;
        off = (sx - tick * sk) / n;
    }
    if !tick.is_finite() || tick < TICK_MIN as f64 / 2.0 { return None; }
    Some((tick, off))
}

pub(crate) fn detect_timing(a: &Analysis, subs: u32) -> Timing {
    let mut tm = Timing::fallback(subs);

    let mut starts: Vec<f64> = Vec::new();
    for ch in 0..N_CH {
        if ch == CH_DAC { continue; }
        for n in &a.notes[ch] {
            if n.end.saturating_sub(n.start) >= MIN_ONSET_NOTE {
                starts.push(n.start as f64);
            }
        }
    }
    starts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));

    if starts.len() >= 24 {
        if let Some((tick, off)) = find_tick(&starts) {
            let mut rpb = subs.max(1) as f64;
            let mut bpm = VGM_RATE * 60.0 / (tick * rpb);
            while bpm > 200.0 { bpm /= 2.0; rpb *= 2.0; }
            while bpm < 60.0 && rpb >= 2.0 { bpm *= 2.0; rpb /= 2.0; }
            bpm = (bpm * 1000.0).round() / 1000.0;
            let tick = VGM_RATE * 60.0 / (bpm * rpb);

            let mut ph = off.rem_euclid(tick);
            if ph > tick / 2.0 { ph -= tick; }
            let grid_off = if ph.abs() <= 0.10 * tick { 0.0 } else { ph };

            let rpb_i = rpb.round().max(1.0) as usize;
            let mut counts: Vec<f64> = Vec::new();
            for &x in &starts {
                let r = ((x - grid_off) / tick).round().max(0.0) as usize;
                if r >= counts.len() { counts.resize(r + 1, 0.0); }
                counts[r] += 1.0;
            }
            let n = counts.len();
            let mean = counts.iter().sum::<f64>() / n.max(1) as f64;
            let var: f64 = counts.iter().map(|c| (c - mean) * (c - mean)).sum();
            let ac = |lag: usize| -> f64 {
                if lag == 0 || n <= lag * 4 || var <= 0.0 { return 0.0; }
                (0..n - lag).map(|i| (counts[i] - mean) * (counts[i + lag] - mean)).sum::<f64>() / var
            };
            let (ac3, ac4) = (ac(3 * rpb_i), ac(4 * rpb_i));
            let beats = if ac3 > ac4 + 0.10 { 3 } else { 4 };

            tm = Timing { tick, rpb, grid_off, bpm, beats, snap: true };
        }
    }

    if let Some(b) = std::env::var("CIRCOLATE_VGM_BPM").ok().and_then(|s| s.trim().parse::<f64>().ok()) {
        if (20.0..=400.0).contains(&b) {
            tm.bpm = b;
            tm.rpb = (VGM_RATE * 60.0 / b) / tm.tick;
        }
    }
    if let Some(b) = std::env::var("CIRCOLATE_VGM_BEATS").ok().and_then(|s| s.trim().parse::<usize>().ok()) {
        if b == 3 || b == 4 { tm.beats = b; }
    }
    tm
}

fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Scores the grid and snapping decisions against the original note times
/// (no audio, no chip). Appended to `vgm_report.txt`.
pub(crate) fn grid_report(a: &Analysis, tm: &Timing, subs: u32) -> String {
    use std::fmt::Write as _;
    let ms = |samples: f64| samples * 1000.0 / VGM_RATE;
    let back = |beats: f64| beats * tm.rpb * tm.tick + tm.grid_off;

    let mut r = String::new();
    let _ = writeln!(r, "\nGrid and snapping");
    let _ = writeln!(r, "  tempo {:.3} BPM, {}/4, {} rows/beat in file, detector rows/beat {:.3}",
                     tm.bpm, tm.beats, subs, tm.rpb);
    let _ = writeln!(r, "  1 grid row = {:.2} samples = {:.2} ms, grid offset {:.1} samples, snapping {}",
                     tm.tick, ms(tm.tick), tm.grid_off, if tm.snap { "ON (detected)" } else { "OFF (fallback 120 BPM)" });
    let _ = writeln!(r, "  snap tolerance: start +/-{:.0}% of a row (= up to {:.1} ms), end +/-{:.0}% (= up to {:.1} ms)",
                     SNAP_START * 100.0, ms(tm.tick * SNAP_START), SNAP_END * 100.0, ms(tm.tick * SNAP_END));

    let mut onsets: Vec<f64> = Vec::new();
    for ch in 0..N_CH {
        if ch == CH_DAC { continue; }
        for n in &a.notes[ch] { onsets.push(n.start as f64); }
    }
    if onsets.len() >= 8 {
        let _ = writeln!(r, "  onset coherence (1.00 = every note starts on the grid):");
        for (name, period) in [("60 Hz frame (735)", 735.0), ("50 Hz frame (882)", 882.0), ("chosen grid", tm.tick)] {
            let _ = writeln!(r, "    {:<18} {:.3}", name, coherence(&onsets, period));
        }
        let c735 = coherence(&onsets, 735.0);
        let cg = coherence(&onsets, tm.tick);
        if c735 > cg + 0.10 {
            let _ = writeln!(r, "  -> note starts are frame-locked tighter than the chosen grid: grid is wrong or too coarse.");
        }
        if !tm.snap {
            let _ = writeln!(r, "  -> no grid detected, notes are placed at raw positions on a fallback 120 BPM clock.");
        }
    }

    let _ = writeln!(r, "  placement error vs original, in ms (what snapping and rounding cost):");
    let _ = writeln!(r, "    {:<6} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8}",
                     "chan", "notes", "start p50", "start p95", "start max", "end p95", "end max", "<1 row");
    for ch in 0..N_CH {
        let ns = &a.notes[ch];
        if ns.is_empty() { continue; }
        let (mut es, mut ee) = (Vec::new(), Vec::new());
        let mut short = 0usize;
        for n in ns {
            let sb = tm.start_beats(n.start).max(0.0);
            let eb = tm.end_beats(n.end).max(sb);
            es.push(ms((back(sb) - n.start as f64).abs()));
            ee.push(ms((back(eb) - n.end as f64).abs()));
            if (n.end - n.start) as f64 / tm.tick < 1.0 { short += 1; }
        }
        let smax = es.iter().cloned().fold(0.0, f64::max);
        let emax = ee.iter().cloned().fold(0.0, f64::max);
        let _ = writeln!(r, "    {:<6} {:>6} {:>10.1} {:>10.1} {:>10.1} {:>10.1} {:>10.1} {:>8}",
                         LABELS[ch], ns.len(), pct(&mut es, 0.5), pct(&mut es, 0.95), smax,
                         pct(&mut ee, 0.95), emax, short);
    }
    let _ = writeln!(r, "  (<1 row = notes shorter than one grid row, which the grid cannot represent faithfully)");
    r
}
