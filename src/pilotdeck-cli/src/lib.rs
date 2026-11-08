//! Minimal transport policy shared by the CLI binaries.
#[cfg(unix)]
pub use pilotdeck_gateway::atomic_file;
pub mod tui;
pub mod update;
pub mod ws_config;

#[cfg(unix)]
pub mod management;

#[cfg(unix)]
pub mod runtime_record;
