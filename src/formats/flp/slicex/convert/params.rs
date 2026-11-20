//! A native 68-byte Slicex region row.
//!
//! Rows are 17 little-endian u32 values. The positional editor-control mapping
//! and one-based conversions for words 4, 8, 11, and 15 are documented in the
//! decompilation notes. Field labels and several scaling assumptions below
//! are still provisional; callers should preserve the raw row when possible.
//!
//! `derived_caches()` exposes the 11 runtime floats that `FUN_00882850`
//! recomputes from a subset of the words on every load. These are not
//! serialized; they drive UI drawing. See `state_format.md` §"region_count +
//! region_records" for the decomp-verified formulas.

#[derive(Debug, Clone)]
pub struct RegionParamBlock<'a> {
    pub raw: &'a [u8],
    pub values: [u32; 17],
}

impl<'a> RegionParamBlock<'a> {
    pub const SIZE: usize = 68;

    pub fn parse(data: &'a [u8], offset: usize) -> Option<Self> {
        let raw = data.get(offset..offset.checked_add(Self::SIZE)?)?;
        let values = std::array::from_fn(|index| {
            let start = index * 4;
            u32::from_le_bytes(raw[start..start + 4].try_into().expect("row size is fixed"))
        });
        Some(Self { raw, values })
    }

    // SRC[loc=values[3] decomp=TODO conf=corpus→verified-by-default] default 0x1f40 = 8000.
    pub fn cutoff_raw(&self) -> u32 { self.values[3] }
    // SRC[loc=values[6], values[7] decomp=TODO conf=corpus→verified-by-default] 128 = centre.
    pub fn pan_l_raw(&self) -> u32 { self.values[6] }
    pub fn pan_r_raw(&self) -> u32 { self.values[7] }
    // SRC[loc=values[5],[9],[12],[16] decomp=TODO conf=corpus→verified-by-default] 12800 = 100%.
    pub fn levels_raw(&self) -> [u32; 4] {
        [self.values[5], self.values[9], self.values[12], self.values[16]]
    }

    /// Map FL's cutoff (0..12800, log scale) to SF2 absolute cents.
    // SRC[loc=n/a (our mapping) decomp=TODO conf=guess] FL 0 -> ~100 Hz, 12800 -> ~20 kHz.
    pub fn cutoff_sf2_cents(&self) -> i16 {
        let norm = (self.cutoff_raw() as f64 / 12800.0).clamp(0.0, 1.0);
        let hz = 100.0 * 200.0f64.powf(norm);
        (1200.0 * (hz / 8.176).log2()).clamp(1500.0, 13500.0) as i16
    }

    /// Approximate left/right pan using the observed 0..255 center-128 range.
    // SRC[loc=values[6],[7] decomp=TODO conf=guess] scale remains inferred.
    pub fn pan_sf2(&self) -> i16 {
        let avg = (self.pan_l_raw() as i64 + self.pan_r_raw() as i64) / 2;
        (((avg - 128) * 500) / 128).clamp(-500, 500) as i16
    }

    /// Approximate attenuation from word 5, in centibels.
    // SRC[loc=values[5] decomp=TODO conf=guess] assumes a linear 0..12800 scale.
    pub fn amp_attenuation_cb(&self) -> u16 {
        let level = self.values[5].min(12800) as f64 / 12800.0;
        if level <= 0.0 { return 960; }
        (-200.0 * level.log10()).clamp(0.0, 960.0) as u16
    }

    /// Decomp-verified conversion of the 11 words that feed the DLL's
    /// in-memory derived cache at record+0x44..+0x6C (`FUN_00882850`).
    ///
    /// These values are recomputed on every load/save and never appear on
    /// wire; they exist to drive UI drawing. Words 0, 1, 4, 8, 11, 15 do
    /// not feed this cache.
    ///
    /// Do NOT use these as substitutes for `cutoff_raw`/`pan_l_raw`/etc.
    /// above. This is a separate runtime-only view; the serialized meaning
    /// of each word is still only corpus-inferred.
    pub fn derived_caches(&self) -> RegionCaches {
        let v = &self.values;
        RegionCaches {
            // +0x44: (f32)w[2] * (1/6400)
            field_2: v[2] as f32 * INV_6400,
            // +0x48: curve_derived(w[3]) = (3^(w/8000) - 1) / 2, guard at 8000
            field_3: curve_derived(v[3]),
            // +0x4C: (f32)w[5] * (1/12800)
            field_5: v[5] as f32 * INV_12800,
            // +0x50: (f32)w[6] * (1/256)
            field_6: v[6] as f32 * INV_256,
            // +0x54: (f32)w[7] * (1/256)
            field_7: v[7] as f32 * INV_256,
            // +0x58: (f32)w[9] * (1/12800)
            field_9: v[9] as f32 * INV_12800,
            // +0x5C: (f32)w[10] (no scaling)
            field_10: v[10] as f32,
            // +0x60: (f32)w[12] * 8.0 * (1/12800)
            field_12: v[12] as f32 * 8.0 * INV_12800,
            // +0x64: (f32)w[13] * (1/12800)
            field_13: v[13] as f32 * INV_12800,
            // +0x68: (f32)w[14] * (1/12800)
            field_14: v[14] as f32 * INV_12800,
            // +0x6C: (f32)w[16] * 2.0 * (1/12800)
            field_16: v[16] as f32 * 2.0 * INV_12800,
        }
    }
}

/// All 11 derived cache floats computed from a region record.
/// Recomputed by `FUN_00882850` on every load/save; never serialized.
#[derive(Debug, Clone, Copy, Default)]
pub struct RegionCaches {
    pub field_2: f32, pub field_3: f32,
    pub field_5: f32, pub field_6: f32, pub field_7: f32,
    pub field_9: f32, pub field_10: f32,
    pub field_12: f32, pub field_13: f32, pub field_14: f32, pub field_16: f32,
}

// Bit-exact f32 reciprocals as stored in the DLL. Per state_format.md:
// "For bit-exact output multiply by those f32 values; dividing by 12800
//  or 6400 can differ in the last bit."
const INV_12800: f32 = f32::from_bits(0x38A3D70A); // ≈ 1/12800
const INV_6400:  f32 = f32::from_bits(0x3923D70A); // ≈ 1/6400
const INV_256:   f32 = f32::from_bits(0x3B800000); // = 1/256 exactly

/// Curve derivation for region word 3 (from FUN_00882850).
///
///     curve_derived(c) = (3^(c/8000) - 1) / 2, with c == 8000 → 1.0
///
/// Range: 0 at c=0, 1 at c=8000. Whether word 3 is a filter-cutoff curve,
/// an envelope shape parameter, or something else is unresolved; the DLL
/// only uses this to populate a UI drawing cache.
fn curve_derived(c: u32) -> f32 {
    if c == 8000 { return 1.0; }
    let t = c as f64 * 0.000125; // c / 8000; f64 intermediate as in DLL
    ((3.0f64.powf(t) - 1.0) * 0.5) as f32
}
