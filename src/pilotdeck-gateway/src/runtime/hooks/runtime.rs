//! `HookRuntime` and `LifecycleRuntime` (TS `extension/hooks/execution/HookRuntime.ts`,
//! `lifecycle/runtime/LifecycleRuntime.ts`).
//!
//! Hooks execute strictly in configuration order, one at a time: TS awaits
//! each executor inside the matcher loop, so concurrent execution would change
//! both side effects and the returned effect/event order. `run` is re-entrant
//! across callers but not parallel within one dispatch.
use super::{
    exec::{
        AgentHookExecutor, CallbackHookExecutor, CallbackInput, Cancellation, CommandHookExecutor,
        CommandHookOptions, ExecutionResult, HookOutcome, HttpHookExecutor, HttpHookOptions,
        PromptHookExecutor, SESSION_END_HOOK_TIMEOUT_MS, ShellPort,
    },
    pending::{
        AsyncHookCompletion, AsyncHookResponse, PendingAsyncHookDescriptor, PendingAsyncHookInput,
        PendingAsyncHooks,
    },
};
use crate::{
    hooks::{
        config::{
            HookConditionError, HookConditionInput, match_hook_condition, match_hook_matcher,
        },
        protocol::{HookCommand, HookMatcher, HookOutput, HookSpecificOutput, HooksSettings},
    },
    host::{HostError, ModuleError},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

// ------------------------------------------------------------------ effects / errors

#[derive(Clone, Debug, PartialEq)]
pub enum HookEffect {
    AdditionalContext {
        content: JsString,
        source: JsString,
    },
    SystemMessage {
        content: JsString,
    },
    Block {
        reason: JsString,
        stop_reason: Option<JsString>,
    },
    PermissionDecision {
        behavior: JsString,
        reason: Option<JsString>,
    },
    UpdatedToolInput {
        input: JsObject,
    },
    UpdatedMcpToolOutput {
        output: JsValue,
    },
    PermissionRequestResult {
        result: JsValue,
    },
    InitialUserMessage {
        message: JsString,
    },
    WatchPaths {
        paths: Vec<JsString>,
    },
    WorktreePath {
        path: JsString,
    },
    RetryPermissionDenied,
}
impl HookEffect {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        match self {
            Self::AdditionalContext { content, source } => {
                object.insert("type".into(), "additional_context".into());
                object.insert("content".into(), JsValue::String(content.clone()));
                object.insert("source".into(), JsValue::String(source.clone()));
            }
            Self::SystemMessage { content } => {
                object.insert("type".into(), "system_message".into());
                object.insert("content".into(), JsValue::String(content.clone()));
            }
            Self::Block {
                reason,
                stop_reason,
            } => {
                object.insert("type".into(), "block".into());
                object.insert("reason".into(), JsValue::String(reason.clone()));
                if let Some(value) = stop_reason {
                    object.insert("stopReason".into(), JsValue::String(value.clone()));
                }
            }
            Self::PermissionDecision { behavior, reason } => {
                object.insert("type".into(), "permission_decision".into());
                object.insert("behavior".into(), JsValue::String(behavior.clone()));
                if let Some(value) = reason {
                    object.insert("reason".into(), JsValue::String(value.clone()));
                }
            }
            Self::UpdatedToolInput { input } => {
                object.insert("type".into(), "updated_tool_input".into());
                object.insert("input".into(), JsValue::Object(input.clone()));
            }
            Self::UpdatedMcpToolOutput { output } => {
                object.insert("type".into(), "updated_mcp_tool_output".into());
                object.insert("output".into(), output.clone());
            }
            Self::PermissionRequestResult { result } => {
                object.insert("type".into(), "permission_request_result".into());
                object.insert("result".into(), result.clone());
            }
            Self::InitialUserMessage { message } => {
                object.insert("type".into(), "initial_user_message".into());
                object.insert("message".into(), JsValue::String(message.clone()));
            }
            Self::WatchPaths { paths } => {
                object.insert("type".into(), "watch_paths".into());
                object.insert(
                    "paths".into(),
                    JsValue::Array(paths.iter().cloned().map(JsValue::String).collect()),
                );
            }
            Self::WorktreePath { path } => {
                object.insert("type".into(), "worktree_path".into());
                object.insert("path".into(), JsValue::String(path.clone()));
            }
            Self::RetryPermissionDenied => {
                object.insert("type".into(), "retry_permission_denied".into());
            }
        }
        JsValue::Object(object)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleError {
    pub code: &'static str,
    pub message: JsString,
    pub hook_name: Option<JsString>,
    pub exit_code: Option<i64>,
}
impl LifecycleError {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("code".into(), self.code.into());
        object.insert("message".into(), JsValue::String(self.message.clone()));
        if let Some(value) = &self.hook_name {
            object.insert("hookName".into(), JsValue::String(value.clone()));
        }
        if let Some(value) = self.exit_code {
            object.insert("exitCode".into(), JsValue::Number(value as f64));
        }
        JsValue::Object(object)
    }
}

// ------------------------------------------------------------------ run input / result

pub struct HookRuntimeRunInput {
    pub event: JsString,
    pub hook_input: JsObject,
    pub match_query: Option<JsString>,
    pub cwd: JsString,
    pub env: Option<JsObject>,
    pub signal: Option<Cancellation>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct HookRuntimeRunResult {
    pub effects: Vec<HookEffect>,
    pub events: Vec<JsObject>,
    pub blocking_errors: Vec<LifecycleError>,
    pub non_blocking_errors: Vec<LifecycleError>,
    pub pending_async_hooks: Vec<PendingAsyncHookDescriptor>,
}

// ------------------------------------------------------------------ async registration bridge

pub struct AsyncHookRegistration {
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub invocation_id: JsString,
    pub timeout_ms: Option<f64>,
}
pub trait AsyncHookRegistrationHandler: Send + Sync {
    fn handle(&self, registration: AsyncHookRegistration);
}

// ------------------------------------------------------------------ event bus

pub trait HookExecutionEventHandler: Send + Sync {
    fn handle(&self, event: &JsObject);
}
struct SubscriptionImpl {
    handler: Arc<dyn HookExecutionEventHandler>,
    active: AtomicBool,
    id: u64,
}
#[derive(Clone)]
pub struct HookExecutionEventSubscription {
    inner: Arc<SubscriptionImpl>,
    bus: Arc<Mutex<EventBusState>>,
}
impl HookExecutionEventSubscription {
    pub fn active(&self) -> bool {
        self.inner.active.load(Ordering::SeqCst)
    }
    pub fn dispose(&self) {
        if !self.inner.active.swap(false, Ordering::SeqCst) {
            return;
        }
        self.bus
            .lock()
            .unwrap()
            .subscriptions
            .retain(|subscription| subscription.id != self.inner.id);
    }
}
#[derive(Default)]
struct EventBusState {
    subscriptions: Vec<Arc<SubscriptionImpl>>,
    disposed: bool,
    next_id: u64,
}
#[derive(Clone, Default)]
struct EventBus(Arc<Mutex<EventBusState>>);
impl EventBus {
    fn subscribe(
        &self,
        handler: Arc<dyn HookExecutionEventHandler>,
    ) -> Result<HookExecutionEventSubscription, HostError> {
        let mut state = self.0.lock().unwrap();
        if state.disposed {
            return Err(ModuleError::new(
                "Cannot subscribe to hook execution events after the bus is disposed.",
            )
            .into());
        }
        state.next_id += 1;
        let subscription = Arc::new(SubscriptionImpl {
            handler,
            active: AtomicBool::new(true),
            id: state.next_id,
        });
        state.subscriptions.push(subscription.clone());
        Ok(HookExecutionEventSubscription {
            inner: subscription,
            bus: self.0.clone(),
        })
    }
    fn emit(&self, event: &JsObject) {
        let subscriptions = {
            let state = self.0.lock().unwrap();
            if state.disposed {
                return;
            }
            state.subscriptions.clone()
        };
        for subscription in subscriptions {
            if !subscription.active.load(Ordering::SeqCst) {
                continue;
            }
            // A diagnostic observer is never part of hook execution semantics.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                subscription.handler.handle(event);
            }));
        }
    }
    fn dispose(&self) {
        let mut state = self.0.lock().unwrap();
        if state.disposed {
            return;
        }
        state.disposed = true;
        for subscription in &state.subscriptions {
            subscription.active.store(false, Ordering::SeqCst);
        }
        state.subscriptions.clear();
    }
}

// ------------------------------------------------------------------ runtime state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeState {
    Active,
    Draining,
    Disposed,
}
impl RuntimeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

pub struct HookRuntimeOptions {
    /// `Date#toISOString()` for `pendingAsyncHooks[].startedAt`.
    pub clock: Arc<dyn Fn() -> JsString + Send + Sync>,
    pub shell: Option<Arc<dyn ShellPort>>,
    pub on_async_registered: Option<Arc<dyn AsyncHookRegistrationHandler>>,
}
impl Default for HookRuntimeOptions {
    fn default() -> Self {
        Self {
            clock: Arc::new(default_clock),
            shell: None,
            on_async_registered: None,
        }
    }
}

fn default_clock() -> JsString {
    crate::runtime::context::prompt::iso_timestamp_now().into()
}

struct Inner {
    settings: HooksSettings,
    command: CommandHookExecutor,
    prompt: PromptHookExecutor,
    http: HttpHookExecutor,
    agent: AgentHookExecutor,
    callback: CallbackHookExecutor,
    pending: PendingAsyncHooks,
    bus: EventBus,
    state: Mutex<RuntimeState>,
    in_flight: Mutex<usize>,
    drained: watch::Sender<usize>,
    dispose_done: watch::Sender<bool>,
    next_async_hook_id: Mutex<u64>,
    clock: Arc<dyn Fn() -> JsString + Send + Sync>,
    on_async_registered: Option<Arc<dyn AsyncHookRegistrationHandler>>,
}

#[derive(Clone)]
pub struct HookRuntime(Arc<Inner>);
impl HookRuntime {
    pub fn new(settings: HooksSettings) -> Self {
        Self::with_options(settings, HookRuntimeOptions::default())
    }
    pub fn with_options(settings: HooksSettings, options: HookRuntimeOptions) -> Self {
        let command = match options.shell {
            Some(shell) => CommandHookExecutor::new(shell),
            None => CommandHookExecutor::default(),
        };
        Self(Arc::new(Inner {
            settings,
            command,
            prompt: PromptHookExecutor::default(),
            http: HttpHookExecutor::default(),
            agent: AgentHookExecutor::default(),
            callback: CallbackHookExecutor::default(),
            pending: PendingAsyncHooks::default(),
            bus: EventBus::default(),
            state: Mutex::new(RuntimeState::Active),
            in_flight: Mutex::new(0),
            drained: watch::channel(0).0,
            dispose_done: watch::channel(false).0,
            next_async_hook_id: Mutex::new(0),
            clock: options.clock,
            on_async_registered: options.on_async_registered,
        }))
    }
    pub fn lifecycle_state(&self) -> &'static str {
        self.0.state.lock().unwrap().as_str()
    }
    pub fn callback_executor(&self) -> CallbackHookExecutor {
        self.0.callback.clone()
    }
    pub fn subscribe_execution_events(
        &self,
        handler: Arc<dyn HookExecutionEventHandler>,
    ) -> Result<HookExecutionEventSubscription, HostError> {
        // Hold the state lock across the bus subscription: dispose sets
        // Draining under this lock before disposing the bus, so a subscription
        // can never be created into an already-disposed bus.
        let state = self.0.state.lock().unwrap();
        if *state != RuntimeState::Active {
            return Err(ModuleError::new(format!(
                "Cannot subscribe to hook execution events; hook runtime is {}.",
                state.as_str()
            ))
            .into());
        }
        self.0.bus.subscribe(handler)
    }
    pub async fn run(&self, input: HookRuntimeRunInput) -> Result<HookRuntimeRunResult, HostError> {
        {
            let state = self.0.state.lock().unwrap();
            if *state != RuntimeState::Active {
                return Err(ModuleError::new(format!(
                    "Cannot run hooks; hook runtime is {}.",
                    state.as_str()
                ))
                .into());
            }
            let mut in_flight = self.0.in_flight.lock().unwrap();
            *in_flight += 1;
            self.0.drained.send_replace(*in_flight);
        }
        let guard = InFlightGuard(self.0.clone());
        let result = self.run_active(input).await;
        drop(guard);
        result
    }
    async fn run_active(
        &self,
        input: HookRuntimeRunInput,
    ) -> Result<HookRuntimeRunResult, HostError> {
        let mut result = HookRuntimeRunResult::default();
        let Some((_, matchers)) = self
            .0
            .settings
            .iter()
            .find(|(event, _)| event == &input.event)
        else {
            return Ok(result);
        };
        for matcher in matchers {
            if !match_hook_matcher(matcher.matcher.as_ref(), input.match_query.as_ref()) {
                continue;
            }
            for hook in &matcher.hooks {
                if !hook_condition(hook, &input.hook_input)? {
                    continue;
                }
                let hook_name = hook_name(matcher, hook);
                let started = started_event(&input.event, &input.hook_input, &hook_name);
                result.events.push(started.clone());
                self.0.bus.emit(&started);

                let execution = self
                    .execute_hook(hook, &input, matcher.plugin_root.as_ref())
                    .await;
                let response =
                    response_event(&input.event, &input.hook_input, &hook_name, &execution);
                result.events.push(response.clone());
                self.0.bus.emit(&response);

                if let HookOutput::Async(r#async) = &execution.output {
                    let invocation_id = r#async.invocation_id.clone();
                    match (&invocation_id, &self.0.on_async_registered) {
                        (Some(invocation_id), Some(handler)) if !invocation_id.is_empty() => {
                            handler.handle(AsyncHookRegistration {
                                hook_name: hook_name.clone(),
                                hook_event: input.event.clone(),
                                invocation_id: invocation_id.clone(),
                                timeout_ms: r#async.timeout_ms,
                            });
                        }
                        _ => {
                            let id = {
                                let mut counter = self.0.next_async_hook_id.lock().unwrap();
                                *counter += 1;
                                JsString::from(format!(
                                    "{}:{}",
                                    String::from_utf16_lossy(hook_name.units()),
                                    *counter
                                ))
                            };
                            let registration = self.0.pending.register(PendingAsyncHookInput {
                                id,
                                started_at: (self.0.clock)(),
                                hook_name: hook_name.clone(),
                                hook_event: input.event.clone(),
                                stdout: execution.stdout.clone(),
                                stderr: execution.stderr.clone(),
                                async_rewake: command_async_rewake(hook),
                            });
                            let registration = match registration {
                                Ok(registration) => registration,
                                Err(message) => {
                                    return Err(ModuleError::new(message).into());
                                }
                            };
                            if let Some(descriptor) = self.0.pending.describe(registration.id()) {
                                result.pending_async_hooks.push(descriptor);
                            }
                        }
                    }
                }

                if execution.outcome == HookOutcome::Blocking {
                    let message = first_non_empty(&[
                        execution.stderr.clone(),
                        execution.stdout.clone(),
                        "Hook blocked execution.".into(),
                    ]);
                    result.blocking_errors.push(LifecycleError {
                        code: "hook_blocking_error",
                        message: message.clone(),
                        hook_name: Some(hook_name.clone()),
                        exit_code: execution.exit_code,
                    });
                    result.effects.push(HookEffect::Block {
                        reason: message,
                        stop_reason: None,
                    });
                } else if matches!(
                    execution.outcome,
                    HookOutcome::NonBlockingError | HookOutcome::Timeout | HookOutcome::Cancelled
                ) {
                    let code = if execution.outcome == HookOutcome::Cancelled {
                        "hook_cancelled"
                    } else {
                        "hook_non_blocking_error"
                    };
                    let message = first_non_empty(&[
                        execution.stderr.clone(),
                        execution.stdout.clone(),
                        format!("Hook ended with outcome {}.", execution.outcome.as_str()).into(),
                    ]);
                    result.non_blocking_errors.push(LifecycleError {
                        code,
                        message,
                        hook_name: Some(hook_name.clone()),
                        exit_code: execution.exit_code,
                    });
                }
                result
                    .effects
                    .extend(effects_from_hook_output(&execution.output, &hook_name));
            }
        }
        Ok(result)
    }
    async fn execute_hook(
        &self,
        hook: &HookCommand,
        input: &HookRuntimeRunInput,
        plugin_root: Option<&JsString>,
    ) -> ExecutionResult {
        match hook {
            HookCommand::Command { command, .. } => {
                self.0
                    .command
                    .execute(CommandHookOptions {
                        command: command.clone(),
                        hook_input: input.hook_input.clone(),
                        cwd: plugin_root.cloned().unwrap_or_else(|| input.cwd.clone()),
                        env: input.env.clone(),
                        signal: input.signal.clone(),
                        timeout_ms: if input.event.equals_str("SessionEnd") {
                            Some(SESSION_END_HOOK_TIMEOUT_MS)
                        } else {
                            None
                        },
                    })
                    .await
            }
            HookCommand::Prompt { prompt, model, .. } => {
                self.0
                    .prompt
                    .execute(
                        prompt,
                        model.as_ref(),
                        &input.hook_input,
                        input.signal.clone(),
                    )
                    .await
            }
            HookCommand::Http {
                url,
                headers,
                allowed_env_vars,
                ..
            } => {
                self.0
                    .http
                    .execute(HttpHookOptions {
                        url: url.clone(),
                        headers: headers.clone(),
                        allowed_env_vars: allowed_env_vars.clone(),
                        hook_input: input.hook_input.clone(),
                        env: input.env.clone(),
                        signal: input.signal.clone(),
                    })
                    .await
            }
            HookCommand::Agent { prompt, model, .. } => {
                self.0
                    .agent
                    .execute(
                        prompt,
                        model.as_ref(),
                        &input.hook_input,
                        input.signal.clone(),
                    )
                    .await
            }
            HookCommand::Callback { name, .. } => {
                self.0
                    .callback
                    .execute(
                        name,
                        CallbackInput {
                            hook_input: input.hook_input.clone(),
                            signal: input.signal.clone(),
                        },
                    )
                    .await
            }
        }
    }
    pub fn collect_async_responses(&self) -> Vec<AsyncHookResponse> {
        self.0.pending.collect_responses()
    }
    pub fn complete_async_hook(&self, id: &JsString, completion: AsyncHookCompletion) -> bool {
        self.0.pending.complete(id, completion)
    }
    pub fn cancel_async_hook(&self, id: &JsString) -> bool {
        self.0.pending.cancel(id)
    }
    pub fn remove_delivered_async_responses(&self) {
        self.0.pending.remove_delivered();
    }
    pub async fn dispose(&self) {
        let mut done = self.0.dispose_done.subscribe();
        let first = {
            let mut state = self.0.state.lock().unwrap();
            match *state {
                RuntimeState::Active => {
                    *state = RuntimeState::Draining;
                    true
                }
                RuntimeState::Draining | RuntimeState::Disposed => false,
            }
        };
        if !first {
            // A second dispose returns the first drain, like TS's drainPromise.
            let _ = done.wait_for(|done| *done).await;
            return;
        }
        self.0.bus.dispose();
        self.0.pending.dispose().await;
        self.0.callback.dispose().await;
        let mut drained = self.0.drained.subscribe();
        let _ = drained.wait_for(|count| *count == 0).await;
        {
            let mut state = self.0.state.lock().unwrap();
            *state = RuntimeState::Disposed;
        }
        self.0.dispose_done.send_replace(true);
    }
}

struct InFlightGuard(Arc<Inner>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut in_flight = self.0.in_flight.lock().unwrap();
        *in_flight = in_flight.saturating_sub(1);
        self.0.drained.send_replace(*in_flight);
    }
}

fn hook_condition(hook: &HookCommand, input: &JsObject) -> Result<bool, HostError> {
    let condition = match hook {
        HookCommand::Command { common, .. }
        | HookCommand::Prompt { common, .. }
        | HookCommand::Http { common, .. }
        | HookCommand::Agent { common, .. } => common.condition.as_ref(),
        HookCommand::Callback { condition, .. } => condition.as_ref(),
    };
    match match_hook_condition(
        condition,
        HookConditionInput {
            tool_name: input.get("toolName").and_then(JsValue::as_string),
            tool_input: input.get("toolInput"),
        },
    ) {
        Ok(matches) => Ok(matches),
        Err(error) => Err(ModuleError::new(condition_error_message(error)).into()),
    }
}

fn condition_error_message(error: HookConditionError) -> JsString {
    // The error crosses the host boundary as `error.message`, not
    // `String(error)`; the dispatcher never catches it.
    JsString::from(error.message())
}

fn hook_type(hook: &HookCommand) -> &'static str {
    match hook {
        HookCommand::Command { .. } => "command",
        HookCommand::Prompt { .. } => "prompt",
        HookCommand::Http { .. } => "http",
        HookCommand::Agent { .. } => "agent",
        HookCommand::Callback { .. } => "callback",
    }
}

fn hook_name(matcher: &HookMatcher, hook: &HookCommand) -> JsString {
    // TS `matcher.pluginName ? ... : ...`: the empty string is falsy.
    match &matcher.plugin_name {
        Some(plugin) if !plugin.is_empty() => JsString::from(format!(
            "{}:{}",
            String::from_utf16_lossy(plugin.units()),
            hook_type(hook)
        )),
        _ => JsString::from(hook_type(hook)),
    }
}

fn command_async_rewake(hook: &HookCommand) -> Option<bool> {
    match hook {
        HookCommand::Command { async_rewake, .. } => *async_rewake,
        _ => None,
    }
}

fn started_event(event: &JsString, input: &JsObject, hook_name: &JsString) -> JsObject {
    let mut object = JsObject::new();
    object.insert("type".into(), "started".into());
    object.insert("sessionId".into(), session_id(input));
    object.insert("hookName".into(), JsValue::String(hook_name.clone()));
    object.insert("hookEvent".into(), JsValue::String(event.clone()));
    object
}

fn response_event(
    event: &JsString,
    input: &JsObject,
    hook_name: &JsString,
    result: &ExecutionResult,
) -> JsObject {
    let mut object = JsObject::new();
    object.insert("type".into(), "response".into());
    object.insert("sessionId".into(), session_id(input));
    object.insert("hookName".into(), JsValue::String(hook_name.clone()));
    object.insert("hookEvent".into(), JsValue::String(event.clone()));
    object.insert("stdout".into(), JsValue::String(result.stdout.clone()));
    object.insert("stderr".into(), JsValue::String(result.stderr.clone()));
    if let Some(code) = result.exit_code {
        object.insert("exitCode".into(), JsValue::Number(code as f64));
    }
    object.insert("outcome".into(), result.outcome.as_str().into());
    if let HookOutput::Async(r#async) = &result.output {
        if let Some(id) = &r#async.invocation_id
            && !id.is_empty()
        {
            object.insert("asyncInvocationId".into(), JsValue::String(id.clone()));
        }
        if let Some(timeout) = r#async.timeout_ms {
            object.insert("asyncTimeoutMs".into(), JsValue::Number(timeout));
        }
    }
    object
}

fn session_id(input: &JsObject) -> JsValue {
    input
        .get("sessionId")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

fn first_non_empty(values: &[JsString]) -> JsString {
    values
        .iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn effects_from_hook_output(output: &HookOutput, hook_name: &JsString) -> Vec<HookEffect> {
    let HookOutput::Sync(sync) = output else {
        return Vec::new();
    };
    let mut effects = Vec::new();
    if let Some(value) = &sync.system_message
        && !value.is_empty()
    {
        effects.push(HookEffect::SystemMessage {
            content: value.clone(),
        });
    }
    if sync.continue_ == Some(false)
        || sync.decision == Some(crate::hooks::protocol::HookDecision::Block)
    {
        effects.push(HookEffect::Block {
            reason: sync
                .reason
                .clone()
                .or_else(|| sync.stop_reason.clone())
                .unwrap_or_else(|| "Hook blocked execution.".into()),
            stop_reason: sync.stop_reason.clone(),
        });
    }
    if let Some(HookSpecificOutput {
        additional_context,
        initial_user_message,
        watch_paths,
        worktree_path,
        permission_decision,
        permission_decision_reason,
        updated_input,
        updated_mcp_tool_output,
        decision,
        retry,
        ..
    }) = &sync.specific
    {
        if let Some(value) = additional_context
            && !value.is_empty()
        {
            effects.push(HookEffect::AdditionalContext {
                content: value.clone(),
                source: hook_name.clone(),
            });
        }
        if let Some(value) = initial_user_message
            && !value.is_empty()
        {
            effects.push(HookEffect::InitialUserMessage {
                message: value.clone(),
            });
        }
        if let Some(paths) = watch_paths
            && !paths.is_empty()
        {
            effects.push(HookEffect::WatchPaths {
                paths: paths.clone(),
            });
        }
        if let Some(path) = worktree_path
            && !path.is_empty()
        {
            effects.push(HookEffect::WorktreePath { path: path.clone() });
        }
        if let Some(behavior) = permission_decision {
            effects.push(HookEffect::PermissionDecision {
                behavior: match behavior {
                    crate::hooks::protocol::PermissionDecision::Allow => "allow",
                    crate::hooks::protocol::PermissionDecision::Deny => "deny",
                    crate::hooks::protocol::PermissionDecision::Ask => "ask",
                    crate::hooks::protocol::PermissionDecision::Passthrough => "passthrough",
                }
                .into(),
                reason: permission_decision_reason.clone(),
            });
        }
        if let Some(input) = updated_input {
            effects.push(HookEffect::UpdatedToolInput {
                input: input.clone(),
            });
        }
        if let Some(output) = updated_mcp_tool_output {
            effects.push(HookEffect::UpdatedMcpToolOutput {
                output: output.clone(),
            });
        }
        if let Some(decision) = decision {
            effects.push(HookEffect::PermissionRequestResult {
                result: permission_decision_to_value(decision),
            });
        }
        if *retry == Some(true) {
            effects.push(HookEffect::RetryPermissionDenied);
        }
    }
    effects
}

fn permission_decision_to_value(
    decision: &crate::hooks::protocol::PermissionHookDecision,
) -> JsValue {
    use crate::hooks::protocol::PermissionHookDecision;
    let mut object = JsObject::new();
    match decision {
        PermissionHookDecision::Allow {
            updated_input,
            updated_permissions,
        } => {
            object.insert("behavior".into(), "allow".into());
            if let Some(value) = updated_input {
                object.insert("updatedInput".into(), JsValue::Object(value.clone()));
            }
            if let Some(values) = updated_permissions {
                object.insert("updatedPermissions".into(), JsValue::Array(values.clone()));
            }
        }
        PermissionHookDecision::Deny { message, interrupt } => {
            object.insert("behavior".into(), "deny".into());
            if let Some(value) = message {
                object.insert("message".into(), JsValue::String(value.clone()));
            }
            if let Some(value) = interrupt {
                object.insert("interrupt".into(), JsValue::Bool(*value));
            }
        }
    }
    JsValue::Object(object)
}

// ------------------------------------------------------------------ lifecycle runtime

pub struct LifecycleDispatchInput {
    pub event: JsString,
    pub base_input: JsObject,
    pub payload: Option<JsObject>,
    pub match_query: Option<JsString>,
    pub cwd: JsString,
    pub env: Option<JsObject>,
    pub signal: Option<Cancellation>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct LifecycleDispatchResult {
    pub effects: Vec<HookEffect>,
    pub events: Vec<JsObject>,
    pub blocking_errors: Vec<LifecycleError>,
    pub non_blocking_errors: Vec<LifecycleError>,
    pub pending_async_hooks: Vec<PendingAsyncHookDescriptor>,
}
impl LifecycleDispatchResult {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "effects".into(),
            JsValue::Array(self.effects.iter().map(HookEffect::to_value).collect()),
        );
        object.insert(
            "messages".into(),
            JsValue::Array(messages_from_effects(&self.effects)),
        );
        object.insert(
            "events".into(),
            JsValue::Array(self.events.iter().cloned().map(JsValue::Object).collect()),
        );
        object.insert(
            "blockingErrors".into(),
            JsValue::Array(
                self.blocking_errors
                    .iter()
                    .map(LifecycleError::to_value)
                    .collect(),
            ),
        );
        object.insert(
            "nonBlockingErrors".into(),
            JsValue::Array(
                self.non_blocking_errors
                    .iter()
                    .map(LifecycleError::to_value)
                    .collect(),
            ),
        );
        object.insert(
            "pendingAsyncHooks".into(),
            JsValue::Array(
                self.pending_async_hooks
                    .iter()
                    .map(PendingAsyncHookDescriptor::to_value)
                    .collect(),
            ),
        );
        JsValue::Object(object)
    }
}

fn messages_from_effects(effects: &[HookEffect]) -> Vec<JsValue> {
    let mut messages = Vec::new();
    for effect in effects {
        let HookEffect::AdditionalContext { content, source } = effect else {
            continue;
        };
        let text = JsString::from(format!(
            "<hook_context source=\"{}\">\n{}\n</hook_context>",
            escape_attribute(source),
            String::from_utf16_lossy(content.units())
        ));
        let mut block = JsObject::new();
        block.insert("type".into(), "text".into());
        block.insert("text".into(), JsValue::String(text));
        let mut metadata = JsObject::new();
        metadata.insert("synthetic".into(), JsValue::Bool(true));
        metadata.insert("purpose".into(), "hook_context".into());
        let mut message = JsObject::new();
        message.insert("role".into(), "user".into());
        message.insert(
            "content".into(),
            JsValue::Array(vec![JsValue::Object(block)]),
        );
        message.insert("metadata".into(), JsValue::Object(metadata));
        messages.push(JsValue::Object(message));
    }
    messages
}

/// TS interpolates the raw source into a double-quoted attribute: only `&`,
/// `<`, `>` and `"` would change the text; TS does not escape any of them.
fn escape_attribute(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

#[derive(Clone)]
pub struct LifecycleRuntime {
    hooks: HookRuntime,
    null: bool,
}
impl LifecycleRuntime {
    pub fn new(hooks: HookRuntime) -> Self {
        Self { hooks, null: false }
    }
    /// TS `NullLifecycleRuntime`: dispatch is empty, but dispose still reaches
    /// the empty hook runtime.
    pub fn null() -> Self {
        Self {
            hooks: HookRuntime::new(Vec::new()),
            null: true,
        }
    }
    pub fn hooks(&self) -> &HookRuntime {
        &self.hooks
    }
    pub fn subscribe_hook_execution_events(
        &self,
        handler: Arc<dyn HookExecutionEventHandler>,
    ) -> Result<HookExecutionEventSubscription, HostError> {
        self.hooks.subscribe_execution_events(handler)
    }
    pub fn complete_async_hook(&self, id: &JsString, completion: AsyncHookCompletion) -> bool {
        self.hooks.complete_async_hook(id, completion)
    }
    pub fn cancel_async_hook(&self, id: &JsString) -> bool {
        self.hooks.cancel_async_hook(id)
    }
    pub fn collect_async_hook_responses(&self) -> Vec<AsyncHookResponse> {
        self.hooks.collect_async_responses()
    }
    pub fn remove_delivered_async_hook_responses(&self) {
        self.hooks.remove_delivered_async_responses();
    }
    pub async fn dispatch(
        &self,
        input: LifecycleDispatchInput,
    ) -> Result<LifecycleDispatchResult, HostError> {
        if self.null {
            return Ok(LifecycleDispatchResult::default());
        }
        let empty = JsObject::new();
        let hook_input = crate::hooks::protocol::create_hook_input(
            &input.event,
            &input.base_input,
            input.payload.as_ref().unwrap_or(&empty),
        );
        let result = self
            .hooks
            .run(HookRuntimeRunInput {
                event: input.event,
                hook_input,
                match_query: input.match_query,
                cwd: input.cwd,
                env: input.env,
                signal: input.signal,
            })
            .await?;
        Ok(LifecycleDispatchResult {
            effects: result.effects,
            events: result.events,
            blocking_errors: result.blocking_errors,
            non_blocking_errors: result.non_blocking_errors,
            pending_async_hooks: result.pending_async_hooks,
        })
    }
    pub async fn dispose(&self) {
        self.hooks.dispose().await;
    }
}

/// TS `serializeLifecycleDispatchResult`, including the field order the sidecar
/// observes and the always-present `pendingAsyncHooks`.
pub fn serialize_lifecycle_dispatch_result(result: &LifecycleDispatchResult) -> JsValue {
    result.to_value()
}

/// TS `parseDispatchResult` validation in `hostLifecycleRuntime.ts`. Rust keeps
/// the boundary value; callers that need typed fields validate separately.
pub fn validate_lifecycle_dispatch_result(value: &JsValue) -> Result<(), JsString> {
    let Some(result) = value.as_object() else {
        return Err("Lifecycle module response must contain a dispatch result.".into());
    };
    for field in [
        "effects",
        "messages",
        "events",
        "blockingErrors",
        "nonBlockingErrors",
    ] {
        if !matches!(result.get(field), Some(JsValue::Array(_))) {
            return Err("Lifecycle module response has an invalid dispatch result.".into());
        }
    }
    if let Some(value) = result.get("pendingAsyncHooks")
        && !value.is_undefined()
        && !matches!(value, JsValue::Array(_))
    {
        return Err("Lifecycle module response has an invalid dispatch result.".into());
    }
    Ok(())
}

/// TS `moduleFailure`: `String(error?.message ?? code ?? "Lifecycle module failed.")`.
pub fn lifecycle_module_failure(value: &JsValue) -> (JsString, Option<JsString>) {
    let message = value
        .get("error")
        .and_then(JsValue::as_object)
        .and_then(|error| error.get("message"))
        .filter(|message| !matches!(message, JsValue::Undefined | JsValue::Null))
        .and_then(|message| super::exec::js_string(message).ok());
    let code = value
        .get("code")
        .filter(|code| !matches!(code, JsValue::Undefined | JsValue::Null));
    let message = message
        .or_else(|| code.and_then(|code| super::exec::js_string(code).ok()))
        .unwrap_or_else(|| "Lifecycle module failed.".into());
    let code = value.get("code").and_then(JsValue::as_string).cloned();
    (message, code)
}

/// `JSON.stringify` of a dispatch result, used by fixtures and diagnostics.
pub fn lifecycle_result_json(result: &LifecycleDispatchResult) -> String {
    stringify_json(&result.to_value())
        .ok()
        .flatten()
        .unwrap_or_default()
}
