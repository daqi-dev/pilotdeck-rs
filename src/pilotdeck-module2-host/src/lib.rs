//! Module2 宿主侧原协议验证、显式互操作 DTO 和通用派发接缝。
//! 本包不执行模型、工具、审批或资源清理；调用方提供 HostExchange。
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
/// 保留共享 frame driver 使用的历史端口名称；本 trait 不提供连接或授权实现。
pub trait KernelClient: Send + Sync {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, LcError>;
}
