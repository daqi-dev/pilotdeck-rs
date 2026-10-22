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
        atomic::{AtomicBool, Ordering},
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
type AbortCallbacks = Vec<Box<dyn Fn() + Send + Sync>>;

#[derive(Default)]
struct AbortInner {
    state: Mutex<(bool, AbortCallbacks)>,
}
impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn abort(&self) {
        let callbacks = {
            let mut state = self.inner.state.lock().unwrap();
            if state.0 {
                return;
            }
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        for callback in callbacks {
            callback();
        }
    }
    pub fn is_aborted(&self) -> bool {
        self.inner.state.lock().unwrap().0
    }
    fn on_abort(&self, callback: Box<dyn Fn() + Send + Sync>) {
        let mut state = self.inner.state.lock().unwrap();
        if !state.0 {
            state.1.push(callback);
        }
    }
    fn clear(&self) {
        self.inner.state.lock().unwrap().1.clear();
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
    accepts: Accepts,
    abort: Option<AbortSignal>,
    result: mpsc::Sender<std::result::Result<DialogAnswer, String>>,
}

struct PollEntry {
    due_ms: i64,
    polling: bool,
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
            accepts,
            abort: abort.clone(),
            result: sender,
        });
        self.inner
            .requests
            .lock()
            .unwrap()
            .insert(request_id.clone(), state.clone());

        if let Some(signal) = &abort
            && !signal.is_aborted()
        {
            let channel = self.clone();
            let id = request_id.clone();
            signal.on_abort(Box::new(move || channel.cancel(&id, "aborted")));
        }

        let stored = StoredDialog {
            request: request_event.clone(),
            created_at: format_epoch_iso((self.inner.options.now_ms)()).into(),
        };
        if let Err(error) = self.persist(&state, &request_event, &stored) {
            self.complete(&state, Err(error.to_string()));
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
            let settled = state.settled.load(Ordering::SeqCst);
            if let Err(error) = settle(&self.inner, &state) {
                if !settled {
                    self.complete(&state, Err(error.to_string()));
                }
                return ChannelRequest {
                    request_id,
                    dialog_kind: dialog_kind.into(),
                    state,
                    receiver,
                };
            }
            if !state.settled.load(Ordering::SeqCst) {
                self.complete(&state, Ok(DialogAnswer::Cancelled(Some("aborted".into()))));
            }
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
            accepts: {
                let accepts = state.accepts.clone();
                Box::new(move |value| accepts(value))
            },
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
                    stop_polling(&channel.inner, &state.request_id);
                    let _ = settle(&channel.inner, &state);
                    Ok(())
                }))
            },
        };
        {
            let mut bus = self.inner.options.bus.lock().unwrap();
            bus.register(&self.inner.options.session_key, pending);
        }
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

        if self.inner.options.take_stored_answer.is_some() {
            let due = (self.inner.options.now_ms)()
                .saturating_add(self.inner.options.poll_interval_ms.max(1));
            {
                let mut polls = self.inner.polls.lock().unwrap();
                if !polls.entries.contains_key(&request_id) {
                    polls.order.push(request_id.clone());
                }
                polls.entries.insert(
                    request_id.clone(),
                    PollEntry {
                        due_ms: due,
                        polling: false,
                    },
                );
            }
            self.poll_once(&request_id);
        }

        if let Some(signal) = &abort
            && signal.is_aborted()
        {
            self.cancel(&request_id, "aborted");
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
        }
        if let (Some(store), Some(key)) = (&self.inner.options.store, &self.inner.options.store_key)
        {
            store.put(key, stored)?;
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

    fn complete(&self, state: &RequestState, answer: std::result::Result<DialogAnswer, String>) {
        if state.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        stop_polling(&self.inner, &state.request_id);
        if let Some(signal) = &state.abort {
            signal.clear();
        }
        let _ = state.result.send(answer);
        self.inner
            .requests
            .lock()
            .unwrap()
            .remove(&state.request_id);
    }

    pub fn abort(&self, request_id: &JsString) {
        self.cancel(request_id, "aborted");
    }

    fn cancel(&self, request_id: &JsString, reason: &str) {
        let state = self.inner.requests.lock().unwrap().get(request_id).cloned();
        let Some(state) = state else {
            return;
        };
        if state.settled.load(Ordering::SeqCst) {
            return;
        }
        if !state.registered.load(Ordering::SeqCst) {
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            if !state.settled.load(Ordering::SeqCst) {
                self.complete(&state, Ok(DialogAnswer::Cancelled(Some(reason.into()))));
            }
            return;
        }
        let entry = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            bus.consume(&self.inner.options.session_key, request_id, "cancelled")
        };
        match entry {
            Ok(Some(entry)) => (entry.resolve)(DialogAnswer::Cancelled(Some(reason.into()))),
            Ok(None) => {}
            Err(error) => {
                if let Some(signal) = &state.abort {
                    signal.clear();
                }
                self.complete(&state, Err(error.to_string()));
                return;
            }
        }
        let mut event = JsObject::new();
        event.insert("type".into(), "user_dialog_cancelled".into());
        event.insert("requestId".into(), JsValue::String(request_id.clone()));
        event.insert(
            "dialogKind".into(),
            JsValue::String(state.dialog_kind.clone()),
        );
        event.insert("reason".into(), JsValue::String(reason.into()));
        if let Err(error) = (self.inner.options.emit)(JsValue::Object(event)) {
            if let Some(signal) = &state.abort {
                signal.clear();
            }
            self.complete(&state, Err(error.to_string()));
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
                    due.push(id.clone());
                }
            }
            due
        };
        for id in due {
            self.poll_once(&id);
        }
    }

    fn poll_once(&self, request_id: &JsString) {
        {
            let mut polls = self.inner.polls.lock().unwrap();
            let Some(entry) = polls.entries.get_mut(request_id) else {
                return;
            };
            if entry.polling {
                return;
            }
            entry.polling = true;
        }
        let state = self.inner.requests.lock().unwrap().get(request_id).cloned();
        let Some(state) = state else {
            finish_poll(&self.inner, request_id);
            return;
        };
        if state.settled.load(Ordering::SeqCst) || !state.registered.load(Ordering::SeqCst) {
            finish_poll(&self.inner, request_id);
            return;
        }
        if state.owner_claimed.load(Ordering::SeqCst)
            && let Some(renew) = &self.inner.options.renew_stored_owner
        {
            match renew(request_id, &state.owner_id) {
                Ok(true) => {}
                // A failed renewal stops this poll; the next tick retries.
                Ok(false) | Err(_) => {
                    finish_poll(&self.inner, request_id);
                    return;
                }
            }
        }
        let answer = match &self.inner.options.take_stored_answer {
            Some(take) => match take(request_id) {
                Ok(answer) => answer,
                Err(_) => {
                    finish_poll(&self.inner, request_id);
                    return;
                }
            },
            None => {
                finish_poll(&self.inner, request_id);
                return;
            }
        };
        let Some(answer) = answer else {
            finish_poll(&self.inner, request_id);
            return;
        };
        if state.settled.load(Ordering::SeqCst) {
            finish_poll(&self.inner, request_id);
            return;
        }
        let entry = {
            let mut bus = self.inner.options.bus.lock().unwrap();
            bus.consume(
                &self.inner.options.session_key,
                request_id,
                "cross_gateway_answer",
            )
        };
        let entry = match entry {
            Ok(Some(entry)) => entry,
            _ => {
                finish_poll(&self.inner, request_id);
                return;
            }
        };
        match &answer.result {
            DialogAnswer::Answered(value) => {
                if !(state.accepts)(value) {
                    (entry.reject)(format!(
                        "Cross-Gateway {} dialog result does not match the pending dialog contract.",
                        state.dialog_kind.to_utf8().unwrap_or_default()
                    ));
                    finish_poll(&self.inner, request_id);
                    return;
                }
                (entry.resolve)(DialogAnswer::Answered(value.clone()));
            }
            DialogAnswer::Cancelled(reason) => {
                (entry.resolve)(DialogAnswer::Cancelled(reason.clone()));
            }
        }
        finish_poll(&self.inner, request_id);
    }
}

fn stop_polling(inner: &Inner, request_id: &JsString) {
    let mut polls = inner.polls.lock().unwrap();
    polls.entries.remove(request_id);
    polls.order.retain(|id| id != request_id);
}
fn finish_poll(inner: &Inner, request_id: &JsString) {
    if let Some(entry) = inner.polls.lock().unwrap().entries.get_mut(request_id) {
        entry.polling = false;
    }
}
fn settle(inner: &Inner, state: &RequestState) -> Result<()> {
    if state.owner_claimed.swap(false, Ordering::SeqCst)
        && let Some(release) = &inner.options.release_stored_owner
    {
        release(&state.request_id, &state.owner_id)?;
    }
    if let Some(journal) = &inner.options.journal {
        journal.remove(&state.request_id)?;
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
