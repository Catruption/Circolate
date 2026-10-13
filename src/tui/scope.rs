//! Global ring buffer of recent samples per VST channel iid, for the
//! live scrolling scope in the tracker view.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Recent samples retained per part for the live scope.  At 48 kHz this
/// is ~85 ms — comfortably more than one frame.
pub const SCOPE_LEN: usize = 4096;
/// Scope slots are indexed by VST channel iid, a u16.  In practice the
/// FLP-derived projects we handle use 0..=127.
pub const SCOPE_MAX_PARTS: usize = 256;

struct Scope {
    inner: Vec<Mutex<VecDeque<f32>>>,
}

static SCOPE: OnceLock<Scope> = OnceLock::new();

fn scope() -> &'static Scope {
    SCOPE.get_or_init(|| {
        let mut inner = Vec::with_capacity(SCOPE_MAX_PARTS);
        for _ in 0..SCOPE_MAX_PARTS {
            inner.push(Mutex::new(VecDeque::with_capacity(SCOPE_LEN)));
        }
        Scope { inner }
    })
}

/// Call from the audio callback, once per part, after the synth produces
/// the part's sample for this frame.  Never blocks.
pub fn scope_push(part: usize, sample: f32) {
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(mut q) = slot.try_lock() {
        if q.len() >= SCOPE_LEN {
            q.pop_front();
        }
        q.push_back(sample);
    }
}

/// Called from the render thread.  Copies the current window into `out`
/// (cleared first).  Doesn't drain, so a paused synth leaves its last
/// window frozen on screen.
pub fn scope_snapshot(part: usize, out: &mut Vec<f32>) {
    out.clear();
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(q) = slot.lock() {
        out.extend(q.iter().copied());
    }
}
