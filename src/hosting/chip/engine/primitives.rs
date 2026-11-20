//! Primitive state machines shared by every chip.
//!
//! The chip spec (CHIP.md §2.4) fixes their behavior:
//!
//!   divider: each step cnt -= 1; if cnt <= 0 { cnt = n; q ^= 1 }
//!            q is 0/1; rise is a one-step pulse on 0 -> 1.
//!            dc_below m: n < m holds q at 1.
//!   lfsr:    on each clock pulse:
//!              fb = mode ? parity(reg & taps) : (reg & 1)
//!              reg = (reg >> 1) | (fb << (width - 1)); q = reg & 1
//!            reset: reg = 1 << (width - 1)
//!
//! No chip-specific quirks live here.

#[derive(Clone, Debug)]
pub struct Divider {
    pub n: u32,
    pub dc_below: u32,
    pub cnt: i32,
    pub q: bool,
}

impl Divider {
    pub fn new() -> Self { Divider { n: 0, dc_below: 0, cnt: 1, q: false } }

    /// Advance one step. Returns true if a rise occurred on this step.
    pub fn step(&mut self) -> bool {
        if self.n < self.dc_below {
            // Below the DC threshold: hold output at 1, no counting.
            self.q = true;
            return false;
        }
        let prev = self.q;
        self.cnt -= 1;
        if self.cnt <= 0 {
            self.cnt = self.n.max(1) as i32;
            self.q = !self.q;
        }
        self.q && !prev
    }

    /// Set the divisor. Called whenever the driving field changes.
    pub fn set_n(&mut self, n: u32) { self.n = n; }

    pub fn level(&self) -> f32 { if self.q { 1.0 } else { 0.0 } }
}

#[derive(Clone, Debug)]
pub struct Lfsr {
    pub reg: u32,
    pub taps: u32,
    pub width: u8,
    /// `true` = parity of `reg & taps`, `false` = bit-0 feedback.
    /// Keep in sync with `step()` and docs/CHIP.md §2.4.
    pub parity_mode: bool,
}

impl Lfsr {
    pub fn new(taps: u32, width: u8, parity_mode: bool) -> Self {
        let width = width.clamp(1, 31);
        Lfsr { reg: 1u32 << (width - 1), taps, width, parity_mode }
    }

    pub fn reset(&mut self) { self.reg = 1u32 << (self.width - 1); }

    pub fn set_taps(&mut self, taps: u32, width: u8) {
        self.taps = taps;
        self.width = width.clamp(1, 31);
        self.reset();
    }

    pub fn step(&mut self) {
        let fb: u32 = if self.parity_mode {
            (self.reg & self.taps).count_ones() & 1
        } else {
            self.reg & 1
        };
        self.reg = (self.reg >> 1) | (fb << (self.width - 1));
    }

    pub fn q(&self) -> bool { self.reg & 1 != 0 }
    pub fn level(&self) -> f32 { if self.q() { 1.0 } else { 0.0 } }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub values: Vec<f64>,
}

impl Table {
    pub fn new(name: String, values: Vec<f64>) -> Self { Table { name, values } }
    pub fn get(&self, i: usize) -> f64 { self.values.get(i).copied().unwrap_or(0.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_rises_every_two_wrap_cycles() {
        // Spec: q toggles every `n` steps, and `rise` is the 0 -> 1
        // edge, so rise-to-rise is 2 * n steps (not n).
        let mut d = Divider { n: 200, dc_below: 0, cnt: 1, q: false };
        assert!(d.step());                    // step 1: q false -> true
        for _ in 0..399 { assert!(!d.step()); }
        assert!(d.step());                    // step 401: next rise
    }

    #[test]
    fn divider_dc_below_holds_high() {
        let mut d = Divider { n: 1, dc_below: 2, cnt: 1, q: false };
        for _ in 0..10 {
            assert!(!d.step());
            assert!(d.q);
        }
    }

    fn cycle_period(lfsr: &mut Lfsr, limit: u32) -> u32 {
        let start = lfsr.reg;
        let mut period = 0u32;
        loop {
            lfsr.step();
            period += 1;
            if lfsr.reg == start { return period; }
            assert!(period < limit, "LFSR did not return to seed");
        }
    }

    #[test]
    fn lfsr_parity_mode_runs_long_sequence() {
        // parity_mode = true selects parity(reg & taps); these 16-bit
        // taps produce a long sequence, unlike bit-0 feedback.
        let mut l = Lfsr::new(0x0009, 16, true);
        assert!(cycle_period(&mut l, 1_000_000) > 1000);
    }

    #[test]
    fn lfsr_bit0_mode_has_short_period() {
        // parity_mode = false selects reg & 1, which rotates the reset
        // bit around a 16-bit register in exactly 16 steps.
        let mut l = Lfsr::new(0x0009, 16, false);
        assert_eq!(cycle_period(&mut l, 1024), 16);
    }
}