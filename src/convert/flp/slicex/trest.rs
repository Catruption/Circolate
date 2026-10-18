//! The ~25 KB `trest` block: a sequence of region records.
//!
//! Record format:
//!
//! ```text
//! u32 tag        (6 = articulator region, 1/2/3 = marker env)
//! u32 version    (always 3)
//! u32 count
//! count × ( f64 dx, f64 y, f32 curve, u8[4] pad )
//! u8[20] config
//! ```
//!
//! Records are not contiguous — the 108-byte articulator param block and
//! occasional 16-byte padding sit between them.  We scan forward four
//! bytes at a time until a valid header appears.
//!
//! ## Provenance
//!
//! SRC[loc=slot1.params .. end of state decomp=TODO conf=derived]
//!
//! mod.rs hands `Trest::parse` the bytes after wav2, i.e. slot 1 from its params
//! onward (slicex.rs layout).  So the offsets in this file mean:
//!
//!   trest+0      slot 1 params (0x4C)
//!   trest+76     slot 1 envelopes x4, kind 6, 80 bytes each at 2 points
//!                = the "four leading tag=6 records";  REGION_START = 76 skips the params
//!   trest+396    slot 1 slice count (= 1)      \
//!   trest+400    slot 1 slice record (0x44)     +-- the "108-byte param block" (params.rs)
//!   trest+468    first 36 bytes of the tail    /
//!   trest+504 ...  8 arts x 6 targets x 6 envelope records = 288 records
//!                (292 = 4 + 288)
//!
//! Envelope record layout is verified (slicex.rs Envelope; the stream parses
//! cleanly through slot 1).  The scan here exists because the tail framing (0x14
//! art headers, 16-byte padding) is not decoded.  slicex.rs::scan_tail keeps the
//! gaps between records, which would show the real framing.
//!
//! To check in the DLL: the loader that reads an envelope (version u32 == 3, a
//! count, then 0x18 bytes per point, then 5 x i32) and the loop around it that
//! reads 6 per target; the order of the six records (ENV, LFO, VEL, MOD X, MOD Y,
//! RAND) and the 0x14 header should be visible in that loop.

use super::params::RegionParamBlock;

#[derive(Debug, Clone, Copy)]
pub struct EnvelopePoint {
    pub dx: f64,
    pub y: f64,
    pub curve: f32,
    pub tail: [u8; 8],
}

#[derive(Debug)]
pub struct RegionRecord<'a> {
    pub offset: usize,
    pub tag: u32,
    pub version: u32,
    pub count: u32,
    pub points: Vec<EnvelopePoint>,
    pub config: &'a [u8],
    pub size: usize,
}

#[derive(Debug)]
pub struct Trest<'a> {
    pub raw: &'a [u8],
    pub records: Vec<RegionRecord<'a>>,
    pub params: Option<RegionParamBlock<'a>>,
}

const MAX_POINTS: usize = 500;
// SRC[loc=slot1.params end (0x4C) decomp=n/a conf=derived]  76 = size of the slot params, see header.
const REGION_START: usize = 76;

impl<'a> Trest<'a> {
    pub fn parse(raw: &'a [u8]) -> Self {
        let mut records = Vec::new();
        let mut pos = REGION_START;

        while pos + 12 <= raw.len() {
            let mut header_off = None;
            let mut scan = pos;
            while scan + 12 <= raw.len() {
                let tag = u32::from_le_bytes(raw[scan..scan+4].try_into().unwrap());
                let ver = u32::from_le_bytes(raw[scan+4..scan+8].try_into().unwrap());
                let cnt = u32::from_le_bytes(raw[scan+8..scan+12].try_into().unwrap()) as usize;
                let tag_ok = matches!(tag, 1 | 2 | 3 | 6);
                if ver == 3 && tag_ok && cnt > 0 && cnt <= MAX_POINTS {
                    let size = 12 + cnt * 24 + 20;
                    if scan + size <= raw.len() {
                        header_off = Some(scan);
                        break;
                    }
                }
                scan += 4;
            }
            let Some(off) = header_off else { break };

            let tag = u32::from_le_bytes(raw[off..off+4].try_into().unwrap());
            let version = u32::from_le_bytes(raw[off+4..off+8].try_into().unwrap());
            let count = u32::from_le_bytes(raw[off+8..off+12].try_into().unwrap()) as usize;

            let mut points = Vec::with_capacity(count);
            for k in 0..count {
                let p = off + 12 + k * 24;
                let dx = f64::from_le_bytes(raw[p..p+8].try_into().unwrap());
                let y = f64::from_le_bytes(raw[p+8..p+16].try_into().unwrap());
                let tail: [u8; 8] = raw[p+16..p+24].try_into().unwrap();
                let curve = f32::from_le_bytes(tail[0..4].try_into().unwrap());
                points.push(EnvelopePoint { dx, y, curve, tail });
            }

            let config_off = off + 12 + count * 24;
            let config = &raw[config_off..config_off + 20];

            records.push(RegionRecord {
                offset: off,
                tag,
                version,
                count: count as u32,
                points,
                config,
                size: 12 + count * 24 + 20,
            });

            pos = config_off + 20;
        }

        // SRC[loc=trest+396 decomp=FUN_008827d0 conf=derived]  see params.rs: probably slot 1's count + slice record
        let params = RegionParamBlock::parse(raw, 396);
        Trest { raw, records, params }
    }

    /// Records excluding the four leading tag=6 records, in file order.
    fn body(&self) -> Vec<&RegionRecord<'a>> {
        self.records.iter().filter(|r| r.tag != 6).collect()
    }

    /// Slicex stores 8 articulators x 6 targets (PAN, VOL, CUT, RES, SPEED,
    /// START) = 48 groups of 6 records: tag1 = ENV, tag2 = LFO, tag3 x4 =
    /// VEL, MOD X, MOD Y, RAND.  (Matches the editor tabs; 292 = 4 + 48*6.)
    /// Returns the six records of one group, or None if the layout doesn't
    /// look like that.
    // SRC[loc=tail, record index (art * 6 + target) * 6 decomp=TODO conf=corpus]
    // The group order (PAN VOL CUT RES SPEED START) and record order were matched to the editor tabs, not read
    // from the DLL.  Only art 0 has been seen in use.
    pub fn group(&self, art: usize, target: usize) -> Option<Vec<&RegionRecord<'a>>> {
        let body = self.body();
        if body.len() % 6 != 0 || target >= 6 {
            return None;
        }
        let i = (art * 6 + target) * 6;
        let g = body.get(i..i + 6)?;
        let ok = g[0].tag == 1 && g[1].tag == 2 && g[2..].iter().all(|r| r.tag == 3);
        ok.then(|| g.to_vec())
    }

    /// ENV record of articulator `art`, target `target`.
    pub fn target_env(&self, art: usize, target: usize) -> Option<&RegionRecord<'a>> {
        self.group(art, target).map(|g| g[0])
    }

    /// VOL envelope of articulator 1 (every slice uses ART 1 in the files
    /// seen so far - UNVERIFIED for projects that assign other ARTs).
    pub fn amp_envelope(&self) -> Option<&RegionRecord<'a>> {
        if let Some(g) = self.group(0, TARGET_VOL) {
            return Some(g[0]).filter(|r| r.count >= 2);
        }
        // Unknown layout: old heuristic.
        self.records.iter().find(|r| r.tag == 1 && r.count >= 2)
    }

    /// SPEED envelope (playback speed => pitch + duration).  y = 0.5 is
    /// normal speed; a single flat point means "untouched".
    pub fn speed_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_SPEED).filter(|r| r.count >= 2)
    }

    /// CUT envelope (UNVERIFIED depth/range; only used with
    /// FLP_SLICEX_APPLY_FILTER_ENV).
    pub fn filter_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_CUT).filter(|r| r.count >= 2)
    }
}

// SRC[loc=tail target index decomp=TODO conf=corpus]  VOL = 1, CUT = 2, SPEED = 4 (PAN 0, RES 3, START 5)
pub const TARGET_VOL: usize = 1;
pub const TARGET_CUT: usize = 2;
pub const TARGET_SPEED: usize = 4;
