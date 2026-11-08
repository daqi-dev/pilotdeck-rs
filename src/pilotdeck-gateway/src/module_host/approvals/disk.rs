//! Host-owned SOP state. A transaction keeps its per-session mutex and IO permit
//! inside the blocking worker through publication, including caller cancellation.
//! The existing pending store owns revision/receipt/handoff authorization math.
use super::pending::PendingSopStore;
use super::{ApprovalFailure, ApprovalPendingStore, StoreResumeInput, StoreStatusInput};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, LazyLock, Mutex, Weak},
};

pub(crate) const STATE_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Debug)]
pub(crate) struct SopFailure {
    pub code: String,
    pub message: String,
}
impl SopFailure {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    fn approval(self) -> ApprovalFailure {
        ApprovalFailure {
            status: None,
            code: if self.code == "gateway_request_failed" {
                None
            } else {
                Some(self.code.as_str().into())
            },
        }
    }
}
fn failed(message: impl Into<String>) -> SopFailure {
    SopFailure::new("gateway_request_failed", message)
}
fn prop<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn set(value: &mut JsValue, key: &str, next: JsValue) {
    let JsValue::Object(object) = value else {
        unreachable!("validated SOP record")
    };
    object.insert(key.into(), next);
}
fn now() -> String {
    use pilotdeck_transport::server::ExecutionClock;
    String::from_utf16_lossy(
        pilotdeck_transport::system_clock::SystemExecutionClock
            .now_iso()
            .units(),
    )
}
static IO: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
type Gate = tokio::sync::Mutex<()>;
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Gate>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn gate(path: &Path) -> Arc<Gate> {
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, weak| weak.strong_count() > 0);
    if let Some(gate) = map.get(path).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Gate::new(()));
    map.insert(path.to_owned(), Arc::downgrade(&gate));
    gate
}
#[derive(Clone)]
pub(crate) struct DiskSopStore {
    root: PathBuf,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
}
impl DiskSopStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            clock: Arc::new(now),
        }
    }
    #[cfg(test)]
    pub(crate) fn with_clock(root: PathBuf, clock: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        Self { root, clock }
    }
    pub(crate) fn path(&self, session: &JsString) -> PathBuf {
        self.root.join(format!(
            "{}.json",
            URL_SAFE_NO_PAD.encode(String::from_utf16_lossy(session.units()).as_bytes())
        ))
    }
    async fn transaction<R: Send + 'static>(
        &self,
        session: JsString,
        operation: impl FnOnce(&DiskSopStore, &JsString, Option<JsValue>) -> Result<R, SopFailure>
        + Send
        + 'static,
    ) -> Result<R, SopFailure> {
        let guard = gate(&self.path(&session)).lock_owned().await;
        let permit = IO
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| failed("SOP state IO is unavailable."))?;
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _permit = permit;
            let value = store.read(&session)?;
            operation(&store, &session, value)
        })
        .await
        .map_err(|_| failed("SOP state worker stopped unexpectedly."))?
    }
    fn seeded(session: &JsString, value: &JsValue) -> Result<PendingSopStore, SopFailure> {
        let pending = PendingSopStore::new(Arc::new(|_| Some("disk".into())));
        pending.seed(None, session, value).map_err(|_| {
            failed(format!(
                "StaffDeck SOP state for '{}' has an invalid schema.",
                String::from_utf16_lossy(session.units())
            ))
        })?;
        Ok(pending)
    }
    fn normalize(session: &JsString, value: &JsValue) -> Result<JsValue, SopFailure> {
        Self::seeded(session, value)?
            .persisted(None, session, value)
            .map_err(|_| failed("SOP state migration failed."))?
            .ok_or_else(|| failed("SOP state migration produced no record."))
    }
    fn read(&self, session: &JsString) -> Result<Option<JsValue>, SopFailure> {
        let path = self.path(session);
        let Some(raw) = read_file(&self.root, path.file_name().expect("encoded session file"))
            .map_err(|e| {
                failed(format!(
                    "Failed to read StaffDeck SOP state for '{}': {e}",
                    String::from_utf16_lossy(session.units())
                ))
            })?
        else {
            return Ok(None);
        };
        let value = parse_json(&String::from_utf8_lossy(&raw)).map_err(|error| {
            failed(format!(
                "StaffDeck SOP state for '{}' is invalid JSON: {error}",
                String::from_utf16_lossy(session.units())
            ))
        })?;
        Ok(Some(Self::normalize(session, &value)?))
    }
    fn write(&self, value: &JsValue) -> Result<(), SopFailure> {
        let session = prop(value, "sessionId")
            .as_string()
            .ok_or_else(|| failed("SOP state has no session id."))?;
        let value = Self::normalize(session, value)?;
        let text = stringify_json(&value)
            .map_err(|_| failed("SOP state cannot be serialized."))?
            .ok_or_else(|| failed("SOP state cannot be serialized."))?;
        let mut bytes = text.into_bytes();
        bytes.push(b'\n');
        if bytes.len() > STATE_LIMIT {
            return Err(SopFailure::new(
                "SOP_STATE_RESOURCE_LIMIT",
                "SOP state exceeded its 8 MiB limit.",
            ));
        }
        publish(
            &self.root,
            self.path(session)
                .file_name()
                .expect("encoded session file"),
            &bytes,
        )
        .map_err(|error| {
            failed(format!(
                "Failed to persist StaffDeck SOP state for '{}': {error}",
                String::from_utf16_lossy(session.units())
            ))
        })
    }
    pub(crate) async fn load(&self, session: &JsString) -> Result<Option<JsValue>, SopFailure> {
        self.transaction(session.clone(), |_, _, value| Ok(value))
            .await
    }
    pub(crate) async fn load_or_create(
        &self,
        session: &JsString,
        bundle: JsValue,
        default: JsString,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |store, session, current| {
            if let Some(value) = current {
                return Ok(value);
            }
            let state = crate::metadata::object([
                ("version", JsValue::Number(1.0)),
                ("selected_skill_id", JsValue::String(default)),
                ("slots_json", JsValue::Object(JsObject::new())),
                ("skill_stack_json", JsValue::Array(vec![])),
                ("successful_tool_names", JsValue::Array(vec![])),
            ]);
            let next = record(
                session,
                bundle,
                state,
                1.0,
                None,
                None,
                JsValue::Object(JsObject::new()),
                (store.clock)(),
            );
            store.write(&next)?;
            Ok(next)
        })
        .await
    }
    pub(crate) async fn replace(
        &self,
        session: &JsString,
        bundle: JsValue,
        state: JsValue,
        expected: Option<f64>,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |store, session, current| {
            if expected.is_some()
                && current
                    .as_ref()
                    .map(|v| prop(v, "revision").as_number().unwrap_or(0.0))
                    != expected
            {
                return Err(SopFailure::new(
                    "SOP_REVISION_CONFLICT",
                    "The SOP changed during preparation.",
                ));
            }
            let next = store.replacement(session, bundle, state, current.as_ref(), None);
            store.write(&next)?;
            Ok(next)
        })
        .await
    }
    fn replacement(
        &self,
        session: &JsString,
        bundle: JsValue,
        state: JsValue,
        current: Option<&JsValue>,
        delivery: Option<JsValue>,
    ) -> JsValue {
        let revision = current
            .and_then(|v| v.get("revision"))
            .and_then(JsValue::as_number)
            .unwrap_or(0.0)
            + 1.0;
        let now = (self.clock)();
        let wait = wait_for_state(&state, current.and_then(|v| v.get("wait")), &now);
        record(
            session,
            bundle,
            state,
            revision,
            wait,
            delivery.or_else(|| current.and_then(|v| v.get("replyDelivery")).cloned()),
            current
                .and_then(|v| v.get("resumeRequests"))
                .cloned()
                .unwrap_or_else(|| JsValue::Object(JsObject::new())),
            now,
        )
    }
    pub(crate) async fn commit_submission(
        &self,
        session: &JsString,
        bundle: JsValue,
        state: JsValue,
        expected: f64,
        turn: JsString,
        result: JsValue,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |store, session, current| {
            let revision = current
                .as_ref()
                .and_then(|v| v.get("revision"))
                .and_then(JsValue::as_number)
                .unwrap_or(0.0);
            if revision != expected {
                return Err(SopFailure::new(
                    "SOP_REVISION_CONFLICT",
                    format!("Expected SOP revision {expected}, found {revision}."),
                ));
            }
            let delivery = crate::metadata::object([
                ("turnId", JsValue::String(turn)),
                ("phase", "pending".into()),
                ("result", result),
                ("committedAt", (store.clock)().as_str().into()),
            ]);
            let next = store.replacement(session, bundle, state, current.as_ref(), Some(delivery));
            store.write(&next)?;
            Ok(next)
        })
        .await
    }
    pub(crate) async fn mark_reply_durable(
        &self,
        session: &JsString,
        expected: JsString,
        delivered: JsString,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |store, _, current| {
            let mut current = current.ok_or_else(|| {
                SopFailure::new(
                    "SOP_REPLY_DELIVERY_NOT_FOUND",
                    format!(
                        "No StaffDeck SOP reply delivery exists for turn '{}'.",
                        String::from_utf16_lossy(expected.units())
                    ),
                )
            })?;
            let mut delivery = prop(&current, "replyDelivery").clone();
            if delivery.get("turnId") != Some(&JsValue::String(expected.clone())) {
                return Err(SopFailure::new(
                    "SOP_REPLY_DELIVERY_NOT_FOUND",
                    format!(
                        "No StaffDeck SOP reply delivery exists for turn '{}'.",
                        String::from_utf16_lossy(expected.units())
                    ),
                ));
            }
            if prop(&delivery, "phase") == &JsValue::from("durable") && expected == delivered {
                return Ok(delivery);
            }
            let now = (store.clock)();
            set(&mut delivery, "turnId", JsValue::String(delivered));
            set(&mut delivery, "phase", "durable".into());
            set(&mut delivery, "durableAt", now.as_str().into());
            set(&mut current, "replyDelivery", delivery.clone());
            set(&mut current, "updatedAt", now.as_str().into());
            store.write(&current)?;
            Ok(delivery)
        })
        .await
    }
    pub(crate) async fn clear_reply_delivery(
        &self,
        session: &JsString,
        turn: JsString,
    ) -> Result<(), SopFailure> {
        self.transaction(session.clone(), move |store, _, current| {
            let Some(mut current) = current else {
                return Ok(());
            };
            if current.get("replyDelivery").and_then(|v| v.get("turnId"))
                != Some(&JsValue::String(turn))
            {
                return Ok(());
            }
            let mut next = JsObject::new();
            for (key, value) in current
                .as_object()
                .expect("normalized record")
                .own_entries()
            {
                if !key.equals_str("replyDelivery") {
                    next.insert(key.clone(), value.clone());
                }
            }
            current = JsValue::Object(next);
            set(&mut current, "updatedAt", (store.clock)().as_str().into());
            store.write(&current)
        })
        .await
    }
    pub(crate) async fn reply_delivery(
        &self,
        session: &JsString,
    ) -> Result<Option<JsValue>, SopFailure> {
        self.transaction(session.clone(), |_, _, current| {
            Ok(current.and_then(|v| v.get("replyDelivery").cloned()))
        })
        .await
    }
    pub(crate) async fn authority(
        &self,
        session: &JsString,
        expected: f64,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |_, _, current| {
            let current = current.ok_or_else(|| {
                SopFailure::new(
                    "SOP_AUTHORITY_STATE_MISSING",
                    "No admitted SOP state exists.",
                )
            })?;
            if prop(&current, "revision").as_number() != Some(expected) {
                return Err(SopFailure::new(
                    "SOP_REVISION_CONFLICT",
                    "The SOP authority is stale.",
                ));
            }
            let state = prop(&current, "state");
            if current.get("wait").is_some_and(super::js_truthy)
                || prop(state, "status") != &JsValue::from("active")
            {
                return Err(SopFailure::new(
                    "SOP_AUTHORITY_STATE_INACTIVE",
                    "The SOP is not executing.",
                ));
            }
            let id = state
                .get("active_skill_id")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .unwrap_or_else(|| prop(state, "selected_skill_id"));
            let matches = match prop(prop(&current, "bundle"), "sops") {
                JsValue::Array(sops) => sops
                    .iter()
                    .filter(|sop| {
                        sop.get("skill_id")
                            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                            .unwrap_or_else(|| prop(sop, "id"))
                            == id
                    })
                    .collect::<Vec<_>>(),
                _ => vec![],
            };
            let invalid = || {
                SopFailure::new(
                    "SOP_AUTHORITY_PIN_INVALID",
                    "The exact SOP pin is unavailable.",
                )
            };
            if matches.len() != 1 {
                return Err(invalid());
            }
            let sop = matches[0];
            if prop(sop, "version").as_string().is_none()
                || prop(sop, "content").as_object().is_none()
                || prop(state, "active_step_id").as_string().is_none()
            {
                return Err(invalid());
            }
            Ok(crate::metadata::object([
                ("sopId", id.clone()),
                ("sopVersion", prop(sop, "version").clone()),
                ("nodeId", prop(state, "active_step_id").clone()),
                ("content", prop(sop, "content").clone()),
            ]))
        })
        .await
    }
    pub(crate) async fn record_successful_tools(
        &self,
        session: &JsString,
        bundle: JsValue,
        default: JsString,
        names: Vec<JsString>,
    ) -> Result<JsValue, SopFailure> {
        self.transaction(session.clone(), move |store, session, current| {
            let current = if let Some(current) = current {
                current
            } else {
                let state = crate::metadata::object([
                    ("version", JsValue::Number(1.0)),
                    ("selected_skill_id", JsValue::String(default)),
                    ("slots_json", JsValue::Object(JsObject::new())),
                    ("skill_stack_json", JsValue::Array(vec![])),
                    ("successful_tool_names", JsValue::Array(vec![])),
                ]);
                let created = record(
                    session,
                    bundle,
                    state,
                    1.0,
                    None,
                    None,
                    JsValue::Object(JsObject::new()),
                    (store.clock)(),
                );
                store.write(&created)?;
                created
            };
            let mut state = prop(&current, "state").clone();
            let mut seen = std::collections::HashSet::new();
            let mut successful = vec![];
            if let JsValue::Array(existing) = prop(&state, "successful_tool_names") {
                for name in existing
                    .iter()
                    .filter_map(JsValue::as_string)
                    .chain(names.iter())
                {
                    if !name.is_empty() && seen.insert(name.clone()) {
                        successful.push(JsValue::String(name.clone()));
                    }
                }
            } else {
                for name in &names {
                    if !name.is_empty() && seen.insert(name.clone()) {
                        successful.push(JsValue::String(name.clone()));
                    }
                }
            }
            set(
                &mut state,
                "successful_tool_names",
                JsValue::Array(successful),
            );
            let next = store.replacement(
                session,
                prop(&current, "bundle").clone(),
                state,
                Some(&current),
                None,
            );
            store.write(&next)?;
            Ok(next)
        })
        .await
    }
    pub(crate) async fn model_context(
        &self,
        session: &JsString,
    ) -> Result<Option<JsValue>, SopFailure> {
        self.transaction(session.clone(), |_, session, current| {
            let Some(current) = current else {
                return Ok(None);
            };
            let state = prop(&current, "state");
            let mut output = JsObject::new();
            if let Some(id) = state
                .get("active_skill_id")
                .filter(|v| v.as_string().is_some())
                .or_else(|| {
                    state
                        .get("selected_skill_id")
                        .filter(|v| v.as_string().is_some())
                })
                .filter(|v| super::js_truthy(v))
            {
                output.insert("sopId".into(), id.clone());
            }
            if let Some(step) = state
                .get("active_step_id")
                .filter(|v| v.as_string().is_some())
            {
                output.insert("stepId".into(), step.clone());
            }
            let snapshot = Self::seeded(session, &current)?
                .snapshot(&StoreStatusInput {
                    session_key: session.clone(),
                    project_key: None,
                })
                .map_err(|_| failed("SOP model context projection failed."))?;
            let mut assignee = snapshot
                .as_ref()
                .and_then(|v| v.get("approval"))
                .and_then(|v| v.get("assigneeUserId"))
                .filter(|v| super::js_truthy(v))
                .cloned();
            if assignee.is_none()
                && let Some(receipts) = prop(&current, "resumeRequests").as_object()
            {
                assignee = receipts
                    .own_entries()
                    .into_iter()
                    .rev()
                    .filter_map(|(_, value)| {
                        value
                            .get("authorization")
                            .and_then(|v| v.get("approval"))
                            .and_then(|v| v.get("assigneeUserId"))
                            .filter(|v| super::js_truthy(v))
                            .cloned()
                    })
                    .next();
            }
            if let Some(assignee) = assignee {
                output.insert("assigneeUserId".into(), assignee);
            }
            Ok(Some(JsValue::Object(output)))
        })
        .await
    }
    pub(crate) async fn status(&self, session: &JsString) -> Result<Option<JsValue>, SopFailure> {
        self.transaction(session.clone(), |_, session, current| {
            let Some(current) = current else {
                return Ok(None);
            };
            Self::seeded(session, &current)?
                .snapshot(&StoreStatusInput {
                    session_key: session.clone(),
                    project_key: None,
                })
                .map_err(|_| failed("SOP status projection failed."))
        })
        .await
    }
    pub(crate) async fn resume(&self, input: StoreResumeInput) -> Result<JsValue, SopFailure> {
        self.transaction(input.session_key.clone(), move |store, session, current| {
            let current = current.ok_or_else(|| {
                SopFailure::new(
                    "SOP_SESSION_NOT_FOUND",
                    format!(
                        "No StaffDeck SOP state exists for '{}'.",
                        String::from_utf16_lossy(session.units())
                    ),
                )
            })?;
            let pending = Self::seeded(session, &current)?;
            let result = pending.apply_resume(&input).map_err(|error| {
                let code = error
                    .code
                    .as_ref()
                    .and_then(JsValue::as_string)
                    .map(|v| String::from_utf16_lossy(v.units()))
                    .unwrap_or_else(|| "SOP_STATE_INVALID".into());
                SopFailure::new(&code, pending.resume_message(&input, &code))
            })?;
            if result.get("duplicate") != Some(&JsValue::Bool(true)) {
                let mut next = pending
                    .persisted(None, session, &current)
                    .map_err(|_| failed("SOP resume export failed."))?
                    .ok_or_else(|| failed("SOP resume export produced no state."))?;
                set(&mut next, "updatedAt", (store.clock)().as_str().into());
                store.write(&next)?;
            }
            Ok(result)
        })
        .await
    }
}
#[allow(clippy::too_many_arguments)]
fn record(
    session: &JsString,
    bundle: JsValue,
    state: JsValue,
    revision: f64,
    wait: Option<JsValue>,
    delivery: Option<JsValue>,
    receipts: JsValue,
    now: String,
) -> JsValue {
    let mut next = JsObject::new();
    next.insert("schemaVersion".into(), JsValue::Number(3.0));
    next.insert("sessionId".into(), JsValue::String(session.clone()));
    next.insert("bundle".into(), bundle);
    next.insert("state".into(), state);
    next.insert("revision".into(), JsValue::Number(revision));
    if let Some(wait) = wait {
        next.insert("wait".into(), wait);
    }
    if let Some(delivery) = delivery {
        next.insert("replyDelivery".into(), delivery);
    }
    next.insert("resumeRequests".into(), receipts);
    next.insert("updatedAt".into(), now.as_str().into());
    JsValue::Object(next)
}
fn wait_for_state(state: &JsValue, existing: Option<&JsValue>, now: &str) -> Option<JsValue> {
    let kind = match prop(state, "status").as_string() {
        Some(s) if s.equals_str("handoff") => "handoff",
        Some(s) if s.equals_str("waiting_external_task") => "external_task",
        _ => return None,
    };
    if let Some(wait) = existing
        && prop(wait, "kind") == &JsValue::from(kind)
        && prop(wait, "skillId") == prop(state, "active_skill_id")
        && prop(wait, "stepId") == prop(state, "active_step_id")
    {
        return Some(wait.clone());
    }
    let mut wait = JsObject::new();
    wait.insert(
        "id".into(),
        uuid::Uuid::new_v4().to_string().as_str().into(),
    );
    wait.insert("kind".into(), kind.into());
    for (target, source) in [("skillId", "active_skill_id"), ("stepId", "active_step_id")] {
        if let Some(value) = state.get(source).filter(|v| v.as_string().is_some()) {
            wait.insert(target.into(), value.clone());
        }
    }
    wait.insert("createdAt".into(), now.into());
    if let Some(id) = state
        .get("awaiting_input_json")
        .and_then(|v| v.get("wait_id"))
        .filter(|v| v.as_string().is_some())
    {
        wait.insert("id".into(), id.clone());
    }
    Some(JsValue::Object(wait))
}

#[cfg(unix)]
fn directory(root: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
}
#[cfg(unix)]
fn read_file(root: &Path, name: &std::ffi::OsStr) -> std::io::Result<Option<Vec<u8>>> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::io::Read;
    let directory = match directory(root) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let fd = match openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(v) => v,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file = std::fs::File::from(fd);
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("SOP state must be a regular file."));
    }
    if meta.len() > STATE_LIMIT as u64 {
        return Err(std::io::Error::other("SOP state exceeded its 8 MiB limit."));
    }
    let mut raw = Vec::new();
    file.take((STATE_LIMIT + 1) as u64).read_to_end(&mut raw)?;
    if raw.len() > STATE_LIMIT {
        return Err(std::io::Error::other("SOP state exceeded its 8 MiB limit."));
    }
    Ok(Some(raw))
}
#[cfg(unix)]
fn publish(root: &Path, name: &std::ffi::OsStr, bytes: &[u8]) -> std::io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
    use std::io::Write;
    std::fs::create_dir_all(root)?;
    let directory = directory(root)?;
    let temporary = format!(".sop-{}.tmp", uuid::Uuid::new_v4());
    let fd = openat(
        &directory,
        temporary.as_str(),
        OFlags::WRONLY
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )?;
    let result = (|| {
        let mut file = std::fs::File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(&directory, temporary.as_str(), &directory, name)?;
        directory.sync_all()
    })();
    if result.is_err() {
        let _ = unlinkat(&directory, temporary.as_str(), AtFlags::empty());
    }
    result
}
#[cfg(not(unix))]
fn read_file(_root: &Path, _name: &std::ffi::OsStr) -> std::io::Result<Option<Vec<u8>>> {
    Err(std::io::Error::other(
        "Native SOP bounded state IO is unavailable on this platform.",
    ))
}
#[cfg(not(unix))]
fn publish(_root: &Path, _name: &std::ffi::OsStr, _bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "Native SOP bounded state IO is unavailable on this platform.",
    ))
}

impl ApprovalPendingStore for DiskSopStore {
    fn status<'a>(
        &'a self,
        input: &'a StoreStatusInput,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            self.status(&input.session_key)
                .await
                .map_err(SopFailure::approval)
        })
    }
    fn resume<'a>(
        &'a self,
        input: &'a StoreResumeInput,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        // The bridge owns a borrowed command; move an exact clone into the
        // blocking transaction so cancellation cannot release its file gate.
        let owned = StoreResumeInput {
            session_key: input.session_key.clone(),
            project_key: input.project_key.clone(),
            request_id: input.request_id.clone(),
            wait_id: input.wait_id.clone(),
            message: input.message.clone(),
            expected_revision: input.expected_revision,
            slot_updates: input.slot_updates.clone(),
            source: input.source,
            authority: input.authority.as_ref().map(|a| super::StoreAuthority {
                tenant_id: a.tenant_id.clone(),
                session_id: a.session_id.clone(),
                subject: super::ApprovalSubject {
                    tenant_id: a.subject.tenant_id.clone(),
                    source: a.subject.source.clone(),
                    disabled: a.subject.disabled,
                    user_id: a.subject.user_id.clone(),
                    role: a.subject.role.clone(),
                },
            }),
        };
        Box::pin(async move { self.resume(owned).await.map_err(SopFailure::approval) })
    }
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
