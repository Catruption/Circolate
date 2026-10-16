//! Memoryless waveshaper backed by a lookup table.
//!
//! The table stores the effect's output for input values evenly spaced
//! across [-1.0, +1.0].  `process` linearly interpolates between
//! entries.  16k entries is more than enough for any smooth distortion
//! curve; the artifacts shipped in `share/native_fx/` use that size.
//!
//! Used for Fruity Fast Dist, Fruity Blood Overdrive, Fruity Soft
//! Clipper, and any other stateless saturator whose transfer curve can
//! be captured with a ramp sweep.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use super::NativeEffect;

pub struct WaveshaperLut {
    table: Vec<f32>,
}

impl WaveshaperLut {
    pub fn from_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
        .with_context(|| format!("reading LUT {}", path.display()))?;
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            return Err(anyhow!(
                "LUT {} has invalid length {} (must be non-zero multiple of 4)",
                               path.display(),
                               bytes.len()
            ));
        }
        let n = bytes.len() / 4;
        let mut table = Vec::with_capacity(n);
        for chunk in bytes.chunks_exact(4) {
            table.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok(Self { table })
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Identity passthrough if the table is empty (shouldn't happen but
    /// keeps the engine from producing silence on a bad load).
    #[inline]
    pub fn process(&self, sample: f32) -> f32 {
        let n = self.table.len();
        if n == 0 {
            return sample;
        }
        let clamped = sample.clamp(-1.0, 1.0);
        let idx_f = (clamped + 1.0) * 0.5 * (n - 1) as f32;
        let i0 = idx_f as usize;
        let i1 = (i0 + 1).min(n - 1);
        let t = idx_f - i0 as f32;
        self.table[i0] * (1.0 - t) + self.table[i1] * t
    }
}

impl NativeEffect for WaveshaperLut {
    fn process_block(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.process(*s);
        }
    }

    fn reset(&mut self) {
        // Memoryless: nothing to reset.
    }
}
