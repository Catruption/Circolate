//! Atomics shared between the audio callback and the UI.

use std::sync::atomic::{AtomicBool, AtomicI64};

/// Playback speed, in multiples of the base rate.  0 is stopped,
/// positive is forward, negative is reverse.  The UI sets this; the
/// audio callback reads it.
pub(crate) struct Transport {
    pub(crate) speed: AtomicI64,
    pub(crate) seek_to: AtomicI64,
    pub(crate) position: AtomicI64,
    pub(crate) done: AtomicBool,
    pub(crate) stop: AtomicBool,
    pub(crate) active_voices: AtomicI64,
    pub(crate) loaded_lines: AtomicI64,
    /// Incremented by the loader worker each time a file reload is
    /// swapped in.  The TUI polls this and flashes a "reloaded"
    /// indicator for a second.  Never printed — stderr writes during
    /// playback scramble the terminal.
    pub(crate) reload_count: AtomicI64,
    /// Set by the TUI every tick: true when the song that is playing
    /// will repeat once it ends (queue loop count > 1).  The audio
    /// callback only wraps seamlessly while this is set.
    pub(crate) loop_next: AtomicBool,
    /// Incremented by the audio callback each time it wraps the song
    /// back to 0:00 itself.  The TUI watches it to count the loop down.
    pub(crate) loop_wraps: AtomicI64,
}