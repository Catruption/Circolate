//! Bus protocol decoders for real-hardware chips.
//!
//! Two protocols, matching CHIP.md §2.3:
//!
//!   latch_data (SN76489): byte stream, latch byte selects channel/type
//!                         and carries low bits; data byte carries high
//!                         bits of the most recent tone register.
//!   addr_data  (YM2612):  (port, register, value) triple.
//!
//! The bus is not on the note path. It exists for the VGM-replay path
//! (milestone 6) which will be retired once the chip models are accurate.

use std::collections::BTreeMap;
use crate::formats::chip::chip::{ChipModel, LatchDataBus, Route, RouteKind, RouteType};
use crate::hosting::chip::engine::registers::RegisterFile;
use crate::formats::chip::expr::{Context, Expr};

/// A single write on the bus.
pub enum BusWrite<'a> {
    Byte(u8),
    Addr { port: u8, reg: u8, val: u8 },
    // Note: not a `&` lifetime; the bytes are copied. The `'a` allows
    // future extension (e.g. slices for multi-byte ops).
    _Marker(std::marker::PhantomData<&'a ()>),
}

pub struct LatchDataState {
    pub latched_chan: u8,
    pub latched_type: u8,   // 0 = tone/noise, 1 = attenuation
    pub latched_reg: Option<String>,  // target register name
}

impl LatchDataState {
    pub fn new() -> Self {
        LatchDataState { latched_chan: 0, latched_type: 0, latched_reg: None }
    }
}

pub struct AddrDataState {
    // Nothing latched; every write is complete.
}

impl AddrDataState {
    pub fn new() -> Self { AddrDataState {} }
}

/// Context used to evaluate bus `latch`/`data` expressions.
struct BusExprContext<'a> {
    model: &'a ChipModel,
    regs: &'a RegisterFile,
    /// Current value of the target register, if known.
    reg_value: Option<u32>,
    /// The low or high nibble from the current byte, if applicable.
    nibble: Option<u32>,
}

impl<'a> Context for BusExprContext<'a> {
    fn var(&self, name: &str) -> Option<f64> {
        if let Some(v) = self.reg_value {
            if name == "reg" { return Some(v as f64); }
        }
        if let Some(v) = self.nibble {
            if name == "lo" { return Some(v as f64); }
            if name == "hi" { return Some(v as f64); }
        }
        None
    }
    fn field(&self, _: &str, _: &str) -> Option<f64> { None }
    fn table(&self, _: &str) -> Option<&[f64]> { None }
    fn mask_of(&self, reg: &str) -> Option<u32> {
        self.model.registers.get(reg).map(|r| {
            if r.width >= 32 { u32::MAX } else { ((1u64 << r.width) - 1) as u32 }
        })
    }
}

impl LatchDataBus {
    pub fn write(
        &self,
        state: &mut LatchDataState,
        byte: u8,
        regs: &mut RegisterFile,
        model: &ChipModel,
    ) {
        if byte & 0x80 != 0 {
            // Latch byte.
            let chan = self.chan_extract(byte);
            let type_ = self.type_extract(byte);
            let nibble = self.lo_extract(byte);

            state.latched_chan = chan;
            state.latched_type = type_;

            // Find the target register via `routes`.
            let voice = voice_for_chan(model, chan, type_);
            let route_type = if type_ == 0 { RouteType::ToneOrNoise } else { RouteType::Atten };
            let route = self.routes.iter().find(|r| {
                r.rtype == route_type
                    && match r.kind {
                        RouteKind::Any => true,
                        RouteKind::Tone => is_tone_voice(model, chan),
                        RouteKind::Noise => is_noise_voice(model, chan),
                    }
            });

            let Some(route) = route else { return; };
            state.latched_reg = Some(route.target.clone());

            // Compute the latch expression (low bits written into reg).
            let current = regs.get(voice.as_deref(), &route.target);
            let ctx = BusExprContext { model, regs, reg_value: Some(current), nibble: Some(nibble) };
            let expr = self.latch_exprs.get(&route.target);
            let new_val = match expr {
                Some(e) => e.eval(&ctx).map(|v| v as u32).unwrap_or(current),
                None => current,
            };
            regs.set(voice.as_deref(), &route.target, new_val);
        } else {
            // Data byte.
            let hi = self.hi_extract(byte);
            let Some(target) = &state.latched_reg else { return; };

            // Data bytes only apply to `div` (or any register with a `data` expr).
            let Some(expr) = self.data_exprs.get(target) else { return; };
            let voice = voice_for_chan(model, state.latched_chan, state.latched_type);
            let current = regs.get(voice.as_deref(), target);
            let ctx = BusExprContext { model, regs, reg_value: Some(current), nibble: Some(hi) };
            if let Ok(v) = expr.eval(&ctx) {
                regs.set(voice.as_deref(), target, v as u32);
            }
        }

        // Apply any on_write actions (e.g. reset LFSR on noise write).
        for action in &self.on_write {
            if state.latched_reg.as_deref() == Some(&action.register) {
                // Action is dispatched by the runtime, not the bus.
                // See CoreState::apply_bus_action.
            }
        }
    }

    fn chan_extract(&self, byte: u8) -> u8 {
        match &self.chan_bits {
            Some(br) => ((byte >> br.lo) & br.mask()) as u8,
            None => 0,
        }
    }
    fn type_extract(&self, byte: u8) -> u8 {
        match &self.type_bit {
            Some(br) => ((byte >> br.lo) & 1) as u8,
            None => 0,
        }
    }
    fn lo_extract(&self, byte: u8) -> u32 {
        match &self.lo_bits {
            Some(br) => ((byte >> br.lo) & br.mask()) as u32,
            None => 0,
        }
    }
    fn hi_extract(&self, byte: u8) -> u32 {
        match &self.hi_bits {
            Some(br) => ((byte >> br.lo) & br.mask()) as u32,
            None => 0,
        }
    }
}

fn voice_for_chan(model: &ChipModel, chan: u8, type_: u8) -> Option<String> {
    // chan=0..2 -> tone1..3 for type 0; chan=3 -> noise.
    // For type 1 (atten) same mapping but targeting `att` register on that voice.
    let _ = type_;
    match chan {
        0..=2 => model.voices.get(chan as usize).map(|v| v.name.clone()),
        3 => model.voices.iter().find(|v| matches!(v.kind, crate::formats::chip::chip::VoiceKind::Noise)).map(|v| v.name.clone()),
        _ => None,
    }
}

fn is_tone_voice(model: &ChipModel, chan: u8) -> bool {
    model.voices.get(chan as usize).map(|v|
        matches!(v.kind, crate::formats::chip::chip::VoiceKind::Tone)
    ).unwrap_or(false)
}

fn is_noise_voice(model: &ChipModel, chan: u8) -> bool {
    model.voices.get(chan as usize).map(|v|
        matches!(v.kind, crate::formats::chip::chip::VoiceKind::Noise)
    ).unwrap_or(false)
}

/// Addr_data protocol: address = base + (ch % chan_per_port) * stride
///                       + op_offset[op+1]; port = ch / chan_per_port.
pub fn addr_data_compute(
    base: u8,
    port: u8,
    chan_per_port: u8,
    chan_stride: u8,
    ch: u8,
    op: Option<u8>,
    op_offsets: &BTreeMap<u8, u8>,
) -> Option<(u8, u8)> {
    if port != ch / chan_per_port { return None; }
    let mut addr = base.wrapping_add((ch % chan_per_port).wrapping_mul(chan_stride));
    if let Some(op1) = op {
        addr = addr.wrapping_add(*op_offsets.get(&op1)?);
    }
    Some((port, addr))
}