// src/sf2_engine/host.rs
//! HostData implementation for the SF2 engine.
//!
//! sfl2 asks the host for MIDI CC values during rendering (via the
//! `ModList` modulators in the SF2 file, e.g. CC7 → attenuation,
//! CC10 → pan).  Per-voice snapshot, defaults matching FluidSynth's
//! channel reset: volume 100, expression 127, balance and pan centred.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::hosting::sf2::sfl2::hi::HostData;

#[derive(Clone)]
pub struct Sf2HostData {
    ccs: Arc<[AtomicU8; 128]>,
}

impl Sf2HostData {
    pub fn new() -> Self {
        let ccs: [AtomicU8; 128] = std::array::from_fn(|i| {
            AtomicU8::new(match i {
                7 => 100,       // Volume (FluidSynth resets to 100, not 127)
                11 => 127,      // Expression
                8 | 10 => 64,   // Balance, Pan
                _ => 0,
            })
        });
        Sf2HostData { ccs: Arc::new(ccs) }
    }

    pub fn set_cc(&self, cc: u8, value: u8) {
        self.ccs[cc as usize].store(value, Ordering::Relaxed);
    }
}

impl Default for Sf2HostData {
    fn default() -> Self { Self::new() }
}

impl HostData for Sf2HostData {
    fn midi_cc_value(&self, cc: u8) -> u8 {
        self.ccs[cc as usize].load(Ordering::Relaxed)
    }
}