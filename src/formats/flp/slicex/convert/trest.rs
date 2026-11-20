//! Typed reader for the Slicex `trest` state block.
//!
//! The block is not a stream of envelopes. In the observed v3 state layout it
//! contains a 76-byte prefix, four tag-6 envelope records, a count and table of
//! 68-byte region records, then eight native groups. Each group contains a
//! 20-byte group header and six articulators; each articulator has a 16-byte
//! header followed by six envelopes (tags 1, 2, 3, 3, 3, 3).
//!
//! The parser follows that framing and requires the final group to end at EOF.
//! It does not scan for tag-like integers in unknown bytes. Unknown headers and
//! envelope tails are retained verbatim for callers that need round-tripping.

use super::params::RegionParamBlock;

const PREFIX_LEN: usize = 76;
const LEADING_ENVELOPES: usize = 4;
const REGION_COUNT_OFFSET: usize = 396;
const REGION_ROW_SIZE: usize = 68;
const GROUP_COUNT: usize = 8;
const GROUP_HEADER_SIZE: usize = 20;
const ARTICULATORS_PER_GROUP: usize = 6;
const ARTICULATOR_HEADER_SIZE: usize = 16;
const ENVELOPES_PER_ARTICULATOR: usize = 6;

#[derive(Debug, Clone, Copy)]
pub struct EnvelopePoint {
    pub dx: f64,
    pub y: f64,
    /// First four bytes of the eight-byte point tail. Kept as raw bits because
    /// its interpretation depends on the envelope type.
    pub curve: f32,
    pub tail: [u8; 8],
}

impl EnvelopePoint {
    /// Segment shape kind (runtime `+0x10`).
    pub fn kind(&self) -> u8 { self.tail[4] }
    /// Segment tension, widened from f32.
    pub fn tension(&self) -> f64 { self.curve as f64 }
    /// Streamed point attribute at +0x11. Session 6 identifies this as a cycle flag.
    pub fn cycle_flag(&self) -> u8 { self.tail[5] }
    /// Streamed attribute at +0x12; meaning remains unresolved.
    pub fn opaque_attr(&self) -> u8 { self.tail[6] }
    /// Streamed +0x13 byte; precompute/editor state, meaning is not fully settled.
    pub fn linear_attr(&self) -> u8 { self.tail[7] }
}

/// The 12 data bytes in a version-3 articulator header after its version word.
#[derive(Debug, Clone, Copy)]
pub struct ItemHeader {
    pub base: f32,
    pub env_amt: f32,
    pub lfo_amt: f32,
}

impl Default for ItemHeader {
    fn default() -> Self {
        Self { base: 0.0, env_amt: 1.0, lfo_amt: 0.5 }
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
    /// Five little-endian i32 values in the 20-byte config trailer.
    pub params: [i32; 5],
    /// Tag-specific bytes after config, kept intact.
    pub extra: &'a [u8],
    /// Tag 1's four u32 multipliers, each represented in 1/128 units.
    pub duration_multipliers: Option<[u32; 4]>,
    /// Index of the owning group; `None` for the four leading tag-6 records.
    pub group_index: Option<usize>,
    /// Index of the articulator within its group; `None` for leading records.
    pub articulator_index: Option<usize>,
    /// Index in the six-envelope articulator sequence; `None` for leading records.
    pub envelope_index: Option<usize>,
    pub size: usize,
}

impl<'a> RegionRecord<'a> {
    pub fn enabled(&self) -> bool { self.params[1] != 0 }
    pub fn sustain_index(&self) -> i32 { self.params[2] }
    pub fn loop_start_index(&self) -> i32 { self.params[3] }
    /// End marker; note-off jumps here, then advances through release points.
    pub fn end_release_index(&self) -> i32 { self.params[4] }
    /// Config bit 0 selects beat-synchronized timing (H: 0x795560).
    pub fn is_synced(&self) -> bool { self.params[0] & 1 != 0 }
    /// Session 6 corpus confirms tag 2's first extra u32 is the LFO rate knob.
    pub fn lfo_rate_knob(&self) -> Option<u32> {
        if self.tag != 2 { return None; }
        Some(u32::from_le_bytes(self.extra.get(..4)?.try_into().ok()?))
    }
    /// Convert the tag-1 integer multipliers from their stored 1/128 units.
    pub fn duration_multipliers(&self) -> Option<[f32; 4]> {
        self.duration_multipliers.map(|v| v.map(|n| n as f32 / 128.0))
    }
}

#[derive(Debug, Clone)]
pub struct GroupHeader<'a> {
    pub offset: usize,
    pub raw: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct ArticulatorHeader<'a> {
    pub offset: usize,
    pub raw: &'a [u8],
    pub version: u32,
}

#[derive(Debug)]
pub struct Trest<'a> {
    pub raw: &'a [u8],
    /// Four leading tag-6 records followed by the 288 group envelope records.
    pub records: Vec<RegionRecord<'a>>,
    /// Raw 20-byte headers for the eight groups.
    pub groups: Vec<GroupHeader<'a>>,
    /// Raw 16-byte headers for the 48 articulators.
    pub articulators: Vec<ArticulatorHeader<'a>>,
    /// Count and first native 68-byte row, used by the existing conversion path.
    /// The complete row table is available through `region_rows`.
    pub region_count: u32,
    pub region_rows: Vec<RegionParamBlock<'a>>,
    pub params: Option<RegionParamBlock<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at trest+0x{:X}", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

fn fail(offset: usize, message: &'static str) -> ParseError {
    ParseError { offset, message }
}

fn read_u32(raw: &[u8], offset: usize) -> Result<u32, ParseError> {
    let bytes = raw.get(offset..offset.checked_add(4).ok_or_else(|| fail(offset, "offset overflow"))?)
        .ok_or_else(|| fail(offset, "truncated u32"))?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("slice length checked")))
}

fn parse_envelope<'a>(
    raw: &'a [u8], offset: usize, expected_tag: Option<u32>,
    group_index: Option<usize>, articulator_index: Option<usize>, envelope_index: Option<usize>,
) -> Result<(RegionRecord<'a>, usize), ParseError> {
    let tag = read_u32(raw, offset)?;
    let version = read_u32(raw, offset + 4)?;
    let count_u32 = read_u32(raw, offset + 8)?;
    if let Some(expected) = expected_tag {
        if tag != expected { return Err(fail(offset, "unexpected envelope tag")); }
    }
    if !matches!(tag, 1 | 2 | 3 | 6) { return Err(fail(offset, "unsupported envelope tag")); }
    if version != 3 { return Err(fail(offset + 4, "unsupported envelope version")); }
    let count = usize::try_from(count_u32).map_err(|_| fail(offset + 8, "point count overflow"))?;
    let extra_len = match tag { 1 => 16, 2 => 20, _ => 0 };
    let points_len = count.checked_mul(24).ok_or_else(|| fail(offset + 8, "point data length overflow"))?;
    let size = 12usize.checked_add(points_len).and_then(|n| n.checked_add(20))
        .and_then(|n| n.checked_add(extra_len)).ok_or_else(|| fail(offset, "envelope size overflow"))?;
    let end = offset.checked_add(size).ok_or_else(|| fail(offset, "envelope end overflow"))?;
    if end > raw.len() { return Err(fail(offset, "envelope extends past trest end")); }

    let mut points = Vec::with_capacity(count);
    for index in 0..count {
        let p = offset + 12 + index * 24;
        let dx = f64::from_le_bytes(raw[p..p + 8].try_into().expect("point bounds checked"));
        let y = f64::from_le_bytes(raw[p + 8..p + 16].try_into().expect("point bounds checked"));
        let tail: [u8; 8] = raw[p + 16..p + 24].try_into().expect("point bounds checked");
        let curve = f32::from_le_bytes(tail[..4].try_into().expect("tail size is fixed"));
        points.push(EnvelopePoint { dx, y, curve, tail });
    }
    let config_offset = offset + 12 + points_len;
    let config = &raw[config_offset..config_offset + 20];
    let params = std::array::from_fn(|i| {
        i32::from_le_bytes(config[i * 4..i * 4 + 4].try_into().expect("config size is fixed"))
    });
    let extra = &raw[config_offset + 20..end];
    let duration_multipliers = (tag == 1).then(|| std::array::from_fn(|i| {
        u32::from_le_bytes(extra[i * 4..i * 4 + 4].try_into().expect("tag-1 extra size is fixed"))
    }));

    Ok((RegionRecord {
        offset, tag, version, count: count_u32, points, config, params, extra,
        duration_multipliers, group_index, articulator_index, envelope_index, size,
    }, end))
}

impl<'a> Trest<'a> {
    pub fn parse(raw: &'a [u8]) -> Result<Self, ParseError> {
        if raw.len() < REGION_COUNT_OFFSET + 4 {
            return Err(fail(raw.len(), "trest is too short for the region count"));
        }

        // These leading records are in the 320-byte area following the 76-byte
        // prefix. Validate their framing instead of looking for tag-shaped data.
        let mut records = Vec::with_capacity(LEADING_ENVELOPES + GROUP_COUNT * ARTICULATORS_PER_GROUP * ENVELOPES_PER_ARTICULATOR);
        let mut pos = PREFIX_LEN;
        for _ in 0..LEADING_ENVELOPES {
            let (record, end) = parse_envelope(raw, pos, Some(6), None, None, None)?;
            records.push(record);
            pos = end;
        }
        if pos != REGION_COUNT_OFFSET {
            return Err(fail(pos, "leading records do not end at the region-count field"));
        }

        let region_count = read_u32(raw, REGION_COUNT_OFFSET)?;
        let row_count = usize::try_from(region_count).map_err(|_| fail(REGION_COUNT_OFFSET, "region count overflow"))?;
        let rows_len = row_count.checked_mul(REGION_ROW_SIZE)
            .ok_or_else(|| fail(REGION_COUNT_OFFSET, "region table size overflow"))?;
        let rows_start = REGION_COUNT_OFFSET + 4;
        let rows_end = rows_start.checked_add(rows_len)
            .ok_or_else(|| fail(rows_start, "region table end overflow"))?;
        if rows_end > raw.len() { return Err(fail(rows_start, "region table extends past trest end")); }

        let mut region_rows = Vec::with_capacity(row_count);
        for index in 0..row_count {
            let row_offset = rows_start + index * REGION_ROW_SIZE;
            let row = RegionParamBlock::parse(raw, row_offset)
                .ok_or_else(|| fail(row_offset, "truncated region row"))?;
            region_rows.push(row);
        }
        let params = region_rows.first().cloned();

        let expected_records = [1, 2, 3, 3, 3, 3];
        let mut groups = Vec::with_capacity(GROUP_COUNT);
        let mut articulators = Vec::with_capacity(GROUP_COUNT * ARTICULATORS_PER_GROUP);
        pos = rows_end;
        for group_index in 0..GROUP_COUNT {
            let group_end = pos.checked_add(GROUP_HEADER_SIZE)
                .ok_or_else(|| fail(pos, "group header end overflow"))?;
            let group_raw = raw.get(pos..group_end).ok_or_else(|| fail(pos, "truncated group header"))?;
            groups.push(GroupHeader { offset: pos, raw: group_raw });
            pos = group_end;

            for articulator_index in 0..ARTICULATORS_PER_GROUP {
                let art_end = pos.checked_add(ARTICULATOR_HEADER_SIZE)
                    .ok_or_else(|| fail(pos, "articulator header end overflow"))?;
                let art_raw = raw.get(pos..art_end).ok_or_else(|| fail(pos, "truncated articulator header"))?;
                let version = u32::from_le_bytes(art_raw[..4].try_into().expect("header length checked"));
                if version != 3 { return Err(fail(pos, "unsupported articulator header version")); }
                articulators.push(ArticulatorHeader { offset: pos, raw: art_raw, version });
                pos = art_end;

                for (envelope_index, &expected_tag) in expected_records.iter().enumerate() {
                    let (record, end) = parse_envelope(
                        raw, pos, Some(expected_tag), Some(group_index),
                        Some(articulator_index), Some(envelope_index),
                    )?;
                    records.push(record);
                    pos = end;
                }
            }
        }
        if pos != raw.len() { return Err(fail(pos, "unexpected bytes after final native group")); }

        Ok(Self { raw, records, groups, articulators, region_count, region_rows, params })
    }

    fn body(&self) -> Vec<&RegionRecord<'a>> {
        self.records.iter().filter(|r| r.group_index.is_some()).collect()
    }

    /// Six envelopes for one group/articulator target. Group is 0..8 and
    /// target is 0..6 (VOL, CUT, RES, SPEED, etc.).
    pub fn group(&self, group: usize, target: usize) -> Option<Vec<&RegionRecord<'a>>> {
        if group >= GROUP_COUNT || target >= ARTICULATORS_PER_GROUP { return None; }
        let body = self.body();
        let first = (group * ARTICULATORS_PER_GROUP + target) * ENVELOPES_PER_ARTICULATOR;
        let records = body.get(first..first + ENVELOPES_PER_ARTICULATOR)?;
        Some(records.to_vec())
    }

    pub fn target_env(&self, group: usize, target: usize) -> Option<&RegionRecord<'a>> {
        self.group(group, target)?.first().copied()
    }

    /// 12-byte payload of the selected articulator header (base, env_amt, lfo_amt).
    pub fn item_header(&self, group: usize, target: usize) -> Option<ItemHeader> {
        let offset = (group * ARTICULATORS_PER_GROUP + target).checked_mul(1)?;
        let header = self.articulators.get(offset)?;
        let bytes = header.raw.get(4..16)?;
        Some(ItemHeader {
            base: f32::from_le_bytes(bytes[0..4].try_into().ok()?),
            env_amt: f32::from_le_bytes(bytes[4..8].try_into().ok()?),
            lfo_amt: f32::from_le_bytes(bytes[8..12].try_into().ok()?),
        })
    }

    /// VOL envelope of group 0, target 1 (enabled, at least one point).
    pub fn amp_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_VOL).filter(|r| r.enabled() && r.count >= 1)
    }

    /// SPEED envelope of group 0 (target 4).
    pub fn speed_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_SPEED).filter(|r| r.enabled() && r.count >= 2)
    }

    /// CUT envelope of group 0 (target 2).
    pub fn filter_envelope(&self) -> Option<&RegionRecord<'a>> {
        self.target_env(0, TARGET_CUT).filter(|r| r.enabled() && r.count >= 2)
    }

    /// LFO record of the selected group/target, if enabled.
    pub fn lfo_envelope(&self, group: usize, target: usize) -> Option<&RegionRecord<'a>> {
        self.group(group, target)?.get(1).copied().filter(|r| r.enabled())
    }
}

// Target indices (PAN 0, VOL 1, CUT 2, RES 3, SPEED 4, START 5).
pub const TARGET_VOL: usize = 1;
pub const TARGET_CUT: usize = 2;
pub const TARGET_SPEED: usize = 4;
