//! Per-step interpreter: chip spec + primitives + expression eval → samples.

use std::collections::BTreeMap;

use crate::hosting::chip::chip::{Chip, Out, Variant};
use crate::hosting::chip::expr::{self, Context, Expr};
use super::primitives::{Divider, Lfsr};

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
    /// Field name -> voice name -> value. Nested maps allow borrowed `&str`
    /// lookups in the per-sample expression evaluator without allocating.
    fields: BTreeMap<String, BTreeMap<String, u32>>,
    dividers: BTreeMap<String, Divider>,
    lfsrs: BTreeMap<String, Lfsr>,
    tables: BTreeMap<String, Vec<f64>>,
    n_exprs: BTreeMap<String, Expr>,
    n_values: BTreeMap<String, Option<u32>>,
    lfsr_mode_exprs: BTreeMap<String, Expr>,
    lfsr_modes: BTreeMap<String, Option<bool>>,
    lfsr_clock_exprs: BTreeMap<String, Expr>,
    lfsr_clocks: BTreeMap<String, Option<bool>>,
    mix_expr: Expr,
    step_rate: f64,
    phase: f64,
    cur_t: f64,
    prev_sample: f32,
    pending: Vec<ScheduledWrite>,
    rises: BTreeMap<String, bool>,
    voice_samples: Vec<(String, f32)>,
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

        let mut tables = BTreeMap::new();
        for (name, spec) in &core.tables {
            let e = expr::parse(&spec.expr).map_err(|e| format!("table `{}`: {}", name, e))?;
            let mut vals = Vec::with_capacity(spec.len as usize);
            for i in 0..spec.len {
                let ctx = LoadCtx { clock, variant: &variant, i: i as f64 };
                vals.push(e.eval(&ctx).map_err(|e| format!("table `{}`[{}]: {}", name, i, e))?);
            }
            tables.insert(name.clone(), vals);
        }

        let mut dividers = BTreeMap::new();
        let mut n_exprs = BTreeMap::new();
        for (id, spec) in &core.dividers {
            let mut d = Divider::new();
            d.dc_below = spec.dc_below.unwrap_or(0);
            dividers.insert(id.clone(), d);
            n_exprs.insert(id.clone(), expr::parse(&spec.n_expr)
                .map_err(|e| format!("divider `{}` n: {}", id, e))?);
        }

        let mut lfsrs = BTreeMap::new();
        let mut lfsr_mode_exprs = BTreeMap::new();
        let mut lfsr_clock_exprs = BTreeMap::new();
        for (id, spec) in &core.lfsrs {
            let taps = expr::parse(&spec.taps).map_err(|e| format!("lfsr `{}` taps: {}", id, e))?
                .eval(&load_ctx).map_err(|e| format!("lfsr `{}` taps: {}", id, e))? as u32;
            let width = expr::parse(&spec.width).map_err(|e| format!("lfsr `{}` width: {}", id, e))?
                .eval(&load_ctx).map_err(|e| format!("lfsr `{}` width: {}", id, e))? as u8;
            lfsrs.insert(id.clone(), Lfsr::new(taps, width, false));
            lfsr_mode_exprs.insert(id.clone(), expr::parse(&spec.mode)
                .map_err(|e| format!("lfsr `{}` mode: {}", id, e))?);
            lfsr_clock_exprs.insert(id.clone(), expr::parse(&spec.clock)
                .map_err(|e| format!("lfsr `{}` clock: {}", id, e))?);
        }

        let mix_expr = expr::parse(core.mix.as_ref().ok_or("core has no mix")?)
            .map_err(|e| format!("mix: {}", e))?;

        // Initialise every field to its register's reset value so mix never
        // hits an unknown-field and returns silence.
        let mut fields: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
        for (fname, field) in &chip.fields {
            for part in &field.parts {
                let reg = match chip.registers.get(&part.reg) { Some(r) => r, None => continue };
                let width = (part.bits_hi - part.bits_lo + 1) as u32;
                let mask = if width >= 32 { u32::MAX } else { (1u32 << width) - 1 };
                let v = (reg.reset >> part.bits_lo as u32) & mask;
                for voice in &reg.voices {
                    fields.entry(fname.clone()).or_default().insert(voice.clone(), v);
                }
            }
        }

        let voice_samples = chip.voices.iter().map(|v| (v.name.clone(), 0.0)).collect();
        let rises = dividers.keys().map(|id| (id.clone(), false)).collect();
        let n_values = n_exprs.keys().map(|id| (id.clone(), None)).collect();
        let lfsr_modes = lfsr_mode_exprs.keys().map(|id| (id.clone(), None)).collect();
        let lfsr_clocks = lfsr_clock_exprs.keys().map(|id| (id.clone(), None)).collect();
        Ok(CoreState {
            chip, variant, fields,
            dividers, lfsrs, tables,
            n_exprs, n_values, lfsr_mode_exprs, lfsr_modes,
            lfsr_clock_exprs, lfsr_clocks, mix_expr,
            step_rate, phase: 0.0, cur_t: 0.0, prev_sample: 0.0,
            pending: Vec::new(), rises,
            voice_samples,
        })
    }

    pub fn step_rate(&self) -> f64 { self.step_rate }
    pub fn voice_samples(&self) -> &[(String, f32)] { &self.voice_samples }

    pub fn voices(&self) -> Vec<String> {
        self.chip.voices.iter().map(|v| v.name.clone()).collect()
    }

    pub fn set_field(&mut self, voice: &str, field: &str, value: u32) {
        self.fields.entry(field.to_string()).or_default().insert(voice.to_string(), value);
    }

    pub fn set_time(&mut self, t: f64) {
        self.cur_t = t;
    }

    pub fn cancel_pending(&mut self, voice: &str, after_t: f64) {
        self.pending.retain(|w| !(w.voice == voice && w.t >= after_t));
    }

    pub fn schedule(&mut self, w: ScheduledWrite) {
        let pos = self.pending.iter().position(|x| x.t > w.t).unwrap_or(self.pending.len());
        self.pending.insert(pos, w);
    }

    pub fn clear_pending(&mut self) { self.pending.clear(); }


    pub fn step(&mut self) -> f32 {
        // Drain any writes whose time has arrived.
        let i = 0;
        while i < self.pending.len() && self.pending[i].t <= self.cur_t {
            let w = self.pending.remove(i);
            self.fields.entry(w.field).or_default().insert(w.voice, w.value);
        }

        // Evaluate all divider updates against the same pre-update state.
        for (id, expr) in &self.n_exprs {
            let ctx = StepCtx {
                chip: &self.chip, variant: &self.variant,
                fields: &self.fields, dividers: &self.dividers,
                lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
                only_voice: None,
            };
            let n = expr.eval(&ctx).ok().map(|v| (v as u32).max(1));
            if let Some(value) = self.n_values.get_mut(id) { *value = n; }
        }
        for (id, n) in &self.n_values {
            if let Some(n) = n {
                if let Some(d) = self.dividers.get_mut(id) { d.set_n(*n); }
            }
        }

        // Step dividers, record rises. LFSR clocks read `.rise`, so this
        // must run before the LFSR phase.
        for (id, d) in self.dividers.iter_mut() {
            let rose = d.step();
            if let Some(value) = self.rises.get_mut(id) { *value = rose; }
        }

        // Evaluate every LFSR mode before applying any of them.
        for (id, expr) in &self.lfsr_mode_exprs {
            let ctx = StepCtx {
                chip: &self.chip, variant: &self.variant,
                fields: &self.fields, dividers: &self.dividers,
                lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
                only_voice: None,
            };
            let mode = expr.eval(&ctx).ok().map(|v| v != 0.0);
            if let Some(value) = self.lfsr_modes.get_mut(id) { *value = mode; }
        }
        for (id, mode) in &self.lfsr_modes {
            if let Some(mode) = mode {
                if let Some(l) = self.lfsrs.get_mut(id) { l.parity_mode = *mode; }
            }
        }

        // Evaluate all LFSR clocks before advancing any of them.
        for (id, expr) in &self.lfsr_clock_exprs {
            let ctx = StepCtx {
                chip: &self.chip, variant: &self.variant,
                fields: &self.fields, dividers: &self.dividers,
                lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
                only_voice: None,
            };
            let clocked = expr.eval(&ctx).ok().map(|v| v != 0.0);
            if let Some(value) = self.lfsr_clocks.get_mut(id) { *value = clocked; }
        }
        for (id, clocked) in &self.lfsr_clocks {
            if *clocked == Some(true) {
                if let Some(l) = self.lfsrs.get_mut(id) { l.step(); }
            }
        }

        // Evaluate mix → sample.
        let ctx = StepCtx {
            chip: &self.chip, variant: &self.variant,
            fields: &self.fields, dividers: &self.dividers,
            lfsrs: &self.lfsrs, tables: &self.tables, rises: &self.rises,
            only_voice: None,
        };
        let raw = self.mix_expr.eval(&ctx).unwrap_or(0.0) as f32;
        for idx in 0..self.voice_samples.len() {
            let voice = &self.chip.voices[idx].name;
            let isolated = StepCtx { only_voice: Some(voice), ..ctx };
            let voice_raw = self.mix_expr.eval(&isolated).unwrap_or(0.0) as f32;
            // A per-voice view is the contribution to the sum, so remove the
            // unipolar mix's shared -1 offset while retaining its scale.
            let voice_sample = if self.chip.out == Out::Bipolar { voice_raw } else { voice_raw * 2.0 };
            self.voice_samples[idx].1 = voice_sample;
        }
        let out = if self.chip.out == Out::Bipolar { raw } else { raw * 2.0 - 1.0 };

        self.cur_t += 1.0 / self.step_rate;
        out
    }
}

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

#[derive(Clone, Copy)]
struct StepCtx<'a> {
    chip: &'a Chip,
    variant: &'a Variant,
    fields: &'a BTreeMap<String, BTreeMap<String, u32>>,
    dividers: &'a BTreeMap<String, Divider>,
    lfsrs: &'a BTreeMap<String, Lfsr>,
    tables: &'a BTreeMap<String, Vec<f64>>,
    rises: &'a BTreeMap<String, bool>,
    only_voice: Option<&'a str>,
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
        if let Some(only) = self.only_voice {
            if voice != only {
                let role = self.chip.fields.get(field).map(|f| f.role);
                if role == Some(crate::hosting::chip::chip::Role::Volume) {
                    return Some(15.0);
                }
                return Some(0.0);
            }
        }
        self.fields.get(field)?.get(voice).map(|v| *v as f64)
    }
    fn table(&self, name: &str) -> Option<&[f64]> {
        self.tables.get(name).map(|v| v.as_slice())
    }
}
