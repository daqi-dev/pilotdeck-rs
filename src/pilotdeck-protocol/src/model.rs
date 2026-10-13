//! Typed canonical model domain. Opaque provider data is confined to explicit
//! schemas, tool inputs, metadata and read-only echoes; it is not session state.
use crate::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProtocol {
    Anthropic,
    OpenAi,
    OpenAiResponses,
    Google,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}
#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePosition {
    /// Version is exactly 1; the type does not imply a Module or Gateway version.
    pub turn_id: JsString,
    pub id: JsString,
    pub previous_id: Option<JsString>,
    pub order: f64,
    pub revision: f64,
    pub offset_utf16: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    pub text: JsString,
    pub block_id: Option<JsString>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingBlock {
    pub text: JsString,
    pub block_id: Option<JsString>,
    pub signature: Option<JsString>,
    pub reasoning_content: Option<JsString>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaSource {
    Base64,
    Url,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageDetail {
    Auto,
    Low,
    High,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ImageBlock {
    pub source: MediaSource,
    pub data: JsString,
    pub mime_type: JsString,
    pub bytes: Option<f64>,
    pub detail: Option<ImageDetail>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct PdfBlock {
    pub data: JsString,
    pub bytes: f64,
    pub pages: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBlock {
    pub source: MediaSource,
    pub data: JsString,
    pub mime_type: JsString,
    pub bytes: Option<f64>,
    pub duration_seconds: Option<f64>,
}
/// An immutable echo, shared by ordinary content clone. Admission snapshots
/// detach the underlying data as well. No credential or process authority.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEcho(pub Arc<JsValue>);
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub timeline: Option<TimelinePosition>,
    pub id: JsString,
    pub name: JsString,
    pub input: JsValue,
    pub raw: Option<ProviderEcho>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ToolResultContent {
    Text(TextBlock),
    Image(ImageBlock),
    Pdf(PdfBlock),
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultItem {
    pub timeline: Option<TimelinePosition>,
    pub body: ToolResultContent,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: JsString,
    pub content: Vec<ToolResultItem>,
    pub is_error: Option<bool>,
    pub raw: Option<ProviderEcho>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultReference {
    pub tool_call_id: JsString,
    pub is_error: Option<bool>,
    pub path: JsString,
    pub read_file_path: Option<JsString>,
    pub original_bytes: f64,
    pub preview: JsString,
    pub has_more: bool,
    pub mime_type: Option<JsString>,
    pub reason: Option<JsString>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Image,
    Pdf,
    Audio,
}
#[derive(Debug, Clone, PartialEq)]
pub struct MediaReference {
    pub tool_call_id: Option<JsString>,
    pub path: JsString,
    pub original_bytes: f64,
    pub preview: JsString,
    pub has_more: bool,
    pub mime_type: JsString,
    pub media_type: MediaType,
    pub pages: Option<f64>,
    pub detail: Option<ImageDetail>,
    pub reason: Option<JsString>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBody {
    Text(TextBlock),
    Thinking(ThinkingBlock),
    Image(ImageBlock),
    Pdf(PdfBlock),
    Audio(AudioBlock),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ToolResultReference(ToolResultReference),
    MediaReference(MediaReference),
}
#[derive(Debug, Clone, PartialEq)]
pub struct ContentBlock {
    pub timeline: Option<TimelinePosition>,
    pub body: ContentBody,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedContextPurpose {
    Material,
    Skill,
    Application,
}
#[derive(Debug, Clone, PartialEq)]
pub struct TrustedContext {
    pub source: JsString,
    pub purpose: TrustedContextPurpose,
    /// Always turn-scoped. A parsed string does not establish authentication.
    pub authorized_principal: JsString,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ForkCarryover {
    pub source_session_id: JsString,
    pub source_turn_id: Option<JsString>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageMetadata {
    pub module_id: Option<JsString>,
    pub model: Option<JsString>,
    pub synthetic: Option<bool>,
    pub transient: Option<bool>,
    pub transient_id: Option<JsString>,
    pub tool_call_id: Option<JsString>,
    pub compact_replacement: Option<bool>,
    pub compact_snapshot_id: Option<JsString>,
    pub purpose: Option<JsString>,
    pub trusted_context: Option<TrustedContext>,
    pub queue_item_id: Option<JsString>,
    pub fork_carryover: Option<ForkCarryover>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub metadata: Option<MessageMetadata>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchema {
    pub name: JsString,
    pub description: Option<JsString>,
    pub input_schema: JsObject,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool { name: JsString },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    Default,
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingConfig {
    pub mode: Option<ThinkingMode>,
    pub enabled: bool,
    pub budget_tokens: Option<f64>,
    pub preserve: Option<bool>,
    pub split_reasoning: Option<bool>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSchema {
    pub name: JsString,
    pub description: Option<JsString>,
    pub schema: JsObject,
    pub strict: Option<bool>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CachePlan {
    pub provider: Option<JsString>,
    pub model: Option<JsString>,
    pub system: bool,
    pub tools: bool,
    pub messages: Vec<f64>,
    pub fingerprint: JsString,
    pub generation: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalModelRequest {
    pub model: JsString,
    pub provider: JsString,
    pub messages: Vec<Message>,
    pub system_prompt: Option<JsString>,
    pub tools: Option<Vec<ToolSchema>>,
    pub tool_choice: Option<ToolChoice>,
    pub max_output_tokens: Option<f64>,
    pub temperature: Option<f64>,
    pub speed: Option<f64>,
    pub thinking: Option<ThinkingConfig>,
    pub stream: Option<bool>,
    pub metadata: Option<JsObject>,
    pub output_schema: Option<OutputSchema>,
    pub cache_breakpoints: Option<Vec<f64>>,
    pub cache_plan: Option<CachePlan>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub cache_read_tokens: Option<f64>,
    pub cache_write_tokens: Option<f64>,
    pub total_tokens: Option<f64>,
    pub native_cost: Option<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Error,
    Unknown,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsFix {
    pub description: JsString,
    pub config_path: Option<JsString>,
    pub command: Option<JsString>,
    pub url: Option<JsString>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionPhase {
    Empty,
    Text,
    Reasoning,
    ToolCall,
}
#[derive(Debug, Clone, PartialEq)]
pub struct InterruptedTool {
    pub id: JsString,
    pub name: JsString,
    pub argument_chars_utf16: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInterruption {
    pub phase: InterruptionPhase,
    pub active_tool_calls: Option<Vec<InterruptedTool>>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ModelError {
    pub provider: JsString,
    pub model: Option<JsString>,
    pub protocol: ModelProtocol,
    /// Open extension code, including the 21 original known codes; never collapsed to unknown.
    pub code: JsString,
    pub status: Option<f64>,
    pub message: JsString,
    pub retryable: bool,
    pub raw: Option<ProviderEcho>,
    pub recoverable_via_compact: Option<bool>,
    pub recoverable_via_image_strip: Option<bool>,
    pub retry_after_ms: Option<f64>,
    pub user_hint: Option<JsString>,
    pub settings_fix: Option<SettingsFix>,
    pub max_context_tokens: Option<f64>,
    pub max_output_tokens: Option<f64>,
    pub available_output_tokens: Option<f64>,
    pub stream_interruption: Option<StreamInterruption>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ModelEvent {
    RequestStarted {
        provider: JsString,
        model: JsString,
        provider_base_url: Option<JsString>,
        request_fingerprint: Option<JsString>,
        metadata: Option<JsObject>,
    },
    MessageStart {
        raw: Option<ProviderEcho>,
    },
    TextDelta {
        text: JsString,
        raw: Option<ProviderEcho>,
    },
    ThinkingDelta {
        text: JsString,
        signature: Option<JsString>,
        reasoning_content: Option<JsString>,
        raw: Option<ProviderEcho>,
    },
    ToolCallStart {
        id: JsString,
        name: JsString,
        raw: Option<ProviderEcho>,
    },
    ToolCallDelta {
        id: JsString,
        delta: JsString,
        raw: Option<ProviderEcho>,
    },
    ToolCallEnd {
        tool_call: ToolCall,
        was_repaired: Option<bool>,
        raw: Option<ProviderEcho>,
    },
    MessageEnd {
        finish_reason: FinishReason,
        raw: Option<ProviderEcho>,
    },
    Usage {
        usage: Usage,
        raw: Option<ProviderEcho>,
    },
    Error(Box<ModelError>),
}
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub content: Vec<ContentBlock>,
    pub usage: Option<Usage>,
    pub finish_reason: FinishReason,
    pub raw: Option<ProviderEcho>,
}

/// Normalized typed-domain snapshot, exposed by shared reference only.
/// Original boundary presence/extensions use PreparedWireRequest; this API alone
/// does not claim structuredClone compatibility for arbitrary JS objects.
/// It never reassembles context on retry.
#[derive(Debug, Clone)]
pub struct PreparedRequest {
    request: Arc<CanonicalModelRequest>,
}
impl PreparedRequest {
    pub fn capture(request: &CanonicalModelRequest) -> Self {
        crate::snapshots::capture_request(request)
    }
    pub fn request(&self) -> &CanonicalModelRequest {
        &self.request
    }
    pub(crate) fn from_detached(request: CanonicalModelRequest) -> Self {
        Self {
            request: Arc::new(request),
        }
    }
}
/// Lossless JSON-compatible boundary snapshot, including own Undefined. It does
/// not claim support for JS functions, prototypes, Date, Map, Set or cyclic graphs.
#[derive(Debug, Clone)]
pub struct BoundarySnapshot {
    value: Arc<JsValue>,
}
impl BoundarySnapshot {
    pub fn capture(value: &JsValue) -> Self {
        crate::snapshots::capture_boundary(value)
    }
    pub fn value(&self) -> &JsValue {
        &self.value
    }
    pub(crate) fn from_detached(value: JsValue) -> Self {
        Self {
            value: Arc::new(value),
        }
    }
}

/// Complete lossless JSON-compatible admission snapshot. Unknown fields and
/// own Undefined/null survive independently of any normalized-domain projection.
/// Native JS object graphs require host-owned handles and binding-specific tests.
#[derive(Debug, Clone)]
pub struct PreparedWireRequest {
    boundary: BoundarySnapshot,
}
impl PreparedWireRequest {
    pub fn capture(request: &JsValue) -> Self {
        Self {
            boundary: BoundarySnapshot::capture(request),
        }
    }
    pub fn request(&self) -> &JsValue {
        self.boundary.value()
    }
}
