//! The 402-byte settings header that follows the embedded WAV.
//!
//! Layout:
//!   +0    f64   sentinel (-1.0)
//!   +8    f64   sentinel (-1.0)
//!   +16   f64   0.0
//!   +24   f64   sample-derived value (duration × 2 in beats)
//!   +32   f32   0.02
//!   +36   u32   filter type (0 = LP, 2 = HP)
//!   +40.. +396  five 80-byte blocks with durations at their start
//!   +396  u32   slice count
//!
//! ## Provenance
//!
//! SRC[loc=slot0.params (0x4C) + slot0 envelopes + slot0 count decomp=TODO conf=derived]
//!
//! LEGACY VIEW.  This "settings header" is really three things in a row, per
//! slicex.rs: the 0x4C-byte slot params, four envelope records, then the slice
//! count.  The arithmetic that shows it:
//!
//!   +0x00 .. +0x4C   slot params   (sel start/end, view range, threshold 0.02 at 0x20,
//!                                   filter type u32 at 0x24, channel view at 0x2C)
//!   +76              envelope 0    kind, version 3, count 2, 2 x 24-byte points, 20 trailer = 80 bytes
//!   +156 / +236 / +316  envelopes 1..3
//!   +396             u32 slice count (76 + 4 x 80), first record at +400
//!
//! The "block durations" at 24, 112, 192, 272, 352 are: params+0x18 (view range
//! end, frames / 22050) and then the x (segment length) of point 1 in each of the
//! four envelopes.  Only valid while all four envelopes have 2 points; the header
//! length is NOT fixed (the fixed 402 in mod.rs is a DECOMP-CHECK, see there).
//!
//! filter_type (+36 = params+0x24, 0 = LP, 2 = HP) is the one field here that
//! slicex.rs does not expose.  decomp=TODO: find the member stored at slot params
//! +0x24 and the code that switches on it.

#[derive(Debug, Clone)]
pub struct SettingsHeader<'a> {
    pub raw: &'a [u8],
    pub sentinel_a: f64,
    pub sentinel_b: f64,
    pub zero_a: f64,
    pub duration_x2: f64,
    pub rate32: f32,
    pub filter_type: u32,
    pub block_durations: [f64; 5],
    pub slice_count: u32,
}

impl<'a> SettingsHeader<'a> {
    pub fn parse(raw: &'a [u8]) -> Option<Self> {
        if raw.len() < 402 {
            return None;
        }
        Some(Self {
            raw,
            sentinel_a: f64::from_le_bytes(raw[0..8].try_into().unwrap()),
             sentinel_b: f64::from_le_bytes(raw[8..16].try_into().unwrap()),
             zero_a: f64::from_le_bytes(raw[16..24].try_into().unwrap()),
             duration_x2: f64::from_le_bytes(raw[24..32].try_into().unwrap()),
             rate32: f32::from_le_bytes(raw[32..36].try_into().unwrap()),
             filter_type: u32::from_le_bytes(raw[36..40].try_into().unwrap()),
             block_durations: [
                 f64::from_le_bytes(raw[24..32].try_into().unwrap()),
             f64::from_le_bytes(raw[112..120].try_into().unwrap()),
             f64::from_le_bytes(raw[192..200].try_into().unwrap()),
             f64::from_le_bytes(raw[272..280].try_into().unwrap()),
             f64::from_le_bytes(raw[352..360].try_into().unwrap()),
             ],
             slice_count: u32::from_le_bytes(raw[396..400].try_into().unwrap()),
        })
    }
}
