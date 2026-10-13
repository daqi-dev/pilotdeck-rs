//! Consumer admission is separate from the intentionally lax Module shape validator.
//! These checks do not own a transport, process, operation ledger or host permission.
use crate::{JsObject, JsString, module::ModuleMethod, wire::ResponseRef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarBinding {
    pub module_id: JsString,
    pub module_instance_id: JsString,
    pub connection_generation: JsString,
    pub capabilities_version: JsString,
}
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityMethod {
    pub name: ModuleMethod,
    /// Original consumer validates method name only. Preserve the remaining raw
    /// fields for consumer-specific enabled/profile/resume handling.
    pub fields: JsObject,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarCapabilities {
    pub version: JsString,
    pub methods: Vec<CapabilityMethod>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionErrorKind {
    UnsupportedVersion,
    MissingBinding,
    CapabilitiesMismatch,
    InvalidMethod,
    TypeError,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionError {
    pub kind: AdmissionErrorKind,
    pub message: JsString,
}
/// Source: original agentLoopSidecarClient.ts parseBinding, not a generic stricter schema.
pub fn parse_sidecar_binding(response: ResponseRef<'_>) -> Result<SidecarBinding, AdmissionError> {
    crate::consumer::parse_binding(response)
}
/// Source: original parseCapabilities. Method names are checked; permissive
/// optional properties remain in fields rather than silently being rewritten.
pub fn parse_sidecar_capabilities(
    response: ResponseRef<'_>,
    expected_version: &JsString,
) -> Result<SidecarCapabilities, AdmissionError> {
    crate::consumer::parse_capabilities(response, expected_version)
}
/// Compare all four binding fields for hello/capabilities correlation. During
/// reconnect, the host separately verifies the retained module id/instance and
/// accepts a new connection generation; it never retries execute blindly.
pub fn same_binding(left: &SidecarBinding, right: &SidecarBinding) -> bool {
    left == right
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub run_id: JsString,
    pub operation_id: JsString,
    pub request_id: JsString,
    pub session_id: JsString,
    pub turn_id: JsString,
}
/// Opaque identifiers issued by the host. Serialized representations do not
/// authorize an arbitrary caller; the issuing host keeps the actual object map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreparedRequestHandle(pub JsString);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelStreamHandle(pub JsString);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDisposition {
    ReuseRecorded,
    QueryStatus,
    RejectConflict,
}
/// Advance calls (stream_next, draining steering, side effects) must reconcile
/// unknown acceptance through the owning ledger. They are not safe to resend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEffect {
    ReadOnly,
    Advance,
    SideEffect,
}
