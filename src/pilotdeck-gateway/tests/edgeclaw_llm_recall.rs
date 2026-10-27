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
        transport::{LlmLogger, LlmMemoryExtractor},
    },
    model::http::HyperModelTransport,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-recall/oracle.json");

/// Records the TS `logger?.warn?.(...)` calls and their position relative to
/// `debugTrace`, so warning content/order are compared with the real baseline.
struct RecordingLogger {
    warnings: Mutex<Vec<JsString>>,
    events: Arc<Mutex<Vec<String>>>,
}
impl RecordingLogger {
    fn new(events: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            warnings: Mutex::new(Vec::new()),
            events,
        }
    }
}
impl LlmLogger for RecordingLogger {
    fn warn(&self, message: &JsString) {
        self.warnings
            .lock()
            .expect("#765 warning lock")
            .push(message.clone());
        self.events
            .lock()
            .expect("#765 warning event lock")
            .push("warn".into());
    }
}

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
        let input_value = decode(
            spec.get("input")
                .unwrap_or_else(|| panic!("#765 spec needs input")),
        );
        let input = &input_value;
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
        let empty_config = matches!(
            spec.get("config").and_then(JsValue::as_string),
            Some(value) if value.equals_str("empty")
        );
        let config = if empty_config {
            js(json!({}))
        } else {
            config(&server.origin)
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let logger = Arc::new(RecordingLogger::new(events.clone()));
        let extractor =
            LlmMemoryExtractor::new(config, Arc::new(HyperModelTransport::new()), Arc::new(()))
                .with_logger(logger.clone());
        let debug = RefCell::new(Vec::new());
        let mut sink = |value: JsValue| {
            debug.borrow_mut().push(value);
            events
                .lock()
                .expect("#765 debug event lock")
                .push("debug".into());
        };
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
        let actual_warnings = logger
            .warnings
            .lock()
            .expect("#765 warnings lock")
            .iter()
            .cloned()
            .map(JsValue::String)
            .collect();
        assert_eq!(
            wire(&JsValue::Array(actual_warnings)),
            wire(&decode(
                expected
                    .get("warnings")
                    .unwrap_or_else(|| panic!("#765 expected needs warnings"))
            )),
            "#765 warnings {name}"
        );
        let actual_events = events
            .lock()
            .expect("#765 events lock")
            .iter()
            .cloned()
            .map(|event| JsValue::String(JsString::from(event)))
            .collect();
        assert_eq!(
            wire(&JsValue::Array(actual_events)),
            wire(&decode(
                expected
                    .get("events")
                    .unwrap_or_else(|| panic!("#765 expected needs events"))
            )),
            "#765 events {name}"
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
    assert_eq!(count, 51, "#765 case count");
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

#[tokio::test]
async fn fallback_warnings_match_ts() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let logger = Arc::new(RecordingLogger::new(events.clone()));
    let extractor = LlmMemoryExtractor::new(
        js(json!({})),
        Arc::new(HyperModelTransport::new()),
        Arc::new(()),
    )
    .with_logger(logger.clone());
    let debug: RefCell<Vec<JsValue>> = RefCell::new(Vec::new());
    let mut sink = |value: JsValue| {
        debug.borrow_mut().push(value);
        events
            .lock()
            .expect("#765 warning debug event lock")
            .push("debug".into());
    };

    let gate = extractor
        .decide_file_memory_route(
            &DecideFileMemoryRouteInput {
                query: "Say hello".into(),
                ..DecideFileMemoryRouteInput::default()
            },
            &mut sink,
        )
        .await;
    assert_eq!(gate, MemoryRoute::None, "#765 gate warning result");

    let shortlist = vec![ProjectShortlistCandidate {
        project_id: "p1".into(),
        project_name: "Alpha".into(),
        ..ProjectShortlistCandidate::default()
    }];
    let selected = extractor
        .select_recall_project(
            &SelectRecallProjectInput {
                query: "Alpha".into(),
                shortlist,
                ..SelectRecallProjectInput::default()
            },
            &mut sink,
        )
        .await;
    assert_eq!(
        selected.get("projectId").and_then(JsValue::as_string),
        Some(&JsString::from("p1")),
        "#765 project selection warning result"
    );

    let selected_ids = extractor
        .select_file_manifest_entries(
            &SelectFileManifestEntriesInput {
                query: "Alpha".into(),
                route: MemoryRoute::Project,
                recent_user_messages: Vec::new(),
                project_meta: None,
                manifest: Vec::new(),
                limit: None,
                agent_id: None,
                timeout_ms: None,
            },
            &mut sink,
        )
        .await;
    assert!(selected_ids.is_empty(), "#765 selection warning result");

    let warnings: Vec<String> = logger
        .warnings
        .lock()
        .expect("#765 fallback warning lock")
        .iter()
        .map(|warning| warning.to_utf8().expect("#765 fallback warning UTF-8"))
        .collect();
    let expected = [
        "[clawxmemory] file memory gate fallback: Error: Could not resolve an OpenClaw model for memory extraction",
        "[clawxmemory] file memory project selection fallback: Error: Could not resolve an OpenClaw model for memory extraction",
        "[clawxmemory] file memory selection fallback: Error: Could not resolve an OpenClaw model for memory extraction",
    ];
    assert_eq!(warnings.len(), 3, "#765 one warning per fallback");
    assert_eq!(warnings[0], expected[0], "#765 gate warning");
    assert_eq!(warnings[1], expected[1], "#765 project selection warning");
    assert_eq!(warnings[2], expected[2], "#765 selection warning");
    assert_eq!(
        *events.lock().expect("#765 warning order lock"),
        ["debug", "warn", "debug", "warn", "debug", "warn"],
        "#765 warning order"
    );
    let records = debug.into_inner();
    assert_eq!(records.len(), 3, "#765 warning debug count");
    for (index, record) in records.iter().enumerate() {
        assert!(
            !wire(record).contains("[clawxmemory]"),
            "#765 warning must not enter debug {index}"
        );
    }
}

/// Hand-checked edge cases on the reachable producer domain: `exact` 0/2,
/// `score = exact * 10 + matched token count`, finite values, ±0 and ties. The
/// full TS TimSort oracle is `js_fallback_sort_matches_ts_reachable_domain`.
///
/// NaN is deliberately not pinned here. `buildProjectShortlist` cannot produce
/// it, and a NaN comparator is not a total order, so V8 TimSort and Rust's
/// stable sort are not required to pick the same first element. The old NaN
/// cells were removed by #765 fix-r4; the reachable domain is locked by
/// `js_fallback_sort_matches_ts_reachable_domain` and the NaN boundary is
/// documented on `choose_best_recall_project_fallback`.
#[test]
fn js_fallback_sort_matches_ts() {
    let project = |id: &str, exact: f64, score: f64| ProjectShortlistCandidate {
        project_id: id.into(),
        project_name: id.into(),
        source_type: Some("general_local".into()),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        exact,
        score,
        ..ProjectShortlistCandidate::default()
    };
    let picked = |shortlist: &[ProjectShortlistCandidate]| {
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::choose_best_recall_project_fallback(
            shortlist,
        )
        .expect("#765 sort sample needs a candidate")
        .project_id
        .to_utf8()
        .expect("#765 sort project id must be UTF-8")
    };

    // TS `!==` on numbers treats -0 and +0 as equal, so the comparison falls
    // through to the finite score.
    assert_eq!(
        picked(&[project("a", -0.0, 2.0), project("b", 0.0, 1.0)]),
        "a",
        "#765 minus zero and plus zero are equal"
    );
    // Reachable finite values: descending exact, then descending score.
    assert_eq!(
        picked(&[project("a", 2.0, 20.0), project("b", 0.0, 9.0)]),
        "a",
        "#765 reachable exact order"
    );
    assert_eq!(
        picked(&[project("a", 0.0, 3.0), project("b", 0.0, 9.0)]),
        "b",
        "#765 reachable score order"
    );
    // A full tie (same exact, score, source type and updatedAt): `Array#sort`
    // is stable, so the earlier element wins.
    assert_eq!(
        picked(&[project("a", 0.0, 3.0), project("b", 0.0, 3.0)]),
        "a",
        "#765 full tie preserves input order"
    );
}

const JS_SORT_ORACLE: &str = include_str!("fixtures/edgeclaw-llm-recall-js-sort/oracle.json");

/// Locks the fallback ordering against the real TS TimSort on the input domain
/// the product can actually produce (`exact` 0/2, `score = exact * 10 + matched
/// token count`). Mixed NaN lists are reachable only by bypassing
/// `buildProjectShortlist`; on those the TS comparator is not a total order and
/// V8 and Rust may pick different first elements, so that domain is not pinned
/// here (see `js_sort_order`).
#[test]
fn js_fallback_sort_matches_ts_reachable_domain() {
    let oracle = baseline::oracle(JS_SORT_ORACLE);
    let domain = oracle.get("domain").expect("#765 js-sort needs domain");
    let strings = |key: &str| -> Vec<JsString> {
        items(
            domain
                .get(key)
                .unwrap_or_else(|| panic!("#765 js-sort needs domain.{key}")),
            key,
        )
        .iter()
        .map(|value| {
            value
                .as_string()
                .unwrap_or_else(|| panic!("#765 js-sort domain.{key} must hold strings"))
                .clone()
        })
        .collect()
    };
    let source_types = strings("sourceType");
    let updated_at = strings("updatedAt");
    let cases = items(
        oracle.get("cases").expect("#765 js-sort needs cases"),
        "cases",
    );
    let mut checked = 0;
    for case in cases {
        let name = string_field(case, "name")
            .to_utf8()
            .expect("#765 js-sort case name must be UTF-8");
        let expected = string_field(case, "picked")
            .to_utf8()
            .expect("#765 js-sort picked must be UTF-8");
        let rows = items(
            case.get("candidates")
                .unwrap_or_else(|| panic!("#765 js-sort {name} needs candidates")),
            "candidates",
        );
        let mut shortlist = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let values = items(row, "candidate");
            assert_eq!(
                values.len(),
                4,
                "#765 js-sort {name} row {index} must have 4 columns"
            );
            let number = |column: usize| {
                values[column].as_number().unwrap_or_else(|| {
                    panic!("#765 js-sort {name} row {index} column {column} must be a number")
                })
            };
            let score = number(0);
            let exact = number(1);
            let source = number(2);
            let updated = number(3);
            assert!(
                score.is_finite(),
                "#765 js-sort {name} row {index} score must be finite"
            );
            assert!(
                exact == 0.0 || exact == 2.0,
                "#765 js-sort {name} row {index} exact must be 0 or 2"
            );
            let tokens = score - exact * 10.0;
            assert!(
                (0.0..=12.0).contains(&tokens) && tokens.fract() == 0.0,
                "#765 js-sort {name} row {index} score must be exact*10 + 0..=12 matched tokens"
            );
            assert!(
                source >= 0.0 && source < source_types.len() as f64,
                "#765 js-sort {name} row {index} source index out of range"
            );
            assert!(
                updated >= 0.0 && updated < updated_at.len() as f64,
                "#765 js-sort {name} row {index} updatedAt index out of range"
            );
            shortlist.push(ProjectShortlistCandidate {
                project_id: format!("p{index}").into(),
                project_name: format!("P{index}").into(),
                updated_at: updated_at[updated as usize].clone(),
                source_type: Some(source_types[source as usize].clone()),
                score,
                exact,
                source: "query".into(),
                ..ProjectShortlistCandidate::default()
            });
        }
        let picked = pilotdeck_gateway::runtime::memory::edgeclaw::llm::recall::choose_best_recall_project_fallback(
            &shortlist,
        )
        .unwrap_or_else(|| panic!("#765 js-sort {name} needs a candidate"))
        .project_id
        .to_utf8()
        .expect("#765 js-sort picked project id must be UTF-8");
        assert_eq!(
            picked, expected,
            "#765 reachable-domain sort first mismatch at {name}: picked {picked}, TS picked {expected}"
        );
        checked += 1;
    }
    assert!(
        checked >= 200,
        "#765 js-sort needs at least 200 cases, got {checked}"
    );
}

#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-recall");
}

#[test]
#[ignore]
fn regenerates_js_sort_identically() {
    baseline::regenerates_identically("edgeclaw-llm-recall-js-sort");
}
