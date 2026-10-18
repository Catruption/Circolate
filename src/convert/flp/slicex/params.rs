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
//! Field names are unknown.  Value hints:
//!   * 12800 = 100% in FL's level scale
//!   * 128 = pan centre
//!   * 8000 = ~62.5% — likely the default cutoff
//!
//! ## Provenance / reinterpretation (derived, check in the DLL)
//!
//! SRC[loc=trest+396 (= slot1.count) decomp=FUN_008827d0 conf=derived for the position, layout of the 17 values corpus]
//!
//! Offset 396 in the trest block is where slot 1's slice count sits (76 bytes of
//! params + 4 x 80 bytes of envelopes), so the "108-byte block" is probably:
//!
//!   +0    u32 lead = 1        slot 1's slice count (the placeholder layer has one record)
//!   +4    17 x u32 values     that one 0x44-byte slice record (17 x 4 = 68)
//!   +72   36 bytes            first 36 bytes of the tail (0x14 art header + 16 more)
//!
//! If so, `values` is not a separate "region block" but the per-slice record
//! layout (see records.rs), and the same 17 fields exist in every slot 0 record.
//! Field guesses below ([3] cutoff, [6]/[7] pan, [5] [9] [12] [16] levels) then
//! describe slice records.
//!
//! To check: FUN_008827d0 is recorded as reading this block.  Open it and see
//! whether it reads 17 u32 for ONE element and is called count times from the slot
//! loader.  If yes, rename it to the slice-record reader and fix these names.

#[derive(Debug, Clone)]
pub struct RegionParamBlock<'a> {
    pub raw: &'a [u8],
    pub lead: u32,
    pub values: [u32; 17],
    pub trail: &'a [u8],
}

impl<'a> RegionParamBlock<'a> {
    // SRC[loc=trest+396 decomp=FUN_008827d0 conf=derived]  396 only holds for slot 1 of a normal file:
    // it moves if slot 1's envelopes do not have 2 points each.  mod.rs passes 396 via trest.rs.
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

    // SRC[loc=values[3] decomp=TODO conf=corpus]  which of the 17 is cutoff was found by diffing saves
    pub fn cutoff_raw(&self) -> u32 {
        self.values[3]
    }
    // SRC[loc=values[6], values[7] decomp=TODO conf=corpus]  128 = centre, from the value alone
    pub fn pan_l_raw(&self) -> u32 {
        self.values[6]
    }
    pub fn pan_r_raw(&self) -> u32 {
        self.values[7]
    }
    // SRC[loc=values[5], [9], [12], [16] decomp=TODO conf=corpus]  12800 = 100%; which level is which is unverified
    pub fn levels_raw(&self) -> [u32; 4] {
        [self.values[5], self.values[9], self.values[12], self.values[16]]
    }

    /// Map FL's cutoff (0..12800, log scale) to SF2 absolute cents.
    /// FL's 0 → ~100 Hz, 12800 → ~20 kHz.  SF2 filter Fc is cents above
    /// 8.176 Hz, clamped to 1500..13500.
    // SRC[loc=n/a (our mapping) decomp=TODO conf=guess]  FL 0..12800 -> 100 Hz..20 kHz is an assumption;
    // compare against an FL render at known cutoff values.  The plugin's own cutoff curve is in the filter code.
    pub fn cutoff_sf2_cents(&self) -> i16 {
        let fl = self.cutoff_raw() as f64;
        let norm = (fl / 12800.0).clamp(0.0, 1.0);
        let hz = 100.0 * 200.0f64.powf(norm);
        let cents = 1200.0 * (hz / 8.176).log2();
        cents.clamp(1500.0, 13500.0) as i16
    }

    /// Pan in SF2's −500..+500 scale, from the two pan fields.
    // SRC[loc=n/a (our mapping) decomp=TODO conf=guess]  assumes 0..255 with 128 centre
    pub fn pan_sf2(&self) -> i16 {
        let l = self.pan_l_raw() as i32;
        let r = self.pan_r_raw() as i32;
        let avg = (l + r) / 2;
        let scaled = (avg - 128) * 500 / 128;
        scaled.clamp(-500, 500) as i16
    }

    /// AMP level in SF2 centibels of attenuation (0 = full, 960 = silence).
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
