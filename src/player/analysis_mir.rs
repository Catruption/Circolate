//! Music Information Retrieval (MIR) helpers for player/analysis.rs.

use std::collections::BTreeSet;
use super::motion::MotionFeatures;

pub const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

#[derive(Clone, Debug)]
pub struct N {
    pub start: f64,
    pub end: f64,
    pub pitch: i32,
    pub beats: f64,
}

pub struct Split {
    pub melody: Vec<N>,
    pub harmony: Vec<N>,
}

pub fn split_roles(notes: &[N], instrument_name: &str) -> Split {
    let name = instrument_name.to_lowercase();

    if name.contains("bass") || name.contains("kick") || name.contains("pad") || name.contains("sustain") || name.contains("chord") || name.contains("string") {
        return Split { melody: vec![], harmony: notes.to_vec() };
    }

    if name.contains("lead") || name.contains("chromatc") || name.contains("glock") || name.contains("shakuhachi") || name.contains("solo") || name.contains("piano_wide") {
        return Split { melody: notes.to_vec(), harmony: vec![] };
    }

    let mut s = notes.to_vec();
    s.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap().then(a.pitch.cmp(&b.pitch)));
    let (mut melody, mut harmony) = (vec![], vec![]);
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j < s.len() && (s[j].start - s[i].start).abs() < 0.01 {
            j += 1;
        }
        let group = &s[i..j];
        let longest = group.iter().map(|n| n.beats).fold(0.0, f64::max);
        let top = group.len() - 1;
        let top_is_melody = if group.len() == 1 {
            group[0].beats < 2.0
        } else {
            group[top].beats <= longest * 0.5
        };
        for (k, n) in group.iter().enumerate() {
            if top_is_melody && k == top {
                melody.push(n.clone());
            } else {
                harmony.push(n.clone());
            }
        }
        i = j;
    }
    Split { melody, harmony }
}

/// Collapse simultaneous notes to the highest pitch at each attack time.
/// This provides a conservative upper-line contour without mixing channels.
pub fn top_line(notes: &[N]) -> Vec<N> {
    let mut sorted = notes.to_vec();
    sorted.sort_by(|a, b| a.start.partial_cmp(&b.start)
        .unwrap_or(std::cmp::Ordering::Equal).then(a.pitch.cmp(&b.pitch)));
    let mut out: Vec<N> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i + 1;
        while j < sorted.len() && (sorted[j].start - sorted[i].start).abs() < 0.01 { j += 1; }
        if let Some(note) = sorted[i..j].iter().max_by_key(|n| n.pitch) { out.push((*note).clone()); }
        i = j;
    }
    out
}

pub fn sounding_at(notes: &[N], row: f64) -> Vec<i32> {
    notes.iter().filter(|n| n.start <= row && row < n.end).map(|n| n.pitch).collect()
}

const TEMPLATES: &[(&str, &[i32])] = &[
    ("", &[0, 4, 7]),
    ("m", &[0, 3, 7]),
    ("5", &[0, 7]),
    ("dim", &[0, 3, 6]),
    ("aug", &[0, 4, 8]),
    ("sus2", &[0, 2, 7]),
    ("sus4", &[0, 5, 7]),
    ("6", &[0, 4, 7, 9]),
    ("m6", &[0, 3, 7, 9]),
    ("maj7", &[0, 4, 7, 11]),
    ("m7", &[0, 3, 7, 10]),
    ("7", &[0, 4, 7, 10]),
    ("m7b5", &[0, 3, 6, 10]),
    ("add9", &[0, 4, 7, 2]),
    ("m(add9)", &[0, 3, 7, 2]),
    ("9", &[0, 4, 7, 10, 2]),
    ("maj9", &[0, 4, 7, 11, 2]),
    ("m9", &[0, 3, 7, 10, 2]),
];

#[derive(Clone, Debug)]
pub struct Chord {
    pub root_pc: i32,
    pub name: String,
    pub suffix: &'static str,
    pub minor: bool,
    pub missing_third: bool,
    pub root_doubled: usize,
    pub span_octaves: f64,
    pub inverted: bool,
    pub outside_intervals: Vec<i32>,
}

pub fn identify(pitches: &[i32]) -> Option<Chord> {
    let bass = *pitches.iter().min()?;
    let top = *pitches.iter().max()?;
    let pcs: BTreeSet<i32> = pitches.iter().map(|p| p.rem_euclid(12)).collect();

    let mut best: Option<(i32, i32, &'static str, Vec<i32>, Vec<i32>)> = None;

    for &root in &pcs {
        for (suffix, tpl) in TEMPLATES {
            let matched: Vec<i32> = tpl.iter().copied()
                .filter(|&iv| pcs.contains(&((root + iv) % 12))).collect();
            let hit = matched.len() as i32;
            // Permit one missing chord tone (for example a maj7 shell with
            // no fifth), but avoid naming a chord from a single coincidental
            // interval.
            if hit < 2 || tpl.len() - matched.len() > 1 {
                continue;
            }

            let mut extra_notes = Vec::new();
            for &pc in &pcs {
                let interval = (pc - root).rem_euclid(12);
                if !tpl.contains(&interval) {
                    extra_notes.push(interval);
                }
            }

            let missing: Vec<i32> = tpl.iter().copied()
                .filter(|&iv| !pcs.contains(&((root + iv) % 12))).collect();
            let mut score = hit * 3 - missing.len() as i32 * 2 - extra_notes.len() as i32 * 2;
            if root == bass.rem_euclid(12) {
                score += 3;
            }
            if missing.is_empty() { score += 2; }

            if best.as_ref().map_or(true, |&(b, r, _, _, _)| score > b || (score == b && root == bass.rem_euclid(12) && r != bass.rem_euclid(12))) {
                best = Some((score, root, suffix, missing, extra_notes));
            }
        }
    }

    let (_, root, suffix, missing, extra_notes) = best?;
    let has = |iv: i32| pcs.contains(&((root + iv) % 12));

    let mut final_name = format!("{}{}", NAMES[root as usize], suffix);
    if !extra_notes.is_empty() {
        let extra_names: Vec<&str> = extra_notes.iter().map(|&iv| {
            match iv {
                1 => "b9", 2 => "9", 3 => "b3", 4 => "3", 5 => "11", 6 => "b5", 7 => "5", 8 => "b13", 9 => "13", 10 => "b7", 11 => "7", _ => ""
            }
        }).filter(|s| !s.is_empty()).collect();
        if !extra_names.is_empty() {
            final_name = format!("{}(add {})", final_name, extra_names.join(","));
        }
    }
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|&iv| match iv {
            0 => "root", 1 => "b9", 2 => "9", 3 => "b3", 4 => "3", 5 => "11",
            6 => "b5", 7 => "5", 8 => "b13", 9 => "13", 10 => "b7", 11 => "7", _ => "?",
        }).collect();
        final_name.push_str(&format!(" (no {})", names.join("/")));
    }

    Some(Chord {
        root_pc: root,
         name: final_name,
         suffix,
         minor: suffix.starts_with('m') && !suffix.starts_with("maj"),
         missing_third: !has(3) && !has(4),
         root_doubled: pitches.iter().filter(|p| p.rem_euclid(12) == root).count(),
         span_octaves: (top - bass) as f64 / 12.0,
         inverted: bass.rem_euclid(12) != root,
         outside_intervals: extra_notes,
    })
}

const MAJ: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MIN: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..a.len() {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    if da == 0.0 || db == 0.0 { 0.0 } else { num / (da * db).sqrt() }
}

pub fn histogram(notes: &[N]) -> [f64; 12] {
    let mut h = [0.0; 12];
    for n in notes {
        h[n.pitch.rem_euclid(12) as usize] += n.beats.max(0.05);
    }
    h
}

pub fn estimate_key(hist: &[f64; 12]) -> (i32, bool) {
    let mut best = (0, true, f64::MIN);
    for tonic in 0..12usize {
        let rotated: Vec<f64> = (0..12).map(|i| hist[(i + tonic) % 12]).collect();
        for (major, prof) in [(true, &MAJ), (false, &MIN)] {
            let r = pearson(&rotated, prof);
            if r > best.2 {
                best = (tonic as i32, major, r);
            }
        }
    }
    (best.0, best.1)
}

pub fn roman(chord: &Chord, tonic: i32) -> String {
    const R: [&str; 12] = ["I", "bII", "II", "bIII", "III", "IV", "#IV", "V", "bVI", "VI", "bVII", "VII"];
    let r = R[(chord.root_pc - tonic).rem_euclid(12) as usize];
    if chord.minor { r.to_lowercase() } else { r.to_string() }
}

pub fn scale_summary(pcs: &BTreeSet<i32>, tonic: i32, major: bool) -> String {
    let scale: &[i32] = if major { &[0, 2, 4, 5, 7, 9, 11] } else { &[0, 2, 3, 5, 7, 8, 10] };
    let outside: Vec<&str> = pcs
    .iter()
    .filter(|&&pc| !scale.contains(&(pc - tonic).rem_euclid(12)))
    .map(|&pc| NAMES[pc as usize])
    .collect();
    let key = format!("{} {}", NAMES[tonic as usize], if major { "major" } else { "minor" });
    if outside.is_empty() {
        format!("{key} (all {} notes in scale)", pcs.len())
    } else {
        format!("{key}, outside: {}", outside.join(" "))
    }
}

pub fn melody_contour(melody: &[N]) -> String {
    let mut m = melody.to_vec();
    m.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    if m.len() < 2 {
        return "Too few notes to describe contour".into();
    }

    let steps: Vec<i32> = m.windows(2).map(|w| w[1].pitch - w[0].pitch).collect();
    let net = m.last().unwrap().pitch - m[0].pitch;
    let low = m.iter().map(|n| n.pitch).min().unwrap_or(0);
    let high = m.iter().map(|n| n.pitch).max().unwrap_or(0);
    let leaps = steps.iter().filter(|s| s.abs() >= 7).count();
    let direction = if net > 0 { "rises" } else if net < 0 { "falls" } else { "returns to its starting pitch" };
    format!("{} attacks; range {}–{}; overall {} {} st; {} leaps of 7+ st",
        m.len(), format!("{}{}", NAMES[low.rem_euclid(12) as usize], low.div_euclid(12)),
        format!("{}{}", NAMES[high.rem_euclid(12) as usize], high.div_euclid(12)),
        direction, net.abs(), leaps)
}

pub fn find_motif(melody: &[N]) -> Option<String> {
    let mut m = melody.to_vec();
    m.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    for p in (3..=6).rev() {
        if m.len() < 2 * p { continue; }
        for first in 0..=m.len() - 2 * p {
            for second in first + p..=m.len() - p {
                let same_intervals = (0..p - 1).all(|k| {
                    m[first + k + 1].pitch - m[first + k].pitch
                        == m[second + k + 1].pitch - m[second + k].pitch
                });
                let same_rhythm = (0..p - 1).all(|k| {
                    let a = m[first + k + 1].start - m[first + k].start;
                    let b = m[second + k + 1].start - m[second + k].start;
                    (a - b).abs() <= 0.08 * m[first + k].beats.max(1.0)
                });
                if same_intervals && same_rhythm {
                    return Some(format!("{}-note interval/rhythm figure repeats at beats {:.2} and {:.2}",
                        p, m[first].start + 1.0, m[second].start + 1.0));
                }
            }
        }
    }
    None
}

pub struct Features {
    pub chord: Option<Chord>,
    pub roman: Option<String>,
    pub contour: String,
    pub melody_mean: Option<f64>,
    pub prev_melody_mean: Option<f64>,
    pub onsets_per_beat: f64,
    pub prev_onsets_per_beat: Option<f64>,
    pub has_motif: bool,
    pub has_bends: bool,
    pub unique_pc_count: usize,
    pub active_pitches: usize,
    pub average_pitch: Option<f64>,
    pub motion: MotionFeatures,
    pub prev_motion: MotionFeatures, // <--- ADD THIS
    pub outside_scale_notes: Vec<String>,
}
