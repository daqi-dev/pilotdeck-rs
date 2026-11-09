//! Real HTTP exchanges with a gate between SSE chunks; no timing-based release.
use super::auto_route_tests::{callback, host_input, object, request, snapshot};
use super::*;
use crate::host::HostHandler;
use crate::runtime::model::router::InvokeErrorKind;
use pilotdeck_protocol::parse_json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

struct EmbeddedOnlySpawner;
impl super::super::sidecar_session::SidecarSpawner for EmbeddedOnlySpawner {
    fn command(&self) -> std::io::Result<tokio::process::Command> {
        panic!("embedded production path must not request a subprocess")
    }
}

const FIRST: &str = "data: {\"id\":\"judge\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"complex\"},\"finish_reason\":null}]}\n\n";
const END: &str = "data: {\"id\":\"judge\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\ndata: [DONE]\n\n";
const FAILURE: &str = "data: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"judge fixture denied\"}}\n\n";
const COMPLETE_FIRST: &str =
    "{\"id\":\"judge\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"complex\"},";
const COMPLETE_END: &str = "\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}";

struct GatedHttp {
    url: String,
    received: Option<oneshot::Receiver<JsValue>>,
    release: Option<oneshot::Sender<()>>,
    disconnected: Option<oneshot::Receiver<()>>,
    task: JoinHandle<()>,
}

async fn chunk(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    stream
        .write_all(format!("{:x}\r\n{text}\r\n", text.len()).as_bytes())
        .await
}

impl GatedHttp {
    async fn start(first: &'static str, tail: &'static str) -> Self {
        Self::start_with_status(first, tail, 200).await
    }

    async fn start_with_status(first: &'static str, tail: &'static str, status: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (received_tx, received) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let (disconnected_tx, disconnected) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body_start = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request ended before headers");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .expect("request content length");
            while bytes.len() < body_start + length {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request ended before body");
                bytes.extend_from_slice(&buffer[..count]);
            }
            let body =
                parse_json(std::str::from_utf8(&bytes[body_start..body_start + length]).unwrap())
                    .unwrap();
            received_tx.send(body).unwrap();
            let headers = format!(
                "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(headers.as_bytes()).await.is_err() {
                let _ = disconnected_tx.send(());
                return;
            }
            if !first.is_empty() && chunk(&mut stream, first).await.is_err() {
                let _ = disconnected_tx.send(());
                return;
            }
            let mut closed = [0; 1];
            tokio::select! {
                result = stream.read(&mut closed) => {
                    assert!(matches!(result, Ok(0) | Err(_)), "unexpected client bytes");
                    let _ = disconnected_tx.send(());
                }
                _ = release_rx => {
                    if !tail.is_empty() { let _ = chunk(&mut stream, tail).await; }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                }
            }
        });
        Self {
            url,
            received: Some(received),
            release: Some(release),
            disconnected: Some(disconnected),
            task,
        }
    }

    async fn received(&mut self) -> JsValue {
        timeout(Duration::from_secs(5), self.received.take().unwrap())
            .await
            .unwrap()
            .unwrap()
    }

    fn release(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }

    async fn disconnected(&mut self) {
        timeout(Duration::from_secs(5), self.disconnected.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for GatedHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn invoker(url: &str) -> SnapshotInvocation {
    let snapshot = snapshot(url, true);
    SnapshotInvocation {
        invocation: None,
        model_config: snapshot
            .get("config")
            .unwrap()
            .get("model")
            .unwrap()
            .clone(),
        snapshot,
        transport: Arc::new(HyperModelTransport::new()),
    }
}

fn judge_request() -> JsValue {
    let mut request = request("proof", "alternate", "classify this")
        .as_object()
        .unwrap()
        .clone();
    request.insert(
        "thinking".into(),
        object([("enabled", JsValue::Bool(false))]),
    );
    request.insert("maxOutputTokens".into(), JsValue::Number(256.0));
    request.insert("stream".into(), JsValue::Bool(false));
    JsValue::Object(request)
}

#[tokio::test]
async fn judge_complete_uses_real_http_and_returns_content_finish_and_usage() {
    let mut server = GatedHttp::start(COMPLETE_FIRST, COMPLETE_END).await;
    let invoker = invoker(&server.url);
    assert!(invoker.supports_complete());
    let complete = tokio::spawn(invoker.complete(judge_request(), None));
    let body = server.received().await;
    assert_eq!(body.get("model"), Some(&"alternate".into()));
    assert_eq!(body.get("stream"), Some(&JsValue::Bool(false)));
    assert_eq!(body.get("max_tokens"), Some(&JsValue::Number(256.0)));
    assert!(
        !complete.is_finished(),
        "complete must await terminal chunk"
    );
    server.release();
    let response = timeout(Duration::from_secs(5), complete)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        response.get("content"),
        Some(&JsValue::Array(vec![object([
            ("type", "text".into()),
            ("text", "complex".into())
        ])]))
    );
    assert_eq!(response.get("finishReason"), Some(&"stop".into()));
    assert_eq!(
        response.get("usage").unwrap().get("totalTokens"),
        Some(&JsValue::Number(12.0))
    );
}

#[tokio::test]
async fn judge_complete_cancels_inflight_http_and_closes_socket() {
    let mut server = GatedHttp::start("", END).await;
    let cancel = AbortFlag::new();
    let complete =
        tokio::spawn(invoker(&server.url).complete(judge_request(), Some(cancel.clone())));
    server.received().await;
    cancel.abort();
    let error = timeout(Duration::from_secs(5), complete)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, InvokeErrorKind::Other);
    assert!(error.message.to_utf8().unwrap().contains("aborted"));
    server.disconnected().await;
}

#[tokio::test]
async fn judge_complete_propagates_provider_failure_instead_of_partial_success() {
    let mut server = GatedHttp::start_with_status(
        "",
        r#"{"error":{"type":"invalid_request_error","message":"judge fixture denied"}}"#,
        400,
    )
    .await;
    let complete = tokio::spawn(invoker(&server.url).complete(judge_request(), None));
    server.received().await;
    server.release();
    let error = timeout(Duration::from_secs(5), complete)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, InvokeErrorKind::Provider);
    assert_eq!(error.code, Some("invalid_request_error".into()));
    assert_eq!(error.message, JsString::from("judge fixture denied"));
    assert!(!error.retryable);
}

#[tokio::test]
async fn enabled_token_saver_prepares_from_real_judge_response() {
    let mut server = GatedHttp::start(COMPLETE_FIRST, COMPLETE_END).await;
    let mut snapshot = snapshot(&server.url, true).as_object().unwrap().clone();
    let mut config = snapshot.get("config").unwrap().as_object().unwrap().clone();
    let mut router = config.get("router").unwrap().as_object().unwrap().clone();
    router.insert(
        "tokenSaver".into(),
        object([
            ("enabled", JsValue::Bool(true)),
            (
                "judge",
                object([("provider", "proof".into()), ("model", "alternate".into())]),
            ),
            ("defaultTier", "simple".into()),
            (
                "tiers",
                object([
                    (
                        "simple",
                        object([(
                            "model",
                            object([
                                ("provider", "project-proof".into()),
                                ("model", "project-only".into()),
                            ]),
                        )]),
                    ),
                    (
                        "complex",
                        object([(
                            "model",
                            object([("provider", "proof".into()), ("model", "alternate".into())]),
                        )]),
                    ),
                ]),
            ),
        ]),
    );
    config.insert("router".into(), JsValue::Object(router));
    snapshot.insert("config".into(), JsValue::Object(config));
    let host = build(
        JsValue::Object(snapshot),
        &host_input("router"),
        Arc::new(EventBuffer::default()),
        AbortFlag::new(),
    )
    .unwrap();
    let prepare = tokio::spawn(host.module_call(callback(
        "prepare",
        request("project-proof", "project-only", "implement a parser"),
        "turn",
    )));
    server.received().await;
    server.release();
    let result = timeout(Duration::from_secs(5), prepare)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        result.get("prepared").unwrap().get("model"),
        Some(&"alternate".into())
    );
}

#[tokio::test]
async fn embedded_production_turn_publishes_only_authoritative_completion() {
    use super::super::{
        handle::LiveSession,
        sidecar_session::{SidecarSession, SidecarSessionConfig},
    };

    let mut server = GatedHttp::start(FIRST, END).await;
    let runtime_snapshot = snapshot(&server.url, true);
    let mut root = runtime_snapshot.as_object().unwrap().clone();
    let mut config = root.get("config").unwrap().as_object().unwrap().clone();
    let mut model_config = config.get("model").unwrap().as_object().unwrap().clone();
    let mut providers = model_config
        .get("providers")
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    for provider in ["project-proof", "proof"] {
        let model = if provider == "proof" {
            "alternate"
        } else {
            "project-only"
        };
        let mut provider_config = providers
            .get(provider)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        let mut models = provider_config
            .get("models")
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        let mut model_config = models.get(model).unwrap().as_object().unwrap().clone();
        let mut capabilities = model_config
            .get("capabilities")
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        capabilities.insert("maxContextTokens".into(), JsValue::Number(8192.0));
        capabilities.insert("supportsSystemPrompt".into(), JsValue::Bool(true));
        model_config.insert("capabilities".into(), JsValue::Object(capabilities));
        models.insert(model.into(), JsValue::Object(model_config));
        provider_config.insert("models".into(), JsValue::Object(models));
        providers.insert(provider.into(), JsValue::Object(provider_config));
    }
    model_config.insert("providers".into(), JsValue::Object(providers));
    config.insert("model".into(), JsValue::Object(model_config));
    root.insert("config".into(), JsValue::Object(config));
    let runtime_snapshot = JsValue::Object(root);
    let session = SidecarSession::new(
        SidecarSessionConfig {
            limits: crate::host::Limits {
                execution_timeout: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            session_key: "embedded-session".into(),
            project_key: None,
            cwd: "/tmp".into(),
            snapshot: runtime_snapshot,
            services: None,
        },
        Arc::new(EmbeddedOnlySpawner),
    );
    let mut turn = session
        .submit(
            parse_json(
                r#"{"agent":{"cwd":"/tmp","provider":"proof","model":"alternate"},"messages":[{"role":"user","content":[{"type":"text","text":"finish once"}]}]}"#,
            )
            .unwrap(),
            parse_json(r#"{"turnId":"embedded-turn","operationId":"embedded-op"}"#).unwrap(),
        )
        .unwrap();

    let mut drain = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = turn.next().await.unwrap() {
            events.push(event);
        }
        events
    });
    tokio::select! {
        _ = server.received() => {}
        result = &mut drain => panic!("embedded turn settled before its model request: {:?}", result.unwrap()),
    }
    server.release();
    let events = timeout(Duration::from_secs(10), drain)
        .await
        .expect("embedded production turn must settle")
        .unwrap();

    let completions: Vec<_> = events
        .iter()
        .filter(|event| event.get("type") == Some(&JsValue::from("turn_completed")))
        .collect();
    assert_eq!(completions.len(), 1, "events: {events:?}");
    assert_eq!(
        completions[0].get("sessionId"),
        Some(&JsValue::from("embedded-session"))
    );
    assert_eq!(
        completions[0].get("turnId"),
        Some(&JsValue::from("embedded-turn"))
    );
    assert!(completions[0].get("result").is_some());
}

fn execution(url: &str, cancel: AbortFlag) -> ProviderExecution {
    let invocation = invoker(url);
    let router = RouterRuntime::new(
        RouterConfig::from_snapshot(&invocation.snapshot),
        Arc::new(HttpRouterInvocation::new(
            Arc::new(invoker(url)),
            invocation.transport,
            invocation.model_config,
        )),
    )
    .unwrap();
    ProviderExecution {
        invocation: None,
        router: Arc::new(router),
        cancel,
        auto_route: false,
        is_main_agent: true,
        fallback_models: Vec::new(),
        managed_model_policy: None,
    }
}

async fn stream(execution: &ProviderExecution) -> Box<dyn model::ModelStream> {
    use model::ModelExecution;
    execution
        .stream(model::StreamInput {
            prepared: model::PreparedInvocation::new(
                request("proof", "alternate", "stream"),
                "proof".into(),
                "alternate".into(),
            ),
            context: JsObject::new(),
            abort: None,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn unconfigured_router_exposes_request_start_before_provider_content() {
    let server = GatedHttp::start("", END).await;
    let mut snapshot = snapshot(&server.url, true).as_object().unwrap().clone();
    let mut config = snapshot.get("config").unwrap().as_object().unwrap().clone();
    config = config
        .entries()
        .iter()
        .filter(|(key, _)| !key.equals_str("router"))
        .cloned()
        .fold(JsObject::new(), |mut result, (key, value)| {
            result.insert(key, value);
            result
        });
    snapshot.insert("config".into(), JsValue::Object(config));
    let host = build(
        JsValue::Object(snapshot),
        &host_input("default"),
        Arc::new(EventBuffer::default()),
        AbortFlag::new(),
    )
    .unwrap();
    let request = request("proof", "alternate", "stream without a router");
    host.module_call(callback("prepare", request.clone(), "default"))
        .await
        .unwrap();
    let host = Arc::new(host);
    let next_host = host.clone();
    let next_request = request.clone();
    let next = tokio::spawn(async move {
        next_host
            .module_call(callback("stream_next", next_request, "default"))
            .await
    });
    // The provider cannot produce content until release; no sleeps choose a stage.
    let first = timeout(Duration::from_secs(2), next)
        .await
        .expect("TS exposes request_started before any response body")
        .unwrap()
        .unwrap();
    let Some(JsValue::Array(events)) = first.get("events") else {
        panic!("stream_next must return its events");
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].get("type"), Some(&"request_started".into()));
    host.module_call(callback("close_stream", request, "default"))
        .await
        .unwrap();
}

async fn first_text(stream: &mut dyn model::ModelStream) {
    for _ in 0..8 {
        let event = timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .expect("first chunk");
        if event.get("type") == Some(&"text_delta".into()) {
            assert_eq!(event.get("text"), Some(&"complex".into()));
            return;
        }
    }
    panic!("missing first text before tail release");
}

#[tokio::test]
async fn provider_stream_delivers_first_chunk_before_tail_and_close_drops_http() {
    let mut server = GatedHttp::start(FIRST, END).await;
    let cancel = AbortFlag::new();
    let mut stream = stream(&execution(&server.url, cancel.clone())).await;
    first_text(stream.as_mut()).await;
    assert!(server.release.is_some(), "terminal chunk remains gated");
    stream.close().await.unwrap();
    server.received().await;
    server.disconnected().await;
    assert!(
        !cancel.is_aborted(),
        "closing one iterator must not abort the turn"
    );
    assert!(stream.next().await.unwrap().is_none());
}

#[tokio::test]
async fn provider_stream_keeps_partial_text_then_provider_error() {
    let mut server = GatedHttp::start(FIRST, FAILURE).await;
    let mut stream = stream(&execution(&server.url, AbortFlag::new())).await;
    first_text(stream.as_mut()).await;
    server.release();
    for _ in 0..8 {
        let Some(event) = timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("partial response lost its error");
        };
        if event.get("type") == Some(&"error".into()) {
            assert_eq!(
                event.get("error").unwrap().get("code"),
                Some(&"invalid_request_error".into())
            );
            return;
        }
    }
    panic!("missing provider error");
}

#[tokio::test]
async fn dropping_provider_stream_closes_pending_http() {
    let mut server = GatedHttp::start(FIRST, END).await;
    let mut stream = stream(&execution(&server.url, AbortFlag::new())).await;
    first_text(stream.as_mut()).await;
    drop(stream);
    server.received().await;
    server.disconnected().await;
}

#[tokio::test]
async fn provider_stream_cancellation_interrupts_a_gated_response() {
    let mut server = GatedHttp::start(FIRST, END).await;
    let cancel = AbortFlag::new();
    let mut stream = stream(&execution(&server.url, cancel.clone())).await;
    first_text(stream.as_mut()).await;
    cancel.abort();
    for _ in 0..8 {
        match timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
        {
            Err(_) | Ok(None) => {
                server.received().await;
                server.disconnected().await;
                return;
            }
            Ok(Some(_)) => {}
        }
    }
    panic!("cancelled request did not settle");
}
