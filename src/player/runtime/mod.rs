//! Engine setup shared by live playback and offline rendering.
//!
//! There is exactly one installer.  A song must get the same hosts,
//! gains and part registrations whether it is heard through cpal or
//! rendered to a WAV.

pub mod install;

pub use install::{find_share_dir, install_all, InstallOptions};
