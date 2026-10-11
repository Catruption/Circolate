//! Atomics shared between the audio callback and the UI.

use std::sync::atomic::{AtomicBool, AtomicI64};

pub(crate) struct Transport {
    pub(crate) paused: AtomicBool,
    pub(crate) seek_to: AtomicI64,
    pub(crate) position: AtomicI64,
    pub(crate) done: AtomicBool,
    pub(crate) stop: AtomicBool,
    pub(crate) active_voices: AtomicI64,
    pub(crate) loaded_lines: AtomicI64,
}
