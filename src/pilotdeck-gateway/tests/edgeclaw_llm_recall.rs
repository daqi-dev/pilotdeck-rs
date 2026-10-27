//! #765 real TS oracle: retrieval-side LLM calls.
//!
//! Every case replays the TS-recorded HTTP script against a loopback stub and
//! compares the full request bytes, the method result and the debug trace.
//! The expected values are produced by `fixtures/edgeclaw-llm-recall/observe.mts`
//! importing the fixed cc7c4e6 source; nothing is synthesized here.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
mod llm_stub;

use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::llm::{
        envelope::MemoryRoute,
        recall::{
            DecideFileMemoryRouteInput, MemoryMessage, ProjectMetaRecord,
            ProjectShortlistCandidate, RecallHeaderEntry, SelectFileManifestEntriesInput,
            SelectRecallProjectInput,
        },
        transport::LlmMemoryExtractor,
    },
    model::http::HyperModelTransport,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-recall/oracle.json");

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#765 oracle/config JSON must parse")
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).expect("#765 stringify_json") {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && matches!(object.get("$undefined"), Some(JsValue::Bool(true))) {
                return JsValue::Undefined;
            }
            if object.len() == 1
                && let Some(JsValue::String(kind)) = object.get("$number")
                && let Ok(kind) = kind.to_utf8()
            {
                return JsValue::Number(match kind.as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    "-0" => -0.0,
                    other => other.parse().expect("#765 numeric sentinel"),
                });
            }
            let mut decoded = JsObject::new();
            for (key, item) in object.entries() {
                decoded.insert(key.clone(), decode(item));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}

fn normalized(value: &Value, origin: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace(origin, "$ORIGIN")),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| normalized(item, origin)).collect())
        }
        Value::Object(object) => {
            let mut out = Map::new();
            for (key, item) in object {
                out.insert(key.clone(), normalized(item, origin));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn config(origin: &str) -> JsValue {
    js(json!({
        "agents": {"defaults": {"model": {"primary": "p/m"}}},
        "models": {"providers": {"p": {
            "api": "chat",
            "baseUrl": origin,
            "apiKey": "fixture-key",
            "models": [{"id": "m"}],
        }}},
    }))
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#765 expected a string")
        .to_utf8()
        .expect("#765 expected valid UTF-8")
}

fn items<'a>(value: &'a JsValue, name: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("#765 {name} must be an array"),
    }
}

fn string_field(value: &JsValue, key: &str) -> JsString {
    value
        .get(key)
        .unwrap_or_else(|| panic!("#765 missing input field {key}"))
        .as_string()
        .unwrap_or_else(|| panic!("#765 input field {key} must be a string"))
        .clone()
}

fn optional_string(value: &JsValue, key: &str) -> Option<JsString> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .filter(|value| !value.is_empty())
}

fn optional_number(value: &JsValue, key: &str) -> Option<f64> {
    value.get(key).and_then(JsValue::as_number)
}

fn messages(value: &JsValue, key: &str) -> Vec<MemoryMessage> {
    match value.get(key) {
        Some(JsValue::Array(items)) => items.iter().map(MemoryMessage::from_value).collect(),
        Some(JsValue::Null) | None => Vec::new(),
        Some(_) => panic!("#765 input field {key} must be an array"),
    }
}

fn shortlist(value: &JsValue) -> Vec<ProjectShortlistCandidate> {
    items(
        value
            .get("shortlist")
            .unwrap_or_else(|| panic!("#765 selectRecallProject needs shortlist")),
        "shortlist",
    )
    .iter()
    .map(ProjectShortlistCandidate::from_value)
    .collect()
}

fn manifest(value: &JsValue) -> Vec<RecallHeaderEntry> {
    items(
        value
            .get("manifest")
            .unwrap_or_else(|| panic!("#765 selectFileManifestEntries needs manifest")),
        "manifest",
    )
    .iter()
    .map(RecallHeaderEntry::from_value)
    .collect()
}

fn project_meta(value: &JsValue) -> Option<ProjectMetaRecord> {
    match value.get("projectMeta") {
        Some(JsValue::Null) | None => None,
        Some(other) => Some(ProjectMetaRecord::from_value(other)),
    }
}

fn route(value: &JsValue) -> MemoryRoute {
    match value
        .get("route")
        .and_then(JsValue::as_string)
        .map(|value| value.to_utf8().expect("#765 route must be valid UTF-8"))
    {
        Some(value) if value == "user" => MemoryRoute::User,
        Some(value) if value == "project" => MemoryRoute::Project,
        Some(value) if value == "mix" => MemoryRoute::Mix,
        Some(value) if value == "none" => MemoryRoute::None,
        Some(value) => panic!("#765 unknown route {value}"),
        None => panic!("#765 missing route"),
    }
}

#[tokio::test]
async fn replay_matrix() {
    let oracle = baseline::oracle(ORACLE);
    let cases = items(
        oracle
            .get("cases")
            .unwrap_or_else(|| panic!("#765 oracle needs cases")),
        "cases",
    );
    let mut count = 0;
    for case in cases {
        let spec = case
            .get("spec")
            .unwrap_or_else(|| panic!("#765 case needs spec"));
        let name = text(
            spec.get("name")
                .unwrap_or_else(|| panic!("#765 spec needs name")),
        );
        let method = text(
            spec.get("method")
                .unwrap_or_else(|| panic!("#765 spec needs method")),
        );
        let input = spec
            .get("input")
            .unwrap_or_else(|| panic!("#765 spec needs input"));
        let steps = items(
            spec.get("steps")
                .unwrap_or_else(|| panic!("#765 spec needs steps")),
            "steps",
        )
        .iter()
        .map(|step| {
            serde_json::from_str(&wire(step))
                .unwrap_or_else(|error| panic!("#765 step {name}: {error}"))
        })
        .collect::<Vec<Value>>();
        let clock = Arc::new(AtomicU64::new(0));
        let server = LlmStub::start(steps, clock).await;
        let extractor = LlmMemoryExtractor::new(
            config(&server.origin),
            Arc::new(HyperModelTransport::new()),
            Arc::new(()),
        );
        let debug = RefCell::new(Vec::new());
        let mut sink = |value: JsValue| debug.borrow_mut().push(value);
        let actual = tokio::time::timeout(Duration::from_secs(10), async {
            match method.as_str() {
                "decideFileMemoryRoute" => {
                    let input = DecideFileMemoryRouteInput {
                        query: string_field(input, "query"),
                        recent_messages: messages(input, "recentMessages"),
                        agent_id: optional_string(input, "agentId"),
                        timeout_ms: optional_number(input, "timeoutMs"),
                    };
                    JsValue::from(
                        extractor
                            .decide_file_memory_route(&input, &mut sink)
                            .await
                            .as_str(),
                    )
                }
                "selectRecallProject" => {
                    let input = SelectRecallProjectInput {
                        query: string_field(input, "query"),
                        recent_user_messages: messages(input, "recentUserMessages"),
                        shortlist: shortlist(input),
                        allow_empty: input
                            .get("allowEmpty")
                            .and_then(JsValue::as_bool)
                            .unwrap_or(false),
                        agent_id: optional_string(input, "agentId"),
                        timeout_ms: optional_number(input, "timeoutMs"),
                    };
                    JsValue::Object(extractor.select_recall_project(&input, &mut sink).await)
                }
                "selectFileManifestEntries" => {
                    let input = SelectFileManifestEntriesInput {
                        query: string_field(input, "query"),
                        route: route(input),
                        recent_user_messages: messages(input, "recentUserMessages"),
                        project_meta: project_meta(input),
                        manifest: manifest(input),
                        limit: optional_number(input, "limit"),
                        agent_id: optional_string(input, "agentId"),
                        timeout_ms: optional_number(input, "timeoutMs"),
                    };
                    JsValue::Array(
                        extractor
                            .select_file_manifest_entries(&input, &mut sink)
                            .await
                            .into_iter()
                            .map(JsValue::String)
                            .collect(),
                    )
                }
                other => panic!("#765 unknown method {other}"),
            }
        })
        .await
        .unwrap_or_else(|_| panic!("#765 {name} timed out"));

        let expected = case
            .get("expected")
            .unwrap_or_else(|| panic!("#765 case needs expected"));
        assert_eq!(
            wire(&actual),
            wire(&decode(
                expected
                    .get("result")
                    .unwrap_or_else(|| panic!("#765 expected needs result"))
            )),
            "#765 result {name}"
        );
        let actual_requests = {
            let requests = server.requests.lock().expect("#765 stub request lock");
            serde_json::to_value(&*requests).expect("#765 requests JSON")
        };
        let expected_requests: Value = serde_json::from_str(&wire(
            expected
                .get("requests")
                .unwrap_or_else(|| panic!("#765 expected needs requests")),
        ))
        .unwrap_or_else(|error| panic!("#765 expected requests {name}: {error}"));
        assert_eq!(
            normalized(&actual_requests, &server.origin),
            normalized(&expected_requests, "$ORIGIN"),
            "#765 requests {name}"
        );
        assert_eq!(
            wire(&JsValue::Array(debug.into_inner())),
            wire(&decode(
                expected
                    .get("debug")
                    .unwrap_or_else(|| panic!("#765 expected needs debug"))
            )),
            "#765 debug {name}"
        );
        let _closed = server.closed.clone();
        server.close().await;
        count += 1;
    }
    assert_eq!(count, 41, "#765 case count");
}

#[test]
fn source_priority_matches_ts() {
    let project = |source_type: Option<&str>| ProjectShortlistCandidate {
        source_type: source_type.map(JsString::from),
        ..ProjectShortlistCandidate::default()
    };
    assert_eq!(
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::recall_project_source_priority(
            &project(Some("general_local"))
        ),
        2.0,
        "#765 general_local priority"
    );
    assert_eq!(
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::recall_project_source_priority(
            &project(Some("workspace_external_mirror"))
        ),
        2.0,
        "#765 mirror priority"
    );
    assert_eq!(
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::recall_project_source_priority(
            &project(Some("workspace_external"))
        ),
        1.0,
        "#765 external priority"
    );
    assert_eq!(
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::recall_project_source_priority(
            &project(None)
        ),
        0.0,
        "#765 missing priority"
    );
}

#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-recall");
}
