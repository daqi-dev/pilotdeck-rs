//! Generation-owned permission binding. Decision and UI requests stay separate,
//! as in PermissionRuntime.ts and createGatewayPermissionHook.ts (cc7c4e6).
//! Callers retain the generation lease until the bound turn ends. The lifecycle
//! adapter in interaction_hooks consumes request(); binding alone does not install hooks.
use super::{DisposeFuture, FamilyResource, InteractionProfile, RuntimeError, RuntimeResult};
use crate::{
    config::permission::RuleSetHandle,
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            ModuleRouter,
            capability::{RuntimeContext, ToolCatalog, ToolDefinition},
            permission as port,
        },
    },
    runtime::{
        interaction::{GatewayInteractionCoordinator, PendingEntry, Registration},
        network::Cancellation,
        permission::{self, PermissionContext, PermissionFuture, PermissionTool},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::sync::oneshot;

struct ToolAdapter {
    tool: Arc<dyn ToolDefinition>,
    name: JsString,
    kind: Option<JsString>,
}
impl PermissionTool for ToolAdapter {
    fn name(&self) -> &JsString {
        &self.name
    }
    fn kind(&self) -> Option<&JsString> {
        self.kind.as_ref()
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        self.tool.is_read_only(input)
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        self.tool.check_permissions(input, context)
    }
}
#[derive(Default)]
struct State {
    closed: bool,
    releases: usize,
    turns: HashMap<JsString, Arc<Run>>,
}
struct Run {
    session: JsString,
    run_id: JsString,
    cancelled: Cancellation,
    requests: Mutex<HashMap<JsString, Registration>>,
    options: PermissionTurnOptions,
}
impl Run {
    fn close(&self) {
        self.cancelled.cancel();
        for (_, registration) in lock(&self.requests).drain() {
            registration.dispose();
        }
    }
}
/// All fields are host authority. A wire context is never merged into these.
pub struct PermissionTurnOptions {
    pub profile: InteractionProfile,
    pub context: PermissionContext,
    pub rules: RuleSetHandle,
    pub coordinator: Arc<GatewayInteractionCoordinator>,
    /// Gateway-event delivery (not the agent event sink). false auto-denies.
    /// This infallible callback has no TS exception/Result channel. A Rust
    /// panic unwinds and drops the pending registration; it is not converted
    /// into a "Permission prompt delivery failed" decision (see
    /// A6.9-PERMISSION-RUN-001-R2).
    pub emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
    pub timeout: Duration,
}
#[derive(Clone, Default)]
pub struct InteractionPermission {
    state: Arc<Mutex<State>>,
}
impl InteractionPermission {
    pub(super) fn owns_turn(
        &self,
        turn: &TurnPermission,
        session: &JsString,
        run: &JsString,
    ) -> bool {
        Arc::ptr_eq(&self.state, &turn.state)
            && &turn.run.session == session
            && &turn.run.run_id == run
            && turn.check(&lock(&self.state)).is_ok()
    }
    pub fn bind_turn(
        &self,
        session: JsString,
        run_id: JsString,
        options: PermissionTurnOptions,
        catalog: Arc<dyn ToolCatalog>,
        router: &mut ModuleRouter,
    ) -> Result<TurnPermission, HostError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(stale());
        }
        let run = Arc::new(Run {
            session: session.clone(),
            run_id,
            cancelled: Cancellation::new(),
            requests: Mutex::default(),
            options,
        });
        if let Some(previous) = state.turns.insert(session, run.clone()) {
            previous.close();
        }
        let turn = TurnPermission {
            state: self.state.clone(),
            run,
        };
        router.permission =
            port::Handler::new(catalog, Arc::new(turn.clone()), Arc::new(turn.clone()));
        Ok(turn)
    }
    pub fn close(&self) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.closed = true;
        state.releases += 1;
        for (_, run) in state.turns.drain() {
            run.close();
        }
    }
    pub fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }
    pub fn release_count(&self) -> usize {
        lock(&self.state).releases
    }
}
impl FamilyResource for InteractionPermission {
    fn dispose(&self) -> DisposeFuture<'_> {
        self.close();
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
#[derive(Clone)]
pub struct TurnPermission {
    state: Arc<Mutex<State>>,
    run: Arc<Run>,
}
impl TurnPermission {
    /// ProfiledPermissionDecisionPort applies admission only after the native
    /// provider returns ask. In particular, its existing deny message wins.
    fn prompt_denial(&self) -> Option<&'static str> {
        let options = &self.run.options;
        if options.profile.policy_mode == "disabled" {
            Some("Interaction is disabled for this profile.")
        } else if !options.profile.can_prompt || !options.context.can_prompt {
            Some("Prompts are disabled for this session.")
        } else if options.profile.permission_provider != "gateway" {
            Some("No interaction answerer is available.")
        } else {
            None
        }
    }

    pub(super) fn is_current(&self) -> bool {
        self.check(&lock(&self.state)).is_ok()
    }
    pub(super) fn cancellation(&self) -> Cancellation {
        self.run.cancelled.clone()
    }
    pub(super) fn same_run(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
    }
    pub(super) fn can_prompt(&self) -> bool {
        self.run.options.profile.permission_provider == "gateway"
            && self.run.options.profile.can_prompt
    }
    fn check(&self, state: &State) -> Result<(), HostError> {
        if state.closed
            || self.run.cancelled.is_cancelled()
            || !state
                .turns
                .get(&self.run.session)
                .is_some_and(|r| Arc::ptr_eq(r, &self.run))
        {
            return Err(stale());
        }
        Ok(())
    }
    pub fn run_id(&self) -> &JsString {
        &self.run.run_id
    }
    pub fn close(&self) {
        let mut state = lock(&self.state);
        self.run.close();
        if state
            .turns
            .get(&self.run.session)
            .is_some_and(|r| Arc::ptr_eq(r, &self.run))
        {
            state.turns.remove(&self.run.session);
        }
    }
    /// PermissionRequest callback boundary. Returns behavior/message, ready for
    /// a hook's specific.decision. Request IDs are host minted, never toolCallId.
    /// `suggestions` must already be extracted from hookInput.permissionSuggestions
    /// by the caller, following TS extractSessionAllowRules: select the first
    /// allow_session option with a rules array, keep object candidates, default
    /// missing/empty toolName to this tool, and retain only nonempty string
    /// patterns with source=session/behavior=allow. Pass an empty vector when
    /// none survive; request then remembers its single-tool fallback rule.
    pub fn request(
        &self,
        tool_name: JsString,
        tool_call_id: JsString,
        input: JsValue,
        suggestions: Vec<crate::config::permission::PermissionRule>,
        cancel: Cancellation,
    ) -> HostFuture<JsValue> {
        let turn = self.clone();
        Box::pin(async move {
            let request_id: JsString = uuid::Uuid::new_v4().to_string().into();
            let (sender, mut receiver) = oneshot::channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let resolve = sender.clone();
            let reject = sender;
            let guard = {
                let state = lock(&turn.state);
                turn.check(&state)?;
                // Match decide's effective order: native Gateway policy denial
                // precedes profile admission, including direct callback misuse.
                if turn.run.options.context.policy_can_prompt == Some(false) {
                    return Ok(denied(
                        "Permission prompt denied because Gateway policy disables prompts for this session.",
                    ));
                }
                if let Some(message) = turn.prompt_denial() {
                    return Ok(denied(message));
                }
                if cancel.is_cancelled() {
                    return Ok(denied("Permission prompt cancelled."));
                }
                let payload = object([
                    ("toolName", JsValue::String(tool_name.clone())),
                    ("toolCallId", JsValue::String(tool_call_id.clone())),
                    ("payload", input.clone()),
                ]);
                let registration = turn
                    .run
                    .options
                    .coordinator
                    .permission_bus()
                    .register(
                        &turn.run.session,
                        PendingEntry {
                            request_id: request_id.clone(),
                            tool_call_id,
                            tool_name: tool_name.clone(),
                            resolve: Arc::new(move |value| {
                                if let Some(sender) = lock(&resolve).take() {
                                    let _ = sender.send(value);
                                }
                            }),
                            reject: Arc::new(move |_| {
                                if let Some(sender) = lock(&reject).take() {
                                    let _ = sender.send(denied("Permission prompt cancelled."));
                                }
                            }),
                        },
                        Some(object([("payload", payload)])),
                    )
                    .map_err(|_| ModuleError::new("Permission request registration failed."))?;
                lock(&turn.run.requests).insert(request_id.clone(), registration);
                RequestGuard {
                    run: turn.run.clone(),
                    request_id: request_id.clone(),
                }
            };
            // Delivery can synchronously answer/reenter; never hold our gate.
            let delivered = (turn.run.options.emit)(object([
                ("type", "permission_request".into()),
                ("requestId", JsValue::String(request_id.clone())),
                ("toolName", JsValue::String(tool_name.clone())),
                ("payload", input),
            ]));
            let answer = if !delivered {
                // TS settle consumes the pending entry: a synchronous answer
                // wins even if emit subsequently reports no sink.
                receiver.try_recv().unwrap_or_else(|_| {
                    denied("Permission prompt could not be delivered to the Web UI.")
                })
            } else {
                tokio::select! {
                    biased;
                    _ = turn.run.cancelled.cancelled() => return Err(stale()),
                    _ = cancel.cancelled() => denied("Permission prompt cancelled."),
                    answer = receiver => answer.unwrap_or_else(|_| denied("Permission prompt denied.")),
                    _ = tokio::time::sleep(turn.run.options.timeout) => denied("Permission prompt timed out."),
                }
            };
            let state = lock(&turn.state);
            turn.check(&state)?;
            drop(guard);
            if answer
                .get("decision")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str("allow"))
            {
                if answer.get("remember").is_some_and(|v| {
                    !matches!(v, JsValue::Undefined | JsValue::Null | JsValue::Bool(false))
                        && !matches!(v, JsValue::Number(n) if *n == 0.0 || n.is_nan())
                        && !matches!(v, JsValue::String(s) if s.is_empty())
                }) {
                    let rules = if suggestions.is_empty() {
                        vec![crate::config::permission::PermissionRule::new(
                            "session", "allow", tool_name,
                        )]
                    } else {
                        suggestions
                    };
                    turn.run.options.rules.update_member("allow", |existing| {
                        for mut rule in rules {
                            rule.source = "session".into();
                            rule.behavior = "allow".into();
                            if !existing
                                .iter()
                                .filter_map(|v| {
                                    crate::config::permission::PermissionRule::from_value(
                                        &v.to_value(),
                                    )
                                })
                                .any(|v| {
                                    // TS createGatewayPermissionHook.ts:181-186 intentionally
                                    // ignores source here, unlike registry grant/merge.
                                    v.tool_name == rule.tool_name
                                        && v.pattern == rule.pattern
                                        && v.behavior.equals_str("allow")
                                })
                            {
                                existing.push(rule.into());
                            }
                        }
                    });
                }
                Ok(object([("behavior", "allow".into())]))
            } else {
                Ok(object([
                    ("behavior", "deny".into()),
                    (
                        "message",
                        answer
                            .get("reason")
                            .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
                            .or_else(|| answer.get("message"))
                            .cloned()
                            .unwrap_or("Permission prompt denied.".into()),
                    ),
                ]))
            }
        })
    }
}
struct RequestGuard {
    run: Arc<Run>,
    request_id: JsString,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Some(registration) = lock(&self.run.requests).remove(&self.request_id) {
            registration.dispose();
        }
    }
}
impl port::PermissionContext for TurnPermission {
    fn prepare(&self, _wire: JsValue) -> HostFuture<RuntimeContext> {
        let turn = self.clone();
        Box::pin(async move {
            turn.check(&lock(&turn.state))?;
            Ok(Arc::new(turn) as RuntimeContext)
        })
    }
}
impl port::PermissionDecision for TurnPermission {
    fn decide(
        &self,
        tool: Arc<dyn ToolDefinition>,
        input: JsValue,
        context: RuntimeContext,
        tool_call_id: JsString,
    ) -> HostFuture<JsValue> {
        let turn = self.clone();
        Box::pin(async move {
            let supplied = context.downcast_ref::<TurnPermission>().ok_or_else(stale)?;
            if !Arc::ptr_eq(&turn.run, &supplied.run) {
                return Err(stale());
            }
            turn.check(&lock(&turn.state))?;
            let mut context = turn.run.options.context.clone();
            context.rules = turn.run.options.rules.snapshot();
            let name = tool
                .fields()
                .get("name")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| ModuleError::new("Permission tool has no name."))?;
            let kind = tool
                .fields()
                .get("kind")
                .and_then(JsValue::as_string)
                .cloned();
            let adapter = ToolAdapter { tool, name, kind };
            let result = tokio::select! {
                biased;
                _ = turn.run.cancelled.cancelled() => return Err(stale()),
                result = permission::decide(&context, &adapter, &input, &tool_call_id) => result?,
            };
            turn.check(&lock(&turn.state))?;
            if result
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("ask"))
                && let Some(message) = turn.prompt_denial()
            {
                return Ok(object([
                    ("type", "deny".into()),
                    (
                        "reason",
                        object([("type", "runtime".into()), ("message", message.into())]),
                    ),
                    ("message", message.into()),
                ]));
            }
            Ok(result)
        })
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn denied(message: &str) -> JsValue {
    object([("behavior", "deny".into()), ("message", message.into())])
}
fn stale() -> HostError {
    ModuleError::new("Permission generation or run is closed or superseded.").into()
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}
pub(super) fn close_resource(resource: Option<Arc<dyn FamilyResource>>) -> RuntimeResult {
    if let Some(resource) = resource {
        resource
            .as_any()
            .downcast_ref::<InteractionPermission>()
            .ok_or_else(|| RuntimeError::new("Invalid interaction permission resource"))?
            .close();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
