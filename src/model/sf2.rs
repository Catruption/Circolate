//! SF2 — parsed data structures.
//!
//! Pure data. No I/O, no parsing.

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

    pub initial_attenuation_cb: i16,
    pub delay_timecents: i16,
    pub attack_timecents: i16,
    pub hold_timecents: i16,
    pub decay_timecents: i16,
    pub sustain_level_cb: i16,
    pub release_timecents: i16,

    pub initial_filter_fc: i16,
    pub initial_filter_q: i16,
}

#[derive(Debug)]
pub struct Sf2File {
    pub samples: Vec<Sf2Sample>,
    pub zones: Vec<Sf2Zone>,
    pub pcm: Vec<i16>,
}