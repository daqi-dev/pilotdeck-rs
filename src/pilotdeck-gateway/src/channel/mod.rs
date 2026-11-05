//! Channel component: registration/config loading, lifecycle, the A7 inbound
//! port and concrete channel adapters (A8.3 `api-server` and `webhook`).
pub mod api_server;
pub mod im_cron;
pub mod loader;
pub mod protocol;
pub mod runtime_status;
pub mod server;
pub mod sms;
#[cfg(test)]
mod tests;
pub mod turn_submitter;
pub mod webhook;
pub mod weixin;
