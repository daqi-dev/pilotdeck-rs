//! TUI state and command model. Transport and terminal integration are separate.
pub mod commands;
#[cfg(unix)]
pub mod permission_settings;
pub mod state;
pub mod transport;

#[cfg(unix)]
pub mod runner;

#[cfg(unix)]
mod chat_layout;

#[cfg(unix)]
mod help;
