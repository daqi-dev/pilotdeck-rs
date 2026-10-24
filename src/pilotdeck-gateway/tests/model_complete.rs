//! Native `complete_model` against the fixed TypeScript baseline
//! (`tests/fixtures/model-complete/generate.mts`): the real TS `complete()`
//! and this port receive the same loopback HTTP status/headers/body frames.
//! The canonical response (or thrown `ModelProviderError`) is compared byte
//! for byte through `JSON.stringify`; no external traffic and no parser-only
//! shortcut.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/model_mock.rs"]
mod model_mock;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Body, Frame},
    header::{HeaderName, HeaderValue},
};
use pilotdeck_gateway::runtime::{
    model::{
        http::{
            BodyFailure, HyperModelTransport, ModelHttpTransport, StreamOptions, TransportFailure,
            TransportFuture, complete_model,
        },
        router::AbortFlag,
    },
    network::ExchangeFuture,
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};

const ORACLE: &str = include_str!("fixtures/model-complete/oracle.json");
const TRANSPORT_ORACLE: &str = include_str!("fixtures/model-complete-transport/oracle.json");
const TIMEOUT_ORACLE: &str = include_str!("fixtures/model-complete-timeout/oracle.json");

struct FrameBody(VecDeque<Bytes>);

impl Body for FrameBody {
    type Data = Bytes;
    type Error = BodyFailure;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        Poll::Ready(self.0.pop_front().map(|bytes| Ok(Frame::data(bytes))))
    }
}

/// The `Peer`/frames transport used by `model_factory.rs`: every reply is a
/// status, headers and a sequence of body frames, and every send is recorded
/// (method, path and wire body) so the oracle's real `requests` can be
/// compared, not only the request count.
#[derive(Clone, Debug)]
struct RecordedRequest {
    method: String,
    path: String,
    body: String,
}

type Reply = (u16, Vec<(String, String)>, Vec<Bytes>);

struct Peer {
    replies: Vec<Reply>,
    sent: Arc<AtomicUsize>,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl ModelHttpTransport for Peer {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let call = self.sent.fetch_add(1, Ordering::SeqCst);
        let (parts, body) = request.into_parts();
        // `Full<Bytes>` in http-body-util 0.1.3 has no `into_inner`; poll the
        // single data frame instead (the body is always immediately ready).
        let mut body = body;
        let mut collected = Vec::new();
        let mut context = Context::from_waker(std::task::Waker::noop());
        while let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut context) {
            if let Ok(data) = frame.into_data() {
                collected.extend_from_slice(&data);
            }
        }
        let path = parts
            .uri
            .path_and_query()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| parts.uri.path().to_string());
        self.recorded.lock().unwrap().push(RecordedRequest {
            method: parts.method.as_str().to_string(),
            path,
            body: String::from_utf8_lossy(&collected).into_owned(),
        });
        let (status, headers, frames) = &self.replies[call.min(self.replies.len() - 1)];
        let mut response = Response::builder().status(*status);
        for (name, value) in headers {
            response = response.header(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let body = FrameBody(frames.iter().cloned().collect()).boxed();
        ExchangeFuture::ready(Ok(response.body(body).unwrap()))
    }
}

fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => &[],
    }
}

/// Split a body into two frames when possible so the response is consumed from
/// a framed body rather than one pre-decoded string.
fn frames(body: &str) -> Vec<Bytes> {
    let bytes = body.as_bytes().to_vec();
    if bytes.len() < 2 {
        return if bytes.is_empty() {
            Vec::new()
        } else {
            vec![Bytes::from(bytes)]
        };
    }
    let middle = bytes.len() / 2;
    vec![
        Bytes::from(bytes[..middle].to_vec()),
        Bytes::from(bytes[middle..].to_vec()),
    ]
}

fn config(provider: &JsValue) -> JsValue {
    let mut providers = JsObject::new();
    providers.insert("p".into(), provider.clone());
    let mut config = JsObject::new();
    config.insert("providers".into(), JsValue::Object(providers));
    JsValue::Object(config)
}

struct Outcome {
    result: Result<JsValue, pilotdeck_gateway::runtime::model::http::ModelStreamError>,
    requests: Vec<RecordedRequest>,
}

fn reply_parts(reply: &JsValue) -> Reply {
    let status = model_mock::get(reply, "status")
        .as_number()
        .map_or(200, |status| status as u16);
    let headers = match model_mock::get(reply, "headers") {
        JsValue::Object(headers) => headers
            .entries()
            .iter()
            .map(|(name, value)| (String::from_utf16_lossy(name.units()), text(value)))
            .collect(),
        _ => Vec::new(),
    };
    (
        status,
        headers,
        frames(&text(model_mock::get(reply, "body"))),
    )
}

async fn run(case: &JsValue) -> Outcome {
    // A fallback case records each reply it consumed; a single-reply case still
    // has just `reply`. Replay the same status/body sequence for the endpoint
    // candidates the Rust request builder actually sends to.
    let replies: Vec<Reply> = match model_mock::get(case, "replies") {
        JsValue::Array(items) => items.iter().map(reply_parts).collect(),
        _ => vec![reply_parts(model_mock::get(case, "reply"))],
    };
    let sent = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let peer = Peer {
        replies,
        sent: sent.clone(),
        recorded: recorded.clone(),
    };
    let request = model_mock::get(case, "request");
    let provider = model_mock::get(case, "provider");
    let config = config(provider);
    let result = complete_model(&peer, request, &config, &StreamOptions::default()).await;
    let requests = recorded.lock().unwrap().clone();
    Outcome { result, requests }
}

fn case<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
    array(model_mock::get(oracle, "cases"))
        .iter()
        .find(|case| text(model_mock::get(case, "name")) == name)
        .unwrap_or_else(|| panic!("missing oracle case {name}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_complete_model_matches_real_ts_complete() {
    let oracle = baseline::oracle(ORACLE);
    let cases = array(model_mock::get(&oracle, "cases"));
    assert_eq!(cases.len(), 54, "unexpected model-complete oracle size");
    for case in cases {
        let name = text(model_mock::get(case, "name"));
        let outcome = run(case).await;
        let expected_count = model_mock::get(case, "requestCount").as_number().unwrap() as usize;
        assert_eq!(
            outcome.requests.len(),
            expected_count,
            "{name}: request count"
        );
        // The oracle records the real TS wire requests. Compare every method,
        // path and body byte so a provider-body or endpoint regression cannot
        // pass on matching counts alone.
        let expected_requests = array(model_mock::get(case, "requests"));
        assert_eq!(
            expected_requests.len(),
            expected_count,
            "{name}: oracle requests length"
        );
        for (index, (actual, expected)) in outcome
            .requests
            .iter()
            .zip(expected_requests.iter())
            .enumerate()
        {
            assert_eq!(
                actual.method,
                text(model_mock::get(expected, "method")),
                "{name}: request {index} method"
            );
            assert_eq!(
                actual.path,
                text(model_mock::get(expected, "path")),
                "{name}: request {index} path"
            );
            assert_eq!(
                actual.body,
                text(model_mock::get(expected, "body")),
                "{name}: request {index} body"
            );
        }
        if let Some(expected) = case.get("response") {
            let actual = outcome
                .result
                .unwrap_or_else(|error| panic!("{name}: unexpected error {error:?}"));
            assert_eq!(
                model_mock::json(&actual),
                model_mock::json(expected),
                "{name}: canonical response"
            );
        } else {
            let expected = model_mock::get(case, "thrown");
            let error = match outcome.result {
                Ok(value) => panic!(
                    "{name}: expected an error, got {}",
                    model_mock::json(&value)
                ),
                Err(error) => error,
            };
            assert_eq!(
                model_mock::json(&model_mock::thrown(&error)),
                model_mock::json(expected),
                "{name}: thrown error"
            );
        }
    }
}

/// Transport that fails before headers for the first `fail_times` sends, then
/// serves one framed reply. The real `complete_model` retry loop drives it.
type ScriptedReply = (u16, Vec<(String, String)>, Vec<Bytes>);

struct Scripted {
    fail_times: usize,
    message: String,
    cause_code: Option<&'static str>,
    hang: bool,
    reply: Option<ScriptedReply>,
    sent: Arc<AtomicUsize>,
}

impl ModelHttpTransport for Scripted {
    fn send(&self, _request: Request<Full<Bytes>>) -> TransportFuture {
        let call = self.sent.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            // A transport that never settles; only the request timer can end
            // the exchange, exactly like a real accepted-but-silent socket.
            return TransportFuture::io(std::future::pending());
        }
        if call < self.fail_times {
            let failure = if self.message == "fetch failed" && self.cause_code.is_none() {
                TransportFailure::fetch_failed(None)
            } else {
                TransportFailure {
                    cause_code: self.cause_code,
                    message: self.message.clone(),
                }
            };
            return TransportFuture::ready(Err(failure));
        }
        let Some((status, headers, frames)) = &self.reply else {
            return TransportFuture::ready(Err(TransportFailure::fetch_failed(None)));
        };
        let mut response = Response::builder().status(*status);
        for (name, value) in headers {
            response = response.header(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let body = FrameBody(frames.iter().cloned().collect()).boxed();
        ExchangeFuture::ready(Ok(response.body(body).unwrap()))
    }
}

struct TransportOutcome {
    result: Result<JsValue, pilotdeck_gateway::runtime::model::http::ModelStreamError>,
    requests: usize,
    elapsed: Duration,
}

async fn run_transport(case: &JsValue) -> TransportOutcome {
    let reply = model_mock::get(case, "reply");
    let reply = if matches!(reply, JsValue::Object(_)) {
        Some(reply_parts(reply))
    } else {
        None
    };
    let behavior = model_mock::get(case, "fetch");
    let fail_times = model_mock::get(behavior, "failTimes")
        .as_number()
        .unwrap_or(0.0) as usize;
    let message = model_mock::get(behavior, "message")
        .as_string()
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_default();
    let cause_code = model_mock::get(behavior, "causeCode")
        .as_string()
        .map(|value| Box::leak(value.to_utf8().unwrap().into_boxed_str()) as &'static str);
    let hang = model_mock::get(behavior, "hang").as_bool().unwrap_or(false);
    let abort_before_send = model_mock::get(behavior, "abortBeforeSend")
        .as_bool()
        .unwrap_or(false);

    let sent = Arc::new(AtomicUsize::new(0));
    let transport = Scripted {
        fail_times,
        message,
        cause_code,
        hang,
        reply,
        sent: sent.clone(),
    };
    let abort = abort_before_send.then(|| {
        let abort = AbortFlag::new();
        abort.abort();
        abort
    });
    let options = StreamOptions {
        stream_timeout_ms: model_mock::get(case, "streamTimeoutMs").as_number(),
        abort,
        ..StreamOptions::default()
    };
    let request = model_mock::get(case, "request");
    let provider = model_mock::get(case, "provider");
    let config = config(provider);
    let started = Instant::now();
    let result = complete_model(&transport, request, &config, &options).await;
    TransportOutcome {
        result,
        requests: sent.load(Ordering::SeqCst),
        elapsed: started.elapsed(),
    }
}

/// Real TS `complete()` against a controlled failing `fetch`: the retry bound
/// is `attempt <= maxRetries` (fractional 1.5 runs attempts 0 and 1 only and
/// exits with the exhausted error), abort is not retried, a non-retryable
/// transport error is sent once, and one failure followed by a success returns
/// the canonical response. Request counts and the final thrown shape are pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_complete_transport_retry_matches_real_ts() {
    let oracle = baseline::oracle(TRANSPORT_ORACLE);
    let cases = array(model_mock::get(&oracle, "cases"));
    assert_eq!(cases.len(), 13, "unexpected transport oracle size");
    for case in cases {
        let name = text(model_mock::get(case, "name"));
        let outcome = run_transport(case).await;
        assert_eq!(
            outcome.requests,
            model_mock::get(case, "fetchCalls").as_number().unwrap() as usize,
            "{name}: fetch calls"
        );
        // A request timeout is a real 100/300 ms timer, not a pre-settled
        // error: prove the exchange actually waited for it, but not ten times
        // the baseline's requested timeout.
        if name.starts_with("transport-timeout") {
            assert!(
                outcome.elapsed >= Duration::from_millis(50),
                "{name}: timeout fired without waiting ({:?})",
                outcome.elapsed
            );
            let maximum = model_mock::get(case, "elapsedAtMostMs")
                .as_number()
                .unwrap() as u64;
            assert!(
                outcome.elapsed <= Duration::from_millis(maximum),
                "{name}: timeout took {:?}, upper bound {maximum}ms; the baseline never scales provider.timeoutMs",
                outcome.elapsed
            );
        }
        if let Some(expected) = case.get("response") {
            let actual = outcome
                .result
                .unwrap_or_else(|error| panic!("{name}: unexpected error {error:?}"));
            assert_eq!(
                model_mock::json(&actual),
                model_mock::json(expected),
                "{name}: canonical response"
            );
        } else {
            let expected = model_mock::get(case, "thrown");
            let error = match outcome.result {
                Ok(value) => panic!(
                    "{name}: expected an error, got {}",
                    model_mock::json(&value)
                ),
                Err(error) => error,
            };
            assert_eq!(
                model_mock::json(&model_mock::thrown(&error)),
                model_mock::json(expected),
                "{name}: thrown error"
            );
        }
    }
}

/// #610 regression: a Responses body has no `choices`; delegating it to the
/// chat-completions parser would produce empty content and `finishReason:
/// unknown`. The TS oracle pins the real Responses output (text, reasoning,
/// tool calls, `tool_call`) and the chat-parser projection of the same bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_complete_is_not_the_chat_completions_parser() {
    let oracle = baseline::oracle(ORACLE);
    let responses = run(case(&oracle, "responses-normal")).await;
    let as_chat = run(case(&oracle, "responses-body-parsed-as-openai")).await;
    let responses = responses.result.unwrap();
    let as_chat = as_chat.result.unwrap();
    assert_eq!(
        model_mock::json(&responses),
        model_mock::json(model_mock::get(
            case(&oracle, "responses-normal"),
            "response"
        ))
    );
    assert_eq!(
        model_mock::json(&as_chat),
        model_mock::json(model_mock::get(
            case(&oracle, "responses-body-parsed-as-openai"),
            "response"
        ))
    );
    assert!(!array(model_mock::get(&responses, "content")).is_empty());
    assert_eq!(
        text(model_mock::get(&responses, "finishReason")),
        "tool_call"
    );
    assert!(array(model_mock::get(&as_chat, "content")).is_empty());
    assert_eq!(text(model_mock::get(&as_chat, "finishReason")), "unknown");
}

/// #611: a 429 is `retryable` and carries the baseline's `Retry-After`
/// handling, but `complete()` throws on the first non-2xx response instead of
/// retrying it (`streamModel.ts` complete path). The header must not leak a
/// `retryAfterMs` value the TS error does not have.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_429_is_retryable_but_is_not_retried() {
    let oracle = baseline::oracle(ORACLE);
    let case = case(&oracle, "anthropic-429-retry-after");
    let outcome = run(case).await;
    assert_eq!(outcome.requests.len(), 1, "complete() must not retry a 429");
    let error = match outcome.result {
        Err(error) => error,
        Ok(value) => panic!("expected 429 error, got {}", model_mock::json(&value)),
    };
    let value = model_mock::thrown(&error);
    let canonical = model_mock::get(&value, "error");
    assert_eq!(
        model_mock::get(canonical, "status"),
        &JsValue::Number(429.0)
    );
    assert_eq!(
        model_mock::get(canonical, "code"),
        &JsValue::String("rate_limit_error".into())
    );
    assert_eq!(
        model_mock::get(canonical, "retryable"),
        &JsValue::Bool(true)
    );
    assert!(
        model_mock::get(canonical, "retryAfterMs").is_undefined(),
        "the Retry-After response header is not consumed by complete()"
    );
}

/// Non-2xx and malformed/empty success bodies are not two variants of the same
/// failure: TS `parseRawJson` keeps the text and the parsers return an empty
/// canonical response. This is pinned by the empty-body/non-JSON cases.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_empty_and_non_json_bodies_stay_raw_text() {
    let oracle = baseline::oracle(ORACLE);
    for name in [
        "anthropic-empty-body",
        "openai-non-json",
        "responses-non-json",
    ] {
        let outcome = run(case(&oracle, name)).await;
        assert_eq!(outcome.requests.len(), 1);
        let value = outcome
            .result
            .unwrap_or_else(|error| panic!("{name}: unexpected error {error:?}"));
        let expected = model_mock::get(case(&oracle, name), "response");
        assert_eq!(
            model_mock::json(&value),
            model_mock::json(expected),
            "{name}"
        );
        assert!(array(model_mock::get(&value, "content")).is_empty());
        assert_eq!(text(model_mock::get(&value, "finishReason")), "unknown");
        assert!(matches!(model_mock::get(&value, "raw"), JsValue::String(_)));
    }
}

/// One response from the real-loopback timeout oracle. `delay_ms` is sent only
/// after the head is flushed (a *body* delay); `delay_headers_ms` happens
/// before any head byte (a *headers* delay, still bounded by the request
/// timer).
#[derive(Clone, Debug, Default)]
struct LoopbackReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay_ms: u64,
    delay_headers_ms: u64,
    truncate: bool,
}

fn loopback_reply(value: &JsValue) -> LoopbackReply {
    let status = model_mock::get(value, "status")
        .as_number()
        .unwrap_or(200.0) as u16;
    let headers = match model_mock::get(value, "headers").as_object() {
        Some(headers) => headers
            .entries()
            .iter()
            .map(|(name, value)| (name.to_utf8().unwrap(), text(value)))
            .collect(),
        None => Vec::new(),
    };
    let body = text(model_mock::get(value, "body")).into_bytes();
    let delay_ms = model_mock::get(value, "delayMs").as_number().unwrap_or(0.0) as u64;
    let delay_headers_ms = model_mock::get(value, "delayHeadersMs")
        .as_number()
        .unwrap_or(0.0) as u64;
    let truncate = matches!(model_mock::get(value, "truncate"), JsValue::Bool(true));
    LoopbackReply {
        status,
        headers,
        body,
        delay_ms,
        delay_headers_ms,
        truncate,
    }
}

/// A real HTTP/1.1 loopback server for the `HyperModelTransport` replay of the
/// TS loopback fixture. The behavior, body and scripted replies come from the
/// oracle; every request is recorded so path/body/count are all compared.
struct Loopback {
    origin: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn start_loopback(case: &JsValue) -> Loopback {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let recorded = requests.clone();
    let flag = stop.clone();
    let behavior = text(model_mock::get(case, "behavior"));
    let body = model_mock::get(case, "httpBody")
        .as_string()
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_default();
    let replies: Vec<LoopbackReply> = model_mock::items(model_mock::get(case, "replies"))
        .iter()
        .map(loopback_reply)
        .collect();
    thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_nodelay(true).unwrap();
                    let behavior = behavior.clone();
                    let body = body.clone();
                    let replies = replies.clone();
                    let recorded = recorded.clone();
                    thread::spawn(move || {
                        serve_loopback(&mut socket, &behavior, &body, &replies, &recorded);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break,
            }
        }
    });
    Loopback {
        origin,
        requests,
        stop,
    }
}

fn write_loopback_head(socket: &mut std::net::TcpStream, reply: &LoopbackReply) -> bool {
    let reason = hyper::StatusCode::from_u16(reply.status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("unknown");
    let length = reply.body.len() + usize::from(reply.truncate) * 10;
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", reply.status);
    if !reply
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        head.push_str("content-type: application/json\r\n");
    }
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {length}\r\n\r\n"));
    socket
        .write_all(head.as_bytes())
        .and_then(|()| socket.flush())
        .is_ok()
}

fn serve_loopback(
    socket: &mut std::net::TcpStream,
    behavior: &str,
    delayed_body: &str,
    replies: &[LoopbackReply],
    recorded: &Mutex<Vec<RecordedRequest>>,
) {
    socket.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
            Err(_) => return,
        }
    }
    let head_text = String::from_utf8_lossy(&head);
    let content_length = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let header_end = head
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(head.len(), |index| index + 4);
    let mut body = head[header_end.min(head.len())..].to_vec();
    while body.len() < content_length {
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
            Err(_) => return,
        }
    }
    body.truncate(content_length);
    let mut start = head_text.lines().next().unwrap_or_default().split(' ');
    let method = start.next().unwrap_or_default().to_string();
    let path = start.next().unwrap_or_default().to_string();
    let index = {
        let mut recorded = recorded.lock().unwrap();
        recorded.push(RecordedRequest {
            method,
            path,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        recorded.len() - 1
    };
    match behavior {
        // Accepted but never answered; the client's request timer must fire.
        "hang" | "abort" => thread::sleep(Duration::from_millis(1500)),
        // Headers (and the first body bytes) arrive immediately; the rest
        // outlives provider.timeoutMs, proving the timer ends at send().
        "delayed-body" => {
            let bytes = delayed_body.as_bytes();
            let split = bytes.len() / 2;
            let response_head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                bytes.len()
            );
            if socket.write_all(response_head.as_bytes()).is_err()
                || socket.write_all(&bytes[..split]).is_err()
                || socket.flush().is_err()
            {
                return;
            }
            thread::sleep(Duration::from_millis(200));
            socket.write_all(&bytes[split..]).ok();
            socket.flush().ok();
        }
        _ => {
            let Some(reply) = replies.get(index.min(replies.len().saturating_sub(1))) else {
                return;
            };
            if reply.delay_headers_ms > 0 {
                thread::sleep(Duration::from_millis(reply.delay_headers_ms));
            }
            if !write_loopback_head(socket, reply) {
                return;
            }
            if reply.truncate {
                socket.write_all(&reply.body).ok();
                socket.flush().ok();
                thread::sleep(Duration::from_millis(20));
                let _ = socket.shutdown(std::net::Shutdown::Both);
                return;
            }
            if reply.delay_ms > 0 {
                thread::sleep(Duration::from_millis(reply.delay_ms));
            }
            socket.write_all(&reply.body).ok();
            socket.flush().ok();
        }
    }
}

/// The oracle's `requests` shape: method, path and wire body for every accepted
/// loopback request, in order.
fn recorded_requests_json(records: &[RecordedRequest]) -> JsValue {
    JsValue::Array(
        records
            .iter()
            .map(|record| {
                let mut value = JsObject::new();
                value.insert("method".into(), record.method.as_str().into());
                value.insert("path".into(), record.path.as_str().into());
                value.insert("body".into(), record.body.as_str().into());
                JsValue::Object(value)
            })
            .collect(),
    )
}

/// Replay the TS loopback fixture through the real `HyperModelTransport`.
/// This is the timeout case the mock table cannot express: a real accepted
/// socket that never sends headers, a delayed body that outlives the request
/// timer, and an external abort that must not be pre-empted by
/// `streamTimeoutMs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_complete_timeout_uses_provider_timeout_and_ignores_stream_timeout() {
    let oracle = baseline::oracle(TIMEOUT_ORACLE);
    let cases = array(model_mock::get(&oracle, "cases"));
    assert_eq!(cases.len(), 17, "unexpected timeout oracle size");
    for case in cases {
        let name = text(model_mock::get(case, "name"));
        let server = start_loopback(case);
        let provider = {
            let mut provider = model_mock::get(case, "provider")
                .as_object()
                .cloned()
                .unwrap();
            let oracle_url = provider
                .get("url")
                .and_then(|value| value.as_string())
                .map(|value| value.to_utf8().unwrap())
                .unwrap_or_default();
            let path = oracle_url
                .strip_prefix("http://mock.local")
                .unwrap_or("/v1");
            provider.insert(
                "url".into(),
                JsValue::String(format!("{}{path}", server.origin).into()),
            );
            JsValue::Object(provider)
        };
        let config = config(&provider);
        let abort = model_mock::get(case, "abortAfterMs")
            .as_number()
            .map(|abort_after| {
                let abort = AbortFlag::new();
                let signal = abort.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(abort_after as u64)).await;
                    signal.abort();
                });
                abort
            });
        let options = StreamOptions {
            stream_timeout_ms: model_mock::get(case, "streamTimeoutMs").as_number(),
            abort,
            ..StreamOptions::default()
        };
        let started = Instant::now();
        let result = complete_model(
            &HyperModelTransport::new(),
            model_mock::get(case, "request"),
            &config,
            &options,
        )
        .await;
        let elapsed = started.elapsed();
        let recorded = server.requests.lock().unwrap().clone();
        assert_eq!(
            model_mock::json(&recorded_requests_json(&recorded)),
            model_mock::json(model_mock::get(case, "requests")),
            "{name}: wire requests"
        );
        assert_eq!(
            recorded.len(),
            model_mock::get(case, "requestCount").as_number().unwrap() as usize,
            "{name}: loopback request count"
        );
        if let Some(expected) = case.get("response") {
            let value = result.unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert_eq!(
                model_mock::json(&value),
                model_mock::json(expected),
                "{name}"
            );
        } else if let Some(expected) = case.get("thrown") {
            let error = result.expect_err(&name);
            assert_eq!(
                model_mock::json(&model_mock::thrown(&error)),
                model_mock::json(expected),
                "{name}"
            );
        }
        let minimum = model_mock::get(case, "elapsedAtLeastMs")
            .as_number()
            .unwrap() as u64;
        assert!(
            elapsed >= Duration::from_millis(minimum),
            "{name}: completed after {elapsed:?}, expected at least {minimum}ms"
        );
        // Per-case upper bound derived by the generator from the baseline's
        // requested timer/body delay plus documented scheduler slack. The old
        // shared 10s bound let a `timeout_ms * 10` product bug pass every
        // 100/300ms case; this one rejects it without changing the real TS
        // scenario.
        let maximum = model_mock::get(case, "elapsedAtMostMs")
            .as_number()
            .unwrap() as u64;
        assert!(
            elapsed <= Duration::from_millis(maximum),
            "{name}: completed after {elapsed:?}, upper bound {maximum}ms; a timer longer than the baseline's requested delay is a failure"
        );
    }
}

/// Rebuild the oracle from the fixed baseline and require byte-identical
/// output (manual run: `cargo test --test model_complete -- --ignored`).
#[test]
#[ignore]
fn regenerates_model_complete_oracle_identically() {
    baseline::regenerates_identically("model-complete");
}

/// Rebuild the controlled-`fetch` transport oracle from the fixed baseline and
/// require byte-identical output.
#[test]
#[ignore]
fn regenerates_model_complete_transport_oracle_identically() {
    baseline::regenerates_identically("model-complete-transport");
}

/// Rebuild the real-loopback timeout oracle from the fixed baseline and
/// require byte-identical output.
#[test]
#[ignore]
fn regenerates_model_complete_timeout_oracle_identically() {
    baseline::regenerates_identically("model-complete-timeout");
}
