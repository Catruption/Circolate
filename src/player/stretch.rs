//! Pitch-preserving time stretch (WSOLA) for samples that declare their
//! own tempo with `|bpm`.

/// Stretch `chans` (equal-length channels at `sr` Hz) so the result is
/// `factor` times as long: `factor = sample_bpm / song_bpm`.  Pitch is kept.
/// Alignment is chosen on the channel mix so stereo images stay coherent.
pub(super) fn stretch(chans: &[&[f32]], factor: f64, sr: f64) -> Vec<Vec<f32>> {
    let nch = chans.len();
    let n_in = chans.first().map_or(0, |c| c.len());
    let out_len = (n_in as f64 * factor).round() as usize;
    if nch == 0 || n_in == 0 || out_len == 0 {
        return vec![Vec::new(); nch];
    }

    let win = (((sr * 0.04) as usize).max(64)) & !1; // ~40 ms, even
    let hop_out = win / 2;
    let hop_in = hop_out as f64 / factor;
    let tol = ((sr * 0.012) as usize).max(8); // ±12 ms search

    if n_in < win * 2 + 2 * tol {
        return linear(chans, out_len);
    }

    // Alignment signal: channel mix.
    let mid: Vec<f32> = (0..n_in)
    .map(|i| chans.iter().map(|c| c[i]).sum::<f32>() / nch as f32)
    .collect();

    let hann: Vec<f32> = (0..win)
    .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / win as f64).cos() as f32)
    .collect();

    let max_pos = n_in - win;
    let mut out = vec![vec![0f32; out_len + win]; nch];
    let frames = out_len / hop_out + 1;
    let mut prev = 0usize;

    for k in 0..frames {
        let nominal = (k as f64 * hop_in).round() as isize;
        let pos = if k == 0 {
            0
        } else {
            // The segment that would naturally follow the previous one.
            let tgt = (prev + hop_out).min(max_pos);
            let lo = (nominal - tol as isize).max(0);
            let hi = (nominal + tol as isize).min(max_pos as isize);
            if lo > hi {
                max_pos
            } else {
                let mut best = lo as usize;
                let mut best_score = f64::MIN;
                for cand in lo as usize..=hi as usize {
                    let (mut dot, mut en) = (0f64, 1e-9f64);
                    let mut i = 0;
                    while i < win {
                        let a = mid[cand + i] as f64;
                        dot += a * mid[tgt + i] as f64;
                        en += a * a;
                        i += 4;
                    }
                    let score = dot / en.sqrt();
                    if score > best_score {
                        best_score = score;
                        best = cand;
                    }
                }
                best
            }
        };

        let o = k * hop_out;
        for c in 0..nch {
            let src = &chans[c][pos..pos + win];
            let dst = &mut out[c];
            for i in 0..win {
                if o + i >= dst.len() {
                    break;
                }
                // Keep the very first half-window at full gain so an
                // onset at the start of the sample is not faded in.
                let w = if k == 0 && i < hop_out { 1.0 } else { hann[i] };
                dst[o + i] += w * src[i];
            }
        }
        prev = pos;
    }

    for c in out.iter_mut() {
        c.truncate(out_len);
    }
    out
}

/// Fallback for very short samples: plain resample (pitch changes).
fn linear(chans: &[&[f32]], out_len: usize) -> Vec<Vec<f32>> {
    chans
    .iter()
    .map(|c| {
        let n = c.len();
        (0..out_len)
        .map(|i| {
            let p = i as f64 * n as f64 / out_len as f64;
            let a = p.floor() as usize;
            let f = (p - a as f64) as f32;
            let x = c[a.min(n - 1)];
            let y = c[(a + 1).min(n - 1)];
            x + (y - x) * f
        })
        .collect()
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengthens_and_keeps_pitch() {
        let sr = 44100.0;
        let n = 44100;
        let sine: Vec<f32> = (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / sr).sin() as f32)
        .collect();
        let out = stretch(&[&sine], 1.2857, sr);
        assert_eq!(out[0].len(), (n as f64 * 1.2857).round() as usize);
        // Count zero crossings: frequency should stay ~440 Hz.
        let zc = out[0].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let hz = zc as f64 / (out[0].len() as f64 / sr);
        assert!((hz - 440.0).abs() < 15.0, "got {hz}");
    }
}
