//! Turn-bound host capabilities. Domain policy never receives a dispatcher map,
//! provider closure, filesystem handle or mutable host Session.
use crate::{CapturedModelEvent, ProjectedMessage, ToolExecutionResult};
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::model::{
    BoundarySnapshot, CachePlan, CanonicalModelRequest, ModelError, ModelProtocol,
    PreparedWireRequest, ToolSchema, Usage,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Instant;

/// Admission produces both views from the same value. The boundary is used at
/// serialization; extensions/own Undefined are not reconstructed from the view.
#[derive(Debug, Clone)]
pub struct Captured<T> {
    pub view: T,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub route: Route,
    pub normalized: RequestProjection,
    pub wire: PreparedWireRequest,
}
#[derive(Debug, Clone)]
pub enum RequestProjection {
    Complete(Box<CanonicalModelRequest>),
    /// Valid original shallow request may contain projected own text:Undefined
    /// or lax media/messages. Keep and forward it; never invent an empty view.
    BoundaryOnly(Vec<crate::ProjectionGap>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub provider: JsString,
    pub model: JsString,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub run_id: JsString,
    pub operation_id: JsString,
    pub request_id: JsString,
    pub idempotency_key: Option<JsString>,
    pub module_instance_id: JsString,
    pub connection_generation: JsString,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelCause {
    Requested(JsString),
    Deadline,
    TransportClosed,
    OwnerRetired,
}
#[derive(Debug, Default)]
struct CancellationState {
    cause: Mutex<Option<CancellationNotice>>,
    wake: Condvar,
    listeners: Mutex<Vec<Weak<WakeSignal>>>,
    cleanup: Mutex<CleanupCancellationState>,
    /// A cleanup owner's total budget also caps reconciliation after dispatch.
    reconciliation_bound: Option<Instant>,
}
#[derive(Debug, Default)]
struct CleanupCancellationState {
    hard_stop: Option<CancelCause>,
    scopes: Vec<Weak<CancellationState>>,
}
#[derive(Debug, Clone)]
struct CancellationNotice {
    cause: CancelCause,
    observed_at: Instant,
    reconciliation_deadline: Option<Instant>,
}
/// Generation-based wait avoids a lost notification between inspecting a
/// pending reply and sleeping. It conveys readiness only, never authority.
#[derive(Debug, Default)]
pub struct WakeSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}
impl WakeSignal {
    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|p| p.into_inner())
    }
    pub fn notify(&self) {
        let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }
    pub fn wait(&self, observed: u64, deadline: Option<Instant>) {
        let generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(deadline) = deadline {
            drop(
                self.changed
                    .wait_timeout_while(
                        generation,
                        deadline.saturating_duration_since(Instant::now()),
                        |generation| *generation == observed,
                    )
                    .unwrap_or_else(|p| p.into_inner()),
            );
        } else {
            drop(
                self.changed
                    .wait_while(generation, |generation| *generation == observed)
                    .unwrap_or_else(|p| p.into_inner()),
            );
        }
    }
}
/// Reader/control owner can wake a pending host call independently of the loop
/// worker. First cancellation wins; poisoning is not interpreted as permission.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<CancellationState>);
impl Cancellation {
    pub fn cancel(&self, cause: CancelCause) -> bool {
        self.record(cause, None)
    }
    /// A terminal owner may supply one fixed reconciliation bound. Recording
    /// it with the first cause prevents a delayed waiter extending that budget.
    pub fn cancel_with_reconciliation_deadline(
        &self,
        cause: CancelCause,
        deadline: Instant,
    ) -> bool {
        self.record(cause, Some(deadline))
    }
    /// Owner death is retained independently of the first soft cancellation.
    pub fn hard_stop_cause(&self) -> Option<CancelCause> {
        self.0
            .cleanup
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hard_stop
            .clone()
    }
    /// Independent soft-cancellation scope that remains linked to owner death.
    /// Weak children cannot keep either the parent or a discarded scope alive.
    pub fn cleanup_scope(&self) -> Self {
        self.new_cleanup_scope(None)
    }
    /// Cleanup must settle within its own fixed total budget, without gaining
    /// another cancellation grace when that budget expires.
    pub fn bounded_cleanup_scope(&self, deadline: Instant) -> Self {
        self.new_cleanup_scope(Some(deadline))
    }
    fn new_cleanup_scope(&self, reconciliation_bound: Option<Instant>) -> Self {
        let child = Self(Arc::new(CancellationState {
            reconciliation_bound,
            ..CancellationState::default()
        }));
        let hard_stop = {
            let mut cleanup = self.0.cleanup.lock().unwrap_or_else(|p| p.into_inner());
            cleanup.scopes.retain(|scope| scope.strong_count() > 0);
            cleanup.scopes.push(Arc::downgrade(&child.0));
            cleanup.hard_stop.clone()
        };
        if let Some(cause) = hard_stop {
            child.cancel(cause);
        }
        child
    }
    fn record(&self, cause: CancelCause, reconciliation_deadline: Option<Instant>) -> bool {
        if matches!(
            cause,
            CancelCause::OwnerRetired | CancelCause::TransportClosed
        ) {
            let (hard_stop, children) = {
                let mut cleanup = self.0.cleanup.lock().unwrap_or_else(|p| p.into_inner());
                let hard_stop = cleanup
                    .hard_stop
                    .get_or_insert_with(|| cause.clone())
                    .clone();
                let children = cleanup
                    .scopes
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>();
                cleanup.scopes.retain(|scope| scope.strong_count() > 0);
                (hard_stop, children)
            };
            // Never recurse while holding parent cleanup/cause/listener locks.
            for child in children {
                Self(child).cancel(hard_stop.clone());
            }
        }
        let mut state = self.0.cause.lock().unwrap_or_else(|p| p.into_inner());
        if state.is_some() {
            return false;
        }
        *state = Some(CancellationNotice {
            cause,
            observed_at: Instant::now(),
            reconciliation_deadline,
        });
        self.0.wake.notify_all();
        drop(state);
        let mut listeners = self.0.listeners.lock().unwrap_or_else(|p| p.into_inner());
        listeners.retain(|listener| {
            if let Some(listener) = listener.upgrade() {
                listener.notify();
                true
            } else {
                false
            }
        });
        true
    }
    pub fn register_wake(&self, wake: &Arc<WakeSignal>) {
        let mut listeners = self.0.listeners.lock().unwrap_or_else(|p| p.into_inner());
        listeners.retain(|listener| listener.strong_count() > 0);
        let listener = Arc::downgrade(wake);
        if !listeners.iter().any(|existing| existing.ptr_eq(&listener)) {
            listeners.push(listener);
        }
        // The caller registers before reading generation and checking cause.
    }
    pub fn cause(&self) -> Option<CancelCause> {
        self.0
            .cause
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|notice| notice.cause.clone())
    }
    /// Compare the first owning cancellation with an already captured local
    /// receipt instant. Reading a cause later must not change ACK-first into
    /// cancellation-first while a worker is delayed.
    pub fn was_cancelled_by(&self, receipt: Instant) -> bool {
        self.0
            .cause
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|notice| notice.observed_at <= receipt)
    }
    pub fn reconciliation_deadline(&self, grace: std::time::Duration) -> Option<Instant> {
        let cancellation_deadline = self
            .0
            .cause
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|notice| {
                notice
                    .reconciliation_deadline
                    .unwrap_or(notice.observed_at + grace)
            });
        match (self.0.reconciliation_bound, cancellation_deadline) {
            (Some(bound), Some(deadline)) => Some(bound.min(deadline)),
            (bound, deadline) => bound.or(deadline),
        }
    }
    pub fn wait_until(&self, deadline: Instant) -> Option<CancelCause> {
        let state = self.0.cause.lock().unwrap_or_else(|p| p.into_inner());
        let (state, timeout) = self
            .0
            .wake
            .wait_timeout_while(
                state,
                deadline.saturating_duration_since(Instant::now()),
                |c| c.is_none(),
            )
            .unwrap_or_else(|p| p.into_inner());
        if timeout.timed_out() && state.is_none() {
            Some(CancelCause::Deadline)
        } else {
            state.as_ref().map(|notice| notice.cause.clone())
        }
    }
}
#[derive(Debug, Clone)]
pub struct CallControl {
    pub cancellation: Cancellation,
    /// Monotonic local deadline derived once from the admitted host deadline.
    pub deadline: Option<Instant>,
}
impl CallControl {
    pub fn check(&self) -> PortResult<()> {
        if let Some(cause) = self.cancellation.cause() {
            return Err(PortError::Cancelled { cause });
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(PortError::Cancelled {
                cause: CancelCause::Deadline,
            });
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub enum PortError {
    NotAdvertised {
        module: &'static str,
        operation: &'static str,
    },
    InvalidResponse {
        message: JsString,
    },
    /// A source-equivalent consumer exception after a known reply, distinct
    /// from lost acceptance or a provider error. No later tools may advance.
    SourceException {
        message: JsString,
        boundary: BoundarySnapshot,
    },
    /// Known ordinary exception constructed by the pinned host consumer.
    /// `boundary` is diagnostic input, never the thrown Error object; payload
    /// terminal/code properties cannot change this explicit classification.
    KnownConsumerException {
        message: JsString,
        boundary: BoundarySnapshot,
    },
    /// The pinned stdio server constructed a terminal capability Error after
    /// successful message coercion. Its optional string code belongs to that
    /// thrown Error; the raw response remains diagnostic provenance only.
    KnownTerminalException {
        message: JsString,
        code: Option<JsString>,
        boundary: BoundarySnapshot,
    },
    HostFailure {
        code: JsString,
        message: JsString,
        boundary: BoundarySnapshot,
    },
    Provider(Box<Captured<ModelError>>),
    /// Original module consumer classifies a canonical object by string
    /// code/message alone. Missing provider/protocol/retryable must not invent
    /// defaults or change it into a non-provider failure.
    ProviderBoundaryOnly {
        code: JsString,
        message: JsString,
        boundary: BoundarySnapshot,
    },
    Cancelled {
        cause: CancelCause,
    },
    TransportBeforeDispatch {
        message: JsString,
    },
    /// Advancing calls and accepted effects are never converted to an ordinary
    /// model-visible tool error or automatically dispatched with a new identity.
    AcceptedUnknown {
        identity: Box<ExecutionIdentity>,
        last_applied_sequence: Option<f64>,
    },
}
pub type PortResult<T> = Result<T, PortError>;

#[derive(Debug, Clone)]
pub struct ModelMetadata {
    pub route: Route,
    pub max_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
    pub token_limits: Option<TokenLimits>,
    pub protocol: Option<ModelProtocol>,
    pub supports_prompt_cache: Option<bool>,
}
#[derive(Debug, Clone)]
pub struct TokenLimits {
    pub max_context_tokens: f64,
    pub max_output_tokens: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequestHandle(pub JsString);
#[derive(Debug, Clone)]
pub struct ModelHints {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub execution: ExecutionHints,
    pub workspace_id: Option<JsString>,
    pub storage_config_version: Option<JsString>,
    pub metadata: Option<JsObject>,
    pub model_override: Option<Route>,
}
#[derive(Debug, Clone)]
pub struct PreparedInvocation {
    /// A host-owned native routing object is referenced by this opaque ID.
    /// It is also the stream_next/close_stream preparationId; no start_stream.
    pub handle: PreparedRequestHandle,
    pub request: CapturedRequest,
    pub route: Route,
    pub max_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
    pub metadata: Option<Captured<ModelMetadata>>,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub events: Vec<StreamEvent>,
    pub done: bool,
}
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Known(Box<CapturedModelEvent>),
    /// Original JS string-buffer coercion can admit a delta outside the
    /// normalized string event domain. The live echo stays unchanged; the
    /// checked reducer capture is its source-equivalent buffer contribution.
    CoercedDelta {
        boundary: BoundarySnapshot,
        reducer: Box<CapturedModelEvent>,
    },
    /// Source buffer/property coercion can throw after forwarding the live
    /// event. Keep that location in the batch instead of discarding its prefix.
    DeferredSourceException {
        boundary: BoundarySnapshot,
        message: JsString,
    },
    /// A known error event whose original lax payload cannot form ModelError.
    /// It still flushes buffers, sets finishError and terminates the invocation.
    ErrorBoundaryOnly {
        boundary: BoundarySnapshot,
        error: BoundarySnapshot,
    },
    /// Original assembler ignores unrecognized event types while the driver
    /// forwards the complete live provider event. No fabricated known event.
    BoundaryOnly(BoundarySnapshot),
}
#[derive(Debug, Clone)]
pub struct TokenCalibration {
    pub route: Route,
    pub actual_input_tokens: f64,
    pub estimated_input_tokens: f64,
}
#[derive(Debug, Clone)]
pub struct BudgetOptions {
    pub max_context_tokens: f64,
    pub reserved_output_tokens: Option<f64>,
    pub use_provider_count: Option<bool>,
    pub calibration: Option<TokenCalibration>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetState {
    Ok,
    Warning,
    Blocking,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateSource {
    Estimator,
    Usage,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    Provider,
    Calibrated,
    Local,
}
#[derive(Debug, Clone)]
pub struct BudgetBreakdown {
    pub total: f64,
    pub system: f64,
    pub tools: f64,
    pub messages: f64,
    pub mcp: f64,
    pub memory: f64,
}
#[derive(Debug, Clone)]
pub struct BudgetSnapshot {
    pub tokens: f64,
    pub max_context_tokens: f64,
    pub warning_ratio: f64,
    pub blocking_ratio: f64,
    pub ratio: f64,
    pub state: BudgetState,
    pub local_estimate_tokens: Option<f64>,
    pub display_tokens: Option<f64>,
    pub estimate_source: Option<EstimateSource>,
    pub usage_tokens: Option<f64>,
    pub calibration_actual_input_tokens: Option<f64>,
    pub calibration_estimated_input_tokens: Option<f64>,
    pub total_context_tokens: Option<f64>,
    pub effective_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
    pub source: Option<BudgetSource>,
    pub exact: Option<bool>,
    pub reserved_output_tokens: Option<f64>,
    pub estimator_error: Option<JsString>,
    pub breakdown: Option<BudgetBreakdown>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    Plan,
    BypassPermissions,
}
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: JsString,
    pub description: JsString,
    pub kind: JsString,
    pub input_schema: JsObject,
    pub read_only: bool,
    pub concurrency_safe: bool,
    pub requires_user_interaction: bool,
    pub required_runtime_capabilities: Option<Vec<JsString>>,
}
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub tool_call_id: JsString,
    pub name: JsString,
    pub arguments: JsValue,
}
#[derive(Debug, Clone)]
pub struct ToolHints {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub cwd: JsString,
    pub permission_mode: PermissionMode,
    pub run_mode: Option<JsString>,
    pub can_prompt: bool,
    pub can_elicit: Option<bool>,
    pub additional_working_directories: Vec<JsString>,
    /// Explicit tool-runtime extensions; host derives authority from its binding.
    pub context_extensions: JsObject,
    pub execution: ExecutionHints,
}
#[derive(Debug, Clone)]
pub struct ExecutionHints {
    pub run_id: JsString,
    pub operation_id: JsString,
    pub idempotency_key: Option<JsString>,
    pub operation_deadline: Option<JsString>,
}
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    Allow {
        reason: BoundarySnapshot,
        updated_input: Option<JsValue>,
    },
    Deny {
        reason: BoundarySnapshot,
        message: JsString,
    },
    Ask {
        reason: BoundarySnapshot,
        request: BoundarySnapshot,
    },
    Cancel {
        reason: BoundarySnapshot,
        message: JsString,
    },
}
#[derive(Debug, Clone)]
pub struct SteerMessage {
    pub item_id: JsString,
    pub message: ProjectedMessage,
    pub allowed_read_files: Option<Vec<JsString>>,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub struct SteerDrain {
    pub messages: Vec<SteerMessage>,
    pub closed: bool,
}
#[derive(Debug, Clone)]
pub struct CompactBoundary {
    /// Actual control-boundary payload. Snapshot + replacement are one owner
    /// commit; later bracket/turn_result are separate records.
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub struct CompactionCommit {
    /// Successful port response only. Driver MUST check control again after ack;
    /// original callbacks can abort and still resolve. This is not fsync proof.
    pub response: BoundarySnapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeContextSurface {
    SystemPrompt,
    UserMessage,
}
#[derive(Debug, Clone)]
pub struct ContextPrepare {
    pub preview_only: Option<bool>,
    pub session_id: JsString,
    pub turn_id: JsString,
    pub step_id: Option<f64>,
    pub cwd: JsString,
    pub runtime_context_surface: Option<RuntimeContextSurface>,
    pub route: Route,
    pub protocol: Option<ModelProtocol>,
    pub supports_prompt_cache: Option<bool>,
    // Original permits open labels here; bound host policy overrides them.
    pub permission_mode: JsString,
    pub run_mode: Option<JsString>,
    pub additional_working_directories: Vec<JsString>,
    pub messages: Vec<ProjectedMessage>,
    pub tools: Vec<ToolSchema>,
    pub custom_system_prompt: Option<JsString>,
    pub append_system_prompt: Option<JsString>,
    pub max_messages: Option<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
    Fatal,
}
#[derive(Debug, Clone)]
pub struct ContextDiagnostic {
    pub code: JsString,
    pub severity: DiagnosticSeverity,
    pub message: JsString,
    pub path: Option<JsString>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBoundaryKind {
    Compact,
    Microcompact,
    Snip,
}
#[derive(Debug, Clone)]
pub struct ContextBoundary {
    pub kind: ContextBoundaryKind,
    pub retained_messages: f64,
    pub metadata: Option<JsObject>,
}
#[derive(Debug, Clone)]
pub struct RuntimeContextSection {
    pub name: JsString,
    pub text: JsString,
}
#[derive(Debug, Clone)]
pub struct InstructionLayer {
    pub scope: JsString,
    pub path: JsString,
    pub content: JsString,
}
#[derive(Debug, Clone)]
pub struct ContextMaterialization {
    pub step_id: Option<f64>,
    pub prompt_generation: Option<f64>,
    pub runtime_contexts: Vec<RuntimeContextSection>,
    pub runtime_context_messages: Option<Vec<ProjectedMessage>>,
    pub instruction_layers: Option<Vec<InstructionLayer>>,
}
#[derive(Debug, Clone)]
pub struct PreparedContext {
    pub messages: Vec<ProjectedMessage>,
    pub system_prompt: Option<JsString>,
    pub system_prompt_parts: Vec<JsString>,
    pub tools: Vec<ToolSchema>,
    pub diagnostics: Vec<ContextDiagnostic>,
    pub boundaries: Vec<ContextBoundary>,
    pub metadata: Option<JsObject>,
    pub materialization: Option<ContextMaterialization>,
    pub cache_breakpoints: Option<Vec<f64>>,
    pub cache_plan: Option<CachePlan>,
}
#[derive(Debug, Clone)]
pub struct SupplementalMessage {
    pub tool_call_id: JsString,
    pub message: ProjectedMessage,
}
#[derive(Debug, Clone)]
pub struct ApplyToolResults {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub tool_result_message: ProjectedMessage,
    pub supplemental_messages: Option<Vec<SupplementalMessage>>,
    pub messages: Vec<ProjectedMessage>,
}
#[derive(Debug, Clone)]
pub struct AppliedToolResults {
    pub messages: Vec<ProjectedMessage>,
    pub appended_messages: Option<Vec<ProjectedMessage>>,
    pub diagnostics: Vec<ContextDiagnostic>,
}
#[derive(Debug, Clone)]
pub struct RecoverModelError {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub error: Captured<ModelError>,
    pub messages: Vec<ProjectedMessage>,
    pub has_attempted_compact: bool,
}
/// Original recovery probes observe the complete captured error boundary.
/// This call never classifies or narrows provider data before host dispatch.
#[derive(Debug, Clone)]
pub struct RecoverModelErrorBoundary {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub error: BoundarySnapshot,
    pub messages: Vec<ProjectedMessage>,
    pub has_attempted_compact: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputLimitScope {
    HardCap,
    Attempt,
}
#[derive(Debug, Clone)]
pub enum RecoveryDecision {
    TruncateHeadAndRetry {
        keep_ratio: f64,
        reason: JsString,
    },
    AdjustOutputAndRetry {
        max_output_tokens: f64,
        reason: JsString,
        scope: Option<OutputLimitScope>,
    },
    CompactAndRetry {
        max_context_tokens: Option<f64>,
        max_output_tokens: Option<f64>,
        reason: JsString,
    },
    StripImagesAndRetry {
        reason: JsString,
    },
    GiveUp {
        reason: JsString,
    },
}
#[derive(Debug, Clone)]
pub struct CaptureTurn {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub messages: Vec<ProjectedMessage>,
    pub errored: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStage {
    PreRoute,
    Routed,
    Recovery,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    Auto,
    Reactive,
    Manual,
}
#[derive(Debug, Clone)]
pub struct CompactionIntent {
    pub session_id: Option<JsString>,
    pub turn_id: Option<JsString>,
    pub stage: BudgetStage,
    pub trigger: CompactionTrigger,
    pub messages: Vec<ProjectedMessage>,
    pub max_context_tokens: Option<f64>,
    pub reserved_output_tokens: Option<f64>,
    pub manual_force: Option<bool>,
    pub allow_fallback_on_failure: Option<bool>,
    pub budget_request: Option<CapturedRequest>,
    pub preparation: Option<ContextPrepare>,
    pub prepared_handle: Option<PreparedRequestHandle>,
    pub calibration: Option<TokenCalibration>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTier {
    Micro,
    Snip,
    Full,
    Emergency,
}
#[derive(Debug, Clone)]
pub enum AutoCompactResult {
    Skipped {
        snapshot: Captured<BudgetSnapshot>,
    },
    Compacted {
        messages: Vec<ProjectedMessage>,
        tier: CompactionTier,
        snapshot: Captured<BudgetSnapshot>,
        /// Original full summary/attachments/hook diagnostics, immutable echo.
        result: Option<BoundarySnapshot>,
        overflow_after_emergency: bool,
    },
}
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub id: Option<JsString>,
    pub content: JsString,
    pub status: TodoStatus,
    pub priority: Option<JsString>,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct TodoUpdate {
    pub id: Option<JsString>,
    pub content: Option<JsString>,
    pub status: Option<TodoStatus>,
    pub priority: Option<JsString>,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub enum PlanTodoMutation {
    Read,
    MarkPlanApproved {
        plan: JsString,
    },
    RecordTodoWrite {
        markdown: JsString,
        todos: Vec<TodoItem>,
        reason: Option<JsString>,
    },
    WriteTodos {
        todos: Vec<TodoUpdate>,
        markdown: Option<JsString>,
        merge: Option<bool>,
        reason: Option<JsString>,
    },
    MarkToolProgress {
        tool_name: JsString,
    },
}
#[derive(Debug, Clone)]
pub struct PlanTodoRequest {
    pub session_id: JsString,
    pub turn_id: JsString,
    pub mutation: PlanTodoMutation,
}
#[derive(Debug, Clone)]
pub struct PlanTodoSnapshot {
    pub approved_plan: Option<JsString>,
    pub requires_initialization: bool,
    pub tool_calls_since_last_todo_write: f64,
    pub last_markdown: Option<JsString>,
    pub todos: Vec<TodoItem>,
    pub active_todos: Vec<TodoItem>,
    pub todo_history: Vec<Captured<TodoWriteHistory>>,
    pub todo_diagnostics: Captured<TodoDiagnostics>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoWriteMode {
    Markdown,
    Structured,
}
#[derive(Debug, Clone)]
pub struct TodoLastWrite {
    pub mode: TodoWriteMode,
    pub merge: bool,
    pub reason: Option<JsString>,
    pub added_count: f64,
    pub removed_count: f64,
    pub changed_count: f64,
    pub deleted_open_item_count: f64,
    pub large_rewrite: bool,
    pub all_completed: bool,
}
#[derive(Debug, Clone)]
pub struct TodoDiagnostics {
    pub write_count: f64,
    pub todo_count: f64,
    pub active_count: f64,
    pub completed_count: f64,
    pub cancelled_count: f64,
    pub large_rewrite_count: f64,
    pub deleted_open_item_count: f64,
    pub completed_without_active_count: f64,
    pub last_write: Option<TodoLastWrite>,
}
#[derive(Debug, Clone)]
pub struct TodoWriteHistory {
    pub created_at: JsString,
    pub mode: TodoWriteMode,
    pub merge: bool,
    pub reason: Option<JsString>,
    pub markdown: Option<JsString>,
    pub todos: Vec<TodoItem>,
    pub diagnostics: Captured<TodoDiagnostics>,
}
#[derive(Debug, Clone)]
pub enum HookEffect {
    /// The original dispatcher validates effect arrays, not each effect. Keep
    /// source-valid unknown/lax values for source-equivalent consumer handling.
    BoundaryOnly {
        boundary: BoundarySnapshot,
    },
    AdditionalContext {
        content: JsString,
        source: JsString,
    },
    SystemMessage {
        content: JsString,
    },
    Block {
        reason: JsString,
        stop_reason: Option<JsString>,
    },
    PermissionDecision {
        behavior: JsString,
        reason: Option<JsString>,
    },
    UpdatedToolInput {
        input: JsObject,
    },
    UpdatedMcpToolOutput {
        output: JsValue,
    },
    PermissionRequestResult {
        result: BoundarySnapshot,
    },
    InitialUserMessage {
        message: JsString,
    },
    WatchPaths {
        paths: Vec<JsString>,
    },
    WorktreePath {
        path: JsString,
    },
    RetryPermissionDenied,
}
#[derive(Debug, Clone)]
pub struct LifecycleResult {
    pub effects: Vec<HookEffect>,
    pub messages: Vec<BoundarySnapshot>,
    pub events: Vec<BoundarySnapshot>,
    pub blocking_errors: Vec<BoundarySnapshot>,
    pub non_blocking_errors: Vec<BoundarySnapshot>,
    pub pending_async_hooks: Option<Vec<BoundarySnapshot>>,
}

/// The 24 actual host operations. Serialization is confined to the transport
/// adapter; one owning worker consumes typed replies. Each reply must correspond
/// to its call variant; InvalidResponse is not a missing-capability fallback.
#[derive(Debug, Clone)]
pub enum HostCall {
    ModelMetadata {
        route: Route,
    },
    ModelPrepare {
        handle: PreparedRequestHandle,
        request: Box<CapturedRequest>,
        hints: ModelHints,
    },
    ModelMaterialize {
        handle: PreparedRequestHandle,
        request: Box<CapturedRequest>,
    },
    ModelPull {
        handle: PreparedRequestHandle,
        request: Box<CapturedRequest>,
        hints: ModelHints,
    },
    ModelClose {
        handle: PreparedRequestHandle,
    },
    ModelEagerStream {
        handle: PreparedRequestHandle,
        request: Option<Box<CapturedRequest>>,
        hints: ModelHints,
    },
    BudgetEstimate {
        request: Box<CapturedRequest>,
    },
    BudgetEvaluate {
        request: Box<CapturedRequest>,
        options: BudgetOptions,
    },
    BudgetCost {
        usage: Option<Usage>,
        route: Route,
    },
    /// The owning Source budget fallback preserves finite own extensions and
    /// property presence. Cost serialization must not reconstruct this from
    /// the closed aggregate-usage view.
    BudgetCostSnapshot {
        usage: Option<BoundarySnapshot>,
        route: Route,
    },
    TurnDrain,
    TurnDrainOrClose,
    TurnPersistCompaction {
        boundary: CompactBoundary,
        messages: Vec<ProjectedMessage>,
    },
    ListTools,
    ExecuteBatch {
        calls: Vec<ToolInvocation>,
        hints: ToolHints,
    },
    ExecuteSingle {
        call: ToolInvocation,
        hints: ToolHints,
    },
    PlanTodo(PlanTodoRequest),
    DecidePermission {
        tool: Captured<ToolDescriptor>,
        input: JsValue,
        hints: ToolHints,
        call_id: Option<JsString>,
    },
    PrepareContext(Box<ContextPrepare>),
    ApplyToolResults(ApplyToolResults),
    RecoverModelError(RecoverModelError),
    RecoverModelErrorBoundary(RecoverModelErrorBoundary),
    CaptureTurn(CaptureTurn),
    AutoCompact(Box<CompactionIntent>),
    DispatchLifecycle {
        event: JsString,
        payload: Option<JsObject>,
    },
    EmitEvent {
        event: BoundarySnapshot,
    },
}
impl HostCall {
    pub fn effect(&self) -> CallEffect {
        match self {
            Self::ModelMetadata { .. }
            | Self::BudgetEstimate { .. }
            | Self::BudgetEvaluate { .. }
            | Self::BudgetCost { .. }
            | Self::BudgetCostSnapshot { .. }
            | Self::ListTools
            | Self::PlanTodo(PlanTodoRequest {
                mutation: PlanTodoMutation::Read,
                ..
            }) => CallEffect::ReadOnly,
            Self::ModelPull { .. }
            | Self::ModelEagerStream { .. }
            | Self::TurnDrain
            | Self::TurnDrainOrClose
            | Self::ModelClose { .. } => CallEffect::Advance,
            // Prepared routing, prompt/cache admission, hooks, permission and
            // compaction can change host-owned state. Read-only tool flags do
            // not grant transport replay for an accepted capability call.
            _ => CallEffect::SideEffect,
        }
    }
    pub fn method(&self) -> (&'static str, &'static str) {
        match self {
            Self::ModelMetadata { .. } => ("model", "get_metadata"),
            Self::ModelPrepare { .. } => ("model", "prepare"),
            Self::ModelMaterialize { .. } => ("model", "materialize_prepared_request"),
            Self::ModelPull { .. } => ("model", "stream_next"),
            Self::ModelClose { .. } => ("model", "close_stream"),
            Self::ModelEagerStream { .. } => ("model", "stream"),
            Self::BudgetEstimate { .. } => ("budget", "estimate_request_input"),
            Self::BudgetEvaluate { .. } => ("budget", "evaluate_request_budget"),
            Self::BudgetCost { .. } | Self::BudgetCostSnapshot { .. } => {
                ("budget", "estimate_usage_cost")
            }
            Self::TurnDrain => ("turn", "drain_steer"),
            Self::TurnDrainOrClose => ("turn", "drain_or_close_steer"),
            Self::TurnPersistCompaction { .. } => ("turn", "persist_compaction"),
            Self::ListTools => ("capability", "list_tools"),
            Self::ExecuteBatch { .. } => ("capability", "execute_batch"),
            Self::ExecuteSingle { .. } => ("capability", "execute"),
            Self::PlanTodo(..) => ("capability", "plan_todo"),
            Self::DecidePermission { .. } => ("permission", "decide"),
            Self::PrepareContext(..) => ("context", "prepare_for_model"),
            Self::ApplyToolResults(..) => ("context", "apply_tool_results"),
            Self::RecoverModelError(..) | Self::RecoverModelErrorBoundary(..) => {
                ("context", "recover_from_model_error")
            }
            Self::CaptureTurn(..) => ("context", "capture_turn"),
            Self::AutoCompact(..) => ("context", "try_auto_compact"),
            Self::DispatchLifecycle { .. } => ("lifecycle", "dispatch"),
            Self::EmitEvent { .. } => ("event", "emit"),
        }
    }
    pub fn accepts(&self, reply: &HostReply) -> bool {
        matches!(
            (self, reply),
            (Self::ModelMetadata { .. }, HostReply::ModelMetadata(..))
                | (Self::ModelPrepare { .. }, HostReply::ModelPrepared(..))
                | (
                    Self::ModelMaterialize { .. },
                    HostReply::ModelMaterialized(..)
                )
                | (Self::ModelPull { .. }, HostReply::ModelChunk(..))
                | (Self::ModelClose { .. }, HostReply::ModelClosed)
                | (
                    Self::ModelEagerStream { .. },
                    HostReply::ModelEagerEvents(..)
                )
                | (Self::BudgetEstimate { .. }, HostReply::BudgetEstimate(..))
                | (Self::BudgetEvaluate { .. }, HostReply::BudgetSnapshot(..))
                | (Self::BudgetCost { .. }, HostReply::BudgetCost(..))
                | (Self::BudgetCostSnapshot { .. }, HostReply::BudgetCost(..))
                | (Self::TurnDrain, HostReply::SteerMessages(..))
                | (Self::TurnDrainOrClose, HostReply::SteerDrain(..))
                | (
                    Self::TurnPersistCompaction { .. },
                    HostReply::CompactionCommitted(..)
                )
                | (Self::ListTools, HostReply::Tools(..))
                | (Self::ExecuteBatch { .. }, HostReply::ToolResults(..))
                | (
                    Self::ExecuteBatch { .. },
                    HostReply::ToolBatchRejected { .. }
                )
                | (
                    Self::ExecuteBatch { .. },
                    HostReply::ToolResultsBoundaryOnly(..)
                )
                | (Self::ExecuteSingle { .. }, HostReply::ToolResult(..))
                | (
                    Self::ExecuteSingle { .. },
                    HostReply::ToolResultBoundaryOnly(..)
                )
                | (Self::PlanTodo(..), HostReply::PlanTodo(..))
                | (Self::PlanTodo(..), HostReply::PlanTodoBoundaryOnly(..))
                | (Self::DecidePermission { .. }, HostReply::Permission(..))
                | (Self::PrepareContext(..), HostReply::Context(..))
                | (Self::PrepareContext(..), HostReply::ContextBoundaryOnly(..))
                | (
                    Self::ApplyToolResults(..),
                    HostReply::ToolResultsApplied(..)
                )
                | (
                    Self::ApplyToolResults(..),
                    HostReply::ToolResultsAppliedBoundaryOnly(..)
                )
                | (Self::RecoverModelError(..), HostReply::Recovery(..))
                | (Self::RecoverModelErrorBoundary(..), HostReply::Recovery(..))
                | (
                    Self::RecoverModelErrorBoundary(..),
                    HostReply::RecoveryBoundaryOnly(..)
                )
                | (
                    Self::RecoverModelError(..),
                    HostReply::RecoveryBoundaryOnly(..)
                )
                | (Self::CaptureTurn(..), HostReply::CapturedTurn)
                | (Self::AutoCompact(..), HostReply::Compacted(..))
                | (Self::AutoCompact(..), HostReply::CompactedBoundaryOnly(..))
                | (Self::DispatchLifecycle { .. }, HostReply::Lifecycle(..))
                | (Self::EmitEvent { .. }, HostReply::EventEmitted)
        )
    }
}
#[derive(Debug, Clone)]
pub enum HostReply {
    ModelMetadata(Captured<ModelMetadata>),
    ModelPrepared(Box<PreparedInvocation>),
    ModelMaterialized(Box<CapturedRequest>),
    ModelChunk(StreamChunk),
    ModelClosed,
    ModelEagerEvents(Vec<StreamEvent>),
    BudgetEstimate(f64),
    BudgetSnapshot(Captured<BudgetSnapshot>),
    BudgetCost(Option<f64>),
    SteerMessages(Vec<SteerMessage>),
    SteerDrain(SteerDrain),
    CompactionCommitted(CompactionCommit),
    Tools(Vec<Captured<ToolDescriptor>>),
    ToolResults(Vec<ToolExecutionResult>),
    /// A physically correlated completed batch whose stock consumer throws an
    /// ordinary exception. Retain acceptance; synthesize only, never redispatch.
    ToolBatchRejected {
        message: JsString,
        boundary: BoundarySnapshot,
        identity: Box<ExecutionIdentity>,
    },
    /// Original shallow-admitted result shapes outside the normalized tool
    /// domain. Consumers must retain source downstream behavior/exceptions;
    /// this is not permission, success fabrication or an automatic retry.
    ToolResultsBoundaryOnly(Vec<BoundarySnapshot>),
    ToolResult(Box<ToolExecutionResult>),
    ToolResultBoundaryOnly(BoundarySnapshot),
    PlanTodo(Captured<PlanTodoSnapshot>),
    PlanTodoBoundaryOnly(BoundarySnapshot),
    Permission(Captured<PermissionDecision>),
    Context(Box<Captured<PreparedContext>>),
    /// Original context owns result presence only. Negotiated assembly reads
    /// this exact immutable prepared value, without inventing diagnostics or
    /// losing unknown metadata fields because a typed projection is narrower.
    ContextBoundaryOnly(BoundarySnapshot),
    ToolResultsApplied(Captured<AppliedToolResults>),
    ToolResultsAppliedBoundaryOnly(BoundarySnapshot),
    Recovery(Captured<RecoveryDecision>),
    RecoveryBoundaryOnly(BoundarySnapshot),
    CapturedTurn,
    Compacted(Box<Captured<AutoCompactResult>>),
    CompactedBoundaryOnly(BoundarySnapshot),
    Lifecycle(Captured<LifecycleResult>),
    EventEmitted,
}
/// Synchronous owning worker API. Implementations must concurrently admit
/// control input and wake pending calls; no GUI thread or stdout writer waits on
/// this worker. No automatic replay is implied by this interface.
pub trait HostPort: Send {
    fn advertised(&self, module: &str, operation: &str) -> bool;
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply>;
}

#[cfg(test)]
mod cleanup_cancellation_tests {
    use super::*;
    #[test]
    fn cleanup_scope_retains_hard_stop_before_and_after_registration() {
        for before in [false, true] {
            for cause in [CancelCause::OwnerRetired, CancelCause::TransportClosed] {
                let parent = Cancellation::default();
                assert!(parent.cancel(CancelCause::Requested("cancel".into())));
                if before {
                    assert!(!parent.cancel(cause.clone()));
                }
                let child = parent.cleanup_scope();
                if !before {
                    assert!(child.cause().is_none());
                    assert!(!parent.cancel(cause.clone()));
                }
                assert!(matches!(parent.cause(), Some(CancelCause::Requested(_))));
                assert_eq!(child.cause(), Some(cause));
            }
        }
    }
    #[test]
    fn cleanup_pending_wait_wakes_on_later_owner_retirement() {
        let parent = Cancellation::default();
        parent.cancel(CancelCause::Requested("cancel".into()));
        let child = parent.cleanup_scope();
        let (ready, admitted) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let wake = Arc::new(WakeSignal::default());
            child.register_wake(&wake);
            let generation = wake.generation();
            ready.send(()).unwrap();
            wake.wait(
                generation,
                Some(Instant::now() + std::time::Duration::from_secs(1)),
            );
            child.cause()
        });
        admitted.recv().unwrap();
        assert!(!parent.cancel(CancelCause::OwnerRetired));
        assert_eq!(worker.join().unwrap(), Some(CancelCause::OwnerRetired));
    }
    #[test]
    fn cleanup_scopes_have_no_strong_ownership_cycle() {
        let parent = Cancellation::default();
        let child = parent.cleanup_scope();
        let weak_parent = Arc::downgrade(&parent.0);
        let weak_child = Arc::downgrade(&child.0);
        drop(child);
        assert!(weak_child.upgrade().is_none());
        drop(parent);
        assert!(weak_parent.upgrade().is_none());
    }
}
