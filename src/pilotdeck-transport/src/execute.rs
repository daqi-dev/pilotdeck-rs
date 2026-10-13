//! Typed factory payload boundary. Projection is immutable; host authority stays
//! in the bound execution owner. Seed/model boundary parsers are separate owners.
use crate::AdvertisedMethods;
use pilotdeck_agent::ports::ToolDescriptor;
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsString, JsValue};

pub use pilotdeck_agent::turn::{
    RunMode, TurnInput as FactoryTurnInput, TurnModelOverride as FactoryModelOverride,
    TurnPermissionContext as FactoryPermissionContext,
    TurnPermissionRules as FactoryPermissionRules, TurnRuntimeConfig as FactoryRuntimeConfig,
    TurnSubagentModel as FactorySubagentModel,
};
#[derive(Debug, Clone)]
pub struct FactoryPayload {
    pub config: FactoryRuntimeConfig,
    pub input: FactoryTurnInput,
    pub tools: Vec<ToolDescriptor>,
    pub methods: AdvertisedMethods,
    pub elicitation_available: bool,
    /// Retained for the separate actual seed/model-state parsers; not a claim
    /// these have become Rust-owned live state or passed their module gate.
    pub seed_state: Option<BoundarySnapshot>,
    pub model_state: Option<BoundarySnapshot>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryPayloadError {
    /// Native prototype mutations require a separate source-equivalent graph
    /// representation. This bounded decoder must fail visibly rather than claim
    /// an own-property map is equivalent. The whole factory gate stays open.
    NativeGraphRequired {
        message: JsString,
    },
    SourceTypeError {
        message: JsString,
    },
    InvalidSubagentModel {
        message: JsString,
    },
    InvalidEnvelope {
        message: JsString,
    },
}
pub trait FactoryPayloadDecoder {
    fn decode(
        &self,
        execute: &JsValue,
        process_cwd: &JsString,
    ) -> Result<FactoryPayload, FactoryPayloadError>;
}
/// Implemented only in the bounded first-party payload module, after source freeze.
pub struct OriginalFactoryPayloadDecoder;

impl FactoryPayloadDecoder for OriginalFactoryPayloadDecoder {
    fn decode(
        &self,
        execute: &JsValue,
        process_cwd: &JsString,
    ) -> Result<FactoryPayload, FactoryPayloadError> {
        crate::factory_payload::decode(execute, process_cwd)
    }
}
