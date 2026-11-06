//! 108-byte region parameter block at trest+396.
//!
//! Layout (confirmed by the decompile of FUN_008827d0):
//!
//! ```text
//! +0    u32 lead       (always 1)
//! +4    17 u32 values  (region defaults)
//! +72   36 bytes       trailing
//! ```
//!
//! Field names are unknown.  Value hints (all confirmed by
//! `InitRegionRecordDefaults` at 0x8827d0):
//!   * 12800 = 100% in FL's level scale   (values[5],[9],[12],[16])
//!   * 128 = pan centre                    (values[6], values[7])
//!   * 8000 = default cutoff               (values[3])
//!
//! The same 17 fields also exist in every slot-0 slice record
//! (records.rs): a slice modified per-slice (retune, reverse, pan)
//! would show up as a varying column.

#[derive(Debug, Clone)]
pub struct RegionParamBlock<'a> {
    pub raw: &'a [u8],
    pub lead: u32,
    pub values: [u32; 17],
    pub trail: &'a [u8],
}

impl<'a> RegionParamBlock<'a> {
    pub fn parse(trest: &'a [u8], offset: usize) -> Option<Self> {
        if trest.len() < offset + 108 {
            return None;
        }
        let raw = &trest[offset..offset + 108];
        let lead = u32::from_le_bytes(raw[0..4].try_into().unwrap());
        let mut values = [0u32; 17];
        for k in 0..17 {
            values[k] = u32::from_le_bytes(raw[4 + k * 4..8 + k * 4].try_into().unwrap());
        }
        let trail = &raw[72..108];
        Some(RegionParamBlock { raw, lead, values, trail })
    }

    // SRC[loc=values[3] decomp=TODO conf=corpus→verified-by-default]  default 0x1f40 = 8000
    pub fn cutoff_raw(&self) -> u32 {
        self.values[3]
    }
    // SRC[loc=values[6], values[7] decomp=TODO conf=corpus→verified-by-default]  128 = centre
    pub fn pan_l_raw(&self) -> u32 { self.values[6] }
    pub fn pan_r_raw(&self) -> u32 { self.values[7] }
    // SRC[loc=values[5],[9],[12],[16] decomp=TODO conf=corpus→verified-by-default]  12800 = 100%
    pub fn levels_raw(&self) -> [u32; 4] {
        [self.values[5], self.values[9], self.values[12], self.values[16]]
    }

    /// Map FL's cutoff (0..12800, log scale) to SF2 absolute cents.
    // SRC[loc=n/a (our mapping) decomp=TODO conf=guess]  FL 0 -> ~100 Hz, 12800 -> ~20 kHz is an assumption.
    pub fn cutoff_sf2_cents(&self) -> i16 {
        let fl = self.cutoff_raw() as f64;
        let norm = (fl / 12800.0).clamp(0.0, 1.0);
        let hz = 100.0 * 200.0f64.powf(norm);
        let cents = 1200.0 * (hz / 8.176).log2();
        cents.clamp(1500.0, 13500.0) as i16
    }

    // SRC[loc=n/a (our mapping) decomp=TODO conf=guess]  assumes 0..255 with 128 centre
    pub fn pan_sf2(&self) -> i16 {
        let l = self.pan_l_raw() as i32;
        let r = self.pan_r_raw() as i32;
        let avg = (l + r) / 2;
        let scaled = (avg - 128) * 500 / 128;
        scaled.clamp(-500, 500) as i16
    }

    // SRC[loc=values[5] decomp=TODO conf=guess]  assumes values[5] is the amp level and linear in 0..12800
    pub fn amp_attenuation_cb(&self) -> u16 {
        let level = self.values[5].min(12800) as f64;
        let scale = level / 12800.0;
        if scale <= 0.0 {
            return 960;
        }
        let cb = -200.0 * scale.log10();
        cb.clamp(0.0, 960.0) as u16
    }
}