//! Register and field tables.
//!
//! A chip declares `reg` entries (name, scope, width, optional addr) and
//! `field` entries (named views of one or more registers). At runtime,
//! `RegisterFile` holds values per (voice, register) or per (global, register).
//! Field reads combine bit slices from one or more registers.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BitRange { pub lo: u8, pub hi: u8 }

impl BitRange {
    pub fn width(self) -> u8 { self.hi - self.lo + 1 }
    pub fn mask(self) -> u32 { ((1u64 << self.width()) - 1) as u32 }
    pub fn new(lo: u8, hi: u8) -> Self { BitRange { lo, hi } }
    pub fn single(bit: u8) -> Self { BitRange { lo: bit, hi: bit } }
}

#[derive(Clone, Debug)]
pub enum RegScope {
    Global,
    Voices(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Register {
    pub name: String,
    pub scope: RegScope,
    pub addr: Option<u8>,
    pub width: u8,
    pub reset: u32,
}

#[derive(Clone, Debug)]
pub struct FieldRegRef {
    pub reg: String,
    pub bits: BitRange,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role { Pitch, Param, Volume, Key }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope { Global, Voice, Op }

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub regs: Vec<FieldRegRef>,
    pub role: Role,
    pub scope: Scope,
}

impl Field {
    pub fn total_width(&self) -> u8 {
        self.regs.iter().map(|r| r.bits.width()).sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct RegisterFile {
    /// (register_name) -> value, for global-scope registers.
    global: BTreeMap<String, u32>,
    /// (voice_name, register_name) -> value.
    per_voice: BTreeMap<(String, String), u32>,
}

impl RegisterFile {
    pub fn new() -> Self { Self::default() }

    /// Initialize from declarations. Sets each register to its reset value
    /// for every scope it declares.
    pub fn from_decls(regs: &[Register]) -> Self {
        let mut f = RegisterFile::new();
        for r in regs {
            match &r.scope {
                RegScope::Global => { f.global.insert(r.name.clone(), r.reset); }
                RegScope::Voices(voices) => {
                    for v in voices {
                        f.per_voice.insert((v.clone(), r.name.clone()), r.reset);
                    }
                }
            }
        }
        f
    }

    pub fn get(&self, voice: Option<&str>, reg: &str) -> u32 {
        match voice {
            None => self.global.get(reg).copied().unwrap_or(0),
            Some(v) => self.per_voice.get(&(v.to_string(), reg.to_string())).copied().unwrap_or(0),
        }
    }

    pub fn set(&mut self, voice: Option<&str>, reg: &str, val: u32) {
        match voice {
            None => { self.global.insert(reg.to_string(), val); }
            Some(v) => { self.per_voice.insert((v.to_string(), reg.to_string()), val); }
        }
    }

    /// Read a field for one voice (or global). Assembles bits from each
    /// register in the field's `regs` list, LSB-first.
    pub fn read_field(&self, field: &Field, voice: Option<&str>) -> u32 {
        let mut out = 0u32;
        let mut shift = 0u32;
        for fr in &field.regs {
            let rv = self.get(voice, &fr.reg);
            let slice = (rv >> fr.bits.lo) & fr.bits.mask();
            out |= slice << shift;
            shift += fr.bits.width() as u32;
        }
        out
    }

    /// Write a field. Splits `val` into the bit slices each register holds,
    /// preserving the other bits of each register (so `att` writes don't
    /// clobber `noise`, which shares the register).
    pub fn write_field(&mut self, field: &Field, voice: Option<&str>, val: u32) {
        let mut shift = 0u32;
        for fr in &field.regs {
            let width = fr.bits.width() as u32;
            let slice = (val >> shift) & ((1u64 << width) - 1) as u32;
            let mut rv = self.get(voice, &fr.reg);
            rv &= !(fr.bits.mask() << fr.bits.lo);
            rv |= (slice & fr.bits.mask()) << fr.bits.lo;
            self.set(voice, &fr.reg, rv);
            shift += width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sn_div() -> (Field, RegisterFile) {
        let reg = Register {
            name: "div".into(),
            scope: RegScope::Voices(vec!["tone1".into(), "tone2".into()]),
            addr: None,
            width: 10,
            reset: 0,
        };
        let field = Field {
            name: "div".into(),
            regs: vec![FieldRegRef { reg: "div".into(), bits: BitRange::new(0, 9) }],
            role: Role::Pitch,
            scope: Scope::Voice,
        };
        let rf = RegisterFile::from_decls(&[reg]);
        (field, rf)
    }

    #[test]
    fn single_register_roundtrip() {
        let (field, mut rf) = sn_div();
        rf.write_field(&field, Some("tone1"), 200);
        assert_eq!(rf.read_field(&field, Some("tone1")), 200);
        assert_eq!(rf.read_field(&field, Some("tone2")), 0);
    }

    #[test]
    fn multi_register_field_fnum() {
        let reg_lo = Register {
            name: "fnum_lo".into(),
            scope: RegScope::Voices(vec!["ch1".into()]),
            addr: Some(0xA0), width: 8, reset: 0,
        };
        let reg_hi = Register {
            name: "fnum_hi_block_ks".into(),
            scope: RegScope::Voices(vec!["ch1".into()]),
            addr: Some(0xA4), width: 8, reset: 0,
        };
        let field = Field {
            name: "fnum".into(),
            regs: vec![
                FieldRegRef { reg: "fnum_lo".into(), bits: BitRange::new(0, 7) },
                FieldRegRef { reg: "fnum_hi_block_ks".into(), bits: BitRange::new(0, 2) },
            ],
            role: Role::Pitch,
            scope: Scope::Voice,
        };
        let mut rf = RegisterFile::from_decls(&[reg_lo, reg_hi]);
        rf.write_field(&field, Some("ch1"), 0x4FF);   // 11-bit value
        assert_eq!(rf.read_field(&field, Some("ch1")), 0x4FF);
        // Setting block (bits 3..5 of fnum_hi_block_ks) must not clobber fnum.
        rf.write_field(&Field {
            name: "block".into(),
            regs: vec![FieldRegRef { reg: "fnum_hi_block_ks".into(), bits: BitRange::new(3, 5) }],
            role: Role::Pitch, scope: Scope::Voice,
        }, Some("ch1"), 5);
        assert_eq!(rf.read_field(&field, Some("ch1")), 0x4FF);
        let block = rf.read_field(&Field {
            name: "block".into(),
            regs: vec![FieldRegRef { reg: "fnum_hi_block_ks".into(), bits: BitRange::new(3, 5) }],
            role: Role::Pitch, scope: Scope::Voice,
        }, Some("ch1"));
        assert_eq!(block, 5);
    }
}