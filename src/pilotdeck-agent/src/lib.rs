//! Pure AgentLoop helpers. No filesystem, provider, permission or Session authority.
use pilotdeck_protocol::model::{
    BoundarySnapshot, ContentBlock, FinishReason, ImageBlock, Message, ModelError, ModelEvent,
    PdfBlock, ToolCall, Usage,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
pub mod budget_terminal;
mod budget_terminal_policy;
pub mod compaction_persistence;
mod compaction_persistence_policy;
pub mod compaction_terminal;
mod compaction_terminal_policy;
pub mod driver;
pub mod early_stream_recovery;
mod early_stream_recovery_policy;
pub use early_stream_recovery_policy::create_model_request_failure_status;
mod large_file_repair;
mod model_event_reducer;
pub mod ordinary_error_recovery;
mod ordinary_error_recovery_policy;
pub mod output_recovery;
mod output_recovery_policy;
pub mod ports;
pub mod request;
mod request_assembly;
pub mod request_materialization;
mod request_materialization_policy;
pub mod session_state;
mod session_state_projection;
pub mod source_invocation;
pub mod source_model;
mod source_model_reducer;
pub mod tool_context;
mod tool_context_policy;
pub mod tool_error_recovery;
mod tool_error_recovery_policy;
pub mod tool_failures;
mod tool_failures_policy;
mod tool_projection;
pub mod turn;
mod turn_timeline;
pub mod turn_wire;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairStrip {
    Assistant,
    ErrorPair,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LargeFileRepairDecision {
    Continue {
        prompt: JsString,
        purpose: JsString,
        strip: Option<RepairStrip>,
    },
    Stop {
        reason: JsString,
    },
}
#[derive(Debug, Clone, Copy)]
pub struct LargeFileRepairToolContext {
    pub output_truncated: bool,
    pub repaired_tool_calls: bool,
    pub finish_reason: Option<FinishReason>,
}
/// Turn-local recovery policy. It never performs a file write or grants tool
/// permissions; the host continues to authorize every requested effect.
#[derive(Debug, Clone, Default)]
pub struct LargeFileRepair {
    pre_draft_attempts: u32,
    post_draft_attempts: u32,
    truncation_recoveries: u32,
    wrote_file: bool,
    pending_large_file_repair: bool,
    recent_file_paths: Vec<JsString>,
}
impl LargeFileRepair {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recommended_max_output_tokens(&self) -> f64 {
        16_384.0
    }
    pub fn has_pending_repair(&self) -> bool {
        self.pending_large_file_repair
    }
    pub fn on_invalid_tool_input(&mut self) -> Option<LargeFileRepairDecision> {
        large_file_repair::on_invalid_tool_input(self)
    }
    pub fn on_no_tool_calls(&mut self) -> Option<LargeFileRepairDecision> {
        large_file_repair::on_no_tool_calls(self)
    }
    pub fn analyze_tool_results(
        &mut self,
        results: &[ToolExecutionResult],
        context: LargeFileRepairToolContext,
    ) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
        large_file_repair::analyze_tool_results(self, results, context)
    }
    pub fn analyze_boundary_results(
        &mut self,
        results: &[BoundarySnapshot],
        context: LargeFileRepairToolContext,
    ) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
        large_file_repair::analyze_boundary_results(self, results, context)
    }
    /// Original recovery only reads each call's name. It does not require an
    /// ID or input projection before choosing whether to repair truncation.
    pub fn recover_from_boundary_truncation(
        &mut self,
        calls: &[BoundarySnapshot],
    ) -> Result<Option<LargeFileRepairDecision>, ProjectionError> {
        large_file_repair::recover_from_boundary_truncation(self, calls)
    }
    pub fn recover_from_repaired_truncation(
        &mut self,
        calls: &[ToolCall],
    ) -> Option<LargeFileRepairDecision> {
        large_file_repair::recover_from_repaired_truncation(self, calls)
    }
}

/// One typed allocator owns live and durable coordinates for a turn. Boundary
/// annotation changes immutable event/message echoes, never Session authority.
/// JavaScript primitive Map keys with SameValueZero equality. The number value
/// keeps the original bit pattern for emitted IDs; hashing canonicalizes only
/// equality (NaN and signed zero), never their boundary representation.
#[derive(Debug, Clone, Copy)]
pub struct SourceTimelineNumber {
    pub value: f64,
}
impl SourceTimelineNumber {
    fn key_bits(self) -> u64 {
        if self.value == 0.0 {
            0
        } else if self.value.is_nan() {
            f64::NAN.to_bits()
        } else {
            self.value.to_bits()
        }
    }
}
impl PartialEq for SourceTimelineNumber {
    fn eq(&self, other: &Self) -> bool {
        self.key_bits() == other.key_bits()
    }
}
impl Eq for SourceTimelineNumber {}
impl std::hash::Hash for SourceTimelineNumber {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.key_bits(), state);
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceTimelineKey {
    String(JsString),
    Number(SourceTimelineNumber),
    Bool(bool),
    Null,
    Undefined,
}
impl From<JsString> for SourceTimelineKey {
    fn from(value: JsString) -> Self {
        Self::String(value)
    }
}
impl From<&str> for SourceTimelineKey {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}
#[derive(Debug, Clone)]
pub enum SourceTimelinePrevious {
    Missing,
    Undefined,
    Present(SourceTimelineKey),
}
#[derive(Debug, Clone)]
pub enum SourceTimelineOffset {
    Number(f64),
    String(JsString),
}
#[derive(Debug, Clone)]
pub struct SourceTimelinePosition {
    pub turn_id: JsString,
    pub id: SourceTimelineKey,
    pub previous_id: SourceTimelinePrevious,
    pub previous_id_appended: bool,
    pub order: f64,
    pub revision: f64,
    pub offset_utf16: Option<SourceTimelineOffset>,
}
#[derive(Debug, Clone)]
pub struct TurnTimeline {
    turn_id: JsString,
    last_model_position: Option<SourceTimelinePosition>,
    previous_id: Option<SourceTimelineKey>,
    next_order: f64,
    revision: f64,
    positions: std::collections::HashMap<SourceTimelineKey, SourceTimelinePosition>,
    provisional_tools: std::collections::HashSet<SourceTimelineKey>,
    offsets: std::collections::HashMap<SourceTimelineKey, SourceTimelineOffset>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineErrorKind {
    TypeError,
    NativeGraphRequired,
}
#[derive(Debug, Clone)]
pub struct TimelineError {
    pub kind: TimelineErrorKind,
    pub message: JsString,
    /// Original mutates earlier blocks before a later exception. The immutable
    /// API returns that partial boundary without granting mutable host authority.
    pub partial_input: Option<BoundarySnapshot>,
}
impl TurnTimeline {
    pub fn new(turn_id: JsString) -> Self {
        turn_timeline::new(turn_id)
    }
    pub fn position(&mut self, id: JsString, provisional: bool) -> SourceTimelinePosition {
        turn_timeline::position(self, id.into(), provisional)
    }
    pub fn update(&mut self, id: JsString) -> SourceTimelinePosition {
        turn_timeline::update(self, id.into())
    }
    pub fn position_boundary(&mut self, id: JsString, provisional: bool) -> BoundarySnapshot {
        let position = self.position(id, provisional);
        BoundarySnapshot::capture(&turn_timeline::wire_position(self, &position))
    }
    pub fn update_boundary(&mut self, id: JsString) -> BoundarySnapshot {
        let position = self.update(id);
        BoundarySnapshot::capture(&turn_timeline::wire_position(self, &position))
    }
    pub fn message(
        &mut self,
        message: &BoundarySnapshot,
    ) -> Result<BoundarySnapshot, TimelineError> {
        turn_timeline::message(self, message)
    }
    pub fn event(&mut self, event: &BoundarySnapshot) -> Result<BoundarySnapshot, TimelineError> {
        turn_timeline::event(self, event)
    }
}

#[derive(Debug, Clone)]
pub struct ModelAssemblerState {
    pub response_id: Option<JsString>,
    pub content: Vec<ContentBlock>,
    pub content_boundaries: Vec<BoundarySnapshot>,
    pub tool_call_boundaries: Vec<BoundarySnapshot>,
    pub error_boundary: Option<BoundarySnapshot>,
    pub text_buffer: JsString,
    pub model: Option<JsString>,
    pub thinking_buffer: JsString,
    pub thinking_reasoning_content_buffer: JsString,
    pub thinking_signature: Option<JsString>,
    pub usage: Usage,
    pub finish_reason: Option<FinishReason>,
    pub has_message_end: bool,
    pub error: Option<Box<ModelError>>,
    pub tool_calls: Vec<ToolCall>,
    pub has_repaired_tool_calls: Option<bool>,
}
#[derive(Debug, Clone)]
pub struct CapturedModelEvent {
    pub event: ModelEvent,
    /// Full matching event after consumer admission, including call/error extensions.
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyError {
    InvalidBoundary { message: JsString },
}
#[derive(Debug, Clone)]
pub struct AssembledAssistantMessage {
    /// Complete assembled original object, including message/toolCalls/errors/presence.
    pub boundary: BoundarySnapshot,
    pub message: Message,
    pub finish_reason: FinishReason,
    pub usage: Option<Usage>,
    pub tool_calls: Vec<ToolCall>,
    pub error: Option<Box<ModelError>>,
    pub has_repaired_tool_calls: Option<bool>,
    pub has_message_end: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamBlockKind {
    Text,
    Thinking,
}
pub fn create_model_assembler(response_id: Option<JsString>) -> ModelAssemblerState {
    model_event_reducer::new(response_id)
}
pub fn stream_block_id(state: &ModelAssemblerState, kind: StreamBlockKind) -> Option<JsString> {
    model_event_reducer::block_id(state, kind)
}
pub fn apply_model_event(
    state: &mut ModelAssemblerState,
    event: &CapturedModelEvent,
) -> Result<(), AssemblyError> {
    model_event_reducer::apply(state, event)
}
pub fn assemble_assistant_message(state: &mut ModelAssemblerState) -> AssembledAssistantMessage {
    model_event_reducer::assemble(state)
}

/// Each tool content keeps the actual full admitted boundary object for raw echoes
/// and supplemental stringify, independently of its normalized typed projection.
#[derive(Debug, Clone)]
pub enum ToolContent {
    Text {
        text: JsString,
        boundary: BoundarySnapshot,
    },
    Json {
        value: JsValue,
        boundary: BoundarySnapshot,
    },
    Image {
        image: ImageBlock,
        boundary: BoundarySnapshot,
    },
    Pdf {
        pdf: PdfBlock,
        boundary: BoundarySnapshot,
    },
    File {
        path: JsString,
        mime_type: Option<JsString>,
        description: Option<JsString>,
        boundary: BoundarySnapshot,
    },
    /// Lax original content remains an exact immutable boundary, not an invented typed kind.
    BoundaryOnly { boundary: BoundarySnapshot },
}
#[derive(Debug, Clone)]
pub struct SupplementalToolMessage {
    pub content: Vec<ToolContent>,
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub enum ToolResultDisposition {
    Success {
        data: Option<JsValue>,
    },
    Error {
        code: JsString,
        message: JsString,
        fields: JsObject,
    },
}
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub disposition: ToolResultDisposition,
    pub content: Vec<ToolContent>,
    pub supplemental_messages: Option<Vec<SupplementalToolMessage>>,
    pub metadata: Option<JsObject>,
    pub started_at: JsString,
    pub completed_at: JsString,
    /// Mandatory full original boundary retains extensions/presence; not a clock,
    /// native graph, authority token or replacement for typed loop state.
    pub boundary: BoundarySnapshot,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    InvalidBoundary {
        message: JsString,
    },
    Codec {
        message: JsString,
    },
    /// The original operation reached a JavaScript TypeError, after any earlier
    /// ordered state changes. Distinct from contradictory typed admission.
    SourceTypeError {
        message: JsString,
    },
}
/// Missing-result policy receives the caller's original clock/recovery context.
/// FIFO pairing does not invent timestamps, permission advice or perform effects.
pub fn pair_tool_results(
    calls: &[ToolCall],
    results: Vec<ToolExecutionResult>,
    missing: &mut dyn FnMut(&ToolCall) -> ToolExecutionResult,
) -> Vec<ToolExecutionResult> {
    tool_projection::pair_results(calls, results, missing)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionGap {
    UndefinedCanonicalText,
    LaxMediaFields,
    UnmodeledContentKind,
    OtherBoundaryShape,
}
#[derive(Debug, Clone)]
pub enum MessageProjection {
    Complete(Box<Message>),
    /// Successful original projection outside the normalized domain. Never a
    /// fabricated empty Message, deleted block or new runtime failure.
    BoundaryOnly(Vec<ProjectionGap>),
}
#[derive(Debug, Clone)]
pub struct ProjectedMessage {
    /// Mandatory exact full projected message; wire consumers use this value.
    pub boundary: BoundarySnapshot,
    pub normalized: MessageProjection,
}
/// Exact projection keeps own text:Undefined and all lax media/extension shapes.
/// Outer Err is source-equivalent conversion failure or inconsistent admission.
pub fn project_tool_results(
    results: &[ToolExecutionResult],
) -> Result<Vec<ProjectedMessage>, ProjectionError> {
    tool_projection::project(results)
}

pub mod usage_fallback;
mod usage_fallback_policy;
