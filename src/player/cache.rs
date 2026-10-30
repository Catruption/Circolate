//! Per-part template cache and fingerprinting.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::trackscript::TrackscriptPart;

use super::stretch;
use super::template::{build_template, PartTemplate};
use super::types::{
    MultiSampleInstrument, Sample, Sf2EngineSpec, Sf2Instrument, VstInstrument,
};

pub(crate) struct Cache {
    enabled: bool,
    /// Hash of everything a template depends on besides the part itself
    /// (sample set, instrument ids, multi-sample config).  Templates are
    /// only inherited across a reload when this matches.
    env: u64,
    map: HashMap<u64, Arc<PartTemplate>>,
    /// Keyed by the *source sample's identity* (its `Arc` pointer) and the
    /// stretch factor, so entries stay valid across reloads that reuse the
    /// same decoded sample.  The source `Arc` is stored in the value to
    /// keep the pointer from being reused for a different sample.
    stretched: HashMap<(usize, u64), (Arc<Sample>, Arc<Sample>)>,
    hits: u64,
    misses: u64,
}

impl Cache {
    pub(crate) fn new(enabled: bool) -> Self {
        Cache {
            enabled,
            env: 0,
            map: HashMap::new(),
            stretched: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    pub(crate) fn set_env(&mut self, env: u64) {
        self.env = env;
    }

    /// Cheap copy (the values are `Arc`s) for the reload watcher to load
    /// against without holding the live cache's lock.
    pub(crate) fn snapshot(&self) -> Cache {
        Cache {
            enabled: self.enabled,
            env: self.env,
            map: self.map.clone(),
            stretched: self.stretched.clone(),
            hits: 0,
            misses: 0,
        }
    }

    /// Carry reusable entries over from the previous load's cache.
    /// `live_hashes` / `live_ptrs` are the part hashes and sample
    /// pointers of the new project, so stale entries aren't kept.
    pub(crate) fn inherit_from(
        &mut self,
        prev: &Cache,
        live_hashes: &HashSet<u64>,
        live_ptrs: &HashSet<usize>,
    ) {
        if self.enabled && prev.enabled && prev.env == self.env {
            for (k, v) in &prev.map {
                if live_hashes.contains(k) {
                    self.map.entry(*k).or_insert_with(|| Arc::clone(v));
                }
            }
        }
        for (k, (src, out)) in &prev.stretched {
            if live_ptrs.contains(&k.0) {
                self.stretched
                    .entry(*k)
                    .or_insert_with(|| (Arc::clone(src), Arc::clone(out)));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn template(
        &mut self,
        hash: u64,
        part: &TrackscriptPart,
        path_to_id: &HashMap<String, u32>,
        samples: &HashMap<u32, Arc<Sample>>,
        sf2_instruments: &HashMap<String, Arc<Sf2Instrument>>,
        sf2_engine_specs: &HashMap<String, Sf2EngineSpec>,
        vst_instruments: &HashMap<String, Arc<VstInstrument>>,
        multi_sample_instruments: &HashMap<String, Arc<MultiSampleInstrument>>,
    ) -> Arc<PartTemplate> {
        if self.enabled {
            if let Some(t) = self.map.get(&hash) {
                self.hits += 1;
                return Arc::clone(t);
            }
        }
        self.misses += 1;
        let t = Arc::new(build_template(
            part,
            path_to_id,
            samples,
            sf2_instruments,
            sf2_engine_specs,
            vst_instruments,
            multi_sample_instruments,
        ));
        if self.enabled {
            self.map.insert(hash, Arc::clone(&t));
        }
        t
    }

    /// `_sid` is unused now (entries are keyed by sample identity); the
    /// parameter is kept so callers don't change.
    pub(crate) fn stretched(
        &mut self,
        _sid: u32,
        sample: &Arc<Sample>,
        factor: f64,
    ) -> Arc<Sample> {
        let key = (Arc::as_ptr(sample) as usize, factor.to_bits());
        if let Some((_, out)) = self.stretched.get(&key) {
            return Arc::clone(out);
        }
        let mut chans: Vec<&[f32]> = vec![&sample.left];
        if sample.stereo {
            chans.push(&sample.right);
        }
        let mut out = stretch::stretch(&chans, factor, sample.rate).into_iter();
        let left = out.next().unwrap_or_default();
        let right = out.next().unwrap_or_else(|| left.clone());
        let scale = |p: i64| (p as f64 * factor).round() as i64;
        let s = Arc::new(Sample {
            left,
            right,
            stereo: sample.stereo,
            rate: sample.rate,
            loop_pts: sample.loop_pts.map(|(a, b)| (scale(a), scale(b))),
        });
        self.stretched.insert(key, (Arc::clone(sample), Arc::clone(&s)));
        s
    }

    pub(crate) fn describe(&self) -> String {
        if self.enabled {
            format!("part cache: {} hits, {} builds", self.hits, self.misses)
        } else {
            format!("part cache: off ({} builds)", self.misses)
        }
    }
}

#[inline]
fn fnv(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(0x100000001b3);
}

#[allow(dead_code)]
pub(crate) fn fingerprint(s: &Sample) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    fnv(&mut h, s.rate.to_bits());
    fnv(&mut h, s.stereo as u64);
    match s.loop_pts {
        Some((a, b)) => {
            fnv(&mut h, a as u64);
            fnv(&mut h, b as u64);
        }
        None => fnv(&mut h, u64::MAX),
    }
    fnv(&mut h, s.left.len() as u64);
    for x in &s.left {
        fnv(&mut h, x.to_bits() as u64);
    }
    for x in &s.right {
        fnv(&mut h, x.to_bits() as u64);
    }
    h
}

pub(crate) fn hash_part(
    part: &TrackscriptPart,
    path_to_id: &HashMap<String, u32>,
    fps: &HashMap<u32, u64>,
) -> u64 {
    let mut h = DefaultHasher::new();
    let sample_key = |path: &str| -> (u32, u64) {
        match path_to_id.get(path) {
            Some(id) => (*id, fps.get(id).copied().unwrap_or(0)),
            None => (0, 0),
        }
    };
    part.sample_path.hash(&mut h);
    sample_key(&part.sample_path).hash(&mut h);
    part.sample_offset.hash(&mut h);
    part.src_bpm.map(f64::to_bits).hash(&mut h);
    part.subs_per_beat.hash(&mut h);
    part.beats_per_measure.hash(&mut h);
    let ob = |x: Option<f64>| x.map(f64::to_bits);
    for ev in &part.events {
        ev.op.hash(&mut h);
        ev.row.to_bits().hash(&mut h);
        ob(ev.pitch_hz).hash(&mut h);
        ob(ev.vol).hash(&mut h);
        ob(ev.pan).hash(&mut h);
        ob(ev.dur_beats).hash(&mut h);
        ev.offset.hash(&mut h);
        ob(ev.src_bpm).hash(&mut h);
        ev.sample_override.hash(&mut h);
        if let Some(p) = &ev.sample_override {
            sample_key(p).hash(&mut h);
        }
    }
    h.finish()
}