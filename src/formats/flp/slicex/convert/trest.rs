//! The ~25 KB `trest` block: a sequence of region records.
//!
//! Record format:
//!
//! ```text
//! u32 tag        (6 = articulator region, 1 = ENV, 2 = LFO, 3 = VEL/MOD/RAND)
//! u32 version    (always 3)
//! u32 count
//! count × ( f64 dx, f64 y, f32 curve, u8[4] attrs )
//! u8[20] config  → 5 × i32 params; params[1] = enabled
//! ```
//!
//! Records are not contiguous — the 108-byte articulator param block and
//! occasional 16-byte padding sit between them.  We scan forward four
//! bytes at a time until a valid header appears.

use super::params::RegionParamBlock;

#[derive(Debug, Clone, Copy)]
pub struct EnvelopePoint {
    pub dx: f64,
    pub y: f64,
    pub curve: f32,
    pub tail: [u8; 8],
}

impl EnvelopePoint {
    /// Segment shape kind (runtime `+0x10`).
    pub fn kind(&self) -> u8 {
        self.tail[4]
    }
    /// Segment tension, widened from f32.
    pub fn tension(&self) -> f64 {
        self.curve as f64
    }
}

/// 12-byte header preceding each articulator's ENV record:
/// `TArticulator+0x20/+0x24/+0x28` = (base, env_amt, lfo_amt) f32.
/// Defaults from `TArticulator` reset (0x796e00).
#[derive(Debug, Clone, Copy)]
pub struct ItemHeader {
    pub base: f32,
    pub env_amt: f32,
    pub lfo_amt: f32,
}

impl Default for ItemHeader {
    fn default() -> Self {
        ItemHeader { base: 0.0, env_amt: 1.0, lfo_amt: 0.5 }
    }
}

#[derive(Debug)]
pub struct RegionRecord<'a> {
    pub offset: usize,
    pub tag: u32,
    pub version: u32,
    pub count: u32,
    pub points: Vec<EnvelopePoint>,
    pub config: &'a [u8],
    /// 5 × i32 from the config trailer.  `params[1]` is "enabled".
    pub params: [i32; 5],
    pub size: usize,
}

impl<'a> RegionRecord<'a> {
    pub fn enabled(&self) -> bool {
        self.params[1] != 0
    }
    pub fn sustain_index(&self) -> i32 { self.params[2] }
    pub fn release_index(&self) -> i32 { self.params[3] }
    pub fn end_index(&self) -> i32 { self.params[4] }
}

#[derive(Debug)]
pub struct Trest<'a> {
    pub raw: &'a [u8],
    pub records: Vec<RegionRecord<'a>>,
    pub params: Option<RegionParamBlock<'a>>,
}

const MAX_POINTS: usize = 500;
const REGION_START: usize = 76;

impl<'a> Trest<'a> {
    pub fn parse(raw: &'a [u8]) -> Self {
        let mut records = Vec::new();
        let mut pos = REGION_START;

        while pos + 12 <= raw.len() {
            let mut header_off = None;
            let mut scan = pos;
            while scan + 12 <= raw.len() {
                let tag = u32::from_le_bytes(raw[scan..scan + 4].try_into().unwrap());
                let ver = u32::from_le_bytes(raw[scan + 4..scan + 8].try_into().unwrap());
                let cnt = u32::from_le_bytes(raw[scan + 8..scan + 12].try_into().unwrap()) as usize;
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

            let tag = u32::from_le_bytes(raw[off..off + 4].try_into().unwrap());
            let version = u32::from_le_bytes(raw[off + 4..off + 8].try_into().unwrap());
            let count = u32::from_le_bytes(raw[off + 8..off + 12].try_into().unwrap()) as usize;

            let mut points = Vec::with_capacity(count);
            for k in 0..count {
                let p = off + 12 + k * 24;
                let dx = f64::from_le_bytes(raw[p..p + 8].try_into().unwrap());
                let y = f64::from_le_bytes(raw[p + 8..p + 16].try_into().unwrap());
                let tail: [u8; 8] = raw[p + 16..p + 24].try_into().unwrap();
                let curve = f32::from_le_bytes(tail[0..4].try_into().unwrap());
                points.push(EnvelopePoint { dx, y, curve, tail });
            }

            let config_off = off + 12 + count * 24;
            let config = &raw[config_off..config_off + 20];
            let params: [i32; 5] = std::array::from_fn(|i| {
                i32::from_le_bytes(config[i * 4..i * 4 + 4].try_into().unwrap())
            });

            records.push(RegionRecord {
                offset: off,
                tag,
                version,
                count: count as u32,
                points,
                config,
                params,
                size: 12 + count * 24 + 20,
            });

            pos = config_off + 20;
        }

        let params = RegionParamBlock::parse(raw, 396);
        Trest { raw, records, params }
    }

    fn body(&self) -> Vec<&RegionRecord<'a>> {
        self.records.iter().filter(|r| r.tag != 6).collect()
    }

    /// Six records of one articulator+target (ENV, LFO, VEL, MOD X, MOD Y, RAND).
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

    pub fn target_env(&self, art: usize, target: usize) -> Option<&RegionRecord<'a>> {
        self.group(art, target).map(|g| g[0])
    }

    /// 12-byte header (base, env_amt, lfo_amt) preceding the ENV record.
    pub fn item_header(&self, art: usize, target: usize) -> Option<ItemHeader> {
        let r = self.target_env(art, target)?;
        self.item_header_of(r)
    }

    fn item_header_of(&self, r: &RegionRecord<'a>) -> Option<ItemHeader> {
        let hdr_off = r.offset.checked_sub(12)?;
        let hdr = self.raw.get(hdr_off..r.offset)?;
        if hdr.len() < 12 {
            return None;
        }
        Some(ItemHeader {
            base: f32::from_le_bytes(hdr[0..4].try_into().ok()?),
            env_amt: f32::from_le_bytes(hdr[4..8].try_into().ok()?),
            lfo_amt: f32::from_le_bytes(hdr[8..12].try_into().ok()?),
        })
    }

    /// VOL envelope of articulator 0 (enabled, ≥1 point).
    pub fn amp_envelope(&self) -> Option<&RegionRecord<'a>> {
        if let Some(g) = self.group(0, TARGET_VOL) {
            if g[0].enabled() && g[0].count >= 1 {
                return Some(g[0]);
            }
        }
        self.records.iter().find(|r| r.tag == 1 && r.enabled() && r.count >= 1)
    }

    /// SPEED envelope of articulator 0 (target 4).
    pub fn speed_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_SPEED)
            .filter(|r| r.enabled() && r.count >= 2)
    }

    /// CUT envelope of articulator 0 (target 2).
    pub fn filter_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_CUT)
            .filter(|r| r.enabled() && r.count >= 2)
    }

    /// LFO record of articulator `art` / target, if enabled.
    pub fn lfo_envelope(&self, art: usize, target: usize) -> Option<&RegionRecord<'a>> {
        self.group(art, target)
            .and_then(|g| g.get(1).copied().filter(|r| r.enabled()))
    }
}

// Target indices (PAN 0, VOL 1, CUT 2, RES 3, SPEED 4, START 5).
pub const TARGET_VOL: usize = 1;
pub const TARGET_CUT: usize = 2;
pub const TARGET_SPEED: usize = 4;