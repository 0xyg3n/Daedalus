//! Shared native application components for the interface, CLI and services.
//! The existing TUI remains the presentation layer.
pub mod adapters;
pub mod app;
pub mod core;
pub mod protocol;

pub mod platform;

#[cfg(test)]
pub(crate) mod test_support;
