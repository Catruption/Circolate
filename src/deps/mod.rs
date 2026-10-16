//! Plugin dependencies: what a project needs, where it lives, and how
//! to substitute when the original isn't available.
//!
//! Knows *what* a dependency is.  Does not know how to make sound —
//! that's `crate::hosting`.

pub mod fetch;
pub mod index;
pub mod registry;
pub mod requires;
pub mod instruments;
