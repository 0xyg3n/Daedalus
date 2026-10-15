//! Shared application components for the Rust migration.
//! The existing TUI remains the presentation layer.
pub mod adapters;
pub mod app;
pub mod core;
pub mod protocol;

pub mod platform;

#[cfg(test)]
pub(crate) mod test_support;
