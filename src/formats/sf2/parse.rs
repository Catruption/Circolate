//! SF2 — RIFF walking and zone extraction.
//!
//! Pure except `load*` wrappers, which are marked TODO to move to `cli/`
//! once `cli/` grows a file-reading surface.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::formats::sf2::model::{Sf2File, Sf2Sample, Sf2Zone};
use crate::formats::sf2::spec::chunks::*;

// ── public entry points ───────────────────────────────────────────────

impl Sf2File {
    /// TODO: move to `cli/`. Reads from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let data =
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&data, None)
    }

    /// TODO: move to `cli/`. Reads from disk.
    pub fn load_preset(path: &Path, bank: u16, preset: u16) -> Result<Self> {
        let data =
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&data, Some((bank, preset)))
    }

    pub fn load_preset_bytes(data: &[u8], bank: u16, preset: u16) -> Result<Self> {
        Self::parse(data, Some((bank, preset)))
    }

    pub fn load_bytes(data: &[u8]) -> Result<Self> {
        Self::parse(data, None)
    }

    /// Convenience: `parse_file` = `load`, named for symmetry with flp/it/midi.
    pub fn parse_file(path: &Path) -> Result<Self> {
        Self::load(path)
    }

    pub fn parse(data: &[u8], want: Option<(u16, u16)>) -> Result<Self> {
        if data.len() < 12 || &data[0..4] != &RIFF || &data[8..12] != &SFBK {
            bail!("not an SF2 (missing RIFF/sfbk)");
        }
        let sdta = find_list(data, &SDTA).ok_or_else(|| anyhow::anyhow!("no sdta"))?;
        let pdta = find_list(data, &PDTA).ok_or_else(|| anyhow::anyhow!("no pdta"))?;

        let smpl = find_chunk(sdta, &SMPL).ok_or_else(|| anyhow::anyhow!("no smpl"))?;
        let shdr = find_chunk(pdta, &SHDR).ok_or_else(|| anyhow::anyhow!("no shdr"))?;
        let inst = find_chunk(pdta, &INST).ok_or_else(|| anyhow::anyhow!("no inst"))?;
        let ibag = find_chunk(pdta, &IBAG).ok_or_else(|| anyhow::anyhow!("no ibag"))?;
        let igen = find_chunk(pdta, &IGEN).ok_or_else(|| anyhow::anyhow!("no igen"))?;

        let phdr = find_chunk(pdta, &PHDR);
        let pbag = find_chunk(pdta, &PBAG);
        let pgen = find_chunk(pdta, &PGEN);

        if smpl.len() % 2 != 0 {
            bail!("smpl size is odd");
        }
        let mut pcm = Vec::with_capacity(smpl.len() / 2);
        for c in smpl.chunks_exact(2) {
            pcm.push(i16::from_le_bytes([c[0], c[1]]));
        }

        if shdr.len() < SHDR_RECORD_SIZE * 2 || shdr.len() % SHDR_RECORD_SIZE != 0 {
            bail!("shdr size not multiple of {}", SHDR_RECORD_SIZE);
        }
        let n_samples = shdr.len() / SHDR_RECORD_SIZE;
        let mut samples = Vec::with_capacity(n_samples);
        for i in 0..n_samples {
            let r = &shdr[i * SHDR_RECORD_SIZE..i * SHDR_RECORD_SIZE + SHDR_RECORD_SIZE];
            let name = cstr(&r[0..20]);
            if name == TERMINAL_NAME {
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

        if inst.len() % INST_RECORD_SIZE != 0 {
            bail!("inst size not multiple of {}", INST_RECORD_SIZE);
        }
        let n_inst = inst.len() / INST_RECORD_SIZE;
        let mut inst_bag_idx: Vec<usize> = Vec::with_capacity(n_inst);
        for i in 0..n_inst {
            let r = &inst[i * INST_RECORD_SIZE..i * INST_RECORD_SIZE + INST_RECORD_SIZE];
            inst_bag_idx.push(u16::from_le_bytes([r[20], r[21]]) as usize);
        }

        if ibag.len() % IBAG_RECORD_SIZE != 0 {
            bail!("ibag size not multiple of {}", IBAG_RECORD_SIZE);
        }
        let n_ibag = ibag.len() / IBAG_RECORD_SIZE;
        let mut ibag_gen_idx: Vec<usize> = Vec::with_capacity(n_ibag);
        for i in 0..n_ibag {
            let r = &ibag[i * IBAG_RECORD_SIZE..i * IBAG_RECORD_SIZE + IBAG_RECORD_SIZE];
            ibag_gen_idx.push(u16::from_le_bytes([r[0], r[1]]) as usize);
        }

        if igen.len() % IGEN_RECORD_SIZE != 0 {
            bail!("igen size not multiple of {}", IGEN_RECORD_SIZE);
        }
        let mut igens: Vec<(u16, u16)> = Vec::with_capacity(igen.len() / IGEN_RECORD_SIZE);
        for i in 0..igen.len() / IGEN_RECORD_SIZE {
            let r = &igen[i * IGEN_RECORD_SIZE..i * IGEN_RECORD_SIZE + IGEN_RECORD_SIZE];
            igens.push((
                u16::from_le_bytes([r[0], r[1]]),
                u16::from_le_bytes([r[2], r[3]]),
            ));
        }

        // Filter to a single preset if requested.
        let mut inst_filter: Option<Vec<usize>> = None;
        if let Some((want_bank, want_preset)) = want {
            if let (Some(phdr), Some(pbag), Some(pgen)) = (phdr, pbag, pgen) {
                if phdr.len() % PHDR_RECORD_SIZE == 0
                    && pbag.len() % PBAG_RECORD_SIZE == 0
                    && pgen.len() % PGEN_RECORD_SIZE == 0
                {
                    let n_phdr = phdr.len() / PHDR_RECORD_SIZE;
                    let n_pbag = pbag.len() / PBAG_RECORD_SIZE;
                    let n_pgen = pgen.len() / PGEN_RECORD_SIZE;

                    let mut phdr_entries: Vec<(u16, u16, usize)> = Vec::with_capacity(n_phdr);
                    for i in 0..n_phdr {
                        let r = &phdr[i * PHDR_RECORD_SIZE..i * PHDR_RECORD_SIZE + PHDR_RECORD_SIZE];
                        let name = cstr(&r[0..20]);
                        if name == PRESET_TERMINAL_NAME {
                            break;
                        }
                        let p = u16::from_le_bytes([r[20], r[21]]);
                        let b = u16::from_le_bytes([r[22], r[23]]);
                        let bi = u16::from_le_bytes([r[24], r[25]]) as usize;
                        phdr_entries.push((p, b, bi));
                    }

                    let mut pbag_gen_idx: Vec<usize> = Vec::with_capacity(n_pbag);
                    for i in 0..n_pbag {
                        let r = &pbag[i * PBAG_RECORD_SIZE..i * PBAG_RECORD_SIZE + PBAG_RECORD_SIZE];
                        pbag_gen_idx.push(u16::from_le_bytes([r[0], r[1]]) as usize);
                    }

                    for pi in 0..phdr_entries.len() {
                        let (p, b, bag_lo) = phdr_entries[pi];
                        if p != want_preset || b != want_bank {
                            continue;
                        }
                        let bag_hi = if pi + 1 < phdr_entries.len() {
                            phdr_entries[pi + 1].2
                        } else {
                            pbag_gen_idx.len()
                        };
                        if bag_lo >= pbag_gen_idx.len() || bag_hi > pbag_gen_idx.len() {
                            break;
                        }
                        let gen_start = pbag_gen_idx[bag_lo];
                        let gen_end = if bag_hi < pbag_gen_idx.len() {
                            pbag_gen_idx[bag_hi]
                        } else {
                            n_pgen
                        };
                        if gen_start >= gen_end || gen_end > n_pgen {
                            break;
                        }
                        let mut wanted_insts: Vec<usize> = Vec::new();
                        for gi in gen_start..gen_end {
                            let r = &pgen[gi * PGEN_RECORD_SIZE..gi * PGEN_RECORD_SIZE + PGEN_RECORD_SIZE];
                            let op = u16::from_le_bytes([r[0], r[1]]);
                            let val = u16::from_le_bytes([r[2], r[3]]);
                            if op == GEN_INSTRUMENT {
                                wanted_insts.push(val as usize);
                            }
                        }
                        inst_filter = Some(wanted_insts);
                        break;
                    }
                }
            }
        }

        let mut zones: Vec<Sf2Zone> = Vec::new();
        for ii in 0..n_inst.saturating_sub(1) {
            if let Some(ref filter) = inst_filter {
                if !filter.contains(&ii) {
                    continue;
                }
            }
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

        for z in &zones {
            if z.loop_mode != 0 {
                if let Some(s) = samples.get_mut(z.sample_idx) {
                    s.loop_mode = z.loop_mode;
                }
            }
        }

        if std::env::var_os("SF2_DEBUG").is_some() {
            eprintln!(
                "sf2: {} samples, {} zones{}",
                samples.len(),
                zones.len(),
                match want {
                    Some((b, p)) => format!(" (bank {} preset {})", b, p),
                    None => String::new(),
                }
            );
            for (i, z) in zones.iter().take(30).enumerate() {
                eprintln!(
                    "  zone {:>3}: key {:>3}..{:<3} vel {:>3}..{:<3} sample {:>4} \
                     root {:?} tune {} atten {} a={:.3} h={:.3} d={:.3} s={:.3} r={:.3}",
                    i, z.key_lo, z.key_hi, z.vel_lo, z.vel_hi, z.sample_idx,
                    z.root_override, z.tune_cents, z.initial_attenuation_cb,
                    crate::formats::sf2::spec::units::timecents_to_secs(z.attack_timecents),
                    crate::formats::sf2::spec::units::timecents_to_secs(z.hold_timecents),
                    crate::formats::sf2::spec::units::timecents_to_secs(z.decay_timecents),
                    crate::formats::sf2::spec::units::attenuation_cb_to_linear(z.sustain_level_cb),
                    crate::formats::sf2::spec::units::timecents_to_secs(z.release_timecents),
                );
            }
        }

        Ok(Sf2File { samples, zones, pcm })
    }
}

// ── internals ────────────────────────────────────────────────────────

fn walk_igens(
    igens: &[(u16, u16)],
    gen_start: usize,
    gen_end: usize,
    samples: &[Sf2Sample],
    out: &mut Vec<Sf2Zone>,
) {
    let mut cur = ZoneAccum::default();
    for &(op, val) in &igens[gen_start..gen_end] {
        if op == GEN_SAMPLE_ID {
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
    initial_attenuation_cb: i16,
    delay_timecents: i16,
    attack_timecents: i16,
    hold_timecents: i16,
    decay_timecents: i16,
    sustain_level_cb: i16,
    release_timecents: i16,
    initial_filter_fc: i16,
    initial_filter_q: i16,
}

impl Default for ZoneAccum {
    fn default() -> Self {
        ZoneAccum {
            key_lo: 0,
            key_hi: 127,
            vel_lo: 0,
            vel_hi: 127,
            coarse_tune: 0,
            fine_tune: 0,
            root_override: None,
            start_offset: 0,
            end_offset: 0,
            loop_start_offset: 0,
            loop_end_offset: 0,
            start_coarse: 0,
            end_coarse: 0,
            loop_start_coarse: 0,
            loop_end_coarse: 0,
            loop_mode: 0,
            saw_key: false,
            saw_vel: false,
            initial_attenuation_cb: 0,
            delay_timecents: -12000,
            attack_timecents: -12000,
            hold_timecents: -12000,
            decay_timecents: -12000,
            sustain_level_cb: 0,
            release_timecents: -12000,
            initial_filter_fc: 13500,
            initial_filter_q: 0,
        }
    }
}

impl ZoneAccum {
    fn apply(&mut self, op: u16, val: u16) {
        match op {
            GEN_START_ADDRS1 => self.start_offset = val as i16 as i32,
            GEN_END_ADDRS1 => self.end_offset = val as i16 as i32,
            GEN_STARTLOOP_ADDRS1 => self.loop_start_offset = val as i16 as i32,
            GEN_ENDLOOP_ADDRS1 => self.loop_end_offset = val as i16 as i32,
            GEN_START_ADDRS2 => self.start_coarse = val as i16 as i32,
            GEN_INITIAL_FILTER_FC => self.initial_filter_fc = val as i16,
            GEN_INITIAL_FILTER_Q => self.initial_filter_q = val as i16,
            GEN_END_ADDRS2 => self.end_coarse = val as i16 as i32,
            GEN_DELAY_MOD_ENV => self.delay_timecents = val as i16,
            GEN_ATTACK_MOD_ENV => self.attack_timecents = val as i16,
            GEN_HOLD_MOD_ENV => self.hold_timecents = val as i16,
            GEN_DECAY_MOD_ENV => self.decay_timecents = val as i16,
            GEN_SUSTAIN_MOD_ENV => self.sustain_level_cb = val as i16,
            GEN_RELEASE_MOD_ENV => self.release_timecents = val as i16,
            GEN_KEY_RANGE => {
                self.key_lo = (val & 0xFF) as u8;
                self.key_hi = (val >> 8) as u8;
                self.saw_key = true;
            }
            GEN_VEL_RANGE => {
                self.vel_lo = (val & 0xFF) as u8;
                self.vel_hi = (val >> 8) as u8;
                self.saw_vel = true;
            }
            GEN_STARTLOOP_ADDRS2 => self.loop_start_coarse = val as i16 as i32,
            GEN_INITIAL_ATTENUATION => self.initial_attenuation_cb = val as i16,
            GEN_ENDLOOP_ADDRS2 => self.loop_end_coarse = val as i16 as i32,
            GEN_COARSE_TUNE => self.coarse_tune = val as i8 as i16,
            GEN_FINE_TUNE => self.fine_tune = val as i8 as i16,
            GEN_SAMPLE_MODES => self.loop_mode = (val & 0x03) as u8,
            GEN_OVERRIDING_ROOT_KEY => self.root_override = Some((val as i8).clamp(0, 127) as u8),
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
            initial_attenuation_cb: self.initial_attenuation_cb,
            delay_timecents: self.delay_timecents,
            attack_timecents: self.attack_timecents,
            hold_timecents: self.hold_timecents,
            decay_timecents: self.decay_timecents,
            sustain_level_cb: self.sustain_level_cb,
            release_timecents: self.release_timecents,
            initial_filter_fc: self.initial_filter_fc,
            initial_filter_q: self.initial_filter_q,
        }
    }
}

fn cstr(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

fn find_list<'a>(data: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    if data.len() < 12 || &data[0..4] != &RIFF {
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
        if id == &LIST && size >= 4 {
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