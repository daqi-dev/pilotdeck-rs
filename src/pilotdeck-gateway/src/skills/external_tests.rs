//! External skills port contract tests. The golden oracle is generated from
//! the real TS `createSkillManagementPort` against a local module host
//! (`tests/fixtures/skills-external/generate.mts`).
use super::external::ManifestWaitGate;
use super::*;
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/skills-external"
);

fn json(text: &str) -> JsValue {
    parse_json(text).expect("#929 test JSON")
}
fn compact(value: &JsValue) -> String {
    stringify_json(value)
        .expect("#929 stringify")
        .expect("#929 defined JSON")
}
fn text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}
fn string_value(value: &JsValue) -> String {
    value
        .as_string()
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}
fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert((*key).into(), value.clone());
    }
    JsValue::Object(object)
}
fn error_shape(error: &SkillError) -> JsValue {
    let mut entries: Vec<(&str, JsValue)> = vec![("name", error.name.into())];
    if !error.code.is_empty() {
        entries.push(("code", JsValue::String(error.code.clone())));
    }
    entries.push(("message", JsValue::String(error.message.clone())));
    if let Some(retryability) = error.retryability() {
        entries.push(("retryability", retryability.into()));
    }
    if let Some(details) = error.details() {
        entries.push(("details", details.clone()));
    }
    if let Some(outcome) = error.outcome() {
        entries.push(("outcome", outcome.into()));
    }
    object(&entries)
}

#[derive(Clone, Debug)]
struct Reply {
    status: u16,
    body: Vec<u8>,
}

/// Replays the oracle's HTTP replies and checks every outbound request
/// against the request the real TS port emitted.
struct ScriptedTransport {
    expected: Vec<JsValue>,
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl ScriptedTransport {
    fn new(expected: Vec<JsValue>, replies: Vec<Reply>) -> Self {
        Self {
            expected,
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn seen(&self) -> Vec<ModuleHttpRequest> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    fn exhausted(&self) -> bool {
        self.replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }
}
impl ModuleHttpTransport for ScriptedTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            let index = self
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len();
            let expected = self
                .expected
                .get(index)
                .unwrap_or_else(|| panic!("#929 request {index} has no TS oracle entry"));
            assert_eq!(
                request.method,
                text(expected, "method"),
                "#929 request {index} method must match TS"
            );
            let path = url::Url::parse(&request.url)
                .expect("#929 request URL")
                .path()
                .to_owned();
            assert_eq!(
                path,
                text(expected, "path"),
                "#929 request {index} path must match TS"
            );
            assert_eq!(
                request.content_type.map(str::to_owned),
                expected
                    .get("contentType")
                    .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
                    .and_then(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units())),
                "#929 request {index} content-type must match TS"
            );
            assert_eq!(
                request.authorization,
                expected
                    .get("authorization")
                    .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
                    .and_then(JsValue::as_string)
                    .map(|value| String::from_utf16_lossy(value.units())),
                "#929 request {index} authorization must match TS"
            );
            let expected_body = expected
                .get("body")
                .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()));
            let actual_body = (!request.body.is_empty())
                .then(|| String::from_utf8_lossy(&request.body).into_owned());
            assert_eq!(
                actual_body, expected_body,
                "#929 request {index} body must match TS byte for byte"
            );
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            let reply = self
                .replies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or_else(|| panic!("#929 no TS HTTP reply for request {index}"));
            Ok(ModuleHttpResponse {
                status: reply.status,
                body: reply.body,
            })
        })
    }
}

struct FixedEnvironment(HashMap<String, String>);
impl ModuleEnvironment for FixedEnvironment {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

fn binding_value_from_case(case: &JsValue) -> JsValue {
    let binding = case.get("binding").expect("#929 case binding");
    let mut binding = binding.clone();
    let endpoint = binding
        .get("endpoint")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
        .replace("<port>", "9");
    if let JsValue::Object(object) = &mut binding {
        object.insert("endpoint".into(), endpoint.as_str().into());
    }
    binding
}

fn binding_from_case(case: &JsValue) -> ExternalSkillBinding {
    ExternalSkillBinding::from_value(&binding_value_from_case(case))
        .expect("#929 ExternalSkillBinding")
}
fn reply_from_case(reply: &JsValue) -> Reply {
    Reply {
        status: reply
            .get("status")
            .and_then(JsValue::as_number)
            .expect("#929 reply status") as u16,
        body: text(reply, "body").into_bytes(),
    }
}

#[tokio::test]
async fn external_skills_oracle_replays_requests_and_outcomes() {
    let oracle = json(include_str!(
        "../../tests/fixtures/skills-external/oracle.json"
    ));
    let cases = oracle.get("cases").expect("#929 cases");
    let JsValue::Array(cases) = cases else {
        panic!("#929 cases must be an array")
    };
    assert!(
        cases.len() >= 24,
        "#929 expected every method's three classes"
    );
    for case in cases {
        let name = text(case, "name");
        let method = text(case, "method");
        let method = SkillMethod::ALL
            .into_iter()
            .find(|candidate| candidate.operation() == method)
            .unwrap_or_else(|| panic!("#929 unknown method {method}"));
        let binding = binding_from_case(case);
        let JsValue::Array(http) = case.get("http").expect("#929 http") else {
            panic!("#929 http must be an array for {name}")
        };
        let JsValue::Array(requests) = case.get("requests").expect("#929 requests") else {
            panic!("#929 requests must be an array for {name}")
        };
        let replies: Vec<Reply> = http.iter().map(reply_from_case).collect();
        let transport = Arc::new(ScriptedTransport::new(requests.clone(), replies));
        let env = Arc::new(FixedEnvironment(
            case.get("env")
                .and_then(JsValue::as_object)
                .map(|object| {
                    object
                        .entries()
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((
                                String::from_utf16_lossy(key.units()),
                                String::from_utf16_lossy(value.as_string()?.units()),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ));
        let port = ExternalSkillsPort::with_environment(binding, transport.clone(), env);
        let input = case.get("input").cloned().unwrap_or(JsValue::Undefined);
        let actual = match port.call(method, &input).await {
            Ok(result) => object(&[("result", result)]),
            Err(error) => object(&[("error", error_shape(&error))]),
        };
        let expected = case.get("outcome").expect("#929 outcome");
        assert_eq!(
            compact(&actual),
            compact(expected),
            "#929 case {name} outcome must match real TS"
        );
        assert!(
            transport.exhausted(),
            "#929 case {name} must consume every TS HTTP reply"
        );
        let seen = transport.seen();
        assert_eq!(
            seen.len(),
            requests.len(),
            "#929 case {name} request count must match real TS"
        );
    }
}

/// One project-key registry: `registry.resolve(projectKey ?? projectRoot)`.
fn snapshot(binding: JsValue) -> JsValue {
    object(&[(
        "config",
        object(&[("modules", object(&[("skills", binding)]))]),
    )])
}

fn external_binding(implementation: &str, prefix: &str, methods: &[&str]) -> JsValue {
    let methods: Vec<JsValue> = methods.iter().map(|method| (*method).into()).collect();
    object(&[
        ("enabled", JsValue::Bool(true)),
        ("implementationId", implementation.into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("endpoint", "http://127.0.0.1:9".into()),
        (
            "manifestPath",
            format!("{prefix}/module-manifest").as_str().into(),
        ),
        (
            "callPath",
            format!("{prefix}/v2/module/call").as_str().into(),
        ),
        ("methods", JsValue::Array(methods)),
        ("timeoutMs", JsValue::Number(2000.0)),
    ])
}

/// Answers a valid manifest for the requested prefix and a successful call
/// whose result records the manifest prefix. Used to prove which project's
/// snapshot binding the facade resolved.
pub(super) struct HarnessTransport {
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl HarnessTransport {
    pub(super) fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
        }
    }
    pub(super) fn seen(&self) -> Vec<ModuleHttpRequest> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
impl ModuleHttpTransport for HarnessTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            let path = url::Url::parse(&request.url)
                .expect("#929 harness URL")
                .path()
                .to_owned();
            if request.method == "GET" {
                let prefix = path.trim_end_matches("/module-manifest");
                let body = format!(
                    r#"{{"protocolVersion":"2.0","implementationId":"{prefix}.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list","read","write","create","delete","import","validate","scan"]}}"#
                );
                return Ok(ModuleHttpResponse {
                    status: 200,
                    body: body.into_bytes(),
                });
            }
            let body = parse_json(&String::from_utf8_lossy(&request.body)).expect("#929 call body");
            let message_id = text(&body, "messageId");
            let request_id = text(&body, "requestId");
            let operation = text(body.get("payload").expect("#929 payload"), "operation");
            let prefix = path.trim_end_matches("/v2/module/call");
            let response = object(&[
                ("kind", "response".into()),
                ("messageId", "module-response-1".into()),
                ("inReplyTo", message_id.as_str().into()),
                ("requestId", request_id.as_str().into()),
                ("ok", JsValue::Bool(true)),
                (
                    "payload",
                    object(&[(
                        "result",
                        object(&[
                            ("operation", operation.as_str().into()),
                            ("manifestPrefix", prefix.into()),
                        ]),
                    )]),
                ),
            ]);
            Ok(ModuleHttpResponse {
                status: 200,
                body: compact(&response).into_bytes(),
            })
        })
    }
}

fn harness_binding_for(key: Option<&JsValue>) -> super::Result<JsValue> {
    let key = key
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default();
    let binding = match key.as_str() {
        "/p/a" => external_binding(
            "/a.skills",
            "/a",
            &[
                "list", "read", "write", "create", "delete", "import", "validate", "scan",
            ],
        ),
        "/p/b" => external_binding(
            "/b.skills",
            "/b",
            &[
                "list", "read", "write", "create", "delete", "import", "validate", "scan",
            ],
        ),
        _ => return Ok(JsValue::Undefined),
    };
    skill_binding_from_snapshot(&snapshot(binding))
}

#[tokio::test]
async fn facade_routes_each_project_to_its_snapshot_binding() {
    let scratch = std::env::temp_dir().join(format!("pdrs-929-facade-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).expect("#929 scratch");
    let manager = SkillManager::new(super::SkillManagerOptions::new(
        scratch.to_string_lossy().into_owned(),
    ))
    .expect("#929 manager");
    let transport = Arc::new(HarnessTransport::new());
    let facade = SkillsFacade::with_transport(manager, harness_binding_for, transport.clone());
    let a = facade
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p/a"}"#))
        .await
        .expect("#929 project A external list must succeed");
    let seen_a = transport.seen();
    assert_eq!(
        seen_a.len(),
        2,
        "#929 project A must hit its own module host"
    );
    assert!(
        seen_a[0].url.ends_with("/a/module-manifest")
            && seen_a[1].url.ends_with("/a/v2/module/call"),
        "#929 project A must use its own snapshot generation"
    );
    assert_eq!(
        string_value(a.get("manifestPrefix").expect("#929 result prefix")),
        "/a",
        "#929 facade must resolve project A's snapshot binding"
    );
    let b = facade
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p/b"}"#))
        .await
        .expect("#929 project B external list must succeed");
    let seen = transport.seen();
    assert_eq!(seen.len(), 4, "#929 each project needs manifest + call");
    assert!(
        seen[2].url.ends_with("/b/module-manifest") && seen[3].url.ends_with("/b/v2/module/call"),
        "#929 project B must use its own snapshot generation"
    );
    assert_eq!(
        string_value(b.get("manifestPrefix").expect("#929 result prefix")),
        "/b",
        "#929 facade must resolve project B's snapshot binding"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[tokio::test]
async fn external_manifest_is_loaded_once_per_port() {
    let manifest = object(&[
        ("protocolVersion", "2.0".into()),
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        (
            "methods",
            JsValue::Array(vec!["list".into(), "read".into()]),
        ),
    ]);
    let call_reply = |body: &JsValue| {
        let message_id = text(body, "messageId");
        let request_id = text(body, "requestId");
        let response = object(&[
            ("kind", "response".into()),
            ("messageId", "m".into()),
            ("inReplyTo", message_id.as_str().into()),
            ("requestId", request_id.as_str().into()),
            ("ok", JsValue::Bool(true)),
            (
                "payload",
                object(&[("result", object(&[("ok", JsValue::Bool(true))]))]),
            ),
        ]);
        compact(&response).into_bytes()
    };
    let transport = Arc::new(DynamicTransport {
        manifest: compact(&manifest).into_bytes(),
        call: Box::new(call_reply),
        seen: Mutex::new(Vec::new()),
    });
    let binding =
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list", "read"]))
            .expect("#929 binding");
    let port = ExternalSkillsPort::new(binding, transport.clone());
    let list_input = json(r#"{"projectKey":"/p"}"#);
    let read_input = json(r#"{"projectKey":"/p","slug":"x","scope":"user"}"#);
    let first = port.call(SkillMethod::List, &list_input);
    let second = port.call(SkillMethod::Read, &read_input);
    let (a, b) = tokio::join!(first, second);
    a.expect("#929 concurrent list");
    b.expect("#929 concurrent read");
    let seen = transport
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let manifests = seen
        .iter()
        .filter(|request| request.method == "GET")
        .count();
    assert_eq!(manifests, 1, "#929 one shared manifest load per port");
    let mut ids: Vec<String> = seen
        .iter()
        .filter(|request| request.method == "POST")
        .map(|request| {
            let body = parse_json(&String::from_utf8_lossy(&request.body)).expect("#929 body");
            text(&body, "messageId")
        })
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["module-http-1", "module-http-2"],
        "#929 client counter"
    );
}

type ReplyBody = Box<dyn Fn(&JsValue) -> Vec<u8> + Send + Sync>;

struct DynamicTransport {
    manifest: Vec<u8>,
    call: ReplyBody,
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl ModuleHttpTransport for DynamicTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            if request.method == "GET" {
                return Ok(ModuleHttpResponse {
                    status: 200,
                    body: self.manifest.clone(),
                });
            }
            let body = parse_json(&String::from_utf8_lossy(&request.body)).expect("#929 call body");
            Ok(ModuleHttpResponse {
                status: 200,
                body: (self.call)(&body),
            })
        })
    }
}

#[test]
#[ignore = "needs PDRS_BASELINE; regenerates oracle.json byte-identically"]
fn external_skills_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("skills-external");
}

#[test]
fn fixture_paths_are_the_documented_ones() {
    assert!(Path::new(FIXTURES).join("generate.mts").exists());
    assert!(Path::new(FIXTURES).join("oracle.json").exists());
}

/// End-to-end success path on the production transport (not the replay fake).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hyper_transport_reaches_a_real_module_host() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#929 module host bind");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("#929 module host addr")
    );
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let service = service_fn(|request: Request<Incoming>| async move {
                    let path = request.uri().path().to_owned();
                    let body = request
                        .into_body()
                        .collect()
                        .await
                        .expect("#929 module host body")
                        .to_bytes();
                    let response = if path == "/module-manifest" {
                        r#"{"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]}"#.to_owned()
                    } else {
                        let request = parse_json(&String::from_utf8_lossy(&body))
                            .expect("#929 module host request");
                        let response = object(&[
                            ("kind", "response".into()),
                            ("messageId", "m".into()),
                            ("inReplyTo", text(&request, "messageId").as_str().into()),
                            ("requestId", text(&request, "requestId").as_str().into()),
                            ("ok", JsValue::Bool(true)),
                            (
                                "payload",
                                object(&[("result", object(&[("operation", "list".into())]))]),
                            ),
                        ]);
                        compact(&response)
                    };
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(response))))
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let binding = ExternalSkillBinding::from_value(&object(&[
        ("enabled", JsValue::Bool(true)),
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("endpoint", endpoint.as_str().into()),
        ("manifestPath", "/module-manifest".into()),
        ("callPath", "/v2/module/call".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
        ("timeoutMs", JsValue::Number(2000.0)),
    ]))
    .expect("#929 real binding");
    let port = ExternalSkillsPort::new(binding, Arc::new(HyperModuleTransport::new()));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#)),
    )
    .await
    .expect("#929 real HTTP call must terminate")
    .expect("#929 real HTTP call must succeed");
    assert_eq!(text(&result, "operation"), "list", "#929 real HTTP result");
    server.abort();
}

#[test]
fn snapshot_projection_applies_the_optional_chain() {
    let binding = external_binding("test.skills", "", &["list"]);
    assert_eq!(
        compact(&skill_binding_from_snapshot(&snapshot(binding.clone())).expect("#929 snapshot")),
        compact(&binding),
        "#929 snapshot projection must return modules.skills"
    );
    let modules_missing = object(&[("config", object(&[]))]);
    assert!(
        matches!(
            skill_binding_from_snapshot(&modules_missing).expect("#929 missing modules"),
            JsValue::Undefined
        ),
        "#929 modules?.skills is undefined when modules is absent"
    );
    let modules_null = object(&[("config", object(&[("modules", JsValue::Null)]))]);
    assert!(
        matches!(
            skill_binding_from_snapshot(&modules_null).expect("#929 null modules"),
            JsValue::Undefined
        ),
        "#929 modules?.skills is undefined when modules is null"
    );
    let config_missing = object(&[]);
    assert_eq!(
        skill_binding_from_snapshot(&config_missing)
            .expect_err("#929 missing config must throw")
            .name,
        "TypeError",
        "#929 config.modules throws when config is absent"
    );
}

struct TimeoutTransport;
impl ModuleHttpTransport for TimeoutTransport {
    fn send<'a>(&'a self, _request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async { Err(ModuleTransportError::Timeout) })
    }
}

#[tokio::test]
async fn timeout_maps_to_module_timeout() {
    let binding = ExternalSkillBinding::from_value(&object(&[
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("endpoint", "http://127.0.0.1:9".into()),
        ("manifestPath", "/module-manifest".into()),
        ("callPath", "/v2/module/call".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
        ("timeoutMs", JsValue::Number(250.0)),
    ]))
    .expect("#929 timeout binding");
    let port = ExternalSkillsPort::new(binding, Arc::new(TimeoutTransport));
    let error = port
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
        .await
        .expect_err("#929 timeout must fail");
    assert_eq!(
        String::from_utf16_lossy(error.code.units()),
        "MODULE_TIMEOUT",
        "#929 timeout keeps MODULE_TIMEOUT"
    );
    assert_eq!(
        String::from_utf16_lossy(error.message.units()),
        "Module HTTP request exceeded 250ms.",
        "#929 timeout message uses the configured timeout"
    );
    assert_eq!(
        error.outcome(),
        Some("result_unknown"),
        "#929 timeout outcome"
    );
}

/// The real TS golden through the facade + snapshot projection seam. The
/// per-method replay above drives the port directly; this case proves the
/// `snapshot.config.modules?.skills` binding reaches the same port.
#[tokio::test]
async fn facade_replays_the_ts_list_success_case() {
    let oracle = json(include_str!(
        "../../tests/fixtures/skills-external/oracle.json"
    ));
    let JsValue::Array(cases) = oracle.get("cases").expect("#929 cases") else {
        panic!("#929 cases must be an array")
    };
    let case = cases
        .iter()
        .find(|case| text(case, "name") == "list-success")
        .expect("#929 list-success case");
    let binding = binding_value_from_case(case);
    let snapshot = snapshot(binding);
    let JsValue::Array(http) = case.get("http").expect("#929 http") else {
        panic!("#929 http must be an array")
    };
    let JsValue::Array(requests) = case.get("requests").expect("#929 requests") else {
        panic!("#929 requests must be an array")
    };
    let transport = Arc::new(ScriptedTransport::new(
        requests.clone(),
        http.iter().map(reply_from_case).collect(),
    ));
    let scratch = std::env::temp_dir().join(format!("pdrs-929-golden-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).expect("#929 scratch");
    let manager = SkillManager::new(super::SkillManagerOptions::new(
        scratch.to_string_lossy().into_owned(),
    ))
    .expect("#929 manager");
    let facade = SkillsFacade::with_transport(
        manager,
        move |_key: Option<&JsValue>| skill_binding_from_snapshot(&snapshot),
        transport.clone(),
    );
    let input = case.get("input").cloned().unwrap_or(JsValue::Undefined);
    let result = facade
        .call(SkillMethod::List, &input)
        .await
        .expect("#929 facade list-success must succeed");
    assert_eq!(
        compact(&object(&[("result", result)])),
        compact(case.get("outcome").expect("#929 outcome")),
        "#929 facade list-success outcome must match real TS"
    );
    assert!(
        transport.exhausted(),
        "#929 facade must consume every TS reply"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Local module host for the real-transport body cases: a valid manifest
/// GET, then a 2xx call response whose body stalls forever, ends before its
/// declared `Content-Length`, or sends an over-limit prefix before stalling.
#[derive(Clone, Copy)]
enum BodyHostMode {
    Stall,
    Truncate,
    Oversize,
}

async fn spawn_body_host(mode: BodyHostMode) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#929 body host bind");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("#929 body host addr")
    );
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..read]);
                if head.starts_with("GET ") {
                    let body = r#"{"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                } else {
                    match mode {
                        BodyHostMode::Stall | BodyHostMode::Truncate => {
                            // Head is complete, body is not: 100 declared
                            // bytes, a 9-byte prefix, then either a stall or
                            // an early EOF.
                            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"kind\":";
                            let _ = stream.write_all(response.as_bytes()).await;
                            match mode {
                                BodyHostMode::Stall => {
                                    tokio::time::sleep(Duration::from_secs(30)).await
                                }
                                BodyHostMode::Truncate => {
                                    let _ = stream.shutdown().await;
                                }
                                BodyHostMode::Oversize => unreachable!(),
                            }
                        }
                        BodyHostMode::Oversize => {
                            // 300 bytes arrive in one frame, but 400 are
                            // declared, so the body stays open. A 256-byte test
                            // limit (above the 139-byte manifest) makes this
                            // an over-limit prefix followed by a stall.
                            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 400\r\n\r\n";
                            let _ = stream.write_all(head.as_bytes()).await;
                            let _ = stream.write_all(&[b' '; 300]).await;
                            tokio::time::sleep(Duration::from_secs(30)).await
                        }
                    }
                }
            });
        }
    });
    (endpoint, server)
}

/// Two 80 ms redirect hops, then a 2xx head whose 150 ms-late body would be
/// valid JSON. Each leg is well under a 250 ms `timeoutMs`; only a shared
/// deadline can classify the exchange as `MODULE_TIMEOUT`.
async fn spawn_slow_redirect_host() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#929 redirect host bind");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("#929 redirect host addr")
    );
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..read]);
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                if path == "/module-manifest" {
                    let body = r#"{"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                } else if path == "/v2/module/call" || path == "/hop1" {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let next = if path == "/v2/module/call" {
                        "/hop1"
                    } else {
                        "/hop2"
                    };
                    let response = format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {next}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                } else {
                    // The call's first and only messageId/requestId are
                    // deterministic after the manifest GET.
                    let body = r#"{"kind":"response","messageId":"module-http-1","inReplyTo":"module-http-1","requestId":"skills-management-list-1","ok":true,"payload":{"result":{"operation":"list"}}}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                }
            });
        }
    });
    (endpoint, server)
}

fn runtime_binding(endpoint: &str, timeout_ms: f64) -> ExternalSkillBinding {
    ExternalSkillBinding::from_value(&object(&[
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("endpoint", endpoint.into()),
        ("manifestPath", "/module-manifest".into()),
        ("callPath", "/v2/module/call".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
        ("timeoutMs", JsValue::Number(timeout_ms)),
    ]))
    .expect("#929 runtime binding")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_body_is_bounded_by_the_request_timeout() {
    let (endpoint, server) = spawn_body_host(BodyHostMode::Stall).await;
    let port = ExternalSkillsPort::new(
        runtime_binding(&endpoint, 250.0),
        Arc::new(HyperModuleTransport::new()),
    );
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#)),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "#929 stalled body must return within timeoutMs instead of waiting forever"
    );
    let error = outcome
        .expect("#929 stalled body outcome")
        .expect_err("#929 stalled body must fail with MODULE_TIMEOUT");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "#929 stalled body must fail at the configured deadline"
    );
    assert!(
        error.code.equals_str("MODULE_TIMEOUT"),
        "#929 stalled body keeps MODULE_TIMEOUT, got {:?}",
        error.code
    );
    assert_eq!(
        String::from_utf16_lossy(error.message.units()),
        "Module HTTP request exceeded 250ms.",
        "#929 stalled body timeout message"
    );
    assert_eq!(
        error.outcome(),
        Some("result_unknown"),
        "#929 stalled body timeout outcome"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_success_body_is_a_protocol_error() {
    let (endpoint, server) = spawn_body_host(BodyHostMode::Truncate).await;
    let port = ExternalSkillsPort::new(
        runtime_binding(&endpoint, 2000.0),
        Arc::new(HyperModuleTransport::new()),
    );
    let error = port
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
        .await
        .expect_err("#929 truncated 2xx body must fail");
    assert!(
        error.code.equals_str("MODULE_PROTOCOL_INCOMPATIBLE"),
        "#929 truncated 2xx body keeps the protocol code, got {:?}",
        error.code
    );
    assert_eq!(
        String::from_utf16_lossy(error.message.units()),
        "Module HTTP response must contain JSON.",
        "#929 truncated 2xx body message"
    );
    assert_eq!(
        error.outcome(),
        None,
        "#929 a 2xx body decode failure is not a transport outcome"
    );
    server.abort();
}

/// Node v22.22.0 probe (`_tmp/20261026-issue-929/fix-r2/ts-probe-overlimit.out`):
/// an over-limit prefix that leaves the connection open still ends in
/// `MODULE_TIMEOUT`; the string-length error only decides after EOF. The
/// transport therefore must not stop reading at the first over-limit frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_limit_body_keeps_the_timeout_priority() {
    let (endpoint, server) = spawn_body_host(BodyHostMode::Oversize).await;
    let port = ExternalSkillsPort::new(
        runtime_binding(&endpoint, 250.0),
        Arc::new(HyperModuleTransport::with_body_limit(256)),
    );
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#)),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "#929 over-limit body must stay bounded by the request deadline instead of waiting forever"
    );
    let error = outcome
        .expect("#929 over-limit body outer deadline")
        .expect_err("#929 over-limit body must fail with a module error");
    assert!(
        error.code.equals_str("MODULE_TIMEOUT"),
        "#929 over-limit body must reach the shared deadline and MODULE_TIMEOUT, not an early protocol error; \
         #929 over-limit body keeps the timeout priority, got {:?} message {:?} after {:?}",
        error.code,
        error.message,
        started.elapsed()
    );
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "#929 over-limit body must be drained until the shared deadline, not stopped at the first over-limit frame"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "#929 over-limit body must still fail at the configured deadline"
    );
    server.abort();
}

/// Two 80 ms redirect hops plus a 150 ms body: every leg is under `timeoutMs`
/// but the total is over it, so the single `setTimeout` deadline must fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_and_body_share_one_deadline() {
    let (endpoint, server) = spawn_slow_redirect_host().await;
    let port = ExternalSkillsPort::new(
        runtime_binding(&endpoint, 250.0),
        Arc::new(HyperModuleTransport::new()),
    );
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#)),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "#929 redirects plus body must be bounded by the one request deadline"
    );
    let error = outcome
        .expect("#929 redirects plus body outer deadline")
        .expect_err("#929 redirects plus body must return MODULE_TIMEOUT, not restart the deadline for the body");
    assert!(
        error.code.equals_str("MODULE_TIMEOUT"),
        "#929 redirects plus body keep MODULE_TIMEOUT, got {:?}",
        error.code
    );
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "#929 the shared deadline must cover both redirect hops and the body"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "#929 redirects plus body must still fail at the configured deadline"
    );
    server.abort();
}

struct RecordingTransport {
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl ModuleHttpTransport for RecordingTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            Err(ModuleTransportError::Unavailable)
        })
    }
}

#[tokio::test]
async fn oversized_timeout_uses_the_node_timer_clamp() {
    for (input, scheduled) in [
        (2_147_483_647.0, 2_147_483_647u64),
        (2_147_483_648.0, 1),
        (1_000_000_000_000.0, 1),
    ] {
        let transport = Arc::new(RecordingTransport {
            seen: Mutex::new(Vec::new()),
        });
        let port = ExternalSkillsPort::new(
            runtime_binding("http://127.0.0.1:9", input),
            transport.clone(),
        );
        let error = port
            .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
            .await
            .expect_err("#929 manifest must fail for the timeout probe");
        assert!(
            error.code.equals_str("MODULE_TRANSPORT_UNAVAILABLE"),
            "#929 timeout probe keeps the transport code"
        );
        let seen = transport
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = seen
            .first()
            .expect("#929 timeout probe must record the manifest request");
        assert_eq!(
            request.timeout,
            Duration::from_millis(scheduled),
            "#929 timeoutMs {input} must schedule the Node timer delay {scheduled}ms"
        );
    }
}

#[tokio::test]
async fn bom_is_stripped_from_manifest_and_response() {
    let manifest = object(&[
        ("protocolVersion", "2.0".into()),
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
    ]);
    let transport = Arc::new(DynamicTransport {
        manifest: format!("\u{feff}{}", compact(&manifest)).into_bytes(),
        call: Box::new(|body| {
            let response = object(&[
                ("kind", "response".into()),
                ("messageId", "m".into()),
                ("inReplyTo", text(body, "messageId").as_str().into()),
                ("requestId", text(body, "requestId").as_str().into()),
                ("ok", JsValue::Bool(true)),
                (
                    "payload",
                    object(&[("result", object(&[("operation", "list".into())]))]),
                ),
            ]);
            format!("\u{feff}{}", compact(&response)).into_bytes()
        }),
        seen: Mutex::new(Vec::new()),
    });
    let port = ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 BOM binding"),
        transport,
    );
    let result = port
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
        .await
        .expect("#929 BOM-prefixed manifest and response must parse");
    assert_eq!(
        text(&result, "operation"),
        "list",
        "#929 BOM response payload"
    );
}

#[tokio::test]
async fn non_response_messages_get_the_full_protocol_validation() {
    let manifest = object(&[
        ("protocolVersion", "2.0".into()),
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
    ]);
    for (body, expected) in [
        (r#"{"kind":"error"}"#, "Message messageId is required."),
        (
            r#"{"kind":"request","messageId":"m","method":"module_call","payload":{}}"#,
            "Module call field runId is required.",
        ),
        (r#"{"kind":"event"}"#, "Event field eventType is required."),
        (
            r#"{"kind":"error","messageId":"m","code":"E","message":"m","retryability":"maybe"}"#,
            "Error retryability is invalid.",
        ),
        (r#"{"kind":"bogus"}"#, "Module message kind is invalid."),
    ] {
        let payload = body.as_bytes().to_vec();
        let transport = Arc::new(DynamicTransport {
            manifest: compact(&manifest).into_bytes(),
            call: Box::new(move |_| payload.clone()),
            seen: Mutex::new(Vec::new()),
        });
        let port = ExternalSkillsPort::new(
            ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
                .expect("#929 protocol binding"),
            transport,
        );
        let error = port
            .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
            .await
            .expect_err("#929 non-response message must fail protocol validation");
        assert!(
            error.code.equals_str("MODULE_PROTOCOL_INCOMPATIBLE"),
            "#929 protocol validation keeps the protocol code for {body}"
        );
        assert_eq!(
            String::from_utf16_lossy(error.message.units()),
            expected,
            "#929 protocol validation message for {body}"
        );
    }
}

#[tokio::test]
async fn provider_error_keeps_unpaired_surrogates() {
    let manifest = object(&[
        ("protocolVersion", "2.0".into()),
        ("implementationId", "test.skills".into()),
        ("contract", "pilotdeck.skills/v1".into()),
        ("transport", "module-http-v2".into()),
        ("methods", JsValue::Array(vec!["list".into()])),
    ]);
    let transport = Arc::new(DynamicTransport {
        manifest: compact(&manifest).into_bytes(),
        call: Box::new(|body| {
            format!(
                r#"{{"kind":"response","messageId":"provider-response","inReplyTo":"{}","requestId":"{}","ok":false,"code":"PROVIDER\ud800","error":{{"code":"PROVIDER\ud800","message":"bad\ud800","retryability":"safe"}}}}"#,
                text(body, "messageId"),
                text(body, "requestId")
            )
            .into_bytes()
        }),
        seen: Mutex::new(Vec::new()),
    });
    let port = ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 surrogate binding"),
        transport,
    );
    let error = port
        .call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
        .await
        .expect_err("#929 provider error must surface");
    assert_eq!(
        error.code.units(),
        &[80, 82, 79, 86, 73, 68, 69, 82, 0xd800],
        "#929 provider code keeps the unpaired surrogate"
    );
    assert_eq!(
        error.message.units(),
        &[b'b' as u16, b'a' as u16, b'd' as u16, 0xd800],
        "#929 provider message keeps the unpaired surrogate"
    );
    assert_eq!(
        error.retryability(),
        Some("safe"),
        "#929 provider retryability projection"
    );
}

struct FlightTransport {
    manifest_calls: std::sync::atomic::AtomicUsize,
    post_calls: std::sync::atomic::AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl ModuleHttpTransport for FlightTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            if request.method == "GET" {
                let call = self
                    .manifest_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    self.started.notify_one();
                    self.release.notified().await;
                    return Err(ModuleTransportError::Unavailable);
                }
                let body = r#"{"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]}"#;
                return Ok(ModuleHttpResponse {
                    status: 200,
                    body: body.as_bytes().to_vec(),
                });
            }
            self.post_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body =
                parse_json(&String::from_utf8_lossy(&request.body)).expect("#929 flight body");
            let response = object(&[
                ("kind", "response".into()),
                ("messageId", "m".into()),
                ("inReplyTo", text(&body, "messageId").as_str().into()),
                ("requestId", text(&body, "requestId").as_str().into()),
                ("ok", JsValue::Bool(true)),
                (
                    "payload",
                    object(&[("result", object(&[("operation", "list".into())]))]),
                ),
            ]);
            Ok(ModuleHttpResponse {
                status: 200,
                body: compact(&response).into_bytes(),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_manifest_failure_is_shared_by_every_waiter() {
    let transport = Arc::new(FlightTransport {
        manifest_calls: std::sync::atomic::AtomicUsize::new(0),
        post_calls: std::sync::atomic::AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        seen: Mutex::new(Vec::new()),
    });
    let port = Arc::new(ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 flight binding"),
        transport.clone(),
    ));
    let first = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), transport.started.notified())
        .await
        .expect("#929 first manifest GET must start");
    let second = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while port.manifest_waiters() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#929 second call must park on the in-flight manifest");
    transport.release.notify_one();
    let first = first.await.expect("#929 first join");
    let second = second.await.expect("#929 second join");
    let first = first.expect_err("#929 first manifest failure must surface to its owner");
    let second = second.expect_err(
        "#929 first manifest failure must surface to the concurrent waiter; \
         #929 concurrent waiter must receive the same manifest failure",
    );
    assert!(
        first.code.equals_str("MODULE_TRANSPORT_UNAVAILABLE")
            && second.code.equals_str("MODULE_TRANSPORT_UNAVAILABLE"),
        "#929 both waiters keep the shared failure code"
    );
    assert_eq!(
        transport
            .manifest_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#929 concurrent calls share exactly one manifest GET"
    );
    assert_eq!(
        transport
            .post_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "#929 a failed manifest must not reach the call POST"
    );
    port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
        .await
        .expect("#929 a call after the failed round must start a new flight");
    assert_eq!(
        transport
            .manifest_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "#929 failure clears the flight so the next call retries"
    );
}

struct CancelTransport {
    manifest_calls: std::sync::atomic::AtomicUsize,
    post_calls: std::sync::atomic::AtomicUsize,
    started: tokio::sync::Notify,
    panic_release: tokio::sync::Notify,
    panic_first: bool,
    seen: Mutex<Vec<ModuleHttpRequest>>,
}
impl ModuleHttpTransport for CancelTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            if request.method == "GET" {
                let call = self
                    .manifest_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    self.started.notify_one();
                    if self.panic_first {
                        // Panic only after the waiter has parked in the
                        // pre-notification window.
                        self.panic_release.notified().await;
                        panic!("#929 test owner panics before settling its manifest flight");
                    }
                    // The first owner never completes; aborting the call must
                    // release its flight.
                    std::future::pending::<()>().await;
                }
                let body = r#"{"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]}"#;
                return Ok(ModuleHttpResponse {
                    status: 200,
                    body: body.as_bytes().to_vec(),
                });
            }
            self.post_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body =
                parse_json(&String::from_utf8_lossy(&request.body)).expect("#929 cancel body");
            let response = object(&[
                ("kind", "response".into()),
                ("messageId", "m".into()),
                ("inReplyTo", text(&body, "messageId").as_str().into()),
                ("requestId", text(&body, "requestId").as_str().into()),
                ("ok", JsValue::Bool(true)),
                (
                    "payload",
                    object(&[("result", object(&[("operation", "list".into())]))]),
                ),
            ]);
            Ok(ModuleHttpResponse {
                status: 200,
                body: compact(&response).into_bytes(),
            })
        })
    }
}

fn cancel_transport(panic_first: bool) -> Arc<CancelTransport> {
    Arc::new(CancelTransport {
        manifest_calls: std::sync::atomic::AtomicUsize::new(0),
        post_calls: std::sync::atomic::AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        panic_release: tokio::sync::Notify::new(),
        panic_first,
        seen: Mutex::new(Vec::new()),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_manifest_owner_releases_its_waiters() {
    let transport = cancel_transport(false);
    let port = Arc::new(ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 cancel binding"),
        transport.clone(),
    ));
    let first = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), transport.started.notified())
        .await
        .expect("#929 first manifest GET must start");
    let second = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while port.manifest_waiters() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("#929 second call must park on the in-flight manifest");
    first.abort();
    let _ = first.await;
    tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("#929 waiter must recover when the in-flight manifest owner is cancelled (bounded)")
        .expect("#929 waiter join")
        .expect("#929 waiter must load the manifest after the owner is cancelled");
    assert_eq!(
        transport
            .manifest_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "#929 the surviving waiter owns the replacement manifest GET"
    );
    assert_eq!(
        transport
            .post_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#929 the surviving waiter reaches its POST"
    );
}

/// P0-1 deterministic interleaving: the waiter has cloned the pending flight
/// and released the manifest lock, but has not created its `Notified` yet.
/// The owner is aborted in exactly that window; the old `Drop` notified with
/// no registered waiter and left no terminal state, so the waiter parked
/// forever. The waiter must now observe `Abandoned`, re-enter the cache and
/// become the replacement owner within the outer 2 s bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_owner_before_waiter_notification_is_recovered() {
    let transport = cancel_transport(false);
    let port = Arc::new(ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 P0 cancel-window binding"),
        transport.clone(),
    ));
    let gate = Arc::new(ManifestWaitGate::new());
    port.install_manifest_wait_gate(gate.clone());
    let first = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), transport.started.notified())
        .await
        .expect("#929 P0 first manifest GET must start");
    let second = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), gate.reached.notified())
        .await
        .expect("#929 P0 waiter must stop after cloning the flight and before creating Notified");
    first.abort();
    let first_join = first.await;
    assert!(
        first_join.is_err(),
        "#929 P0 the owner task must be cancelled before it settles the flight"
    );
    gate.release.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("#929 P0 a waiter that cloned the flight before owner cancellation must return within 2s");
    joined
        .expect("#929 P0 waiter task must not panic")
        .expect("#929 P0 waiter must load the manifest after the owner is cancelled");
    assert_eq!(
        transport
            .manifest_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "#929 P0 the recovered waiter owns the replacement manifest GET"
    );
    assert_eq!(
        transport
            .post_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#929 P0 the recovered waiter reaches its POST"
    );
    assert_eq!(
        port.manifest_waiters(),
        0,
        "#929 P0 the waiter counter must return to zero after recovery"
    );
}

/// P0-1 panic path: the owner unwinds while the waiter sits in the same
/// pre-notification window. Unwinding runs `FlightOwner::drop`, so it must
/// publish `Abandoned` too; the waiter must recover within the outer bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicked_owner_before_waiter_notification_is_recovered() {
    let transport = cancel_transport(true);
    let port = Arc::new(ExternalSkillsPort::new(
        ExternalSkillBinding::from_value(&external_binding("test.skills", "", &["list"]))
            .expect("#929 P0 panic-window binding"),
        transport.clone(),
    ));
    let gate = Arc::new(ManifestWaitGate::new());
    port.install_manifest_wait_gate(gate.clone());
    let first = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), transport.started.notified())
        .await
        .expect("#929 P0 panicking first manifest GET must start");
    let second = {
        let port = port.clone();
        tokio::spawn(async move {
            port.call(SkillMethod::List, &json(r#"{"projectKey":"/p"}"#))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), gate.reached.notified())
        .await
        .expect("#929 P0 waiter must stop after cloning the flight and before creating Notified");
    transport.panic_release.notify_one();
    let first_join = first.await;
    assert!(
        first_join.is_err(),
        "#929 P0 the owner task must have panicked before settling the flight"
    );
    gate.release.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .expect("#929 P0 a waiter that cloned the flight before owner panic must return within 2s");
    joined
        .expect("#929 P0 waiter task must not panic")
        .expect("#929 P0 waiter must load the manifest after the owner panicked");
    assert_eq!(
        transport
            .manifest_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "#929 P0 the recovered waiter owns the replacement manifest GET after a panic"
    );
    assert_eq!(
        transport
            .post_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#929 P0 the recovered waiter reaches its POST after a panic"
    );
    assert_eq!(
        port.manifest_waiters(),
        0,
        "#929 P0 the waiter counter must return to zero after a panic"
    );
}
