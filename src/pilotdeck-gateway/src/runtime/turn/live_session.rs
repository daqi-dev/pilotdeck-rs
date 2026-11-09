//! AgentSession's outer generator. Turn completion is a yield, not disposal.
use super::live::{ActiveTurn, LiveTurnStream, SessionState};
use crate::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::lifecycle::{Binding, LifecyclePort},
    },
    runtime::{
        hooks::{Cancellation, LifecycleDispatchInput},
        plugins::PluginSessionContributionLease,
        project::{
            ProjectResources, Resource, RuntimeError, RuntimeLease,
            interaction_hooks::InteractionHooks,
        },
        session::{LiveTurn, SessionNext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, watch};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SessionStatus {
    #[default]
    Idle,
    Running,
    Aborted,
    Failed,
}
impl SessionStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
        }
    }
}

pub(super) struct SessionLifecycle {
    pub(super) port: Arc<dyn LifecyclePort>,
    pub(super) cwd: JsString,
    pub(super) transcript_path: JsString,
    owned: Option<SessionOwnership>,
    executor: tokio::runtime::Handle,
    sdk: Option<SdkSessionHooks>,
}
struct SdkSessionHooks {
    handlers: Arc<Mutex<crate::runtime::sdk_session::SessionHandlerRegistry>>,
    registration: Option<crate::runtime::sdk_session::HandlerRegistration>,
    session_registration: Option<crate::runtime::sdk_session::HandlerRegistration>,
    signal: Cancellation,
    file: Option<Arc<crate::runtime::hooks::LifecycleRuntime>>,
    config: Option<Arc<crate::runtime::hooks::LifecycleRuntime>>,
}
impl SdkSessionHooks {
    fn revoke(&mut self) {
        if let Some(token) = self.registration.take() {
            self.handlers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .unregister_config_change(&token);
        }
        if let Some(token) = self.session_registration.take() {
            self.handlers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .unregister_session_hook(&token);
        }
    }
}
impl Drop for SdkSessionHooks {
    fn drop(&mut self) {
        self.revoke();
        self.signal.abort();
    }
}

/// Poll once before returning so the HookRuntime owns the in-flight receipt
/// before a concurrent session close can dispose it.
fn admit_sdk_observer(
    mut work: HostFuture<crate::runtime::hooks::LifecycleDispatchResult>,
    permit: tokio::sync::OwnedSemaphorePermit,
    executor: &tokio::runtime::Handle,
) {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match work.as_mut().poll(&mut context) {
        std::task::Poll::Ready(_) => drop(permit),
        std::task::Poll::Pending => {
            executor.spawn(async move {
                let _permit = permit;
                let _ = work.await;
            });
        }
    }
}
struct SessionOwnership {
    lease: Arc<RuntimeLease>,
    contribution: Option<Arc<PluginSessionContributionLease>>,
    turn: Option<Arc<super::interaction::InteractionTurn>>,
}
impl SessionLifecycle {
    pub(super) fn bind(
        lease: Arc<RuntimeLease>,
        identity: &ExecutionIdentity,
        input: &mut super::interaction::InteractionTurnInput,
        signal: Cancellation,
        cwd: JsString,
        transcript_path: JsString,
    ) -> Result<Option<Self>, HostError> {
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| ModuleError::new("Leased project resources are missing."))?
            .context();
        let Some(resource) = context.resource(Resource::InteractionHooks) else {
            return Ok(None);
        };
        let hooks = resource
            .as_any()
            .downcast_ref::<InteractionHooks>()
            .ok_or_else(|| ModuleError::new("Invalid leased interaction hooks resource."))?;
        let turn_signal = signal.clone();
        let port = hooks.bind_session_with_options(
            Binding {
                session_id: identity.session_id.clone(),
                turn_id: identity.turn_id.clone(),
                cwd: cwd.clone(),
                permission_mode: None,
                base_env: input.base_env.clone(),
                signal: Some(signal),
            },
            identity.run_id.clone(),
            input.hooks.clone(),
            input
                .session_hook_options
                .clone()
                .or_else(|| input.hook_options.clone())
                .unwrap_or_default(),
        )?;
        let port: Arc<dyn LifecyclePort> = Arc::new(port);
        let sdk = input
            .sdk_hook_state
            .as_ref()
            .map(|sdk| {
                let signal = Cancellation::new();
                let overflowed = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let report_limit = {
                    let overflowed = overflowed.clone();
                    let abort = turn_signal.clone();
                    let emit = input.permission.emit.clone();
                    Arc::new(move || {
                        use std::sync::atomic::Ordering;
                        if !overflowed.swap(true, Ordering::SeqCst) {
                            let mut error = JsObject::new();
                            error.insert("type".into(), "error".into());
                            error.insert("code".into(), "SDK_HOOK_DISPATCH_LIMIT".into());
                            error.insert(
                                "message".into(),
                                "SDK hook dispatch exceeded its native concurrency budget.".into(),
                            );
                            error.insert("recoverable".into(), JsValue::Bool(false));
                            emit(JsValue::Object(error));
                            abort.abort();
                        }
                    })
                };
                let settings = |name: &str| {
                    sdk.settings
                        .iter()
                        .filter(|(event, _)| event.equals_str(name))
                        .cloned()
                        .collect::<crate::hooks::protocol::HooksSettings>()
                };
                let file_settings = settings("FileChanged");
                let file = (!file_settings.is_empty()).then(|| {
                    Arc::new(crate::runtime::hooks::LifecycleRuntime::new(
                        crate::runtime::hooks::HookRuntime::with_options(
                            file_settings,
                            input
                                .session_hook_options
                                .clone()
                                .or_else(|| input.hook_options.clone())
                                .unwrap_or_default(),
                        ),
                    ))
                });
                if file.is_some() && input.sdk_file_slot.is_none() {
                    return Err(ModuleError::new("SDK FileChanged slot is missing.").into());
                }
                if let (Some(slot), Some(runtime)) = (&input.sdk_file_slot, &file) {
                    slot.set((runtime.clone(), signal.clone())).map_err(|_| {
                        ModuleError::new("SDK FileChanged owner was already bound.")
                    })?;
                }
                let config_settings = settings("ConfigChange");
                let config = (!config_settings.is_empty()).then(|| {
                    let mut options = input
                        .session_hook_options
                        .clone()
                        .or_else(|| input.hook_options.clone())
                        .unwrap_or_default();
                    // Fixed TS SessionInteractionBundle's ConfigChange owner has
                    // a local pending registry, unlike main/FileChanged hooks.
                    // An async receipt must not acquire gateway turn authority.
                    options.on_async_registered = None;
                    Arc::new(crate::runtime::hooks::LifecycleRuntime::new(
                        crate::runtime::hooks::HookRuntime::with_options(config_settings, options),
                    ))
                });
                // Observer backpressure belongs to this session. Another
                // session's slow HTTP endpoint must not consume its allowance.
                let observer_slots = Arc::new(tokio::sync::Semaphore::new(64));
                let registration = config.as_ref().map(|runtime| {
                    let runtime = Arc::downgrade(runtime);
                    let report_limit = report_limit.clone();
                    let cancel = signal.clone();
                    let session = identity.session_id.clone();
                    let cwd = cwd.clone();
                    let executor = tokio::runtime::Handle::current();
                    let slots = observer_slots.clone();
                    let handler: crate::runtime::sdk_session::SdkConfigChangeHandler =
                        Arc::new(move |notice| {
                            let Some(runtime) = runtime.upgrade() else {
                                return;
                            };
                            let Ok(permit) = slots.clone().try_acquire_owned() else {
                                report_limit();
                                return;
                            };
                            let session = session.clone();
                            let cwd = cwd.clone();
                            let cancel = cancel.clone();
                            let notice = notice.clone();
                            let mut base = JsObject::new();
                            base.insert("sessionId".into(), JsValue::String(session));
                            base.insert("transcriptPath".into(), "".into());
                            base.insert("cwd".into(), JsValue::String(cwd.clone()));
                            let mut payload = JsObject::new();
                            payload.insert(
                                "changedPaths".into(),
                                JsValue::Array(
                                    notice
                                        .changed_paths
                                        .into_iter()
                                        .map(JsValue::String)
                                        .collect(),
                                ),
                            );
                            payload.insert(
                                "changeClasses".into(),
                                JsValue::Array(
                                    notice
                                        .change_classes
                                        .into_iter()
                                        .map(JsValue::String)
                                        .collect(),
                                ),
                            );
                            let work: HostFuture<_> = Box::pin(async move {
                                runtime
                                    .dispatch(LifecycleDispatchInput {
                                        event: "ConfigChange".into(),
                                        base_input: base,
                                        payload: Some(payload),
                                        match_query: Some("ConfigChange".into()),
                                        cwd,
                                        env: None,
                                        signal: Some(cancel),
                                    })
                                    .await
                            });
                            admit_sdk_observer(work, permit, &executor);
                        });
                    sdk.handlers
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .register_config_change(&identity.session_id, handler)
                });
                let session_port = Arc::downgrade(&port);
                let report_limit = report_limit.clone();
                let session_cancel = signal.clone();
                let session_id = identity.session_id.clone();
                let session_cwd = cwd.clone();
                let executor = tokio::runtime::Handle::current();
                let slots = observer_slots;
                let session_handler: crate::runtime::sdk_session::SessionHookHandler =
                    Arc::new(move |event, payload| {
                        let Some(port) = session_port.upgrade() else {
                            return;
                        };
                        let Ok(permit) = slots.clone().try_acquire_owned() else {
                            report_limit();
                            return;
                        };
                        let session = session_id.clone();
                        let cwd = session_cwd.clone();
                        let cancel = session_cancel.clone();
                        let payload = payload.as_object().cloned().unwrap_or_default();
                        let mut base = JsObject::new();
                        base.insert("sessionId".into(), JsValue::String(session));
                        base.insert("transcriptPath".into(), "".into());
                        base.insert("cwd".into(), JsValue::String(cwd.clone()));
                        let name: JsString = event.as_str().into();
                        let work = port.dispatch(LifecycleDispatchInput {
                            event: name.clone(),
                            base_input: base,
                            payload: Some(payload),
                            match_query: Some(name),
                            cwd,
                            env: None,
                            signal: Some(cancel),
                        });
                        admit_sdk_observer(work, permit, &executor);
                    });
                let session_registration = sdk
                    .handlers
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .register_session_hook(&identity.session_id, session_handler);
                Ok::<SdkSessionHooks, HostError>(SdkSessionHooks {
                    handlers: sdk.handlers.clone(),
                    registration,
                    session_registration: Some(session_registration),
                    signal,
                    file,
                    config,
                })
            })
            .transpose()?;
        Ok(Some(Self {
            sdk,
            port,
            cwd,
            transcript_path,
            owned: Some(SessionOwnership {
                lease,
                contribution: None,
                turn: None,
            }),
            executor: tokio::runtime::Handle::current(),
        }))
    }
    #[cfg(test)]
    fn test(port: Arc<dyn LifecyclePort>) -> Self {
        Self {
            sdk: None,
            port,
            cwd: "/virtual/743".into(),
            transcript_path: "/virtual/743/session.jsonl".into(),
            owned: None,
            executor: tokio::runtime::Handle::current(),
        }
    }
    pub(super) fn retain_turn(&mut self, turn: Arc<super::interaction::InteractionTurn>) {
        if let Some(owned) = &mut self.owned {
            turn.adopt_session_finish();
            owned.contribution = turn.contribution_hold();
            owned.turn = Some(turn);
        }
    }
    pub(super) fn steer(&self) -> Option<crate::runtime::project::interaction_steer::TurnSteer> {
        self.owned
            .as_ref()
            .and_then(|owned| owned.turn.as_ref())
            .and_then(|turn| turn.ports().steer)
    }
    fn finish_steer(&self) {
        if let Some(steer) = self
            .owned
            .as_ref()
            .and_then(|owned| owned.turn.as_ref())
            .and_then(|turn| turn.ports().steer)
        {
            steer.finish_session();
        }
    }
    fn dispatch(
        &self,
        event: &str,
        session: &JsString,
        signal: Cancellation,
        payload: JsObject,
    ) -> HostFuture<()> {
        let mut base_input = JsObject::new();
        base_input.insert("sessionId".into(), JsValue::String(session.clone()));
        base_input.insert(
            "transcriptPath".into(),
            JsValue::String(self.transcript_path.clone()),
        );
        base_input.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        let future = self.port.dispatch(LifecycleDispatchInput {
            event: event.into(),
            base_input,
            payload: Some(payload),
            match_query: Some(event.into()),
            cwd: self.cwd.clone(),
            env: None,
            signal: Some(signal),
        });
        // AgentSession ignores effects/errors returned by the hook executor.
        // A dispatch rejection is a different boundary and propagates.
        // #996: ordinary-hook-error-rejects-session turns a hook executor
        // error into a session rejection (live-session/schemata.py). Off —
        // the ignored result.
        Box::pin(async move {
            if crate::mutants::on("ordinary-hook-error-rejects-session") {
                future.await.and_then(|result| {
                    if result.non_blocking_errors.is_empty() && result.blocking_errors.is_empty() {
                        Ok(())
                    } else {
                        Err(ModuleError::new("mutated hook error").into())
                    }
                })
            } else {
                future.await.map(|_| ())
            }
        })
    }
    fn close(mut self) -> HostFuture<()> {
        let mut done = self.spawn_close();
        Box::pin(async move {
            loop {
                if let Some(result) = done.borrow_and_update().clone() {
                    return result;
                }
                done.changed()
                    .await
                    .map_err(|_| ModuleError::new("Session cleanup stopped before completion."))?;
            }
        })
    }
    fn spawn_close(&mut self) -> watch::Receiver<Option<Result<(), HostError>>> {
        let owned = self.owned.take();
        let mut sdk = self.sdk.take();
        if let Some(sdk) = &mut sdk {
            sdk.revoke();
        }
        let port = self.port.clone();
        let (done, completion) = watch::channel(None);
        let cleanup = async move {
            let mut errors = Vec::new();
            if let Some(sdk) = &sdk {
                for runtime in [sdk.file.as_ref(), sdk.config.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Err(error) = runtime.dispose_checked().await {
                        errors.push(error.to_string());
                    }
                }
            }
            if let Err(e) = port.dispose().await {
                // #996: disposer-error-value-lost replaces the disposer error
                // (live-session/schemata.py). Off — the original error.
                errors.push(if crate::mutants::on("disposer-error-value-lost") {
                    format!("lost disposer error: {e}")
                } else {
                    e.to_string()
                });
            }
            if let Some(SessionOwnership {
                lease,
                contribution,
                turn,
            }) = owned
            {
                // The plugin contribution backs both runtimes. A timed-out
                // HostClient disposer cannot release it ahead of turn drains.
                if let Some(turn) = turn {
                    if let Err(e) = turn.wait_closed().await {
                        errors.push(e.to_string());
                    }
                    if let Some(steer) = turn.ports().steer {
                        steer.finish_session();
                    }
                }
                if let Some(contribution) = contribution
                    && let Ok(mut contribution) = Arc::try_unwrap(contribution)
                    && let Err(e) = contribution.release().await
                {
                    errors.push(e.to_string());
                }
                if let Ok(mut lease) = Arc::try_unwrap(lease)
                    && let Err(e) = lease.release().await
                {
                    errors.push(e.to_string());
                }
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(ModuleError::new(errors.join("; ")).into())
            }
        };
        // Keep the same guarded completion contract as the turn-owned cleanup:
        // a stopped executor can discard this future without ever polling it.
        // #996: shutdown-bare-spawn replaces the guarded completion with a bare
        // spawn, and drop-needs-current-runtime drops the lifecycle's runtime
        // for the current one (live-session/schemata.py). Off — the original
        // guarded completion on the captured executor.
        if crate::mutants::on("shutdown-bare-spawn") {
            self.executor
                .spawn(async move { done.send_replace(Some(cleanup.await)) });
        } else {
            // `let` binding: the current runtime handle is a temporary that
            // must outlive the spawn call, and it is only taken when the
            // switch is on — `Handle::current()` panics off-runtime.
            let current = crate::mutants::on("drop-needs-current-runtime")
                .then(tokio::runtime::Handle::current);
            crate::runtime::cleanup_task::spawn_after(
                current.as_ref().unwrap_or(&self.executor),
                done,
                || {},
                cleanup,
            );
        }
        completion
    }
}
impl Drop for SessionLifecycle {
    fn drop(&mut self) {
        if self.owned.is_some() {
            self.spawn_close();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Started,
    Prepare,
    Start,
    Setup,
    Turn,
    End,
    Ended,
    Done,
}

pub(super) struct SessionStream {
    turn: Box<dyn LiveTurn>,
    // The production stream also owns the lazy host preparation. Tests supply
    // a prepared runner to exercise the same shell at deterministic yields.
    preparing: Option<(LiveTurnStream, oneshot::Receiver<Option<SessionLifecycle>>)>,
    lifecycle: Option<SessionLifecycle>,
    pending: Option<HostFuture<()>>,
    session: JsString,
    state: Arc<Mutex<SessionState>>,
    active: Option<Arc<ActiveTurn>>,
    phase: Phase,
    result: Option<JsValue>,
    end_reason: &'static str,
}
struct EmptyTurn;
impl LiveTurn for EmptyTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async { Ok(None) })
    }
}
impl SessionStream {
    pub(super) fn new(
        turn: LiveTurnStream,
        ready: oneshot::Receiver<Option<SessionLifecycle>>,
        session: JsString,
        state: Arc<Mutex<SessionState>>,
        active: Arc<ActiveTurn>,
    ) -> Self {
        Self {
            turn: Box::new(EmptyTurn),
            preparing: Some((turn, ready)),
            lifecycle: None,
            pending: None,
            session,
            state,
            active: Some(active),
            phase: Phase::Started,
            result: None,
            end_reason: "prompt_input_exit",
        }
    }
    fn event(&self, kind: &str) -> JsValue {
        let mut event = JsObject::new();
        event.insert("type".into(), kind.into());
        event.insert("sessionId".into(), JsValue::String(self.session.clone()));
        if kind == "session_ended" {
            event.insert("reason".into(), self.end_reason.into());
        }
        JsValue::Object(event)
    }
    async fn hook(&mut self, event: &str, fields: &[(&str, &str)]) -> Result<(), RuntimeError> {
        if self.pending.is_none()
            && let Some(lifecycle) = &self.lifecycle
        {
            let mut payload = JsObject::new();
            for (key, value) in fields {
                payload.insert((*key).into(), (*value).into());
            }
            self.pending = Some(lifecycle.dispatch(
                event,
                &self.session,
                self.state.lock().unwrap().signal(),
                payload,
            ));
        }
        if let Some(pending) = &mut self.pending {
            let result = pending.await.map_err(|e| RuntimeError::new(e.message));
            self.pending = None;
            result?;
        }
        Ok(())
    }
    fn fail(&mut self, error: RuntimeError, in_turn: bool) -> RuntimeError {
        if in_turn {
            // #996: runner-error-preserves-running keeps the Running status
            // (live-session/schemata.py). Off — the original fail().
            if !crate::mutants::on("runner-error-preserves-running") {
                self.state.lock().unwrap().fail();
            }
        }
        self.phase = Phase::Done;
        self.turn = Box::new(EmptyTurn);
        self.preparing = None;
        self.lifecycle.take();
        self.active.take();
        error
    }
}
impl LiveTurn for SessionStream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            loop {
                match self.phase {
                    Phase::Started => {
                        self.phase = Phase::Prepare;
                        return Ok(Some(self.event("session_started")));
                    }
                    Phase::Prepare => {
                        if let Some((turn, ready)) = &mut self.preparing {
                            self.lifecycle = match turn.prepare_session(ready).await {
                                Ok(lifecycle) => lifecycle,
                                Err(error) => return Err(self.fail(error, false)),
                            };
                            self.turn = Box::new(self.preparing.take().expect("prepared turn").0);
                        }
                        self.phase = Phase::Start;
                    }
                    Phase::Start => {
                        // #996: skip-start misses the Start hook and
                        // start-source reports a resumed start
                        // (live-session/schemata.py). Off — the original
                        // startup hook.
                        if let Err(error) = self
                            .hook(
                                if crate::mutants::on("skip-start") {
                                    "MissingStart"
                                } else {
                                    "SessionStart"
                                },
                                &[(
                                    "source",
                                    if crate::mutants::on("start-source") {
                                        "resume"
                                    } else {
                                        "startup"
                                    },
                                )],
                            )
                            .await
                        {
                            return Err(self.fail(error, false));
                        }
                        self.phase = Phase::Setup;
                    }
                    Phase::Setup => {
                        // #996: skip-setup misses the Setup hook
                        // (live-session/schemata.py). Off — the original hook.
                        if let Err(error) = self
                            .hook(
                                if crate::mutants::on("skip-setup") {
                                    "MissingSetup"
                                } else {
                                    "Setup"
                                },
                                &[],
                            )
                            .await
                        {
                            return Err(self.fail(error, false));
                        }
                        self.phase = Phase::Turn;
                        return Ok(Some(self.event("setup_completed")));
                    }
                    Phase::Turn => match self.turn.next().await {
                        Ok(Some(event)) => {
                            if event.get("type") == Some(&"turn_completed".into()) {
                                self.result = event.get("result").cloned();
                            }
                            return Ok(Some(event));
                        }
                        Err(error) => return Err(self.fail(error, true)),
                        Ok(None) => {
                            let Some(result) = self.result.take() else {
                                return Err(self.fail(
                                    RuntimeError::new("Agent turn ended without a result."),
                                    true,
                                ));
                            };
                            self.end_reason = self.state.lock().unwrap().complete(&result);
                            if let Some(lifecycle) = &self.lifecycle {
                                // #996: skip-finish-before-end skips the steer
                                // finish (live-session/schemata.py). Off — the
                                // original finish.
                                if !crate::mutants::on("skip-finish-before-end") {
                                    lifecycle.finish_steer();
                                }
                            }
                            self.phase = Phase::End;
                        }
                    },
                    Phase::End => {
                        // #996: skip-end misses the End hook
                        // (live-session/schemata.py). Off — the original hook.
                        if let Err(error) = self
                            .hook(
                                if crate::mutants::on("skip-end") {
                                    "MissingEnd"
                                } else {
                                    "SessionEnd"
                                },
                                &[("reason", self.end_reason)],
                            )
                            .await
                        {
                            return Err(self.fail(error, false));
                        }
                        // #996: recompute-end-reason recomputes the reason from
                        // the final status (live-session/schemata.py). Off —
                        // the reason recorded at completion.
                        if crate::mutants::on("recompute-end-reason") {
                            self.end_reason =
                                if self.state.lock().unwrap().status == SessionStatus::Aborted {
                                    "other"
                                } else {
                                    "prompt_input_exit"
                                };
                        }
                        self.phase = Phase::Ended;
                        // #996: session-ended-event misses the session_ended
                        // event (live-session/schemata.py). Off — the original
                        // event.
                        return Ok(Some(self.event(
                            if crate::mutants::on("session-ended-event") {
                                "missing_session_ended"
                            } else {
                                "session_ended"
                            },
                        )));
                    }
                    Phase::Ended => {
                        if self.pending.is_none()
                            && let Some(lifecycle) = self.lifecycle.take()
                        {
                            self.pending = Some(lifecycle.close());
                        }
                        if let Some(pending) = &mut self.pending {
                            let result = pending.await.map_err(|e| RuntimeError::new(e.message));
                            self.pending = None;
                            if let Err(error) = result {
                                return Err(self.fail(error, false));
                            }
                        }
                        self.active.take();
                        self.phase = Phase::Done;
                    }
                    Phase::Done => return Ok(None),
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "live_session_tests.rs"]
mod tests;
