//! Replay of the real TypeScript user-dialog journal/channel oracle.
//!
//! Every step compares the exact JS value text (key order, number spelling,
//! lone surrogates), the journal bytes, the store/owner snapshots, callback
//! order and pending bus state. Clock and timers are explicit: `tick()` is
//! driven by an atomic clock and never sleeps.

use super::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::system_clock::format_epoch_iso;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc,
    },
};

fn fixture() -> JsValue {
    let oracle = include_str!("../../tests/fixtures/user-dialog-channel/oracle.json");
    assert!(
        !oracle.contains(".channel-") && !oracle.contains(".journal-"),
        "oracle.json leaks a generation path"
    );
    crate::test_baseline::oracle(oracle)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-udc-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or_else(|| panic!("missing {name}"))
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn text_field(value: &JsValue, name: &str) -> String {
    text(field(value, name))
}
fn js_string(value: &JsValue) -> JsString {
    value.as_string().unwrap().clone()
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("not an array: {value:?}"),
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}
fn bool_field(value: &JsValue, name: &str, default: bool) -> bool {
    value
        .get(name)
        .and_then(JsValue::as_bool)
        .unwrap_or(default)
}
fn number_field(value: &JsValue, name: &str, default: f64) -> f64 {
    value
        .get(name)
        .and_then(JsValue::as_number)
        .unwrap_or(default)
}
fn optional_id(value: &JsValue, name: &str) -> Option<JsString> {
    value.get(name).and_then(JsValue::as_string).cloned()
}
fn default_prompt() -> JsString {
    let mut units: Vec<u16> = "继续？".encode_utf16().collect();
    units.push(0xD800);
    JsString::from_utf16(units)
}
fn runtime(clock: &Arc<AtomicI64>, uuid: &'static str) -> DialogRuntime {
    let clock = clock.clone();
    DialogRuntime {
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
        uuid: Arc::new(move || uuid.to_owned()),
    }
}
fn equal(actual: &JsValue, expected: &JsValue, context: &str) {
    assert_eq!(
        stringify_json(actual).unwrap(),
        stringify_json(expected).unwrap(),
        "{context}"
    );
}
fn result_value(result: Result<JsValue>) -> JsValue {
    result.unwrap_or_else(|error| object([("error", JsValue::from(normalize_error(&error)))]))
}
fn normalize_error(error: &DialogError) -> &str {
    match error {
        DialogError::Io(_) => "<io>",
        DialogError::Invalid(message)
            if message.starts_with("Unable to read Gateway user-dialog journal ") =>
        {
            "journal-read"
        }
        DialogError::Invalid(message) => message,
    }
}

#[derive(Default)]
struct Logs {
    journal: Mutex<Vec<JsValue>>,
    store: Mutex<Vec<JsValue>>,
    owner: Mutex<Vec<JsValue>>,
    take: Mutex<Vec<JsValue>>,
    emit: Mutex<Vec<JsValue>>,
    bus: Mutex<Vec<JsValue>>,
    uuid: Mutex<Vec<JsValue>>,
}
fn log(target: &Mutex<Vec<JsValue>>, entry: JsValue) {
    target.lock().unwrap().push(entry);
}
impl Logs {
    fn push_journal(&self, entry: JsValue) {
        log(&self.journal, entry);
    }
    fn snapshot(&self) -> JsValue {
        object([
            (
                "journal",
                JsValue::Array(self.journal.lock().unwrap().clone()),
            ),
            ("store", JsValue::Array(self.store.lock().unwrap().clone())),
            ("owner", JsValue::Array(self.owner.lock().unwrap().clone())),
            ("take", JsValue::Array(self.take.lock().unwrap().clone())),
            ("emit", JsValue::Array(self.emit.lock().unwrap().clone())),
            ("bus", JsValue::Array(self.bus.lock().unwrap().clone())),
            ("uuid", JsValue::Array(self.uuid.lock().unwrap().clone())),
        ])
    }
}

struct LoggingJournal {
    actual: GatewayUserDialogJournal,
    logs: Arc<Logs>,
    record_fail: AtomicBool,
    remove_fail: AtomicBool,
}
impl LoggingJournal {
    fn recover(&self) -> Result<Vec<JsValue>> {
        self.actual.recover()
    }
}
impl UserDialogJournal for LoggingJournal {
    fn record(&self, event: &JsValue) -> Result<()> {
        self.logs
            .push_journal(JsValue::Array(vec!["record".into(), event.clone()]));
        if self.record_fail.load(Ordering::SeqCst) {
            return Err(DialogError::Invalid("journal record failed".into()));
        }
        self.actual.record(event)
    }
    fn remove(&self, request_id: &JsString) -> Result<()> {
        self.logs.push_journal(JsValue::Array(vec![
            "remove".into(),
            JsValue::String(request_id.clone()),
        ]));
        if self.remove_fail.load(Ordering::SeqCst) {
            return Err(DialogError::Invalid("journal remove failed".into()));
        }
        self.actual.remove(request_id)
    }
}

struct LoggingStore {
    logs: Arc<Logs>,
    dialogs: Arc<Mutex<Vec<(JsString, StoredDialog)>>>,
    put_fail: AtomicBool,
    remove_fail: AtomicBool,
    abort_on_put: AtomicBool,
    signal: Arc<Mutex<Option<AbortSignal>>>,
}
impl UserDialogStore for LoggingStore {
    fn put(&self, _key: &StoreKey, dialog: &StoredDialog) -> Result<()> {
        log(
            &self.logs.store,
            JsValue::Array(vec!["put".into(), dialog.to_value()]),
        );
        if self.abort_on_put.load(Ordering::SeqCst)
            && let Some(signal) = self.signal.lock().unwrap().clone()
        {
            signal.abort();
        }
        if self.put_fail.load(Ordering::SeqCst) {
            return Err(DialogError::Invalid("store put failed".into()));
        }
        let id = dialog
            .request
            .get("requestId")
            .and_then(JsValue::as_string)
            .expect("dialog id")
            .clone();
        let mut dialogs = self.dialogs.lock().unwrap();
        if let Some(index) = dialogs.iter().position(|(candidate, _)| candidate == &id) {
            dialogs[index].1 = dialog.clone();
        } else {
            dialogs.push((id, dialog.clone()));
        }
        Ok(())
    }
    fn remove(&self, _key: &StoreKey, request_id: &JsString) -> Result<()> {
        log(
            &self.logs.store,
            JsValue::Array(vec!["remove".into(), JsValue::String(request_id.clone())]),
        );
        if self.remove_fail.load(Ordering::SeqCst) {
            return Err(DialogError::Invalid("store remove failed".into()));
        }
        self.dialogs
            .lock()
            .unwrap()
            .retain(|(candidate, _)| candidate != request_id);
        Ok(())
    }
}

#[derive(Clone)]
struct OwnerEntry {
    owner_id: JsString,
    expires_at: i64,
}
struct OwnerHarness {
    logs: Arc<Logs>,
    owners: Mutex<Vec<(JsString, OwnerEntry)>>,
    claim_mode: Mutex<String>,
    renew_mode: Mutex<String>,
    release_mode: Mutex<String>,
    ttl_ms: AtomicI64,
    clock: Arc<AtomicI64>,
}
impl OwnerHarness {
    fn claim(&self, request_id: &JsString, owner_id: &JsString) -> Result<bool> {
        log(
            &self.logs.owner,
            JsValue::Array(vec![
                "claim".into(),
                JsValue::String(request_id.clone()),
                JsValue::String(owner_id.clone()),
            ]),
        );
        match self.claim_mode.lock().unwrap().as_str() {
            "throw" => return Err(DialogError::Invalid("claim exploded".into())),
            "false" => return Ok(false),
            _ => {}
        }
        let now = self.clock.load(Ordering::SeqCst);
        let mut owners = self.owners.lock().unwrap();
        if let Some((_, current)) = owners.iter().find(|(id, _)| id == request_id)
            && current.expires_at > now
            && current.owner_id != *owner_id
        {
            return Ok(false);
        }
        let entry = OwnerEntry {
            owner_id: owner_id.clone(),
            expires_at: now + self.ttl_ms.load(Ordering::SeqCst),
        };
        if let Some(index) = owners.iter().position(|(id, _)| id == request_id) {
            owners[index].1 = entry;
        } else {
            owners.push((request_id.clone(), entry));
        }
        Ok(true)
    }
    fn renew(&self, request_id: &JsString, owner_id: &JsString) -> Result<bool> {
        log(
            &self.logs.owner,
            JsValue::Array(vec![
                "renew".into(),
                JsValue::String(request_id.clone()),
                JsValue::String(owner_id.clone()),
            ]),
        );
        match self.renew_mode.lock().unwrap().as_str() {
            "throw" => return Err(DialogError::Invalid("renew exploded".into())),
            "false" => return Ok(false),
            _ => {}
        }
        let now = self.clock.load(Ordering::SeqCst);
        let mut owners = self.owners.lock().unwrap();
        let Some((_, current)) = owners.iter_mut().find(|(id, _)| id == request_id) else {
            return Ok(false);
        };
        if current.owner_id != *owner_id || current.expires_at <= now {
            return Ok(false);
        }
        current.expires_at = now + self.ttl_ms.load(Ordering::SeqCst);
        Ok(true)
    }
    fn release(&self, request_id: &JsString, owner_id: &JsString) -> Result<bool> {
        log(
            &self.logs.owner,
            JsValue::Array(vec![
                "release".into(),
                JsValue::String(request_id.clone()),
                JsValue::String(owner_id.clone()),
            ]),
        );
        if self.release_mode.lock().unwrap().as_str() == "throw" {
            return Err(DialogError::Invalid("release exploded".into()));
        }
        let mut owners = self.owners.lock().unwrap();
        let Some(index) = owners
            .iter()
            .position(|(id, current)| id == request_id && current.owner_id == *owner_id)
        else {
            return Ok(false);
        };
        owners.remove(index);
        Ok(true)
    }
}

struct AnswerHarness {
    logs: Arc<Logs>,
    answers: Mutex<Vec<(JsString, StoredAnswer)>>,
    take_throws: AtomicBool,
}
impl AnswerHarness {
    fn take(&self, request_id: &JsString) -> Result<Option<StoredAnswer>> {
        log(
            &self.logs.take,
            JsValue::Array(vec![JsValue::String(request_id.clone())]),
        );
        if self.take_throws.load(Ordering::SeqCst) {
            return Err(DialogError::Invalid("take failed".into()));
        }
        let mut answers = self.answers.lock().unwrap();
        let Some(index) = answers.iter().position(|(id, _)| id == request_id) else {
            return Ok(None);
        };
        let (_, answer) = answers.remove(index);
        Ok(Some(answer))
    }
}

struct Config {
    journal: bool,
    store: bool,
    owner: bool,
    take: bool,
    journal_record_fail: AtomicBool,
    journal_remove_fail: AtomicBool,
    store_put_fail: AtomicBool,
    store_remove_fail: AtomicBool,
    emit_fail: AtomicBool,
    bus_observer_fail: AtomicBool,
    claim: Mutex<String>,
    renew: Mutex<String>,
    release: Mutex<String>,
    take_mode: Mutex<String>,
    abort_on_put: AtomicBool,
    abort_on_register: AtomicBool,
    owner_ttl_ms: AtomicI64,
}
impl Config {
    fn from_value(value: &JsValue) -> Self {
        let mode = |name: &str| {
            Mutex::new(
                value
                    .get(name)
                    .and_then(JsValue::as_string)
                    .map(|value| value.to_utf8().unwrap())
                    .unwrap_or_else(|| "ok".into()),
            )
        };
        Self {
            journal: bool_field(value, "journal", true),
            store: bool_field(value, "store", true),
            owner: bool_field(value, "owner", true),
            take: bool_field(value, "take", true),
            journal_record_fail: AtomicBool::new(bool_field(value, "journalRecordFail", false)),
            journal_remove_fail: AtomicBool::new(bool_field(value, "journalRemoveFail", false)),
            store_put_fail: AtomicBool::new(bool_field(value, "storePutFail", false)),
            store_remove_fail: AtomicBool::new(bool_field(value, "storeRemoveFail", false)),
            emit_fail: AtomicBool::new(bool_field(value, "emitFail", false)),
            bus_observer_fail: AtomicBool::new(bool_field(value, "busObserverFail", false)),
            claim: mode("claim"),
            renew: mode("renew"),
            release: mode("release"),
            take_mode: mode("takeMode"),
            abort_on_put: AtomicBool::new(bool_field(value, "abortOnPut", false)),
            abort_on_register: AtomicBool::new(bool_field(value, "abortOnRegister", false)),
            owner_ttl_ms: AtomicI64::new(number_field(value, "ownerTtlMs", 5000.0) as i64),
        }
    }
}

struct Slot {
    request: ChannelRequest,
    outcome: Option<JsValue>,
}

fn request_parts(action: &JsValue) -> (&JsValue, &JsValue) {
    (field(action, "kind"), field(action, "request"))
}

fn build_request(kind: &str, request: &JsValue, signal: Option<AbortSignal>) -> RequestKind {
    let tool_call_id = request
        .get("toolCallId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| "tool-1".into());
    let tool_name = request
        .get("toolName")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| format!("request_{kind}").into());
    let prompt = request
        .get("prompt")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(default_prompt);
    match kind {
        "input" => RequestKind::Input(InputRequest {
            tool_call_id,
            tool_name,
            prompt,
            placeholder: optional_id(request, "placeholder"),
            allow_empty: bool_field(request, "allowEmpty", false),
            abort: signal,
        }),
        "select" => RequestKind::Select(SelectRequest {
            tool_call_id,
            tool_name,
            prompt,
            choices: array(field(request, "choices"))
                .iter()
                .map(|choice| SelectChoice {
                    value: js_string(field(choice, "value")),
                    label: optional_id(choice, "label"),
                    description: optional_id(choice, "description"),
                })
                .collect(),
            default_value: optional_id(request, "defaultValue"),
            abort: signal,
        }),
        "confirm" => RequestKind::Confirm(ConfirmRequest {
            tool_call_id,
            tool_name,
            prompt,
            confirm_label: optional_id(request, "confirmLabel"),
            cancel_label: optional_id(request, "cancelLabel"),
            default_value: request.get("defaultValue").and_then(JsValue::as_bool),
            abort: signal,
        }),
        "form" => RequestKind::Form(FormRequest {
            tool_call_id,
            tool_name,
            prompt,
            schema: field(request, "schema").clone(),
            abort: signal,
        }),
        _ => panic!("unknown request kind {kind}"),
    }
}
enum RequestKind {
    Input(InputRequest),
    Select(SelectRequest),
    Confirm(ConfirmRequest),
    Form(FormRequest),
}

fn replay_channel_case(oracle: &JsValue, case: &JsValue) {
    let name = text_field(case, "name");
    let clock = Arc::new(AtomicI64::new(number_field(oracle, "initialMs", 0.0) as i64));
    let scratch = Scratch::new();
    let journal_path = scratch.0.join("sessions/transcript.jsonl.dialogs.json");
    let logs = Arc::new(Logs::default());
    let config = Arc::new(Config::from_value(field(case, "config")));
    let actual_journal = GatewayUserDialogJournal::new(
        journal_path.clone(),
        JournalOptions {
            runtime: runtime(&clock, "journal-tmp"),
        },
    );
    let logging_journal = Arc::new(LoggingJournal {
        actual: actual_journal,
        logs: logs.clone(),
        record_fail: AtomicBool::new(false),
        remove_fail: AtomicBool::new(false),
    });
    let dialogs = Arc::new(Mutex::new(Vec::new()));
    let current_signal = Arc::new(Mutex::new(None::<AbortSignal>));
    logging_journal.record_fail.store(
        config.journal_record_fail.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    logging_journal.remove_fail.store(
        config.journal_remove_fail.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    let logging_store = Arc::new(LoggingStore {
        logs: logs.clone(),
        dialogs: dialogs.clone(),
        put_fail: AtomicBool::new(config.store_put_fail.load(Ordering::SeqCst)),
        remove_fail: AtomicBool::new(config.store_remove_fail.load(Ordering::SeqCst)),
        abort_on_put: AtomicBool::new(config.abort_on_put.load(Ordering::SeqCst)),
        signal: current_signal.clone(),
    });
    let owners = Arc::new(OwnerHarness {
        logs: logs.clone(),
        owners: Mutex::new(Vec::new()),
        claim_mode: Mutex::new(config.claim.lock().unwrap().clone()),
        renew_mode: Mutex::new(config.renew.lock().unwrap().clone()),
        release_mode: Mutex::new(config.release.lock().unwrap().clone()),
        ttl_ms: AtomicI64::new(config.owner_ttl_ms.load(Ordering::SeqCst)),
        clock: clock.clone(),
    });
    let answers = Arc::new(AnswerHarness {
        logs: logs.clone(),
        answers: Mutex::new(Vec::new()),
        take_throws: AtomicBool::new(config.take_mode.lock().unwrap().as_str() == "throw"),
    });
    let bus_observer = {
        let logs = logs.clone();
        let config = config.clone();
        let current_signal = current_signal.clone();
        Arc::new(Mutex::new(GatewayUserDialogBus::new(
            runtime(&clock, "bus-lease"),
            Some(Box::new(move |event| {
                log(&logs.bus, event.clone());
                if config.bus_observer_fail.load(Ordering::SeqCst) {
                    return Err(DialogError::Invalid("observer offline".into()));
                }
                if config.abort_on_register.load(Ordering::SeqCst)
                    && event
                        .get("type")
                        .and_then(JsValue::as_string)
                        .is_some_and(|value| value.equals_str("requested"))
                    && let Some(signal) = current_signal.lock().unwrap().clone()
                {
                    signal.abort();
                }
                Ok(())
            })),
        )))
    };
    let session: JsString = "session".into();
    let mut options = ChannelOptions::new(session.clone(), bus_observer.clone(), {
        let logs = logs.clone();
        let config = config.clone();
        Arc::new(move |event| {
            log(&logs.emit, event);
            if config.emit_fail.load(Ordering::SeqCst) {
                Err(DialogError::Invalid("emit failed".into()))
            } else {
                Ok(())
            }
        })
    });
    options.journal = if config.journal {
        Some(logging_journal.clone())
    } else {
        None
    };
    options.store = if config.store {
        Some(logging_store.clone())
    } else {
        None
    };
    options.store_key = Some(StoreKey {
        project_root: "/fixture/project".into(),
        pilot_home: "/fixture/home".into(),
        session_id: "session".into(),
    });
    options.take_stored_answer = if config.take {
        let answers = answers.clone();
        Some(Arc::new(move |id: &JsString| answers.take(id)))
    } else {
        None
    };
    if config.owner {
        let harness = owners.clone();
        options.claim_stored_owner = Some(Arc::new(move |id: &JsString, owner: &JsString| {
            harness.claim(id, owner)
        }));
        let harness = owners.clone();
        options.renew_stored_owner = Some(Arc::new(move |id: &JsString, owner: &JsString| {
            harness.renew(id, owner)
        }));
        let harness = owners.clone();
        options.release_stored_owner = Some(Arc::new(move |id: &JsString, owner: &JsString| {
            harness.release(id, owner)
        }));
    }
    options.uuid = {
        let logs = logs.clone();
        Arc::new(move || {
            let id = format!("uuid-{}", logs.uuid.lock().unwrap().len() + 1);
            log(&logs.uuid, JsValue::String(id.clone().into()));
            id
        })
    };
    options.now_ms = {
        let clock = clock.clone();
        Arc::new(move || clock.load(Ordering::SeqCst))
    };
    options.poll_interval_ms = number_field(oracle, "pollIntervalMs", 100.0) as i64;
    options.form_accepts = Some(Arc::new(|schema: &JsValue, value: &JsValue| {
        let _ = schema;
        value.get("name").and_then(JsValue::as_string).is_some()
    }));
    let channel = GatewayUserDialogChannel::new(options);
    let mut slots: Vec<(String, Slot)> = Vec::new();
    let mut signal: Option<AbortSignal> = None;
    let mut signals: Vec<(String, AbortSignal)> = Vec::new();

    for (index, step) in array(field(case, "steps")).iter().enumerate() {
        let action = field(step, "action");
        let outcome = (|| -> Result<JsValue> {
            Ok(match text_field(action, "op").as_str() {
                "begin" => {
                    let (kind, request) = request_parts(action);
                    let kind = text(kind);
                    let use_signal = bool_field(action, "signal", false);
                    if use_signal {
                        let created = match action
                            .get("signalName")
                            .and_then(JsValue::as_string)
                            .map(|value| value.to_utf8().unwrap())
                        {
                            Some(name) => {
                                match signals.iter().find(|(candidate, _)| candidate == &name) {
                                    Some((_, existing)) => existing.clone(),
                                    None => {
                                        let created = AbortSignal::new();
                                        signals.push((name, created.clone()));
                                        created
                                    }
                                }
                            }
                            None => AbortSignal::new(),
                        };
                        *current_signal.lock().unwrap() = Some(created.clone());
                        signal = Some(created);
                    }
                    let slot_name = action
                        .get("slot")
                        .and_then(JsValue::as_string)
                        .map(|value| value.to_utf8().unwrap())
                        .unwrap_or_else(|| "default".into());
                    let built = build_request(&kind, request, signal.clone());
                    let handle = match built {
                        RequestKind::Input(request) => channel.request_input(&request),
                        RequestKind::Select(request) => channel.request_select(&request),
                        RequestKind::Confirm(request) => channel.request_confirm(&request),
                        RequestKind::Form(request) => channel.request_form(&request),
                    };
                    slots.push((
                        slot_name,
                        Slot {
                            request: handle,
                            outcome: None,
                        },
                    ));
                    JsValue::Null
                }
                "abort" => {
                    let named = action
                        .get("signalName")
                        .and_then(JsValue::as_string)
                        .map(|value| value.to_utf8().unwrap());
                    let target = match named {
                        Some(name) => signals
                            .iter()
                            .find(|(candidate, _)| candidate == &name)
                            .map(|(_, signal)| signal.clone())
                            .expect("named abort signal"),
                        None => signal.clone().expect("abort signal"),
                    };
                    target.abort();
                    JsValue::Null
                }
                "tick" => {
                    let ms = number_field(action, "ms", 0.0) as i64;
                    clock.fetch_add(ms, Ordering::SeqCst);
                    channel.tick();
                    JsValue::Number(clock.load(Ordering::SeqCst) as f64)
                }
                "set" => {
                    let patch = field(action, "config");
                    if let Some(value) = patch.get("journalRecordFail").and_then(JsValue::as_bool) {
                        config.journal_record_fail.store(value, Ordering::SeqCst);
                        logging_journal.record_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("journalRemoveFail").and_then(JsValue::as_bool) {
                        config.journal_remove_fail.store(value, Ordering::SeqCst);
                        logging_journal.remove_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("storePutFail").and_then(JsValue::as_bool) {
                        config.store_put_fail.store(value, Ordering::SeqCst);
                        logging_store.put_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("storeRemoveFail").and_then(JsValue::as_bool) {
                        config.store_remove_fail.store(value, Ordering::SeqCst);
                        logging_store.remove_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("emitFail").and_then(JsValue::as_bool) {
                        config.emit_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("busObserverFail").and_then(JsValue::as_bool) {
                        config.bus_observer_fail.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("abortOnPut").and_then(JsValue::as_bool) {
                        config.abort_on_put.store(value, Ordering::SeqCst);
                        logging_store.abort_on_put.store(value, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("ownerTtlMs").and_then(JsValue::as_number) {
                        config.owner_ttl_ms.store(value as i64, Ordering::SeqCst);
                        owners.ttl_ms.store(value as i64, Ordering::SeqCst);
                    }
                    if let Some(value) = patch.get("claim").and_then(JsValue::as_string) {
                        *owners.claim_mode.lock().unwrap() = value.to_utf8().unwrap();
                    }
                    if let Some(value) = patch.get("renew").and_then(JsValue::as_string) {
                        *owners.renew_mode.lock().unwrap() = value.to_utf8().unwrap();
                    }
                    if let Some(value) = patch.get("release").and_then(JsValue::as_string) {
                        *owners.release_mode.lock().unwrap() = value.to_utf8().unwrap();
                    }
                    if let Some(value) = patch.get("takeMode").and_then(JsValue::as_string) {
                        answers
                            .take_throws
                            .store(value.equals_str("throw"), Ordering::SeqCst);
                    }
                    JsValue::Null
                }
                "claim" => {
                    let id = slot_id(action, &slots);
                    let ttl = number_field(action, "ttl", 1000.0);
                    let lease = optional_id(action, "lease");
                    let mut bus = bus_observer.lock().unwrap();
                    claim_value(bus.claim(&session, &id, ttl, lease.as_ref())?)
                }
                "releaseBus" => {
                    let id = slot_id(action, &slots);
                    let lease = optional_id(action, "lease").expect("release lease");
                    let mut bus = bus_observer.lock().unwrap();
                    JsValue::Bool(bus.release(&session, &id, &lease))
                }
                "check" => {
                    let id = slot_id(action, &slots);
                    let value = field(action, "value");
                    let bus = bus_observer.lock().unwrap();
                    let accepted = bus
                        .peek(&session, &id)
                        .map_or(JsValue::Null, |entry| JsValue::Bool((entry.accepts)(value)));
                    let pending = bus.pending_count(&session);
                    object([
                        ("accepted", accepted),
                        ("pending", JsValue::Number(pending as f64)),
                    ])
                }
                "respond" => {
                    let id = slot_id(action, &slots);
                    let lease = optional_id(action, "lease");
                    let cancel = bool_field(action, "cancel", false);
                    let consumed = {
                        let mut bus = bus_observer.lock().unwrap();
                        bus.consume_for_response(
                            &session,
                            &id,
                            lease.as_ref(),
                            if cancel { "cancelled" } else { "answered" },
                        )?
                    };
                    match consumed {
                        ConsumeResult::NotPending => object([
                            ("consumed", JsValue::Bool(false)),
                            ("reason", "not_pending".into()),
                        ]),
                        ConsumeResult::LeaseRequired => object([
                            ("consumed", JsValue::Bool(false)),
                            ("reason", "lease_required".into()),
                        ]),
                        ConsumeResult::Entry(entry) => {
                            if cancel {
                                let reason = optional_id(action, "reason");
                                (entry.resolve)(DialogAnswer::Cancelled(reason));
                                object([
                                    ("consumed", JsValue::Bool(true)),
                                    ("cancelled", JsValue::Bool(true)),
                                ])
                            } else {
                                (entry.resolve)(DialogAnswer::Answered(
                                    field(action, "value").clone(),
                                ));
                                object([
                                    ("consumed", JsValue::Bool(true)),
                                    ("answered", JsValue::Bool(true)),
                                ])
                            }
                        }
                    }
                }
                "submitAnswer" => {
                    let id = slot_id(action, &slots);
                    let result = DialogAnswer::from_value(field(action, "result"))?;
                    let answer = StoredAnswer {
                        request_id: id.clone(),
                        result,
                        submitted_at: format_epoch_iso(clock.load(Ordering::SeqCst)).into(),
                    };
                    let mut answers = answers.answers.lock().unwrap();
                    if let Some(index) = answers.iter().position(|(candidate, _)| candidate == &id)
                    {
                        answers[index].1 = answer;
                    } else {
                        answers.push((id, answer));
                    }
                    JsValue::Null
                }
                "recover" => JsValue::Array(logging_journal.recover()?),
                "corruptJournal" => {
                    let _ = fs::remove_file(&journal_path);
                    fs::create_dir_all(&journal_path)?;
                    JsValue::Null
                }
                "rejectSession" => {
                    let reason = action
                        .get("reason")
                        .and_then(JsValue::as_string)
                        .map(|value| value.to_utf8().unwrap())
                        .unwrap_or_else(|| "closed".into());
                    bus_observer
                        .lock()
                        .unwrap()
                        .reject_session(&session, &reason);
                    JsValue::Null
                }
                "snapshot" => JsValue::Null,
                op => panic!("unhandled channel op {op}"),
            })
        })();
        let result = result_value(outcome);
        equal(
            &result,
            field(step, "result"),
            &format!("{name} step {index} result"),
        );
        let observation = channel_observation(
            &clock,
            &journal_path,
            &logs,
            &dialogs,
            &owners,
            &answers,
            &bus_observer,
            &session,
            &mut slots,
        );
        equal(
            &observation,
            field(step, "observation"),
            &format!("{name} step {index} observation"),
        );
    }
}

fn slot_id(action: &JsValue, slots: &[(String, Slot)]) -> JsString {
    let name = action
        .get("slot")
        .and_then(JsValue::as_string)
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_else(|| "default".into());
    slots
        .iter()
        .find(|(candidate, _)| candidate == &name)
        .map(|(_, slot)| slot.request.request_id().clone())
        .expect("request slot")
}

#[allow(clippy::too_many_arguments)]
fn channel_observation(
    clock: &AtomicI64,
    journal_path: &Path,
    logs: &Logs,
    dialogs: &Arc<Mutex<Vec<(JsString, StoredDialog)>>>,
    owners: &OwnerHarness,
    answers: &AnswerHarness,
    bus: &Arc<Mutex<GatewayUserDialogBus>>,
    session: &JsString,
    slots: &mut [(String, Slot)],
) -> JsValue {
    let mut outcomes = JsObject::new();
    for (name, slot) in slots {
        if slot.outcome.is_none()
            && let Some(result) = slot.request.try_result()
        {
            slot.outcome = Some(match result {
                Ok(answer) => object([
                    ("ok", JsValue::Bool(true)),
                    ("value", answer.to_channel_value()),
                ]),
                Err(error) => object([
                    ("ok", JsValue::Bool(false)),
                    ("error", JsValue::from(error.as_str())),
                ]),
            });
        }
        outcomes.insert(
            name.clone().into(),
            slot.outcome.clone().unwrap_or(JsValue::Null),
        );
    }
    let (pending, pending_count) = {
        let mut bus = bus.lock().unwrap();
        (
            JsValue::Array(bus.list(session)),
            JsValue::Number(bus.pending_count(session) as f64),
        )
    };
    let bytes = match fs::read(journal_path) {
        Ok(bytes) => JsValue::String(String::from_utf8_lossy(&bytes).into_owned().into()),
        Err(_) => JsValue::Null,
    };
    let store = JsValue::Array(
        dialogs
            .lock()
            .unwrap()
            .iter()
            .map(|(id, dialog)| {
                JsValue::Array(vec![JsValue::String(id.clone()), dialog.to_value()])
            })
            .collect(),
    );
    let now = clock.load(Ordering::SeqCst);
    let owners_snapshot = JsValue::Array(
        owners
            .owners
            .lock()
            .unwrap()
            .iter()
            .map(|(id, owner)| {
                object([
                    ("id", JsValue::String(id.clone())),
                    ("ownerId", JsValue::String(owner.owner_id.clone())),
                    ("expiresAt", JsValue::Number(owner.expires_at as f64)),
                    ("active", JsValue::Bool(owner.expires_at > now)),
                ])
            })
            .collect(),
    );
    let answers_snapshot = JsValue::Array(
        answers
            .answers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, answer)| {
                JsValue::Array(vec![JsValue::String(id.clone()), raw_answer_value(answer)])
            })
            .collect(),
    );
    object([
        ("outcomes", JsValue::Object(outcomes)),
        ("pending", pending),
        ("pendingCount", pending_count),
        ("journalBytes", bytes),
        ("journalExists", JsValue::Bool(journal_path.exists())),
        ("store", store),
        ("owners", owners_snapshot),
        ("answers", answers_snapshot),
        ("logs", logs.snapshot()),
    ])
}

fn claim_value(claim: LeaseClaim) -> JsValue {
    match claim {
        LeaseClaim::Claimed(lease) => object([
            ("claimed", JsValue::Bool(true)),
            (
                "lease",
                object([
                    ("leaseId", JsValue::String(lease.lease_id)),
                    ("expiresAt", JsValue::String(lease.expires_at)),
                ]),
            ),
        ]),
        LeaseClaim::Busy { expires_at } => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", "claimed".into()),
            ("expiresAt", JsValue::String(expires_at)),
        ]),
        LeaseClaim::NotPending => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", "not_pending".into()),
        ]),
    }
}

struct Probe {
    _scratch: Scratch,
    channel: GatewayUserDialogChannel,
    signal: AbortSignal,
    journal: Arc<GatewayUserDialogJournal>,
    journal_path: PathBuf,
    dialogs: Arc<Mutex<Vec<(JsString, StoredDialog)>>>,
    owners: Arc<OwnerHarness>,
    bus: Arc<Mutex<GatewayUserDialogBus>>,
    logs: Arc<Logs>,
}
impl Probe {
    fn begin_input(&self) -> ChannelRequest {
        self.channel.request_input(&InputRequest {
            tool_call_id: "tool-1".into(),
            tool_name: "request_user_input".into(),
            prompt: "abort".into(),
            placeholder: None,
            allow_empty: false,
            abort: Some(self.signal.clone()),
        })
    }
}

/// Minimal channel harness that keeps the real journal/store/owner/bus so a
/// deterministic interleaving can assert the complete durable cleanup.
fn probe(signal: AbortSignal, hook: Box<dyn Fn(&JsValue) + Send + Sync>) -> Probe {
    let scratch = Scratch::new();
    let journal_path = scratch.0.join("sessions/transcript.jsonl.dialogs.json");
    let clock = Arc::new(AtomicI64::new(0));
    let logs = Arc::new(Logs::default());
    let journal = Arc::new(GatewayUserDialogJournal::new(
        journal_path.clone(),
        JournalOptions {
            runtime: runtime(&clock, "journal-tmp"),
        },
    ));
    let dialogs = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(LoggingStore {
        logs: logs.clone(),
        dialogs: dialogs.clone(),
        put_fail: AtomicBool::new(false),
        remove_fail: AtomicBool::new(false),
        abort_on_put: AtomicBool::new(false),
        signal: Arc::new(Mutex::new(None)),
    });
    let owners = Arc::new(OwnerHarness {
        logs: logs.clone(),
        owners: Mutex::new(Vec::new()),
        claim_mode: Mutex::new("ok".into()),
        renew_mode: Mutex::new("ok".into()),
        release_mode: Mutex::new("ok".into()),
        ttl_ms: AtomicI64::new(5000),
        clock: clock.clone(),
    });
    let hook = Arc::new(hook);
    let bus = Arc::new(Mutex::new(GatewayUserDialogBus::new(
        runtime(&clock, "bus-lease"),
        Some(Box::new({
            let logs = logs.clone();
            move |event| {
                log(&logs.bus, event.clone());
                hook(&event);
                Ok(())
            }
        })),
    )));
    let session: JsString = "session".into();
    let mut options = ChannelOptions::new(session.clone(), bus.clone(), {
        let logs = logs.clone();
        Arc::new(move |event| {
            log(&logs.emit, event);
            Ok(())
        })
    });
    options.journal = Some(journal.clone() as Arc<dyn UserDialogJournal>);
    options.store = Some(store as Arc<dyn UserDialogStore>);
    options.store_key = Some(StoreKey {
        project_root: "/fixture/project".into(),
        pilot_home: "/fixture/home".into(),
        session_id: "session".into(),
    });
    options.claim_stored_owner = Some({
        let owners = owners.clone();
        Arc::new(move |id: &JsString, owner: &JsString| owners.claim(id, owner))
    });
    options.release_stored_owner = Some({
        let owners = owners.clone();
        Arc::new(move |id: &JsString, owner: &JsString| owners.release(id, owner))
    });
    let uuid = Arc::new(AtomicI64::new(0));
    options.uuid = Arc::new(move || format!("uuid-{}", uuid.fetch_add(1, Ordering::SeqCst) + 1));
    options.now_ms = {
        let clock = clock.clone();
        Arc::new(move || clock.load(Ordering::SeqCst))
    };
    let channel = GatewayUserDialogChannel::new(options);
    Probe {
        _scratch: scratch,
        channel,
        signal,
        journal,
        journal_path,
        dialogs,
        owners,
        bus,
        logs,
    }
}

fn assert_probe_settled(probe: &Probe, request: &mut ChannelRequest) {
    match request.try_result().expect("request settled") {
        Ok(DialogAnswer::Cancelled(Some(reason))) => {
            assert!(reason.equals_str("aborted"), "reason {reason:?}")
        }
        other => panic!("unexpected outcome {other:?}"),
    }
    assert_eq!(
        probe.bus.lock().unwrap().pending_count(&"session".into()),
        0,
        "bus entry leaked"
    );
    assert!(!probe.journal_path.exists(), "journal record leaked");
    assert!(probe.dialogs.lock().unwrap().is_empty(), "store leaked");
    assert!(
        probe.owners.owners.lock().unwrap().is_empty(),
        "owner lease leaked"
    );
    assert!(probe.journal.recover().unwrap().is_empty());
    assert!(
        probe.logs.emit.lock().unwrap().iter().any(|event| event
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.equals_str("user_dialog_cancelled"))),
        "missing cancellation event"
    );
}

#[test]
fn abort_during_registration_settles_bus_journal_store_and_lease() {
    let signal = AbortSignal::new();
    let abort_signal = signal.clone();
    let probe = probe(
        signal,
        Box::new(move |event| {
            if event
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("requested"))
            {
                abort_signal.abort();
            }
        }),
    );
    let mut request = probe.begin_input();
    assert_probe_settled(&probe, &mut request);
}

#[test]
fn abort_from_another_thread_during_registration_settles_everything() {
    let (started, started_rx) = mpsc::channel::<()>();
    let (finished, finished_rx) = mpsc::channel::<()>();
    let finished_rx = Arc::new(Mutex::new(finished_rx));
    let probe = probe(
        AbortSignal::new(),
        Box::new(move |event| {
            if event
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("requested"))
            {
                started.send(()).unwrap();
                finished_rx.lock().unwrap().recv().unwrap();
            }
        }),
    );
    let channel = probe.channel.clone();
    let aborter = std::thread::spawn(move || {
        started_rx.recv().unwrap();
        channel.abort(&"uuid-1".into());
        finished.send(()).unwrap();
    });
    let mut request = probe.begin_input();
    aborter.join().unwrap();
    assert_probe_settled(&probe, &mut request);
}

#[test]
fn abort_signal_removes_only_its_own_registration() {
    let signal = AbortSignal::new();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let first = signal
        .on_abort(Arc::new({
            let hits = hits.clone();
            move || hits.lock().unwrap().push(1)
        }))
        .unwrap();
    signal
        .on_abort(Arc::new({
            let hits = hits.clone();
            move || hits.lock().unwrap().push(2)
        }))
        .unwrap();
    signal.remove_abort(first);
    signal.abort();
    assert_eq!(*hits.lock().unwrap(), vec![2]);
    // Removing after abort (or an unknown token) is a no-op.
    signal.remove_abort(first);
}

#[test]
fn journal_concurrent_record_and_remove_are_serialized() {
    let scratch = Scratch::new();
    let path = scratch.0.join("journal.json");
    let journal = Arc::new(GatewayUserDialogJournal::new(
        path.clone(),
        JournalOptions {
            runtime: DialogRuntime::default(),
        },
    ));
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for worker in 0..8 {
        let journal = journal.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for index in 0..25 {
                let mut event = JsObject::new();
                event.insert("type".into(), "user_dialog_request".into());
                event.insert(
                    "requestId".into(),
                    JsValue::String(format!("w{worker}-{index}").into()),
                );
                event.insert("dialogKind".into(), "input".into());
                event.insert("prompt".into(), "并发".into());
                journal.record(&JsValue::Object(event)).unwrap();
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let read = || -> Vec<JsValue> {
        let parsed = parse_json(&fs::read_to_string(&path).unwrap()).unwrap();
        array(field(&parsed, "dialogs")).to_vec()
    };
    let recorded = read();
    assert_eq!(recorded.len(), 200, "concurrent record lost entries");
    let ids: std::collections::HashSet<_> = recorded
        .iter()
        .map(|dialog| text_field(dialog, "requestId"))
        .collect();
    assert_eq!(ids.len(), 200, "concurrent record duplicated entries");

    let mut workers = Vec::new();
    for worker in 0..8 {
        let journal = journal.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for index in (0..25).step_by(2) {
                journal
                    .remove(&format!("w{worker}-{index}").into())
                    .unwrap();
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(read().len(), 96, "concurrent remove lost updates");
}

fn raw_answer_value(answer: &StoredAnswer) -> JsValue {
    // The host answer map keeps an empty cancellation reason; the hosted-store
    // writer (`DialogAnswer::to_value`) drops it. Observation mirrors the host
    // map, not the store file.
    let result = match &answer.result {
        DialogAnswer::Answered(value) => {
            object([("behavior", "answered".into()), ("value", value.clone())])
        }
        DialogAnswer::Cancelled(reason) => {
            let mut value = object([("behavior", "cancelled".into())]);
            if let Some(reason) = reason {
                set(&mut value, "reason", JsValue::String(reason.clone()));
            }
            value
        }
    };
    object([
        ("requestId", JsValue::String(answer.request_id.clone())),
        ("result", result),
        ("submittedAt", JsValue::String(answer.submitted_at.clone())),
    ])
}

fn replay_journal_case(case: &JsValue, initial_ms: f64) {
    let name = text_field(case, "name");
    let clock = Arc::new(AtomicI64::new(initial_ms as i64));
    let scratch = Scratch::new();
    let path = scratch.0.join("transcript.jsonl.dialogs.json");
    if let Some(initial) = case
        .get("initial")
        .filter(|initial| !matches!(initial, JsValue::Null))
    {
        if bool_field(initial, "directory", false) {
            fs::create_dir_all(&path).unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text_field(initial, "text")).unwrap();
        }
    }
    let journal = GatewayUserDialogJournal::new(
        path.clone(),
        JournalOptions {
            runtime: runtime(&clock, "journal-tmp"),
        },
    );
    for (index, step) in array(field(case, "steps")).iter().enumerate() {
        let action = field(step, "action");
        let result = result_value((|| -> Result<JsValue> {
            Ok(match text_field(action, "op").as_str() {
                "record" => {
                    journal.record(field(action, "event"))?;
                    JsValue::Null
                }
                "remove" => {
                    journal.remove(&js_string(field(action, "id")))?;
                    JsValue::Null
                }
                "recover" => JsValue::Array(journal.recover()?),
                "clear" => {
                    journal.clear()?;
                    JsValue::Null
                }
                "corruptJournal" => {
                    let _ = fs::remove_file(&path);
                    fs::create_dir_all(&path)?;
                    JsValue::Null
                }
                "snapshot" => JsValue::Null,
                op => panic!("unhandled journal op {op}"),
            })
        })());
        equal(
            &result,
            field(step, "result"),
            &format!("journal {name} step {index} result"),
        );
        let observation = object([
            (
                "bytes",
                match fs::read(&path) {
                    Ok(bytes) => {
                        JsValue::String(String::from_utf8_lossy(&bytes).into_owned().into())
                    }
                    Err(_) => JsValue::Null,
                },
            ),
            ("exists", JsValue::Bool(path.exists())),
            ("directory", JsValue::Bool(path.is_dir())),
        ]);
        equal(
            &observation,
            field(step, "observation"),
            &format!("journal {name} step {index} observation"),
        );
    }
}

fn creation_path(case: &JsValue) -> Option<PathBuf> {
    case.get("transcriptPath")
        .and_then(JsValue::as_string)
        .map(|value| PathBuf::from(value.to_utf8().unwrap()))
}

#[test]
fn channel_and_journal_match_real_ts_oracle_step_by_step() {
    let oracle = fixture();
    for case in array(field(&oracle, "channelCases")) {
        replay_channel_case(&oracle, case);
    }
    let initial_ms = number_field(&oracle, "initialMs", 0.0);
    for case in array(field(&oracle, "journalCases")) {
        replay_journal_case(case, initial_ms);
    }
    for case in array(field(&oracle, "creationCases")) {
        let runtime = DialogRuntime::default();
        let actual = create_gateway_user_dialog_journal(
            creation_path(case).as_deref(),
            JournalOptions { runtime },
        )
        .map(|journal| journal.path().to_string_lossy().into_owned());
        let expected = field(case, "path");
        assert_eq!(
            actual.as_deref(),
            expected
                .as_string()
                .map(|value| value.to_utf8().unwrap())
                .as_deref(),
            "creation {}",
            text_field(case, "name")
        );
    }
}

#[test]
#[ignore = "requires a real PilotDeck baseline checkout and node_modules/tsx"]
fn user_dialog_channel_fixtures_regenerate_identically_from_ts() {
    crate::test_baseline::regenerates_identically("user-dialog-channel");
}
