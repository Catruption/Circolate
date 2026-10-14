// src/sf2_engine/fx/mod.rs
//! Effect primitives used by the SF2 engine: a Dattorro plate reverb
//! and a 3-voice modulated-delay chorus, matching the two effects
//! FluidSynth enables by default.
//!
//! `FxOutput` is the small trait the units write their wet signal
//! through.  Two impls: one for a fixed `[[f32; 2]]` array (the
//! sflt-common shape), one for a `&mut [&mut [f32]]` slice pair (which
//! is what the player's per-channel slices look like).

mod chorus;
mod reverb;

pub use chorus::{ChorusMode, ChorusUnit};
pub use reverb::{ReverbMode, ReverbUnit};

pub trait FxOutput {
    fn write(&mut self, output: &[&[f32]], gain: [f32; 2]);
    fn channel_len(&self) -> usize;
}

impl FxOutput for &mut [[f32; 2]] {
    fn write(&mut self, output: &[&[f32]], gain: [f32; 2]) {
        for i in 0..output.len() {
            for j in 0..output[i].len() {
                self[j][i] += output[i][j] * gain[i];
            }
        }
    }

    fn channel_len(&self) -> usize {
        self.len()
    }
}

impl FxOutput for &mut [&mut [f32]] {
    fn write(&mut self, output: &[&[f32]], gain: [f32; 2]) {
        for i in 0..self.len() {
            if i >= output.len() || i >= gain.len() {
                break;
            }
            let src = output[i];
            let g = gain[i];
            let chan = &mut self[i];
            let n = chan.len().min(src.len());
            for j in 0..n {
                chan[j] += src[j] * g;
            }
        }
    }

    fn channel_len(&self) -> usize {
        if self.is_empty() { 0 } else { self[0].len() }
    }
}