//! Player: project loading, streaming, transport, offline render.

pub mod cache;
pub mod engine;
pub mod envelope;
pub mod lines;
pub mod loader;
pub mod notes;
pub mod project;
pub mod render;
pub mod scope;
pub mod stretch;
pub mod template;
pub mod transport;
pub mod types;
pub mod inserts;
pub mod analysis;
pub mod analysis_mir;
pub mod theory;

// Re-exports for call sites that say `player::X` instead of the full
// submodule path.  `pub(crate) use` rather than `pub use` because the
// underlying items are `pub(crate)` — re-exporting them at `pub` was
// the E0365 cluster.
pub(crate) use cache::Cache;
pub(crate) use engine::{Engine, Msg};
pub(crate) use loader::Loader;
pub(crate) use project::{load_project, Project};
pub(crate) use render::{render_offline, write_wav};
pub(crate) use transport::Transport;
pub(crate) use types::Sample;
