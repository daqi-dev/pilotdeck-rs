//! Gateway-owned user-dialog channel.
//!
//! The channel projects native tool requests into Gateway bus events, journals
//! them before exposing them, optionally publishes them to a host store, and
//! polls cross-Gateway answers. Timers are host-driven: `tick()` runs the same
//! 100 ms poll cadence as the TS `setInterval` without depending on a wall
//! clock. Abort notifications are delivered explicitly through `AbortSignal`.

use super::{
    DialogAnswer, DialogError, DialogRuntime, FileGatewayUserDialogStore, Result, StoreKey,
    StoredAnswer, StoredDialog, UserDialogJournal,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::format_epoch_iso;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

pub const LIVE_OWNER_RENEW_INTERVAL_MS: i64 = 100;

pub type EmitFn = Arc<dyn Fn(JsValue) -> Result<()> + Send + Sync>;
pub type OwnerFn = Arc<dyn Fn(&JsString, &JsString) -> Result<bool> + Send + Sync>;
pub type TakeAnswerFn = Arc<dyn Fn(&JsString) -> Result<Option<StoredAnswer>> + Send + Sync>;
pub type FormAcceptsFn = Arc<dyn Fn(&JsValue, &JsValue) -> bool + Send + Sync>;

/// Host-owned durable store boundary used by the channel. The host owns
/// atomicity and cross-process locking; `FileGatewayUserDialogStore` is the
/// local implementation.
pub trait UserDialogStore: Send + Sync {
    fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> Result<()>;
    fn remove(&self, key: &StoreKey, request_id: &JsString) -> Result<()>;
}

impl UserDialogStore for FileGatewayUserDialogStore {
    fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> Result<()> {
        FileGatewayUserDialogStore::put(self, key, dialog)
    }
    fn remove(&self, key: &StoreKey, request_id: &JsString) -> Result<()> {
        FileGatewayUserDialogStore::remove(self, key, request_id)
    }
}

#[derive(Clone)]
pub struct ChannelOptions {
    pub session_key: JsString,
    pub bus: Arc<Mutex<super::GatewayUserDialogBus>>,
    pub emit: EmitFn,
    pub journal: Option<Arc<dyn UserDialogJournal>>,
    pub store: Option<Arc<dyn UserDialogStore>>,
    pub store_key: Option<StoreKey>,
    pub take_stored_answer: Option<TakeAnswerFn>,
    pub claim_stored_owner: Option<OwnerFn>,
    pub renew_stored_owner: Option<OwnerFn>,
    pub release_stored_owner: Option<OwnerFn>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub poll_interval_ms: i64,
    /// Schema validation for form answers stays outside this package. Without
    /// a validator form answers are passed through as TS values, exactly as
    /// received; local callers still validate before consuming the bus.
    pub form_accepts: Option<FormAcceptsFn>,
}

impl ChannelOptions {
    pub fn new(
        session_key: impl Into<JsString>,
        bus: Arc<Mutex<super::GatewayUserDialogBus>>,
        emit: EmitFn,
    ) -> Self {
        let runtime = DialogRuntime::default();
        Self {
            session_key: session_key.into(),
            bus,
            emit,
            journal: None,
            store: None,
            store_key: None,
            take_stored_answer: None,
            claim_stored_owner: None,
            renew_stored_owner: None,
            release_stored_owner: None,
            uuid: runtime.uuid,
            now_ms: runtime.now,
            poll_interval_ms: LIVE_OWNER_RENEW_INTERVAL_MS,
            form_accepts: None,
        }
    }
}

/// A host-side abort notification. The TS channel listens to `AbortSignal`;
/// Rust hosts call `abort()` on the channel (directly or from their signal).
#[derive(Clone, Default)]
pub struct AbortSignal {
    inner: Arc<AbortInner>,
}
type AbortCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct AbortState {
    aborted: bool,
    next_token: u64,
    callbacks: Vec<(u64, AbortCallback)>,
}

#[derive(Default)]
struct AbortInner {
    state: Mutex<AbortState>,
}
impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn abort(&self) {
        let callbacks = {
            let mut state = self.inner.state.lock().unwrap();
            if state.aborted {
                return;
            }
            state.aborted = true;
            std::mem::take(&mut state.callbacks)
        };
        for (_, callback) in callbacks {
            callback();
        }
    }
    pub fn is_aborted(&self) -> bool {
        self.inner.state.lock().unwrap().aborted
    }
    /// Registers a callback and returns the token needed to remove only this
    /// registration, mirroring TS `addEventListener`/`removeEventListener`.
    pub(super) fn on_abort(&self, callback: AbortCallback) -> Option<u64> {
        let mut state = self.inner.state.lock().unwrap();
        if state.aborted {
            return None;
        }
        state.next_token += 1;
        let token = state.next_token;
        state.callbacks.push((token, callback));
        Some(token)
    }
    pub(super) fn remove_abort(&self, token: u64) {
        if token == 0 {
            return;
        }
        self.inner
            .state
            .lock()
            .unwrap()
            .callbacks
            .retain(|(id, _)| *id != token);
    }
}

#[derive(Clone)]
pub struct InputRequest {
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub prompt: JsString,
    pub placeholder: Option<JsString>,
    pub allow_empty: bool,
    pub abort: Option<AbortSignal>,
}

#[derive(Clone)]
pub struct SelectChoice {
    pub value: JsString,
    pub label: Option<JsString>,
    pub description: Option<JsString>,
}

#[derive(Clone)]
pub struct SelectRequest {
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub prompt: JsString,
    pub choices: Vec<SelectChoice>,
    pub default_value: Option<JsString>,
    pub abort: Option<AbortSignal>,
}

#[derive(Clone)]
pub struct ConfirmRequest {
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub prompt: JsString,
    pub confirm_label: Option<JsString>,
    pub cancel_label: Option<JsString>,
    pub default_value: Option<bool>,
    pub abort: Option<AbortSignal>,
}

#[derive(Clone)]
pub struct FormRequest {
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub prompt: JsString,
    pub schema: JsValue,
    pub abort: Option<AbortSignal>,
}

type Accepts = Arc<dyn Fn(&JsValue) -> bool + Send + Sync>;

struct RequestState {
    request_id: JsString,
    dialog_kind: JsString,
    owner_id: JsString,
    registered: AtomicBool,
    settled: AtomicBool,
    cancelled_before_registration: AtomicBool,
    owner_claimed: AtomicBool,
    abort_registration: AtomicU64,
    accepts: Accepts,
    /// Registration number of the bus entry this request owns (#1599). A host
    /// callback can reenter the bus and re-register the same request id while
    /// an answer is being taken; the poll compares this number before
    /// consuming so the answer stays with the dialog it was taken for.
    generation: AtomicU64,
    /// Publication order of this registration in the channel's request table
    /// (#1599). `bus.register` numbers by bus call order, which a reentrant
    /// host callback can invert relative to the table; the table is what
    /// decides who owns a request id, so the poll registry orders by this.
    registration: AtomicU64,
    /// Sticky "a later registration published this request id" flag (#1599).
    /// The request table alone cannot answer this once the replacement has
    /// completed and removed its own entry: an absent entry must not read as
    /// "still mine", or a suspended original would publish over the
    /// replacement and settle its rows.
    replaced: AtomicBool,
    abort: Option<AbortSignal>,
    result: mpsc::Sender<std::result::Result<DialogAnswer, String>>,
}

struct PollEntry {
    due_ms: i64,
    polling: bool,
    /// Registration in the request table this poll belongs to. A reentrant
    /// host callback may replace the dialog under the same request id; the old
    /// request must then leave the replacement's poll alone (#1599).
    registration: u64,
}
#[derive(Default)]
struct PollRegistry {
    order: Vec<JsString>,
    entries: HashMap<JsString, PollEntry>,
}

struct Inner {
    options: ChannelOptions,
    requests: Mutex<HashMap<JsString, Arc<RequestState>>>,
    polls: Mutex<PollRegistry>,
    /// Monotonic registration order (#1599). Assigned while the request table
    /// is held, so `registration` orders exactly like table ownership: the
    /// registration that published last is the one that owns the id.
    next_registration: AtomicU64,
}

#[derive(Clone)]
pub struct GatewayUserDialogChannel {
    inner: Arc<Inner>,
}

/// The native tool's pending request. `try_result` returns the answer once the
/// bus, an abort, or a cross-Gateway poll settles it.
pub struct ChannelRequest {
    request_id: JsString,
    dialog_kind: JsString,
    state: Arc<RequestState>,
    receiver: mpsc::Receiver<std::result::Result<DialogAnswer, String>>,
}
impl ChannelRequest {
    pub fn request_id(&self) -> &JsString {
        &self.request_id
    }
    pub fn dialog_kind(&self) -> &JsString {
        &self.dialog_kind
    }
    pub fn is_pending(&self) -> bool {
        !self.state.settled.load(Ordering::SeqCst)
    }
    pub fn try_result(&mut self) -> Option<std::result::Result<DialogAnswer, String>> {
        self.receiver.try_recv().ok()
    }
}

impl GatewayUserDialogChannel {
    pub fn new(options: ChannelOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                requests: Mutex::new(HashMap::new()),
                polls: Mutex::new(PollRegistry::default()),
                next_registration: AtomicU64::new(0),
            }),
        }
    }

    pub fn request_input(&self, request: &InputRequest) -> ChannelRequest {
        let event = {
            let request = request.clone();
            move |request_id: &JsString| {
                let mut value = base_event(
                    "input",
                    request_id,
                    &request.tool_call_id,
                    &request.tool_name,
                    &request.prompt,
                );
                if let Some(placeholder) = &request.placeholder
                    && !placeholder.is_empty()
                {
                    set(
                        &mut value,
                        "placeholder",
                        JsValue::String(placeholder.clone()),
                    );
                }
                if request.allow_empty {
                    set(&mut value, "allowEmpty", JsValue::Bool(true));
                }
                value
            }
        };
        self.begin(
            "input",
            request.abort.clone(),
            Arc::new(|value: &JsValue| value.as_string().is_some()),
            event,
        )
    }

    pub fn request_select(&self, request: &SelectRequest) -> ChannelRequest {
        let values: HashSet<JsString> = request
            .choices
            .iter()
            .map(|choice| choice.value.clone())
            .collect();
        let event = {
            let request = request.clone();
            move |request_id: &JsString| {
                let mut value = base_event(
                    "select",
                    request_id,
                    &request.tool_call_id,
                    &request.tool_name,
                    &request.prompt,
                );
                let choices = request
                    .choices
                    .iter()
                    .map(|choice| {
                        let mut value = JsObject::new();
                        value.insert("value".into(), JsValue::String(choice.value.clone()));
                        if let Some(label) = &choice.label {
                            value.insert("label".into(), JsValue::String(label.clone()));
                        }
                        if let Some(description) = &choice.description {
                            value
                                .insert("description".into(), JsValue::String(description.clone()));
                        }
                        JsValue::Object(value)
                    })
                    .collect();
                set(&mut value, "choices", JsValue::Array(choices));
                if let Some(default_value) = &request.default_value {
                    set(
                        &mut value,
                        "defaultValue",
                        JsValue::String(default_value.clone()),
                    );
                }
                value
            }
        };
        self.begin(
            "select",
            request.abort.clone(),
            Arc::new(move |value: &JsValue| {
                value
                    .as_string()
                    .is_some_and(|value| values.contains(value))
            }),
            event,
        )
    }

    pub fn request_confirm(&self, request: &ConfirmRequest) -> ChannelRequest {
        let event = {
            let request = request.clone();
            move |request_id: &JsString| {
                let mut value = base_event(
                    "confirm",
                    request_id,
                    &request.tool_call_id,
                    &request.tool_name,
                    &request.prompt,
                );
                if let Some(confirm_label) = &request.confirm_label {
                    set(
                        &mut value,
                        "confirmLabel",
                        JsValue::String(confirm_label.clone()),
                    );
                }
                if let Some(cancel_label) = &request.cancel_label {
                    set(
                        &mut value,
                        "cancelLabel",
                        JsValue::String(cancel_label.clone()),
                    );
                }
                if let Some(default_value) = request.default_value {
                    set(&mut value, "defaultValue", JsValue::Bool(default_value));
                }
                value
            }
        };
        self.begin(
            "confirm",
            request.abort.clone(),
            Arc::new(|value: &JsValue| value.as_bool().is_some()),
            event,
        )
    }

    pub fn request_form(&self, request: &FormRequest) -> ChannelRequest {
        let event = {
            let request = request.clone();
            move |request_id: &JsString| {
                let mut value = base_event(
                    "form",
                    request_id,
                    &request.tool_call_id,
                    &request.tool_name,
                    &request.prompt,
                );
                set(&mut value, "schema", request.schema.clone());
                value
            }
        };
        let validator = self.inner.options.form_accepts.clone();
        let schema = request.schema.clone();
        self.begin(
            "form",
            request.abort.clone(),
            Arc::new(move |value: &JsValue| {
                validator
                    .as_ref()
                    .is_none_or(|accepts| accepts(&schema, value))
            }),
            event,
        )
    }

    fn begin(
        &self,
        dialog_kind: &str,
        abort: Option<AbortSignal>,
        accepts: Accepts,
        event: impl FnOnce(&JsString) -> JsValue + Send + 'static,
    ) -> ChannelRequest {
        let request_id: JsString = (self.inner.options.uuid)().into();
        let owner_id: JsString = (self.inner.options.uuid)().into();
        let request_event = event(&request_id);
        let (sender, receiver) = mpsc::channel();
        let state = Arc::new(RequestState {
            request_id: request_id.clone(),
            dialog_kind: dialog_kind.into(),
            owner_id,
            registered: AtomicBool::new(false),
            settled: AtomicBool::new(false),
            cancelled_before_registration: AtomicBool::new(false),
            owner_claimed: AtomicBool::new(false),
            abort_registration: AtomicU64::new(0),
            accepts,
            generation: AtomicU64::new(0),
            registration: AtomicU64::new(0),
            replaced: AtomicBool::new(false),
            abort: abort.clone(),
            result: sender,
        });
        {
            // #1599: number the registration while the request table is held,
            // so the poll registry's order matches table ownership even when a
            // host callback reenters between these two steps.
            let mut requests = self.inner.requests.lock().unwrap();
            let registration = self.inner.next_registration.fetch_add(1, Ordering::SeqCst) + 1;
            state.registration.store(registration, Ordering::SeqCst);
            // #1599: supersession is sticky. The replaced request keeps the
            // flag even after this registration completes and removes its own
            // table entry, so a suspended original can never mistake the empty
            // slot for "the id is still mine".
            if let Some(previous) = requests.get(&request_id)
                && !Arc::ptr_eq(previous, &state)
            {
                previous.replaced.store(true, Ordering::SeqCst);
            }
            requests.insert(request_id.clone(), state.clone());
        }

        if let Some(signal) = &abort {
            let channel = self.clone();
            // #1599: bind the callback to this registration, not just the id.
            // A same-id replacement owns the id once it publishes, and the
            // signal of the superseded request must not cancel it.
            let owned = state.clone();
            if let Some(token) =
                signal.on_abort(Arc::new(move || channel.cancel_owned(&owned, "aborted")))
            {
                state.abort_registration.store(token, Ordering::SeqCst);
            }
        }

        let stored = StoredDialog {
            request: request_event.clone(),
            created_at: format_epoch_iso((self.inner.options.now_ms)()).into(),
        };
        if let Err(error) = self.persist(&state, &request_event, &stored) {
            // A pre-registration abort resolves the waiter before a persist
            // failure can surface, matching the TS promise ordering.
            let answer = if state.cancelled_before_registration.load(Ordering::SeqCst) {
                Ok(DialogAnswer::Cancelled(Some("aborted".into())))
            } else {
                Err(error.to_string())
            };
            self.complete(&state, answer);
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }

        // #1599: `persist` is host code. If it reentered and registered the
        // same request id, that registration owns the id and the bus bucket.
        // Registering here would replace its bus entry by id
        // (`GatewayUserDialogBus.ts:150-160`), so both waiters would be left
        // without a usable entry. End the superseded waiter instead.
        if self.end_if_superseded(&state, "store write") {
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }

        let aborted = state.cancelled_before_registration.load(Ordering::SeqCst)
            || abort.as_ref().is_some_and(AbortSignal::is_aborted);
        if aborted {
            if let Err(error) = settle(&self.inner, &state) {
                self.complete(&state, Err(error.to_string()));
                return ChannelRequest {
                    request_id,
                    dialog_kind: dialog_kind.into(),
                    state,
                    receiver,
                };
            }
            self.complete(&state, Ok(DialogAnswer::Cancelled(Some("aborted".into()))));
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }

        let tool_call_id = request_event
            .get("toolCallId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let tool_name = request_event
            .get("toolName")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let pending = super::PendingDialog {
            request_id: request_id.clone(),
            dialog_kind: state.dialog_kind.clone(),
            tool_call_id,
            tool_name,
            event: request_event.clone(),
            // #1442: `accepts` is host code, so it travels as a cloneable Arc
            // and only runs after the bus guard drops.
            accepts: state.accepts.clone(),
            resolve: {
                let channel = self.clone();
                let state = state.clone();
                Box::new(move |answer| channel.complete(&state, Ok(answer)))
            },
            reject: {
                let channel = self.clone();
                let state = state.clone();
                Box::new(move |message| channel.complete(&state, Err(message)))
            },
            on_settled: {
                let channel = self.clone();
                let state = state.clone();
                Some(Box::new(move || {
                    stop_polling(
                        &channel.inner,
                        &state.request_id,
                        state.registration.load(Ordering::SeqCst),
                    );
                    let _ = settle(&channel.inner, &state);
                    Ok(())
                }))
            },
        };
        {
            let mut bus = self.inner.options.bus.lock().unwrap();
            let generation = bus.register(&self.inner.options.session_key, pending);
            state.generation.store(generation, Ordering::SeqCst);
            // #1442: the change observer is host code; deliver it only after
            // the guard drops.
            bus.drain_events()
        }
        .deliver();
        state.registered.store(true, Ordering::SeqCst);

        if let Err(error) = (self.inner.options.emit)(request_event.clone()) {
            self.complete(&state, Err(error.to_string()));
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }

        // An abort that landed between the pre-registration check and
        // `registered = true` (for example from the bus change observer) only
        // raised the flag; the entry is now consumable, so run the registered
        // cancellation path. This is the only place that settles such a race.
        if state.cancelled_before_registration.load(Ordering::SeqCst)
            || abort.as_ref().is_some_and(AbortSignal::is_aborted)
        {
            // Bound to this registration: if a host callback already replaced
            // the id, the replacement must not be cancelled from here (#1599).
            self.cancel_owned(&state, "aborted");
        }

        // `now_ms` and `emit` are host code; either may have registered the
        // same request id while this registration was still assembling itself.
        let poll_due = (!state.settled.load(Ordering::SeqCst)
            && self.inner.options.take_stored_answer.is_some())
        .then(|| {
            (self.inner.options.now_ms)().saturating_add(self.inner.options.poll_interval_ms.max(1))
        });
        if self.end_if_superseded(&state, "timer read") {
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }
        if let Some(due) = poll_due {
            let registration = state.registration.load(Ordering::SeqCst);
            let installed = {
                let mut polls = self.inner.polls.lock().unwrap();
                // #1599: a concurrent registration on another thread can
                // install its poll between the ownership check above and this
                // insert. TS keeps one timer per request
                // (`GatewayUserDialogChannel.ts:130-133`), so only the newest
                // registration may own the entry; a late install from a
                // superseded one is ignored rather than resetting the
                // replacement's in-flight poll.
                let current = polls
                    .entries
                    .get(&request_id)
                    .map(|entry| entry.registration);
                if current.is_none_or(|current| current < registration) {
                    if current.is_none() {
                        polls.order.push(request_id.clone());
                    }
                    polls.entries.insert(
                        request_id.clone(),
                        PollEntry {
                            due_ms: due,
                            polling: false,
                            registration,
                        },
                    );
                    true
                } else {
                    false
                }
            };
            if installed {
                self.poll_once(&request_id, None);
            }
        }

        ChannelRequest {
            request_id,
            dialog_kind: dialog_kind.into(),
            state,
            receiver,
        }
    }

    fn persist(
        &self,
        state: &RequestState,
        request_event: &JsValue,
        stored: &StoredDialog,
    ) -> Result<()> {
        if let Some(journal) = &self.inner.options.journal {
            journal.record(request_event)?;
            // `record` is host code and may have registered a same-id
            // replacement. Writing this request's row now would overwrite the
            // replacement's persisted request (#1599).
            if replaced(&self.inner, state) {
                return Err(DialogError::Invalid(replaced_during("journal write")));
            }
        }
        if let (Some(store), Some(key)) = (&self.inner.options.store, &self.inner.options.store_key)
        {
            store.put(key, stored)?;
            // Same window one call later: the store hook may have published a
            // same-id replacement that already wrote its own row and lease.
            if replaced(&self.inner, state) {
                return Err(DialogError::Invalid(replaced_during("store write")));
            }
            if let Some(claim) = &self.inner.options.claim_stored_owner {
                let claimed = claim(&state.request_id, &state.owner_id)?;
                if !claimed {
                    return Err(DialogError::Invalid(format!(
                        "Gateway user-dialog owner lease was unavailable for {}.",
                        state.request_id.to_utf8().unwrap_or_default()
                    )));
                }
                state.owner_claimed.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    fn complete(
        &self,
        state: &Arc<RequestState>,
        answer: std::result::Result<DialogAnswer, String>,
    ) {
        if state.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        // A reentrant host callback may have registered a new request under the
        // same id; the poll registry then belongs to it and must survive this
        // (superseded) request's completion (#1599).
        if !self.superseded(state) {
            stop_polling(
                &self.inner,
                &state.request_id,
                state.registration.load(Ordering::SeqCst),
            );
        }
        if let Some(signal) = &state.abort {
            let token = state.abort_registration.swap(0, Ordering::SeqCst);
            if token != 0 {
                signal.remove_abort(token);
            }
        }
        let _ = state.result.send(answer);
        let mut requests = self.inner.requests.lock().unwrap();
        if requests
            .get(&state.request_id)
            .is_some_and(|current| Arc::ptr_eq(current, state))
        {
            requests.remove(&state.request_id);
        }
    }

    /// Whether a *different* request has taken over `state`'s id. An absent
    /// entry alone is not supersession — `complete` removes the entry before
    /// the settle hook runs, and that hook still owns the journal/store/lease
    /// cleanup — but the sticky `replaced` flag keeps a real supersession
    /// visible after the replacement has completed and left the table (#1599).
    fn superseded(&self, state: &Arc<RequestState>) -> bool {
        replaced(&self.inner, state)
    }

    /// Ends a registration that a reentrant host callback replaced under the
    /// same request id. The replacement owns the bus entry, so this waiter has
    /// nothing left to answer through; `begin` must not publish or poll for it
    /// (`bus.register` replaces the bucket by id, #1599).
    fn end_if_superseded(&self, state: &Arc<RequestState>, stage: &str) -> bool {
        if !self.superseded(state) {
            return false;
        }
        let _ = settle(&self.inner, state);
        self.complete(state, Err(replaced_during(stage)));
        true
    }

    pub fn abort(&self, request_id: &JsString) {
        self.cancel(request_id, "aborted");
    }

    fn cancel(&self, request_id: &JsString, reason: &str) {
        let state = self.inner.requests.lock().unwrap().get(request_id).cloned();
        let Some(state) = state else {
            return;
        };
        self.cancel_owned(&state, reason);
    }

    /// Cancels `state` only while it still owns its request id. The abort
    /// callback of a superseded registration must not consume the same-id
    /// replacement that owns the bus entry now (#1599).
    fn cancel_owned(&self, state: &Arc<RequestState>, reason: &str) {
        if self.superseded(state) {
            return;
        }
        if state.settled.load(Ordering::SeqCst) {
            return;
        }
        if !state.registered.load(Ordering::SeqCst) {
            // The request is between the persist gate and `registered = true`.
            // Only raise the flag: removing the state here would make the
            // post-registration cancel a no-op and leak the bus entry, owner
            // lease, journal record and store record. `begin` settles it once
            // the entry is registered.
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            return;
        }
        // #1442: the settle hook, the resolve callback and the change
        // observer are host code; none of them run under the bus guard. TS
        // `remove()` runs onSettled before its mutations, and a throw leaves
        // the dialog pending and broadcast-free (fail-closed,
        // GatewayUserDialogBus.ts:219-226), so a settle failure restores the
        // consumed entry instead of resolving.
        let (consumed, pre, outbox) = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            // `pre` holds changes queued before this consume (normally
            // empty here); a lease that expires inside `consume` lands in
            // `outbox`. A settle failure rolls back only the entry and its
            // own "settled" broadcast, so `outbox` is delivered through
            // `without_settled` (GatewayUserDialogBus.ts:219-226).
            let pre = bus.drain_events();
            let consumed = bus.consume(
                &self.inner.options.session_key,
                &state.request_id,
                "cancelled",
            );
            let outbox = bus.drain_events();
            (consumed, pre, outbox)
        };
        match consumed {
            Some(consumed) => {
                let settled = consumed
                    .entry
                    .on_settled
                    .as_ref()
                    .map_or(Ok(()), |hook| hook());
                if let Err(error) = settled {
                    pre.deliver();
                    outbox
                        .without_settled(&self.inner.options.session_key, &state.request_id)
                        .deliver();
                    let mut bus = self.inner.options.bus.lock().unwrap();
                    let replaced = bus.restore(&self.inner.options.session_key, consumed);
                    drop(bus);
                    // The request id now belongs to a reentrant registration;
                    // the old waiter cannot come back, so end it explicitly
                    // instead of leaving it pending forever (#1599).
                    if let Some(replaced) = replaced {
                        (replaced.reject)(replaced_message());
                    }
                    self.complete(state, Err(error.to_string()));
                    return;
                }
                pre.deliver();
                outbox.deliver();
                (consumed.entry.resolve)(DialogAnswer::Cancelled(Some(reason.into())));
            }
            None => {
                pre.deliver();
                outbox.deliver();
            }
        }
        let mut event = JsObject::new();
        event.insert("type".into(), "user_dialog_cancelled".into());
        event.insert(
            "requestId".into(),
            JsValue::String(state.request_id.clone()),
        );
        event.insert(
            "dialogKind".into(),
            JsValue::String(state.dialog_kind.clone()),
        );
        event.insert("reason".into(), JsValue::String(reason.into()));
        if let Err(error) = (self.inner.options.emit)(JsValue::Object(event)) {
            self.complete(state, Err(error.to_string()));
        }
    }

    /// Runs every poll whose 100 ms deadline has elapsed. The host drives this
    /// from its timer loop; tests drive it from an explicit clock.
    pub fn tick(&self) {
        let now = (self.inner.options.now_ms)();
        let interval = self.inner.options.poll_interval_ms.max(1);
        let due = {
            let mut polls = self.inner.polls.lock().unwrap();
            let order = polls.order.clone();
            let mut due = Vec::new();
            for id in &order {
                let Some(entry) = polls.entries.get_mut(id) else {
                    continue;
                };
                if entry.due_ms <= now {
                    entry.due_ms = now.saturating_add(interval);
                    // Carry the entry's identity: a host callback reached from
                    // another poll in this batch can replace the registration
                    // under the same id, and the replacement's own deadline
                    // must not be pulled forward by this stale due item
                    // (#1599).
                    due.push((id.clone(), entry.registration));
                }
            }
            due
        };
        for (id, registration) in due {
            self.poll_once(&id, Some(registration));
        }
    }

    fn poll_once(&self, request_id: &JsString, due_registration: Option<u64>) {
        let requests = self.inner.requests.lock().unwrap();
        let Some(state) = requests.get(request_id).cloned() else {
            // No registration owns this id, so any poll entry left for it is an
            // orphan. Remove it while the registration map is still held: a
            // registration publishes its state before it installs a poll entry,
            // so this lock keeps a concurrent registration from landing a live
            // entry between the check and the removal (#1599).
            let mut polls = self.inner.polls.lock().unwrap();
            polls.entries.remove(request_id);
            polls.order.retain(|id| id != request_id);
            return;
        };
        drop(requests);
        // #1599: a reentrant host callback may replace the registration while
        // this poll is in flight; every poll mutation below stays bound to the
        // registration captured here.
        let registration = state.registration.load(Ordering::SeqCst);
        // The bus numbers registrations in call order; this one is what
        // `consume` validates when the answer arrives.
        let generation = state.generation.load(Ordering::SeqCst);
        {
            let mut polls = self.inner.polls.lock().unwrap();
            let Some(entry) = polls.entries.get(request_id) else {
                return;
            };
            if due_registration.is_some_and(|due| entry.registration != due) {
                // `tick` collected this due item for an older registration and
                // a host callback replaced the dialog before the batch reached
                // it; the replacement keeps its own deadline (#1599).
                return;
            }
            let entry_registration = entry.registration;
            if entry_registration < registration {
                // The entry is left over from a superseded registration that
                // installed after the current waiter already gave up on its own
                // poll. TS pairs the timer with the request
                // (`GatewayUserDialogChannel.ts:130-133`), so an entry the
                // current registration never installed is dropped instead of
                // polled (#1599).
                polls.entries.remove(request_id);
                polls.order.retain(|id| id != request_id);
                return;
            }
            if entry_registration > registration {
                // A newer registration owns this entry; this pass is stale and
                // must not disturb its in-flight poll.
                return;
            }
            let Some(entry) = polls.entries.get_mut(request_id) else {
                return;
            };
            if entry.polling {
                return;
            }
            entry.polling = true;
        }
        if state.settled.load(Ordering::SeqCst) || !state.registered.load(Ordering::SeqCst) {
            stop_polling(&self.inner, request_id, registration);
            return;
        }
        if state.owner_claimed.load(Ordering::SeqCst)
            && let Some(renew) = &self.inner.options.renew_stored_owner
        {
            match renew(request_id, &state.owner_id) {
                Ok(true) => {}
                // A failed renewal stops this poll; the next tick retries.
                Ok(false) | Err(_) => {
                    finish_poll(&self.inner, request_id, registration);
                    return;
                }
            }
        }
        let answer = match &self.inner.options.take_stored_answer {
            Some(take) => match take(request_id) {
                Ok(answer) => answer,
                Err(_) => {
                    finish_poll(&self.inner, request_id, registration);
                    return;
                }
            },
            None => {
                finish_poll(&self.inner, request_id, registration);
                return;
            }
        };
        let Some(answer) = answer else {
            finish_poll(&self.inner, request_id, registration);
            return;
        };
        if state.settled.load(Ordering::SeqCst) {
            stop_polling(&self.inner, request_id, registration);
            return;
        }
        // #1442: the settle hook and the completion callbacks are host code;
        // they run only after the bus guard drops.
        let (consumed, pre, outbox, replaced) = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            // #1599: this answer was taken for the registration `state` owns
            // and is validated by `state.accepts` (dialog kind, schema). A
            // host callback above (`take_stored_answer`) may reenter the bus
            // and re-register the same request id with a different contract;
            // consuming that entry would hand the old answer to the new
            // waiter, so an unbound entry is left pending instead.
            let pre = bus.drain_events();
            let replaced =
                bus.pending_replaced(&self.inner.options.session_key, request_id, generation);
            let consumed = (!replaced).then(|| {
                bus.consume(
                    &self.inner.options.session_key,
                    request_id,
                    "cross_gateway_answer",
                )
            });
            let outbox = bus.drain_events();
            (consumed.flatten(), pre, outbox, replaced)
        };
        let consumed = match consumed {
            Some(consumed) => consumed,
            None => {
                pre.deliver();
                outbox.deliver();
                if replaced {
                    // The registration this poll answered is gone, so the
                    // taken answer has nowhere to go: fail the original waiter
                    // explicitly instead of leaving it pending forever
                    // (#1599). The replacement keeps its own poll/answer.
                    stop_polling(&self.inner, request_id, registration);
                    let _ = settle(&self.inner, &state);
                    self.complete(
                        &state,
                        Err("Gateway user-dialog was replaced while its \
                             stored answer was taken."
                            .into()),
                    );
                    return;
                }
                finish_poll(&self.inner, request_id, registration);
                return;
            }
        };
        // A failed settle hook restores the entry (TS remove() fail-closed,
        // GatewayUserDialogBus.ts:219-226) and discards its own queued
        // broadcast; changes that already happened inside the same consume
        // (an expired lease) still reach the renderer.
        let settled = consumed
            .entry
            .on_settled
            .as_ref()
            .map_or(Ok(()), |hook| hook());
        if settled.is_err() {
            pre.deliver();
            outbox
                .without_settled(&self.inner.options.session_key, request_id)
                .deliver();
            let mut bus = self.inner.options.bus.lock().unwrap();
            let replaced = bus.restore(&self.inner.options.session_key, consumed);
            drop(bus);
            if let Some(replaced) = replaced {
                (replaced.reject)(replaced_message());
            }
            finish_poll(&self.inner, request_id, registration);
            return;
        }
        pre.deliver();
        outbox.deliver();
        match &answer.result {
            DialogAnswer::Answered(value) => {
                if !(state.accepts)(value) {
                    (consumed.entry.reject)(format!(
                        "Cross-Gateway {} dialog result does not match the pending dialog contract.",
                        state.dialog_kind.to_utf8().unwrap_or_default()
                    ));
                    finish_poll(&self.inner, request_id, registration);
                    return;
                }
                (consumed.entry.resolve)(DialogAnswer::Answered(value.clone()));
            }
            DialogAnswer::Cancelled(reason) => {
                // TS drops a falsy cross-Gateway reason:
                // `...(answer.result.reason ? { reason } : {})`.
                let reason = reason.clone().filter(|reason| !reason.is_empty());
                (consumed.entry.resolve)(DialogAnswer::Cancelled(reason));
            }
        }
        finish_poll(&self.inner, request_id, registration);
    }
}

/// Rejection handed to a waiter whose request id a reentrant host callback
/// re-registered while this registration was still assembling itself (#1599).
fn replaced_during(stage: &str) -> String {
    format!("Gateway user-dialog was replaced while its {stage} was running.")
}

/// Rejection handed to a waiter whose request id a reentrant host callback
/// re-registered while the settle hook was failing (#1599).
fn replaced_message() -> String {
    "Gateway user-dialog was replaced while its settle hook failed.".into()
}

fn stop_polling(inner: &Inner, request_id: &JsString, registration: u64) {
    let mut polls = inner.polls.lock().unwrap();
    // TS keeps one timer per request (`GatewayUserDialogChannel.ts:130-133`),
    // so a request may only stop the poll its own registration installed. A
    // same-id replacement owns the entry now (#1599).
    if !polls
        .entries
        .get(request_id)
        .is_some_and(|entry| entry.registration == registration)
    {
        return;
    }
    polls.entries.remove(request_id);
    polls.order.retain(|id| id != request_id);
}
fn finish_poll(inner: &Inner, request_id: &JsString, registration: u64) {
    if let Some(entry) = inner.polls.lock().unwrap().entries.get_mut(request_id)
        && entry.registration == registration
    {
        entry.polling = false;
    }
}
/// Whether a *different* registration owns `state`'s request id. An absent
/// entry is not supersession: `complete` removes it before the settle hook
/// runs, and that hook still owns the journal/store/lease cleanup.
/// Whether a *different* registration owns `state`'s request id (or owned it
/// and has since completed). See `RequestState::replaced` (#1599).
fn replaced(inner: &Inner, state: &RequestState) -> bool {
    state.replaced.load(Ordering::SeqCst)
        || inner
            .requests
            .lock()
            .unwrap()
            .get(&state.request_id)
            .is_some_and(|current| !std::ptr::eq(Arc::as_ptr(current), state))
}

fn superseded_by_registration(inner: &Inner, state: &Arc<RequestState>) -> bool {
    replaced(inner, state)
}

fn settle(inner: &Inner, state: &Arc<RequestState>) -> Result<()> {
    // A superseded request must not delete the store/journal rows its
    // replacement wrote under the same id (#1599). An absent entry only means
    // `complete` already removed it; the cleanup below still belongs to this
    // request.
    if superseded_by_registration(inner, state) {
        return Ok(());
    }
    if state.owner_claimed.swap(false, Ordering::SeqCst)
        && let Some(release) = &inner.options.release_stored_owner
    {
        release(&state.request_id, &state.owner_id)?;
        // `release` is host code; a reentrant callback may have registered the
        // same id and written its own journal/store rows. Stop before deleting
        // the replacement's records (#1599).
        if superseded_by_registration(inner, state) {
            return Ok(());
        }
    }
    if let Some(journal) = &inner.options.journal {
        journal.remove(&state.request_id)?;
        // Same window as above, one destructive step later.
        if superseded_by_registration(inner, state) {
            return Ok(());
        }
    }
    if let (Some(store), Some(key)) = (&inner.options.store, &inner.options.store_key) {
        store.remove(key, &state.request_id)?;
    }
    Ok(())
}

fn base_event(
    dialog_kind: &str,
    request_id: &JsString,
    tool_call_id: &JsString,
    tool_name: &JsString,
    prompt: &JsString,
) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type".into(), "user_dialog_request".into());
    event.insert("requestId".into(), JsValue::String(request_id.clone()));
    event.insert("dialogKind".into(), dialog_kind.into());
    event.insert("toolCallId".into(), JsValue::String(tool_call_id.clone()));
    event.insert("toolName".into(), JsValue::String(tool_name.clone()));
    event.insert("prompt".into(), JsValue::String(prompt.clone()));
    JsValue::Object(event)
}

fn set(value: &mut JsValue, name: &str, field: JsValue) {
    if let JsValue::Object(object) = value {
        object.insert(name.into(), field);
    }
}
