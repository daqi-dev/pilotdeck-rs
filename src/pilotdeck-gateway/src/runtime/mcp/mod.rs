//! MCP host ports.
//!
//! A6.5-pr11 owns only the SDK MCP configuration state. The MCP client and
//! runtime are A6.5-pr10; [`sdk`] therefore talks to a resolved project
//! runtime through injected ports and never connects to a server.

pub mod sdk;
