//! MCP host ports.
//!
//! - [`client`], [`protocol`], [`naming`] (A6.5-pr10a): the single-server
//!   client (stdio and plain-HTTP transports in `wire`/`http`), its
//!   wire/config types and tool naming.
//! - [`sdk`] (A6.5-pr11): SDK MCP configuration state; it talks to a resolved
//!   project runtime through injected ports and never connects to a server.
//! - [`gateway`] (#1215): the U6 four-control gateway adapter over
//!   [`sdk::SdkMcpConfig`] and the #1011 session owner; not wired into the
//!   default routing.
//! - [`runtime`], [`bridge`], [`registry`] (A6.5-pr10b): the multi-server
//!   runtime, the MCP-to-tool bridge, and project/session ownership.

pub mod bridge;
pub mod client;
pub mod gateway;
#[cfg(test)]
mod gateway_tests;
mod http;
pub mod naming;
mod output_schema;
pub mod protocol;
pub mod registry;
pub mod runtime;
pub mod sdk;
mod wire;
