// `sfl2`, the sflt-rs soundfont engine
//
// this is a port of the original C++ implementation,
// which is why you might find it to be more "C-like"
// than an idiomatic implementation would be.

// the `lo` module is an implementation of the types defined by the SF2 spec.
// the `hi` module builds higher-level abstractions on top of these types,
// and implements the actual synthesis.

// this implementation intentionally leaves certain parts of the SF2
// synthesis model up to the front-end, such as:
//
// - reverb & chorus processing
// - panning
// - ExclusiveClass handling
//
// these features are omitted to avoid making the synthesis engine too
// opinionated in terms of buffer format, voice storage, etc.  `sfl2`
// only renders single InstVoices into mono buffers, and any stereo
// processing is the responsibility of the front-end.

mod util;
mod riff;
pub mod scala;

pub mod lo;
pub mod hi;