//! SN76489 register tracking for timbre snapshots and envelope shapes.
//! Key-on/key-off detection is handled by `soundlog::Sn76489State`;
//! this module only tracks the register bytes needed for the patch
//! snapshot, plus the att-trajectory that becomes `amp adsr`.

use super::regs::{PsgRegs, TimbreSnapshot};

pub(crate) struct PsgState {
    tone: [u16; 3],
    vol: [u8; 4],
    noise_ctl: u8,
    latch_ch: usize,
    latch_vol: bool,
}

impl PsgState {
    pub(crate) fn new() -> Self {
        PsgState {
            tone: [0; 3],
            vol: [15; 4],
            noise_ctl: 0,
            latch_ch: 0,
            latch_vol: false,
        }
    }

    pub(crate) fn write(&mut self, b: u8) {
        if b & 0x80 != 0 {
            self.latch_ch = ((b >> 5) & 3) as usize;
            self.latch_vol = b & 0x10 != 0;
            let d = b & 0x0F;
            let ch = self.latch_ch;
            if self.latch_vol {
                self.vol[ch] = d;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x3F0) | d as u16;
            } else {
                self.noise_ctl = d & 7;
            }
        } else {
            let d = b & 0x3F;
            let ch = self.latch_ch;
            if self.latch_vol {
                self.vol[ch] = d & 0x0F;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x00F) | ((d as u16) << 4);
            } else {
                self.noise_ctl = d & 7;
            }
        }
    }

    pub(crate) fn snapshot(&self, ch: usize) -> TimbreSnapshot {
        if ch == 3 {
            TimbreSnapshot::Psg(PsgRegs {
                is_noise: true,
                tone: self.tone[2],
                vol: self.vol[3],
                noise_ctl: self.noise_ctl,
            })
        } else {
            TimbreSnapshot::Psg(PsgRegs {
                is_noise: false,
                tone: self.tone[ch],
                vol: self.vol[ch],
                noise_ctl: 0,
            })
        }
    }
}
