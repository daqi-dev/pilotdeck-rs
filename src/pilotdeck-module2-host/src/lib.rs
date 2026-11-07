//! PD-owned host-side Module2 DTOs and dispatch. Runtime effects use the public Kernel SDK.
pub mod clock;
pub mod dispatch;
mod host_methods;
pub mod protocol;
pub use host_methods::{HOST_METHODS, HostMethod, host_method};
#[derive(Debug)]
pub struct LcError {
    pub code: Option<i64>,
    pub message: String,
}
/// Legacy-shaped marker used only by the generic frame driver; native adapters never call it.
pub trait KernelClient: Send + Sync {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, LcError>;
}
