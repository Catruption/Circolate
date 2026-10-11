//! Shared playback engine: project loading, streaming loader, mixer.
//!
//! Split into submodules:
//!   - `project`   : samples.json + SF2 loading, part templates, `Project`, `Loader`
//!   - `engine`    : voice scheduling, mixer, DSP (`Engine`, `Scheduled`, `Msg`)
//!   - `render`    : offline WAV render (`render_offline`, `write_wav`)
//!   - `transport` : atomics shared between the audio callback and the UI

mod engine;
mod project;
mod render;
mod transport;

pub(crate) use engine::{Engine, Msg};
pub(crate) use project::{load_project, Loader, Project, Sample};
pub(crate) use render::{render_offline, write_wav};
pub(crate) use transport::Transport;

