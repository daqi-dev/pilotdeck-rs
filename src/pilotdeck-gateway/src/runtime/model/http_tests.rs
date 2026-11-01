use super::complete::retry_decide_request;
use super::*;
use std::{future::Future, pin::Pin};

struct PendingBody;
impl hyper::body::Body for PendingBody {
    type Data = Bytes;
    type Error = BodyFailure;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        std::task::Poll::Pending
    }
}

pub(super) async fn poll_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

/// Actual TS provider: Infinity is accepted as a positive config value but
/// setTimeout arms 1ms, preserving "Infinityms" in the idle error message.
/// Compare normalized timers before choosing idle versus undici body timeout.
#[tokio::test(start_paused = true)]
async fn provider_idle_timeout_uses_node_timer_delay() {
    for (idle, elapsed) in [(f64::INFINITY, 1), (60.9, 60), (61.1, 61)] {
        let mut retry = JsObject::new();
        retry.insert("streamIdleTimeoutMs".into(), JsValue::Number(idle));
        let mut provider = JsObject::new();
        provider.insert("retry".into(), JsValue::Object(retry));
        let provider = JsValue::Object(provider);
        let options = StreamOptions::default();
        let configured = idle_timeout_ms(&provider, &options);
        assert_eq!(configured, idle);
        let mut body = PendingBody.boxed();
        let mut continuation = false;
        let mut read = Box::pin(read_frame(
            &mut body,
            configured,
            &options,
            &mut continuation,
        ));
        poll_pending(read.as_mut()).await;
        if elapsed > 1 {
            tokio::time::advance(Duration::from_millis(elapsed - 1)).await;
            poll_pending(read.as_mut()).await;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
        // Do not await here: start_paused would auto-advance to a mistakenly
        // longer timer, concealing fractional rounding / Infinity regressions.
        let error = std::future::poll_fn(|cx| {
            let std::task::Poll::Ready(result) = read.as_mut().poll(cx) else {
                panic!("idle timeout was not ready at {elapsed}ms");
            };
            std::task::Poll::Ready(result.unwrap_err())
        })
        .await;
        assert_eq!(error.name(), JsString::from("StreamIdleTimeoutError"));
        assert_eq!(
            error.message_text(),
            format!(
                "Stream idle timeout: no data received for {}ms",
                crate::js_value::number_string(idle)
            )
        );
    }
}

fn retry_provider(entries: &[(&str, f64)]) -> JsValue {
    let mut retry = JsObject::new();
    for (key, value) in entries {
        retry.insert((*key).into(), JsValue::Number(*value));
    }
    let mut provider = JsObject::new();
    provider.insert("retry".into(), JsValue::Object(retry));
    JsValue::Object(provider)
}

/// `RetryPolicy.ts:44,50` calls `Math.min`, which propagates NaN and projects
/// `-0` for a signed-zero tie. Rust `f64::min` does neither on every input.
/// Node v22.22.0 probe:
/// `_tmp/20261026-issue-947/fix-r1/node/retry-policy-probe.out`.
#[test]
fn retry_delay_uses_js_math_min_for_nan_and_signed_zero() {
    // baseDelay * 2 overflows to Infinity; jitter 0 makes `Infinity * 0` NaN,
    // and Math.min(NaN, 8000) stays NaN (RetryPolicy.ts:50).
    let overflow = retry_provider(&[
        ("baseDelayMs", 1e308),
        ("maxDelayMs", 8_000.0),
        ("jitter", 0.0),
    ]);
    let (_, request_delay) = retry_decide_request(&overflow, 1.0, 0.0);
    assert!(
        request_delay.is_nan(),
        "#947 retry_decide_request must propagate NaN from Math.min, got {request_delay}"
    );
    let (_, stream_delay) = retry_decide(&overflow, 1.0, None, 0.0);
    assert!(
        stream_delay.is_nan(),
        "#947 retry_decide stream must propagate NaN from Math.min, got {stream_delay}"
    );

    // retryAfterMs goes through Math.min(input.retryAfterMs, maxDelayMs)
    // (RetryPolicy.ts:44); a NaN maxDelay wins over a finite retry-after.
    let nan_max = retry_provider(&[("maxDelayMs", f64::NAN)]);
    let (_, after_delay) = retry_decide(&nan_max, 0.0, Some(100.0), 0.0);
    assert!(
        after_delay.is_nan(),
        "#947 retry_after must propagate NaN from Math.min, got {after_delay}"
    );

    // Math.min(-0, +0) and Math.min(+0, -0) are both -0.
    for (retry_after, max_delay) in [(-0.0, 0.0), (0.0, -0.0)] {
        let provider = retry_provider(&[("maxDelayMs", max_delay)]);
        let (_, delay) = retry_decide(&provider, 0.0, Some(retry_after), 0.0);
        assert_eq!(
            delay.to_bits(),
            (-0.0f64).to_bits(),
            "#947 retry_after Math.min signed-zero tie ({retry_after}, {max_delay}) must project -0"
        );
    }

    // Ordinary capping is unchanged.
    let ordinary = retry_provider(&[("baseDelayMs", 500.0), ("maxDelayMs", 8_000.0)]);
    assert_eq!(
        retry_decide_request(&ordinary, 1.0, 0.0).1,
        1000.0,
        "#947 ordinary request retry delay"
    );
    let capped = retry_provider(&[("baseDelayMs", 10_000.0), ("maxDelayMs", 8_000.0)]);
    assert_eq!(
        retry_decide_request(&capped, 1.0, 0.0).1,
        8_000.0,
        "#947 ordinary request retry cap"
    );
}

/// Default idle and undici bodyTimeout are both 600000 ms. On a tie TS's
/// native-timer idle watchdog wins (undici's coarse fast timer fires late), so
/// the visible error is the idle timeout, not `terminated`.
#[tokio::test(start_paused = true)]
async fn provider_default_idle_watchdog_wins_body_timeout_tie() {
    let options = StreamOptions::default();
    let idle = idle_timeout_ms(&JsValue::Object(JsObject::new()), &options);
    assert_eq!(idle, 600_000.0);
    assert_eq!(options.body_timeout_ms, idle);
    let mut body = PendingBody.boxed();
    let mut continuation = false;
    let mut read = Box::pin(read_frame(&mut body, idle, &options, &mut continuation));
    poll_pending(read.as_mut()).await;
    tokio::time::advance(Duration::from_millis(599_999)).await;
    poll_pending(read.as_mut()).await;
    tokio::time::advance(Duration::from_millis(1)).await;
    let error = poll_ready(read.as_mut()).await.unwrap_err();
    assert_eq!(error.name(), JsString::from("StreamIdleTimeoutError"));
    assert_eq!(
        error.message_text(),
        "Stream idle timeout: no data received for 600000ms"
    );
}

#[tokio::test(start_paused = true)]
async fn provider_finite_idle_and_shorter_body_are_negative_controls() {
    let options = StreamOptions::default();
    let mut body = PendingBody.boxed();
    let mut continuation = false;
    let mut read = Box::pin(read_frame(&mut body, 1000.0, &options, &mut continuation));
    poll_pending(read.as_mut()).await;
    tokio::time::advance(Duration::from_millis(1)).await;
    poll_pending(read.as_mut()).await;
    drop(read);

    let options = StreamOptions {
        body_timeout_ms: 10.0,
        ..StreamOptions::default()
    };
    let mut continuation = false;
    let mut read = Box::pin(read_frame(&mut body, 1000.0, &options, &mut continuation));
    poll_pending(read.as_mut()).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    let error = read.await.unwrap_err();
    assert_eq!(error.name(), JsString::from("TypeError"));
    assert_eq!(error.message_text(), "terminated");
}

#[test]
fn request_started_matches_all_protocol_oracles() {
    for (source, expected_count) in [
        (
            include_str!("../../../tests/fixtures/model-stream/oracle.json"),
            30,
        ),
        (
            include_str!("../../../tests/fixtures/model-stream-google/oracle.json"),
            28,
        ),
        (
            include_str!("../../../tests/fixtures/model-stream-openai/oracle.json"),
            30,
        ),
        (
            include_str!("../../../tests/fixtures/model-stream-responses/oracle.json"),
            21,
        ),
    ] {
        let oracle = crate::test_baseline::oracle(source);
        let mut checked = 0;
        for case in super::super::count_body::array(field(&oracle, "cases")) {
            let expected = super::super::count_body::array(field(case, "timeline"))
                .iter()
                .map(|entry| field(entry, "event"))
                .find(|event| is(field(event, "type"), "request_started"));
            let Some(expected) = expected else { continue };
            let merge = |base: &JsValue, overrides: &JsValue| {
                let mut result = base.as_object().unwrap().clone();
                if let Some(overrides) = overrides.as_object() {
                    for (key, value) in overrides.entries() {
                        result.insert(key.clone(), value.clone());
                    }
                }
                JsValue::Object(result)
            };
            let mut current = merge(field(&oracle, "request"), field(case, "request"));
            put(&mut current, "stream", JsValue::Bool(true));
            let mut provider = merge(field(&oracle, "provider"), field(case, "provider"));
            if !field(case, "url").is_undefined() {
                put(&mut provider, "url", field(case, "url").clone());
            }
            let origin = match header_text(field(case, "target")).as_str() {
                "closed" => "http://127.0.0.1:12346",
                "tls-trusted" | "tls-untrusted" => "https://localhost:12347",
                _ => "http://127.0.0.1:12345",
            };
            let url = header_text(field(&provider, "url")).replace("$MOCK", origin);
            put(&mut provider, "url", url.as_str().into());
            assert_eq!(
                crate::js_value::json_stringify(
                    &request_started_event(&provider, &current).unwrap()
                )
                .unwrap()
                .unwrap()
                .replace("http://127.0.0.1:12345", "$MOCK")
                .replace("http://127.0.0.1:12346", "$CLOSED")
                .replace("https://localhost:12347", "$TLS"),
                crate::js_value::json_stringify(expected).unwrap().unwrap(),
                "{}: request_started",
                header_text(field(case, "name"))
            );
            checked += 1;
        }
        assert_eq!(checked, expected_count, "initial request_started count");
    }
}

#[test]
fn retry_after_header_and_message_parsing() {
    // delta-seconds, rounded like Math.round(seconds * 1000)
    assert_eq!(
        errors::retry_after_header(Some(" 0.0125 "), 0.0),
        Some(13.0)
    );
    assert_eq!(errors::retry_after_header(Some(""), 0.0), None);
    assert_eq!(errors::retry_after_header(None, 0.0), None);
    // IMF-fixdate in the future/past
    let now = 1_445_412_470_000.0; // 2015-10-21T07:27:50Z
    assert_eq!(
        errors::retry_after_header(Some("Wed, 21 Oct 2015 07:28:00 GMT"), now),
        Some(10_000.0)
    );
    assert_eq!(
        errors::retry_after_header(Some("Wed, 21 Oct 2015 07:27:00 GMT"), now),
        None
    );
    assert_eq!(
        retry_after_from_message("Please try again in 1.898s"),
        Some(1898.0)
    );
    assert_eq!(
        retry_after_from_message("RETRY IN 2 minutes"),
        Some(120_000.0)
    );
    assert_eq!(retry_after_from_message("try again in 0s"), None);
    // `i` without `u`: U+017F (long s) does not fold onto ASCII `s`.
    assert_eq!(retry_after_from_message("try again in 3\u{17f}"), None);
}

#[test]
fn js_round_matches_math_round() {
    assert_eq!(js_round(2.5), 3.0);
    assert_eq!(js_round(-2.5), -2.0);
    assert_eq!(js_round(2.75), 3.0);
}

#[test]
fn provider_error_projects_onto_module_error() {
    let mut canonical = JsObject::new();
    canonical.insert("code".into(), "rate_limit_error".into());
    canonical.insert("message".into(), "slow down".into());
    canonical.insert("retryable".into(), JsValue::Bool(true));
    canonical.insert("retryAfterMs".into(), JsValue::Number(20.0));
    let module = ModuleError::from(ModelStreamError::Provider(canonical.clone()));
    assert_eq!(module.code, Some("rate_limit_error".into()));
    assert_eq!(module.retryable, Some(true));
    assert_eq!(module.retry_after_ms, Some(20.0));
    assert_eq!(module.canonical, Some(canonical));
    let request = ModuleError::from(ModelStreamError::Request {
        code: "provider_not_found".into(),
        message: "Provider x does not exist.".into(),
        details: None,
    });
    assert_eq!(request.retryable, Some(false));
}

#[test]
fn network_errors_classify_like_normalize_network_error() {
    assert_eq!(
        NetworkFetchError::classify("fetch failed", Some("ECONNREFUSED")).code,
        "network_connection_refused"
    );
    assert_eq!(
        NetworkFetchError::classify("fetch failed", Some("UNABLE_TO_GET_ISSUER_CERT_LOCALLY")).code,
        "network_fetch_failed"
    );
    assert_eq!(
        NetworkFetchError::classify("fetch failed", Some("ERR_TLS_CERT_ALTNAME_INVALID")).code,
        "network_tls_error"
    );
    assert_eq!(
        NetworkFetchError::classify("fetch failed", Some("ENOTFOUND")).code,
        "network_dns_error"
    );
}

/// Abort while waiting for response headers drops the in-flight exchange.
struct Never(Arc<std::sync::atomic::AtomicBool>);
impl ModelHttpTransport for Never {
    fn send(&self, _request: Request<Full<Bytes>>) -> TransportFuture {
        struct Flag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let flag = Flag(self.0.clone());
        TransportFuture::io(async move {
            let _flag = flag;
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn abort_before_headers_cancels_the_exchange() {
    let request = parse_json(r#"{"provider":"p","model":"m","messages":[]}"#).unwrap();
    let config = parse_json(
        r#"{"providers":{"p":{"id":"p","protocol":"anthropic","url":"http://127.0.0.1:8080/v1","apiKey":"k","headers":{},
        "models":{"m":{"id":"m","capabilities":{"supportsStreaming":true,"maxOutputTokens":8},"multimodal":{"input":["text"]}}}}}}"#,
    )
    .unwrap();
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let abort = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(abort.clone()),
        ..StreamOptions::default()
    };
    let mut events = Vec::new();
    let later = abort.clone();
    let _abort = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        later.abort();
    });
    let mut sink = |output: StreamOutput| {
        if let StreamOutput::Event(event) = output {
            events.push(event);
        }
    };
    let error = stream_model(
        &Never(dropped.clone()),
        &request,
        &config,
        &options,
        &mut sink,
    )
    .await
    .unwrap_err();
    assert!(error.name().equals_str("AbortError"));
    assert_eq!(events.len(), 1, "request_started only");
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn google_protocol_dispatch_matches_the_recorded_sdk_request() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/model-stream-google/oracle.json"
    ));
    let merge = |base: &JsValue, overrides: &JsValue| {
        let mut result = base.as_object().unwrap().clone();
        if let Some(overrides) = overrides.as_object() {
            for (key, value) in overrides.entries() {
                result.insert(key.clone(), value.clone());
            }
        }
        JsValue::Object(result)
    };
    let mut checked = 0;
    let mut no_request = Vec::new();
    for case in super::super::count_body::array(field(&oracle, "cases")) {
        let recorded = super::super::count_body::array(field(case, "requests"));
        if recorded.is_empty() {
            no_request.push(header_text(field(case, "name")));
            continue;
        }
        let request = merge(field(&oracle, "request"), field(case, "request"));
        let provider = merge(field(&oracle, "provider"), field(case, "provider"));
        let model = field(
            field(&provider, "models"),
            &header_text(field(&request, "model")),
        );
        let body = providers::build_model_request(&request, &provider, model).unwrap();
        let body = providers::google::final_body(&body, &provider);
        for recorded in recorded {
            assert_eq!(
                crate::js_value::json_stringify(&body).unwrap().unwrap(),
                header_text(field(recorded, "body")),
                "{}: SDK request body",
                header_text(field(case, "name"))
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 29);
    assert_eq!(
        no_request,
        [
            "thinking-unsupported",
            "request-validation",
            "connection-refused"
        ]
    );
}

/// Scripted transport: one response per call, body frames served in order;
/// a frame of `None` stalls forever.
/// Status and body frames (`None` stalls) of one scripted response.
type Reply = (u16, Vec<Option<&'static [u8]>>);
struct Scripted(std::sync::Mutex<Vec<Reply>>);
impl ModelHttpTransport for Scripted {
    fn send(&self, _request: Request<Full<Bytes>>) -> TransportFuture {
        let (status, frames) = self.0.lock().unwrap().remove(0);
        let stream = futures_body(frames);
        TransportFuture::ready(Ok(Response::builder().status(status).body(stream).unwrap()))
    }
}
fn futures_body(frames: Vec<Option<&'static [u8]>>) -> ModelBody {
    struct Frames(std::collections::VecDeque<Option<&'static [u8]>>);
    impl hyper::body::Body for Frames {
        type Data = Bytes;
        type Error = BodyFailure;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
            match self.0.pop_front() {
                None => std::task::Poll::Ready(None),
                Some(None) => {
                    self.0.push_front(None);
                    std::task::Poll::Pending
                }
                Some(Some(bytes)) => std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(
                    Bytes::from_static(bytes),
                )))),
            }
        }
    }
    Frames(frames.into_iter().collect()).boxed()
}
pub(super) fn stream_config() -> (JsValue, JsValue) {
    let request = parse_json(r#"{"provider":"p","model":"m","messages":[]}"#).unwrap();
    let config = parse_json(
        r#"{"providers":{"p":{"id":"p","protocol":"anthropic","url":"http://127.0.0.1:8080/v1","apiKey":"k","headers":{},
        "retry":{"baseDelayMs":1,"maxDelayMs":1},
        "models":{"m":{"id":"m","capabilities":{"supportsStreaming":true,"maxOutputTokens":8},"multimodal":{"input":["text"]}}}}}}"#,
    )
    .unwrap();
    (request, config)
}

#[derive(Clone, Copy, Debug)]
enum RejectionOrder {
    ReadySyncAbort,
    OverdueSyncAbort,
    ErrorBeforeParentTimer,
    BothOverdueErrorFirst,
    BothOverdueParentFirst,
}

/// #415/#410: actual streamModel.ts at cc7c4e65 (SHA-256
/// 6e2f9450194326b89b16ffbc99254f03687d9e908fe17c740fe9daa02e98bd26)
/// was probed with a rejecting fetch (Google: generateContentStream Promise).
/// Ready rejection + same-stack abort, and an overdue but undispatched error
/// timer + same-stack abort, both throw AbortError: This operation was aborted.
/// If the 5ms error callback and its microtasks finish before the 10ms parent
/// callback, HTTP throws ModelProviderError: fixture transport failure; Google
/// emits an error event with that message. A settled rejection's continuation
/// runs, but the provider catch rechecks signal.aborted (TS :749 / :502).
/// Unlike countRequestInput, it cannot simply preserve the rejected value.
/// Both timers overdue (busy-wait 30ms): error at 5ms / parent at 10ms keeps
/// the provider error; parent at 5ms / error at 10ms throws AbortError for all
/// four protocols. The event loop drains the error microtasks before the later
/// parent callback. Probe: _tmp/20261022-issue-410/provider-abort-probe.mts.
async fn assert_provider_rejection_order(order: RejectionOrder) {
    struct Rejecting {
        order: RejectionOrder,
        signal: AbortFlag,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl ModelHttpTransport for Rejecting {
        fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let error = Err(TransportFailure {
                cause_code: None,
                message: "fixture transport failure".into(),
            });
            match self.order {
                RejectionOrder::ReadySyncAbort => {
                    let rejected = TransportFuture::ready(error);
                    self.signal.abort();
                    rejected
                }
                RejectionOrder::BothOverdueParentFirst => {
                    TransportFuture::after(Duration::from_millis(10), error)
                }
                _ => TransportFuture::after(Duration::from_millis(5), error),
            }
        }
    }
    for protocol in ["anthropic", "openai", "openai-responses", "google"] {
        let (request, mut config) = stream_config();
        let mut provider = field(field(&config, "providers"), "p").clone();
        put(&mut provider, "protocol", protocol.into());
        put(
            &mut provider,
            "retry",
            parse_json(r#"{"streamMaxRetries":0}"#).unwrap(),
        );
        let mut providers = JsObject::new();
        providers.insert("p".into(), provider);
        put(&mut config, "providers", JsValue::Object(providers));
        let signal = AbortFlag::new();
        let transport = Rejecting {
            order,
            signal: signal.clone(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let options = StreamOptions {
            abort: Some(signal.clone()),
            ..StreamOptions::default()
        };
        let mut events = Vec::new();
        let mut sink = |event| events.push(event);
        let mut pending = Box::pin(stream_model(
            &transport, &request, &config, &options, &mut sink,
        ));
        let started_at = tokio::time::Instant::now();
        match order {
            RejectionOrder::ReadySyncAbort => {}
            RejectionOrder::OverdueSyncAbort => {
                poll_pending(pending.as_mut()).await;
                tokio::time::advance(Duration::from_millis(20)).await;
                signal.abort();
            }
            RejectionOrder::ErrorBeforeParentTimer => {
                poll_pending(pending.as_mut()).await;
                tokio::time::advance(Duration::from_millis(5)).await;
            }
            RejectionOrder::BothOverdueErrorFirst | RejectionOrder::BothOverdueParentFirst => {
                poll_pending(pending.as_mut()).await;
                tokio::time::advance(Duration::from_millis(30)).await;
                let parent_delay = if matches!(order, RejectionOrder::BothOverdueErrorFirst) {
                    10
                } else {
                    5
                };
                // Neither the 5ms nor the 10ms event has been polled since
                // registration. Retain the parent's original timer deadline.
                signal.cancel_at(started_at + Duration::from_millis(parent_delay));
                assert!(signal.is_aborted(), "both timers overdue before polling");
            }
        }
        let outcome = poll_ready(pending.as_mut()).await;
        drop(pending);
        assert_eq!(transport.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(&events[0], StreamOutput::Event(event)
            if field(event, "type") == &JsValue::from("request_started")));
        if matches!(
            order,
            RejectionOrder::ErrorBeforeParentTimer | RejectionOrder::BothOverdueErrorFirst
        ) {
            if matches!(order, RejectionOrder::ErrorBeforeParentTimer) {
                assert!(!signal.is_aborted(), "the error microtasks run first");
            }
            if protocol == "google" {
                outcome.unwrap();
                assert_eq!(events.len(), 2);
                assert!(matches!(&events[1], StreamOutput::Event(event)
                    if field(event, "type") == &JsValue::from("error")
                        && field(field(event, "error"), "message")
                            == &JsValue::from("fixture transport failure")));
            } else {
                let error = outcome.unwrap_err();
                assert_eq!(error.name(), JsString::from("ModelProviderError"));
                assert_eq!(error.message_text(), "fixture transport failure");
                assert_eq!(events.len(), 1);
            }
            // Deliver the parent only after the rejection continuation has
            // finished, as Node does between separate timer callbacks.
            if matches!(order, RejectionOrder::ErrorBeforeParentTimer) {
                tokio::time::advance(Duration::from_millis(5)).await;
                signal.cancel();
            }
        } else {
            let error = outcome.expect_err("earlier parent abort must be thrown");
            assert_eq!(
                error.name(),
                JsString::from("AbortError"),
                "{protocol}: {order:?}"
            );
            assert_eq!(error.message_text(), "This operation was aborted");
            assert_eq!(events.len(), 1, "no retry or error event after abort");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn provider_rejected_promise_rechecks_synchronous_abort() {
    assert_provider_rejection_order(RejectionOrder::ReadySyncAbort).await;
}

#[tokio::test(start_paused = true)]
async fn provider_overdue_rejection_loses_to_synchronous_abort() {
    assert_provider_rejection_order(RejectionOrder::OverdueSyncAbort).await;
}

#[tokio::test(start_paused = true)]
async fn provider_rejection_microtasks_finish_before_later_parent_timer() {
    assert_provider_rejection_order(RejectionOrder::ErrorBeforeParentTimer).await;
}

#[tokio::test(start_paused = true)]
async fn provider_both_overdue_timers_preserve_error_continuation_order() {
    assert_provider_rejection_order(RejectionOrder::BothOverdueErrorFirst).await;
    assert_provider_rejection_order(RejectionOrder::BothOverdueParentFirst).await;
}

/// Real streamModel.ts: an overdue 100ms Google idle watchdog finishes its
/// error microtasks before the overdue 120ms parent callback. Check the public
/// stream entry, including throwIfGoogleAbort and canonical error emission.
#[tokio::test(start_paused = true)]
async fn google_idle_timeout_continuation_precedes_later_parent_timer() {
    struct PendingHeaders;
    impl ModelHttpTransport for PendingHeaders {
        fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
            TransportFuture::io(std::future::pending())
        }
    }
    for (parent_ms, timeout_wins) in [(120, true), (100, false), (80, false)] {
        let (request, mut config) = stream_config();
        let mut provider = field(field(&config, "providers"), "p").clone();
        put(&mut provider, "protocol", "google".into());
        put(
            &mut provider,
            "retry",
            parse_json(r#"{"streamMaxRetries":0}"#).unwrap(),
        );
        let mut providers = JsObject::new();
        providers.insert("p".into(), provider);
        put(&mut config, "providers", JsValue::Object(providers));
        let signal = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(signal.clone()),
            stream_timeout_ms: Some(100.0),
            ..StreamOptions::default()
        };
        let mut events = Vec::new();
        let mut sink = |event| events.push(event);
        let mut pending = Box::pin(stream_model(
            &PendingHeaders,
            &request,
            &config,
            &options,
            &mut sink,
        ));
        let started = tokio::time::Instant::now();
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(150)).await;
        signal.cancel_at(started + Duration::from_millis(parent_ms));
        let result = poll_ready(pending.as_mut()).await;
        drop(pending);
        if timeout_wins {
            result.unwrap();
            assert_eq!(events.len(), 2);
            assert!(matches!(&events[1], StreamOutput::Event(event)
                if field(event, "type") == &JsValue::from("error")
                    && field(field(event, "error"), "code") == &JsValue::from("timeout")
                    && field(field(event, "error"), "message") == &JsValue::from("Stream idle timeout: no data received for 100ms")
                    && field(field(field(event, "error"), "raw"), "name") == &JsValue::from("StreamIdleTimeoutError")));
        } else {
            assert!(result.unwrap_err().name().equals_str("AbortError"));
            assert_eq!(events.len(), 1);
        }
    }
}

/// The same TS watchdog wraps every stream.next(), after headers and after
/// partial text. Exercise the outer Google abort recheck, not only read_frame.
#[tokio::test(start_paused = true)]
async fn google_body_idle_timeout_continuation_precedes_later_parent_timer() {
    for partial in [true, false] {
        for (parent_ms, synchronous, timeout_wins) in [
            (120, false, true),
            (100, false, false),
            (80, false, false),
            (120, true, false),
        ] {
            let (request, mut config) = stream_config();
            let mut provider = field(field(&config, "providers"), "p").clone();
            put(&mut provider, "protocol", "google".into());
            put(
                &mut provider,
                "retry",
                parse_json(r#"{"streamMaxRetries":0}"#).unwrap(),
            );
            let mut providers = JsObject::new();
            providers.insert("p".into(), provider);
            put(&mut config, "providers", JsValue::Object(providers));
            let frames = if partial {
                vec![Some(b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hello\"}]}}]}\n\n".as_slice()), None]
            } else {
                vec![None]
            };
            let transport = Scripted(std::sync::Mutex::new(vec![(200, frames)]));
            let signal = AbortFlag::new();
            let options = StreamOptions {
                abort: Some(signal.clone()),
                stream_timeout_ms: Some(100.0),
                ..StreamOptions::default()
            };
            let mut events = Vec::new();
            let mut sink = |output| {
                if let StreamOutput::Event(event) = output {
                    events.push(event);
                }
            };
            let mut pending = Box::pin(stream_model(
                &transport, &request, &config, &options, &mut sink,
            ));
            let started = tokio::time::Instant::now();
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(150)).await;
            if synchronous {
                signal.abort();
            } else {
                signal.cancel_at(started + Duration::from_millis(parent_ms));
            }
            let result = poll_ready(pending.as_mut()).await;
            drop(pending);
            assert_eq!(transport.0.lock().unwrap().len(), 0);
            let mut expected_tags = vec!["request_started"];
            if partial {
                expected_tags.extend(["message_start", "text_delta"]);
                assert_eq!(field(&events[2], "text"), &JsValue::from("hello"));
            }
            if timeout_wins {
                result.unwrap();
                expected_tags.push("error");
                let error = field(events.last().unwrap(), "error");
                assert_eq!(field(error, "code"), &JsValue::from("timeout"));
                assert_eq!(
                    field(field(error, "raw"), "name"),
                    &JsValue::from("StreamIdleTimeoutError")
                );
                assert_eq!(
                    field(error, "message"),
                    &JsValue::from("Stream idle timeout: no data received for 100ms")
                );
                assert_eq!(
                    field(field(error, "streamInterruption"), "phase"),
                    &JsValue::from(if partial { "text" } else { "empty" })
                );
            } else {
                assert!(result.unwrap_err().name().equals_str("AbortError"));
            }
            assert_eq!(tags(&events), expected_tags);
        }
    }
}

/// TS throwIfGoogleAbort checks the message before canonicalization/retries,
/// even when the parent signal has never been cancelled.
#[tokio::test(start_paused = true)]
async fn google_abort_message_preserves_error_without_retry() {
    struct AbortedTransport(std::sync::atomic::AtomicUsize);
    impl ModelHttpTransport for AbortedTransport {
        fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            TransportFuture::ready(Err(TransportFailure {
                cause_code: None,
                message: "fetch failed: upstream request ABORTED unexpectedly".into(),
            }))
        }
    }
    let (request, mut config) = stream_config();
    let mut provider = field(field(&config, "providers"), "p").clone();
    put(&mut provider, "protocol", "google".into());
    put(
        &mut provider,
        "retry",
        parse_json(r#"{"streamMaxRetries":2,"baseDelayMs":1,"maxDelayMs":1}"#).unwrap(),
    );
    let mut providers = JsObject::new();
    providers.insert("p".into(), provider);
    put(&mut config, "providers", JsValue::Object(providers));
    let signal = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(signal.clone()),
        ..StreamOptions::default()
    };
    let transport = AbortedTransport(std::sync::atomic::AtomicUsize::new(0));
    let mut outputs = Vec::new();
    let outcome = stream_model(&transport, &request, &config, &options, &mut |output| {
        outputs.push(output);
    })
    .await;
    assert!(!signal.is_aborted());
    assert_eq!(
        transport.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "abort-like transport errors must not start another attempt"
    );
    let error = outcome.expect_err("abort-like errors must be thrown, not emitted");
    assert!(matches!(error, ModelStreamError::Thrown { name, message }
        if name.equals_str("TypeError")
            && message.equals_str("fetch failed: upstream request ABORTED unexpectedly")));
    assert_eq!(
        outputs.len(),
        1,
        "request_started only, no retry or error event"
    );
    assert!(matches!(&outputs[0], StreamOutput::Event(event)
        if field(event, "type") == &JsValue::from("request_started")));
}

#[tokio::test]
async fn undelimited_sse_buffer_over_the_v8_limit_throws_range_error() {
    let _limit = SseLimitGuard::set(64);
    let (request, config) = stream_config();
    // Valid events are consumed; then 65 units without "\n\n" overflow.
    let transport = Scripted(std::sync::Mutex::new(vec![(
        200,
        vec![
            Some(b"data: {\"type\":\"message_start\"}\n\n".as_slice()),
            Some(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"".as_slice()),
            Some(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".as_slice()),
        ],
    )]));
    let mut events = Vec::new();
    let mut sink = |output: StreamOutput| {
        if let StreamOutput::Event(event) = output {
            events.push(event);
        }
    };
    let error = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut sink,
    )
    .await
    .unwrap_err();
    assert!(error.name().equals_str("RangeError"), "{error:?}");
    assert!(error.message().equals_str("Invalid string length"));
    assert_eq!(events.len(), 2, "request_started + message_start, no retry");
}

#[tokio::test]
async fn stalled_error_body_hits_the_body_timeout() {
    let (request, config) = stream_config();
    // Headers of a 500, half a JSON body, then silence.
    let transport = Scripted(std::sync::Mutex::new(vec![(
        500,
        vec![Some(b"{\"type\":\"error\",".as_slice()), None],
    )]));
    let options = StreamOptions {
        body_timeout_ms: 50.0,
        ..StreamOptions::default()
    };
    let mut sink = |_: StreamOutput| {};
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        stream_model(&transport, &request, &config, &options, &mut sink),
    )
    .await
    .expect("must not hang without an abort signal")
    .unwrap_err();
    assert!(error.name().equals_str("TypeError") && error.message().equals_str("terminated"));
    assert!(started.elapsed() >= Duration::from_millis(50));
}

#[test]
fn retry_after_ranges_follow_node_date_parse() {
    // node probe (r1/probe-r1.out.txt): 32 Oct / 00 / 25:00 / 07:60 /
    // 24:00:01 -> NaN; 31 Feb rolls over; 24:00:00 is next midnight; a
    // second >= 60 reads as :00. These now exercise the shared Node22
    // `Date.parse` port (#360-2); the hand-rolled IMF-only parser is gone.
    let at = |text: &str| errors::retry_after_header(Some(text), 0.0);
    assert_eq!(
        at("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(1_445_412_480_000.0)
    );
    assert_eq!(at("Wed, 32 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(at("Wed, 00 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(at("Wed, 21 Oct 2015 25:00:00 GMT"), None);
    assert_eq!(at("Wed, 21 Oct 2015 07:60:00 GMT"), None);
    assert_eq!(at("Wed, 21 Oct 2015 24:00:01 GMT"), None);
    assert_eq!(at("Wed, 99999999999999999999 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(
        at("Wed, 31 Feb 2015 07:28:00 GMT"),
        Some(1_425_367_680_000.0)
    );
    assert_eq!(
        at("Wed, 21 Oct 2015 24:00:00 GMT"),
        Some(1_445_472_000_000.0)
    );
    assert_eq!(
        at("Wed, 1 Oct 2015 07:28:00 GMT"),
        Some(1_443_684_480_000.0)
    );
    assert_eq!(
        at("Wed, 21 Oct 2015 07:28:61 GMT"),
        Some(1_445_412_480_000.0)
    );
    assert_eq!(at("Wed, 21 Oct 275761 07:28:00 GMT"), None);
}

#[test]
fn abort_message_on_provider_errors_is_not_retryable() {
    let mut canonical = JsObject::new();
    canonical.insert("message".into(), "request aborted by upstream".into());
    canonical.insert("retryable".into(), JsValue::Bool(true));
    assert!(!is_retryable_stream_error(&ModelStreamError::Provider(
        canonical
    )));
}

fn run_with_limit(
    limit: usize,
    frames: Vec<Option<&'static [u8]>>,
) -> (ModelStreamError, Vec<JsValue>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _limit = LimitGuard::set(limit);
    let (request, config) = stream_config();
    let transport = Scripted(std::sync::Mutex::new(vec![(200, frames)]));
    let mut events = Vec::new();
    let mut sink = |output: StreamOutput| {
        if let StreamOutput::Event(event) = output {
            events.push(event);
        }
    };
    let error = runtime
        .block_on(stream_model(
            &transport,
            &request,
            &config,
            &StreamOptions::default(),
            &mut sink,
        ))
        .unwrap_err();
    (error, events)
}
fn tags(events: &[JsValue]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            e.get("type")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap()
        })
        .collect()
}

/// node probe r2: `inputBuffer + partial` past V8's limit throws
/// `RangeError: Invalid string length` out of the normalizer; streamModel
/// does not retry it and yields nothing for that SSE event.
#[test]
fn tool_input_accumulator_over_the_v8_limit_throws_range_error() {
    let (error, events) = run_with_limit(
        12,
        vec![
            Some(b"data: {\"type\":\"message_start\"}\n\n".as_slice()),
            Some(b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"n\"}}\n\n".as_slice()),
            // 12 units: exactly the limit is still allowed.
            Some(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"abcdef\"}}\n\n".as_slice()),
            Some(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"x\"}}\n\n".as_slice()),
        ],
    );
    assert!(error.name().equals_str("RangeError"), "{error:?}");
    assert!(error.message().equals_str("Invalid string length"));
    assert_eq!(
        tags(&events),
        [
            "request_started",
            "message_start",
            "tool_call_start",
            "tool_call_delta"
        ],
        "no retry, no event for the overflowing delta"
    );
}

/// `partialText += event.text` in the checkpoint throws after the stream
/// guard ran and before the event is yielded.
#[test]
fn partial_text_accumulator_over_the_v8_limit_throws_range_error() {
    let (error, events) = run_with_limit(
        8,
        vec![
            Some(b"data: {\"type\":\"message_start\"}\n\n".as_slice()),
            Some(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"abcdefgh\"}}\n\n".as_slice()),
            Some(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"i\"}}\n\n".as_slice()),
        ],
    );
    assert!(
        error.name().equals_str("RangeError")
            && error.message().equals_str("Invalid string length")
    );
    assert_eq!(
        tags(&events),
        ["request_started", "message_start", "text_delta"]
    );
}

#[test]
fn append_js_string_allows_exactly_the_limit() {
    let _limit = LimitGuard::set(3);
    let mut buffer = vec![1, 2];
    assert!(append_js_string(&mut buffer, &[3]).is_ok());
    assert!(append_js_string(&mut buffer, &[4]).is_err());
    assert_eq!(buffer, [1, 2, 3], "unchanged after the throw");
}

/// node probe r2 (`r2/probe-r2.out.txt`): years beyond V8's range are NaN
/// (no overflow panic for 9 digits), the ±8.64e15 edge is inclusive, and
/// years below 100 are two-digit whatever their padding.
#[test]
fn retry_after_year_edges_follow_node_date_parse() {
    let at = |text: &str| errors::retry_after_header(Some(text), 0.0);
    assert_eq!(at("Fri, 01 Jan 999999999 00:00:00 GMT"), None);
    assert_eq!(at("Fri, 01 Jan 292278995 00:00:00 GMT"), None);
    assert_eq!(at("Fri, 01 Jan 1000000 00:00:00 GMT"), None);
    assert_eq!(at("Fri, 31 Dec 999999999 24:00:00 GMT"), None);
    assert_eq!(
        at("Fri, 01 Jan 275760 00:00:00 GMT"),
        Some(8_639_977_881_600_000.0)
    );
    assert_eq!(
        at("Fri, 13 Sep 275760 00:00:00 GMT"),
        Some(8_640_000_000_000_000.0)
    );
    assert_eq!(at("Fri, 14 Sep 275760 00:00:00 GMT"), None);
    assert_eq!(at("Fri, 01 Jan 0 00:00:00 GMT"), Some(946_684_800_000.0));
    assert_eq!(at("Fri, 01 Jan 0000 00:00:00 GMT"), Some(946_684_800_000.0));
    assert_eq!(at("Wed, 21 Oct 0099 07:28:00 GMT"), Some(940_490_880_000.0));
    assert_eq!(at("Fri, 01 Jan 2015 999999999:00:00 GMT"), None);
    assert_eq!(
        at("Fri, 01 Jan 2015 00:00:999999999 GMT"),
        Some(1_420_070_400_000.0)
    );
}

/// Text deltas accumulate across events in `StreamingCheckpoint` with
/// `append_js_string`: the V8 string cap is reached on the third delta.
#[test]
fn text_deltas_accumulate_under_the_v8_string_limit() {
    use pilotdeck_protocol::parse_json;
    let _limit = LimitGuard::set(4);
    let mut checkpoint = Checkpoint::default();
    let mut deltas = 0;
    for text in ["ab", "cd"] {
        let event = parse_json(&format!(r#"{{"type":"text_delta","text":"{text}"}}"#)).unwrap();
        checkpoint.on_event(&event).unwrap();
        deltas += 1;
    }
    let event = parse_json(r#"{"type":"text_delta","text":"e"}"#).unwrap();
    let error = checkpoint.on_event(&event).unwrap_err();
    assert!(matches!(
        error,
        ModelStreamError::Thrown { ref name, ref message }
            if name.equals_str("RangeError") && message.equals_str("Invalid string length")
    ));
    assert_eq!(deltas, 2);
}

/// The provider must preserve the transport's timer through redirects and
/// endpoint fallback, including when all events are observed in one late poll.
pub(super) struct TimedHeaders(pub(super) Duration);
impl ModelHttpTransport for TimedHeaders {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        TransportFuture::after(
            self.0,
            Ok(Response::new(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
            )),
        )
    }
}

pub(super) async fn poll_ready<F: Future>(mut future: Pin<&mut F>) -> F::Output {
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        std::task::Poll::Ready(result) => std::task::Poll::Ready(result),
        std::task::Poll::Pending => panic!("must settle without advancing the paused clock"),
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn provider_exchange_orders_response_timeout_and_parent() {
    for (response_ms, parent_ms, sync, expected) in [
        (60, 80, false, "response"),
        (100, 120, false, "timeout"),
        (110, 90, false, "abort"),
        (60, 80, true, "abort"),
    ] {
        let provider = parse_json(r#"{"id":"p","protocol":"anthropic","url":"http://example.test/v1","apiKey":"k","headers":{}}"#).unwrap();
        let abort = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            stream_timeout_ms: Some(100.0),
            ..StreamOptions::default()
        };
        let started = tokio::time::Instant::now();
        let transport = TimedHeaders(Duration::from_millis(response_ms));
        let body = JsValue::Null;
        let mut request = Box::pin(send_provider_request(
            &transport, &provider, &body, &options, true,
        ));
        poll_pending(request.as_mut()).await;
        tokio::time::advance(Duration::from_millis(130)).await;
        if sync {
            abort.abort();
        } else {
            abort.cancel_at(started + Duration::from_millis(parent_ms));
        }
        let result = poll_ready(request.as_mut()).await;
        match expected {
            "response" => assert!(result.is_ok(), "{result:?}"),
            "abort" => assert!(result.unwrap_err().name().equals_str("AbortError")),
            "timeout" => assert!(
                result
                    .unwrap_err()
                    .message_text()
                    .contains("Stream idle timeout")
            ),
            _ => unreachable!(),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn provider_retry_delay_orders_expiry_and_parent() {
    for (parent_ms, sync, success) in [
        (49, false, false),
        (50, false, true),
        (60, false, true),
        (60, true, false),
    ] {
        let abort = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            ..StreamOptions::default()
        };
        let started = tokio::time::Instant::now();
        let mut pending = Box::pin(delay(50.0, &options));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(70)).await;
        if sync {
            abort.abort();
        } else {
            abort.cancel_at(started + Duration::from_millis(parent_ms));
        }
        let result = poll_ready(pending.as_mut()).await;
        assert_eq!(
            result.is_ok(),
            success,
            "parent_ms={parent_ms}, sync={sync}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn provider_overdue_retry_continuation_starts_next_attempt_before_later_abort() {
    let (request, mut config) = stream_config();
    let mut provider = field(field(&config, "providers"), "p").clone();
    put(
        &mut provider,
        "retry",
        parse_json(r#"{"baseDelayMs":50,"maxDelayMs":50,"jitter":0}"#).unwrap(),
    );
    let mut providers = JsObject::new();
    providers.insert("p".into(), provider);
    put(&mut config, "providers", JsValue::Object(providers));
    let transport = Scripted(std::sync::Mutex::new(vec![
        (429, vec![Some(b"{}")]),
        (204, Vec::new()),
    ]));
    let abort = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(abort.clone()),
        ..StreamOptions::default()
    };
    let started = tokio::time::Instant::now();
    let mut sink = |_| {};
    let mut pending = Box::pin(stream_model(
        &transport, &request, &config, &options, &mut sink,
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(70)).await;
    abort.cancel_at(started + Duration::from_millis(60));
    poll_ready(pending.as_mut()).await.unwrap();
    assert!(
        transport.0.lock().unwrap().is_empty(),
        "retry's continuation must issue attempt two"
    );
}

#[tokio::test(start_paused = true)]
async fn google_overdue_retry_continuation_starts_next_attempt_before_later_abort() {
    for partial in [true, false] {
        for (parent_ms, sync, calls) in [(60, false, 2), (49, false, 1), (60, true, 1)] {
            let (request, mut config) = stream_config();
            let mut provider = field(field(&config, "providers"), "p").clone();
            put(&mut provider, "protocol", "google".into());
            put(
                &mut provider,
                "retry",
                parse_json(r#"{"baseDelayMs":50,"maxDelayMs":50,"jitter":0}"#).unwrap(),
            );
            let mut providers = JsObject::new();
            providers.insert("p".into(), provider);
            put(&mut config, "providers", JsValue::Object(providers));
            let transport = Scripted(std::sync::Mutex::new(vec![
                (
                    if partial { 200 } else { 429 },
                    if partial {
                        vec![Some(b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]}}]}\n\n".as_slice())]
                    } else {
                        vec![Some(b"{}".as_slice())]
                    },
                ),
                (204, Vec::new()),
            ]));
            let abort = AbortFlag::new();
            let options = StreamOptions {
                abort: Some(abort.clone()),
                ..StreamOptions::default()
            };
            let started = tokio::time::Instant::now();
            let mut sink = |_| {};
            let mut pending = Box::pin(stream_model(
                &transport, &request, &config, &options, &mut sink,
            ));
            poll_pending(pending.as_mut()).await;
            tokio::time::advance(Duration::from_millis(70)).await;
            if sync {
                abort.abort();
            } else {
                abort.cancel_at(started + Duration::from_millis(parent_ms));
            }
            let result = poll_ready(pending.as_mut()).await;
            if calls == 2 {
                // The queued retry and its ready 204 error both precede the
                // later parent timer (real TS emits a provider error).
                result.unwrap();
            } else {
                assert!(result.unwrap_err().name().equals_str("AbortError"));
            }
            assert_eq!(2 - transport.0.lock().unwrap().len(), calls);
        }
    }
}

/// Keep the independent undici bodyTimeout and synchronous cancellation cases.
/// TS response.text has detached its parent listener; SSE still observes it.
async fn assert_provider_body_ordering(text: bool) {
    for sync in [true, false] {
        let abort = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            body_timeout_ms: 600_000.0,
            ..StreamOptions::default()
        };
        let mut body = PendingBody.boxed();
        let mut continuation = false;
        let mut pending = Box::pin(async {
            if text {
                response_text(&mut body, &options, false).await.map(|_| ())
            } else {
                read_frame(&mut body, 700_000.0, &options, &mut continuation)
                    .await
                    .map(|_| ())
            }
        });
        let started = tokio::time::Instant::now();
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(599_999)).await;
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(2)).await;
        if sync {
            abort.abort();
        } else {
            abort.cancel_at(started + Duration::from_millis(600_001));
        }
        let error = poll_ready(pending.as_mut()).await.unwrap_err();
        assert_eq!(
            error.message_text(),
            if sync && !text {
                "This operation was aborted"
            } else {
                "terminated"
            }
        );
    }
}

#[tokio::test(start_paused = true)]
async fn provider_body_text_timeout_outlives_detached_parent() {
    assert_provider_body_ordering(true).await;
}
#[tokio::test(start_paused = true)]
async fn provider_body_frame_orders_idle_timeout_and_abort() {
    assert_provider_body_ordering(false).await;
}

/// Exercise the public stream entry point, including its pre/post-read abort
/// checks. TS drains the queued body continuation before a later abort timer.
async fn assert_headers_reach_first_body(google: bool, error_body: bool) {
    struct HeadersAndBody {
        google: bool,
        error_body: bool,
        stalled: bool,
    }
    impl ModelHttpTransport for HeadersAndBody {
        fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
            let data = if self.error_body {
                r#"{"error":{"message":"FIRST","code":400,"status":"INVALID_ARGUMENT"}}"#
            } else if self.google {
                "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"FIRST\"}]},\"finishReason\":\"STOP\"}]}\n\n"
            } else {
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"FIRST\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
            };
            let body = if self.stalled {
                PendingBody.boxed()
            } else {
                Full::new(Bytes::from(data))
                    .map_err(|never| match never {})
                    .boxed()
            };
            TransportFuture::after(
                Duration::from_millis(20),
                Ok(Response::builder()
                    .status(if self.error_body { 400 } else { 200 })
                    .header(
                        "content-type",
                        if self.error_body {
                            "application/json"
                        } else {
                            "text/event-stream"
                        },
                    )
                    .body(body)
                    .unwrap()),
            )
        }
    }
    for (parent_ms, sync, stalled, expected) in [
        (30, false, false, true),
        (10, false, false, false),
        (30, true, false, false),
        (30, false, true, false),
    ] {
        if !google && error_body && stalled {
            continue; // Detached response.text cancellation is in the TS matrix.
        }
        let (request, mut config) = stream_config();
        let mut provider = field(field(&config, "providers"), "p").clone();
        put(
            &mut provider,
            "protocol",
            if google { "google" } else { "anthropic" }.into(),
        );
        put(
            &mut provider,
            "retry",
            parse_json(r#"{"streamMaxRetries":0}"#).unwrap(),
        );
        let mut providers = JsObject::new();
        providers.insert("p".into(), provider);
        put(&mut config, "providers", JsValue::Object(providers));
        let signal = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(signal.clone()),
            ..StreamOptions::default()
        };
        let transport = HeadersAndBody {
            google,
            error_body,
            stalled,
        };
        let mut events = Vec::new();
        let mut sink = |event| events.push(event);
        let started = tokio::time::Instant::now();
        let mut pending = Box::pin(stream_model(
            &transport, &request, &config, &options, &mut sink,
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(70)).await;
        if sync {
            signal.abort();
        } else {
            signal.cancel_at(started + Duration::from_millis(parent_ms));
        }
        let outcome = poll_ready(pending.as_mut()).await;
        drop(pending);
        let first = events.iter().any(|event| match event {
            StreamOutput::Event(value) if error_body => field(field(value, "error"), "message")
                .as_string()
                .is_some_and(|s| String::from_utf16_lossy(s.units()).contains("FIRST")),
            StreamOutput::Event(value) => field(value, "text") == &JsValue::from("FIRST"),
            _ => false,
        });
        assert_eq!(
            first, expected,
            "google={google}, error_body={error_body}, parent={parent_ms}, sync={sync}, stalled={stalled}: {outcome:?}"
        );
        if expected {
            outcome.unwrap();
            if !error_body {
                assert!(events.iter().any(|event| matches!(event,
                    StreamOutput::Event(value) if field(value, "type") == &JsValue::from("message_end"))));
                assert!(
                    !events.iter().any(|event| matches!(event,
                    StreamOutput::Event(value) if field(value, "type") == &JsValue::from("error")))
                );
            }
        } else {
            assert!(outcome.unwrap_err().name().equals_str("AbortError"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn provider_headers_continue_into_first_frame() {
    assert_headers_reach_first_body(false, false).await;
}
#[tokio::test(start_paused = true)]
async fn provider_headers_continue_into_error_text() {
    assert_headers_reach_first_body(false, true).await;
}
#[tokio::test(start_paused = true)]
async fn google_headers_continue_into_first_frame() {
    assert_headers_reach_first_body(true, false).await;
}
#[tokio::test(start_paused = true)]
async fn google_headers_continue_into_error_text() {
    assert_headers_reach_first_body(true, true).await;
}

// ---------------------------------------------------------------------------
// Non-stream multi-endpoint shape probe: V8 `MAX_STRING_LENGTH` bound.
//
// TS `shouldUseEndpointResponse` runs `await response.clone().json()` for every
// 2xx candidate while `endpoints.length > 1`. The clone reads the body to EOF
// first; only then does the byte→string conversion throw. A body over V8's
// 536870888 UTF-16-unit limit therefore rejects the shape (fall back), while
// the last candidate is still returned and its later `response.text()` throws
// the same plain `Error: Cannot create a string longer than 0x1fffffe8
// characters` (real probes `probe-shape-string-limit-valid.mts` and
// `-last.mts` in the issue's `_tmp/20261025-issue-610`).

fn complete_config() -> (JsValue, JsValue) {
    let request = parse_json(
        r#"{"provider":"p","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}],"maxOutputTokens":32}"#,
    )
    .unwrap();
    let config = parse_json(
        r#"{"providers":{"p":{"id":"p","protocol":"openai","url":"http://127.0.0.1:18080/api","apiKey":"k","headers":{},"retry":{"requestMaxRetries":0,"baseDelayMs":0,"maxDelayMs":0},"models":{"m":{"id":"m","capabilities":{"maxContextTokens":1000,"maxOutputTokens":32},"multimodal":{"input":["text"]}}}}}}"#,
    )
    .unwrap();
    (request, config)
}

fn complete_config_with_timeout(timeout_ms: f64) -> (JsValue, JsValue) {
    let (request, mut config) = complete_config();
    let mut provider = field(field(&config, "providers"), "p").clone();
    put(&mut provider, "timeoutMs", JsValue::Number(timeout_ms));
    let mut providers = JsObject::new();
    providers.insert("p".into(), provider);
    put(&mut config, "providers", JsValue::Object(providers));
    (request, config)
}

/// A URL that resolves to exactly one endpoint candidate, so `complete()`
/// reads the body with the plain `response_text` reader instead of the
/// multi-endpoint shape probe (`buildProviderChatEndpointCandidates` returns
/// one candidate when the base URL already ends in `chat/completions`).
fn complete_single_endpoint_config() -> (JsValue, JsValue) {
    let (request, mut config) = complete_config();
    let mut provider = field(field(&config, "providers"), "p").clone();
    put(
        &mut provider,
        "url",
        JsValue::String("http://127.0.0.1:18080/api/v1/chat/completions".into()),
    );
    let mut providers = JsObject::new();
    providers.insert("p".into(), provider);
    put(&mut config, "providers", JsValue::Object(providers));
    (request, config)
}

const TEST_TINY_CHOICES: &[u8] = br#"{"choices":[]}"#;
/// 53 UTF-16 units, valid OpenAI shape, over every small test limit below.
pub(super) const TEST_OVERLONG_CHOICES: &[u8] =
    br#"{"choices":[],"pad":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;

#[derive(Clone, Copy)]
pub(super) enum TestFrame {
    Data(&'static [u8]),
    /// Sleep before yielding this frame; the timer starts on its first poll.
    After(Duration, &'static [u8]),
    /// undici `TypeError: terminated`.
    Error,
}

pub(super) struct SequenceBody {
    pub(super) frames: std::collections::VecDeque<TestFrame>,
    pub(super) timer: Option<Pin<Box<tokio::time::Sleep>>>,
    /// Set once the frame stream reached EOF, so tests can assert the reader
    /// drained the body instead of timing the frames.
    pub(super) eof: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl hyper::body::Body for SequenceBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        let Some(frame) = self.frames.front().copied() else {
            self.eof.store(true, std::sync::atomic::Ordering::Relaxed);
            return std::task::Poll::Ready(None);
        };
        match frame {
            TestFrame::Data(data) => {
                self.frames.pop_front();
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(data)))))
            }
            TestFrame::After(delay, data) => {
                if self.timer.is_none() {
                    self.timer = Some(Box::pin(tokio::time::sleep(delay)));
                }
                std::task::ready!(self.timer.as_mut().unwrap().as_mut().poll(cx));
                self.timer = None;
                self.frames.pop_front();
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(data)))))
            }
            TestFrame::Error => {
                self.frames.pop_front();
                std::task::Poll::Ready(Some(Err(BodyFailure::Terminated)))
            }
        }
    }
}

/// Scripted transport with ordered frame timing; one entry per endpoint.
/// The parallel flag list records, per requested endpoint, whether its body
/// was polled to EOF — the deterministic drain signal the cap tests assert
/// instead of wall-clock frame timing.
struct Sequence(
    std::sync::Mutex<Vec<(u16, Vec<TestFrame>)>>,
    Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
);
impl ModelHttpTransport for Sequence {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        let mut replies = self.0.lock().unwrap();
        let (status, frames) = replies.remove(0);
        let index = self.1.len() - replies.len() - 1;
        drop(replies);
        let body = SequenceBody {
            frames: frames.into(),
            timer: None,
            eof: self.1[index].clone(),
        }
        .boxed();
        TransportFuture::ready(Ok(Response::builder().status(status).body(body).unwrap()))
    }
}
impl Sequence {
    /// Whether the i-th requested endpoint's body reached EOF.
    fn eof(&self, endpoint: usize) -> bool {
        self.1[endpoint].load(std::sync::atomic::Ordering::Relaxed)
    }
}
fn sequence(replies: Vec<Vec<TestFrame>>) -> Sequence {
    sequence_with_status(replies.into_iter().map(|frames| (200, frames)).collect())
}
fn sequence_with_status(replies: Vec<(u16, Vec<TestFrame>)>) -> Sequence {
    let flags = (0..replies.len())
        .map(|_| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .collect();
    Sequence(std::sync::Mutex::new(replies), flags)
}

/// The cap counts the received UTF-8 bytes, not the decoded UTF-16 units
/// (node v22.22 real-TS probe, `_tmp/20261025-issue-647-probe/RESULT.md`).
/// This body is 24 units but 27 bytes, and the first `é` is split across two
/// frames: at a 24-byte cap it overflows and falls back, at exactly 27 it is
/// accepted without a second request.
#[tokio::test]
async fn complete_shape_probe_counts_received_bytes_not_utf16_units() {
    {
        let _limit = LimitGuard::set(24);
        let (request, config) = complete_config();
        let transport = sequence(vec![
            vec![
                TestFrame::Data(b"{\"choices\":[],\"n\":\"\xc3"),
                TestFrame::Data(b"\xa9\xc3\xa9\xc3\xa9\"}"),
            ],
            vec![TestFrame::Data(TEST_TINY_CHOICES)],
        ]);
        let result = complete_model(&transport, &request, &config, &StreamOptions::default())
            .await
            .expect("the over-byte-cap first candidate falls back");
        assert!(matches!(result, JsValue::Object(_)));
        assert!(
            transport.0.lock().unwrap().is_empty(),
            "both endpoints were requested"
        );
    }
    {
        let _limit = LimitGuard::set(27);
        let (request, config) = complete_config();
        let transport = sequence(vec![vec![
            TestFrame::Data(b"{\"choices\":[],\"n\":\"\xc3"),
            TestFrame::Data(b"\xa9\xc3\xa9\xc3\xa9\"}"),
        ]]);
        let result = complete_model(&transport, &request, &config, &StreamOptions::default())
            .await
            .expect("exactly the byte limit is still a valid string");
        assert!(matches!(result, JsValue::Object(_)));
        assert!(
            transport.0.lock().unwrap().is_empty(),
            "under-limit candidate must be accepted without fallback"
        );
    }
}

/// An over-limit first candidate may only fall back after the body reaches
/// EOF: both frames must be drained first.
#[tokio::test(start_paused = true)]
async fn complete_shape_probe_drains_overlong_first_body_before_fallback() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_config();
    let transport = sequence(vec![
        vec![
            TestFrame::After(Duration::from_millis(40), TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(40), b" "),
        ],
        vec![TestFrame::Data(TEST_TINY_CHOICES)],
    ]);
    let result = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect("second candidate accepted");
    assert!(matches!(result, JsValue::Object(_)));
    assert!(
        transport.0.lock().unwrap().is_empty(),
        "both endpoints were requested"
    );
    assert!(
        transport.eof(0),
        "fallback happened before the over-limit body reached EOF"
    );
}

/// The last candidate is still returned after the over-long clone read; its
/// later `response.text()` throws the real V8 plain `Error`, not
/// `TypeError: terminated` or `RangeError`.
#[tokio::test(start_paused = true)]
async fn complete_shape_probe_last_overlong_body_throws_v8_error_after_eof() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_config();
    let transport = sequence(vec![
        vec![TestFrame::Data(b"{}")],
        vec![
            TestFrame::After(Duration::from_millis(40), TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(40), b" "),
        ],
    ]);
    let error = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect_err("overlong last body must throw");
    assert!(error.name().equals_str("Error"), "{error:?}");
    assert_eq!(
        error.message_text(),
        crate::fetch_redirect::STRING_TOO_LONG,
        "real TS: Error, not RangeError/TypeError"
    );
    assert!(
        transport.0.lock().unwrap().is_empty(),
        "both endpoints were requested"
    );
    assert!(
        transport.eof(1),
        "error was raised before the body reached EOF"
    );
}

/// A first candidate whose over-long body finishes after the outer request
/// deadline: the next endpoint must not be requested; the timer is reported.
#[tokio::test(start_paused = true)]
async fn complete_shape_probe_overlong_body_past_deadline_reports_timeout() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_config_with_timeout(20.0);
    let transport = sequence(vec![
        vec![
            TestFrame::Data(TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(50), b" "),
        ],
        vec![TestFrame::Data(TEST_TINY_CHOICES)],
    ]);
    let error = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect_err("deadline must win");
    assert!(
        error
            .message_text()
            .contains("Model request timed out after 20ms."),
        "{error:?}"
    );
    assert_eq!(
        transport.0.lock().unwrap().len(),
        1,
        "the second endpoint must not be requested after the timer fired"
    );
}

/// A caller abort during the over-long body read also wins after EOF (the
/// clone read is detached from the parent signal, exactly like TS).
#[tokio::test(start_paused = true)]
async fn complete_shape_probe_overlong_body_parent_abort_wins_after_eof() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_config();
    let abort = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(abort.clone()),
        ..StreamOptions::default()
    };
    let started = tokio::time::Instant::now();
    let transport = sequence(vec![
        vec![
            TestFrame::Data(TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(50), b" "),
        ],
        vec![TestFrame::Data(TEST_TINY_CHOICES)],
    ]);
    let mut pending = Box::pin(complete_model(&transport, &request, &config, &options));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(30)).await;
    // Deliver the parent event with its original time while the detached
    // body read is still waiting for its 50ms frame.
    abort.cancel_at(started + Duration::from_millis(20));
    tokio::time::advance(Duration::from_millis(30)).await;
    let error = poll_ready(pending.as_mut())
        .await
        .expect_err("parent abort must win");
    assert!(error.name().equals_str("AbortError"), "{error:?}");
    assert_eq!(
        transport.0.lock().unwrap().len(),
        1,
        "the second endpoint must not be requested after the abort"
    );
}

/// Body read failures keep priority over a string overflow: the clone read
/// rejects with `TypeError: terminated`, so the last candidate's replayed
/// body reports that category rather than the V8 string error.
#[tokio::test]
async fn complete_shape_probe_body_error_beats_string_overflow_on_last() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_config();
    let transport = sequence(vec![
        vec![TestFrame::Data(b"{}")],
        vec![TestFrame::Data(TEST_OVERLONG_CHOICES), TestFrame::Error],
    ]);
    let error = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect_err("body failure must throw");
    assert!(
        error.name().equals_str("TypeError") && error.message_text() == "terminated",
        "{error:?}"
    );
    assert!(transport.0.lock().unwrap().is_empty());
}

/// At EOF an incomplete UTF-8 sequence becomes U+FFFD in the decoded text,
/// but the cap already counted its bytes when they were received: 3 bytes
/// (`é` plus a lone `\xc3`) overflow a 2-byte cap even though the decoded
/// text is only 2 UTF-16 units, and exactly 3 bytes stay valid.
#[tokio::test]
async fn shape_probe_counts_eof_trailing_bytes_against_string_limit() {
    {
        let _limit = LimitGuard::set(2);
        let mut body = replay_body(Bytes::from_static(b"\xc3\xa9\xc3"), None);
        assert!(matches!(
            read_shape_probe(&mut body, &StreamOptions::default()).await,
            ShapeProbe::StringTooLong
        ));
    }
    {
        let _limit = LimitGuard::set(3);
        let mut body = replay_body(Bytes::from_static(b"\xc3\xa9\xc3"), None);
        assert!(matches!(
            read_shape_probe(&mut body, &StreamOptions::default()).await,
            ShapeProbe::Complete { .. }
        ));
    }
}

/// The in-loop cap boundary is exact across frames: 16 UTF-16 units (the
/// limit) are accepted, while 17 units reject the candidate and fall back to
/// the next endpoint. The same probe must discard an over-limit frame
/// immediately, so a later body read error carries no retained prefix.
#[tokio::test]
async fn complete_shape_probe_cap_boundary_is_exact_across_frames() {
    let _limit = LimitGuard::set(16);

    let (request, config) = complete_config();
    let at_limit = sequence(vec![vec![
        TestFrame::Data(b"{\"choices\":["),
        TestFrame::Data(b"  ]}"),
    ]]);
    let accepted = complete_model(&at_limit, &request, &config, &StreamOptions::default())
        .await
        .expect("exactly the limit is still valid");
    assert!(matches!(accepted, JsValue::Object(_)));
    assert!(
        at_limit.0.lock().unwrap().is_empty(),
        "an exactly-at-limit first candidate must not fall back"
    );

    let (request, config) = complete_config();
    let over_limit = sequence(vec![
        vec![
            TestFrame::Data(b"{\"choices\":["),
            TestFrame::Data(b"   ]}"),
        ],
        vec![TestFrame::Data(TEST_TINY_CHOICES)],
    ]);
    let accepted = complete_model(&over_limit, &request, &config, &StreamOptions::default())
        .await
        .expect("limit + 1 must fall back to the next endpoint");
    assert!(matches!(accepted, JsValue::Object(_)));
    assert!(
        over_limit.0.lock().unwrap().is_empty(),
        "#818 cap boundary: limit + 1 must reject the first candidate and request the next endpoint"
    );

    let mut body = replay_body(
        Bytes::from_static(br#"{"choices":["x"]}"#),
        Some(BodyFailure::Terminated),
    );
    match read_shape_probe(&mut body, &StreamOptions::default()).await {
        ShapeProbe::ReadError { bytes } => assert!(
            bytes.is_empty(),
            "limit + 1 in the main loop must discard the prefix, kept {} bytes",
            bytes.len()
        ),
        _ => panic!("a body read error must win over the drained cap"),
    }
}

/// With no `LimitGuard` override the test executes the same default tail
/// expression the production build compiles. Pin V8's real contract literal
/// (Node 22.22 `buffer.constants.MAX_STRING_LENGTH`), not the constant against
/// itself: a mutation of the shared default to `usize::MAX` must fail here.
#[test]
fn shape_string_limit_defaults_to_v8_max() {
    assert_eq!(
        string_limit(),
        0x1fffffe8,
        "#818 string limit defaults to v8 max"
    );
}

/// Nested guards restore the previous override, and the outermost drop returns
/// to the shared production default rather than leaving an override behind.
#[test]
fn limit_guard_nests_and_drop_restores_the_shared_default() {
    assert_eq!(string_limit(), 0x1fffffe8);
    {
        let _outer = LimitGuard::set(4);
        assert_eq!(string_limit(), 4);
        {
            let _inner = LimitGuard::set(8);
            assert_eq!(string_limit(), 8);
        }
        assert_eq!(string_limit(), 4);
    }
    assert_eq!(string_limit(), 0x1fffffe8);
}

/// Same shared-tail contract for the SSE reader's own bound: pin V8's real
/// literal, not the constant against itself — a mutation of the shared
/// default tail (or the thread-local's fallback) must fail here (#642).
#[test]
fn sse_limit_defaults_to_v8_max() {
    assert_eq!(sse_limit(), 0x1fffffe8);
}

/// The SSE knob is independent of the accumulator knob: shrinking one must
/// leave the other at the shared default, and nested guards restore it.
#[test]
fn sse_limit_guard_nests_and_drop_restores_the_shared_default() {
    assert_eq!(sse_limit(), 0x1fffffe8);
    {
        let _outer = SseLimitGuard::set(64);
        let _string = LimitGuard::set(4);
        assert_eq!(sse_limit(), 64);
        assert_eq!(string_limit(), 4);
        {
            let _inner = SseLimitGuard::set(128);
            assert_eq!(sse_limit(), 128);
        }
        assert_eq!(sse_limit(), 64);
    }
    assert_eq!(sse_limit(), 0x1fffffe8);
    assert_eq!(string_limit(), 0x1fffffe8);
}

/// A panicking test body still restores the default: the guard's Drop runs
/// while unwinding, so the override cannot leak past the test.
#[test]
fn sse_limit_guard_restores_the_default_across_a_panic() {
    let result = std::panic::catch_unwind(|| {
        let _limit = SseLimitGuard::set(4);
        // The override is visible, so this literal assert must panic.
        assert_eq!(sse_limit(), 0x1fffffe8);
    });
    assert!(
        result.is_err(),
        "the override must make the inner assert fail"
    );
    assert_eq!(sse_limit(), 0x1fffffe8);
}

/// Exactly the SSE bound is still allowed at the real call site
/// (`SseBuffer::with_limit(sse_limit())`): an undelimited remainder of
/// exactly 64 units does not throw RangeError; the stream only ends at EOF
/// with the remainder's JSON SyntaxError. The 65-unit overflow case is
/// `undelimited_sse_buffer_over_the_v8_limit_throws_range_error`.
#[tokio::test]
async fn undelimited_sse_buffer_exactly_at_the_v8_limit_is_allowed() {
    let _limit = SseLimitGuard::set(64);
    let (request, config) = stream_config();
    // A valid event is consumed; then exactly 64 units without "\n\n".
    let transport = Scripted(std::sync::Mutex::new(vec![(
        200,
        vec![
            Some(b"data: {\"type\":\"message_start\"}\n\n".as_slice()),
            Some(b"data: ".as_slice()),
            Some(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".as_slice()),
        ],
    )]));
    let mut events = Vec::new();
    let mut sink = |output: StreamOutput| {
        if let StreamOutput::Event(event) = output {
            events.push(event);
        }
    };
    let error = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut sink,
    )
    .await
    .unwrap_err();
    assert!(error.name().equals_str("SyntaxError"), "{error:?}");
    assert_eq!(events.len(), 2, "request_started + message_start, no retry");
}

// ---------------------------------------------------------------------------
// Plain `response_text` (single-endpoint `complete()`, the non-2xx body read,
// and the streaming error response share it): V8 `MAX_STRING_LENGTH` bound.
//
// TS `Response.text()` reads the whole body first and only then converts the
// bytes to a string. An over-limit body therefore still reaches EOF, a
// mid-read body failure wins over the string error, and the EOF replacement
// character of a trailing incomplete UTF-8 sequence counts toward the cap.
// The conversion throws a plain `Error: Cannot create a string longer than
// 0x1fffffe8 characters`, never a `RangeError` (real TS probes
// `_tmp/20261025-issue-641/RESULT.md` and
// `_tmp/20261025-issue-610/probe-reader-string-limit-single.mts`).

/// A single-endpoint `complete()` body over the cap: read to EOF, then throw
/// the plain V8 `Error` — not `RangeError`, and not before the last frame.
#[tokio::test(start_paused = true)]
async fn complete_response_text_overlong_body_reads_to_eof_then_throws_plain_error() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_single_endpoint_config();
    let transport = sequence(vec![vec![
        TestFrame::After(Duration::from_millis(40), TEST_OVERLONG_CHOICES),
        TestFrame::After(Duration::from_millis(40), b" "),
    ]]);
    let error = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect_err("an over-limit body must fail the text conversion");
    assert!(error.name().equals_str("Error"), "{error:?}");
    assert_eq!(
        error.message_text(),
        crate::fetch_redirect::STRING_TOO_LONG,
        "#818 overlong body: real TS Error, not RangeError/TypeError"
    );
    assert!(
        transport.eof(0),
        "#818 overlong body reads to EOF before the error"
    );
}

/// The cap counts the received UTF-8 bytes, not the decoded UTF-16 units
/// (node v22.22 real-TS probe, `_tmp/20261025-issue-647-probe/RESULT.md`):
/// `中中中` is 9 bytes but 3 UTF-16 units, so a body past the byte cap with
/// units to spare still throws, while exactly the byte cap succeeds.
#[tokio::test]
async fn response_text_cap_counts_received_bytes_not_utf16_units() {
    {
        let _limit = LimitGuard::set(5);
        // "中中中": 9 bytes, 3 UTF-16 units.
        let mut body = replay_body(
            Bytes::from_static(b"\xe4\xb8\xad\xe4\xb8\xad\xe4\xb8\xad"),
            None,
        );
        let error = response_text(&mut body, &StreamOptions::default(), false)
            .await
            .expect_err("#818 byte cap: 9 received bytes must pass the 5-byte cap");
        assert!(
            error.name().equals_str("Error")
                && error.message_text() == crate::fetch_redirect::STRING_TOO_LONG,
            "{error:?}"
        );
    }
    {
        let _limit = LimitGuard::set(9);
        let mut body = replay_body(
            Bytes::from_static(b"\xe4\xb8\xad\xe4\xb8\xad\xe4\xb8\xad"),
            None,
        );
        let text = response_text(&mut body, &StreamOptions::default(), false)
            .await
            .expect("exactly the byte cap is still a valid string");
        assert_eq!(text, "中中中");
    }
    {
        // "中中" plus a lone \xc3: a trailing incomplete UTF-8 sequence only
        // becomes U+FFFD at EOF, but its bytes were counted when received:
        // 7 bytes over a 6-byte cap throw even though the decoded text is
        // only 3 UTF-16 units.
        let _limit = LimitGuard::set(6);
        let mut body = replay_body(Bytes::from_static(b"\xe4\xb8\xad\xe4\xb8\xad\xc3"), None);
        let error = response_text(&mut body, &StreamOptions::default(), false)
            .await
            .expect_err("the trailing incomplete sequence's bytes count");
        assert!(
            error.name().equals_str("Error")
                && error.message_text() == crate::fetch_redirect::STRING_TOO_LONG,
            "{error:?}"
        );
    }
    {
        let _limit = LimitGuard::set(7);
        let mut body = replay_body(Bytes::from_static(b"\xe4\xb8\xad\xe4\xb8\xad\xc3"), None);
        let text = response_text(&mut body, &StreamOptions::default(), false)
            .await
            .expect("exactly the byte cap is still a valid string");
        assert_eq!(text, "中中\u{fffd}");
    }
}

/// A body read failure keeps priority over the string conversion: the frame
/// that crosses the cap is followed by `TypeError: terminated`, which is what
/// `await response.text()` rejects with (same error object as the body).
#[tokio::test]
async fn response_text_body_error_beats_string_overflow() {
    let _limit = LimitGuard::set(4);
    let mut body = replay_body(
        Bytes::from_static(b"abcdefgh"),
        Some(BodyFailure::Terminated),
    );
    let error = response_text(&mut body, &StreamOptions::default(), false)
        .await
        .expect_err("the body failure must win");
    assert!(
        error.name().equals_str("TypeError") && error.message_text() == "terminated",
        "{error:?}"
    );
}

/// The non-2xx shared path in `complete()`: TS awaits the same
/// `response.text()` before any status-specific normalization, so an
/// over-long error body throws the plain V8 `Error`, not a provider error.
#[tokio::test(start_paused = true)]
async fn complete_non_2xx_overlong_error_body_throws_the_v8_error() {
    let _limit = LimitGuard::set(32);
    let (request, config) = complete_single_endpoint_config();
    let transport = sequence_with_status(vec![(
        500,
        vec![
            TestFrame::After(Duration::from_millis(40), TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(40), b" "),
        ],
    )]);
    let error = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect_err("an over-long error body must fail the text conversion");
    assert!(error.name().equals_str("Error"), "{error:?}");
    assert_eq!(error.message_text(), crate::fetch_redirect::STRING_TOO_LONG);
    assert!(
        transport.eof(0),
        "the error was raised before the error body reached EOF"
    );
}

/// The streaming error response path shares the same reader: a non-2xx
/// stream response with an over-long body throws the plain V8 `Error`
/// straight out of `streamModel`, before any retry or error-event
/// normalization.
#[tokio::test(start_paused = true)]
async fn stream_non_2xx_overlong_error_body_throws_the_v8_error() {
    let _limit = LimitGuard::set(32);
    let (request, config) = stream_config();
    let transport = sequence_with_status(vec![(
        500,
        vec![
            TestFrame::After(Duration::from_millis(40), TEST_OVERLONG_CHOICES),
            TestFrame::After(Duration::from_millis(40), b" "),
        ],
    )]);
    let mut outputs = Vec::new();
    let error = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut |output| outputs.push(output),
    )
    .await
    .expect_err("an over-long error body must fail the text conversion");
    assert!(error.name().equals_str("Error"), "{error:?}");
    assert_eq!(error.message_text(), crate::fetch_redirect::STRING_TOO_LONG);
    assert!(
        transport.eof(0),
        "the error was raised before the error body reached EOF"
    );
    assert_eq!(
        outputs.len(),
        1,
        "request_started only, no retry progress or error event"
    );
    assert!(matches!(&outputs[0], StreamOutput::Event(event)
        if field(event, "type") == &JsValue::from("request_started")));
}

// --- PILOTDECK_DUMP_REQUEST safe creation (A6.4-MODEL-DUMP-SECURE-001) ---
// Deterministic conflict/permission checks for `write_dump_exclusively`: no
// races, no touching any pre-existing temp entries — every path is created
// inside a throwaway per-test directory.

/// A private throwaway directory acquired exclusively for one test. The name
/// is a fresh UUID (a pid is *not* unique: it is reused after a process
/// dies), so nothing another user, process, or previous run left behind can
/// be under it; on collision (a UUID clash) the next name is tried instead of
/// deleting anything. Only a directory this guard actually created is
/// removed, and only while it still carries the owner token written at
/// acquisition, so drop-time cleanup can never delete a same-named directory
/// planted in between. `PDRS_DUMP_TEST_ROOT` redirects the parent root for
/// isolated verification; tests never probe or delete predictable names in
/// the shared temp directory.
struct DumpScratch {
    dir: std::path::PathBuf,
    token: String,
}
impl DumpScratch {
    fn acquire(tag: &str) -> Self {
        let root = std::env::var("PDRS_DUMP_TEST_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        for _ in 0..8 {
            let id = uuid::Uuid::new_v4();
            let dir = root.join(format!("pdrs-dump-test-{tag}-{id}"));
            // Exclusive create (no create_dir_all): a missing root fails
            // loudly instead of reaching into an unexpected tree.
            if std::fs::create_dir(&dir).is_ok() {
                let token = format!("owner:{id}\n");
                if std::fs::write(dir.join(".owner"), &token).is_err() {
                    let _ = std::fs::remove_dir_all(&dir);
                    panic!("dump scratch: cannot write owner token into {dir:?}");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                return Self { dir, token };
            }
            // Colliding with a pre-existing entry: try the next UUID. Never
            // remove the colliding entry — it is not ours.
        }
        panic!("dump scratch: could not exclusively create a test directory in {root:?}");
    }
}
impl Drop for DumpScratch {
    fn drop(&mut self) {
        // Remove only what this guard acquired, and only while the owner
        // token still matches (a directory without it was never ours).
        if let Ok(token) = std::fs::read_to_string(self.dir.join(".owner"))
            && token == self.token
        {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
impl std::ops::Deref for DumpScratch {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.dir
    }
}

fn fixed_names<'a>(names: &'a [&'a str]) -> impl FnMut() -> String + 'a {
    let mut index = 0;
    move || {
        let name = names[index].to_string();
        index += 1;
        name
    }
}

#[cfg(unix)]
#[test]
fn dump_creation_refuses_to_follow_a_pre_existing_symlink() {
    let dir = DumpScratch::acquire("symlink");
    // The victim stands in for another user's (or our own) sensitive data.
    let victim = dir.join("victim.json");
    std::fs::write(&victim, "SECRET-VICTIM-DATA").unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("taken.json")).unwrap();
    let path = write_dump_exclusively(
        &dir,
        "{\"dump\":true}",
        &mut fixed_names(&["taken.json", "free.json"]),
        8,
    )
    .expect("second candidate must be used");
    assert_eq!(path, dir.join("free.json"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"dump\":true}");
    // O_CREAT|O_EXCL refused the symlink: the victim was neither truncated
    // nor turned into the dump.
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "SECRET-VICTIM-DATA"
    );
}

#[test]
fn dump_creation_refuses_to_truncate_an_existing_file() {
    let dir = DumpScratch::acquire("existing");
    let taken = dir.join("taken.json");
    std::fs::write(&taken, "PRE-EXISTING").unwrap();
    let path = write_dump_exclusively(
        &dir,
        "{\"dump\":true}",
        &mut fixed_names(&["taken.json", "free.json"]),
        8,
    )
    .expect("second candidate must be used");
    assert_eq!(path, dir.join("free.json"));
    // A same-millisecond collision (TS `Date.now()` alone) truncates; the
    // exclusive create moves to the next name and leaves the first intact.
    assert_eq!(std::fs::read_to_string(&taken).unwrap(), "PRE-EXISTING");
}

#[test]
fn dump_creation_reports_failure_when_all_candidates_exist() {
    let dir = DumpScratch::acquire("exhausted");
    for name in ["taken1.json", "taken2.json"] {
        std::fs::write(dir.join(name), "PRE-EXISTING").unwrap();
    }
    assert!(
        write_dump_exclusively(
            &dir,
            "{\"dump\":true}",
            &mut fixed_names(&["taken1.json", "taken2.json"]),
            2,
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("taken1.json")).unwrap(),
        "PRE-EXISTING"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("taken2.json")).unwrap(),
        "PRE-EXISTING"
    );
}

#[test]
fn dump_files_are_owner_only() {
    let dir = DumpScratch::acquire("permissions");
    let path = write_dump_exclusively(&dir, "{}", &mut fixed_names(&["fresh.json"]), 8)
        .expect("fresh candidate must be used");
    let file = std::fs::File::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "dump must stay private to this user");
    }
}

// `Date.now()` alone (TS `streamModel.ts:199-200`) lets same-millisecond
// requests overwrite each other's dumps; the production name generator adds
// the pid and a process-wide monotonic sequence, which must hold however the
// callers interleave.
#[test]
fn dump_names_stay_unique_within_one_millisecond() {
    const NOW_MS: u128 = 1_758_000_000_000;
    const THREADS: usize = 32;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let (sender, receiver) = std::sync::mpsc::channel();
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let barrier = barrier.clone();
            let sender = sender.clone();
            std::thread::spawn(move || {
                barrier.wait();
                sender.send(next_dump_name(NOW_MS)).unwrap();
            })
        })
        .collect();
    drop(sender);
    let mut names: Vec<String> = receiver.iter().collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let pid = std::process::id();
    for name in &names {
        assert!(
            name.starts_with(&format!("pilotdeck_request_{NOW_MS}_{pid}_")),
            "name lost its timestamp/pid shape: {name}"
        );
        assert!(name.ends_with(".json"));
    }
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        THREADS,
        "same-millisecond names must stay distinct"
    );
}

// `DumpFailure::from_io` maps errnos through the Node v22 libuv table
// generated into `node_io`, not the platform's strerror text: darwin/glibc
// print "File name too long" for ENAMETOOLONG while Node prints libuv's
// "name too long", and ELOOP likewise drifts. The errno carries libuv's sign
// (uv_errno_t is the negated OS errno; Node's fs errors report ENOENT as -2
// — TS probe `_tmp/20261025-issue-621-fix-r2/ts-probe/`).
#[test]
fn dump_failure_maps_errnos_through_the_libuv_table() {
    for (raw, code, description) in [
        (libc::ENOENT, "ENOENT", "no such file or directory"),
        (libc::EACCES, "EACCES", "permission denied"),
        (libc::ENOTDIR, "ENOTDIR", "not a directory"),
        (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
        (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
        (libc::EFBIG, "EFBIG", "file too large"),
        (libc::EIO, "EIO", "i/o error"),
    ] {
        let failure = DumpFailure::from_io(
            &std::io::Error::from_raw_os_error(raw),
            "open",
            Some(std::path::Path::new("/tmp/pilotdeck_request_1.json")),
        );
        assert_eq!(failure.code, Some(code), "errno {raw}");
        assert_eq!(
            failure.description.as_deref(),
            Some(description),
            "errno {raw}"
        );
        assert_eq!(failure.errno, Some(-raw), "libuv-signed errno for {raw}");
        assert_eq!(
            failure.message(),
            format!("{code}: {description}, open '/tmp/pilotdeck_request_1.json'"),
            "errno {raw}"
        );
    }
    // A write-stage failure (open succeeded, write failed) has no path in
    // Node's error and no `open '<path>'` argument in the message (TS probe:
    // `EFBIG: file too large, write` with `raw:{errno,code,syscall}`).
    let write = DumpFailure::from_io(
        &std::io::Error::from_raw_os_error(libc::EFBIG),
        "write",
        None,
    );
    assert_eq!(write.errno, Some(-libc::EFBIG));
    assert_eq!(write.message(), "EFBIG: file too large, write");
}

// The SIGXFSZ registration must degrade, never panic, when there is no
// tokio runtime: the dump path is plain library code, and a library caller
// on a non-async thread keeps the default disposition (writes past
// RLIMIT_FSIZE then kill the process like plain Rust) instead of taking the
// whole process down at registration. The registration attempt itself is
// exercised (not the process-wide OnceLock, which other tests may already
// have initialized from inside their runtimes), on a fresh thread with no
// runtime context; tokio's `signal()` alone would panic there
// (`scheduler::Handle::current()`), hence the runtime probe in
// `register_sigxfsz`.
#[cfg(unix)]
#[test]
fn sigxfsz_registration_degrades_without_a_runtime() {
    let attempt = std::thread::spawn(crate::register_sigxfsz)
        .join()
        .expect("registration attempt must not panic without a runtime");
    assert!(
        attempt.is_err(),
        "no-runtime registration must degrade, got: {:?}",
        attempt.map(|_| ())
    );
}

pub(super) const RETRY_WORDS_ORACLE: &str =
    include_str!("../../../tests/fixtures/model-stream-retry-words/oracle.json");

pub(super) fn oracle_text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("oracle string")
        .to_utf8()
        .expect("oracle UTF-8")
}

/// Real TS `streamModel` goldens: every retry word and its case/substring/
/// name variants. `retryWord` records membership in the TS source list;
/// `retried` records what the running generator actually did (abort names
/// short-circuit `aborted`, so those two differ). The Rust decision function
/// must agree with both columns.
#[test]
fn real_ts_stream_retry_message_words() {
    let oracle = crate::test_baseline::oracle(RETRY_WORDS_ORACLE);
    let cases = super::super::count_body::array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 30, "fixture case count");
    let mut positives = 0;
    let mut words = 0;
    for case in cases {
        let error_name = oracle_text(field(case, "errorName"));
        let message = oracle_text(field(case, "message"));
        let lower = message.to_lowercase();
        assert_eq!(
            retryable_message(&lower),
            field(case, "retryWord").as_bool().expect("retryWord"),
            "retryable_message for {error_name}: {message:?}"
        );
        let error = ModelStreamError::Thrown {
            name: error_name.clone().into(),
            message: message.clone().into(),
        };
        let expected = field(case, "retried").as_bool().expect("retried");
        assert_eq!(
            is_retryable_stream_error(&error),
            expected,
            "is_retryable_stream_error for {error_name}: {message:?}"
        );
        if field(case, "name")
            .as_string()
            .is_some_and(|name| name.to_utf8().is_ok_and(|name| name.starts_with("word-")))
        {
            words += 1;
            assert!(
                field(case, "retryWord").as_bool().unwrap(),
                "word case must be a source-list member"
            );
        }
        if expected {
            positives += 1;
        }
    }
    assert_eq!(words, 8, "one golden per TS retry word");
    assert!(positives >= 10, "positive retry goldens");
}

/// Real TS `RouterRuntime` goldens for the canonical Thrown classification
/// used by the router's transient-retry layer (`isNetworkTransient` +
/// `classifyNetworkErrorCode`). This is a different TS list from streamModel's:
/// dns/etimedout/abort are retryable here, timeout/aborted are not.
#[test]
fn real_ts_router_transient_thrown_words_match_fallback() {
    let oracle = crate::test_baseline::oracle(RETRY_WORDS_ORACLE);
    let cases = super::super::count_body::array(field(&oracle, "routerCases"));
    assert_eq!(cases.len(), 30, "router case count");
    let request = parse_json(r#"{"provider":"test","model":"m","messages":[]}"#).unwrap();
    for case in cases {
        let error_name = oracle_text(field(case, "errorName"));
        let message = oracle_text(field(case, "message"));
        let canonical = super::super::fallback::canonical_error(
            ModelStreamError::Thrown {
                name: error_name.clone().into(),
                message: message.clone().into(),
            },
            &request,
            "openai".into(),
        );
        assert_eq!(
            oracle_text(field(&canonical, "code")),
            oracle_text(field(case, "code")),
            "canonical code for {error_name}: {message:?}"
        );
        assert_eq!(
            field(&canonical, "retryable").as_bool().expect("retryable"),
            field(case, "retryable").as_bool().expect("retryable"),
            "canonical retryable for {error_name}: {message:?}"
        );
    }
}
