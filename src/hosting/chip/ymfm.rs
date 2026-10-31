//! FM synthesis via `ymfm-sys`. Implements `ChipCore` by translating
//! chip-file field names (`mul1`, `tl3`, `alg`, `key_on`, …) into YM2612
//! register writes and pulling samples from the ymfm emulator.

use ymfm_sys::ffi::{self, ChipType};
use ymfm_sys::ChipPtr;

use crate::hosting::chip::engine::core::ChipCore;
use crate::hosting::chip::engine::runtime::ScheduledWrite;

const YM_SCALE: f32 = 3.0 / 32768.0;
const YM_VOICE_SCALE: f32 = YM_SCALE * (128.0 * 64.0 / (6.0 * 65.0));

/// Parse a field name like `mul1` or `ar3` into `(base, op_index)`.
/// `op_index` is 1..4, or None for channel-scope fields.
fn parse_op_field(field: &str) -> Option<(&str, Option<u8>)> {
    let bytes = field.as_bytes();
    if let Some(&last) = bytes.last() {
        if last.is_ascii_digit() && field.len() > 1 {
            let op = (last - b'0') as u8;
            if (1..=4).contains(&op) {
                return Some((&field[..field.len() - 1], Some(op)));
            }
        }
    }
    Some((field, None))
}

pub struct YmfmBackend {
    chip: ChipPtr,
    voice_samples: Vec<(String, f32)>,
    channels: usize,
    native_rate: f64,
    host_rate: f64,
    buf: Vec<i32>,
    voice_buf: Vec<i32>,
    shadow: [[u8; 0x100]; 2],
    pending: Vec<ScheduledWrite>,
    register_pending: Vec<RegisterWrite>,
    register_cursor: usize,
    dac_enabled: bool,
    cur_t: f64,
}

#[derive(Clone, Copy)]
struct RegisterWrite { t: f64, port: u8, reg: u8, value: u8 }

unsafe impl Send for YmfmBackend {}

impl YmfmBackend {
    pub fn new(clock: u32, host_rate: u32) -> Result<Self, String> {
        let chip = ffi::create_chip(ChipType::Ym2612, clock);
        if chip.is_null() {
            return Err("ymfm-sys returned null chip".into());
        }
        let channels = (chip.channels() as usize).max(1);
        let native_rate = (chip.sample_rate() as f64).max(1.0);
        Ok(Self {
            chip,
            voice_samples: (1..=6).map(|n| (format!("ch{}", n), 0.0)).collect(),
            channels, native_rate, host_rate: host_rate as f64,
            buf: vec![0; channels],
            voice_buf: vec![0; 12],
            shadow: [[0u8; 0x100]; 2],
            pending: Vec::new(),
            register_pending: Vec::new(), register_cursor: 0, dac_enabled: false,
           cur_t: 0.0,
        })
    }

    /// "ch1".."ch6" → `(bank, ch_in_bank)`. bank 0 = ch1-3, bank 1 = ch4-6.
    fn voice_to_bank_ch(voice: &str) -> Option<(usize, u8)> {
        let n = voice.strip_prefix("ch")?.parse::<u8>().ok()?;
        if !(1..=6).contains(&n) { return None; }
        let zero = n - 1;
        Some(((zero / 3) as usize, zero % 3))
    }

    /// Read-modify-write a packed register byte. `val` is masked to `bits_lo..=bits_hi`.
    fn write_packed(&mut self, bank: usize, reg: u8, bits_lo: u8, bits_hi: u8, val: u8) {
        let width = bits_hi.saturating_sub(bits_lo) + 1;
        let mask = if width >= 8 { 0xFFu8 } else { ((1u16 << width) - 1) as u8 };
        let old = self.shadow[bank][reg as usize];
        let merged = (old & !(mask << bits_lo)) | ((val & mask) << bits_lo);
        self.shadow[bank][reg as usize] = merged;
        let addr_port = (bank as u32) * 2;
        let data_port = addr_port + 1;
        self.write_all(addr_port, reg);
        self.write_all(data_port, merged);
    }

    /// Update a shadow register byte without touching the chip.
    fn set_bits(&mut self, bank: usize, reg: u8, bits_lo: u8, bits_hi: u8, val: u8) {
        let width = bits_hi.saturating_sub(bits_lo) + 1;
        let mask = if width >= 8 { 0xFFu8 } else { ((1u16 << width) - 1) as u8 };
        let old = self.shadow[bank][reg as usize];
        self.shadow[bank][reg as usize] = (old & !(mask << bits_lo)) | ((val & mask) << bits_lo);
    }

    /// Send the channel's frequency to the chip: 0xA4 (hi/block) first, then
    /// 0xA0 (lo), because the chip only applies the latched hi on a lo write.
    fn flush_pitch(&mut self, bank: usize, ch_in_bank: u8) {
        let hi_reg = 0xA4 + ch_in_bank;
        let lo_reg = 0xA0 + ch_in_bank;
        let hi = self.shadow[bank][hi_reg as usize] & 0x3F;
        let lo = self.shadow[bank][lo_reg as usize];
        let addr_port = (bank as u32) * 2;
        let data_port = addr_port + 1;
        self.write_all(addr_port, hi_reg);
        self.write_all(data_port, hi);
        self.write_all(addr_port, lo_reg);
        self.write_all(data_port, lo);
    }

    fn write_all(&mut self, port: u32, value: u8) {
        self.chip.pin_mut().write(port, value);
    }

    fn apply_field(&mut self, voice: &str, field: &str, val: u32) {
        // Key-on/off: write 0x28 (global register) with [op_mask][ch_selector].
        // ch_selector is 0-2 for ch1-3 and 4-6 for ch4-6 (skips 3).
        if field == "key_on" || field == "key" {
            let Some((bank, ch_in_bank)) = Self::voice_to_bank_ch(voice) else { return };
            let global_ch = (bank as u8) * 3 + ch_in_bank;
            let ch_selector = if global_ch < 3 { global_ch } else { global_ch + 1 };
            let key_val = ((val as u8) & 0xF0) | ch_selector;
            self.write_all(0, 0x28);
            self.write_all(1, key_val);
            return;
        }

        let Some((bank, ch_in_bank)) = Self::voice_to_bank_ch(voice) else { return };

        // Operator-scope: "mul1" → base=0x30, op=0, register=0x30+0*4+ch.
        if let Some((base_name, Some(op_idx))) = parse_op_field(field) {
            let base_reg: u16 = match base_name {
                "mul" | "dt" => 0x30,
                "tl"         => 0x40,
                "ar" | "rs"  => 0x50,
                "dr" | "am"  => 0x60,
                "sr"         => 0x70,
                "rr" | "sl"  => 0x80,
                "ssg_eg"     => 0x90,
                _ => return,
            };
            let reg = (base_reg + ((op_idx - 1) as u16 * 4) + ch_in_bank as u16) as u8;
            let (lo, hi) = match base_name {
                "mul"    => (0, 3),
                "dt"     => (4, 6),   // FIX: was (0, 3), corrupting MUL
                "tl"     => (0, 6),
                "ar" | "dr" | "sr" => (0, 4),
                "rs" => (6, 7),
                "am" => (7, 7),
                "rr"     => (0, 3),
                "sl"     => (4, 7),   // FIX: was (0, 3), corrupting RR
                "ssg_eg" => (0, 3),
                _ => return,
            };
            self.write_packed(bank, reg, lo, hi, val as u8);
            return;
        }

        // Channel-scope fields.
        match field {
            "fnum" => {
                // The YM2612 latches the 0xA4 write (block + fnum high bits)
                // and applies it when 0xA0 is written. Update the shadow
                // bytes, then flush hi-then-lo so the new pitch is complete.
                let fnum = val as u16;
                self.set_bits(bank, 0xA0 + ch_in_bank, 0, 7, (fnum & 0xFF) as u8);
                self.set_bits(bank, 0xA4 + ch_in_bank, 0, 2, ((fnum >> 8) & 0x07) as u8);
                self.flush_pitch(bank, ch_in_bank);
            }
            "block" => {
                self.set_bits(bank, 0xA4 + ch_in_bank, 3, 5, val as u8);
                self.flush_pitch(bank, ch_in_bank);
            }
            // Bits 6-7 of 0xA4 are not a real register field on the YM2612.
            "key_scale" => {}
            "alg" => {
                let reg = (0xB0 + ch_in_bank as u16) as u8;
                self.write_packed(bank, reg, 0, 2, val as u8);
            }
            "fb" => {
                let reg = (0xB0 + ch_in_bank as u16) as u8;
                self.write_packed(bank, reg, 3, 5, val as u8);
            }
            "pms" => {
                let reg = (0xB4 + ch_in_bank as u16) as u8;
                self.write_packed(bank, reg, 0, 2, val as u8);
            }
            "ams" => {
                let reg = (0xB4 + ch_in_bank as u16) as u8;
                self.write_packed(bank, reg, 4, 5, val as u8);
            }
            "lr" => {
                // LR lives on 0xB4 bits 6-7, NOT 0xB0. Writing to 0xB0 is a
                // no-op on YM2612 (those bits are unused).
                let v = if (val as u8) == 0 { 3 } else { val as u8 };
                let reg = (0xB4 + ch_in_bank as u16) as u8;
                self.write_packed(bank, reg, 6, 7, v);
            }
            _ => {}
        }
    }
}

impl ChipCore for YmfmBackend {
    fn step(&mut self) -> f32 {
        // FIX: single mutable borrow, and no redundant inner phase accumulator.
        // ChipPlayer's outer loop already resamples; step() should generate one
        // chip sample per call.
        let i = 0;
        while i < self.pending.len() && self.pending[i].t <= self.cur_t {
            let w = self.pending.remove(i);
            let (voice, field, value) = (w.voice.clone(), w.field.clone(), w.value);
            self.apply_field(&voice, &field, value);
        }
        while self.register_cursor < self.register_pending.len()
            && self.register_pending[self.register_cursor].t <= self.cur_t
        {
            let w = self.register_pending[self.register_cursor];
            self.register_cursor += 1;
            let bank = w.port as usize;
            self.shadow[bank][w.reg as usize] = w.value;
            if bank == 0 && w.reg == 0x2B { self.dac_enabled = w.value & 0x80 != 0; }
            let port = (bank as u32) * 2;
            self.write_all(port, w.reg);
            self.write_all(port + 1, w.value);
        }
        // Retire consumed events in batches so the audio callback does not
        // allocate while applying timestamped DAC writes.
        if self.register_cursor >= 1024 && self.register_cursor * 2 >= self.register_pending.len() {
            self.register_pending.drain(..self.register_cursor);
            self.register_cursor = 0;
        }

        let mix = if self.chip.pin_mut().generate_voice_outputs(&mut self.voice_buf) {
            let mut sum_l = 0i32;
            let mut sum_r = 0i32;
            for idx in 0..6 {
                let l = self.voice_buf[idx * 2];
                let r = self.voice_buf[idx * 2 + 1];
                let sample = (l + r) as f32 * 0.5 * YM_VOICE_SCALE;
                self.voice_samples[idx].1 = sample;
                sum_l += l;
                sum_r += r;
            }
            let l = (sum_l * 128) * 64 / (6 * 65);
            let r = (sum_r * 128) * 64 / (6 * 65);
            (l + r) as f32 * 0.5 * YM_SCALE
        } else {
            self.chip.pin_mut().generate(&mut self.buf);
            let l = self.buf[0] as f32 * YM_SCALE;
            let r = if self.channels > 1 { self.buf[1] as f32 * YM_SCALE } else { l };
            self.voice_samples.iter_mut().for_each(|v| v.1 = 0.0);
            (l + r) * 0.5
        };

        // FIX: advance cur_t at native rate, not host rate.
        self.cur_t += 1.0 / self.native_rate;
        mix
    }

    fn schedule(&mut self, w: ScheduledWrite) {
        let pos = self.pending.iter().position(|x| x.t > w.t).unwrap_or(self.pending.len());
        self.pending.insert(pos, w);
    }

    fn schedule_register_write(&mut self, t: f64, port: u8, reg: u8, value: u8) {
        let t = t.max(self.cur_t);
        let write = RegisterWrite { t, port: port.min(1), reg, value };
        if self.register_pending.last().map_or(true, |last| last.t <= t) {
            self.register_pending.push(write);
            return;
        }
        let i = self.register_pending.partition_point(|w| w.t <= t).max(self.register_cursor);
        self.register_pending.insert(i, write);
    }

    fn reserve_register_writes(&mut self, count: usize) {
        self.register_pending.reserve(count);
    }

    fn cancel_register_writes_from(&mut self, t: f64) {
        if self.register_cursor > 0 {
            self.register_pending.drain(..self.register_cursor);
            self.register_cursor = 0;
        }
        self.register_pending.retain(|w| w.t < t);
    }

    fn dac_enabled(&self) -> bool { self.dac_enabled }

    fn voice_samples(&self) -> &[(String, f32)] { &self.voice_samples }

    fn set_field(&mut self, voice: &str, field: &str, value: u32) {
        self.apply_field(voice, field, value);
    }

    fn voices(&self) -> Vec<String> {
        (1..=6).map(|n| format!("ch{}", n)).collect()
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.register_pending.clear();
        self.register_cursor = 0;
        if self.dac_enabled {
            self.write_all(0, 0x2B);
            self.write_all(1, 0x00);
            self.shadow[0][0x2B] = 0;
            self.dac_enabled = false;
        }
    }
    fn set_time(&mut self, t: f64) { self.cur_t = t; }
    fn step_rate(&self) -> f64 { self.native_rate }

    fn cancel_pending(&mut self, voice: &str, after_t: f64) {
        self.pending.retain(|w| !(w.voice == voice && w.t >= after_t));
    }
}
