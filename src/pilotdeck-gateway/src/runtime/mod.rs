//! Gateway-owned runtime services. Project-scoped resource generations live
//! in `project`; package implementations live in their own modules and
//! register a resource group with `project::ResourceBundle`.
pub mod async_hooks;
pub mod background;
pub mod context;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod permission;
pub mod plugins;
pub mod project;
pub mod refresh;
pub mod sdk_session;
pub mod snapshot;
pub mod steer;
pub mod tools;
