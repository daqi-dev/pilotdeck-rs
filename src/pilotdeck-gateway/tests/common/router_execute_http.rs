use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use pilotdeck_gateway::runtime::model::{
    fallback::HttpRouterInvocation,
    http::{BodyFailure, ModelBody, ModelHttpTransport, TransportFailure, TransportFuture},
};
use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};
struct Replies {
    remaining: Mutex<VecDeque<JsValue>>,
    requests: Trace,
}
struct ReplyBody {
    chunks: VecDeque<Bytes>,
    end: String,
}
impl hyper::body::Body for ReplyBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        if let Some(bytes) = self.chunks.pop_front() {
            return Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))));
        }
        match self.end.as_str() {
            "hold" => Poll::Pending,
            "destroy" => {
                self.end.clear();
                Poll::Ready(Some(Err(BodyFailure)))
            }
            _ => Poll::Ready(None),
        }
    }
}
impl ModelHttpTransport for Replies {
    fn send(&self, request: hyper::Request<Full<Bytes>>) -> TransportFuture {
        let reply = self
            .remaining
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP exchange");
        let records = self.requests.clone();
        TransportFuture::io(async move {
            if get(&reply, "fault").as_string().is_some() {
                return Err(TransportFailure::fetch_failed(Some("ECONNREFUSED")));
            }
            let method = request.method().to_string();
            let path = request.uri().path().to_string();
            let mut headers: Vec<_> = request
                .headers()
                .iter()
                .map(|(key, value)| {
                    JsValue::Array(vec![
                        key.as_str().into(),
                        String::from_utf8_lossy(value.as_bytes()).as_ref().into(),
                    ])
                })
                .collect();
            headers.sort_by_key(|header| text(&items(header)[0]));
            let body = request.into_body().collect().await.unwrap().to_bytes();
            records.lock().unwrap().push(obj(&[
                ("method", method.as_str().into()),
                ("path", path.as_str().into()),
                ("headers", JsValue::Array(headers)),
                ("body", std::str::from_utf8(&body).unwrap().into()),
            ]));
            let mut response = hyper::Response::builder()
                .status(get(&reply, "status").as_number().unwrap_or(200.0) as u16)
                .header(
                    "content-type",
                    get(get(&reply, "headers"), "content-type")
                        .as_string()
                        .and_then(|v| v.to_utf8().ok())
                        .unwrap_or_else(|| "text/event-stream".into()),
                );
            if let Some(headers) = get(&reply, "headers").as_object() {
                for (key, value) in headers.entries() {
                    if !key.equals_str("content-type") {
                        response = response.header(key.to_utf8().unwrap(), text(value));
                    }
                }
            }
            let chunks = items(get(&reply, "chunks"))
                .iter()
                .map(|chunk| Bytes::from(text(chunk)))
                .collect();
            let end = get(&reply, "end")
                .as_string()
                .and_then(|v| v.to_utf8().ok())
                .unwrap_or_default();
            let body: ModelBody = ReplyBody { chunks, end }.boxed();
            Ok(response.body(body).unwrap())
        })
    }
}
fn providers() -> JsValue {
    let config = parsed(r#"{"providers":{}}"#);
    let mut config = config.as_object().unwrap().clone();
    let mut providers = JsObject::new();
    for id in ["a", "b", "c"] {
        let provider = parsed(
            r#"{"protocol":"anthropic","apiKey":"fixture-key","retry":{"streamMaxRetries":0},"models":{"m":{"id":"m","capabilities":{"supportsStreaming":true,"supportsToolUse":true,"supportsSystemPrompt":true,"maxOutputTokens":64},"multimodal":{"input":["text","image"]}}}}"#,
        );
        let provider = merge(
            &provider,
            &obj(&[
                ("id", id.into()),
                ("url", format!("http://mock.test/{id}/v1").as_str().into()),
            ]),
        );
        providers.insert(id.into(), provider);
    }
    config.insert("providers".into(), JsValue::Object(providers));
    JsValue::Object(config)
}
fn sse(event: JsValue) -> JsValue {
    format!(
        "event: {}\ndata: {}\n\n",
        text(get(&event, "type")),
        json(&event)
    )
    .as_str()
    .into()
}
fn chunks(text_delta: bool, finish: bool) -> Vec<JsValue> {
    let mut chunks = vec![sse(parsed(
        r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"m","usage":{"input_tokens":11,"output_tokens":1}}}"#,
    ))];
    if text_delta {
        chunks.extend([sse(parsed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#)),sse(parsed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#))]);
    }
    if finish {
        chunks.extend([sse(parsed(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#)),sse(parsed(r#"{"type":"message_stop"}"#))]);
    }
    chunks
}
#[path = "model_mock.rs"]
mod model_mock;
struct RecordingHttp {
    inner: HttpRouterInvocation,
    calls: Trace,
}
impl RouterModelInvocation for RecordingHttp {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.inner.get_capabilities(provider, model)
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.inner.get_multimodal(provider, model)
    }
    fn get_provider_protocol(&self, provider: &JsString) -> JsString {
        self.inner.get_provider_protocol(provider)
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        self.calls.lock().unwrap().push(request.clone());
        self.inner.stream(request, options, sink)
    }
}
fn http_config(spec: &JsValue, origin: &str, closed: &str) -> JsValue {
    let raw = json(&providers()).replace("http://mock.test", origin);
    let mut config = parsed(&raw).as_object().unwrap().clone();
    let mut configured = config
        .get("providers")
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    let mut first = configured.get("a").unwrap().as_object().unwrap().clone();
    if get(&items(get(spec, "replies"))[0], "fault")
        .as_string()
        .is_some()
    {
        first.insert("url".into(), format!("{closed}/a/v1").as_str().into());
    }
    if let Some(timeout) = get(spec, "streamTimeoutMs").as_number() {
        first.insert(
            "retry".into(),
            obj(&[
                ("streamMaxRetries", JsValue::Number(0.0)),
                ("streamIdleTimeoutMs", JsValue::Number(timeout)),
            ]),
        );
    }
    configured.insert("a".into(), JsValue::Object(first));
    config.insert("providers".into(), JsValue::Object(configured));
    JsValue::Object(config)
}
async fn replay(spec: &JsValue) -> (JsValue, JsValue, JsValue) {
    let requests: Trace = Arc::default();
    let transport = Arc::new(Replies {
        remaining: Mutex::new(items(get(spec, "replies")).iter().cloned().collect()),
        requests: requests.clone(),
    });
    let (trace, _, usage) = execute_http(
        spec,
        transport.clone(),
        http_config(spec, "http://mock.test", "http://closed.test"),
    )
    .await;
    assert!(transport.remaining.lock().unwrap().is_empty());
    let requests = JsValue::Array(requests.lock().unwrap().clone());
    (trace, requests, usage)
}
async fn execute_http(
    spec: &JsValue,
    transport: Arc<dyn ModelHttpTransport>,
    model_config: JsValue,
) -> (JsValue, JsValue, JsValue) {
    let trace: Trace = Arc::default();
    let calls: Trace = Arc::default();
    let delegate = Arc::new(Invoker {
        spec: spec.clone(),
        calls: Arc::default(),
        generation: Mutex::new(JsValue::Undefined),
    });
    let adapter = Arc::new(RecordingHttp {
        inner: HttpRouterInvocation::new(delegate, transport, model_config),
        calls: calls.clone(),
    });
    let mut deps = RouterDeps::new(adapter);
    let events = trace.clone();
    deps.events = Arc::new(move |value| {
        events
            .lock()
            .unwrap()
            .push(obj(&[("kind", "router".into()), ("value", value)]))
    });
    let config = RouterConfig::from_snapshot(&parsed(
        r#"{"config":{"router":{"fallback":{"default":[{"provider":"b","model":"m"}]},"zeroUsageRetry":{"enabled":false},"transientRetry":{"enabled":false}}}}"#,
    ));
    let router = RouterRuntime::with_deps(config, deps)
        .unwrap()
        .with_provider_health(Arc::new(TracedHealth {
            inner: NativeRouterProviderHealth::new(Arc::new(|| 0.0)),
            trace: trace.clone(),
        }));
    let abort = AbortFlag::new();
    let ctx = RouterExecuteContext {
        session_id: "s".into(),
        turn_id: "t".into(),
        stream_options: StreamOptions {
            abort: Some(abort.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let decision = parsed(
        r#"{"provider":"a","model":"m","scenarioType":"default","isSubagent":false,"orchestrating":false,"resolvedFrom":"scenario","mutations":{}}"#,
    );
    let request = parsed(
        r#"{"provider":"a","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"maxOutputTokens":100}"#,
    );
    let result = router
        .execute(&decision, &request, &ctx, &mut |event| {
            let cancel = get(spec, "abortAfterText").as_bool() == Some(true)
                && get(&event, "type")
                    .as_string()
                    .is_some_and(|v| v.equals_str("text_delta"));
            trace
                .lock()
                .unwrap()
                .push(obj(&[("kind", "event".into()), ("value", event)]));
            if cancel {
                abort.abort();
            }
        })
        .await;
    if let Err(RouterExecuteError::Model(error)) = result {
        trace
            .lock()
            .unwrap()
            .push(obj(&[("kind", "thrown".into()), ("value", thrown(&error))]));
    } else {
        result.unwrap();
    }
    let trace = JsValue::Array(trace.lock().unwrap().clone());
    let requests = JsValue::Array(calls.lock().unwrap().clone());
    (
        trace,
        requests,
        router
            .get_session_usage(&"s".into())
            .unwrap_or(JsValue::Undefined),
    )
}
#[tokio::test(start_paused = true)]
async fn http_faults_use_real_provider_path_and_fallback_boundaries() {
    let ok = obj(&[("chunks", JsValue::Array(chunks(true, true)))]);
    for fault in [
        parsed(
            r#"{"status":429,"headers":{"content-type":"application/json"},"chunks":["{\"error\":{\"type\":\"rate_limit_error\",\"message\":\"rate limit\"}}"]}"#,
        ),
        parsed(
            r#"{"status":503,"headers":{"content-type":"application/json"},"chunks":["{\"error\":{\"type\":\"api_error\",\"message\":\"server fault\"}}"]}"#,
        ),
        parsed(r#"{"fault":"ECONNREFUSED"}"#),
        obj(&[
            ("chunks", JsValue::Array(chunks(false, false))),
            ("end", "hold".into()),
        ]),
    ] {
        let spec = obj(&[
            ("streamTimeoutMs", JsValue::Number(10.0)),
            ("replies", JsValue::Array(vec![fault, ok.clone()])),
        ]);
        let (trace, _, usage) = replay(&spec).await;
        assert_eq!(
            items(&trace)
                .iter()
                .filter(|entry| get(get(entry, "value"), "type")
                    .as_string()
                    .is_some_and(|v| v.equals_str("pilotdeck_router_fallback")))
                .count(),
            1
        );
        assert_eq!(get(&usage, "outputTokens").as_number(), Some(7.0));
    }
    for end in ["destroy", "hold"] {
        let spec = obj(&[
            ("abortAfterText", JsValue::Bool(end == "hold")),
            (
                "replies",
                JsValue::Array(vec![obj(&[
                    ("chunks", JsValue::Array(chunks(true, false))),
                    ("end", end.into()),
                ])]),
            ),
        ]);
        let (trace, requests, _) = replay(&spec).await;
        assert_eq!(items(&requests).len(), 1);
        assert!(!json(&trace).contains("pilotdeck_router_fallback"));
        assert!(json(&trace).contains(if end == "hold" {
            "AbortError"
        } else {
            "terminated"
        }));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires freshly generated --http oracle and loopback listening permissions"]
async fn http_sequences_match_real_ts_oracle() {
    let path = std::env::var("PDRS_ROUTER_HTTP_ORACLE")
        .expect("generate.mts /tmp/owned-output --http; set PDRS_ROUTER_HTTP_ORACLE");
    let raw = std::fs::read_to_string(path).unwrap();
    let oracle = baseline::oracle(&raw);
    let cases = items(get(&oracle, "executeCases"));
    assert_eq!(cases.len(), 7);
    for case in cases {
        let spec = get(case, "spec");
        let expected_count = items(get(spec, "replies"))
            .iter()
            .filter(|reply| get(reply, "fault").is_undefined())
            .count();
        let replies = items(get(spec, "replies"))
            .iter()
            .filter(|reply| get(reply, "fault").is_undefined())
            .map(|reply| {
                merge(
                    reply,
                    &obj(&[(
                        "chunks",
                        JsValue::Array(
                            items(get(reply, "chunks"))
                                .iter()
                                .map(|chunk| obj(&[("text", chunk.clone())]))
                                .collect(),
                        ),
                    )]),
                )
            })
            .collect();
        let exchange = Arc::new(Mutex::new(model_mock::Exchange {
            replies,
            ..Default::default()
        }));
        let excluded = items(get(&oracle, "transportHeaders"))
            .iter()
            .map(text)
            .collect();
        let server = model_mock::start_mock(exchange.clone(), excluded, None);
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let closed = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        drop(listener);
        let (trace, calls, usage) = execute_http(
            spec,
            Arc::new(pilotdeck_gateway::runtime::model::http::HyperModelTransport::new()),
            http_config(spec, &server.origin, &closed),
        )
        .await;
        if items(get(spec, "replies")).iter().any(|reply| {
            get(reply, "end")
                .as_string()
                .is_some_and(|end| end.equals_str("hold"))
        }) {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let done =
                        exchange.lock().unwrap().requests.iter().any(|request| {
                            request.get("clientClosed") == Some(&JsValue::Bool(true))
                        });
                    if done {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("held response must be closed by timeout/cancellation");
        }
        let normalize = |value: &JsValue| {
            json(value)
                .replace(&server.origin, "$MOCK")
                .replace(&closed, "$CLOSED")
        };
        assert_eq!(
            normalize(&trace),
            json(get(case, "trace")),
            "{} trace",
            text_name(case)
        );
        expected_bytes(
            &calls,
            get(case, "calls"),
            &format!("{} invocations", text_name(case)),
        );
        let requests = exchange.lock().unwrap().requests.clone();
        assert_eq!(requests.len(), expected_count);
        expected_bytes(
            &JsValue::Array(requests.into_iter().map(JsValue::Object).collect()),
            get(case, "requests"),
            &format!("{} HTTP requests", text_name(case)),
        );
        // The oracle is JSON: TS drops undefined usage fields when serialising.
        assert_eq!(
            stringify_json(&usage).unwrap(),
            stringify_json(get(case, "sessionUsage")).unwrap(),
            "{} session usage",
            text_name(case)
        );
    }
}
fn text_name(case: &JsValue) -> String {
    text(get(get(case, "spec"), "name"))
}

#[tokio::test]
async fn request_validation_details_reach_the_router_canonical_error() {
    use pilotdeck_gateway::runtime::model::http::stream_model;
    let mut config = providers().as_object().unwrap().clone();
    let mut configured = config
        .get("providers")
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    let mut provider = configured.get("a").unwrap().as_object().unwrap().clone();
    let mut models = provider.get("models").unwrap().as_object().unwrap().clone();
    let mut model = models.get("m").unwrap().as_object().unwrap().clone();
    model.insert(
        "multimodal".into(),
        parsed(r#"{"input":["text","image"],"maxImagesPerRequest":1}"#),
    );
    models.insert("m".into(), JsValue::Object(model));
    provider.insert("models".into(), JsValue::Object(models));
    configured.insert("a".into(), JsValue::Object(provider));
    config.insert("providers".into(), JsValue::Object(configured));
    let request = parsed(
        r#"{"provider":"a","model":"m","messages":[{"role":"user","content":[{"type":"image","source":"base64","mimeType":"image/png","data":"AA=="},{"type":"image","source":"base64","mimeType":"image/png","data":"AA=="}]}]}"#,
    );
    let transport = Replies {
        remaining: Mutex::new(VecDeque::new()),
        requests: Arc::default(),
    };
    let error = stream_model(
        &transport,
        &request,
        &JsValue::Object(config),
        &StreamOptions::default(),
        &mut |_| panic!("validation must precede stream events"),
    )
    .await
    .unwrap_err();
    let ModelStreamError::Request {
        code,
        message,
        details,
    } = error
    else {
        panic!("expected request validation");
    };
    let oracle = baseline::oracle(ORACLE);
    let case = items(get(&oracle, "executeCases"))
        .iter()
        .find(|case| text_name(case) == "request-error-details")
        .unwrap();
    let expected = get(&items(get(get(case, "spec"), "scripts"))[0], "throw");
    assert_eq!(&code, get(expected, "code").as_string().unwrap());
    assert_eq!(&message, get(expected, "message").as_string().unwrap());
    assert_eq!(details.as_deref().unwrap(), get(expected, "details"));
}
