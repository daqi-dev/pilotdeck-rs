//! Channel component: registration/config loading, lifecycle, the A7 inbound
//! port and concrete channel adapters (A8.3 `api-server`).
pub mod api_server;
pub mod loader;
pub mod protocol;
pub mod server;
#[cfg(test)]
mod tests;
pub mod turn_submitter;
