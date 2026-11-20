//! One injectable mixer state, shared by the UI and the audio engine.
//!
//! `global()` is the TUI-facing instance: keybindings write there and live
//! playback reads the same handle, so faders feel immediate.  Offline
//! rendering constructs a fresh `MixerState::default()` and passes it to
//! `Engine`, so a WAV render depends on the project — not on whichever
//! fader happened to be moved last in the UI.
//!
//! Reads are `ArcSwap::load`: wait-free and allocation-free on the audio
//! thread.  Writers copy-edit-swap at UI rates.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;

const MAX_VOLUME: f64 = 4.0;
const STEP: f64 = 0.05;

#[derive(Default, Clone)]
struct Volumes {
    /// Unity is stored as "absent" so snapshots only hold real overrides.
    parts: HashMap<String, f64>,
}

#[derive(Default, Clone)]
struct Mutes {
    /// part path -> (block line, start row)
    muted_notes: HashMap<String, HashSet<(usize, i64)>>,
    muted_parts: HashSet<String>,
    soloed_part: Option<String>,
}

/// Immutable mute/solo view for the tracker renderer.
#[derive(Clone, Default)]
pub struct MuteSnapshot {
    pub muted_parts: HashSet<String>,
    pub soloed_part: Option<String>,
    pub muted_notes: HashSet<(String, usize, i64)>,
}

/// The whole mixer's mutable state, held behind an `Arc` by `Engine`.
pub struct MixerState {
    volumes: ArcSwap<Volumes>,
    mutes: ArcSwap<Mutes>,
    generation: AtomicU64,
}

impl Default for MixerState {
    fn default() -> Self {
        Self {
            volumes: ArcSwap::from_pointee(Volumes::default()),
            mutes: ArcSwap::from_pointee(Mutes::default()),
            generation: AtomicU64::new(0),
        }
    }
}

static GLOBAL: LazyLock<Arc<MixerState>> =
    LazyLock::new(|| Arc::new(MixerState::default()));

/// Clone this into live playback.  Never use it for offline rendering.
pub fn global() -> Arc<MixerState> {
    Arc::clone(&GLOBAL)
}

impl MixerState {
    // ── volume ─────────────────────────────────────────────────────────

    /// Current volume multiplier for a part, or `1.0` when unset.
    pub fn volume(&self, part_path: &str) -> f64 {
        self.volumes.load().parts.get(part_path).copied().unwrap_or(1.0)
    }

    /// Step a fader by `direction * 0.05`, clamped to `[0, 4]`.
    pub fn step_volume(&self, part_path: &str, direction: i32) -> f64 {
        let next = (self.volume(part_path) + direction as f64 * STEP)
        .clamp(0.0, MAX_VOLUME);
        self.volumes.rcu(|cur| {
            let mut map = Volumes::clone(cur);
            if (next - 1.0).abs() < 1e-6 {
                map.parts.remove(part_path);
            } else {
                map.parts.insert(part_path.to_string(), next);
            }
            map
        });
        next
    }

    pub fn reset_volume(&self, part_path: &str) {
        self.volumes.rcu(|cur| {
            let mut map = Volumes::clone(cur);
            map.parts.remove(part_path);
            map
        });
    }

    pub fn clear_volumes(&self) {
        self.volumes.store(Default::default());
    }

    pub fn volume_snapshot(&self) -> HashMap<String, f64> {
        HashMap::clone(&self.volumes.load().parts)
    }

    // ── mute / solo ────────────────────────────────────────────────────

    /// Monotonic version for caches that depend on mute/solo state.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn update_mute(&self, f: impl Fn(&mut Mutes)) {
        self.mutes.rcu(|cur| {
            let mut next = Mutes::clone(cur);
            f(&mut next);
            next
        });
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether one note should be audible.
    pub fn is_note_active(&self, part_path: &str, block_line: usize, start_row: i64) -> bool {
        let s = self.mutes.load();
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

    /// Part-level mute/solo only, ignoring individual note mutes.
    pub fn is_part_active(&self, part_path: &str) -> bool {
        let s = self.mutes.load();
        !s.soloed_part.as_deref().is_some_and(|p| p != part_path)
        && !s.muted_parts.contains(part_path)
    }

    pub fn toggle_note_mute(&self, part_path: &str, block_line: usize, start_row: i64) {
        self.update_mute(|s| {
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

    pub fn toggle_part_mute(&self, part_path: &str) {
        self.update_mute(|s| {
            if !s.muted_parts.remove(part_path) {
                if s.soloed_part.as_deref() == Some(part_path) {
                    s.soloed_part = None;
                }
                s.muted_parts.insert(part_path.to_string());
            }
        });
    }

    pub fn toggle_solo(&self, part_path: &str) {
        self.update_mute(|s| {
            if s.soloed_part.as_deref() == Some(part_path) {
                s.soloed_part = None;
            } else {
                s.muted_parts.remove(part_path);
                s.soloed_part = Some(part_path.to_string());
            }
        });
    }

    pub fn set_solo(&self, part: Option<&str>) {
        self.update_mute(|s| {
            if let Some(p) = part {
                s.muted_parts.remove(p);
            }
            s.soloed_part = part.map(str::to_string);
        });
    }

    pub fn soloed(&self) -> Option<String> {
        self.mutes.load().soloed_part.clone()
    }

    pub fn clear_mutes(&self) {
        self.mutes.store(Default::default());
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mute_snapshot(&self) -> MuteSnapshot {
        let s = self.mutes.load();
        MuteSnapshot {
            muted_parts: s.muted_parts.clone(),
            soloed_part: s.soloed_part.clone(),
            muted_notes: s.muted_notes
            .iter()
            .flat_map(|(path, rows)| rows.iter().map(move |&(l, r)| (path.clone(), l, r)))
            .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_are_isolated() {
        let a = MixerState::default();
        let b = MixerState::default();
        a.step_volume("part", -1);
        a.toggle_part_mute("part");
        assert!((a.volume("part") - 0.95).abs() < 1e-6);
        assert!(!a.is_part_active("part"));
        assert_eq!(b.volume("part"), 1.0);
        assert!(b.is_part_active("part"));
    }

    #[test]
    fn global_changes_do_not_leak_into_fresh_states() {
        // This is the contract that keeps offline renders deterministic:
        // render_offline constructs a default MixerState, never global().
        let live = global();
        let path = "samples/IsolationTest.sf2#part";
        let before = live.volume(path);
        live.step_volume(path, -1);
        assert_eq!(MixerState::default().volume(path), 1.0);
        live.reset_volume(path);
        assert_eq!(live.volume(path), before);
    }

    #[test]
    fn generation_tracks_mute_changes_only() {
        let s = MixerState::default();
        let g = s.generation();
        s.step_volume("part", -1);
        assert_eq!(s.generation(), g);
        s.toggle_part_mute("part");
        assert_eq!(s.generation(), g + 1);
    }
}
