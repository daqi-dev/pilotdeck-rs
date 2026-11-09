//! Exercise production preparation and HTTP execution with distinct auto/fixed models.
use super::*;
use crate::host::HostHandler;
use crate::runtime::network::Cancellation;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Response, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::parse_json;
use std::{convert::Infallible, time::Duration};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub(super) fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

pub(super) fn snapshot(base_url: &str, with_default: bool) -> JsValue {
    let provider = |id: &str, model: &str, path: &str| {
        object([
            ("id", id.into()),
            ("protocol", "openai".into()),
            ("url", format!("{base_url}/{path}/v1").as_str().into()),
            ("apiKey", "public-auto-route-fixture".into()),
            (
                "retry",
                object([("streamMaxRetries", JsValue::Number(0.0))]),
            ),
            (
                "models",
                object([(
                    model,
                    object([
                        ("id", model.into()),
                        (
                            "multimodal",
                            object([("input", JsValue::Array(vec!["text".into()]))]),
                        ),
                        (
                            "capabilities",
                            object([
                                ("supportsStreaming", JsValue::Bool(true)),
                                ("supportsToolUse", JsValue::Bool(true)),
                                ("maxOutputTokens", JsValue::Number(64.0)),
                            ]),
                        ),
                    ]),
                )]),
            ),
        ])
    };
    let scenarios = if with_default {
        object([(
            "default",
            object([("provider", "proof".into()), ("model", "alternate".into())]),
        )])
    } else {
        object([])
    };
    object([(
        "config",
        object([
            (
                "agent",
                object([("model", "project-proof/project-only".into())]),
            ),
            (
                "model",
                object([(
                    "providers",
                    object([
                        (
                            "project-proof",
                            provider("project-proof", "project-only", "project"),
                        ),
                        ("proof", provider("proof", "alternate", "auto")),
                    ]),
                )]),
            ),
            (
                "router",
                object([
                    ("enabled", JsValue::Bool(true)),
                    ("scenarios", scenarios),
                    ("stats", object([("enabled", JsValue::Bool(false))])),
                ]),
            ),
        ]),
    )])
}

pub(super) fn host_input(source: &str) -> JsValue {
    object([
        ("sessionId", "auto-session".into()),
        ("turnId", "auto-turn".into()),
        ("modelSelectionSource", source.into()),
        ("agent", object([("cwd", "/tmp".into())])),
    ])
}

pub(super) fn request(provider: &str, model: &str, text: &str) -> JsValue {
    object([
        ("provider", provider.into()),
        ("model", model.into()),
        (
            "messages",
            JsValue::Array(vec![object([
                ("role", "user".into()),
                (
                    "content",
                    JsValue::Array(vec![object([
                        ("type", "text".into()),
                        ("text", text.into()),
                    ])]),
                ),
            ])]),
        ),
        ("maxOutputTokens", JsValue::Number(8.0)),
    ])
}

pub(super) fn callback(operation: &str, request: JsValue, forged_source: &str) -> JsValue {
    object([
        ("module", "model".into()),
        (
            "payload",
            object([
                ("operation", operation.into()),
                ("preparationId", "prepared-auto".into()),
                ("request", request),
                ("modelSelectionSource", forged_source.into()),
                (
                    "context",
                    object([
                        ("modelSelectionSource", forged_source.into()),
                        (
                            "metadata",
                            object([("modelSelectionSource", forged_source.into())]),
                        ),
                    ]),
                ),
            ]),
        ),
    ])
}

async fn call(router: &ModuleRouter, operation: &str, request: JsValue, source: &str) -> JsObject {
    timeout(
        Duration::from_secs(5),
        router.module_call(callback(operation, request, source)),
    )
    .await
    .expect("production model callback must settle")
    .expect("production model callback must succeed")
}

struct HttpFixture {
    base_url: String,
    requests: Arc<Mutex<Vec<(String, JsValue)>>>,
    task: JoinHandle<()>,
}
impl HttpFixture {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let recorded = recorded.clone();
                let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let recorded = recorded.clone();
                    async move {
                        let path = request.uri().path().to_owned();
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        let body = parse_json(std::str::from_utf8(&body).unwrap()).unwrap();
                        recorded.lock().unwrap().push((path, body));
                        let body = concat!(
                            "data: {\"id\":\"auto-proof\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"routed\"},\"finish_reason\":null}]}\n\n",
                            "data: {\"id\":\"auto-proof\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
                            "data: [DONE]\n\n",
                        );
                        Ok::<_, Infallible>(
                            Response::builder()
                                .header("content-type", "text/event-stream")
                                .header("connection", "close")
                                .body(Full::new(Bytes::from_static(body.as_bytes())))
                                .unwrap(),
                        )
                    }
                });
                http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .unwrap();
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn assert_streamed(response: &JsObject) {
    let Some(JsValue::Array(events)) = response.get("events") else {
        panic!("stream must return events");
    };
    assert!(
        events
            .iter()
            .any(|event| event.get("type") == Some(&"text_delta".into())),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.get("type") == Some(&"error".into())),
        "{events:?}"
    );
}

#[tokio::test]
async fn auto_prepares_router_default_and_streams_saved_selection_over_http() {
    let fixture = HttpFixture::start().await;
    let router = build(
        snapshot(&fixture.base_url, true),
        &host_input("router"),
        Arc::new(EventBuffer::default()),
        Cancellation::default(),
    )
    .unwrap();
    let prepared = call(
        &router,
        "prepare",
        request("project-proof", "project-only", "initial"),
        "turn",
    )
    .await;
    let prepared = prepared.get("prepared").unwrap();
    assert_eq!(prepared.get("provider"), Some(&"proof".into()));
    assert_eq!(prepared.get("model"), Some(&"alternate".into()));
    assert!(prepared.get("decision").is_none());
    assert!(prepared.get("request").unwrap().get("decision").is_none());
    assert!(
        prepared
            .get("request")
            .unwrap()
            .get("modelSelectionSource")
            .is_none()
    );
    let streamed = call(
        &router,
        "stream",
        request("project-proof", "project-only", "replacement"),
        "default",
    )
    .await;
    assert_streamed(&streamed);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let (path, body) = &requests[0];
    assert_eq!(path, "/auto/v1/chat/completions");
    assert_eq!(body.get("model"), Some(&"alternate".into()));
    let Some(JsValue::Array(messages)) = body.get("messages") else {
        panic!("model messages");
    };
    assert_eq!(messages[0].get("content"), Some(&"replacement".into()));
    assert!(body.get("decision").is_none());
    assert!(body.get("modelSelectionSource").is_none());
}

#[tokio::test]
async fn explicit_sources_ignore_router_default_and_forged_callback_source() {
    let fixture = HttpFixture::start().await;
    for source in ["turn", "session", "default"] {
        let router = build(
            snapshot(&fixture.base_url, true),
            &host_input(source),
            Arc::new(EventBuffer::default()),
            Cancellation::default(),
        )
        .unwrap();
        let prepared = call(
            &router,
            "prepare",
            request("project-proof", "project-only", source),
            "router",
        )
        .await;
        let prepared = prepared.get("prepared").unwrap();
        assert_eq!(prepared.get("provider"), Some(&"project-proof".into()));
        assert_eq!(prepared.get("model"), Some(&"project-only".into()));
        let streamed = call(
            &router,
            "stream",
            request("proof", "alternate", source),
            "router",
        )
        .await;
        assert_streamed(&streamed);
    }
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (path, body) in requests.iter() {
        assert_eq!(path, "/project/v1/chat/completions");
        assert_eq!(body.get("model"), Some(&"project-only".into()));
        assert!(body.get("modelSelectionSource").is_none());
    }
}

#[tokio::test]
async fn only_auto_requires_a_router_decision_and_retains_its_error_code() {
    for source in ["router", "turn", "session", "default"] {
        let router = build(
            snapshot("http://127.0.0.1:1", false),
            &host_input(source),
            Arc::new(EventBuffer::default()),
            Cancellation::default(),
        )
        .unwrap();
        let result = router
            .module_call(callback(
                "prepare",
                request("project-proof", "project-only", source),
                "router",
            ))
            .await;
        if source == "router" {
            let error = result.unwrap_err();
            assert_eq!(
                error.module_error.unwrap().code,
                Some("router_no_default".into())
            );
        } else {
            assert!(result.is_ok(), "{source}: {result:?}");
        }
    }
}

#[tokio::test]
async fn cancelled_auto_stream_does_not_start_an_http_request() {
    let fixture = HttpFixture::start().await;
    let cancel = Cancellation::default();
    let router = build(
        snapshot(&fixture.base_url, true),
        &host_input("router"),
        Arc::new(EventBuffer::default()),
        cancel.clone(),
    )
    .unwrap();
    call(
        &router,
        "prepare",
        request("project-proof", "project-only", "cancelled"),
        "turn",
    )
    .await;
    cancel.abort();
    let result = call(
        &router,
        "stream",
        request("project-proof", "project-only", "cancelled"),
        "turn",
    )
    .await;
    assert_eq!(result.get("events"), Some(&JsValue::Array(Vec::new())));
    assert!(fixture.requests.lock().unwrap().is_empty());
}
