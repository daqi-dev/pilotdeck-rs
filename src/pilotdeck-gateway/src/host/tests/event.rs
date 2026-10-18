use super::*;
use crate::host::execution::module_response;
use crate::host::modules::ModuleRouter;
use crate::host::modules::event::{EventSink, Handler, TurnIdentity};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";
const SOURCE_PATHS: [&str; 3] = [
    "src/agent/modules/transport/agentLoopSidecarClient.ts",
    "src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts",
    "src/agent/protocol/events.ts",
];
const ORACLE_JSON: &str = include_str!("../../../tests/fixtures/host-event/oracle.json");

fn oracle() -> &'static JsValue {
    static ORACLE: OnceLock<JsValue> = OnceLock::new();
    ORACLE.get_or_init(|| parse_json(ORACLE_JSON).unwrap())
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    let JsValue::Array(items) = value.get(key).unwrap() else {
        panic!("{key}")
    };
    items
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("expected a string, got {value:?}"))
        .to_utf8()
        .unwrap()
}

/// Rebuild JS property presence from the fixture's `undefined` marker.
fn materialize(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(materialize).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && object.get("__pdrsUndefined") == Some(&JsValue::Bool(true)) {
                return JsValue::Undefined;
            }
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                result.insert(key.clone(), materialize(value));
            }
            JsValue::Object(result)
        }
        value => value.clone(),
    }
}
fn drained(handler: &Handler) -> JsValue {
    JsValue::Array(handler.drain().into_iter().map(JsValue::Object).collect())
}

#[derive(Default)]
struct FakeSink {
    emitted: Mutex<Vec<JsObject>>,
    buffer: Mutex<Vec<JsObject>>,
    attempted: Mutex<usize>,
    /// `(message, fields)` for a port whose emitter throws. The fields are fed
    /// to `ModuleError::from_js_fields` exactly like a host port that projects
    /// a thrown JS error.
    failure: Option<(JsString, JsValue)>,
}
impl FakeSink {
    fn throwing(spec: &JsValue) -> Self {
        let kind = spec
            .get("kind")
            .and_then(JsValue::as_string)
            .expect("sink kind")
            .to_utf8()
            .unwrap();
        let (message, fields) = match kind.as_str() {
            "error" => (
                spec.get("message")
                    .and_then(JsValue::as_string)
                    .expect("sink message")
                    .clone(),
                spec.get("fields").cloned().unwrap_or(JsValue::Undefined),
            ),
            "string" => (
                spec.get("value")
                    .and_then(JsValue::as_string)
                    .expect("sink value")
                    .clone(),
                JsValue::Undefined,
            ),
            other => panic!("unknown sink kind {other}"),
        };
        Self {
            failure: Some((message, fields)),
            ..Self::default()
        }
    }
}
impl EventSink for FakeSink {
    fn emit(&self, event: JsObject) -> Result<(), HostError> {
        *self.attempted.lock().unwrap() += 1;
        if let Some((message, fields)) = &self.failure {
            return Err(ModuleError::from_js_fields(message.clone(), fields).into());
        }
        self.emitted.lock().unwrap().push(event.clone());
        self.buffer.lock().unwrap().push(event);
        Ok(())
    }
    fn drain(&self) -> Vec<JsObject> {
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }
}

#[tokio::test]
async fn real_ts_event_identity_and_drain() {
    let oracle = oracle();
    let cases = array(oracle, "cases");
    assert_eq!(cases.len(), 46);
    let turn = oracle.get("turn").unwrap();
    let identity = TurnIdentity::new(
        text(turn.get("sessionId").unwrap()),
        text(turn.get("turnId").unwrap()),
    );
    let mut calls = 0;
    for case in cases {
        let name = text(case.get("name").unwrap());
        let failure = case.get("sink");
        let sink = Arc::new(match failure {
            Some(spec) => FakeSink::throwing(spec),
            None => FakeSink::default(),
        });
        let handler = Handler::new(identity.clone(), sink.clone());
        for step in array(case, "steps") {
            calls += 1;
            let call = materialize(step.get("call").unwrap());
            let response = module_response(&call, handler.call(call.clone()).await);
            assert_eq!(
                &response,
                step.get("response").unwrap(),
                "{name} call={call:?}"
            );
            if let Some(expected) = step.get("drained") {
                assert_eq!(&drained(&handler), expected, "{name} drained");
            }
        }
        assert_eq!(
            &drained(&handler),
            case.get("drainAfter").unwrap(),
            "{name} drainAfter"
        );
        assert!(handler.drain().is_empty(), "{name} drains are destructive");
        let accepted = array(case, "steps")
            .iter()
            .filter(|step| {
                step.get("response")
                    .and_then(|response| response.get("ok"))
                    .and_then(JsValue::as_bool)
                    == Some(true)
            })
            .count();
        // A throwing sink receives every validated event and then rejects the
        // call, so its expected delivery count is the step count.
        let delivered = if failure.is_some() {
            array(case, "steps").len()
        } else {
            accepted
        };
        assert_eq!(
            *sink.attempted.lock().unwrap(),
            delivered,
            "{name} only validated events reach the sink"
        );
        assert_eq!(
            sink.emitted.lock().unwrap().len(),
            if failure.is_some() { 0 } else { accepted },
            "{name} throwing sinks never record events"
        );
        handler.dispose().await.unwrap();
    }
    assert_eq!(calls, 49);
}

/// `turnId` is optional by key presence: absent is accepted for session-level
/// events, while an explicitly `undefined` key still mismatches the turn.
#[tokio::test]
async fn turn_id_is_checked_by_property_presence() {
    let sink = Arc::new(FakeSink::default());
    let handler = Handler::new(TurnIdentity::new("session", "turn"), sink.clone());
    let absent = parse_json(r#"{"type":"session_started","sessionId":"session"}"#).unwrap();
    let present =
        parse_json(r#"{"type":"session_started","sessionId":"session","turnId":null}"#).unwrap();
    assert!(handler.call(event_call(absent.clone())).await.is_ok());
    assert!(handler.call(event_call(present.clone())).await.is_err());
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);

    let mut event = JsObject::new();
    event.insert("type".into(), JsValue::String("session_started".into()));
    event.insert("sessionId".into(), JsValue::String("session".into()));
    event.insert("turnId".into(), JsValue::Undefined);
    assert!(
        handler
            .call(event_call(JsValue::Object(event)))
            .await
            .is_err()
    );
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);
}

struct FailingSink;
impl EventSink for FailingSink {
    fn emit(&self, _event: JsObject) -> Result<(), HostError> {
        Err(HostError::new(HostErrorKind::Callback, "sink failed"))
    }
}

#[tokio::test]
async fn sink_failure_rejects_the_module_call_unchanged() {
    let handler = Handler::new(TurnIdentity::new("session", "turn"), Arc::new(FailingSink));
    let event =
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap();
    let error = handler.call(event_call(event)).await.unwrap_err();
    assert_eq!(error.message, "sink failed");
    assert_eq!(error.kind, HostErrorKind::Callback);
    assert!(error.module_error.is_none());
    // The trait's default drain models a port installed without a buffer.
    assert!(handler.drain().is_empty());
}

#[tokio::test]
async fn module_router_reaches_the_event_handler() {
    let sink = Arc::new(FakeSink::default());
    let router = ModuleRouter {
        event: Handler::new(TurnIdentity::new("session", "turn"), sink.clone()),
        ..ModuleRouter::default()
    };
    let call = event_call(
        parse_json(r#"{"type":"turn_started","sessionId":"session","turnId":"turn"}"#).unwrap(),
    );
    let response = module_response(&call, router.module_call(call.clone()).await);
    assert_eq!(
        response.get("payload").unwrap().get("result").unwrap(),
        &JsValue::Null
    );
    assert_eq!(sink.emitted.lock().unwrap().len(), 1);
    router.dispose().await.unwrap();
}

#[tokio::test]
async fn missing_sink_fails_closed_after_wire_validation() {
    let handler = Handler::default();
    let unsupported = request(Some(parse_json(r#"{"operation":"x"}"#).unwrap()));
    let response = module_response(&unsupported, handler.call(unsupported.clone()).await);
    assert_eq!(
        response.get("error").unwrap().get("message").unwrap(),
        &JsValue::from("Unsupported sidecar event operation.")
    );
    let missing_operation = request(None);
    let response = module_response(
        &missing_operation,
        handler.call(missing_operation.clone()).await,
    );
    assert_eq!(
        response.get("error").unwrap().get("message").unwrap(),
        &JsValue::from("Sidecar payload field operation must be a non-empty string.")
    );
    // The TS client rejects a null payload before dispatch (client:1086); at
    // the module boundary it is the same missing-field case as a missing key.
    let null_payload = request(Some(JsValue::Null));
    let response = module_response(&null_payload, handler.call(null_payload.clone()).await);
    assert_eq!(
        response.get("error").unwrap().get("message").unwrap(),
        &JsValue::from("Sidecar payload field operation must be a non-empty string.")
    );
    let emit = event_call(parse_json(r#"{"type":"x","sessionId":"session"}"#).unwrap());
    let response = module_response(&emit, handler.call(emit.clone()).await);
    assert_eq!(
        response.get("error").unwrap().get("message").unwrap(),
        &JsValue::from("Host did not provide an event sink.")
    );
    assert!(handler.drain().is_empty());
}

fn request(payload: Option<JsValue>) -> JsValue {
    let mut call = JsObject::new();
    call.insert("messageId".into(), JsValue::from("call"));
    call.insert("requestId".into(), JsValue::from("request"));
    call.insert("module".into(), JsValue::from("event"));
    if let Some(payload) = payload {
        call.insert("payload".into(), payload);
    }
    JsValue::Object(call)
}

fn event_call(event: JsValue) -> JsValue {
    let mut payload = JsObject::new();
    payload.insert("operation".into(), JsValue::from("emit"));
    payload.insert("event".into(), event);
    request(Some(JsValue::Object(payload)))
}

#[test]
fn oracle_comes_from_the_fixed_baseline() {
    let oracle = oracle();
    assert_eq!(text(oracle.get("sourceCommit").unwrap()), BASELINE_COMMIT);
    let hashes = oracle.get("sourceSha256").unwrap();
    for source in SOURCE_PATHS {
        let hash = text(hashes.get(source).unwrap());
        assert_eq!(hash.len(), 64, "missing or malformed hash for {source}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
    assert!(!ORACLE_JSON.contains(".generate-"));
    checks_oracle_against_baseline_when_available(hashes);
}

/// With `PDRS_BASELINE` set, re-run the generator's provenance checks: the
/// checkout must have no modifications to tracked files, and every recorded
/// hash must equal the blob at the pinned commit (not just the working tree).
fn checks_oracle_against_baseline_when_available(hashes: &JsValue) {
    let Ok(baseline) = std::env::var("PDRS_BASELINE") else {
        return;
    };
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&baseline)
            .args(args)
            .output()
            .expect("run git against PDRS_BASELINE");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]))
        .trim()
        .to_string();
    assert_eq!(
        head, BASELINE_COMMIT,
        "PDRS_BASELINE must be checked out at the fixed commit"
    );
    let tracked = String::from_utf8_lossy(&git(&["status", "--porcelain", "--untracked-files=no"]))
        .into_owned();
    assert!(
        tracked.is_empty(),
        "PDRS_BASELINE has uncommitted changes to tracked files:\n{tracked}"
    );
    for source in SOURCE_PATHS {
        let spec = format!("{BASELINE_COMMIT}:{source}");
        let committed = git(&["show", &spec]);
        let actual = format!("{:x}", Sha256::digest(&committed));
        assert_eq!(
            actual,
            text(hashes.get(source).unwrap()),
            "oracle hash for {source} does not match {BASELINE_COMMIT}"
        );
    }
}

/// Regenerates the oracle from the real TS dispatcher and compares bytes.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn event_fixtures_regenerate_identically_from_ts() {
    let baseline = PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE to the PilotDeck checkout"),
    );
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host-event");
    // Every invocation gets its own directory; nothing outside it is removed.
    let scratch = std::env::temp_dir().join(format!("pdrs-event-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).unwrap();
    let output = std::process::Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(fixtures.join("generate.mts"))
        .arg(&scratch)
        .env("PDRS_BASELINE", &baseline)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = std::fs::read(scratch.join("oracle.json")).unwrap();
    let committed = std::fs::read(fixtures.join("oracle.json")).unwrap();
    assert_eq!(generated, committed, "oracle.json");
    assert!(
        !scratch.join("oracle.json.tmp").exists(),
        "generator left a temporary file behind"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}
