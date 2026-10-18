//! Gateway-owned runtime services. Project-scoped resource generations live
//! in `project`; package implementations live in their own modules and
//! register a resource group with `project::ResourceBundle`.
pub mod async_hooks;
pub mod context;
pub mod memory;
pub mod model;
pub mod permission;
pub mod project;
pub mod refresh;
pub mod snapshot;
pub mod steer;
pub mod tools;
