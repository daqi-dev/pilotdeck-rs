//! Native SOP node execution borrows the current outer turn's authority.
//! Fresh model/context/router/client state never rebinds the session resources.
use super::{host_factory::{Deployment, SessionPorts, TurnHostFactory}, interaction::{InteractionTurn, InteractionTurnInput}, loop_control::SopNodePlan};
use crate::{host::{HostClient, HostError, HostErrorKind, HostFuture, Limits, modules::{capability, context, model, model_port}}, runtime::{project::RuntimeLease, hooks::Cancellation}, storage::recorder::Recorder};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::{path::PathBuf, sync::Arc};
use tokio::process::Command;

/// Captured before the parent's ports are consumed. These are the *same turn's*
/// tools, SDK opaque guards and services, not a rerun of the mutable factory.
pub(crate) struct NodeTemplate {
    deployment: Deployment,
    limits: Limits,
    expected: Option<JsString>,
    native_terminal_errors: bool,
    model: model_port::RouterModelPortOptions,
    model_session: JsString,
    tool_results_dir: PathBuf,
    context_runtime: Option<Arc<dyn context::ContextRuntime>>,
    context_identity: crate::host::modules::tool_context::ContextRequestIdentity,
    context_config: JsObject,
    execution: Arc<dyn capability::ToolExecution>,
    tool_context: Arc<dyn capability::ToolContext>,
    observer: Arc<dyn capability::CapabilityResultObserver>,
    plan: Option<Arc<dyn capability::PlanTodoHandler>>,
    permission: crate::runtime::project::interaction_permission::PermissionTurnOptions,
    signal: Cancellation,
    switches: crate::host::modules::CompositionSwitches,
}
pub(super) struct NodeHost {
    pub client: HostFuture<HostClient>,
    pub handler: Arc<super::interaction::BorrowedInteractionTurn>,
    pub lease: Arc<RuntimeLease>,
    pub manifest: JsObject,
}
impl NodeTemplate {
    pub(super) fn capture(ports: &SessionPorts, identity: &ExecutionIdentity, deployment: Deployment, limits: Limits, expected: Option<JsString>, native_terminal_errors: bool) -> Self {
        Self { deployment, limits, expected, native_terminal_errors, model_session: identity.session_id.clone(), model: ports.model.clone(), tool_results_dir: ports.tool_results_dir.clone(), context_runtime: ports.context_runtime.clone(), context_identity: ports.context_identity.clone(), context_config: ports.context_config.clone(), execution: ports.tool_execution.clone(), tool_context: ports.tool_context.clone(), observer: ports.tool_observer.clone(), plan: ports.plan_todo.clone(), permission: ports.interaction.permission.clone(), signal: ports.interaction.signal.clone(), switches: ports.interaction.switches }
    }
    pub(super) async fn prepare(&self, parent: Arc<InteractionTurn>, identity: &ExecutionIdentity, recorder: Arc<Recorder>, plan: &SopNodePlan) -> Result<NodeHost, HostError> {
        let lease = parent.fork_generation()?;
        let execution: Arc<dyn capability::ToolExecution> = Arc::new(BorrowedTools(self.execution.clone()));
        let execution = plan.binding.as_ref().map_or_else(|| execution.clone(), |binding| binding.tools(execution.clone()));
        let tool_context: Arc<dyn capability::ToolContext> = Arc::new(BorrowedContext(self.tool_context.clone()));
        let signal = self.signal.clone();
        let ports = SessionPorts { model: self.model.clone(), recorder: Some(model_port::DurableRecorder {recorder: recorder.clone(), recording_session_id: Some(identity.session_id.clone())}), tool_results_dir: self.tool_results_dir.clone(), context_runtime: self.context_runtime.clone(), context_identity: self.context_identity.for_identity(identity), context_config: self.context_config.clone(), tool_execution: execution, tool_context, tool_observer: self.observer.clone(), plan_todo: self.plan.clone(), turn: Default::default(), interaction: InteractionTurnInput { permission: self.permission.clone(), hooks: Default::default(), sdk_hook_state: None, sdk_file_slot: None, hook_options: None, session_hook_options: None, contribution: None, base_env: None, signal: signal.clone(), steer_recorder: None, record_message: None, messages: Vec::new(), switches: self.switches, turn_ports: Default::default() } };
        let (mut router, _, lease) = TurnHostFactory::assemble_sop(lease, identity, ports, plan.binding.clone(), plan.model_override.clone(), self.model_session.clone()).await?;
        router.context = router.context.with_recorder(recorder.clone());
        let lease = Arc::new(lease);
        let handler = Arc::new(parent.borrow_node(router, lease.clone(), identity, recorder, signal, self.switches).await?);
        let manifest = handler.manifest().clone();
        let deployment = self.deployment.clone(); let expected = self.expected.clone(); let limits = self.limits; let native = self.native_terminal_errors;
        let client = Box::pin(async move {
            let client = match deployment {
                Deployment::Stdio { command, args, cwd, env } => { let mut command = Command::new(command); command.args(args).current_dir(cwd).env_clear().envs(env); HostClient::spawn(&mut command, expected.as_ref(), limits).await }
                Deployment::Tcp { host, port, connect_timeout_ms } => HostClient::connect_tcp(&host, port, connect_timeout_ms.map(std::time::Duration::from_millis), expected.as_ref(), limits).await,
                Deployment::Native => Err(HostError::new(HostErrorKind::Protocol, "Native SOP node requires an installed host transport.")),
            }?;
            Ok(client.with_native_terminal_errors(native))
        });
        Ok(NodeHost { client, handler, lease, manifest })
    }
}
/// The parent owns dispatcher shutdown, generation and opaque permission/hooks.
/// Delegating execution preserves its native scheduler and once-only grants.
struct BorrowedTools(Arc<dyn capability::ToolExecution>);
impl capability::ToolCatalog for BorrowedTools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> { self.0.list() }
}
impl capability::ToolExecution for BorrowedTools {
    fn applies_mode_effects(&self) -> bool { self.0.applies_mode_effects() }
    fn refresh(&self) -> HostFuture<()> { self.0.refresh() }
    fn execute_all(&self, calls: Vec<capability::ToolCall>, context: capability::ToolInvocationContext) -> HostFuture<Vec<JsObject>> { self.0.execute_all(calls, context) }
}
struct BorrowedContext(Arc<dyn capability::ToolContext>);
impl capability::ToolContext for BorrowedContext {
    fn prepare(&self, call: JsValue) -> HostFuture<capability::ToolInvocationContext> { self.0.prepare(call) }
}
/// Force a node route on the actual provider preparation, without replacing
/// metadata/materialization or the existing sticky/durable execution chain.
pub(super) struct NodeModel {
    pub inner: Arc<dyn model::ModelExecution>, pub route: JsObject,
}
impl model::ModelExecution for NodeModel {
    fn prepare(&self, mut input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        input.context.insert("modelOverride".into(), JsValue::Object(self.route.clone())); self.inner.prepare(input)
    }
    fn stream(&self, input: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> { self.inner.stream(input) }
}
