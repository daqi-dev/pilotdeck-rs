//! #768: index-side LLM calls (classify/create/select) replayed against the
//! fixed real TS baseline. Every case asserts the request wire bytes, the
//! candidate/decision result, the debug trace and the call count.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
mod llm_stub;
use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        llm::{
            index::{
                ClassifyTurnInput, IndexError, MemoryClassificationLabel, MemoryCreateKind,
                MemoryNoteInput, SelectIndexProjectInput,
            },
            transport::{LlmLogger, LlmMemoryExtractor},
        },
        messages::{MemoryMessage, MemoryRole},
    },
    model::http::HyperModelTransport,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-index/oracle.json");

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}

fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = value {
        v
    } else {
        panic!("array")
    }
}

fn utf8(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}

/// Key order is immaterial on both sides of the wire comparison.
fn sorted(value: &JsValue, origin: &str) -> JsValue {
    match value {
        JsValue::String(s) => match s.to_utf8() {
            Ok(s) => JsValue::String(s.replace(origin, "$ORIGIN").into()),
            Err(_) => value.clone(),
        },
        JsValue::Array(v) => JsValue::Array(v.iter().map(|v| sorted(v, origin)).collect()),
        JsValue::Object(o) => {
            let mut entries = o.entries().to_vec();
            entries.sort_by(|a, b| a.0.units().cmp(b.0.units()));
            let mut out = JsObject::new();
            for (k, v) in entries {
                out.insert(k, sorted(&v, origin));
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}

fn equal(actual: &JsValue, expected: &JsValue, origin: &str, name: &str) {
    assert_eq!(
        stringify_json(&sorted(actual, origin)).unwrap(),
        stringify_json(&sorted(expected, origin)).unwrap(),
        "{name}"
    );
}

fn config(spec: &Value, origin: &str) -> JsValue {
    if spec.get("noModel").is_some() {
        // The review probe's empty config: model resolution fails before any
        // request, so the fallback warning carries the resolution error.
        return js(json!({}));
    }
    js(json!({
        "agents": {"defaults": {"model": {"primary": "p/m"}}},
        "models": {"providers": {"p": {
            "api": spec["api"],
            "baseUrl": format!("{origin}/v1"),
            "apiKey": "fixture-key",
            "models": [{"id": "m"}],
        }}},
    }))
}

fn message(value: &Value) -> MemoryMessage {
    MemoryMessage {
        msg_id: None,
        role: MemoryRole::from_js(&JsString::from(value["role"].as_str().unwrap())),
        content: JsString::from(value["content"].as_str().unwrap()),
    }
}

fn messages(spec: &Value) -> Vec<MemoryMessage> {
    spec["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(message)
        .collect()
}

fn agent_id(spec: &Value) -> Option<JsString> {
    spec.get("agentId")
        .map(|agent| JsString::from(agent.as_str().unwrap()))
}

fn classification(spec: &Value) -> MemoryClassificationLabel {
    let raw = &spec["classification"];
    let kind = match raw["type"].as_str().unwrap() {
        "user" => MemoryCreateKind::User,
        "project" => MemoryCreateKind::Project,
        _ => MemoryCreateKind::Feedback,
    };
    MemoryClassificationLabel {
        kind,
        reason: JsString::from(raw["reason"].as_str().unwrap()),
        evidence: JsString::from(raw["evidence"].as_str().unwrap()),
    }
}

/// `Ok(value)`/`Err` shaped exactly like the recorded TS result.
fn outcome(result: Result<JsValue, &IndexError>) -> JsValue {
    let mut out = JsObject::new();
    match result {
        Ok(value) => {
            out.insert("ok".into(), value);
        }
        Err(error) => {
            out.insert("error".into(), JsValue::String(error.message()));
            out.insert(
                "name".into(),
                JsValue::String(
                    match error {
                        IndexError::Type(_) => "TypeError",
                        IndexError::Range => "RangeError",
                        _ => "Error",
                    }
                    .into(),
                ),
            );
        }
    }
    JsValue::Object(out)
}

/// The TS constructor's optional `logger`; every fallback warning lands here.
#[derive(Default)]
struct RecordingLogger(Mutex<Vec<JsString>>);
impl LlmLogger for RecordingLogger {
    fn warn(&self, message: &JsString) {
        self.0
            .lock()
            .expect("#768 logger lock")
            .push(message.clone());
    }
}

/// The `{field, depth}` spec marker: both sides build the deep value in code
/// because neither V8's JSON.stringify nor serde_json can record/replay it.
fn deep_value(field: &str, depth: u64) -> JsValue {
    let mut value = JsValue::Object(JsObject::new());
    for _ in 0..depth {
        let mut wrap = JsObject::new();
        wrap.insert(field.into(), value);
        value = JsValue::Object(wrap);
    }
    value
}

async fn run_case(
    spec: &Value,
    extractor: &LlmMemoryExtractor,
    debug: &mut Vec<JsValue>,
) -> JsValue {
    let batch = messages(spec);
    let focus_index = spec["focus"].get("ref").and_then(Value::as_u64);
    let standalone = if let Some(index) = spec["focus"].get("copy").and_then(Value::as_u64) {
        Some(batch[index as usize].clone())
    } else if focus_index.is_none() {
        Some(message(&spec["focus"]["value"]))
    } else {
        None
    };
    let focus: &MemoryMessage = match focus_index {
        Some(index) => &batch[index as usize],
        None => standalone.as_ref().expect("focus message"),
    };
    let meta = spec.get("meta").map(|meta| js(meta.clone())).or_else(|| {
        spec.get("metaDeep").map(|deep| {
            let field = deep["field"].as_str().expect("#768 metaDeep field");
            let mut record = JsObject::new();
            record.insert("description".into(), JsValue::String("d".into()));
            record.insert(
                field.into(),
                deep_value(field, deep["depth"].as_u64().unwrap()),
            );
            JsValue::Object(record)
        })
    });
    match spec["kind"].as_str().unwrap() {
        "classify" => {
            let input = ClassifyTurnInput {
                timestamp: JsString::from("2026-01-01T00:00:00.000Z"),
                session_key: agent_id_or_none(spec, "sessionKey"),
                focus_user_turn: focus,
                batch_context_messages: &batch,
                current_project_meta: meta.as_ref(),
                agent_id: agent_id(spec),
                timeout_ms: None,
            };
            let result = extractor
                .classify_memory_turn(&input, |event| debug.push(event))
                .await;
            outcome(Ok(result.to_value()))
        }
        "create" => {
            let label = classification(spec);
            let input = MemoryNoteInput {
                timestamp: JsString::from(
                    spec.get("timestamp")
                        .and_then(Value::as_str)
                        .unwrap_or("2026-01-01T00:00:00.000Z"),
                ),
                session_key: agent_id_or_none(spec, "sessionKey"),
                focus_user_turn: focus,
                batch_context_messages: &batch,
                current_project_meta: meta.as_ref(),
                classification: &label,
                agent_id: agent_id(spec),
                timeout_ms: None,
            };
            let result = match label.kind {
                MemoryCreateKind::User => {
                    extractor
                        .create_user_memory_note(&input, |event| debug.push(event))
                        .await
                }
                MemoryCreateKind::Project => {
                    extractor
                        .create_project_memory_note(&input, |event| debug.push(event))
                        .await
                }
                MemoryCreateKind::Feedback => {
                    extractor
                        .create_feedback_memory_note(&input, |event| debug.push(event))
                        .await
                }
            };
            match &result {
                Ok(Some(candidate)) => outcome(Ok(JsValue::Object(clone_object(candidate)))),
                Ok(None) => outcome(Ok(JsValue::Null)),
                Err(error) => outcome(Err(error)),
            }
        }
        "select" => {
            let candidate = js(spec["candidate"].clone());
            let shortlist: Vec<JsValue> = if let Some(deep) = spec.get("shortlistDeep") {
                let field = deep["field"].as_str().expect("#768 shortlistDeep field");
                let mut record = JsObject::new();
                record.insert("description".into(), JsValue::String("d".into()));
                record.insert("matchedText".into(), JsValue::String("m".into()));
                record.insert(
                    field.into(),
                    deep_value(field, deep["depth"].as_u64().unwrap()),
                );
                vec![JsValue::Object(record)]
            } else {
                spec["shortlist"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|project| js(project.clone()))
                    .collect()
            };
            let recent: Option<Vec<MemoryMessage>> = spec
                .get("recentUserMessages")
                .map(|list| list.as_array().unwrap().iter().map(message).collect());
            let input = SelectIndexProjectInput {
                candidate: &candidate,
                candidate_preview: &JsString::from(spec["candidatePreview"].as_str().unwrap()),
                focus_turn: focus,
                recent_user_messages: recent.as_deref(),
                shortlist: &shortlist,
                agent_id: agent_id(spec),
                timeout_ms: None,
            };
            let decision = extractor
                .select_index_project(&input, |event| debug.push(event))
                .await;
            outcome(Ok(decision.to_value()))
        }
        other => panic!("unknown case kind {other}"),
    }
}

fn agent_id_or_none(spec: &Value, key: &str) -> Option<JsString> {
    spec.get(key)
        .and_then(|value| value.as_str())
        .map(JsString::from)
}

fn clone_object(object: &JsObject) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in object.entries() {
        out.insert(key.clone(), value.clone());
    }
    out
}

#[tokio::test]
async fn oracle_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let mut count = 0;
    for case in array(oracle.get("cases").unwrap()) {
        let spec: Value =
            serde_json::from_str(&stringify_json(case.get("spec").unwrap()).unwrap().unwrap())
                .unwrap();
        let expected = case.get("expected").unwrap();
        let name = utf8(case.get("spec").unwrap().get("name").unwrap());
        let clock = Arc::new(AtomicU64::new(0));
        let server = LlmStub::start(
            spec["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|step| step["kind"] != "error")
                .cloned()
                .collect(),
            clock,
        )
        .await;
        let recorder = Arc::new(RecordingLogger::default());
        let extractor = LlmMemoryExtractor::new(
            config(&spec, &server.origin),
            Arc::new(HyperModelTransport::new()),
            Arc::new(()),
        )
        .with_logger(recorder.clone());
        let mut debug = Vec::new();
        // 30s, not 5s: a mutant that drops the depth gate replays a ~200MB
        // deep prompt through the stub at opt-level 0 (~5s alone), and its
        // kill must be the named result assertion — never a watchdog Elapsed.
        let actual = tokio::time::timeout(
            Duration::from_secs(30),
            run_case(&spec, &extractor, &mut debug),
        )
        .await
        .expect("bounded loopback case");
        // The wire bytes are the primary parity evidence, so they compare
        // before the derived debug trace: a prompt mutation otherwise dies
        // on `debug <case>` first, where the negative controls do not bind.
        equal(
            &actual,
            expected.get("result").unwrap(),
            &server.origin,
            &format!("result {name}"),
        );
        equal(
            &js(json!(*server.requests.lock().unwrap())),
            expected.get("requests").unwrap(),
            &server.origin,
            &format!("requests {name}"),
        );
        equal(
            &JsValue::Array(debug),
            expected.get("debug").unwrap(),
            &server.origin,
            &format!("debug {name}"),
        );
        let warnings: Vec<JsValue> = recorder
            .0
            .lock()
            .expect("#768 warnings lock")
            .iter()
            .map(|warning| JsValue::String(warning.clone()))
            .collect();
        equal(
            &JsValue::Array(warnings),
            expected.get("warnings").unwrap(),
            &server.origin,
            &format!("warnings {name}"),
        );
        let expected_calls = match expected.get("calls") {
            Some(JsValue::Number(calls)) => *calls as usize,
            _ => panic!("calls {name}"),
        };
        assert_eq!(
            server.requests.lock().unwrap().len(),
            expected_calls,
            "calls {name}"
        );
        // close() aborts every accepted connection; a recorded request always
        // leaves a stored `closed` permit to consume, so this never waits.
        let closed = server.closed.clone();
        let had_requests = !server.requests.lock().unwrap().is_empty();
        server.close().await;
        if had_requests {
            closed.notified().await;
        }
        count += 1;
    }
    assert_eq!(count, 98, "every oracle case replays");
}

#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-index");
}
