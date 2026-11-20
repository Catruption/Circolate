//! TUI-facing facade for global mute/solo state.
//!
//! The state itself lives in [`super::state::MixerState`], which is also
//! injected into the audio engine.  These functions address the global
//! (live-playback) instance only; offline renders get a fresh state.
//!
//! Mute and solo are exclusive per part: soloing a muted part unmutes
//! it, muting a soloed part unsolos it.  Any number of parts can be
//! muted; at most one can be soloed.

/// Immutable mute/solo view for the tracker renderer.
pub type Snapshot = super::state::MuteSnapshot;

/// Monotonic version for caches that depend on mute/solo state.
pub fn generation() -> u64 {
    super::state::global().generation()
}

/// Whether a given note should be audible.
pub fn is_note_active(part_path: &str, block_line: usize, start_row: i64) -> bool {
    super::state::global().is_note_active(part_path, block_line, start_row)
}

/// Whether the part is enabled by mute/solo state, ignoring note mutes.
pub fn is_part_active(part_path: &str) -> bool {
    super::state::global().is_part_active(part_path)
}

pub fn toggle_note_mute(part_path: &str, block_line: usize, start_row: i64) {
    super::state::global().toggle_note_mute(part_path, block_line, start_row);
}

pub fn toggle_part_mute(part_path: &str) {
    super::state::global().toggle_part_mute(part_path);
}

pub fn toggle_solo(part_path: &str) {
    super::state::global().toggle_solo(part_path);
}

pub fn set_solo(part: Option<&str>) {
    super::state::global().set_solo(part);
}

pub fn soloed() -> Option<String> {
    super::state::global().soloed()
}

pub fn clear_all() {
    super::state::global().clear_mutes();
}

pub fn snapshot() -> Snapshot {
    super::state::global().mute_snapshot()
}
