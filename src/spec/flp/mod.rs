//! FL Studio project format — spec tables.
//!
//! Pure data. Event IDs, constants, and type-name mappings for the
//! byte-level FLP reader. No logic, no parsing.

pub mod events;
pub use events::*;
