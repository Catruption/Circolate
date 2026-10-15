//! Primitives shared by the tracker views.  Anything a horizontal and
//! a vertical view both need — braille rasterisation, note-state
//! styling, note formatting, waveform drawing, label rendering,
//! hit-test plumbing — goes here.

pub mod braille;
pub mod hit;
pub mod label;
pub mod note_fmt;
pub mod note_style;
pub mod waveform;