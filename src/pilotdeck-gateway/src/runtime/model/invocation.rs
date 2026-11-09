//! Raw provider attempts (`storage/invocationStorage.ts:65-164`).
//! Staging is synchronous and durable before the provider transport runs.
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    collections::HashSet,
    future::Future,
    io::Write,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type InvocationFuture = Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;
pub trait ModelInvocationLogSink: Send + Sync {
    fn stage(&self, record: &JsValue) -> std::io::Result<()>;
    fn append(&self, record: JsValue) -> InvocationFuture;
}

#[derive(Clone)]
pub struct ModelInvocation {
    pub context: JsObject,
    pub sink: Arc<dyn ModelInvocationLogSink>,
}

#[derive(Clone)]
pub struct JsonlInvocationLogSink {
    root: PathBuf,
    appended: Arc<Mutex<HashSet<String>>>,
}
impl JsonlInvocationLogSink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            appended: Arc::new(Mutex::new(HashSet::new())),
        }
    }
    fn path_for(&self, record: &JsValue) -> std::io::Result<PathBuf> {
        Ok(self
            .root
            .join("workspaces")
            .join(safe_part(record, "workspaceId")?)
            .join("sessions")
            .join(safe_part(record, "sessionId")?)
            .join("llm/invocations.jsonl"))
    }
}
fn safe_part(record: &JsValue, key: &str) -> std::io::Result<String> {
    let value = record
        .get(key)
        .and_then(JsValue::as_string)
        .ok_or_else(|| std::io::Error::other("Invalid storage identity"))?;
    let raw = String::from_utf16_lossy(value.units());
    let raw = crate::metadata::js_trim(&raw);
    if raw.is_empty()
        || raw.contains('\0')
        || raw
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
    {
        return Err(std::io::Error::other("Invalid storage identity"));
    }
    let mut part = String::new();
    let mut dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            part.push(ch);
            dash = false;
        } else if !dash {
            part.push('-');
            dash = true;
        }
    }
    let part = part.trim_matches('-');
    if part.is_empty() || matches!(part, "." | "..") {
        return Err(std::io::Error::other("Invalid storage identity"));
    }
    Ok(part.to_owned())
}
fn encode(record: &JsValue) -> std::io::Result<String> {
    crate::js_value::json_stringify(record)
        .map_err(|_| std::io::Error::other("Invalid invocation record"))?
        .ok_or_else(|| std::io::Error::other("Invalid invocation record"))
}
impl ModelInvocationLogSink for JsonlInvocationLogSink {
    fn stage(&self, record: &JsValue) -> std::io::Result<()> {
        let path = self.path_for(record)?;
        let pending = path.parent().unwrap().join(".pending");
        std::fs::create_dir_all(&pending)?;
        let destination = pending.join(format!("{}.json", safe_part(record, "requestLogId")?));
        let temporary = pending.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(encode(record)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, destination)?;
        #[cfg(not(windows))]
        std::fs::File::open(pending)?.sync_all()?;
        Ok(())
    }
    fn append(&self, record: JsValue) -> InvocationFuture {
        let sink = self.clone();
        Box::pin(async move {
            // Serialize appends across clones. No await holds this guard.
            let mut appended = sink
                .appended
                .lock()
                .map_err(|_| std::io::Error::other("Invocation log lock poisoned"))?;
            let id = safe_part(&record, "requestLogId")?;
            if appended.contains(&id) {
                return Ok(());
            }
            let path = sink.path_for(&record)?;
            let directory = path.parent().unwrap();
            std::fs::create_dir_all(directory)?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)?;
            writeln!(file, "{}", encode(&record)?)?;
            // As in TS, the staged record is removed only after append succeeds.
            let _ = std::fs::remove_file(directory.join(".pending").join(format!("{id}.json")));
            appended.insert(id);
            Ok(())
        })
    }
}

pub(super) struct InvocationAttempt {
    invocation: ModelInvocation,
    record: JsObject,
    abort: Option<super::router::AbortFlag>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    finished: bool,
    raw: Vec<u8>,
}
impl InvocationAttempt {
    pub fn begin(
        options: &super::http::StreamOptions,
        invocation: Option<&ModelInvocation>,
        provider: &JsValue,
        model: &JsValue,
        stream: bool,
        body: &JsValue,
        attempt: f64,
    ) -> Result<Option<Self>, super::http::ModelStreamError> {
        let Some(invocation) = invocation else {
            return Ok(None);
        };
        // Audit logs the body after extraBody, including its exact UTF-16 JSON.
        let mut body = body.as_object().cloned().unwrap_or_default();
        if let Some(extra) = super::count_body::field(provider, "extraBody").as_object() {
            for (key, value) in extra.own_entries() {
                body.insert(key.clone(), value.clone());
            }
        }
        let body = JsValue::Object(body);
        let request_body = crate::js_value::json_stringify(&body)
            .map_err(|_| super::http::ModelStreamError::Thrown {
                name: "RangeError".into(),
                message: crate::js_value::StackOverflow::MESSAGE.into(),
            })?
            .unwrap_or_default();
        let mut record = invocation.context.clone();
        let started = crate::runtime::context::prompt::iso_timestamp((options.clock)());
        for (key, value) in [
            (
                "requestLogId",
                uuid::Uuid::new_v4().to_string().as_str().into(),
            ),
            (
                "requestId",
                uuid::Uuid::new_v4().to_string().as_str().into(),
            ),
            ("attempt", JsValue::Number(attempt)),
            ("provider", super::count_body::field(provider, "id").clone()),
            (
                "protocol",
                super::count_body::field(provider, "protocol").clone(),
            ),
            ("model", model.clone()),
            ("stream", JsValue::Bool(stream)),
            ("requestBytes", JsValue::Number(request_body.len() as f64)),
            ("requestBody", request_body.as_str().into()),
            ("outcome", "success".into()),
            ("responseComplete", JsValue::Bool(false)),
            ("startedAt", started.as_str().into()),
            ("completedAt", started.as_str().into()),
        ] {
            record.insert(key.into(), value);
        }
        invocation
            .sink
            .stage(&JsValue::Object(record.clone()))
            .map_err(|error| super::http::ModelStreamError::Thrown {
                name: "Error".into(),
                message: error.to_string().into(),
            })?;
        Ok(Some(Self {
            invocation: invocation.clone(),
            record,
            abort: options.abort.clone(),
            clock: options.clock.clone(),
            finished: false,
            raw: Vec::new(),
        }))
    }
    pub fn capture(&mut self, bytes: &[u8]) {
        self.raw.extend_from_slice(bytes);
    }
    pub fn status(&mut self, status: u16) {
        self.record
            .insert("httpStatus".into(), JsValue::Number(f64::from(status)));
    }
    pub fn has_status(&self) -> bool {
        self.record.get("httpStatus").is_some()
    }
    pub fn finish(
        &mut self,
        outcome: &str,
        complete: bool,
        error: Option<&super::http::ModelStreamError>,
    ) {
        if self.finished {
            return;
        }
        self.finished = true;
        let outcome = if self
            .abort
            .as_ref()
            .is_some_and(super::router::AbortFlag::is_aborted)
        {
            "aborted"
        } else if error.is_some_and(|error| error.name().equals_str("StreamIdleTimeoutError")) {
            "timeout"
        } else {
            outcome
        };
        self.record.insert("outcome".into(), outcome.into());
        self.record
            .insert("responseComplete".into(), JsValue::Bool(complete));
        self.record.insert(
            "completedAt".into(),
            crate::runtime::context::prompt::iso_timestamp((self.clock)())
                .as_str()
                .into(),
        );
        if self.record.get("httpStatus").is_some() || !self.raw.is_empty() {
            self.record.insert(
                "responseBody".into(),
                String::from_utf8_lossy(&self.raw).as_ref().into(),
            );
            self.record.insert(
                "responseBytes".into(),
                JsValue::Number(self.raw.len() as f64),
            );
        }
        let future = self
            .invocation
            .sink
            .append(JsValue::Object(self.record.clone()));
        let correlation = ["runId", "turnId", "requestLogId", "attempt"]
            .iter()
            .map(|key| {
                format!(
                    "{key}={}",
                    self.record
                        .get(key)
                        .map(|v| crate::js_value::to_string_units(v)
                            .map(|u| String::from_utf16_lossy(&u))
                            .unwrap_or_default())
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        tokio::spawn(async move {
            if let Err(error) = future.await {
                eprintln!(
                    "[PilotDeck] failed to persist model invocation audit {correlation}: {error}"
                );
            }
        });
    }
}
impl Drop for InvocationAttempt {
    fn drop(&mut self) {
        self.finish("transport_error", false, None);
    }
}

pub(super) fn capture(attempt: &mut Option<InvocationAttempt>, bytes: &[u8]) {
    if let Some(attempt) = attempt {
        attempt.capture(bytes);
    }
}
pub(super) fn finish(
    attempt: &mut Option<InvocationAttempt>,
    outcome: &str,
    complete: bool,
    error: Option<&super::http::ModelStreamError>,
) {
    if let Some(attempt) = attempt {
        attempt.finish(outcome, complete, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    #[tokio::test]
    async fn durable_stage_precedes_append_and_duplicate_terminal_is_ignored() {
        let root = std::env::temp_dir().join(format!("pdrs-invocation-{}", uuid::Uuid::new_v4()));
        let sink = JsonlInvocationLogSink::new(&root);
        let record = parse_json(r#"{"workspaceId":" w / project ","sessionId":"s","requestLogId":"log","requestBody":"{\"lone\":\"\\ud800\"}","outcome":"success","responseComplete":false}"#).unwrap();
        sink.stage(&record).unwrap();
        let directory = root.join("workspaces/w-project/sessions/s/llm");
        let pending = directory.join(".pending/log.json");
        assert_eq!(
            std::fs::read_to_string(&pending).unwrap(),
            encode(&record).unwrap()
        );
        assert!(!directory.join("invocations.jsonl").exists());
        sink.append(record.clone()).await.unwrap();
        sink.append(record.clone()).await.unwrap();
        let text = std::fs::read_to_string(directory.join("invocations.jsonl")).unwrap();
        assert_eq!(text, format!("{}\n", encode(&record).unwrap()));
        assert!(!pending.exists());
        // Isolated newly-created fixture retained for evidence; user material
        // is never removed by tests.
    }

    #[test]
    fn identity_validation_rejects_traversal_before_creating_any_path() {
        let root = std::env::temp_dir().join(format!("pdrs-invocation-{}", uuid::Uuid::new_v4()));
        let sink = JsonlInvocationLogSink::new(&root);
        for workspace in ["", "../escape", "x/../escape", "x\\.\\escape", "\0bad"] {
            let mut record = JsObject::new();
            record.insert("workspaceId".into(), workspace.into());
            record.insert("sessionId".into(), "s".into());
            record.insert("requestLogId".into(), "r".into());
            assert!(sink.stage(&JsValue::Object(record)).is_err());
        }
        assert!(!root.exists());
    }
}
