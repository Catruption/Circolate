//! 96-byte outer header at the start of a Slicex state blob.
//!
//! Bytes 0..95.  Most are constants; the five that vary across the
//! corpus are exposed as named fields.  All names are UNVERIFIED
//! except where noted.
//!
//! ## Provenance
//!
//! SRC[loc=state+0x00 .. state+0x60 decomp=TODO conf=verified for the layout, corpus for the field names]
//!
//! These 96 bytes are not one structure in the DLL.  By slicex.rs they are
//! version (u32) + block20 (0x20) + main (0x2F, this+0x12F0) + header12
//! (3 x i32) = 95 bytes, plus slot 0's flag byte at 95.  Field map, derived by
//! arithmetic (main starts at state+0x24, header12 at state+0x53):
//!
//!   sub_tab       raw[60]  main[0x18]   just after main[0x14] (selected art group)
//!   toggle_71     raw[71]  main[0x23]
//!   auto_dump     raw[74]  main[0x26]
//!   has_override  raw[83]  header12[0], low byte
//!   last_slice    raw[87]  header12[1], low byte
//!
//! The names are guesses from toggling things in the UI.  To check each one: find
//! the plugin's SaveRestoreState writer, see which member it stores at that
//! position, and what that member does.  decomp=TODO for all five.

#[derive(Debug, Clone)]
pub struct OuterHeader<'a> {
    pub raw: &'a [u8],
    /// Envelope editor sub-tab (0 = ENV, 1 = LFO).  UNVERIFIED.
    pub sub_tab: u8,
    /// Unknown toggle (byte 71).  UNVERIFIED.
    pub toggle_71: u8,
    /// AUTO-DUMP (1 = on).  UNVERIFIED.
    pub auto_dump: u8,
    /// Per-slice sample override present.  UNVERIFIED.
    pub has_override: u8,
    /// Last / selected slice index.  UNVERIFIED.
    pub last_slice: u8,
}

impl<'a> OuterHeader<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 0x60 {
            return None;
        }
        let raw = &data[..0x60];
        Some(Self {
            raw,
            sub_tab: raw[60],
            toggle_71: raw[71],
            auto_dump: raw[74],
            has_override: raw[83],
            last_slice: raw[87],
        })
    }
}
