//! Chip register snapshots and their patch-file rendering.

#[derive(Clone, Copy, Debug)]
pub(crate) struct YmChannelRegs {
    pub(crate) fnum_lo: u8,
    pub(crate) fnum_hi: u8,
    /// Bits 0-5 = alg/fb (reg 0xB0), bits 6-7 = L/R (reg 0xB4).
    pub(crate) alg_fb_lr: u8,
    /// AMS (bits 4-5) and PMS (bits 0-2) from reg 0xB4.
    pub(crate) ams_pms: u8,
    pub(crate) mul_dt:  [u8; 4],
    pub(crate) tl:      [u8; 4],
    pub(crate) ar_ks:   [u8; 4],
    pub(crate) dr_am:   [u8; 4],
    pub(crate) sr:      [u8; 4],
    pub(crate) sl_rr:   [u8; 4],
    pub(crate) ssg_eg:  [u8; 4],
}

impl Default for YmChannelRegs {
    fn default() -> Self {
        Self {
            fnum_lo: 0,
            fnum_hi: 0,
            // Pan bits (6-7) initialize to 1 per hardware power-on state.
            // Games that never write 0xB4 depend on this (After Burner II,
            // etc.). Reference: jsgroth YM2612 Part 5; BlastEm sets
            // channels[i].lr = 0xC0 at init ("some games seem to expect
            // that the LR flags start out as 1").
            alg_fb_lr: 0xC0,
            ams_pms: 0,
            mul_dt: [0; 4],
            tl: [0; 4],
            ar_ks: [0; 4],
            dr_am: [0; 4],
            sr: [0; 4],
            sl_rr: [0; 4],
            ssg_eg: [0; 4],
        }
    }
}

/// Active-operator mask per algorithm. Bit set = operator is inert for
/// the current algorithm, so TL on that operator does not affect the
/// sound and must not be part of instrument identity.
/// Ported from vgm2pre `SubYM2612::slot()`:
///   slotMap[8] = {0x08,0x08,0x08,0x08,0x0c,0x0e,0x0e,0x0f}
fn slot_for_algo(algo: u8) -> u8 {
    const SLOT_MAP: [u8; 8] = [0x08, 0x08, 0x08, 0x08, 0x0c, 0x0e, 0x0e, 0x0f];
    SLOT_MAP[(algo & 7) as usize]
}

fn tl_matters_for(algo: u8, op: usize) -> bool {
    (slot_for_algo(algo) & (1 << op)) == 0
}

impl YmChannelRegs {
    pub(crate) fn apply(&mut self, reg: u8, val: u8, port: u8) {
        if let Some((_ch, op)) = classify_op_register(reg, port) {
            match reg & 0xF0 {
                0x30 => self.mul_dt[op]  = val,
                0x40 => self.tl[op]      = val,
                0x50 => self.ar_ks[op]   = val,
                0x60 => self.dr_am[op]   = val,
                0x70 => self.sr[op]      = val,
                0x80 => self.sl_rr[op]   = val,
                0x90 => self.ssg_eg[op]  = val,
                _ => {}
            }
            return;
        }
        let ch_in_port = (reg & 3) as usize;
        if ch_in_port > 2 { return; }
        // Match on `reg & 0xFC`, NOT `reg & 0xF0`. The 0xF0 mask collapses
        // 0xA4 into 0xA0 (so fnum_hi writes land in fnum_lo) and 0xB4 into
        // 0xB0 (so LR writes get clobbered by alg/fb).
        match reg & 0xFC {
            0xA0 => self.fnum_lo = val,
            0xA4 => self.fnum_hi = val,
            0xB0 => {
                // FB (bits 5-3), alg (bits 2-0). Preserve LR from 0xB4.
                self.alg_fb_lr = (self.alg_fb_lr & 0xC0) | (val & 0x3F);
            }
            0xB4 => {
                // LR (bits 7-6) into alg_fb_lr; AMS (5-4) and PMS (2-0) into
                // ams_pms. Reference: sub.ym2612.cpp `case 0xb0` handles
                // regs 0xB4-0xB6 with `pms = val&7; ams = (val>>4)&3`.
                self.alg_fb_lr = (self.alg_fb_lr & 0x3F) | (val & 0xC0);
                self.ams_pms = val & 0x3F;
            }
            _ => {}
        }
        let _ = port;
    }

    pub(crate) fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut acc = |b: u8| {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        };
        // Instrument identity per vgm2pre::compare():
        //   - alg + fb only (mask off pan bits 6-7; pan is not identity —
        //     pre.ym2612.hpp has no pan field).
        //   - ams_pms included (compare(use_ext_cmp) does so).
        //   - TL only on active operators (compare() uses slot(ins)).
        //   - `am` (bit 7 of dr_am) included because the YMFM backend
        //     applies operator AM enable to register 0x60.
        //   - Pitch (fnum/block) NOT hashed — pitch lives on the note.
        acc(self.alg_fb_lr & 0x3F);
        acc(self.ams_pms);
        let algo = self.alg_fb_lr & 7;
        for i in 0..4 {
            acc(self.mul_dt[i]);
            if tl_matters_for(algo, i) { acc(self.tl[i]); }
            acc(self.ar_ks[i]);
            acc(self.dr_am[i]);
            acc(self.sr[i]);
            acc(self.sl_rr[i]);
            acc(self.ssg_eg[i]);
        }
        h
    }

    pub(crate) fn to_patch_body(&self, name: &str, title: &str, source_name: &str) -> String {
        let mut s = String::new();
        s.push_str("patch 1\n");
        s.push_str(&format!("name \"{}\"\n", name));
        s.push_str("author \"lifted\"\n");
        s.push_str(&format!("license \"VGM {} — see source ROM\"\n", title));
        s.push_str(&format!("origin vgm:{} source:{}\n\n", title, source_name));
        s.push_str("requires fm.operator\n");
        s.push_str("requires pitch.fnum_block\n");
        s.push_str("requires amp.envelope\n");
        s.push_str("requires pan.lr\n\n");
        s.push_str("source fm.operator\n\n");
        s.push_str("native ym2612\n");
        // Pitch (fnum/block) is derived per note by adapter::dispatch_fm,
        // not baked into the patch. Reference: vgm2pre::compare() excludes
        // fnum/block; pre.ym2612.hpp has no pitch fields in ym2612_ch_t.
        s.push_str(&format!("  alg {}\n", self.alg_fb_lr & 7));
        s.push_str(&format!("  fb {}\n", (self.alg_fb_lr >> 3) & 7));
        // Pan is emitted as-is. No `lr==0 → 3` rescue; a channel with pan=0
        // is silenced, matching hardware and jsgroth Part 5.
        let lr = (self.alg_fb_lr >> 6) & 3;
        s.push_str(&format!("  lr {}\n", lr));
        s.push_str(&format!("  pms {}\n", self.ams_pms & 7));
        s.push_str(&format!("  ams {}\n", (self.ams_pms >> 4) & 3));
        for op in 0..4 {
            s.push_str(&format!("  mul{} {}\n", op + 1, self.mul_dt[op] & 0x0F));
            s.push_str(&format!("  dt{} {}\n",  op + 1, (self.mul_dt[op] >> 4) & 7));
            s.push_str(&format!("  tl{} {}\n",  op + 1, self.tl[op] & 0x7F));
            // rs / ar / am / dr / sr / rr / sl / ssg all emitted per
            // saveVGI/saveTFI/saveTYI/saveEIF/saveDMP in vgm2pre.
            s.push_str(&format!("  rs{} {}\n",  op + 1, (self.ar_ks[op] >> 6) & 3));
            s.push_str(&format!("  ar{} {}\n",  op + 1, self.ar_ks[op] & 0x1F));
            s.push_str(&format!("  am{} {}\n",  op + 1, (self.dr_am[op] >> 7) & 1));
            s.push_str(&format!("  dr{} {}\n",  op + 1, self.dr_am[op] & 0x1F));
            s.push_str(&format!("  sr{} {}\n",  op + 1, self.sr[op] & 0x1F));
            s.push_str(&format!("  rr{} {}\n",  op + 1, self.sl_rr[op] & 0x0F));
            s.push_str(&format!("  sl{} {}\n",  op + 1, (self.sl_rr[op] >> 4) & 0x0F));
            s.push_str(&format!("  ssg{} {}\n", op + 1, self.ssg_eg[op] & 0x0F));
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PsgRegs {
    pub(crate) is_noise: bool,
    pub(crate) tone: u16,
    pub(crate) vol: u8,
    pub(crate) noise_ctl: u8,
}

impl PsgRegs {
    pub(crate) fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        // Pitch and attenuation are note automation, not instrument identity.
        let bytes: Vec<u8> = vec![self.is_noise as u8, self.noise_ctl];
        for &b in &bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    pub(crate) fn to_patch_body(&self, name: &str, title: &str, source_name: &str) -> String {
        let src = if self.is_noise { "noise.lfsr" } else { "tone.square" };
        let mut s = String::new();
        s.push_str("patch 1\n");
        s.push_str(&format!("name \"{}\"\n", name));
        s.push_str("author \"lifted\"\n");
        s.push_str(&format!("license \"VGM {} — see source ROM\"\n", title));
        s.push_str(&format!("origin vgm:{} source:{}\n\n", title, source_name));
        s.push_str(&format!("requires {}\n", src));
        if !self.is_noise {
            s.push_str("requires pitch.divider\n");
        }
        s.push_str("requires amp.attenuation\n\n");
        s.push_str(&format!("source {}\n\n", src));
        s.push_str("native sn76489\n");
        if self.is_noise {
            s.push_str(&format!("  noise_rate {}\n", self.noise_ctl & 3));
            s.push_str(&format!("  noise_fb {}\n",   (self.noise_ctl >> 2) & 1));
        }
        s.push('\n');
        s
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TimbreSnapshot {
    Ym(YmChannelRegs),
    Psg(PsgRegs),
    /// Raw-sample DAC channel. Playback is handled by
    /// `hosting::chip::dac::DacState`, which bypasses the adapter.
    /// This variant exists so the halotext carries a DAC channel;
    /// the adapter path (requires sample.pcm) is a dead-end fallback.
    Dac,
}

impl TimbreSnapshot {
    pub(crate) fn hash(&self) -> u64 {
        match self {
            TimbreSnapshot::Ym(r)  => r.hash(),
            TimbreSnapshot::Psg(r) => r.hash(),
            TimbreSnapshot::Dac    => 0xDA_CAFE_BABE_0001u64,
        }
    }

    pub(crate) fn to_patch_body(&self, name: &str, title: &str, source_name: &str) -> String {
        match self {
            TimbreSnapshot::Ym(r)  => r.to_patch_body(name, title, source_name),
            TimbreSnapshot::Psg(r) => r.to_patch_body(name, title, source_name),
            TimbreSnapshot::Dac    => {
                let mut s = String::new();
                s.push_str("patch 1\n");
                s.push_str(&format!("name \"{}\"\n", name));
                s.push_str("author \"lifted\"\n");
                s.push_str(&format!("license \"VGM {} — see source ROM\"\n", title));
                s.push_str(&format!("origin vgm:{} source:{}\n\n", title, source_name));
                s.push_str("requires sample.pcm\n\n");
                s.push_str("source sample.pcm\n");
                s
            }
        }
    }
}

pub(crate) fn classify_op_register(reg: u8, port: u8) -> Option<(usize, usize)> {
    let base = reg & 0xF0;
    if !matches!(base, 0x30 | 0x40 | 0x50 | 0x60 | 0x70 | 0x80 | 0x90) {
        return None;
    }
    let ch_in_port = (reg & 3) as usize;
    if ch_in_port > 2 { return None; }
    let op = ((reg >> 2) & 3) as usize;
    let ch = if port == 0 { ch_in_port } else { ch_in_port + 3 };
    Some((ch, op))
}
