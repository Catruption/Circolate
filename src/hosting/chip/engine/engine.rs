//! Per-step interpreter. Ties chip spec + primitives + expression eval into
//! a sample producer. One `step()` = one core step at `step_rate`.

use std::collections::BTreeMap;

use crate::formats::chip::chip::{Chip, Out, Variant};
use crate::formats::chip::expr::{self, Context, Expr};
use super::primitives::{Divider, Lfsr};

/// A pending field write. `t` is absolute seconds from playback start.
#[derive(Clone, Debug)]
pub struct ScheduledWrite {
    pub t: f64,
    pub voice: String,
    pub field: String,
    pub value: u32,
}

pub struct CoreState {
    chip: Chip,
    variant: Variant,
    fields: BTreeMap<(String, String), u32>,
    dividers: BTreeMap<String, Divider>,
    lfsrs: BTreeMap<String, Lfsr>,
    tables: BTreeMap<String, Vec<f64>>,
    n_exprs: BTreeMap<String, Expr>,
    lfsr_mode_exprs: BTreeMap<String, Expr>,
    lfsr_clock_exprs: BTreeMap<String, Expr>,
    mix_expr: Expr,
    step_rate: f64,
    phase: f64,
    cur_t: f64,
    prev_sample: f32,
    pending: Vec<ScheduledWrite>,
    rises: BTreeMap<String, bool>,
}

impl CoreState {
    pub fn new(chip: Chip) -> Result<Self, String> {
        let core = chip.core.as_ref().ok_or("chip has no core block")?;
        let variant_name = chip.default_variant.as_deref()
            .ok_or("chip has no default_variant")?;
        let variant = chip.variants.get(variant_name)
            .ok_or_else(|| format!("variant `{}` not found", variant_name))?
            .clone();
        let clock = chip.clock as f64;
        let load_ctx = LoadCtx { clock, variant: &variant, i: 0.0 };

        let step_rate = expr::parse(&chip.step_expr)
            .map_err(|e| format!("step: {}", e))?
            .eval(&load_ctx).map_err(|e| format!("step: {}", e))?;
        if step_rate <= 0.0 {
            return Err(format!("step rate must be positive, got {}", step_rate));
        }

        // Build tables (i runs 0..len, rhs evaluated once at load)
        let mut tables = BTreeMap::new();
        for (name, spec) in &core.tables {
            let e = expr::parse(&spec.expr)
                .map_err(|e| format!("table `{}`: {}", name, e))?;
            let mut vals = Vec::with_capacity(spec.len as usize);
            for i in 0..spec.len {
                let ctx = LoadCtx { clock, variant: &variant, i: i as f64 };
                vals.push(e.eval(&ctx).map_err(|e| format!("table `{}`[{}]: {}", name, i, e))?);
            }
            tables.insert(name.clone(), vals);
        }

        // Build dividers
        let mut dividers = BTreeMap::new();
        let mut n_exprs = BTreeMap::new();
        for (id, spec) in &core.dividers {
            let mut d = Divider::new();
            d.dc_below = spec.dc_below.unwrap_or(0);
            dividers.insert(id.clone(), d);
            n_exprs.insert(id.clone(), expr::parse(&spec.n_expr)
                .map_err(|e| format!("divider `{}` n: {}", id, e))?);
        }

        // Build LFSRs — taps/width from variant context
        let mut lfsrs = BTreeMap::new();
        let mut lfsr_mode_exprs = BTreeMap::new();
        let mut lfsr_clock_exprs = BTreeMap::new();
        for (id, spec) in &core.lfsrs {
            let taps = expr::parse(&spec.taps)
                .map_err(|e| format!("lfsr `{}` taps: {}", id, e))?
                .eval(&load_ctx).map_err(|e| format!("lfsr `{}` taps: {}", id, e))? as u32;
            let width = expr::parse(&spec.width)
                .map_err(|e| format!("lfsr `{}` width: {}", id, e))?
                .eval(&load_ctx).map_err(|e| format!("lfsr `{}` width: {}", id, e))? as u8;
            lfsrs.insert(id.clone(), Lfsr::new(taps, width, false));
            lfsr_mode_exprs.insert(id.clone(), expr::parse(&spec.mode)
                .map_err(|e| format!("lfsr `{}` mode: {}", id, e))?);
            lfsr_clock_exprs.insert(id.clone(), expr::parse(&spec.clock)
                .map_err(|e| format!("lfsr `{}` clock: {}", id, e))?);
        }

        let mix_expr = expr::parse(core.mix.as_ref().ok_or("core has no mix")?)
            .map_err(|e| format!("mix: {}", e))?;

        // Initialise every field to its register's reset value, so mix
        // doesn't hit unknown-field and produce silence.
        let mut fields: BTreeMap<(String, String), u32> = BTreeMap::new();
        for (fname, field) in &chip.fields {
            for part in &field.parts {
                let reg = match chip.registers.get(&part.reg) { Some(r) => r, None => continue };
                let width = (part.bits_hi - part.bits_lo + 1) as u32;
                let mask = if width >= 32 { u32::MAX } else { (1u32 << width) - 1 };
                let v = (reg.reset >> part.bits_lo as u32) & mask;
                for voice in &reg.voices {
                    fields.insert((fname.clone(), voice.clone()), v);
                }
            }
        }

        Ok(CoreState {
            chip, variant, fields,
            dividers, lfsrs, tables,
            n_exprs, lfsr_mode_exprs, lfsr_clock_exprs, mix_expr,
            step_rate, phase: 0.0, cur_t: 0.0, prev_sample: 0.0,
            pending: Vec::new(), rises: BTreeMap::new(),
        })
    }

    pub fn step_rate(&self) -> f64 { self.step_rate }

    pub fn set_field(&mut self, voice: &str, field: &str, value: u32) {
        self.fields.insert((field.to_string(), voice.to_string()), value);
    }

    pub fn schedule(&mut self, w: ScheduledWrite) {
        let pos = self.pending.iter().position(|x| x.t > w.t).unwrap_or(self.pending.len());
        self.pending.insert(pos, w);
    }

    /// Advance one core step. Returns one sample.
    pub fn step(&mut self) -> f32 {
        // Drain any writes whose time has arrived.
        let mut i = 0;
        while i < self.pending.len() && self.pending[i].t <= self.cur_t {
            let w = self.pending.remove(i);
            self.fields.insert((w.field, w.voice), w.value);
        }

        // Re-evaluate n=<expr> for each divider against current fields.
        {
            let ctx = StepCtx {
                chip: &self.chip, variant: &self.variant,
                fields: &self.fields, dividers: &self.dividers,
                lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
            };
            let updates: Vec<(String, u32)> = self.n_exprs.iter()
                .filter_map(|(id, e)| e.eval(&ctx).ok().map(|v| (id.clone(), (v as u32).max(1))))
                .collect();
            for (id, n) in updates {
                if let Some(d) = self.dividers.get_mut(&id) { d.set_n(n); }
            }
        }

        // Step dividers, record rises.
        self.rises.clear();
        for (id, d) in self.dividers.iter_mut() {
            let rose = d.step();
            self.rises.insert(id.clone(), rose);
        }

        // Update LFSR mode, then step those clocked by a rise.
        {
            let ctx = StepCtx {
                chip: &self.chip, variant: &self.variant,
                fields: &self.fields, dividers: &self.dividers,
                lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
            };
            let modes: Vec<(String, bool)> = self.lfsr_mode_exprs.iter()
                .filter_map(|(id, e)| e.eval(&ctx).ok().map(|v| (id.clone(), v != 0.0)))
                .collect();
            for (id, m) in modes {
                if let Some(l) = self.lfsrs.get_mut(&id) { l.parity_mode = m; }
            }
            let clocks: Vec<String> = self.lfsr_clock_exprs.iter()
                .filter_map(|(id, e)| e.eval(&ctx).ok()
                    .filter(|v| *v != 0.0).map(|_| id.clone()))
                .collect();
            for id in clocks {
                if let Some(l) = self.lfsrs.get_mut(&id) { l.step(); }
            }
        }

        // Evaluate mix → sample.
        let ctx = StepCtx {
            chip: &self.chip, variant: &self.variant,
            fields: &self.fields, dividers: &self.dividers,
            lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
        };
        let raw = self.mix_expr.eval(&ctx).unwrap_or(0.0) as f32;
        let out = if self.chip.out == Out::Bipolar { raw } else { raw * 2.0 - 1.0 };

        self.cur_t += 1.0 / self.step_rate;
        out
    }

    /// Fill `out` at `host_rate` from the native step rate.
    pub fn render(&mut self, out: &mut [f32], host_rate: u32) {
        let ratio = self.step_rate / host_rate as f64;
        for slot in out.iter_mut() {
            self.phase += ratio;
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                self.prev_sample = self.step();
            }
            *slot = self.prev_sample;
        }
    }
}

// ── Contexts ──────────────────────────────────────────────

/// Parse numbers including 0x/0b prefixes. Needed because `variant.taps`
/// stores `"0x0009"` as a raw string.
fn parse_num(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok().map(|v| v as f64)
    } else if let Some(b) = s.strip_prefix("0b") {
        u64::from_str_radix(b, 2).ok().map(|v| v as f64)
    } else {
        s.parse().ok()
    }
}

struct LoadCtx<'a> { clock: f64, variant: &'a Variant, i: f64 }

impl<'a> Context for LoadCtx<'a> {
    fn var(&self, name: &str) -> Option<f64> {
        match name {
            "i"     => Some(self.i),
            "clock" => Some(self.clock),
            _ => {
                let key = name.strip_prefix("variant.").unwrap_or(name);
                self.variant.keys.get(key).and_then(|v| parse_num(v))
            }
        }
    }
    fn field(&self, _: &str, _: &str) -> Option<f64> { None }
    fn table(&self, _: &str) -> Option<&[f64]> { None }
}

struct StepCtx<'a> {
    chip: &'a Chip,
    variant: &'a Variant,
    fields: &'a BTreeMap<(String, String), u32>,
    dividers: &'a BTreeMap<String, Divider>,
    lfsrs: &'a BTreeMap<String, Lfsr>,
    tables: &'a BTreeMap<String, Vec<f64>>,
    rises: &'a BTreeMap<String, bool>,
}

impl<'a> Context for StepCtx<'a> {
    fn var(&self, name: &str) -> Option<f64> {
        if name == "clock" { return Some(self.chip.clock as f64); }
        if let Some(k) = name.strip_prefix("variant.") {
            return self.variant.keys.get(k).and_then(|v| parse_num(v));
        }
        if let Some((id, prop)) = name.split_once('.') {
            match prop {
                "q" => {
                    if let Some(d) = self.dividers.get(id) { return Some(d.level() as f64); }
                    if let Some(l) = self.lfsrs.get(id)    { return Some(l.level() as f64); }
                }
                "rise" => return Some(if *self.rises.get(id)? { 1.0 } else { 0.0 }),
                _ => {}
            }
        }
        None
    }
    fn field(&self, field: &str, voice: &str) -> Option<f64> {
        self.fields.get(&(field.to_string(), voice.to_string())).map(|v| *v as f64)
    }
    fn table(&self, name: &str) -> Option<&[f64]> {
        self.tables.get(name).map(|v| v.as_slice())
    }
}