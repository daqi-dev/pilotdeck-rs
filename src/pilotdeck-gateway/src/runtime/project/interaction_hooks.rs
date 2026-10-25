//! Configured hooks owned by a project generation, with opaque per-run receipts.
//! The caller keeps its RuntimeLease until the turn and its cleanup have drained.
use super::interaction_permission::{InteractionPermission, TurnPermission};
use super::{DisposeFuture, FamilyResource, RuntimeError, RuntimeResult};
use crate::{
    config::permission::PermissionRule,
    hooks::protocol::*,
    host::{
        HostError, HostFuture, ModuleError,
        modules::{ModuleRouter, lifecycle},
    },
    runtime::hooks::{
        AsyncHookCompletion, AsyncHookResponse, Cancellation, HookRuntime, HookRuntimeOptions,
        LifecycleDispatchInput, LifecycleDispatchResult, LifecycleRuntime,
        PendingAsyncHookDescriptor,
        exec::{
            CallbackFuture, CallbackHookHandler, CallbackInput, CallbackRegistration,
            CallbackResult,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, MutexGuard, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{runtime::Handle, sync::watch};

const PERMISSION_CALLBACK: &str = "pilotdeck.gateway.permission";

#[derive(Default)]
struct State {
    closed: bool,
    releases: usize,
    turns: HashMap<JsString, Arc<Run>>,
    draining: Vec<Arc<Run>>,
}
struct Run {
    owner: Weak<Mutex<State>>,
    session: JsString,
    run_id: JsString,
    runtime: LifecycleRuntime,
    permission: Option<TurnPermission>,
    registration: Option<CallbackRegistration>,
    cancelled: Cancellation,
    external: Cancellation,
    closed: AtomicBool,
    releases: AtomicUsize,
    done: watch::Sender<bool>,
    jobs: watch::Sender<usize>,
    executor: Handle,
    pending: Mutex<Vec<PendingAsyncHookDescriptor>>,
}
impl Run {
    fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancelled.cancel();
        if let Some(permission) = &self.permission {
            permission.close();
        }
        if let Some(registration) = &self.registration {
            registration.dispose();
        }
        lock(&self.pending).clear();
        let run = self.clone();
        // Cleanup survives a dropped/timed-out caller's dispose future.
        self.executor.spawn(async move {
            let mut jobs = run.jobs.subscribe();
            let _ = jobs.wait_for(|count| *count == 0).await;
            run.runtime.dispose().await;
            run.releases.fetch_add(1, Ordering::SeqCst);
            if let Some(owner) = run.owner.upgrade() {
                let mut state = lock(&owner);
                if state
                    .turns
                    .get(&run.session)
                    .is_some_and(|current| Arc::ptr_eq(current, &run))
                {
                    state.turns.remove(&run.session);
                }
                state.draining.retain(|current| !Arc::ptr_eq(current, &run));
            }
            run.done.send_replace(true);
        });
    }
    async fn wait_closed(&self) {
        let mut done = self.done.subscribe();
        let _ = done.wait_for(|done| *done).await;
    }
}

#[derive(Clone)]
pub struct InteractionHooks {
    state: Arc<Mutex<State>>,
    permission: InteractionPermission,
}
impl InteractionHooks {
    pub fn new(permission: InteractionPermission) -> Self {
        Self {
            state: Arc::default(),
            permission,
        }
    }
    /// `settings` is the host's leased session contribution config (TS
    /// ProjectSessionRuntimeBundle:380), never sidecar input. Keep that lease
    /// alongside the project lease until wait_closed completes.
    pub fn bind_turn(
        &self,
        binding: lifecycle::Binding,
        run_id: JsString,
        settings: HooksSettings,
        permission: Option<TurnPermission>,
        router: &mut ModuleRouter,
    ) -> Result<TurnHooks, HostError> {
        self.bind_with_options(
            binding,
            run_id,
            settings,
            permission,
            router,
            HookRuntimeOptions::default(),
        )
    }
    fn bind_with_options(
        &self,
        mut binding: lifecycle::Binding,
        run_id: JsString,
        mut settings: HooksSettings,
        permission: Option<TurnPermission>,
        router: &mut ModuleRouter,
        options: HookRuntimeOptions,
    ) -> Result<TurnHooks, HostError> {
        let executor = Handle::try_current()
            .map_err(|_| ModuleError::new("Hook binding requires a Tokio runtime."))?;
        let mut state = lock(&self.state);
        if state.closed {
            return Err(stale());
        }
        if let Some(permission) = &permission
            && !self
                .permission
                .owns_turn(permission, &binding.session_id, &run_id)
        {
            return Err(stale());
        }
        if let Some(previous) = state.turns.get(&binding.session_id)
            && previous
                .permission
                .as_ref()
                .zip(permission.as_ref())
                .is_some_and(|(old, new)| old.same_run(new))
        {
            return Err(ModuleError::new("A hook run requires its own permission binding.").into());
        }
        let prompting = permission.as_ref().is_some_and(TurnPermission::can_prompt);
        if prompting {
            let matcher = HookMatcher {
                hooks: vec![HookCommand::Callback {
                    name: PERMISSION_CALLBACK.into(),
                    condition: None,
                    status_message: None,
                }],
                ..Default::default()
            };
            if let Some((_, matchers)) = settings
                .iter_mut()
                .find(|(event, _)| event.equals_str("PermissionRequest"))
            {
                matchers.push(matcher);
            } else {
                settings.push(("PermissionRequest".into(), vec![matcher]));
            }
        }
        let runtime = HookRuntime::with_options(settings, options);
        let registration = if prompting {
            Some(
                runtime
                    .callback_executor()
                    .register(
                        &PERMISSION_CALLBACK.into(),
                        Arc::new(PermissionCallback(
                            permission.clone().expect("prompting has permission"),
                        )),
                    )
                    .map_err(ModuleError::new)?,
            )
        } else {
            None
        };
        let run = Arc::new(Run {
            owner: Arc::downgrade(&self.state),
            session: binding.session_id.clone(),
            run_id,
            runtime: LifecycleRuntime::new(runtime),
            permission,
            registration,
            cancelled: Cancellation::new(),
            external: binding.signal.take().unwrap_or_default(),
            closed: AtomicBool::new(false),
            releases: AtomicUsize::new(0),
            done: watch::channel(false).0,
            jobs: watch::channel(0).0,
            executor,
            pending: Mutex::default(),
        });
        state.draining.retain(|run| !*run.done.borrow());
        if let Some(previous) = state.turns.insert(run.session.clone(), run.clone()) {
            previous.close();
            state.draining.push(previous);
        }
        // The externally supplied abort is observed even when no dispatch is pending.
        let monitor = run.clone();
        let state_ref = Arc::downgrade(&self.state);
        let permission_cancel = run
            .permission
            .as_ref()
            .map(TurnPermission::cancellation)
            .unwrap_or_default();
        run.executor.spawn(async move {
            tokio::select! {
                _ = monitor.external.cancelled() => {},
                _ = permission_cancel.cancelled() => {},
                _ = monitor.cancelled.cancelled() => {},
            }
            if let Some(state) = state_ref.upgrade() {
                TurnHooks {
                    state,
                    run: monitor,
                }
                .close();
            }
        });
        binding.signal = Some(run.cancelled.clone());
        let turn = TurnHooks {
            state: self.state.clone(),
            run,
        };
        router.lifecycle = lifecycle::Handler::with_port(Arc::new(turn.clone()), binding);
        Ok(turn)
    }
    pub fn close(&self) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.closed = true;
        state.releases += 1;
        for run in state.turns.values().chain(&state.draining) {
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
impl FamilyResource for InteractionHooks {
    fn dispose(&self) -> DisposeFuture<'_> {
        self.close();
        let runs: Vec<_> = {
            let state = lock(&self.state);
            state
                .turns
                .values()
                .chain(&state.draining)
                .cloned()
                .collect()
        };
        Box::pin(async move {
            for run in runs {
                run.wait_closed().await;
            }
            Ok(())
        })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Receipt identity stays on the host; serialized descriptors are not authority.
#[derive(Clone)]
pub struct HookReceipt {
    run: Weak<Run>,
    descriptor: PendingAsyncHookDescriptor,
}
impl HookReceipt {
    pub fn descriptor(&self) -> &PendingAsyncHookDescriptor {
        &self.descriptor
    }
}
#[derive(Clone)]
pub struct TurnHooks {
    state: Arc<Mutex<State>>,
    run: Arc<Run>,
}
impl TurnHooks {
    fn check(&self, state: &State) -> Result<(), HostError> {
        if state.closed
            || self.run.closed.load(Ordering::SeqCst)
            || self.run.external.is_cancelled()
            || self
                .run
                .permission
                .as_ref()
                .is_some_and(|permission| !permission.is_current())
            || !state
                .turns
                .get(&self.run.session)
                .is_some_and(|run| Arc::ptr_eq(run, &self.run))
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
            .is_some_and(|run| Arc::ptr_eq(run, &self.run))
        {
            state.turns.remove(&self.run.session);
            state.draining.retain(|run| !*run.done.borrow());
            if !*self.run.done.borrow() {
                state.draining.push(self.run.clone());
            }
        }
    }
    pub async fn wait_closed(&self) {
        self.run.wait_closed().await;
    }
    pub fn release_count(&self) -> usize {
        self.run.releases.load(Ordering::SeqCst)
    }
    pub fn pending_receipts(&self) -> Result<Vec<HookReceipt>, HostError> {
        let state = lock(&self.state);
        self.check(&state)?;
        Ok(lock(&self.run.pending)
            .iter()
            .map(|descriptor| HookReceipt {
                run: Arc::downgrade(&self.run),
                descriptor: descriptor.clone(),
            })
            .collect())
    }
    fn valid_receipt(&self, state: &State, receipt: &HookReceipt) -> bool {
        self.check(state).is_ok()
            && receipt
                .run
                .upgrade()
                .is_some_and(|run| Arc::ptr_eq(&run, &self.run))
            && lock(&self.run.pending).iter().any(|d| {
                d.id == receipt.descriptor.id && d.generation == receipt.descriptor.generation
            })
    }
    pub fn complete_async_hook(
        &self,
        receipt: &HookReceipt,
        completion: AsyncHookCompletion,
    ) -> bool {
        let state = lock(&self.state);
        self.valid_receipt(&state, receipt)
            && self
                .run
                .runtime
                .complete_async_hook(&receipt.descriptor.id, completion)
    }
    pub fn cancel_async_hook(&self, receipt: &HookReceipt) -> bool {
        let state = lock(&self.state);
        if !self.valid_receipt(&state, receipt) {
            return false;
        }
        let cancelled = self.run.runtime.cancel_async_hook(&receipt.descriptor.id);
        lock(&self.run.pending).retain(|d| d.id != receipt.descriptor.id);
        cancelled
    }
    pub fn collect_async_responses(&self) -> Result<Vec<AsyncHookResponse>, HostError> {
        let state = lock(&self.state);
        self.check(&state)?;
        let responses = self.run.runtime.collect_async_hook_responses();
        self.run.runtime.remove_delivered_async_hook_responses();
        lock(&self.run.pending).retain(|d| !responses.iter().any(|r| r.id == d.id));
        Ok(responses)
    }
}
impl lifecycle::LifecyclePort for TurnHooks {
    fn dispatch(&self, mut input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
        let turn = self.clone();
        Box::pin(async move {
            {
                let state = lock(&turn.state);
                turn.check(&state)?;
                turn.run.jobs.send_modify(|count| *count += 1);
            }
            let job = DispatchJob(turn.run.clone());
            let worker = turn.clone();
            input.signal = Some(turn.run.cancelled.clone());
            // Keep shell TERM/drain alive even if the caller cancels or drops
            // its receive future. close waits for this job before disposal.
            let task = turn.run.executor.spawn(async move {
                let _job = job;
                worker.check(&lock(&worker.state))?;
                worker.run.runtime.dispatch(input).await
            });
            let mut on_drop = CancelDispatch(Some(turn.clone()));
            let result = tokio::select! {
                biased;
                _ = turn.run.cancelled.cancelled() => return Err(stale()),
                _ = turn.run.external.cancelled() => { turn.close(); return Err(stale()); },
                result = task => {
                    on_drop.0 = None;
                    result.map_err(|e| ModuleError::new(e.to_string()))??
                },
            };
            let state = lock(&turn.state);
            turn.check(&state)?;
            lock(&turn.run.pending).extend(result.pending_async_hooks.iter().cloned());
            on_drop.0 = None;
            Ok(result)
        })
    }
    fn dispose(&self) -> HostFuture<()> {
        self.close();
        let turn = self.clone();
        Box::pin(async move {
            turn.wait_closed().await;
            Ok(())
        })
    }
}

struct DispatchJob(Arc<Run>);
impl Drop for DispatchJob {
    fn drop(&mut self) {
        self.0.jobs.send_modify(|count| *count -= 1);
    }
}
struct CancelDispatch(Option<TurnHooks>);
impl Drop for CancelDispatch {
    fn drop(&mut self) {
        if let Some(turn) = &self.0 {
            turn.close();
        }
    }
}

struct PermissionCallback(TurnPermission);
impl CallbackHookHandler for PermissionCallback {
    fn call(&self, input: CallbackInput) -> CallbackFuture {
        let permission = self.0.clone();
        Box::pin(async move {
            let fields = &input.hook_input;
            let tool = fields
                .get("toolName")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or("UnknownTool".into());
            let call = fields
                .get("toolCallId")
                .and_then(JsValue::as_string)
                .or_else(|| fields.get("toolUseId").and_then(JsValue::as_string))
                .cloned()
                .unwrap_or_default();
            let payload = fields
                .get("toolInput")
                .or_else(|| fields.get("input"))
                .cloned()
                .unwrap_or(JsValue::Object(JsObject::new()));
            let suggestions = session_allow_rules(fields.get("permissionSuggestions"), &tool);
            let result = permission
                .request(
                    tool,
                    call,
                    payload,
                    suggestions,
                    input.signal.unwrap_or_default(),
                )
                .await
                .map_err(|error| JsString::from(error.to_string()))?;
            let decision = if result
                .get("behavior")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("allow"))
            {
                PermissionHookDecision::Allow {
                    updated_input: None,
                    updated_permissions: None,
                }
            } else {
                PermissionHookDecision::Deny {
                    message: result.get("message").and_then(JsValue::as_string).cloned(),
                    interrupt: None,
                }
            };
            Ok(CallbackResult::Output(HookOutput::Sync(Box::new(
                HookSyncOutput {
                    specific: Some(HookSpecificOutput {
                        hook_event_name: "PermissionRequest".into(),
                        decision: Some(decision),
                        additional_context: None,
                        initial_user_message: None,
                        watch_paths: None,
                        permission_decision: None,
                        permission_decision_reason: None,
                        updated_input: None,
                        updated_mcp_tool_output: None,
                        retry: None,
                        worktree_path: None,
                    }),
                    ..Default::default()
                },
            ))))
        })
    }
}
fn session_allow_rules(value: Option<&JsValue>, fallback: &JsString) -> Vec<PermissionRule> {
    let Some(JsValue::Array(options)) = value else {
        return vec![];
    };
    let Some(JsValue::Array(rules)) = options
        .iter()
        .filter_map(JsValue::as_object)
        .find(|option| {
            option
                .get("id")
                .and_then(JsValue::as_string)
                .is_some_and(|id| id.equals_str("allow_session"))
                && matches!(option.get("rules"), Some(JsValue::Array(_)))
        })
        .and_then(|option| option.get("rules"))
    else {
        return vec![];
    };
    rules
        .iter()
        .filter_map(JsValue::as_object)
        .filter_map(|rule| {
            let tool = rule
                .get("toolName")
                .and_then(JsValue::as_string)
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback);
            if tool.is_empty() {
                return None;
            }
            let mut result = PermissionRule::new("session", "allow", tool.clone());
            result.pattern = rule
                .get("pattern")
                .and_then(JsValue::as_string)
                .filter(|s| !s.is_empty())
                .cloned();
            Some(result)
        })
        .collect()
}
fn stale() -> HostError {
    ModuleError::new("Hook generation or run is closed or superseded.").into()
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}
pub(super) async fn close_resource(resource: Option<Arc<dyn FamilyResource>>) -> RuntimeResult {
    if let Some(resource) = resource {
        resource
            .as_any()
            .downcast_ref::<InteractionHooks>()
            .ok_or_else(|| RuntimeError::new("Invalid interaction hooks resource"))?
            .dispose()
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
