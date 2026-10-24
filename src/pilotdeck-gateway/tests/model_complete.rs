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
            BodyFailure, ModelHttpTransport, StreamOptions, TransportFailure, TransportFuture,
            complete_model,
        },
        router::AbortFlag,
    },
    network::ExchangeFuture,
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

const ORACLE: &str = include_str!("fixtures/model-complete/oracle.json");
const TRANSPORT_ORACLE: &str = include_str!("fixtures/model-complete-transport/oracle.json");

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
/// status, headers and a sequence of body frames, and every send is counted.
struct Peer {
    status: u16,
    headers: Vec<(String, String)>,
    frames: Vec<Bytes>,
    sent: Arc<AtomicUsize>,
}

impl ModelHttpTransport for Peer {
    fn send(&self, _request: Request<Full<Bytes>>) -> TransportFuture {
        self.sent.fetch_add(1, Ordering::SeqCst);
        let mut response = Response::builder().status(self.status);
        for (name, value) in &self.headers {
            response = response.header(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let body = FrameBody(self.frames.iter().cloned().collect()).boxed();
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
    requests: usize,
}

async fn run(case: &JsValue) -> Outcome {
    let reply = model_mock::get(case, "reply");
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
    let sent = Arc::new(AtomicUsize::new(0));
    let peer = Peer {
        status,
        headers,
        frames: frames(&text(model_mock::get(reply, "body"))),
        sent: sent.clone(),
    };
    let request = model_mock::get(case, "request");
    let provider = model_mock::get(case, "provider");
    let config = config(provider);
    let result = complete_model(&peer, request, &config, &StreamOptions::default()).await;
    Outcome {
        result,
        requests: sent.load(Ordering::SeqCst),
    }
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
    assert_eq!(cases.len(), 28, "unexpected model-complete oracle size");
    for case in cases {
        let name = text(model_mock::get(case, "name"));
        let outcome = run(case).await;
        assert_eq!(
            outcome.requests,
            model_mock::get(case, "requestCount").as_number().unwrap() as usize,
            "{name}: request count"
        );
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
    reply: Option<ScriptedReply>,
    sent: Arc<AtomicUsize>,
}

impl ModelHttpTransport for Scripted {
    fn send(&self, _request: Request<Full<Bytes>>) -> TransportFuture {
        let call = self.sent.fetch_add(1, Ordering::SeqCst);
        if call < self.fail_times {
            let failure = if self.message == "fetch failed" {
                TransportFailure::fetch_failed(None)
            } else {
                TransportFailure {
                    cause_code: None,
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

async fn run_transport(case: &JsValue) -> Outcome {
    let reply = model_mock::get(case, "reply");
    let reply = if matches!(reply, JsValue::Object(_)) {
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
        Some((
            status,
            headers,
            frames(&text(model_mock::get(reply, "body"))),
        ))
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
    let abort_before_send = model_mock::get(behavior, "abortBeforeSend")
        .as_bool()
        .unwrap_or(false);

    let sent = Arc::new(AtomicUsize::new(0));
    let transport = Scripted {
        fail_times,
        message,
        reply,
        sent: sent.clone(),
    };
    let mut options = StreamOptions::default();
    if abort_before_send {
        let abort = AbortFlag::new();
        abort.abort();
        options.abort = Some(abort);
    }
    let request = model_mock::get(case, "request");
    let provider = model_mock::get(case, "provider");
    let config = config(provider);
    let result = complete_model(&transport, request, &config, &options).await;
    Outcome {
        result,
        requests: sent.load(Ordering::SeqCst),
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
    assert_eq!(cases.len(), 7, "unexpected transport oracle size");
    for case in cases {
        let name = text(model_mock::get(case, "name"));
        let outcome = run_transport(case).await;
        assert_eq!(
            outcome.requests,
            model_mock::get(case, "fetchCalls").as_number().unwrap() as usize,
            "{name}: fetch calls"
        );
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
    assert_eq!(outcome.requests, 1, "complete() must not retry a 429");
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
        assert_eq!(outcome.requests, 1);
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
