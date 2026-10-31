//! Global ring buffer of recent samples per VST channel iid, for the
//! live scrolling scope in the tracker view.

use std::sync::{Mutex, OnceLock};

pub const SCOPE_LEN: usize = 4096;
pub const SCOPE_MAX_PARTS: usize = 256;

/// Reserved scope slots for VGM chip mixes and isolated hardware voices.
/// Chip-wide mixes, used for a bare `vgm_embedded` part:
pub const VGM_SLOT_PSG: usize = SCOPE_MAX_PARTS - 1;
pub const VGM_SLOT_YM: usize = SCOPE_MAX_PARTS - 2;
/// Isolated taps, one per hardware voice: FM1..FM6, PSG1..PSG3, NOISE, DAC.
pub const VGM_SLOT_FM0: usize = SCOPE_MAX_PARTS - 16; // FM1 = +0 .. FM6 = +5
pub const VGM_SLOT_PSG0: usize = VGM_SLOT_FM0 + 6; // PSG1 = +0 .. PSG3 = +2
pub const VGM_SLOT_NOISE: usize = VGM_SLOT_FM0 + 9;
pub const VGM_SLOT_DAC: usize = VGM_SLOT_FM0 + 10;

pub fn vgm_voice_scope_slot(voice: &str) -> Option<usize> {
    match voice {
        "ch1" => Some(VGM_SLOT_FM0), "ch2" => Some(VGM_SLOT_FM0 + 1),
        "ch3" => Some(VGM_SLOT_FM0 + 2), "ch4" => Some(VGM_SLOT_FM0 + 3),
        "ch5" => Some(VGM_SLOT_FM0 + 4), "ch6" => Some(VGM_SLOT_FM0 + 5),
        "tone1" => Some(VGM_SLOT_PSG0), "tone2" => Some(VGM_SLOT_PSG0 + 1),
        "tone3" => Some(VGM_SLOT_PSG0 + 2), "noise" => Some(VGM_SLOT_NOISE),
        "dac" => Some(VGM_SLOT_DAC),
        _ => None,
    }
}

/// Map a VGM part's source path (`vgm_embedded/FM1`, or converted patch
/// names such as `banks/FM1_hash.patch`) to the live chip scope.
pub fn vgm_scope_slot(path: &str) -> Option<usize> {
    let lower = path.to_ascii_lowercase();
    let tail = lower.rsplit('/').next().unwrap_or(&lower)
        .strip_suffix(".patch").unwrap_or(lower.rsplit('/').next().unwrap_or(&lower));
    if let Some((prefix, _)) = tail.split_once('_') {
        let voice = match prefix {
            "fm1" => "ch1", "fm2" => "ch2", "fm3" => "ch3",
            "fm4" => "ch4", "fm5" => "ch5", "fm6" => "ch6",
            "psg1" => "tone1", "psg2" => "tone2", "psg3" => "tone3",
            "noise" => "noise", "dac" => "dac",
            _ => return None,
        };
        return vgm_voice_scope_slot(voice);
    }
    // Unknown channel-like patch names fall back to the chip-wide tap.
    if ["fm", "dac"].iter().any(|p| tail.starts_with(p)
        && tail.as_bytes().get(p.len()).copied() == Some(b'_')) {
        return Some(VGM_SLOT_YM);
    }
    if ["psg", "noise"].iter().any(|p| tail.starts_with(p)
        && tail.as_bytes().get(p.len()).copied() == Some(b'_')) {
        return Some(VGM_SLOT_PSG);
    }
    let num = |prefix: &str, max: usize| -> Option<usize> {
        let n: usize = tail.strip_prefix(prefix)?.parse().ok()?;
        if (1..=max).contains(&n) { Some(n - 1) } else { None }
    };
    if tail == "dac" {
        Some(VGM_SLOT_DAC)
    } else if tail == "noise" {
        Some(VGM_SLOT_NOISE)
    } else if tail.starts_with("fm") {
        Some(num("fm", 6).map_or(VGM_SLOT_YM, |i| VGM_SLOT_FM0 + i))
    } else if tail.starts_with("psg") {
        Some(num("psg", 3).map_or(VGM_SLOT_PSG, |i| VGM_SLOT_PSG0 + i))
    } else if lower.contains("vgm") {
        Some(VGM_SLOT_YM) // bare `vgm_embedded` placeholder part
    } else {
        None
    }
}

struct Scope {
    inner: Vec<Mutex<ScopeRing>>,
}

struct ScopeRing {
    samples: Box<[f32; SCOPE_LEN]>,
    write: usize,
    len: usize,
}

impl ScopeRing {
    fn new() -> Self {
        Self { samples: Box::new([0.0; SCOPE_LEN]), write: 0, len: 0 }
    }

    fn push(&mut self, value: f32) {
        self.samples[self.write] = value;
        self.write = (self.write + 1) % SCOPE_LEN;
        self.len = (self.len + 1).min(SCOPE_LEN);
    }

    fn copy_recent(&self, count: usize, out: &mut Vec<f32>) {
        let count = count.min(self.len);
        let start = (self.write + SCOPE_LEN - count) % SCOPE_LEN;
        let first = count.min(SCOPE_LEN - start);
        out.extend_from_slice(&self.samples[start..start + first]);
        out.extend_from_slice(&self.samples[..count - first]);
    }
}

static SCOPE: OnceLock<Scope> = OnceLock::new();

fn scope() -> &'static Scope {
    SCOPE.get_or_init(|| {
        let mut inner = Vec::with_capacity(SCOPE_MAX_PARTS);
        for _ in 0..SCOPE_MAX_PARTS {
            inner.push(Mutex::new(ScopeRing::new()));
        }
        Scope { inner }
    })
}

pub fn scope_push(part: usize, sample: f32) {
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(mut q) = slot.try_lock() {
        q.push(sample);
    }
}

/// Push one rendered block (stereo mixed down to mono, times `gain`)
/// with a single lock attempt, so the audio thread pays one `try_lock`
/// per block instead of one per sample.  If the UI is mid-snapshot the
/// block is dropped, never waited on.
pub fn scope_push_block(part: usize, l: &[f32], r: &[f32], gain: f32) {
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    let Ok(mut q) = slot.try_lock() else { return };
    let n = l.len().min(r.len());
    let skip = n.saturating_sub(SCOPE_LEN);
    for j in skip..n {
        q.push((l[j] + r[j]) * 0.5 * gain);
    }
}

pub fn scope_snapshot(part: usize, out: &mut Vec<f32>) {
    out.clear();
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(q) = slot.lock() {
        q.copy_recent(q.len, out);
    }
}

/// Copy only the newest samples needed for a rendered envelope. Avoids
/// cloning the whole ring (4096 samples) on every tracker redraw.
pub fn scope_snapshot_recent(part: usize, max_samples: usize, out: &mut Vec<f32>) {
    out.clear();
    if max_samples == 0 { return; }
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(q) = slot.lock() {
        q.copy_recent(max_samples, out);
    }
}
