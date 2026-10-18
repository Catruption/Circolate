//! Global ring buffer of recent samples per VST channel iid, for the
//! live scrolling scope in the tracker view.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

pub const SCOPE_LEN: usize = 4096;
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

/// Push one rendered block (stereo mixed down to mono, times `gain`)
/// with a single lock attempt, so the audio thread pays one `try_lock`
/// per block instead of one per sample.  If the UI is mid-snapshot the
/// block is dropped, never waited on.
pub fn scope_push_block(part: usize, l: &[f32], r: &[f32], gain: f32) {
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    let Ok(mut q) = slot.try_lock() else { return };
    let n = l.len().min(r.len());
    let skip = n.saturating_sub(SCOPE_LEN);
    let incoming = n - skip;
    let len = q.len();
    let overflow = (len + incoming).saturating_sub(SCOPE_LEN).min(len);
    q.drain(..overflow);
    for j in skip..n {
        q.push_back((l[j] + r[j]) * 0.5 * gain);
    }
}

pub fn scope_snapshot(part: usize, out: &mut Vec<f32>) {
    out.clear();
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(q) = slot.lock() {
        out.extend(q.iter().copied());
    }
}
