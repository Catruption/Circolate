//! One file per rendered piece.  Widgets take what they need by
//! reference and never touch `RenderState` directly (except to record
//! hit-test rectangles, which is the mechanism the event loop uses to
//! route clicks back to them).

pub mod cover;
pub mod help;
pub mod notch;
pub mod queue;
pub mod spiral;