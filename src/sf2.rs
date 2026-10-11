//! Minimal SoundFont 2 (SF2) reader.
//!
//! Parses pdta sections and exposes a flat list of zones.  Each zone has a
//! key range, a velocity range, a sample reference, and tuning.  The player
//! picks the last matching zone (later zones override earlier ones).
//!
//! Limitations:
//!   * Preset structure is ignored; all instrument zones are flattened
//!     into one list.  Good enough for single-instrument banks; wrong for
//!     GM banks where many presets share instrument zones.
//!   * Modulators (pmod/imod) are ignored.
//!   * No volume envelope, filter, or pan generators applied yet.

use anyhow::{bail, Context, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Sf2Sample {
    pub name: String,
    pub start: u32,
    pub end: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub rate: u32,
    pub root_key: u8,
    pub loop_mode: u8,
}

#[derive(Debug, Clone)]
pub struct Sf2Zone {
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    pub sample_idx: usize,
    pub tune_cents: i16,
    pub root_override: Option<u8>,
    pub start_offset: i32,
    pub end_offset: i32,
    pub loop_start_offset: i32,
    pub loop_end_offset: i32,
    pub loop_mode: u8,
}

#[derive(Debug)]
pub struct Sf2File {
    pub samples: Vec<Sf2Sample>,
    pub zones: Vec<Sf2Zone>,
    pub pcm: Vec<i16>,
}

impl Sf2File {
    pub fn load(path: &Path) -> Result<Self> {
        let data =
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&data).with_context(|| format!("parsing {}", path.display()))
    }

    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"sfbk" {
            bail!("not an SF2 (missing RIFF/sfbk)");
        }
        let sdta = find_list(data, b"sdta").ok_or_else(|| anyhow::anyhow!("no sdta"))?;
        let pdta = find_list(data, b"pdta").ok_or_else(|| anyhow::anyhow!("no pdta"))?;

        let smpl = find_chunk(sdta, b"smpl").ok_or_else(|| anyhow::anyhow!("no smpl"))?;
        let shdr = find_chunk(pdta, b"shdr").ok_or_else(|| anyhow::anyhow!("no shdr"))?;
        let inst = find_chunk(pdta, b"inst").ok_or_else(|| anyhow::anyhow!("no inst"))?;
        let ibag = find_chunk(pdta, b"ibag").ok_or_else(|| anyhow::anyhow!("no ibag"))?;
        let igen = find_chunk(pdta, b"igen").ok_or_else(|| anyhow::anyhow!("no igen"))?;

        // ── smpl → PCM frames (i16 LE) ─────────────────────────────────
        if smpl.len() % 2 != 0 {
            bail!("smpl size is odd");
        }
        let mut pcm = Vec::with_capacity(smpl.len() / 2);
        for c in smpl.chunks_exact(2) {
            pcm.push(i16::from_le_bytes([c[0], c[1]]));
        }

        // ── shdr → samples ─────────────────────────────────────────────
        if shdr.len() < 46 * 2 || shdr.len() % 46 != 0 {
            bail!("shdr size not multiple of 46");
        }
        let n_samples = shdr.len() / 46;
        let mut samples = Vec::with_capacity(n_samples);
        for i in 0..n_samples {
            let r = &shdr[i * 46..i * 46 + 46];
            let name = cstr(&r[0..20]);
            if name == "EOS" {
                break;
            }
            let start = u32::from_le_bytes([r[20], r[21], r[22], r[23]]);
            let end = u32::from_le_bytes([r[24], r[25], r[26], r[27]]);
            let loop_start = u32::from_le_bytes([r[28], r[29], r[30], r[31]]);
            let loop_end = u32::from_le_bytes([r[32], r[33], r[34], r[35]]);
            let rate = u32::from_le_bytes([r[36], r[37], r[38], r[39]]);
            let root_key = r[40];
            samples.push(Sf2Sample {
                name,
                start,
                end,
                loop_start,
                loop_end,
                rate,
                root_key,
                loop_mode: 0,
            });
        }

        // ── inst → ibag ────────────────────────────────────────────────
        // Each inst entry (22 bytes) has a u16 at offset 20 pointing into
        // ibag.  Consecutive entries delimit this instrument's zone range.
        if inst.len() % 22 != 0 {
            bail!("inst size not multiple of 22");
        }
        let n_inst = inst.len() / 22;
        let mut inst_bag_idx: Vec<usize> = Vec::with_capacity(n_inst);
        for i in 0..n_inst {
            let r = &inst[i * 22..i * 22 + 22];
            inst_bag_idx.push(u16::from_le_bytes([r[20], r[21]]) as usize);
        }

        // ── ibag → igen ────────────────────────────────────────────────
        if ibag.len() % 4 != 0 {
            bail!("ibag size not multiple of 4");
        }
        let n_ibag = ibag.len() / 4;
        let mut ibag_gen_idx: Vec<usize> = Vec::with_capacity(n_ibag);
        for i in 0..n_ibag {
            let r = &ibag[i * 4..i * 4 + 4];
            ibag_gen_idx.push(u16::from_le_bytes([r[0], r[1]]) as usize);
        }

        // ── igen generators ────────────────────────────────────────────
        if igen.len() % 4 != 0 {
            bail!("igen size not multiple of 4");
        }
        let mut igens: Vec<(u16, u16)> = Vec::with_capacity(igen.len() / 4);
        for i in 0..igen.len() / 4 {
            let r = &igen[i * 4..i * 4 + 4];
            igens.push((
                u16::from_le_bytes([r[0], r[1]]),
                        u16::from_le_bytes([r[2], r[3]]),
            ));
        }

        // ── Walk instrument zones ──────────────────────────────────────
        // The last inst entry is EOI.  For instrument ii:
        //   ibag range: [inst_bag_idx[ii], inst_bag_idx[ii+1])
        //   igen range: [ibag_gen_idx[ibag_start], ibag_gen_idx[ibag_end])
        // Within that igen slice, zones are separated by sampleID (op 53).
        let mut zones: Vec<Sf2Zone> = Vec::new();
        for ii in 0..n_inst.saturating_sub(1) {
            let bag_lo = inst_bag_idx[ii];
            let bag_hi = inst_bag_idx[ii + 1];
            if bag_lo >= ibag_gen_idx.len() || bag_hi > ibag_gen_idx.len() {
                continue;
            }
            let gen_start = ibag_gen_idx[bag_lo];
            let gen_end = if bag_hi < ibag_gen_idx.len() {
                ibag_gen_idx[bag_hi]
            } else {
                igens.len()
            };
            if gen_start >= gen_end || gen_end > igens.len() {
                continue;
            }
            walk_igens(&igens, gen_start, gen_end, &samples, &mut zones);
        }

        if zones.is_empty() {
            bail!("no instrument zones found");
        }

        // ── Propagate zone loop_mode into the sample ───────────────────
        for z in &zones {
            if z.loop_mode != 0 {
                if let Some(s) = samples.get_mut(z.sample_idx) {
                    s.loop_mode = z.loop_mode;
                }
            }
        }

        if std::env::var_os("SF2_DEBUG").is_some() {
            eprintln!("sf2: {} samples, {} zones", samples.len(), zones.len());
            for (i, z) in zones.iter().take(30).enumerate() {
                eprintln!(
                    "  zone {:>3}: key {:>3}..{:<3} vel {:>3}..{:<3} sample {:>4} root {:?} tune {}",
                    i, z.key_lo, z.key_hi, z.vel_lo, z.vel_hi, z.sample_idx,
                    z.root_override, z.tune_cents
                );
            }
        }

        Ok(Sf2File { samples, zones, pcm })
    }
}

fn walk_igens(
    igens: &[(u16, u16)],
              gen_start: usize,
              gen_end: usize,
              samples: &[Sf2Sample],
              out: &mut Vec<Sf2Zone>,
) {
    // A zone ends with a sampleID generator (op 53).  The generators
    // before it apply to that sample.  See SF2 spec §7.3.
    let mut cur = ZoneAccum::default();
    for &(op, val) in &igens[gen_start..gen_end] {
        if op == 53 {
            let sidx = val as usize;
            if sidx < samples.len() {
                out.push(cur.finish(sidx, &samples[sidx]));
            }
            cur = ZoneAccum::default();
        } else {
            cur.apply(op, val);
        }
    }
}

#[derive(Default)]
struct ZoneAccum {
    key_lo: u8,
    key_hi: u8,
    vel_lo: u8,
    vel_hi: u8,
    coarse_tune: i16,
    fine_tune: i16,
    root_override: Option<u8>,
    start_offset: i32,
    end_offset: i32,
    loop_start_offset: i32,
    loop_end_offset: i32,
    start_coarse: i32,
    end_coarse: i32,
    loop_start_coarse: i32,
    loop_end_coarse: i32,
    loop_mode: u8,
    saw_key: bool,
    saw_vel: bool,
}

impl ZoneAccum {
    fn apply(&mut self, op: u16, val: u16) {
        match op {
            0 => self.start_offset = val as i16 as i32,
            1 => self.end_offset = val as i16 as i32,
            2 => self.loop_start_offset = val as i16 as i32,
            3 => self.loop_end_offset = val as i16 as i32,
            4 => self.start_coarse = val as i16 as i32,
            12 => self.end_coarse = val as i16 as i32,
            45 => self.loop_start_coarse = val as i16 as i32,
            50 => self.loop_end_coarse = val as i16 as i32,
            43 => {
                self.key_lo = (val & 0xFF) as u8;
                self.key_hi = (val >> 8) as u8;
                self.saw_key = true;
            }
            44 => {
                self.vel_lo = (val & 0xFF) as u8;
                self.vel_hi = (val >> 8) as u8;
                self.saw_vel = true;
            }
            51 => self.coarse_tune = val as i8 as i16,
            52 => self.fine_tune = val as i8 as i16,
            54 => self.loop_mode = (val & 0x03) as u8,
            58 => self.root_override = Some((val as i8).clamp(0, 127) as u8),
            _ => {}
        }
    }

    fn finish(&self, sample_idx: usize, sample: &Sf2Sample) -> Sf2Zone {
        Sf2Zone {
            key_lo: if self.saw_key { self.key_lo } else { 0 },
            key_hi: if self.saw_key { self.key_hi } else { 127 },
            vel_lo: if self.saw_vel { self.vel_lo } else { 0 },
            vel_hi: if self.saw_vel { self.vel_hi } else { 127 },
            sample_idx,
            tune_cents: self.coarse_tune.saturating_mul(100) + self.fine_tune,
            root_override: self.root_override.or(Some(sample.root_key)),
            start_offset: self.start_offset + self.start_coarse * 32768,
            end_offset: self.end_offset + self.end_coarse * 32768,
            loop_start_offset: self.loop_start_offset + self.loop_start_coarse * 32768,
            loop_end_offset: self.loop_end_offset + self.loop_end_coarse * 32768,
            loop_mode: self.loop_mode,
        }
    }
}

// ── RIFF helpers ───────────────────────────────────────────────────────

fn cstr(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

fn find_list<'a>(data: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    if data.len() < 12 || &data[0..4] != b"RIFF" {
        return None;
    }
    let mut p = 12;
    while p + 8 <= data.len() {
        let id = &data[p..p + 4];
        let size =
        u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]) as usize;
        let body = p + 8;
        if body + size > data.len() {
            break;
        }
        if id == b"LIST" && size >= 4 {
            let ty = &data[body..body + 4];
            if ty == want {
                return Some(&data[body + 4..body + size]);
            }
        }
        p = body + size + (size & 1);
    }
    None
}

fn find_chunk<'a>(data: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    let mut p = 0;
    while p + 8 <= data.len() {
        let id = &data[p..p + 4];
        let size =
        u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]) as usize;
        let body = p + 8;
        if body + size > data.len() {
            break;
        }
        if id == want {
            return Some(&data[body..body + size]);
        }
        p = body + size + (size & 1);
    }
    None
}
