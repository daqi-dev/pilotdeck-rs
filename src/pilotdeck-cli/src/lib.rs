//! Minimal transport policy shared by the CLI binaries.
#[cfg(unix)]
pub mod atomic_file;
pub mod tui;
pub mod ws_config;
