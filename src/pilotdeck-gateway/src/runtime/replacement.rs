//! Volatile fork/replace/finalize coordination from
//! `GatewayTurnReplacementCoordinator.ts:1-294` and `InProcessGateway.ts:2872-2890`.
//! Durable transcript edits remain behind storage ports. Requests/results retain
//! their protocol values so provider-specific fields are forwarded unchanged.
use super::{refresh::BoxFuture, session::SessionOwner};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::task::AbortHandle;

#[derive(Clone, Debug, PartialEq)]
pub struct ReplacementError {
    pub code: Option<&'static str>,
    pub message: String,
    pub details: Option<Box<JsValue>>,
}
impl ReplacementError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            details: None,
        }
    }
    fn dialog(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            ..Self::new(message)
        }
    }
}
impl std::fmt::Display for ReplacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ReplacementError {}
pub type ReplacementResult = Result<JsValue, ReplacementError>;
pub type StorageOperation = Arc<dyn Fn(JsValue) -> BoxFuture<ReplacementResult> + Send + Sync>;
pub type WarningSink = Arc<dyn Fn(&str, &ReplacementError) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ReplacementStorage {
    pub fork_session: Option<StorageOperation>,
    pub replace_last_turn: Option<StorageOperation>,
    pub finalize_last_turn_replacement: Option<StorageOperation>,
}

/// Implementations must abort the expected run through the completion fence,
/// and close/drain the session-owned writer before returning from close.
pub trait ReplacementSession: Send + Sync {
    fn active_turn_run_id(&self, key: &JsString) -> Option<JsString>;
    fn has_active_turn(&self, key: &JsString) -> bool;
    fn abort_expected_turn(
        &self,
        key: &JsString,
        run: &JsString,
    ) -> BoxFuture<Result<(), ReplacementError>>;
    fn close_session(&self, key: &JsString) -> BoxFuture<Result<(), ReplacementError>>;
}
impl ReplacementSession for SessionOwner {
    fn active_turn_run_id(&self, key: &JsString) -> Option<JsString> {
        self.active_turn_run_id(key)
    }
    fn has_active_turn(&self, key: &JsString) -> bool {
        self.has_active_turn(key)
    }
    fn abort_expected_turn(
        &self,
        key: &JsString,
        run: &JsString,
    ) -> BoxFuture<Result<(), ReplacementError>> {
        let owner = self.clone();
        let (key, run) = (key.clone(), run.clone());
        Box::pin(async move {
            owner
                .abort_turn(&key, Some(&run), Some(&"message_replaced".into()))
                .await
                .map_err(|error| ReplacementError::new(error.message))
        })
    }
    fn close_session(&self, key: &JsString) -> BoxFuture<Result<(), ReplacementError>> {
        let close = self.close(key);
        Box::pin(async move {
            close
                .await
                .map_err(|error| ReplacementError::new(error.message))
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Submitting,
    Finalizing,
}
struct Pending {
    transaction_id: JsString,
    replacement_turn_id: JsString,
    project_key: Option<JsValue>,
    phase: Phase,
    identity: Arc<()>,
    timer: Option<(Arc<()>, AbortHandle)>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some((_, timer)) = self.timer.take() {
            timer.abort();
        }
    }
}
#[derive(Default)]
struct State {
    pending: HashMap<JsString, Pending>,
    reservations: HashSet<JsString>,
}
struct Inner {
    state: Mutex<State>,
    storage: ReplacementStorage,
    session: Arc<dyn ReplacementSession>,
    timeout: Duration,
    warn: WarningSink,
}

#[derive(Clone)]
pub struct ReplacementCoordinator(Arc<Inner>);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitClaim {
    None,
    Claimed,
    Conflict,
}
impl ReplacementCoordinator {
    pub fn new(
        storage: ReplacementStorage,
        session: Arc<dyn ReplacementSession>,
        timeout_ms: u64,
        warn: Option<WarningSink>,
    ) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            storage,
            session,
            timeout: Duration::from_millis(timeout_ms.max(1)),
            warn: warn.unwrap_or_else(|| Arc::new(|message, error| eprintln!("{message} {error}"))),
        }))
    }
    pub fn reserve_transcript_write(
        &self,
        key: &JsString,
        operation: &str,
    ) -> Result<(), ReplacementError> {
        let mut state = self.0.state.lock().unwrap();
        if state.reservations.contains(key) || state.pending.contains_key(key) {
            return Err(ReplacementError::dialog(
                "SESSION_BUSY",
                format!("Cannot {operation} while another transcript update is pending."),
            ));
        }
        state.reservations.insert(key.clone());
        Ok(())
    }
    pub fn release_transcript_write(&self, key: &JsString) {
        self.0.state.lock().unwrap().reservations.remove(key);
    }
    pub fn has_transcript_write_reservation(&self, key: &JsString) -> bool {
        self.0.state.lock().unwrap().reservations.contains(key)
    }
    pub fn fork_session(&self, input: JsValue) -> BoxFuture<ReplacementResult> {
        match &self.0.storage.fork_session {
            Some(fork) => running(port(|| fork(input))),
            None => failed(ReplacementError::new(
                "fork_session is not configured. Wire `forkSession` via createLocalGateway.",
            )),
        }
    }
    /// Admission runs at call time. The owned operation continues if its caller
    /// drops the returned future, matching an already-started JS promise.
    pub fn replace_last_turn(&self, input: JsValue) -> BoxFuture<ReplacementResult> {
        let Some(replace) = self.0.storage.replace_last_turn.clone() else {
            return failed(ReplacementError::new(
                "replace_last_turn is not configured. Wire `replaceLastTurn` via createLocalGateway.",
            ));
        };
        let replacement = input.get("replacementTurnId").and_then(JsValue::as_string);
        if replacement.is_none_or(|id| id.units().iter().all(|unit| js_whitespace(*unit))) {
            return failed(ReplacementError::dialog(
                "replace_invalid_input",
                "replacementTurnId is required.",
            ));
        }
        let replacement = replacement.unwrap().clone();
        let key = string(&input, "sessionKey");
        let active = {
            // Serialize competing reservations. Session getters must not call
            // back into this coordinator (as with the baseline's sync getters).
            let mut state = self.0.state.lock().unwrap();
            if state.reservations.contains(&key) || state.pending.contains_key(&key) {
                return failed(ReplacementError::dialog(
                    "replace_turn_pending",
                    "A replacement transaction is already pending for this session.",
                ));
            }
            let active = match port_call(|| self.0.session.active_turn_run_id(&key)) {
                Ok(active) => active.filter(|id| !id.is_empty()),
                Err(error) => return failed(error),
            };
            if let Some(active) = &active
                && *active != string(&input, "expectedTurnId")
            {
                let mut error = ReplacementError::dialog(
                    "replace_turn_conflict",
                    "The selected message is no longer the active turn.",
                );
                let mut details = JsObject::new();
                details.insert("activeRunId".into(), JsValue::String(active.clone()));
                details.insert(
                    "expectedTurnId".into(),
                    input
                        .get("expectedTurnId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                error.details = Some(Box::new(JsValue::Object(details)));
                return failed(error);
            }
            if self.0.storage.finalize_last_turn_replacement.is_none() {
                return failed(ReplacementError::new(
                    "finalize_last_turn_replacement is required when replace_last_turn is configured.",
                ));
            }
            state.reservations.insert(key.clone());
            active
        };
        let this = self.clone();
        // Invoke abort synchronously, as the TS async prefix does.
        let abort = active.map(|run| port(|| self.0.session.abort_expected_turn(&key, &run)));
        running(async move {
            let result = async {
                if let Some(abort) = abort {
                    abort.await?;
                }
                port(|| this.0.session.close_session(&key)).await?;
                let result = port(|| replace(input.clone())).await?;
                let pending = Pending {
                    transaction_id: string(&result, "transactionId"),
                    replacement_turn_id: replacement,
                    project_key: input.get("projectKey").cloned(),
                    phase: Phase::Prepared,
                    identity: Arc::new(()),
                    timer: None,
                };
                {
                    let mut state = this.0.state.lock().unwrap();
                    // TS :107 publishes by key even after dispose; an older
                    // completion may overwrite a newer pending transaction.
                    state.pending.insert(key.clone(), pending);
                    this.schedule_locked(&mut state, &key);
                }
                Ok(result)
            }
            .await;
            let mut state = this.0.state.lock().unwrap();
            if result.is_err() {
                state.pending.remove(&key);
            }
            state.reservations.remove(&key);
            result
        })
    }
    /// Like TS, claiming disables expiry until explicit release or commit.
    /// Admission callers must own that cleanup if their submit future is dropped.
    pub fn claim_for_submit(&self, key: &JsString, run: &JsString) -> SubmitClaim {
        let mut state = self.0.state.lock().unwrap();
        let Some(pending) = state.pending.get_mut(key) else {
            return SubmitClaim::None;
        };
        if pending.replacement_turn_id != *run || pending.phase != Phase::Prepared {
            return SubmitClaim::Conflict;
        }
        pending.phase = Phase::Submitting;
        cancel_timer(pending);
        SubmitClaim::Claimed
    }
    pub fn release_submit_claim(&self, key: &JsString, run: &JsString) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(pending) = state.pending.get_mut(key)
            && pending.replacement_turn_id == *run
            && pending.phase == Phase::Submitting
        {
            pending.phase = Phase::Prepared;
            self.schedule_locked(&mut state, key);
        }
    }
    pub fn commit_accepted_input(&self, key: &JsString, run: &JsString) -> BoxFuture<()> {
        let Some(finalize) = self.0.storage.finalize_last_turn_replacement.clone() else {
            return Box::pin(async {});
        };
        let input = {
            let mut state = self.0.state.lock().unwrap();
            let Some(pending) = state.pending.get_mut(key) else {
                return Box::pin(async {});
            };
            if pending.replacement_turn_id != *run || pending.phase != Phase::Submitting {
                return Box::pin(async {});
            }
            pending.phase = Phase::Finalizing;
            cancel_timer(pending);
            finalize_input(key, pending, "commit")
        };
        let (this, key) = (self.clone(), key.clone());
        let operation = port(|| finalize(input));
        let commit = running(async move {
            if let Err(error) = operation.await {
                this.warn(
                    "[pilotdeck] failed to remove accepted replacement backup:",
                    &error,
                );
            }
            // TS clearPending(:175) deliberately clears the current key, even
            // if dispose allowed a newer transaction while this port awaited.
            this.0.state.lock().unwrap().pending.remove(&key);
            Ok(JsValue::Null)
        });
        Box::pin(async move {
            let _ = commit.await;
        })
    }
    pub fn finalize_last_turn_replacement(&self, input: JsValue) -> BoxFuture<ReplacementResult> {
        let Some(finalize) = self.0.storage.finalize_last_turn_replacement.clone() else {
            return failed(ReplacementError::new(
                "finalize_last_turn_replacement is not configured. Wire `finalizeLastTurnReplacement` via createLocalGateway.",
            ));
        };
        let key = string(&input, "sessionKey");
        let identity = {
            let mut state = self.0.state.lock().unwrap();
            let Some(pending) = state
                .pending
                .get_mut(&key)
                .filter(|p| p.transaction_id == string(&input, "transactionId"))
            else {
                return failed(ReplacementError::dialog(
                    "replace_transaction_conflict",
                    "The replacement transaction is no longer pending.",
                ));
            };
            if pending.phase != Phase::Prepared {
                return failed(ReplacementError::dialog(
                    "replace_transaction_pending",
                    "The replacement transaction is already being finalized.",
                ));
            }
            pending.phase = Phase::Finalizing;
            cancel_timer(pending);
            pending.identity.clone()
        };
        let this = self.clone();
        // Run the active-turn check before returning control to the caller.
        let rollback = string(&input, "action") == JsString::from("rollback");
        let close = if rollback {
            let active = match port_call(|| this.0.session.has_active_turn(&key)) {
                Ok(active) => active,
                Err(error) => {
                    this.reset_prepared(&key, &identity);
                    return failed(error);
                }
            };
            if active {
                this.reset_prepared(&key, &identity);
                return failed(ReplacementError::dialog(
                    "replace_transaction_active",
                    "The replacement cannot be rolled back while its turn is active.",
                ));
            }
            Some(port(|| this.0.session.close_session(&key)))
        } else {
            None
        };
        running(async move {
            let result = async {
                if let Some(close) = close {
                    close.await?;
                }
                port(|| finalize(input)).await
            }
            .await;
            if result.is_ok() {
                this.0.state.lock().unwrap().pending.remove(&key);
            } else {
                this.reset_prepared(&key, &identity);
            }
            result
        })
    }
    pub fn dispose(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.pending.clear();
        state.reservations.clear();
    }
    fn reset_prepared(&self, key: &JsString, identity: &Arc<()>) {
        let mut state = self.0.state.lock().unwrap();
        if let Some(pending) = state.pending.get_mut(key)
            && Arc::ptr_eq(&pending.identity, identity)
        {
            pending.phase = Phase::Prepared;
        }
        // TS mutates the captured pending object, then schedules by key. An
        // in-flight failure after dispose must not reset a newer submit claim.
        self.schedule_locked(&mut state, key);
    }
    fn schedule_locked(&self, state: &mut State, key: &JsString) {
        let Some(pending) = state
            .pending
            .get_mut(key)
            .filter(|p| p.phase == Phase::Prepared)
        else {
            return;
        };
        cancel_timer(pending);
        let weak = Arc::downgrade(&self.0);
        let (key, transaction) = (key.clone(), pending.transaction_id.clone());
        let timer_identity = Arc::new(());
        let timer_token = timer_identity.clone();
        let sleep = tokio::time::sleep(self.0.timeout);
        pending.timer = Some((
            timer_identity,
            tokio::spawn(async move {
                sleep.await;
                if let Some(inner) = weak.upgrade() {
                    let this = Self(inner);
                    this.rollback_expired(key, transaction, timer_token);
                }
            })
            .abort_handle(),
        ));
    }
    // Diagnostics must not unwind past transaction cleanup or retry scheduling.
    fn warn(&self, message: &str, error: &ReplacementError) {
        let _ = port_call(|| (self.0.warn)(message, error));
    }
    fn rollback_expired(&self, key: JsString, transaction: JsString, timer_token: Arc<()>) {
        let Some(finalize) = self.0.storage.finalize_last_turn_replacement.clone() else {
            return;
        };
        let (input, identity) = {
            let mut state = self.0.state.lock().unwrap();
            let Some(pending) = state.pending.get_mut(&key).filter(|p| {
                p.transaction_id == transaction
                    && p.phase == Phase::Prepared
                    && p.timer
                        .as_ref()
                        .is_some_and(|(token, _)| Arc::ptr_eq(token, &timer_token))
            }) else {
                return;
            };
            // This timer has fired; never abort its own rollback continuation.
            pending.timer.take();
            let active = match port_call(|| self.0.session.has_active_turn(&key)) {
                Ok(active) => active,
                Err(error) => {
                    self.schedule_locked(&mut state, &key);
                    drop(state);
                    self.warn(
                        "[pilotdeck] failed to roll back expired replacement transaction:",
                        &error,
                    );
                    return;
                }
            };
            if active {
                self.schedule_locked(&mut state, &key);
                return;
            }
            pending.phase = Phase::Finalizing;
            (
                finalize_input(&key, pending, "rollback"),
                pending.identity.clone(),
            )
        };
        let this = self.clone();
        let close = port(|| self.0.session.close_session(&key));
        tokio::spawn(async move {
            let result = async {
                close.await?;
                port(|| finalize(input)).await
            }
            .await;
            match result {
                Ok(_) => {
                    this.0.state.lock().unwrap().pending.remove(&key);
                }
                Err(error) => {
                    this.warn(
                        "[pilotdeck] failed to roll back expired replacement transaction:",
                        &error,
                    );
                    this.reset_prepared(&key, &identity);
                }
            }
        });
    }
}
fn cancel_timer(pending: &mut Pending) {
    if let Some((_, timer)) = pending.timer.take() {
        timer.abort();
    }
}
fn failed(error: ReplacementError) -> BoxFuture<ReplacementResult> {
    Box::pin(async { Err(error) })
}
// Catch both the synchronous port prefix and every poll, so the unwind never
// escapes through a coordinator lock guard or skips the error continuation.
// Treat a Rust port panic like a TS thrown/rejected Error: preserve string
// payloads and run the ordinary error cleanup below.
fn port_call<T>(call: impl FnOnce() -> T) -> Result<T, ReplacementError> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "replacement port panicked".to_owned());
        ReplacementError::new(message)
    })
}
fn port<T: Send + 'static>(
    call: impl FnOnce() -> BoxFuture<Result<T, ReplacementError>>,
) -> BoxFuture<Result<T, ReplacementError>> {
    match port_call(call) {
        Ok(mut future) => Box::pin(std::future::poll_fn(move |cx| {
            match port_call(|| future.as_mut().poll(cx)) {
                Ok(poll) => poll,
                Err(error) => Poll::Ready(Err(error)),
            }
        })),
        Err(error) => Box::pin(async move { Err(error) }),
    }
}
fn running(
    future: impl std::future::Future<Output = ReplacementResult> + Send + 'static,
) -> BoxFuture<ReplacementResult> {
    let task = tokio::spawn(future);
    Box::pin(async move {
        task.await
            .unwrap_or_else(|error| Err(ReplacementError::new(error.to_string())))
    })
}
fn string(value: &JsValue, field: &str) -> JsString {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}
fn finalize_input(key: &JsString, pending: &Pending, action: &str) -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(key.clone()));
    if let Some(project) = &pending.project_key {
        input.insert("projectKey".into(), project.clone());
    }
    input.insert(
        "transactionId".into(),
        JsValue::String(pending.transaction_id.clone()),
    );
    input.insert("action".into(), JsValue::String(action.into()));
    JsValue::Object(input)
}
fn js_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

#[cfg(test)]
#[path = "replacement_tests.rs"]
mod tests;
