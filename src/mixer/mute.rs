//! Global mute/solo state.  The tracker UI mutates this; the audio
//! callback reads it.
//!
//! Reads are lock-free and allocation-free (`ArcSwap::load`), so the
//! audio thread never waits on the UI.  Writers copy the state, edit
//! the copy, and swap it in (`rcu`), which is fine at UI rates.
//!
//! Mute and solo are exclusive per part: soloing a muted part unmutes
//! it, muting a soloed part unsolos it.  Any number of parts can be
//! muted; at most one can be soloed.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use arc_swap::ArcSwap;

#[derive(Default, Clone)]
struct Inner {
    /// Individually muted notes: part path -> (block line, start row).
    /// Nested so the audio side can look up by `&str` without building
    /// a `(String, ..)` key (which would allocate on every call).
    muted_notes: HashMap<String, HashSet<(usize, i64)>>,
    /// Parts whose every note is muted.
    muted_parts: HashSet<String>,
    /// The single soloed part, if any.  When set, only that part's
    /// notes are audible (subject to `muted_notes`).
    soloed_part: Option<String>,
}

static MUTE: LazyLock<ArcSwap<Inner>> = LazyLock::new(|| ArcSwap::from_pointee(Inner::default()));

/// Copy-edit-swap.  `f` may run more than once if another writer races us.
fn update(f: impl Fn(&mut Inner)) {
    MUTE.rcu(|cur| {
        let mut next = Inner::clone(cur);
        f(&mut next);
        next
    });
}

// ── audio-side read ────────────────────────────────────────────────────

/// Whether a given note should be audible.  Wait-free, no allocation.
///
/// - `part_path` is the same string `Project::part_path` returns.
/// - `block_line` is the line the note's block originated on (stable
///   across the note's lifetime, even for carried notes).
/// - `start_row` is the note's row within its block, in the source's
///   own coordinate system.
pub fn is_note_active(part_path: &str, block_line: usize, start_row: i64) -> bool {
    let s = MUTE.load();
    if s.soloed_part.as_deref().is_some_and(|p| p != part_path) {
        return false;
    }
    if s.muted_parts.contains(part_path) {
        return false;
    }
    !s.muted_notes
        .get(part_path)
        .is_some_and(|rows| rows.contains(&(block_line, start_row)))
}

// ── mutators ───────────────────────────────────────────────────────────

pub fn toggle_note_mute(part_path: &str, block_line: usize, start_row: i64) {
    update(|s| {
        let key = (block_line, start_row);
        let rows = s.muted_notes.entry(part_path.to_string()).or_default();
        if !rows.remove(&key) {
            rows.insert(key);
        }
        if rows.is_empty() {
            s.muted_notes.remove(part_path);
        }
    });
}

/// Toggle mute on a part.  Muting the currently-soloed part unsolos it.
pub fn toggle_part_mute(part_path: &str) {
    update(|s| {
        if !s.muted_parts.remove(part_path) {
            if s.soloed_part.as_deref() == Some(part_path) {
                s.soloed_part = None;
            }
            s.muted_parts.insert(part_path.to_string());
        }
    });
}

/// Toggle solo on a part.  Soloing a different part moves the solo
/// there.  Soloing a muted part unmutes it.
pub fn toggle_solo(part_path: &str) {
    update(|s| {
        if s.soloed_part.as_deref() == Some(part_path) {
            s.soloed_part = None;
        } else {
            s.muted_parts.remove(part_path);
            s.soloed_part = Some(part_path.to_string());
        }
    });
}

/// Set the soloed part explicitly (`None` clears it).  Used by `h`/`l`
/// navigation, where "move the solo pointer" is a set, not a toggle.
pub fn set_solo(part: Option<&str>) {
    update(|s| {
        if let Some(p) = part {
            s.muted_parts.remove(p);
        }
        s.soloed_part = part.map(str::to_string);
    });
}

/// The part currently soloed, if any.
pub fn soloed() -> Option<String> {
    MUTE.load().soloed_part.clone()
}

pub fn clear_all() {
    MUTE.store(Default::default());
}

// ── render snapshot ────────────────────────────────────────────────────

/// Snapshot of the current state, for the renderer's visual feedback.
pub struct Snapshot {
    pub muted_parts: HashSet<String>,
    pub soloed_part: Option<String>,
    pub muted_notes: HashSet<(String, usize, i64)>,
}

pub fn snapshot() -> Snapshot {
    let s = MUTE.load();
    Snapshot {
        muted_parts: s.muted_parts.clone(),
        soloed_part: s.soloed_part.clone(),
        muted_notes: s
            .muted_notes
            .iter()
            .flat_map(|(path, rows)| rows.iter().map(move |&(l, r)| (path.clone(), l, r)))
            .collect(),
    }
}
