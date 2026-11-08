//! Composition of tool handlers for one owned Always-On phase.
use std::sync::Arc;
use pilotdeck_protocol::{JsString, JsValue};
use crate::runtime::tools::{ToolError, ToolFuture, ToolOutput, ToolRunContext};
use super::{chat_tool::ChatHandler, manager::AlwaysOnRunContextRegistry,
    plan_tool::PlanHandler, report_tool::ReportHandler,
    tools::{PhaseHandlerLease, PhaseToolHandler}, workspace_tool::WorkspaceHandler};

pub(crate) enum PhaseHandlers {
    Discovery { plan: Arc<PlanHandler>, chat: Arc<ChatHandler> },
    Workspace(Arc<WorkspaceHandler>),
    Execution,
    Report(Arc<ReportHandler>),
}
impl PhaseHandlers {
    pub(crate) fn install(self: &Arc<Self>, contexts: &Arc<AlwaysOnRunContextRegistry>,
        session: JsString) -> Result<PhaseHandlerLease, crate::host::HostError> {
        contexts.install_tool_handler(session, self.clone())
    }
    /// First revoke/unregister the phase handler so no new calls enter, then
    /// await every already-admitted owned write or workspace operation.
    pub(crate) async fn drain(&self) {
        match self {
            Self::Discovery { plan, .. } => plan.pending.drain().await,
            Self::Workspace(workspace) => workspace.pending.drain().await,
            Self::Report(report) => report.pending.drain().await,
            Self::Execution => {},
        }
    }
}
impl PhaseToolHandler for PhaseHandlers {
    fn execute(&self, name: &'static str, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        match self {
            Self::Discovery { plan, .. } if name == "always_on_discovery_plan" => plan.execute(name, input, context),
            Self::Discovery { chat, .. } if name == "always_on_read_chat_history" => chat.execute(name, input, context),
            Self::Workspace(workspace) if name == "always_on_prepare_workspace" => workspace.execute(name, input, context),
            Self::Report(report) if name == "always_on_report" => report.execute(name, input, context),
            _ => Box::pin(async move { Err(ToolError::failed(format!("{name} is not available in this Always-On phase").as_str())) }),
        }
    }
}
