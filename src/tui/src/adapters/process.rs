//! Bounded, owned subprocesses for short-lived adapter queries.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::run as run_terminal;
#[cfg(unix)]
pub use unix::{Output, run, run_with_input};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Output, run, run_terminal, run_with_input};
