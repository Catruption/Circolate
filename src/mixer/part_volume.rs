//! Per-part volume offsets.  Mirrors `mute.rs`: the TUI mutates this,
//! the engine reads it live per audio block.  Session-only — nothing is
//! persisted.  Reset at process exit.
//!
//! Reads are lock-free and allocation-free (`ArcSwap::load`), so the
//! audio thread never waits on the UI.  Writers copy the map, edit the
//! copy, and swap it in (`rcu`), which is fine at UI rates.
//!
//! Scope: applies to sample voices (raw WAV, embedded audio, multi-sample)
//! and to VST/SF2 engines.  `Engine::render` reads the multiplier per
//! voice per block (and ramps from the previous block's value, so a step
//! doesn't click), and `Engine::render_vsts` reads it per iid per block,
//! so changing a header takes effect on the next 512-sample block.
//!
//! ## Range
//!
//! `0.0 ..= MAX_VOLUME` (default `MAX_VOLUME = 4.0`, i.e. +12 dB).
//! Unity (1.0) is stored as "absent from the map" so `snapshot()` only
//! contains actual overrides.  Boosts above 100% are safe because
//! `play/audio.rs::soft_clip` shapes any peak that exceeds full scale
//! into the range the output hardware expects — clipping happens *there*,
//! not inside the mixer, and it happens gracefully.

use std::collections::HashMap;
use std::sync::LazyLock;

use arc_swap::ArcSwap;

/// Upper bound for per-part boost.  `4.0` = +12 dB, matching the
/// default fader range in Reaper / Ardour.  Above this, the soft
/// clipper is fully saturated — the increase is a distortion shift,
/// not a loudness increase.
const MAX_VOLUME: f64 = 4.0;

const STEP: f64 = 0.05;

static PART_VOL: LazyLock<ArcSwap<HashMap<String, f64>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Copy-edit-swap.  `f` may run more than once if another writer races us.
fn update(f: impl Fn(&mut HashMap<String, f64>)) {
    PART_VOL.rcu(|cur| {
        let mut next = HashMap::clone(cur);
        f(&mut next);
        next
    });
}

/// Current volume multiplier for a part.  Returns `1.0` (unity) when
/// the part has no override.  Wait-free, no allocation.
pub fn get(part_path: &str) -> f64 {
    PART_VOL.load().get(part_path).copied().unwrap_or(1.0)
}

/// Step the volume for a part.  `direction` is `+1` for scroll-up,
/// `-1` for scroll-down.  Clamps to `[0.0, MAX_VOLUME]`; unity is
/// stored as "absent from the map" so `snapshot()` only contains
/// non-unity parts.  Returns the new value.
pub fn step(part_path: &str, direction: i32) -> f64 {
    let cur = get(part_path);
    let next = (cur + direction as f64 * STEP).clamp(0.0, MAX_VOLUME);
    update(|m| {
        if (next - 1.0).abs() < 1e-6 {
            m.remove(part_path);
        } else {
            m.insert(part_path.to_string(), next);
        }
    });
    next
}

/// Reset a single part to 100%.
pub fn reset(part_path: &str) {
    update(|m| {
        m.remove(part_path);
    });
}

/// Clear every per-part override.
pub fn clear_all() {
    PART_VOL.store(Default::default());
}

/// Snapshot of non-unity parts.  Reserved for a future volume panel.
#[allow(dead_code)]
pub fn snapshot() -> HashMap<String, f64> {
    HashMap::clone(&**PART_VOL.load())
}
