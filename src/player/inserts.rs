//! Insert effect chains: ordered VST hosts, one chain per mixer insert.
//!
//! Two-phase loading:
//!
//! * **Metadata** — `load_chain_metadata` reads `inserts/*.instrument`
//!   and `inserts/routing.json` via `AssetSource`, producing
//!   `ProjectInsertChain` records attached to `Project`.  No plugins
//!   are opened, so this works for `.tsz` archives and directory
//!   sources alike, and it makes chain info available to the views.
//! * **Audio** — `install_chains` walks `Project.chains`, resolves
//!   each slot's plugin by name via `PluginIndex`, opens the host, and
//!   hands the resulting `InsertChain` to `Engine`.
//!
//! ## Plugin resolution
//!
//! **By name only.**  The `Plugin:` field is the canonical identifier;
//! `Path:` is ignored — stale, doxxing, and nothing depends on it.
//!
//! ## State blobs
//!
//! Fallback substitutes do NOT understand the original FL plugin's
//! state blob.  Only exact matches receive `state_bytes`; fallbacks get
//! empty state.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::asset::AssetSource;
use crate::deps::index::PluginIndex;
use crate::hosting::vst::ffi::VstHost;

use super::engine::{Engine, VST_BLOCK};
use super::Project;

// ── metadata (portable, no plugin loading) ────────────────────────────

pub struct ProjectInsertSlot {
    pub name: String,
    pub replaces: Option<String>,
    pub gain: f32,
    pub is_fallback: bool,
    /// Populated during metadata load.  Empty for fallbacks and for
    /// slots whose sidecar had no `State:` field.
    pub state_bytes: Vec<u8>,
}

pub struct ProjectInsertChain {
    pub insert: u8,
    pub slots: Vec<ProjectInsertSlot>,
    pub volume: f32,
}

struct Sidecar {
    kind: String,
    plugin: String,
    replaces: Option<String>,
    state: Option<String>,
    gain: f32,
    volume: f32,
    unresolved: bool,
    is_fallback: bool,
}

fn parse_sidecar_text(text: &str) -> Sidecar {
    let mut s = Sidecar {
        kind: String::new(),
        plugin: String::new(),
        replaces: None,
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
            "Replaces" => s.replaces = Some(v),
            "State" => s.state = Some(v),
            "Gain" => s.gain = v.parse().unwrap_or(1.0),
            "Volume" => s.volume = v.parse().unwrap_or(1.0),
            "Unresolved" => s.unresolved = v.eq_ignore_ascii_case("true"),
            "Fallback" => s.is_fallback = v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    s
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

#[derive(Deserialize, Default)]
struct RoutingFile {
    #[serde(default)]
    channel_to_insert: BTreeMap<String, u8>,
}

/// Read `inserts/*.instrument` and `inserts/routing.json` via
/// `AssetSource`.  No plugins are opened.  Missing `inserts/` returns
/// empty maps.
pub fn load_chain_metadata(
    source: &dyn AssetSource,
) -> (
    BTreeMap<u8, ProjectInsertChain>,
    BTreeMap<u16, u8>,
) {
    let mut chains: BTreeMap<u8, ProjectInsertChain> = BTreeMap::new();
    let mut routing: BTreeMap<u16, u8> = BTreeMap::new();

    let mut files = source.list("inserts", Some("instrument"));
    files.sort();

    for rel in files {
        let Some(stem) = Path::new(&rel).file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((insert_idx, _slot_idx)) = parse_slot_filename(stem) else {
            continue;
        };
        let Ok(Some(text)) = source.read_text(&rel) else { continue };
        let side = parse_sidecar_text(&text);

        if side.unresolved || side.kind != "vst3" || side.plugin.is_empty() {
            continue;
        }

        // Only exact matches receive the original state blob.
        // Fallbacks load with empty state so the substitute plugin
        // doesn't try to parse foreign parameter bytes.
        let state_bytes = if side.is_fallback {
            Vec::new()
        } else {
            side.state
            .as_deref()
            .and_then(|r| source.read_bytes(r).ok().flatten())
            .unwrap_or_default()
        };

        chains
        .entry(insert_idx)
        .or_insert_with(|| ProjectInsertChain {
            insert: insert_idx,
            slots: Vec::new(),
                        volume: side.volume,
        })
        .slots
        .push(ProjectInsertSlot {
            name: side.plugin,
            replaces: side.replaces,
            gain: side.gain,
            is_fallback: side.is_fallback,
            state_bytes,
        });
    }

    if let Ok(Some(text)) = source.read_text("inserts/routing.json") {
        if let Ok(rf) = serde_json::from_str::<RoutingFile>(&text) {
            for (k, v) in rf.channel_to_insert {
                if let Ok(iid) = k.parse::<u16>() {
                    routing.insert(iid, v);
                }
            }
        }
    }

    (chains, routing)
}

// ── audio installation (opens VST hosts) ─────────────────────────────

/// Peak level (≈ -90 dBFS) below which a chain's input and output count
/// as silent for idle-gating purposes.
const IDLE_THRESHOLD: f32 = 3.0e-5;

/// How long a chain must stay silent (input AND output) before its plugins
/// stop being run.  Long enough to outlast reverb tails and tempo-synced
/// delay echoes; raise it if you use delays with more than 8 s of gap.
const IDLE_HOLD_SECS: f64 = 8.0;

pub struct InsertSlot {
    pub name: String,
    pub gain: f32,
    pub host: VstHost,
}

pub struct InsertChain {
    pub insert: u8,
    pub slots: Vec<InsertSlot>,
    pub volume: f32,
    /// The chain's own in/out buffer.  Instruments mix into it, then
    /// `process_scratch` runs the plugins over it in place.  Owning it here
    /// lets chains be processed in parallel without aliasing.
    pub buf_l: Vec<f32>,
    pub buf_r: Vec<f32>,
    idle_blocks: u32,
    idle_hold_blocks: u32,
}

#[inline]
fn peak(l: &[f32], r: &[f32]) -> f32 {
    let mut m = 0.0f32;
    for &s in l {
        m = m.max(s.abs());
    }
    for &s in r {
        m = m.max(s.abs());
    }
    m
}

impl InsertChain {
    pub fn new(insert: u8, slots: Vec<InsertSlot>, volume: f32, sample_rate: u32) -> Self {
        let hold = (IDLE_HOLD_SECS * sample_rate.max(1) as f64 / VST_BLOCK as f64) as u32;
        InsertChain {
            insert,
            slots,
            volume,
            buf_l: vec![0.0; VST_BLOCK as usize],
            buf_r: vec![0.0; VST_BLOCK as usize],
            idle_blocks: 0,
            idle_hold_blocks: hold.max(1),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Process caller-supplied buffers in place (offline / legacy path).
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

    /// Process the chain's own buffers in place for the first `n` frames.
    /// Skips the plugins entirely once input and output have been silent
    /// for `IDLE_HOLD_SECS`; any input wakes the chain immediately.
    pub fn process_scratch(&mut self, n: usize) {
        let n = n.min(self.buf_l.len()).min(self.buf_r.len());
        let in_peak = peak(&self.buf_l[..n], &self.buf_r[..n]);

        if in_peak > IDLE_THRESHOLD {
            self.idle_blocks = 0;
        } else if self.idle_blocks >= self.idle_hold_blocks {
            self.buf_l[..n].fill(0.0);
            self.buf_r[..n].fill(0.0);
            return;
        }

        let l = &mut self.buf_l[..n];
        let r = &mut self.buf_r[..n];
        for slot in &mut self.slots {
            slot.host.process_inplace(&mut *l, &mut *r);
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

        let out_peak = peak(l, r);
        if in_peak <= IDLE_THRESHOLD && out_peak <= IDLE_THRESHOLD {
            self.idle_blocks = self.idle_blocks.saturating_add(1);
        } else {
            self.idle_blocks = 0;
        }
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

/// Walk `project.chains`, load each slot's plugin, and register the
/// resulting `InsertChain` on the engine.  Also installs channel→insert
/// routing from `project.channel_to_insert`.
pub fn install_chains(engine: &mut Engine, project: &Project, sample_rate: u32) {
    if project.chains.is_empty() {
        return;
    }
    let index = PluginIndex::scan();

    for (&insert_idx, chain) in &project.chains {
        let mut slots: Vec<InsertSlot> = Vec::new();

        for slot in &chain.slots {
            let Some(plugin_path) = index.find_vst3(&slot.name).cloned() else {
                eprintln!(
                    "  insert {:02}: plugin '{}' not installed, skipping",
                    insert_idx, slot.name
                );
                continue;
            };
            let state = if slot.is_fallback {
                &[][..]
            } else {
                &slot.state_bytes[..]
            };
            match VstHost::load(plugin_path.to_str().unwrap_or(""), state, sample_rate) {
                Ok(host) => slots.push(InsertSlot {
                    name: slot.name.clone(),
                                       gain: slot.gain,
                                       host,
                }),
                Err(e) => eprintln!(
                    "  insert {:02}: failed to load '{}': {:#}",
                    insert_idx, slot.name, e
                ),
            }
        }

        if !slots.is_empty() {
            engine.install_insert_chain(
                insert_idx,
                InsertChain::new(insert_idx, slots, chain.volume, sample_rate),
            );
        }
    }

    for (&iid, &insert) in &project.channel_to_insert {
        engine.set_channel_insert(iid, insert);
    }
}
