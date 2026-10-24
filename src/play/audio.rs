//! Audio-boundary helpers: soft clipper and stdout silencing.

/// Soft-knee peak shaper at the cpal output boundary.
///
/// Replaces the hard `clamp(-1.0, 1.0)` so that boosted parts saturate
/// gracefully rather than truncate.  Below `KNEE` the response is exactly
/// linear; above it the output asymptotes to ±1.0.  The output is
/// guaranteed to be inside `[-1, 1]` for any finite input, so the cpal
/// driver never sees an out-of-range sample.
///
/// At `x = 1.0` (nominal full scale), the shaped output is ~0.928
/// (≈ -0.65 dBFS), which leaves automatic headroom for intersample
/// peaks.  At `x = 4.0` (+12 dB), the shaped output is ~0.9999 —
/// effectively saturated but still strictly inside the representable
/// range.
///
/// `KNEE = 0.7` is the standard soft-knee point used by most DAW-grade
/// saturators: it preserves clarity on loud-but-not-clipping material
/// while catching peaks from boosted parts.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.7;
    const RANGE: f32 = 1.0 - KNEE;

    if !x.is_finite() {
        return 0.0;
    }
    let ax = x.abs();
    if ax <= KNEE {
        x
    } else {
        let sign = if x < 0.0 { -1.0 } else { 1.0 };
        sign * (KNEE + RANGE * ((ax - KNEE) / RANGE).tanh())
    }
}

// ── silence ────────────────────────────────────────────────────────────

/// Redirect stdout and stderr to `/dev/null` for the duration of the
/// guard's lifetime, then restore them on `Drop`.  Used to swallow the
/// chatter from `convert::convert_file` and `player::load_project`
/// during the pre-TUI phase, where any output corrupts the alternate
/// screen.
#[cfg(unix)]
pub struct Silence {
    saved_out: i32,
    saved_err: i32,
}

#[cfg(unix)]
impl Silence {
    pub fn new() -> Option<Self> {
        unsafe {
            let saved_out = libc::dup(1);
            let saved_err = libc::dup(2);
            if saved_out < 0 || saved_err < 0 {
                if saved_out >= 0 {
                    libc::close(saved_out);
                }
                if saved_err >= 0 {
                    libc::close(saved_err);
                }
                return None;
            }
            let devnull = libc::open(
                b"/dev/null\0".as_ptr() as *const libc::c_char,
                                     libc::O_WRONLY,
            );
            if devnull < 0 {
                libc::close(saved_out);
                libc::close(saved_err);
                return None;
            }
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            libc::close(devnull);
            Some(Silence {
                saved_out,
                saved_err,
            })
        }
    }
}

#[cfg(unix)]
impl Drop for Silence {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved_out, 1);
            libc::dup2(self.saved_err, 2);
            libc::close(self.saved_out);
            libc::close(self.saved_err);
        }
    }
}

#[cfg(not(unix))]
pub struct Silence;

#[cfg(not(unix))]
impl Silence {
    pub fn new() -> Option<Self> {
        Some(Silence)
    }
}
