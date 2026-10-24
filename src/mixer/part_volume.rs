//! Per-part volume offsets.  Mirrors `mute.rs`: the TUI mutates this,
//! the engine reads it live per audio block.  Session-only — nothing is
//! persisted.  Reset at process exit.
//!
//! Scope: applies to sample voices (raw WAV, embedded audio, multi-sample)
//! and to VST/SF2 engines.  `Engine::render` reads the multiplier per
//! voice per block, and `Engine::render_vsts` reads it per iid per block,
//! so changing a header takes effect on the next 512-sample block.
//!
//! ## Range
//!
//! `0.0 ..= MAX_VOLUME` (default `MAX_VOLUME = 4.0`, i.e. +12 dB).
//! Unity (1.0) is stored as "absent from the map" so `snapshot()` only
//! contains actual overrides.  Boosts above 100% are safe because
//! `play.rs::soft_clip` shapes any peak that exceeds full scale into the
//! range the output hardware expects — clipping happens *there*, not
//! inside the mixer, and it happens gracefully.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Upper bound for per-part boost.  `4.0` = +12 dB, matching the
/// default fader range in Reaper / Ardour.  Above this, the soft
/// clipper in `play.rs` is fully saturated — the increase is a
/// distortion shift, not a loudness increase.
const MAX_VOLUME: f64 = 4.0;

const STEP: f64 = 0.05;

struct PartVolumeState {
    inner: Mutex<HashMap<String, f64>>,
}

static PART_VOL: OnceLock<PartVolumeState> = OnceLock::new();

fn state() -> &'static PartVolumeState {
    PART_VOL.get_or_init(|| PartVolumeState {
        inner: Mutex::new(HashMap::new()),
    })
}

/// Current volume multiplier for a part.  Returns `1.0` (unity) when
/// the part has no override.  Called from the audio callback once per
/// voice per block, so the lock is cold in practice.
pub fn get(part_path: &str) -> f64 {
    let s = state();
    let inner = match s.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.get(part_path).copied().unwrap_or(1.0)
}

/// Step the volume for a part.  `direction` is `+1` for scroll-up,
/// `-1` for scroll-down.  Clamps to `[0.0, MAX_VOLUME]`; unity is
/// stored as "absent from the map" so `snapshot()` only contains
/// non-unity parts.  Returns the new value.
pub fn step(part_path: &str, direction: i32) -> f64 {
    let s = state();
    let mut inner = match s.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let cur = inner.get(part_path).copied().unwrap_or(1.0);
    let next = (cur + direction as f64 * STEP).clamp(0.0, MAX_VOLUME);
    if (next - 1.0).abs() < 1e-6 {
        inner.remove(part_path);
    } else {
        inner.insert(part_path.to_string(), next);
    }
    next
}

/// Reset a single part to 100%.
pub fn reset(part_path: &str) {
    let s = state();
    let mut inner = match s.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.remove(part_path);
}

/// Clear every per-part override.
pub fn clear_all() {
    let s = state();
    let mut inner = match s.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.clear();
}

/// Snapshot of non-unity parts.  Reserved for a future volume panel.
#[allow(dead_code)]
pub fn snapshot() -> HashMap<String, f64> {
    let s = state();
    let inner = match s.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.clone()
}
