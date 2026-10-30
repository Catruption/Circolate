//! Process-wide caches that outlive a single `Project` load.
//!
//! A reload re-parses the trackscript, but nearly everything it
//! references is unchanged: audio files, SoundFonts, the plugin list.
//! These stores hand back the *same* `Arc`s for unchanged content, so a
//! reload costs a hash comparison instead of a decode, and two copies of
//! a song never sit in memory at once.  `sweep()` (called at the end of
//! every load) drops anything only the cache still holds.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt::{Debug, Write as _};
use std::hash::Hasher;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

use anyhow::Result;
use arc_swap::ArcSwapOption;

use crate::audio_decode::decode_audio;
use crate::deps::index::PluginIndex;

use super::project::Project;
use super::types::{Sample, Sf2Instrument};

// ── hashing ────────────────────────────────────────────────────────────

struct HashWriter(DefaultHasher);

impl std::fmt::Write for HashWriter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// Hash anything `Debug` without allocating the string.  Every
/// trackscript type derives `Debug`, so a field added later is covered
/// automatically.  Don't feed it a `HashMap` (iteration order is random);
/// sort into a `Vec` first.
pub(crate) fn hash_debug<T: Debug + ?Sized>(t: &T) -> u64 {
    let mut w = HashWriter(DefaultHasher::new());
    let _ = write!(w, "{:?}", t);
    w.0.finish()
}

pub(crate) fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(b);
    h.finish()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ── decoded audio ──────────────────────────────────────────────────────

static DECODED: LazyLock<Mutex<HashMap<u64, Arc<Sample>>>> = LazyLock::new(Default::default);

/// Decode `bytes` (or reuse the earlier decode of identical bytes).
/// Returns the sample and its content key, which doubles as the
/// fingerprint `hash_part` uses.
pub(crate) fn decode_cached(
    bytes: &[u8],
    rel: &str,
    loop_pts: Option<(i64, i64)>,
) -> Result<(Arc<Sample>, u64)> {
    let key = hash_debug(&(hash_bytes(bytes), rel, loop_pts));
    if let Some(s) = lock(&DECODED).get(&key) {
        return Ok((Arc::clone(s), key));
    }
    let decoded = decode_audio(bytes, rel)?;
    let rate = decoded.rate;
    let stereo = decoded.channels_count() == 2;
    let mut chans = decoded.channels;
    let right = if stereo { chans.pop().unwrap() } else { Vec::new() };
    let left = chans.pop().unwrap_or_default();
    let smp = Arc::new(Sample { left, right, stereo, rate, loop_pts });
    lock(&DECODED).insert(key, Arc::clone(&smp));
    Ok((smp, key))
}

// ── SoundFont instruments ──────────────────────────────────────────────

/// A built SF2 instrument with sample ids starting at 0.  Callers shift
/// it into their own id range with `Sf2Instrument::rebased`.
pub(crate) struct Sf2Built {
    pub(crate) inst: Sf2Instrument,
    /// `samples[i]` has id `base + i`.
    pub(crate) samples: Vec<Arc<Sample>>,
}

static SF2_BUILT: LazyLock<Mutex<HashMap<u64, Arc<Sf2Built>>>> = LazyLock::new(Default::default);

pub(crate) fn sf2_built(
    key: u64,
    build: impl FnOnce() -> Result<Sf2Built>,
) -> Result<Arc<Sf2Built>> {
    if let Some(b) = lock(&SF2_BUILT).get(&key) {
        return Ok(Arc::clone(b));
    }
    let built = Arc::new(build()?);
    lock(&SF2_BUILT).insert(key, Arc::clone(&built));
    Ok(built)
}

/// Drop cache entries nothing else references any more.
pub(crate) fn sweep() {
    lock(&DECODED).retain(|_, s| Arc::strong_count(s) > 1);
    lock(&SF2_BUILT).retain(|_, b| b.samples.iter().any(|s| Arc::strong_count(s) > 1));
}

// ── plugin index ───────────────────────────────────────────────────────

/// One disk scan per session instead of one per load/install.
pub(crate) fn plugin_index() -> &'static PluginIndex {
    static INDEX: OnceLock<PluginIndex> = OnceLock::new();
    INDEX.get_or_init(PluginIndex::scan)
}

// ── the project the TUI should be showing ──────────────────────────────

static LIVE: LazyLock<ArcSwapOption<Project>> = LazyLock::new(ArcSwapOption::empty);

/// Called by the session at start and by the reload watcher.
pub(crate) fn publish_live(p: Arc<Project>) {
    LIVE.store(Some(p));
}

/// Latest loaded project.  The TUI should read lyrics (and anything else
/// that can change on reload) from here, not from the `Arc<Project>` it
/// was handed at startup.
#[allow(dead_code)]
pub(crate) fn live_project() -> Option<Arc<Project>> {
    LIVE.load_full()
}
