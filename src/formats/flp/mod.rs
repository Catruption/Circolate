//! FL Studio project support.
//!
//! Parsing, the source model, format facts, and Halotext conversion live
//! together here so FLP-specific changes have one home.

pub mod audio;
pub mod convert;
pub mod fidelity;
pub mod model;
pub mod spec;
pub mod slicex;
mod parse;

pub(crate) const MIN_EMBEDDED_BYTES: usize = 512;

pub use parse::parse_file;
