//! Gateway-owned runtime services. Project-scoped resource generations live
//! in `project`; package implementations live in their own modules and
//! register a resource group with `project::ResourceBundle`.
pub mod model;
pub mod project;
pub mod snapshot;
pub mod steer;
