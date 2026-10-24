//! Empirical models of Image-Line native effects.
//!
//! Each effect class has a small Rust type that consumes a binary
//! artifact shipped under `share/native_fx/`.  The artifacts are
//! produced offline by the `profile/` scripts and capture the effect's
//! transfer function, impulse response, or filter coefficients.
//!
//! The engine does not know these are "FL effects" — it just sees a
//! `NativeEffect` trait object and calls `process()` on each block.

pub mod waveshaper;

pub use waveshaper::WaveshaperLut;

use anyhow::Result;

/// Common interface for all native effect models.
///
/// Buffers are mono f32.  Stereo is handled by the surrounding chain;
/// a native effect is instantiated once per channel.
pub trait NativeEffect: Send {
    fn process_block(&mut self, buf: &mut [f32]);
    fn reset(&mut self);
}

/// Where shipped native-effect artifacts live, relative to the install
/// root.  `install_root()` is defined in `crate::deps::registry`.
pub fn artifact_dir() -> Option<std::path::PathBuf> {
    crate::deps::registry::native_fx_dir()
}

/// Load a named artifact from the shipped `share/native_fx/` directory.
pub fn artifact_path(name: &str) -> Option<std::path::PathBuf> {
    artifact_dir().map(|d| d.join(name))
}

/// Convenience: load a waveshaper LUT by artifact name (no extension).
pub fn load_waveshaper(name: &str) -> Result<WaveshaperLut> {
    let path = artifact_path(&format!("{name}.lut.bin"))
    .ok_or_else(|| anyhow::anyhow!("install root not found"))?;
    WaveshaperLut::from_file(&path)
}
