//! Host-owned SOP orchestration ports. No sidecar payload can create bindings.
use crate::host::{ExecuteRequest, ExecuteResult, HostError, modules::{capability, context, model}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) type SopFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub(crate) struct SopLoopInput {
    pub request: ExecuteRequest,
    pub signal: crate::host::Cancellation,
    pub visible_messages: Vec<JsValue>,
}
pub(crate) enum SopNodeContext {
    Inherit,
    Isolated { sop: JsString, node: JsString },
}
/// Wrappers belong to the trusted controller state, never JSON/wire markers.
pub(crate) trait SopRunBinding: Send + Sync {
    fn tools(&self, base: Arc<dyn capability::ToolExecution>) -> Arc<dyn capability::ToolExecution> { base }
    fn materializer(&self, base: Option<Arc<dyn model::RequestMaterializer>>) -> Option<Arc<dyn model::RequestMaterializer>> { base }
    fn model(&self, base: Arc<dyn model::ModelExecution>) -> Arc<dyn model::ModelExecution> { base }
    fn context(&self, base: Arc<dyn context::ContextRuntime>) -> Arc<dyn context::ContextRuntime> { base }
}
pub(crate) struct SopNodePlan {
    pub context: SopNodeContext,
    pub request: ExecuteRequest,
    pub instruction: JsString,
    pub model_override: Option<JsObject>,
    pub step: JsValue,
    pub binding: Option<Arc<dyn SopRunBinding>>,
}
pub(crate) struct SopNodeOutcome {
    pub result: JsValue,
    pub messages: Vec<JsValue>,
}
pub(crate) type SopModelValidator = Arc<dyn Fn(JsObject) -> crate::host::HostFuture<()> + Send + Sync>;
pub(crate) trait SopNodeExecutor: Send + Sync {
    fn model_validator(&self) -> Result<SopModelValidator, HostError> {
        Err(HostError::new(crate::host::HostErrorKind::Session, "SOP model validation is unavailable."))
    }
    fn run<'a>(&'a self, plan: SopNodePlan) -> SopFuture<'a, Result<SopNodeOutcome, HostError>>;
    fn append_parent_reply<'a>(&'a self, message: JsValue) -> SopFuture<'a, Result<(), HostError>>;
    fn emit_parent<'a>(&'a self, event: JsValue) -> SopFuture<'a, Result<(), HostError>>;
}
pub(crate) trait HostLoopController: Send + Sync {
    fn run<'a>(&'a self, input: SopLoopInput, native: &'a dyn SopNodeExecutor)
        -> SopFuture<'a, Result<ExecuteResult, HostError>>;
}
