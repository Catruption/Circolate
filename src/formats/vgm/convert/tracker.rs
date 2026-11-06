//! Note data. Event detection and register tracking live in `analyze.rs`.

use super::regs::TimbreSnapshot;

pub(crate) struct Note {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) pitch: f64,
    pub(crate) pitch_points: Vec<PitchPoint>,
    pub(crate) vol: f64,
    pub(crate) vol_points: Vec<VolumePoint>,
    pub(crate) timbre: TimbreSnapshot,
    pub(crate) timbre_hash: u64,
}

pub(crate) struct PitchPoint {
    pub(crate) sample: u32,
    pub(crate) pitch: f64,
}

pub(crate) struct VolumePoint {
    pub(crate) sample: u32,
    pub(crate) volume: f64,
}
