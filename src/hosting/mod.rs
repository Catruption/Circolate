//! Hosting: turn a resolved plugin spec into audio.
//!
//! Knows *how* to make sound.  Does not know where a spec came from —
//! that's `crate::deps`.
pub mod native;
pub mod sf2;
pub mod vst;
