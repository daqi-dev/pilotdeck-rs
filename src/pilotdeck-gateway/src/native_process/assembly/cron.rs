//! A8 U8 (#1326): the cron runtime's assembly half. `build` turns the
//! normalized `cron` config section into the one `CronManager` the native
//! process owns; `NativeCronGatewayBody` is the production
//! `CronGatewayBody` behind `bind_agent_gateway` (the scheduler that drives
//! it is runtime/cron/scheduler.rs, #2295 — no `unimplemented!()` seam,
//! every method is real).
use std::path::Path;
use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::runtime::{
    cron::{
        CronError, CronGatewayBody, CronManager, CronManagerConfig, CronManagerOptions,
        CronTurnSubmit,
    },
    session::SessionOwner,
    turn::submit::{SubmitError, TurnSubmission, TurnSubmitter},
};

/// The `cron:` section of the normalized config snapshot
/// (config/snapshot.rs `cron()`, the port of `parseCronConfig`):
/// `Some` only when the section exists — the exact condition TS creates a
/// `CronManager` under (`ProjectAutomationBundle.createCron`). An existing
/// section is already normalized (defaults filled, invalid values fallen
/// back), so this reads the four fields rather than re-validating
/// (`sections.rs` did the diagnostics on load).
fn manager_config(section: &JsValue) -> CronManagerConfig {
    CronManagerConfig {
        enabled: section
            .get("enabled")
            .and_then(JsValue::as_bool)
            .unwrap_or(true),
        timezone: section
            .get("timezone")
            .and_then(JsValue::as_string)
            .and_then(|zone| zone.to_utf8().ok())
            .unwrap_or_else(|| "UTC".into()),
        max_concurrent_runs: section
            .get("maxConcurrentRuns")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0),
        run_timeout_minutes: section
            .get("runTimeoutMinutes")
            .and_then(JsValue::as_number)
            .unwrap_or(60.0),
    }
}

pub(super) fn cron_io_error(error: CronError) -> std::io::Error {
    std::io::Error::other(String::from_utf16_lossy(error.message().units()))
}

/// The native process's one production `CronManager::new` (test fixtures
/// construct their own): `None` keeps the unconfigured shape — the six names
/// stay out of capabilities and every method answers the `requireCron` gate.
pub(super) fn build(
    section: Option<&JsValue>,
    pilot_home: &Path,
) -> Result<Option<Arc<CronManager>>, std::io::Error> {
    let Some(section) = section else {
        return Ok(None);
    };
    let manager = CronManager::new(CronManagerOptions {
        config: manager_config(section),
        pilot_home: pilot_home.to_string_lossy().into_owned(),
        // The native assembly owns no shared session-override registry to
        // hand over (the SDK session registry MCP shares is a different
        // mechanism), so the option stays `None` and #1325's runtime keeps
        // its per-runtime default — never a second hand-rolled registry.
        session_overrides: None,
        now: None,
        uuid: None,
        logger: None,
        scheduler_idle_poll_ms: None,
    })
    .map_err(cron_io_error)?;
    Ok(Some(manager))
}

pub(super) fn bind_callbacks(
    manager: &CronManager,
    server: crate::channel::server::ChannelServer,
    runtime: tokio::runtime::Handle,
    shutdown: crate::runtime::network::Cancellation,
) -> Result<(), CronError> {
    let callbacks = crate::runtime::cron::CronExecutionCallbacks {
        assistant_text_limit_units: Some(
            crate::runtime::cron::NATIVE_CRON_MAX_ASSISTANT_TEXT_UNITS,
        ),
        on_result_delivery: Some(Arc::new(move |value| {
            if tokio::runtime::Handle::try_current().is_ok() {
                return Err(CronError::error(
                    "Cron result delivery requires its scheduler worker thread.",
                ));
            }
            let required = |key: &str| {
                value
                    .get(key)
                    .and_then(JsValue::as_string)
                    .cloned()
                    .ok_or_else(|| CronError::error(format!("Cron delivery missing {key}.")))
            };
            let delivery = crate::channel::im_cron::CronResultDelivery {
                task_id: required("taskId")?,
                run_id: required("runId")?,
                session_key: required("sessionKey")?,
                channel_key: crate::channel::protocol::ChannelKey::new(required("channelKey")?),
                origin_session_key: value
                    .get("originSessionKey")
                    .and_then(JsValue::as_string)
                    .cloned(),
                origin_channel_key: value
                    .get("originChannelKey")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .map(crate::channel::protocol::ChannelKey::new),
                text: required("text")?,
            };
            let task = String::from_utf16_lossy(delivery.task_id.units());
            let run = String::from_utf16_lossy(delivery.run_id.units());
            // CronFire invokes this synchronous callback on its owned OS
            // worker. Keep delivery inside that worker's lifetime so native
            // stop_and_drain cannot report success while delivery is pending.
            match complete_delivery(
                &runtime,
                Some(&shutdown),
                std::time::Duration::from_secs(30),
                server.deliver_cron_result(delivery),
            ) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    eprintln!("[cron] result delivery was not handled task={task} run={run}");
                    Ok(())
                }
                Err(error) => Err(CronError::error(format!(
                    "Native cron result delivery failed task={task} run={run}: {error}"
                ))),
            }
        })),
        ..Default::default()
    };
    manager.set_execution_callbacks(callbacks)
}

fn complete_delivery(
    runtime: &tokio::runtime::Handle,
    shutdown: Option<&crate::runtime::network::Cancellation>,
    budget: std::time::Duration,
    delivery: impl std::future::Future<Output = bool>,
) -> Result<bool, &'static str> {
    runtime.block_on(async {
        let stopping = async {
            match shutdown {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = stopping => Err("delivery cancelled during gateway shutdown"),
            result = tokio::time::timeout(budget, delivery) => result.map_err(|_| "delivery timed out"),
        }
    })
}

#[cfg(test)]
mod delivery_completion_tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn production_binding_keeps_scheduler_delivery_owned_until_shutdown() {
        use crate::channel::{im_cron::CronResultDelivery, protocol::*, server::ChannelServer};
        use crate::runtime::cron::CronAgentGateway;
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        };
        struct Handle;
        impl ChannelHandle for Handle {
            fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
                Box::pin(async { Ok(()) })
            }
        }
        struct Adapter {
            key: ChannelKey,
            receipt: Mutex<Option<tokio::sync::oneshot::Sender<CronResultDelivery>>>,
            dropped: Arc<AtomicBool>,
        }
        impl ChannelAdapter for Adapter {
            fn channel_key(&self) -> &ChannelKey {
                &self.key
            }
            fn start(
                &self,
                _: ChannelStartDeps,
            ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
                Box::pin(async { Ok(Arc::new(Handle) as Arc<dyn ChannelHandle>) })
            }
            fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
                let receipt = self.receipt.lock().unwrap().take().unwrap();
                let delivery = delivery.clone();
                struct Dropped(Arc<AtomicBool>);
                impl Drop for Dropped {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::SeqCst);
                    }
                }
                let dropped = Dropped(self.dropped.clone());
                Box::pin(async move {
                    let _dropped = dropped;
                    receipt.send(delivery).unwrap();
                    std::future::pending().await
                })
            }
        }
        struct Gateway;
        impl CronAgentGateway for Gateway {
            fn submit_turn(&self, _: &CronTurnSubmit) -> Result<(), CronError> {
                panic!("observed turn required")
            }
            fn submit_turn_observed(
                &self,
                _: &CronTurnSubmit,
                observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
            ) -> Result<(), CronError> {
                observe(&crate::metadata::object([
                    ("type", "assistant_text_delta".into()),
                    ("text", "production delivery".into()),
                ]))
            }
            fn abort_turn(
                &self,
                _: &JsString,
                _: Option<&JsString>,
                _: Option<&JsString>,
            ) -> Result<(), CronError> {
                Ok(())
            }
            fn close_session(&self, _: &JsString, _: Option<&JsString>) -> Result<(), CronError> {
                Ok(())
            }
        }
        let scratch = crate::test_scratch::Scratch::new("native-cron-delivery-binding");
        let home = scratch.path().join("home");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let manager = Arc::new(
            CronManager::new(CronManagerOptions {
                config: CronManagerConfig {
                    enabled: true,
                    timezone: "UTC".into(),
                    max_concurrent_runs: 1.0,
                    run_timeout_minutes: 1.0,
                },
                pilot_home: home.to_string_lossy().into_owned(),
                session_overrides: None,
                now: Some(Arc::new(|| 1_767_225_600_000.0)),
                uuid: None,
                logger: None,
                scheduler_idle_poll_ms: Some(60_000),
            })
            .unwrap(),
        );
        let server =
            ChannelServer::new(Default::default(), Arc::new(|| Box::pin(async { Ok(()) })));
        let (receipt, ready) = tokio::sync::oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        server
            .hot_start_channel(Arc::new(Adapter {
                key: ChannelKey::new("fixture"),
                receipt: Mutex::new(Some(receipt)),
                dropped: dropped.clone(),
            }))
            .await
            .unwrap();
        let shutdown = crate::runtime::network::Cancellation::new();
        bind_callbacks(
            &manager,
            server.clone(),
            tokio::runtime::Handle::current(),
            shutdown.clone(),
        )
        .unwrap();
        manager.bind_agent_gateway(Arc::new(Gateway)).unwrap();
        let project_value = JsValue::from(project.to_string_lossy().as_ref());
        let created = manager
            .create_task(&crate::metadata::object([
                ("projectKey", project_value.clone()),
                ("message", "fixture task".into()),
                ("sessionKey", "fixture:chat=test:general".into()),
                ("channelKey", "fixture".into()),
                (
                    "schedule",
                    crate::metadata::object([
                        ("type", "once".into()),
                        ("runAt", "2099-01-01T00:00:00.000Z".into()),
                    ]),
                ),
            ]))
            .unwrap();
        manager
            .run_task_now(&crate::metadata::object([
                ("projectKey", project_value),
                (
                    "taskId",
                    created.get("task").unwrap().get("taskId").unwrap().clone(),
                ),
            ]))
            .unwrap();
        manager
            .run_tick_once(Some(project.to_str().unwrap()))
            .unwrap();
        let delivery = tokio::time::timeout(std::time::Duration::from_secs(3), ready)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.text, JsString::from("production delivery"));
        assert_eq!(
            delivery.origin_channel_key,
            Some(ChannelKey::new("fixture"))
        );
        assert!(!dropped.load(Ordering::SeqCst));
        let drain = tokio::task::spawn_blocking(move || {
            manager.stop_and_drain(std::time::Duration::from_secs(2), || shutdown.cancel())
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), drain)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            dropped.load(Ordering::SeqCst),
            "the real bound callback must drop pending delivery before drain returns"
        );
        server.close().await.unwrap();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_drains_pending_delivery_before_current_thread_runtime_closes() {
        let runtime = tokio::runtime::Handle::current();
        let shutdown = crate::runtime::network::Cancellation::new();
        let worker_shutdown = shutdown.clone();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let witness = Dropped(dropped.clone());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            complete_delivery(
                &runtime,
                Some(&worker_shutdown),
                std::time::Duration::from_secs(30),
                async {
                    let _witness = witness;
                    entered.send(()).unwrap();
                    std::future::pending::<bool>().await
                },
            )
        });
        ready.await.unwrap();
        shutdown.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .expect("shutdown must not wait for the 30-second delivery deadline")
            .unwrap();
        assert_eq!(result, Err("delivery cancelled during gateway shutdown"));
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "delivery must be dropped before drain returns"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fire_worker_cannot_finish_before_delivery_and_reports_timeout() {
        let runtime = tokio::runtime::Handle::current();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            complete_delivery(&runtime, None, std::time::Duration::from_secs(2), async {
                entered.send(()).unwrap();
                wait.await.unwrap();
                true
            })
        });
        ready.await.unwrap();
        assert!(
            !worker.is_finished(),
            "fire worker must retain pending delivery"
        );
        release.send(()).unwrap();
        assert_eq!(worker.await.unwrap(), Ok(true));
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            complete_delivery(
                &runtime,
                None,
                std::time::Duration::from_millis(20),
                std::future::pending(),
            )
        })
        .await
        .unwrap();
        assert_eq!(result, Err("delivery timed out"));
    }
}

/// `submitTurn`'s input exactly as CronFire.ts:80-88 builds it — the seven
/// fields the port's `CronTurnSubmit` carries; a task without a projectKey
/// omits the key, which reads as `undefined` to the submitter like TS.
fn submit_input(input: &CronTurnSubmit) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "sessionKey".into(),
        JsValue::String(input.session_key.clone()),
    );
    out.insert(
        "channelKey".into(),
        JsValue::String(input.channel_key.clone()),
    );
    if let Some(project_key) = &input.project_key {
        out.insert("projectKey".into(), JsValue::String(project_key.clone()));
    }
    out.insert("message".into(), JsValue::String(input.message.clone()));
    out.insert("mode".into(), JsValue::String(input.mode.clone()));
    out.insert("runId".into(), JsValue::String(input.run_id.clone()));
    out.insert("timeoutMs".into(), JsValue::Number(input.timeout_ms));
    JsValue::Object(out)
}

/// The turn-execution seam of the live gateway. Threading contract: cron's
/// port methods are synchronous, while turn submission and session close are
/// futures the gateway's current-thread runtime owns. `run_turn` must drain
/// the whole turn (the port returns only after the turn unwound), so it may
/// only run on a non-runtime thread — the cron scheduler's own thread
/// (runtime/cron/scheduler.rs), or the fixtures' driver threads — and blocks
/// the captured `Handle`;
/// `close_session` mirrors TS's fire-and-forget call sites
/// (`.catch(() => undefined)`, never awaited) by scheduling the close and
/// returning; `abort_turn` is synchronous end to end
/// (`start_gateway_abort`, InProcessGateway.ts:2000-2013's synchronous
/// prefix).
pub(super) struct NativeCronGatewayBody {
    owner: SessionOwner,
    submitter: Arc<TurnSubmitter>,
    runtime: tokio::runtime::Handle,
    shutdown: crate::runtime::network::Cancellation,
}

impl NativeCronGatewayBody {
    pub(super) fn new(
        owner: SessionOwner,
        submitter: Arc<TurnSubmitter>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            owner,
            submitter,
            runtime,
            shutdown: crate::runtime::network::Cancellation::new(),
        }
    }
    pub(super) fn shutdown_signal(&self) -> crate::runtime::network::Cancellation {
        self.shutdown.clone()
    }
}

/// A turn that never ran or did not finish, read off the event the drain
/// would otherwise discard.
///
/// `runtime/turn/submit.rs` reports every such outcome as an ordinary `Ok`
/// stream whose last event is an `error`: preflight refusals
/// (`session_busy` / `SESSION_BUSY`, `workspace_busy`,
/// `CAPABILITY_UNAVAILABLE`, registry errors — all `recoverable: true`), and
/// mid-turn failures (`turn_timeout`, agent failures — `recoverable: false`).
/// The port's contract is `Err` for "cannot be admitted (busy, maintenance,
/// shutdown) **or the turn itself fails**" (`CronGatewayBody::run_turn`),
/// and TS's `CronFire` reads any `type === "error"` event as a failed
/// outcome (CronFire.ts:125-140), so the boundary surfaces the event as
/// `Err`. Which outcome that becomes — `failed`, `cron_run_timeout`,
/// `aborted` — is the firing half's (fire.rs, #2295) call; the code is
/// passed through untouched so it can tell them apart.
fn failed_turn(event: &JsValue) -> Option<CronError> {
    let object = event.as_object()?;
    // Hot path: a turn's stream is mostly `text_delta` / `agent_status`, and
    // those must not pay a UTF-16 -> String conversion just to be classified.
    let is_error = object
        .get_property("type")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("error"));
    if !is_error {
        return None;
    }
    let text = |name: &str| {
        object
            .get_property(name)
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
    };
    let code = text("code")
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| "cron_run_failed".to_string());
    let message = text("message")
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| code.clone());
    Some(CronError::gateway(code.as_str(), JsString::from(message)))
}

/// The event stream a turn submission is, as the drain sees it: one step at
/// a time, `Err` reserved for transport failures (never for an event).
/// Factored out so the drain loop itself is testable against a scripted
/// stream — the rejection it guards is the loop's `return Err`, not just the
/// per-event classifier.
trait CronTurnStream {
    fn next_event(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<JsValue>, SubmitError>>;
}

impl CronTurnStream for TurnSubmission {
    fn next_event(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<JsValue>, SubmitError>> {
        self.next()
    }
}

/// Drain one admitted turn to its end. The first `error` event ends the
/// drain with `Err` (see `failed_turn`); everything else is the `CronFire`
/// half's (fire.rs, #2295).
async fn drain_turn<S: CronTurnStream>(stream: &mut S) -> Result<(), CronError> {
    while let Some(event) = stream.next_event().await.map_err(|error| {
        CronError::gateway(
            "cron_run_failed",
            JsString::from(format!("Error: {}", error.message)),
        )
    })? {
        if let Some(failure) = failed_turn(&event) {
            return Err(failure);
        }
    }
    Ok(())
}

#[cfg(test)]
async fn drain_observed_turn<S: CronTurnStream>(
    stream: &mut S,
    observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    abort: impl FnMut(),
) -> Result<(), CronError> {
    drain_observed_turn_with_shutdown(stream, observe, abort, None, || {}).await
}
async fn drain_observed_turn_with_shutdown<S: CronTurnStream>(
    stream: &mut S,
    observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    mut abort: impl FnMut(),
    shutdown: Option<&crate::runtime::network::Cancellation>,
    mut stop: impl FnMut(),
) -> Result<(), CronError> {
    let mut failure = None;
    let mut stopping = false;
    let mut retry = tokio::time::interval(std::time::Duration::from_millis(25));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retry.tick().await;
    loop {
        // next_event/TurnSubmission::next owns a completion receiver while
        // awaiting End settlement. Keep that very future pinned across retry
        // ticks; canceling/recreating it would lose the settlement receiver.
        let step = {
            let next = stream.next_event();
            tokio::pin!(next);
            loop {
                tokio::select! {
                    biased;
                    _ = async { shutdown.expect("guarded shutdown signal").cancelled().await }, if shutdown.is_some() && !stopping => {
                        stopping = true; stop();
                    },
                    _ = retry.tick(), if stopping => {
                        // First abort may precede SessionOwner's live handle.
                        stop();
                    },
                    step = &mut next => break step,
                }
            }
        };
        match step {
            Ok(Some(event)) => {
                if failure.is_none()
                    && let Err(error) = observe(&event)
                {
                    failure = Some(error);
                    abort();
                }
            }
            Ok(None) => break,
            Err(error) => {
                return Err(
                    failure.unwrap_or_else(|| CronError::gateway(&error.code, error.message))
                );
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

impl CronGatewayBody for NativeCronGatewayBody {
    fn run_turn_observed(
        &self,
        input: &CronTurnSubmit,
        observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(CronError::error(
                "Cron run_turn cannot execute on the gateway runtime thread; the scheduler must call it from its own thread.",
            ));
        }
        let submitter = self.submitter.clone();
        let owner = self.owner.clone();
        let shutdown = self.shutdown.clone();
        self.runtime.block_on(async move {
            if shutdown.is_cancelled() {
                return Err(CronError::gateway(
                    "agent_aborted",
                    "Cron stopped before turn admission.",
                ));
            }
            let mut submission = submitter.submit_reliable(submit_input(input));
            drain_observed_turn_with_shutdown(
                &mut submission,
                observe,
                || {
                    let _ = owner.start_gateway_abort(
                        &input.session_key,
                        Some(&input.run_id),
                        Some(&JsString::from("system:cron_event_failure")),
                    );
                },
                Some(&shutdown),
                || {
                    let _ = owner.start_gateway_abort(
                        &input.session_key,
                        Some(&input.run_id),
                        Some(&JsString::from("system:cron_shutdown")),
                    );
                },
            )
            .await
        })
    }
    fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        // Blocking a worker of the gateway's (current-thread) runtime would
        // deadlock the drain below, which only that runtime can advance.
        // An ambient handle here means a runtime thread called us — a
        // wiring bug that is answered, not deadlocked over.
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(CronError::error(JsString::from(
                "Cron run_turn cannot execute on the gateway runtime thread; the scheduler must call it from its own thread.",
            )));
        }
        let submitter = self.submitter.clone();
        self.runtime.block_on(async move {
            let mut submission = submitter.submit(submit_input(input));
            // The event stream itself is the `CronFire` half's (fire.rs,
            // #2295: store claims, run records, result delivery); this
            // boundary only
            // keeps the port contract of unwinding the admitted turn and
            // reporting a turn that never ran or did not finish as `Err`
            // (`drain_turn`).
            drain_turn(&mut submission).await
        })
    }

    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.owner
            .start_gateway_abort(session_key, run_id, reason)
            .map(|_| ())
            .map_err(|error| CronError::error(error.message))
    }

    fn close_session(
        &self,
        session_key: &JsString,
        _reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        // Every cron call site fires closeSession without awaiting it
        // (`CronRuntime.ts` `.catch(() => undefined)`); dropping the handle
        // detaches the task, which is that fire-and-forget shape. The owner's
        // close has no reason channel, so the reason stops at this boundary.
        let owner = self.owner.clone();
        let key = session_key.clone();
        std::mem::drop(self.runtime.spawn(async move {
            let _ = owner.close(&key).await;
        }));
        Ok(())
    }
}

#[cfg(test)]
mod cron_wiring_tests;

#[cfg(test)]
mod observed_drain_tests;
