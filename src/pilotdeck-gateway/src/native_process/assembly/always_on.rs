//! #2294: the always-on runtime's assembly half. The bundle's
//! `createAlwaysOn` conditions (no `alwaysOn` section, or no project with
//! `enabled: true`) live in `automation.rs`; this file is the factory the
//! bundle calls once they pass — the one `AlwaysOnManager::new` of the
//! native process — plus the production `AlwaysOnAgentGateway` behind
//! `bind_agent_gateway`: the abort face, `NativeAlwaysOnGatewayBody`, which
//! ends in the gateway's synchronous abort prefix exactly like the cron
//! counterpart (`assembly/cron.rs`).
mod standalone;
pub(super) use standalone::standalone_factory;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pilotdeck_protocol::{JsString, JsValue};

use super::automation::{
    AlwaysOnBindHooks, AlwaysOnFactory, AutomationFuture, ProjectAutomationAgentGateway,
    ProjectAutomationProvider,
};
use crate::runtime::{
    always_on::{AlwaysOnAgentGateway, AlwaysOnManager, AlwaysOnManagerOptions, GatewayAbortInput,
        PhaseRequest, PhaseResult, PhaseCompletion, ControlError},
    turn::submit::TurnSubmitter,
    session::SessionOwner,
};

/// The native process's one `AlwaysOnManager::new` (test fixtures construct
/// their own). The bundle only calls this with a live config — every create
/// condition is its (:287-288) decision.
pub(super) fn native_factory() -> AlwaysOnFactory {
    Arc::new(|input| {
        let home = input.pilot_home.clone();
        let manager = AlwaysOnManager::new_with_resolver(AlwaysOnManagerOptions {
            config: input.config,
            pilot_home: input.pilot_home,
            now: None,
            uuid: None,
            logger: None,
        }, |key| bounded_paths(&home, key))?;
        Ok(Arc::new(NativeAlwaysOnProvider {
            manager: Arc::new(manager),
        }))
    })
}

/// Caller runs on an owned blocking worker. Refused markers must not silently
/// fall back to an unrelated home project identity.
pub(super) fn bounded_paths(home: &std::path::Path, project: &str)
    -> std::io::Result<crate::runtime::always_on::AlwaysOnPaths> {
    let metadata = crate::metadata::Metadata {
        home: home.to_path_buf(), cwd: std::path::PathBuf::new(),
        files: super::super::methods::session_dir::BoundedSessionMarkers::new(),
    };
    let result = crate::runtime::always_on::resolve_always_on_paths(home, project, &metadata);
    if metadata.files.was_refused() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "Automation project marker was refused by the native storage boundary."));
    }
    result
}

/// The abort face the always-on control half calls
/// (`AlwaysOnRuntime.ts:387-392`): the gateway's synchronous abort prefix
/// (`start_gateway_abort`, InProcessGateway.ts:2000-2013), whose error is the
/// `Error.message` the TS catch reads.
#[derive(Clone)]
pub(super) struct NativeAlwaysOnGatewayBody {
    owner: SessionOwner,
    submitter: Arc<TurnSubmitter>,
    overrides: Arc<super::super::session_overrides::SessionOverrides>,
    runtime: tokio::runtime::Handle,
    shutdown: crate::runtime::network::Cancellation,
}

impl NativeAlwaysOnGatewayBody {
    pub(super) fn new(
        owner: SessionOwner, submitter: Arc<TurnSubmitter>,
        overrides: Arc<super::super::session_overrides::SessionOverrides>,
        runtime: tokio::runtime::Handle,
        shutdown: crate::runtime::network::Cancellation,
    ) -> Self {
        Self { owner, submitter, overrides, runtime, shutdown }
    }

    async fn execute(
        self, request: PhaseRequest,
        sender: &mut tokio::sync::oneshot::Sender<Result<PhaseResult, ControlError>>,
    ) -> Result<PhaseResult, ControlError> {
        if self.shutdown.is_cancelled() {
            return Err(phase_error("agent_aborted", "Automation stopped before turn admission."));
        }
        let lease = self.overrides.acquire_automation(request.session_key.clone(),
            super::super::session_overrides::AutomationOverrides {
                cwd: request.cwd.clone(), permission_mode: request.permission_mode.clone(),
                bypass_available: request.bypass_available, can_prompt: request.can_prompt,
                exclude_tools: request.exclude_tools.clone(), permission_deny: request.permission_deny.clone(),
                context_registry: request.register_automation_tools.then(|| request.context_registry.clone()),
            }).map_err(|message| phase_error("session_busy", message))?;
        let completion = NativePhaseCompletion {
            state: Some((self.owner.clone(), request.session_key.clone(), lease)),
            runtime: self.runtime.clone(),
        };
        let mut submission = self.submitter.submit_reliable(phase_input(&request));
        let mut events = Vec::new();
        let mut error = None;
        let mut size = 0usize;
        let mut terminal = false;
        let mut stopping = false;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(25));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            // next() owns a consumer completion receiver: retain this exact
            // future across cancellation ticks instead of losing the fence.
            let event = {
                let next = submission.next();
                tokio::pin!(next);
                loop {
                    tokio::select! {
                        biased;
                        _ = sender.closed(), if !stopping => { stopping = true; },
                        _ = self.shutdown.cancelled(), if !stopping => { stopping = true; },
                        _ = interval.tick(), if stopping => {
                            let _ = self.owner.start_gateway_abort(&request.session_key,
                                Some(&request.run_id), Some(&"system:always_on_cancelled".into()));
                        },
                        event = &mut next => break event,
                    }
                    if stopping {
                        error.get_or_insert_with(|| phase_error("agent_aborted", "Automation phase was cancelled."));
                        let _ = self.owner.start_gateway_abort(&request.session_key,
                            Some(&request.run_id), Some(&"system:always_on_cancelled".into()));
                    }
                }
            };
            let event = match event {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(failure) => return Err(phase_error(&failure.code, &failure.message)),
            };
            request.observer.on_event(&request.session_key, &request.channel_key, &request.run_id, &event).await;
            let kind = event.get("type").and_then(JsValue::as_string);
            terminal |= kind.is_some_and(|kind| kind.equals_str("turn_completed"));
            if error.is_none() && kind.is_some_and(|kind| kind.equals_str("error")) {
                let text = |key: &str, fallback: &str| event.get(key).and_then(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units())).filter(|value| !value.is_empty())
                    .unwrap_or_else(|| fallback.to_owned());
                error = Some(phase_error(&text("code", "always_on_run_failed"), &text("message", "Automation phase failed.")));
            }
            let bytes = pilotdeck_protocol::stringify_json(&event).map_err(|failure|
                phase_error("always_on_event_invalid", &failure.to_string()))?.map_or(0, |json| json.len());
            if events.len() < MAX_PHASE_EVENTS && size.checked_add(bytes).is_some_and(|size| size <= MAX_PHASE_EVENT_BYTES) {
                size += bytes;
                events.push(event);
            } else {
                error = Some(phase_error("always_on_event_limit", "Automation phase event budget exceeded."));
                stopping = true;
                let _ = self.owner.start_gateway_abort(&request.session_key,
                    Some(&request.run_id), Some(&"system:always_on_event_limit".into()));
            }
        }
        if !terminal && error.is_none() {
            return Err(phase_error("always_on_incomplete", "Automation phase ended without a terminal event."));
        }
        Ok(PhaseResult::new(events, error).with_completion(Box::new(completion)))
    }
}

const MAX_PHASE_EVENTS: usize = 16_384;
const MAX_PHASE_EVENT_BYTES: usize = 16 * 1024 * 1024;
fn phase_error(code: &str, message: &str) -> ControlError {
    ControlError { code: code.into(), message: message.into() }
}
fn phase_input(request: &PhaseRequest) -> JsValue {
    let prefix: Vec<u16> = "always-on/".encode_utf16().collect();
    let phase = request.channel_key.units().strip_prefix(prefix.as_slice())
        .map(|units| JsValue::String(JsString::from_utf16(units.to_vec())))
        .unwrap_or(JsValue::Undefined);
    crate::metadata::object([
        ("sessionKey", JsValue::String(request.session_key.clone())),
        ("projectKey", JsValue::String(request.project_key.clone())),
        ("channelKey", JsValue::String(request.channel_key.clone())),
        ("message", JsValue::String(request.message.clone())),
        ("runId", JsValue::String(request.run_id.clone())),
        ("mode", JsValue::String(request.permission_mode.clone())),
        ("telemetry", crate::metadata::object([
            ("ownerModule", "always_on".into()), ("executionKind", "always_on".into()), ("phase", phase),
        ])),
    ])
}
struct NativePhaseCompletion {
    state: Option<(SessionOwner, JsString, super::super::session_overrides::AutomationOverrideLease)>,
    runtime: tokio::runtime::Handle,
}
impl NativePhaseCompletion {
    fn schedule(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let (owner, key, lease) = self.state.take()?;
        Some(self.runtime.spawn(async move {
            // Keep the config lease until the original owner has released its
            // recorder, tools and generation, including cancelled callers.
            let _lease = lease;
            if let Err(error) = owner.close(&key).await {
                eprintln!("[always-on] phase session close failed: {error}");
            }
        }))
    }
}
impl Drop for NativePhaseCompletion {
    fn drop(&mut self) { if let Some(task) = self.schedule() { drop(task); } }
}
impl PhaseCompletion for NativePhaseCompletion {
    fn close(mut self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let task = self.schedule();
        Box::pin(async move { if let Some(task) = task { let _ = task.await; } })
    }
}
impl AlwaysOnAgentGateway for NativeAlwaysOnGatewayBody {
    fn run_phase<'a>(&'a self, request: PhaseRequest)
        -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
        let body = self.clone();
        let (mut sender, receiver) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let result = body.execute(request, &mut sender).await;
            // A dropped receiver drops the receipt and starts owned cleanup.
            let _ = sender.send(result);
        });
        Box::pin(async move { receiver.await.map_err(|_| phase_error(
            "always_on_task_failed", "Automation phase worker stopped."))? })
    }

    fn abort_turn<'a>(
        &'a self,
        input: GatewayAbortInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
        // `start_gateway_abort` is synchronous end to end (the connection
        // selects its consumer fence at the Router await boundary), so the
        // prefix completes before the future is even polled.
        let run_id = input.run_id.map(JsString::from);
        let reason = input.reason.map(JsString::from);
        let outcome = self
            .owner
            .start_gateway_abort(
                &JsString::from(input.session_key),
                run_id.as_ref(),
                reason.as_ref(),
            )
            .map(|_| ())
            .map_err(|error| JsString::from(error.message));
        Box::pin(async move { outcome })
    }
}

/// The native always-on provider (`ProjectAutomationAlwaysOnProvider`): the
/// manager behind the lifecycle surface the bundle drives. In TS the
/// provider **is** the control (`ProjectAutomationAlwaysOnProvider extends
/// AlwaysOnControlPort`); the port splits the two, and
/// [`ProjectAutomationProvider::always_on_manager`] is where that identity
/// lands for the composition faces.
pub(super) struct NativeAlwaysOnProvider {
    manager: Arc<AlwaysOnManager>,
}

impl ProjectAutomationProvider for NativeAlwaysOnProvider {
    fn bind_agent_gateway(
        &self,
        gateway: &ProjectAutomationAgentGateway,
        hooks: Option<AlwaysOnBindHooks<'_>>,
    ) -> Result<(), std::io::Error> {
        // The always-on bind signature always carries the hooks record; the
        // busy member may be absent (ProjectAutomationBundle.ts:318-326).
        self.manager.bind_agent_gateway(
            gateway.always_on.clone(),
            hooks.and_then(|hooks| hooks.is_project_busy).cloned(),
        )
    }

    fn start(self: Arc<Self>) -> AutomationFuture {
        let manager = self.manager.clone();
        Box::pin(async move { manager.start().await.map_err(|error| error.to_string()) })
    }

    fn stop(self: Arc<Self>) -> AutomationFuture {
        let manager = self.manager.clone();
        Box::pin(async move { manager.stop().await.map_err(|error| error.to_string()) })
    }

    /// TS composes the manager's tool definitions into `extraTools`
    /// (`getTools`, AlwaysOnManager.ts:137-139). The port's tool definitions
    /// are the fire pipeline's deep half, which stays behind the `fire.rs`
    /// seam, so the publish composes none — recorded in the -R1 ledger.
    fn tools(&self) -> Vec<JsString> {
        Vec::new()
    }

    fn always_on_manager(&self) -> Option<Arc<AlwaysOnManager>> {
        Some(self.manager.clone())
    }
}
