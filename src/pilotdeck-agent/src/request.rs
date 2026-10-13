//! Per-request context assembly state, separate from routing and turn policy.
use crate::ProjectedMessage;
use crate::ports::{
    CallControl, Captured, ContextPrepare, HostPort, ModelMetadata, PortResult, Route,
    ToolDescriptor,
};
use crate::turn::{TurnInput, TurnRuntimeConfig};
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;
#[derive(Debug, Clone)]
pub struct RequestPreparationState {
    pub tools: Vec<ToolDescriptor>,
    pub metadata: Vec<Captured<ModelMetadata>>,
    pub plan_todo_addendum: Option<JsString>,
    pub elicitation_available: bool,
    pub token_caps: Vec<RouteTokenCaps>,
}
#[derive(Debug, Clone)]
pub struct PreparedRequestIntent {
    pub route: Route,
    pub request: BoundarySnapshot,
    pub diagnostics: Vec<BoundarySnapshot>,
    pub has_system_prompt: bool,
}
/// The original announces loaded instructions after context acknowledgement,
/// before media/cache finalization can fail. The composition owner supplies
/// this notice port; request assembly neither dispatches hooks nor publishes IO.
pub trait RequestPreparationObserver {
    fn instructions_loaded(&mut self, has_system_prompt: bool) -> PortResult<()>;
}
pub struct RequestAssemblyInput<'a> {
    pub control: &'a CallControl,
    pub config: &'a TurnRuntimeConfig,
    pub input: &'a TurnInput,
    pub messages: &'a [ProjectedMessage],
}
pub trait RequestAssembler {
    fn assemble(
        &self,
        state: &mut RequestPreparationState,
        host: &mut dyn HostPort,
        parameters: RequestAssemblyInput<'_>,
        observer: &mut dyn RequestPreparationObserver,
    ) -> PortResult<PreparedRequestIntent>;
}
pub struct OriginalRequestAssembler;

/// Original per-call request options. Preview and notification suppression are
/// independent: compaction previews suppress the notice without mutating host
/// lifecycle state. The ordinary request retains the default notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestAssemblyOptions {
    pub preview_only: Option<bool>,
    pub emit_instruction_events: bool,
}
impl Default for RequestAssemblyOptions {
    fn default() -> Self {
        Self {
            preview_only: None,
            emit_instruction_events: true,
        }
    }
}
impl OriginalRequestAssembler {
    /// AgentLoop-owned limits, distinct from raw request fallback fields.
    pub fn compaction_limits(
        &self,
        state: &RequestPreparationState,
        config: &TurnRuntimeConfig,
        route: &Route,
        pre_route: bool,
    ) -> (f64, f64) {
        crate::request_assembly::compaction_limits(state, config, route, pre_route)
    }
    /// Pure source prompt intent; budget evaluators need this alongside their
    /// preview request to rematerialize changed candidate message windows.
    pub fn context_preparation(
        &self,
        state: &RequestPreparationState,
        parameters: RequestAssemblyInput<'_>,
    ) -> PortResult<ContextPrepare> {
        crate::request_assembly::context_preparation(state, parameters, None)
    }
    pub fn assemble_with_options(
        &self,
        state: &mut RequestPreparationState,
        host: &mut dyn HostPort,
        parameters: RequestAssemblyInput<'_>,
        options: RequestAssemblyOptions,
        observer: &mut dyn RequestPreparationObserver,
    ) -> PortResult<PreparedRequestIntent> {
        crate::request_assembly::assemble_with_options(state, host, parameters, options, observer)
    }
}

/// Frozen read projection of session-owned recovery caps. Assembly reads it;
/// only the owning turn recovery machine may replace cap state.
#[derive(Debug, Clone)]
pub struct RouteTokenCaps {
    pub route: Route,
    pub max_context_tokens: Option<f64>,
    pub attempt_max_output_tokens: Option<f64>,
    pub requested_max_output_tokens: Option<f64>,
    pub session_max_output_tokens: Option<f64>,
    pub hard_max_output_tokens: Option<f64>,
}

impl RequestAssembler for OriginalRequestAssembler {
    fn assemble(
        &self,
        state: &mut RequestPreparationState,
        host: &mut dyn HostPort,
        parameters: RequestAssemblyInput<'_>,
        observer: &mut dyn RequestPreparationObserver,
    ) -> PortResult<PreparedRequestIntent> {
        request_assembly_bridge(state, host, parameters, observer)
    }
}
fn request_assembly_bridge(
    state: &mut RequestPreparationState,
    host: &mut dyn HostPort,
    parameters: RequestAssemblyInput<'_>,
    observer: &mut dyn RequestPreparationObserver,
) -> PortResult<PreparedRequestIntent> {
    crate::request_assembly::assemble(
        state,
        host,
        parameters.control,
        parameters.config,
        parameters.input,
        parameters.messages,
        observer,
    )
}
