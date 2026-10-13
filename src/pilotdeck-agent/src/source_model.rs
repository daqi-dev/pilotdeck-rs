//! Source-valid open model-event reduction. This is typed assembler state, not
//! a dispatcher, mutable Session dictionary or authority-bearing provider blob.
//! Boundary values are retained only in open protocol fields of the original.
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
#[derive(Debug, Clone)]
pub enum SourceField {
    Missing,
    Present(BoundarySnapshot),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStateKey {
    ResponseId,
    Content,
    TextBuffer,
    Model,
    ThinkingBuffer,
    ThinkingReasoningContentBuffer,
    ThinkingSignature,
    Usage,
    FinishReason,
    HasMessageEnd,
    Error,
    ToolCalls,
    HasRepairedToolCalls,
}
#[derive(Debug, Clone)]
pub struct SourceUsage {
    pub input_tokens: SourceField,
    pub output_tokens: SourceField,
    pub cache_read_tokens: SourceField,
    pub cache_write_tokens: SourceField,
    pub total_tokens: SourceField,
    pub native_cost: SourceField,
}
#[derive(Debug, Clone)]
pub struct SourceAssemblerState {
    /// Bounded typed property insertion order, including later own Undefined.
    pub property_order: Vec<SourceStateKey>,
    pub response_id: SourceField,
    pub content: Vec<BoundarySnapshot>,
    pub text_buffer: JsString,
    pub model: SourceField,
    pub thinking_buffer: JsString,
    pub thinking_reasoning_content_buffer: JsString,
    pub thinking_signature: SourceField,
    pub usage: SourceUsage,
    pub finish_reason: SourceField,
    pub has_message_end: bool,
    pub error: SourceField,
    pub tool_calls: Vec<BoundarySnapshot>,
    pub has_repaired_tool_calls: SourceField,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceAssemblyError {
    /// Actual source property access/ToPrimitive/trim/length exception. Any
    /// prior ordered state changes remain in the supplied typed state.
    TypeError { message: JsString },
    /// Native graphs/functions are not represented by immutable JSON boundaries.
    NativeGraphRequired { message: JsString },
}
pub trait SourceModelReducer {
    fn create(&self, response_id: &JsValue) -> SourceAssemblerState;
    fn block_id(
        &self,
        state: &SourceAssemblerState,
        kind: &JsString,
    ) -> Result<Option<JsString>, SourceAssemblyError>;
    fn apply(
        &self,
        state: &mut SourceAssemblerState,
        event: &BoundarySnapshot,
    ) -> Result<(), SourceAssemblyError>;
    fn assemble(
        &self,
        state: &mut SourceAssemblerState,
    ) -> Result<BoundarySnapshot, SourceAssemblyError>;
    /// Exact source state projection for independent exported-function differential.
    fn snapshot(&self, state: &SourceAssemblerState) -> JsObject;
}
pub struct OriginalSourceModelReducer;

impl SourceModelReducer for OriginalSourceModelReducer {
    fn create(&self, response_id: &JsValue) -> SourceAssemblerState {
        crate::source_model_reducer::create(response_id)
    }
    fn block_id(
        &self,
        state: &SourceAssemblerState,
        kind: &JsString,
    ) -> Result<Option<JsString>, SourceAssemblyError> {
        crate::source_model_reducer::block_id(state, kind)
    }
    fn apply(
        &self,
        state: &mut SourceAssemblerState,
        event: &BoundarySnapshot,
    ) -> Result<(), SourceAssemblyError> {
        crate::source_model_reducer::apply(state, event)
    }
    fn assemble(
        &self,
        state: &mut SourceAssemblerState,
    ) -> Result<BoundarySnapshot, SourceAssemblyError> {
        crate::source_model_reducer::assemble(state)
    }
    fn snapshot(&self, state: &SourceAssemblerState) -> JsObject {
        crate::source_model_reducer::snapshot(state)
    }
}
