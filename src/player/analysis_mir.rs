//! Music Information Retrieval (MIR) helpers for player/analysis.rs.

use std::collections::BTreeSet;

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
}

pub fn identify(pitches: &[i32]) -> Option<Chord> {
    let bass = *pitches.iter().min()?;
    let top = *pitches.iter().max()?;
    let pcs: BTreeSet<i32> = pitches.iter().map(|p| p.rem_euclid(12)).collect();

    let mut best: Option<(i32, i32, &'static str, Vec<i32>)> = None;

    for &root in &pcs {
        for (suffix, tpl) in TEMPLATES {
            let hit = tpl.iter().filter(|&&iv| pcs.contains(&((root + iv) % 12))).count() as i32;
            if hit < tpl.len() as i32 {
                continue;
            }

            let mut extra_notes = Vec::new();
            for &pc in &pcs {
                let interval = (pc - root).rem_euclid(12);
                if !tpl.contains(&interval) {
                    extra_notes.push(interval);
                }
            }

            let mut score = hit * 2;
            if root == bass.rem_euclid(12) {
                score += 5;
            }

            // FIXED: use .as_ref() to prevent moving `best`
            if best.as_ref().map_or(true, |&(b, _, _, _)| score > b) {
                best = Some((score, root, suffix, extra_notes));
            }
        }
    }

    let (_, root, suffix, extra_notes) = best?;
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

    Some(Chord {
        root_pc: root,
         name: final_name,
         suffix,
         minor: suffix.starts_with('m') && !suffix.starts_with("maj"),
         missing_third: !has(3) && !has(4),
         root_doubled: pitches.iter().filter(|p| p.rem_euclid(12) == root).count(),
         span_octaves: (top - bass) as f64 / 12.0,
         inverted: bass.rem_euclid(12) != root,
    })
}

const MAJ: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MIN: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    // FIXED: `da` and `db` need to be mutable
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
    if m.len() < 3 {
        return "Too few melody notes".into();
    }

    let steps: Vec<i32> = m.windows(2).map(|w| w[1].pitch - w[0].pitch).collect();

    if steps.iter().all(|s| s.abs() == 2) {
        return "Whole-Tone run (ascending/descending): dreamlike, floating, unresolved".into();
    }

    if steps.iter().all(|s| s.abs() == 1) {
        return "Chromatic run: intense, climbing tension".into();
    }

    let n = steps.len();
    let leaps = steps.iter().filter(|s| s.abs() > 4).count();
    let stepwise = steps.iter().filter(|s| (1..=2).contains(&s.abs())).count();
    let net = m.last().unwrap().pitch - m[0].pitch;

    if leaps * 3 > n {
        "Leaping: dramatic, disjointed".into()
    } else if stepwise * 2 >= n && net.abs() <= 2 {
        "Stepwise, circling its start: hypnotic, conversational".into()
    } else if stepwise * 2 >= n {
        format!("Stepwise, {}: smooth, flowing", if net > 0 { "rising" } else { "falling" })
    } else {
        "Mixed steps and skips: unpredictable".into()
    }
}

pub fn find_motif(melody: &[N]) -> Option<String> {
    let mut m = melody.to_vec();
    m.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let pcs: Vec<i32> = m.iter().map(|n| n.pitch.rem_euclid(12)).collect();
    for p in 2..=6 {
        if pcs.len() >= 2 * p && pcs[..p] == pcs[p..2 * p] {
            let fig: Vec<&str> = pcs[..p].iter().map(|&x| NAMES[x as usize]).collect();
            return Some(format!("{}-note figure repeating: {}", p, fig.join(" ")));
        }
    }
    None
}

pub struct Features {
    pub chord: Option<Chord>,
    pub roman: Option<String>,
    pub contour: String, // <--- ADDED THIS
    pub melody_mean: Option<f64>,
    pub prev_melody_mean: Option<f64>,
    pub onsets_per_beat: f64,
    pub prev_onsets_per_beat: Option<f64>,
    pub has_motif: bool,
    pub has_bends: bool,
    pub unique_pc_count: usize,
    pub active_pitches: usize,
    pub average_pitch: Option<f64>,
}
