//! Unwired A8.3 channel component: registration/config loading and lifecycle.
//! The A7 inbound port and concrete channel implementations are separate units.
pub mod loader;
pub mod protocol;
pub mod server;
#[cfg(test)]
mod tests;
