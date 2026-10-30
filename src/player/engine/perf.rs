//! Render-load counters.  Written by `Engine::render` (a handful of
//! relaxed atomic adds per block), read by anything that wants to show
//! or log them.  `perf_snapshot()` returns the numbers accumulated since
//! the previous call and resets them, so poll it at a fixed interval.
//!
//! All load figures are fractions of the audio budget: 0.25 means the
//! stage used a quarter of the time the block takes to play.  `worst`
//! is the single slowest block; anything near or above 1.0 is an
//! underrun.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

pub(crate) struct Perf {
    pub(crate) voices_ns: AtomicU64,
    pub(crate) vst_ns: AtomicU64,
    pub(crate) shim_ns: AtomicU64,
    pub(crate) fx_ns: AtomicU64,
    total_ns: AtomicU64,
    budget_ns: AtomicU64,
    blocks: AtomicU64,
    worst_milli: AtomicU64,
    overruns: AtomicU64,
}

pub(crate) static PERF: Perf = Perf {
    voices_ns: AtomicU64::new(0),
    vst_ns: AtomicU64::new(0),
    shim_ns: AtomicU64::new(0),
    fx_ns: AtomicU64::new(0),
    total_ns: AtomicU64::new(0),
    budget_ns: AtomicU64::new(0),
    blocks: AtomicU64::new(0),
    worst_milli: AtomicU64::new(0),
    overruns: AtomicU64::new(0),
};

/// Add the time since `*t` to `slot`, and restart the lap.
#[inline]
pub(crate) fn lap(t: &mut Instant, slot: &AtomicU64) {
    let now = Instant::now();
    slot.fetch_add(now.duration_since(*t).as_nanos() as u64, Relaxed);
    *t = now;
}

impl Perf {
    #[inline]
    pub(crate) fn record_block(&self, total: Duration, budget_ns: u64) {
        let total_ns = total.as_nanos() as u64;
        self.total_ns.fetch_add(total_ns, Relaxed);
        self.budget_ns.fetch_add(budget_ns, Relaxed);
        self.blocks.fetch_add(1, Relaxed);
        if budget_ns > 0 {
            let milli = total_ns.saturating_mul(1000) / budget_ns;
            self.worst_milli.fetch_max(milli, Relaxed);
            if milli >= 1000 {
                self.overruns.fetch_add(1, Relaxed);
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PerfSnapshot {
    pub(crate) blocks: u64,
    /// Average total render time / audio time.
    pub(crate) load: f32,
    /// Slowest single block, same units.
    pub(crate) worst: f32,
    /// Blocks that took longer than they play for.
    pub(crate) overruns: u64,
    pub(crate) voices: f32,
    pub(crate) vst: f32,
    pub(crate) shim: f32,
    pub(crate) fx: f32,
}

/// Counters since the last call (which are then reset).
#[allow(dead_code)]
pub(crate) fn perf_snapshot() -> PerfSnapshot {
    let budget = PERF.budget_ns.swap(0, Relaxed) as f64;
    let frac = |v: u64| if budget > 0.0 { (v as f64 / budget) as f32 } else { 0.0 };
    PerfSnapshot {
        blocks: PERF.blocks.swap(0, Relaxed),
        load: frac(PERF.total_ns.swap(0, Relaxed)),
        worst: PERF.worst_milli.swap(0, Relaxed) as f32 / 1000.0,
        overruns: PERF.overruns.swap(0, Relaxed),
        voices: frac(PERF.voices_ns.swap(0, Relaxed)),
        vst: frac(PERF.vst_ns.swap(0, Relaxed)),
        shim: frac(PERF.shim_ns.swap(0, Relaxed)),
        fx: frac(PERF.fx_ns.swap(0, Relaxed)),
    }
}
