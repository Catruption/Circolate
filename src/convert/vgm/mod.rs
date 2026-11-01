//! VGM → halotext + chip patches.

pub(crate) mod analyze;
pub(crate) mod dac;
pub(crate) mod psg;
pub(crate) mod regs;
pub(crate) mod timing;
pub(crate) mod tracker;
pub(crate) mod halotext;
pub(crate) mod util;

use anyhow::{Context, Result};
use std::path::Path;

use super::{AuxFile, ConvertOptions, Converted};
use analyze::analyze;
use timing::detect_timing;
use halotext::{build_halotext, shadow_halotext};

pub(crate) const VGM_RATE: f64 = 44100.0;
pub(crate) const BPM: f64 = 120.0;
pub(crate) const BEATS_PER_MEASURE: f64 = 4.0;

pub(crate) const MIN_ONSET_NOTE: u32 = 1500;
pub(crate) const TICK_MIN: usize = 1500;
pub(crate) const TICK_MAX: usize = 30000;
pub(crate) const MIN_COHERENCE: f64 = 0.55;
pub(crate) const SNAP_START: f64 = 0.20;
pub(crate) const SNAP_END: f64 = 0.05;
pub(crate) const FALLBACK_SECONDS: f64 = 120.0;
pub(crate) const SOURCE: &str = "vgm_embedded";

pub(crate) const DAC_GAP: u32 = 1500;
pub(crate) const DAC_MIDI: i32 = 36;

pub(crate) const CH_FM: usize = 0;
pub(crate) const CH_DAC: usize = 6;
pub(crate) const CH_PSG: usize = 7;
pub(crate) const CH_NOISE: usize = 10;
pub(crate) const N_CH: usize = 11;
pub(crate) const LABELS: [&str; N_CH] = [
    "FM1", "FM2", "FM3", "FM4", "FM5", "FM6", "DAC", "PSG1", "PSG2", "PSG3", "NOISE",
];

pub fn convert_vgm(path: &Path, opts: &ConvertOptions) -> Result<Converted> {
    let mut raw_bytes = std::fs::read(path).context("reading VGM file")?;

    let is_vgz_ext = path.extension().and_then(|s| s.to_str())
    .map(|s| s.eq_ignore_ascii_case("vgz")).unwrap_or(false);
    let is_gzip_magic = raw_bytes.len() > 2 && raw_bytes[0] == 0x1F && raw_bytes[1] == 0x8B;
    if is_vgz_ext || is_gzip_magic {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&raw_bytes[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).context("decompressing VGZ")?;
        raw_bytes = decompressed;
    }

    let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("(untitled)").to_string();
    let source_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(untitled)").to_string();
    let subs = opts.subdivisions.max(1);

    let analysis = analyze(&raw_bytes)?;

    let (halotext, patches, grid_text) = match &analysis {
        Some(a) if a.notes.iter().any(|n| !n.is_empty()) => {
            let tm = detect_timing(a, subs);
            use std::collections::HashMap;
            let mut seen: HashMap<(usize, u64), regs::TimbreSnapshot> = HashMap::new();
            for ch in 0..N_CH {
                for n in &a.notes[ch] {
                    seen.entry((ch, n.timbre_hash)).or_insert_with(|| n.timbre.clone());
                }
            }
            let mut patches: Vec<(String, Vec<u8>)> = Vec::new();
            let mut keys: Vec<(usize, u64)> = seen.keys().cloned().collect();
            keys.sort();
            for key in keys {
                let timbre = &seen[&key];
                let (ch, hash) = key;
                let name = format!("{}_{:08x}", LABELS[ch], (hash as u32));
                let body = timbre.to_patch_body(&name, &title, &source_name);
                let rel = format!("banks/{}/{}.patch", title, name);
                patches.push((rel, body.into_bytes()));
            }
            (build_halotext(a, &title, &source_name, subs, &tm), patches, format!("{}{}", timing::grid_report(a, &tm, subs), dac::report(&raw_bytes, a.total)))
        }
        _ => {
            let total_seconds = analysis
            .as_ref()
            .map(|a| a.total as f64 / VGM_RATE)
            .filter(|s| *s > 0.0)
            .unwrap_or(FALLBACK_SECONDS);
            (shadow_halotext(total_seconds, &title, &source_name, subs), Vec::new(), String::new())
        }
    };

    let mut aux = vec![AuxFile { rel_path: "audio.vgm".to_string(), bytes: raw_bytes }];
    for (rel_path, bytes) in patches {
        aux.push(AuxFile { rel_path, bytes });
    }
    if let Some(a) = &analysis {
        aux.push(AuxFile { rel_path: "vgm_report.txt".to_string(), bytes: format!("{}\n{}", a.report, grid_text).into_bytes() });
    }

    Ok(Converted {
        halotext,
       samples: Vec::new(),
       samples_json: String::new(),
       aux,
    })
}
