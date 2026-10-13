//! Global mute/solo state.  The tracker UI mutates this; the audio
//! callback reads it.  A single `Mutex<Inner>` keeps the invariant that
//! a note's audibility is consistent with what the UI is displaying.
//!
//! The audio side needs one call per note-on: `is_note_active`.  See the
//! doc comment on that function for the exact invocation.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct Inner {
    /// Notes that are individually muted, keyed by
    /// (part path, block line, start row within the block).
    muted_notes: HashSet<(String, usize, i64)>,
    /// Parts whose every note is muted.
    muted_parts: HashSet<String>,
    /// Soloed parts.  When non-empty, only notes from these parts are
    /// audible, regardless of muted_notes / muted_parts.
    soloed_parts: HashSet<String>,
}

struct MuteState {
    inner: Mutex<Inner>,
}

static MUTE: OnceLock<MuteState> = OnceLock::new();

fn state() -> &'static MuteState {
    MUTE.get_or_init(|| MuteState { inner: Mutex::new(Inner::default()) })
}

/// Whether a given note should be audible.
///
/// Call this from the audio callback when a note-on is about to be
/// dispatched:
///
/// ```ignore
/// if crate::tui::mute::is_note_active(&part_path, block_line, start_row) {
///     // schedule the note as usual
/// }
/// ```
///
/// - `part_path` is the same string `Project::part_path` returns.
/// - `block_line` is the index of the line the note's block originated
///   on, not the currently-playing line.  For a carried note, this is
///   the original block's line, not the line the note has been carried
///   into.  Stable across the note's lifetime.
/// - `start_row` is the note's row within its block, in the source's
///   own coordinate system.
pub fn is_note_active(part_path: &str, block_line: usize, start_row: i64) -> bool {
    let m = state();
    let inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !inner.soloed_parts.is_empty() && !inner.soloed_parts.contains(part_path) {
        return false;
    }
    if inner.muted_parts.contains(part_path) {
        return false;
    }
    if inner.muted_notes.contains(&(part_path.to_string(), block_line, start_row)) {
        return false;
    }
    true
}

pub fn toggle_note_mute(part_path: &str, block_line: usize, start_row: i64) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let key = (part_path.to_string(), block_line, start_row);
    if !inner.muted_notes.remove(&key) {
        inner.muted_notes.insert(key);
    }
}

pub fn toggle_part_mute(part_path: &str) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !inner.muted_parts.remove(part_path) {
        inner.muted_parts.insert(part_path.to_string());
    }
}

pub fn toggle_part_solo(part_path: &str) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !inner.soloed_parts.remove(part_path) {
        inner.soloed_parts.insert(part_path.to_string());
    }
}

pub fn clear_all() {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.muted_notes.clear();
    inner.muted_parts.clear();
    inner.soloed_parts.clear();
}

/// Snapshot of the current state, for the renderer's visual feedback.
pub fn snapshot() -> (HashSet<String>, HashSet<String>, HashSet<(String, usize, i64)>) {
    let m = state();
    let inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    (
        inner.muted_parts.clone(),
     inner.soloed_parts.clone(),
     inner.muted_notes.clone(),
    )
}
