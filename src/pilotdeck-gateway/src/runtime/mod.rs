//! Gateway-owned runtime services. Project-scoped resource generations live
//! in `project`; package implementations live in their own modules and
//! register a resource group with `project::ResourceBundle`.
pub mod async_hooks;
pub mod model;
pub mod project;
pub mod steer;
