//! Global mute/solo state.  The tracker UI mutates this; the audio
//! callback reads it.  A single `Mutex<Inner>` keeps the invariant that
//! a note's audibility is consistent with what the UI is displaying.
//!
//! Mute and solo are exclusive per part: soloing a muted part unmutes
//! it, muting a soloed part unsolos it.  Any number of parts can be
//! muted; at most one can be soloed.
//!
//! The audio side needs one call per note-on: `is_note_active`.  See
//! the doc comment on that function for the exact invocation.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct Inner {
    /// Notes that are individually muted, keyed by
    /// (part path, block line, start row within the block).
    muted_notes: HashSet<(String, usize, i64)>,
    /// Parts whose every note is muted.
    muted_parts: HashSet<String>,
    /// The single soloed part, if any.  When set, only that part's
    /// notes are audible (subject to `muted_notes`).
    soloed_part: Option<String>,
}

struct MuteState {
    inner: Mutex<Inner>,
}

static MUTE: OnceLock<MuteState> = OnceLock::new();

fn state() -> &'static MuteState {
    MUTE.get_or_init(|| MuteState { inner: Mutex::new(Inner::default()) })
}

// ── audio-side read ────────────────────────────────────────────────────

/// Whether a given note should be audible.
///
/// Call this from the audio callback when a note-on is about to be
/// dispatched:
///
/// ```ignore
/// if crate::mixer::mute::is_note_active(&part_path, block_line, start_row) {
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

    // Solo, when set, wins over everything else.
    if let Some(soloed) = &inner.soloed_part {
        if soloed != part_path {
            return false;
        }
    }
    if inner.muted_parts.contains(part_path) {
        return false;
    }
    if inner.muted_notes.contains(&(part_path.to_string(), block_line, start_row)) {
        return false;
    }
    true
}

// ── mutators ───────────────────────────────────────────────────────────

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

/// Toggle mute on a part.  Muting the currently-soloed part unsolos it,
/// because the two states are exclusive.
pub fn toggle_part_mute(part_path: &str) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if !inner.muted_parts.remove(part_path) {
        if inner.soloed_part.as_deref() == Some(part_path) {
            inner.soloed_part = None;
        }
        inner.muted_parts.insert(part_path.to_string());
    }
}

/// Toggle solo on a part.  Soloing a different part moves the solo
/// there.  Soloing a muted part unmutes it, because the two states are
/// exclusive.
pub fn toggle_solo(part_path: &str) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if inner.soloed_part.as_deref() == Some(part_path) {
        inner.soloed_part = None;
    } else {
        inner.muted_parts.remove(part_path);
        inner.soloed_part = Some(part_path.to_string());
    }
}

/// Set the soloed part explicitly.  `None` clears it.  Used by `h`/`l`
/// navigation, where "move the solo pointer" is a set operation, not a
/// toggle.
pub fn set_solo(part: Option<&str>) {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    match part {
        Some(p) => {
            inner.muted_parts.remove(p);
            inner.soloed_part = Some(p.to_string());
        }
        None => inner.soloed_part = None,
    }
}

/// The part currently soloed, if any.  Takes the lock; callers that
/// already have a snapshot should use that instead.
pub fn soloed() -> Option<String> {
    let m = state();
    let inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.soloed_part.clone()
}

pub fn clear_all() {
    let m = state();
    let mut inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    inner.muted_notes.clear();
    inner.muted_parts.clear();
    inner.soloed_part = None;
}

// ── render snapshot ────────────────────────────────────────────────────

/// Snapshot of the current state, for the renderer's visual feedback.
pub struct Snapshot {
    pub muted_parts: HashSet<String>,
    pub soloed_part: Option<String>,
    pub muted_notes: HashSet<(String, usize, i64)>,
}

pub fn snapshot() -> Snapshot {
    let m = state();
    let inner = match m.inner.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    Snapshot {
        muted_parts: inner.muted_parts.clone(),
        soloed_part: inner.soloed_part.clone(),
        muted_notes: inner.muted_notes.clone(),
    }
}