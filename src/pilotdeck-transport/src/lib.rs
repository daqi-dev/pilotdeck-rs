//! Module2.0 transport adapters, distinct from Gateway1.2 and host policy.
use pilotdeck_agent::ports::{
    CallControl, ExecutionIdentity, HostCall, HostPort, HostReply, PortResult,
};
use pilotdeck_agent::{CapturedModelEvent, ProjectedMessage, ToolExecutionResult};
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub mod default_factory;
pub mod execute;
mod factory_payload;
mod host_operations;
mod model_admission;
mod notifications;
pub mod server;
pub mod stdio;
pub mod system_clock;

#[derive(Debug, Clone)]
pub struct HostExchangeResponse {
    /// Issued by the bound exchange owner from its actual pending-call record;
    /// never reconstructed from untrusted response/payload identity fields.
    pub identity: ExecutionIdentity,
    pub boundary: BoundarySnapshot,
    /// Frozen by the owning reader at the first correlated reply. The stock
    /// stdio consumer's abort listener wins only if cancellation preceded ACK;
    /// a worker's later cancellation check cannot reconstruct that ordering.
    pub cancelled_before_response: bool,
}

/// Bound exchange shell owns correlated IDs, cancellation, exact request cache
/// and acceptance state. Adapter receives a complete ModuleResponse boundary.
/// It never gives host authority to an arbitrary payload identity.
pub trait HostExchange: Send {
    /// SDK module-failure results use two independent wall-clock samples per
    /// tool, rather than the AgentLoop's single missing-result timestamp.
    fn wall_now_iso(&self) -> JsString {
        crate::server::ExecutionClock::now_iso(&crate::system_clock::SystemExecutionClock)
    }
    fn exchange(
        &mut self,
        control: &CallControl,
        module: &'static str,
        operation: &'static str,
        effect: CallEffect,
        payload: JsObject,
    ) -> PortResult<HostExchangeResponse>;
}
#[derive(Debug, Clone)]
pub struct AdvertisedMethods {
    /// Original advertised order/duplicates retained. Methods are consumed only
    /// for exact known module/operation membership, not authority grants.
    pub modules: Vec<(JsString, Vec<JsString>)>,
}
impl AdvertisedMethods {
    pub fn supports(&self, module: &str, operation: &str) -> bool {
        self.modules.iter().any(|(m, operations)| {
            m.equals_str(module) && operations.iter().any(|op| op.equals_str(operation))
        })
    }
}
pub struct RemoteHost<E: HostExchange> {
    exchange: E,
    methods: AdvertisedMethods,
}
impl<E: HostExchange> RemoteHost<E> {
    pub fn new(exchange: E, methods: AdvertisedMethods) -> Self {
        Self { exchange, methods }
    }
    pub fn into_exchange(self) -> E {
        self.exchange
    }
}
impl<E: HostExchange> HostPort for RemoteHost<E> {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        self.methods.supports(module, operation)
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        host_operations::call(&mut self.exchange, &self.methods, control, call)
    }
}
/// Consumer admission retains the full admitted value with its typed view.
/// Lax canonical messages successfully admit BoundaryOnly, not an empty message.
pub fn admit_message(value: &JsValue) -> PortResult<ProjectedMessage> {
    model_admission::message(value)
}
pub fn admit_model_event(value: &JsValue) -> PortResult<CapturedModelEvent> {
    model_admission::event(value)
}
pub fn admit_tool_result(value: &JsValue) -> PortResult<ToolExecutionResult> {
    model_admission::tool_result(value)
}
pub fn admit_model_request(value: &JsValue) -> PortResult<pilotdeck_agent::ports::CapturedRequest> {
    model_admission::request(value)
}

/// Decode the normalized normal-turn view of a source assembler result. The
/// mandatory raw assembled boundary remains the authority for open fields.
pub fn admit_assistant_assembly(
    value: &JsValue,
) -> PortResult<pilotdeck_agent::AssembledAssistantMessage> {
    model_admission::assembled(value)
}
