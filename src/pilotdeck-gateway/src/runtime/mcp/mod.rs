//! MCP host ports.
//!
//! - [`client`], [`protocol`], [`naming`] (A6.5-pr10a): the single-server
//!   client (stdio and plain-HTTP transports in `wire`/`http`), its
//!   wire/config types and tool naming.
//! - [`sdk`] (A6.5-pr11): SDK MCP configuration state; it talks to a resolved
//!   project runtime through injected ports and never connects to a server.
//!
//! The runtime and tool bridge on top of [`client`] are A6.5-pr10b.

pub mod client;
mod http;
pub mod naming;
pub mod protocol;
pub mod sdk;
mod wire;
