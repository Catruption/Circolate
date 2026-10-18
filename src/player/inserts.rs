//! Insert effect chains: ordered VST hosts, one chain per mixer insert.
//!
//! Loaded from the `.instrument` sidecars the FLP converter writes
//! under `<trackscript_dir>/inserts/`.  Each sidecar names one effect
//! (VST3 substitute, or a native LUT — currently only VST3 is hosted).
//!
//! Chain order within an insert is by filename, which sorts
//! lexicographically as `insertNN_slotMM_...`, i.e. numeric slot order.
//!
//! Channel-to-insert routing comes from `routing.json` in the same
//! directory:
//!
//!     { "channel_to_insert": { "0": 3, "1": 4 } }
//!
//! Channels not listed route to master (no chain).
//!
//! ## Insert fader
//!
//! Each `.instrument` sidecar carries a `Volume:` line for the mixer
//! insert fader that owns this slot. `load_chains` copies it from the
//! first-loaded slot of each insert into `InsertChain::volume`, and
//! `process()` applies it once at the tail of the chain. Currently the
//! converter writes unity (see `convert/flp/extract.rs::insert_volume`)
//! — the plumbing is in place, the parser is the follow-up.
//!
//! ## State blobs
//!
//! The sidecar carries a `State:` path pointing at the original FL
//! plugin's parameter blob.  That blob is only meaningful to the exact
//! plugin it came from.  Fallback substitutes (PeakEater, Dragonfly,
//! Setekh, ...) have entirely different parameter formats, so feeding
//! them the FL plugin's state is worse than useless: some reject it,
//! others silently load garbage.  Fallback slots therefore load with
//! their own defaults, and per-slot parameter mapping is deferred.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::hosting::vst::ffi::VstHost;

pub struct InsertSlot {
    pub name: String,
    pub gain: f32,
    pub host: VstHost,
}

pub struct InsertChain {
    pub insert: u8,
    pub slots: Vec<InsertSlot>,
    /// Mixer-insert fader, applied *after* every slot in `slots` has
    /// processed. Unity is 1.0. Sourced from the `Volume:` line in the
    /// `.instrument` sidecars (all slots of an insert carry the same
    /// value; the first loaded wins). Remains 1.0 until the FLP parser
    /// exposes the fader position — see
    /// `convert/flp/extract.rs::insert_volume`.
    pub volume: f32,
}

impl InsertChain {
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Run every slot in-place on the given stereo buffer, then apply
    /// the insert fader.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        for slot in &mut self.slots {
            slot.host.process_inplace(&mut l[..n], &mut r[..n]);
            if (slot.gain - 1.0).abs() > 1e-4 {
                for i in 0..n {
                    l[i] *= slot.gain;
                    r[i] *= slot.gain;
                }
            }
        }
        if (self.volume - 1.0).abs() > 1e-4 {
            for i in 0..n {
                l[i] *= self.volume;
                r[i] *= self.volume;
            }
        }
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

struct Sidecar {
    kind: String,
    plugin: String,
    path: String,
    state: Option<String>,
    gain: f32,
    volume: f32,
    unresolved: bool,
    is_fallback: bool,
}

fn parse_sidecar(path: &Path) -> Option<Sidecar> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut s = Sidecar {
        kind: String::new(),
        plugin: String::new(),
        path: String::new(),
        state: None,
        gain: 1.0,
        volume: 1.0,
        unresolved: false,
        is_fallback: false,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Instrument ") || line == "{" || line == "}" {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim().trim_end_matches(';').trim().trim_matches('\'').to_string();
        match k.trim() {
            "Kind" => s.kind = v,
            "Plugin" => s.plugin = v,
            "Path" => s.path = v,
            "State" => s.state = Some(v),
            "Gain" => s.gain = v.parse().unwrap_or(1.0),
            "Volume" => s.volume = v.parse().unwrap_or(1.0),
            "Unresolved" => s.unresolved = v.eq_ignore_ascii_case("true"),
            "Fallback" => s.is_fallback = v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    Some(s)
}

fn parse_slot_filename(stem: &str) -> Option<(u8, u32)> {
    let rest = stem.strip_prefix("insert")?;
    let (nn, rest) = rest.split_once('_')?;
    let insert: u8 = nn.parse().ok()?;
    let rest = rest.strip_prefix("slot")?;
    let (mm, _) = rest.split_once('_')?;
    let slot: u32 = mm.parse().ok()?;
    Some((insert, slot))
}

/// Load every insert chain from `<inserts_dir>/*.instrument`.
/// Missing directory is not an error.
pub fn load_chains(inserts_dir: &Path, sample_rate: u32) -> BTreeMap<u8, InsertChain> {
    let mut out: BTreeMap<u8, InsertChain> = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(inserts_dir) else {
        return out;
    };

    let mut files: Vec<PathBuf> = rd
    .flatten()
    .map(|e| e.path())
    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("instrument"))
    .collect();
    files.sort();

    for path in files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let Some((insert_idx, slot_idx)) = parse_slot_filename(stem) else {
            continue;
        };
        let Some(side) = parse_sidecar(&path) else { continue };
        if side.unresolved || side.kind != "vst3" || side.path.is_empty() {
            continue;
        }

        // Fallback substitutes do NOT understand the original FL plugin's
        // state blob — parameter IDs are entirely different, and passing
        // one to the wrong plugin causes garbage presets or outright
        // activation failures.  Only exact matches get their state.
        let state_bytes: Vec<u8> = if side.is_fallback {
            Vec::new()
        } else {
            match &side.state {
                Some(rel) => {
                    let state_path = path.parent().unwrap_or(Path::new(".")).join(rel);
                    std::fs::read(&state_path).unwrap_or_default()
                }
                None => Vec::new(),
            }
        };

        let host = match VstHost::load(&side.path, &state_bytes, sample_rate) {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "  insert {:02} slot {:02}: failed to load '{}': {:#}",
                    insert_idx, slot_idx, side.plugin, e
                );
                continue;
            }
        };
        let tag = if side.is_fallback { "fallback" } else { "exact" };
        println!(
            "  insert {:02} slot {:02}: loaded '{}' ({})",
                 insert_idx, slot_idx, side.plugin, tag
        );

        // `or_insert_with` runs only on the first slot of each insert
        // (files sort by slot index), so `chain.volume` is populated
        // from slot 0 — which is where `extract.rs` writes the insert
        // fader. All slots carry the same `Volume:` value; any of them
        // would do, but slot 0 is the obvious source.
        out.entry(insert_idx)
        .or_insert_with(|| InsertChain {
            insert: insert_idx,
            slots: Vec::new(),
                        volume: side.volume,
        })
        .slots
        .push(InsertSlot {
            name: side.plugin,
            gain: side.gain,
            host,
        });
    }

    out
}

#[derive(Deserialize, Default)]
struct RoutingFile {
    #[serde(default)]
    channel_to_insert: BTreeMap<String, u8>,
}

/// Load channel→insert routing from `<dir>/routing.json`.
pub fn load_routing(dir: &Path) -> BTreeMap<u16, u8> {
    let path = dir.join("routing.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return BTreeMap::new(),
    };
    let file: RoutingFile = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  routing.json parse error: {:#}", e);
            return BTreeMap::new();
        }
    };
    file.channel_to_insert
    .into_iter()
    .filter_map(|(k, v)| k.parse::<u16>().ok().map(|iid| (iid, v)))
    .collect()
}
