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
        }
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

impl CronGatewayBody for NativeCronGatewayBody {
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
