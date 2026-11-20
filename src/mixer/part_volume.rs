//! TUI-facing facade for per-part volume offsets.
//!
//! The state itself lives in [`super::state::MixerState`], which is also
//! injected into the audio engine.  These functions address the global
//! (live-playback) instance only; offline renders get a fresh state and
//! never see fader changes made here.
//!
//! Range is `0.0 ..= 4.0` (+12 dB); unity is stored as "absent from the
//! map", so `snapshot()` only contains actual overrides.

use std::collections::HashMap;

/// Current volume multiplier for a part.  `1.0` when there is no override.
pub fn get(part_path: &str) -> f64 {
    super::state::global().volume(part_path)
}

/// Step the volume for a part.  `direction` is `+1` for scroll-up,
/// `-1` for scroll-down.  Returns the new value.
pub fn step(part_path: &str, direction: i32) -> f64 {
    super::state::global().step_volume(part_path, direction)
}

/// Reset a single part to 100%.
pub fn reset(part_path: &str) {
    super::state::global().reset_volume(part_path);
}

/// Clear every per-part override.
pub fn clear_all() {
    super::state::global().clear_volumes();
}

/// Snapshot of non-unity parts.  Reserved for a future volume panel.
#[allow(dead_code)]
pub fn snapshot() -> HashMap<String, f64> {
    super::state::global().volume_snapshot()
}
