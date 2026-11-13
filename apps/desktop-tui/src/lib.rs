//! Step Studio's TUI surface: the four dashboards §9.4 names.
//!
//! A library rather than only a binary so the read models can be tested against
//! what the daemon actually emits — an integration test cannot reach into a
//! binary crate, and a hand-written sample is exactly the thing that hides
//! contract drift.

pub mod app;
pub mod dashboards;
pub mod model;
