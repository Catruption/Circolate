//! Backend interface for `ChipPlayer`. `CoreState` implements this via
//! the primitive DSL; `YmfmBackend` implements it by forwarding register
//! writes to `ymfm-sys`.

use crate::hosting::chip::engine::runtime::{CoreState, ScheduledWrite};

pub trait ChipCore: Send {
    fn step(&mut self) -> f32;
    /// Per-voice mono samples from the most recent step, when the backend can
    /// expose them. The normal `step` value remains the full chip mix.
    fn voice_samples(&self) -> &[(String, f32)] { &[] }
    fn schedule(&mut self, w: ScheduledWrite);
    /// Queue a raw chip-register write at chip-clock time.
    fn schedule_register_write(&mut self, _t: f64, _port: u8, _reg: u8, _value: u8) {}
    fn cancel_register_writes_from(&mut self, _t: f64) {}
    fn reserve_register_writes(&mut self, _count: usize) {}
    fn dac_enabled(&self) -> bool { false }
    fn set_field(&mut self, voice: &str, field: &str, value: u32);
    fn voices(&self) -> Vec<String>;
    fn clear_pending(&mut self);
    fn set_time(&mut self, t: f64);
    fn step_rate(&self) -> f64;

    /// Cancel every pending write to `voice` whose time is `>= after_t`.
    /// Used when a channel retriggers so the previous note's scheduled
    /// key-off doesn't kill the new note.
    fn cancel_pending(&mut self, voice: &str, after_t: f64);
}

impl ChipCore for CoreState {
    fn step(&mut self) -> f32 { CoreState::step(self) }
    fn voice_samples(&self) -> &[(String, f32)] { CoreState::voice_samples(self) }
    fn schedule(&mut self, w: ScheduledWrite) { CoreState::schedule(self, w); }
    fn set_field(&mut self, voice: &str, field: &str, value: u32) {
        CoreState::set_field(self, voice, field, value);
    }
    fn voices(&self) -> Vec<String> { CoreState::voices(self) }
    fn clear_pending(&mut self) { CoreState::clear_pending(self); }
    fn set_time(&mut self, t: f64) { CoreState::set_time(self, t); }
    fn step_rate(&self) -> f64 { CoreState::step_rate(self) }
    fn cancel_pending(&mut self, voice: &str, after_t: f64) {
        CoreState::cancel_pending(self, voice, after_t);
    }
}
