//! Context application consumes already executed tools; it cannot execute or
//! authorize them again. The host owns artifact storage and context policy.
use crate::ports::{CallControl, HostPort, PortResult};
use crate::turn::TurnInput;
use crate::{ProjectedMessage, ToolExecutionResult};
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct ToolContextInput<'a> {
    pub control: &'a CallControl,
    pub input: &'a TurnInput,
    pub messages: &'a [ProjectedMessage],
    pub results: &'a [ToolExecutionResult],
    pub projected: &'a [ProjectedMessage],
}

/// Open source collections remain immutable evidence when they have no array
/// projection. Assignment and later iteration have different exception phases.
#[derive(Debug, Clone)]
pub enum ContextMessageCollection {
    Items(Vec<ProjectedMessage>),
    BoundaryOnly(BoundarySnapshot),
}

#[derive(Debug, Clone)]
pub struct ToolContextOutcome {
    pub messages: ContextMessageCollection,
    pub appended_messages: ContextMessageCollection,
    /// Returned JSON tails have distinct identity. Only the source's applicable
    /// tail branch reattaches the canonical instances after timeline annotation.
    pub reattach_tail: bool,
    /// Legacy push preserves identity only when the emitted collection is the
    /// original projection. A catch after assigning another appended collection
    /// pushes different instances and must not annotate that tail by value.
    pub fallback_projection_tail: bool,
}

pub struct OriginalToolContextPolicy;
impl OriginalToolContextPolicy {
    pub fn apply(
        &self,
        host: &mut dyn HostPort,
        input: ToolContextInput<'_>,
    ) -> PortResult<ToolContextOutcome> {
        crate::tool_context_policy::apply(host, input)
    }
}
