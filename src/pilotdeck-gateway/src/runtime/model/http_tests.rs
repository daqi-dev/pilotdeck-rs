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

/// Reuse the original TS error cases without needing a listening socket.
/// This supplements (and does not replace) the real HTTP oracle replay.
#[tokio::test]
async fn google_sdk_errors_match_the_recorded_status_text_and_content_type() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/model-stream-google/oracle.json"
    ));
    let mut checked = 0;
    for case in super::super::count_body::array(field(&oracle, "cases")) {
        if ![
            "error-json-401",
            "error-custom-status-text",
            "error-content-type-case",
        ]
        .contains(&header_text(field(case, "name")).as_str())
        {
            continue;
        }
        let reply = &super::super::count_body::array(field(case, "replies"))[0];
        let mut response =
            Response::builder().status(field(reply, "status").as_number().unwrap() as u16);
        for (name, value) in field(reply, "headers").as_object().unwrap().entries() {
            response = response.header(name.to_utf8().unwrap(), header_text(value));
        }
        if let Some(reason) = field(reply, "statusText").as_string() {
            response = response
                .extension(hyper::ext::ReasonPhrase::try_from(reason.to_utf8().unwrap()).unwrap());
        }
        let bytes: Vec<u8> = super::super::count_body::array(field(reply, "chunks"))
            .iter()
            .flat_map(|chunk| header_text(field(chunk, "text")).into_bytes())
            .collect();
        let body = Full::new(Bytes::from(bytes))
            .map_err(|never| match never {})
            .boxed();
        let error = google_response_error(
            response.body((body, false)).unwrap(),
            &StreamOptions::default(),
            field(&oracle, "provider"),
        )
        .await
        .unwrap();
        let ModelStreamError::Provider(actual) = error else {
            panic!("expected canonical SDK error");
        };
        let timeline = super::super::count_body::array(field(case, "timeline"));
        let mut expected = field(field(timeline.last().unwrap(), "event"), "error")
            .as_object()
            .unwrap()
            .clone();
        // The outer retry loop adds this checkpoint after SDK normalization.
        // Its full shape remains covered by the loopback timeline replay.
        expected.insert("streamInterruption".into(), JsValue::Undefined);
        assert_eq!(
            crate::js_value::json_stringify(&JsValue::Object(actual)).unwrap(),
            crate::js_value::json_stringify(&JsValue::Object(expected)).unwrap(),
            "{}: SDK error",
            header_text(field(case, "name"))
        );
        checked += 1;
    }
    assert_eq!(checked, 3);
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

/// Internal cancellation constructs AbortError with a fixed message. Exercise
/// the production catch handler directly to cover a distinct SDK abort reason.
#[test]
fn google_abort_name_preserves_original_message() {
    let error = ModelStreamError::thrown("AbortError", "SDK cancelled this specific request");
    let error = throw_if_google_abort(error, &StreamOptions::default(), false).unwrap_err();
    assert!(
        matches!(error, ModelStreamError::Thrown { name, message }
        if name.equals_str("AbortError")
            && message.equals_str("SDK cancelled this specific request")),
        "the original AbortError name and message must survive"
    );
}

#[tokio::test]
async fn undelimited_sse_buffer_over_the_v8_limit_throws_range_error() {
    SSE_LIMIT.with(|limit| limit.set(64));
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
    SSE_LIMIT.with(|limit| limit.set(super::super::count_http::MAX_STRING_LENGTH));
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
fn imf_fixdate_ranges_follow_date_parse() {
    // node probe (r1/probe-r1.out.txt): 32 Oct / 00 / 25:00 / 07:60 /
    // 24:00:01 -> NaN; 31 Feb rolls over; 24:00:00 is next midnight; a
    // second >= 60 reads as :00.
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
fn imf_fixdate_year_edges_follow_date_parse() {
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
struct TimedHeaders(Duration);
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

async fn poll_ready<F: Future>(mut future: Pin<&mut F>) -> F::Output {
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

/// Google uses bare SDK fetch but must preserve the same transport event
/// sources as the generic provider, including every redirected hop.
#[tokio::test(start_paused = true)]
async fn google_exchange_orders_response_timeout_and_parent() {
    for (response_ms, parent_ms, sync, expected) in [
        (60, 80, false, "response"),
        (100, 120, false, "timeout"),
        (110, 90, false, "abort"),
        (60, 80, true, "abort"),
    ] {
        let provider = parse_json(r#"{"id":"p","protocol":"google","url":"http://example.test","apiKey":"k","headers":{}}"#).unwrap();
        let abort = AbortFlag::new();
        let options = StreamOptions {
            abort: Some(abort.clone()),
            stream_timeout_ms: Some(100.0),
            ..StreamOptions::default()
        };
        let started = tokio::time::Instant::now();
        let transport = TimedHeaders(Duration::from_millis(response_ms));
        let model = JsValue::from("m");
        let body = JsValue::Null;
        let mut pending = Box::pin(send_google_request(
            &transport, &provider, &model, &body, &options,
        ));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(130)).await;
        if sync {
            abort.abort();
        } else {
            abort.cancel_at(started + Duration::from_millis(parent_ms));
        }
        let result = poll_ready(pending.as_mut())
            .await
            .map_err(|(error, _)| error);
        match expected {
            "response" => assert!(result.is_ok(), "{result:?}"),
            "abort" => assert!(result.unwrap_err().name().equals_str("AbortError")),
            "timeout" => assert!(
                result
                    .unwrap_err()
                    .name()
                    .equals_str("StreamIdleTimeoutError")
            ),
            _ => unreachable!(),
        }
    }
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

#[tokio::test(start_paused = true)]
async fn google_redirect_retains_absolute_deadline_and_second_hop_source() {
    struct Redirect(std::sync::atomic::AtomicUsize);
    impl ModelHttpTransport for Redirect {
        fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
            let first = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            TransportFuture::after(
                Duration::from_millis(if first { 60 } else { 50 }),
                Ok(Response::builder()
                    .status(if first { 307 } else { 200 })
                    .header("location", "/second")
                    .body(
                        Full::new(Bytes::new())
                            .map_err(|never| match never {})
                            .boxed(),
                    )
                    .unwrap()),
            )
        }
    }
    let provider = parse_json(
        r#"{"id":"p","protocol":"google","url":"http://example.test","apiKey":"k","headers":{}}"#,
    )
    .unwrap();
    let options = StreamOptions {
        stream_timeout_ms: Some(100.0),
        ..StreamOptions::default()
    };
    let transport = Redirect(std::sync::atomic::AtomicUsize::new(0));
    let model = JsValue::from("m");
    let body = JsValue::Null;
    let mut pending = Box::pin(send_google_request(
        &transport, &provider, &model, &body, &options,
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(60)).await;
    poll_pending(pending.as_mut()).await;
    assert_eq!(transport.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(60)).await;
    assert!(
        poll_ready(pending.as_mut())
            .await
            .unwrap_err()
            .0
            .name()
            .equals_str("StreamIdleTimeoutError")
    );
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

const TEST_TINY_CHOICES: &[u8] = br#"{"choices":[]}"#;
/// 53 UTF-16 units, valid OpenAI shape, over every small test limit below.
const TEST_OVERLONG_CHOICES: &[u8] = br#"{"choices":[],"pad":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;

#[derive(Clone, Copy)]
enum TestFrame {
    Data(&'static [u8]),
    /// Sleep before yielding this frame; the timer starts on its first poll.
    After(Duration, &'static [u8]),
    /// undici `TypeError: terminated`.
    Error,
}

struct SequenceBody {
    frames: std::collections::VecDeque<TestFrame>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}
impl hyper::body::Body for SequenceBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        let Some(frame) = self.frames.front().copied() else {
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
struct Sequence(std::sync::Mutex<Vec<(u16, Vec<TestFrame>)>>);
impl ModelHttpTransport for Sequence {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        let (status, frames) = self.0.lock().unwrap().remove(0);
        let body = SequenceBody {
            frames: frames.into(),
            timer: None,
        }
        .boxed();
        TransportFuture::ready(Ok(Response::builder().status(status).body(body).unwrap()))
    }
}
fn sequence(replies: Vec<Vec<TestFrame>>) -> Sequence {
    Sequence(std::sync::Mutex::new(
        replies.into_iter().map(|frames| (200, frames)).collect(),
    ))
}

/// The cap counts UTF-16 units of the decoded text (not bytes). This body is
/// 24 units but 27 bytes, and the first `é` is split across two frames.
#[tokio::test]
async fn complete_shape_probe_counts_utf16_units_not_bytes() {
    let _limit = LimitGuard::set(24);
    let (request, config) = complete_config();
    let transport = sequence(vec![vec![
        TestFrame::Data(b"{\"choices\":[],\"n\":\"\xc3"),
        TestFrame::Data(b"\xa9\xc3\xa9\xc3\xa9\"}"),
    ]]);
    let result = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect("exactly the limit is still a valid string");
    assert!(matches!(result, JsValue::Object(_)));
    assert!(
        transport.0.lock().unwrap().is_empty(),
        "under-limit candidate must be accepted without fallback"
    );
}

/// An over-limit first candidate may only fall back after the body reaches
/// EOF: the body frames are 40ms apart and both must be drained first.
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
    let started = tokio::time::Instant::now();
    let result = complete_model(&transport, &request, &config, &StreamOptions::default())
        .await
        .expect("second candidate accepted");
    assert!(matches!(result, JsValue::Object(_)));
    assert!(
        transport.0.lock().unwrap().is_empty(),
        "both endpoints were requested"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(70),
        "fallback happened before the over-limit body reached EOF: {:?}",
        started.elapsed()
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
    let started = tokio::time::Instant::now();
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
        started.elapsed() >= Duration::from_millis(70),
        "error was raised before the body reached EOF: {:?}",
        started.elapsed()
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

/// At EOF an incomplete UTF-8 sequence becomes U+FFFD; that unit counts
/// toward V8's limit just like decoded text (TS TextDecoder stream mode).
#[tokio::test]
async fn shape_probe_counts_eof_replacement_against_string_limit() {
    {
        let _limit = LimitGuard::set(2);
        let mut body = replay_body(Bytes::from_static(b"{}\xc3"), None);
        assert!(matches!(
            read_shape_probe(&mut body, &StreamOptions::default()).await,
            ShapeProbe::StringTooLong
        ));
    }
    {
        let _limit = LimitGuard::set(3);
        let mut body = replay_body(Bytes::from_static(b"{}\xc3"), None);
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
        "limit + 1 must reject the first candidate and request the next endpoint"
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
    assert_eq!(string_limit(), 0x1fffffe8);
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

// The canonical Google error event for a failed dump write: `raw` is the fs
// Error's own enumerable fields (`normalize_model_error` keeps the `raw` it is
// handed), `code` falls through the classifiers to the fs `code`, and
// `retryable` is false — TS `isRetryable(undefined, "ENOENT")` is false for
// every errno code (probe: ENOENT/EACCES/ENOTDIR/EFBIG all false).
#[test]
fn dump_failure_error_projects_the_node_fs_error() {
    let provider = parse_json(r#"{"id":"dump-provider","protocol":"google"}"#).unwrap();
    let failure = DumpFailure::from_io(
        &std::io::Error::from_raw_os_error(libc::ENOENT),
        "open",
        Some(std::path::Path::new("/tmp/pilotdeck_request_1.json")),
    );
    let ModelStreamError::Provider(canonical) = dump_failure_error(&provider, &failure) else {
        panic!("a dump write failure is a provider error");
    };
    assert_eq!(
        canonical.get("code").and_then(JsValue::as_string),
        Some(&JsString::from("ENOENT"))
    );
    assert_eq!(canonical.get("retryable"), Some(&JsValue::Bool(false)));
    let raw = canonical.get("raw").expect("raw survives normalization");
    assert_eq!(
        raw.get("errno"),
        Some(&JsValue::Number(f64::from(-libc::ENOENT)))
    );
    assert_eq!(
        raw.get("code").and_then(JsValue::as_string),
        Some(&JsString::from("ENOENT"))
    );
    assert_eq!(
        raw.get("syscall").and_then(JsValue::as_string),
        Some(&JsString::from("open"))
    );
    assert_eq!(
        raw.get("path").and_then(JsValue::as_string),
        Some(&JsString::from("/tmp/pilotdeck_request_1.json"))
    );
}

// The dump directory resolves like Node's `os.tmpdir()` on unix, not like
// `std::env::temp_dir()`: TMPDIR, then TMP, then TEMP — empty values skip to
// the next variable like JS `||` — else `/tmp`, with exactly one trailing `/`
// dropped while the result stays longer than the root. Every expectation
// below was probed against node v22.22.0
// (`_tmp/20261025-issue-621-fix-r4/node-probe/`); an empty TMPDIR under
// `std::env::temp_dir()` answers `""`, which would drop a dump into the
// process's working directory.
#[test]
fn node_tmpdir_matches_os_tmpdir_resolution() {
    let dir = |tmpdir: Option<&str>, tmp: Option<&str>, temp: Option<&str>| {
        node_tmpdir_from(|key| match key {
            "TMPDIR" => tmpdir.map(str::to_string),
            "TMP" => tmp.map(str::to_string),
            "TEMP" => temp.map(str::to_string),
            _ => None,
        })
    };
    assert_eq!(dir(Some("/tmp/user"), None, None), "/tmp/user");
    // Empty values fall through, like JS `||`.
    assert_eq!(dir(Some(""), Some("/tmp/t"), None), "/tmp/t");
    assert_eq!(dir(None, Some(""), Some("/tmp/e")), "/tmp/e");
    assert_eq!(dir(None, None, Some("/tmp/e")), "/tmp/e");
    assert_eq!(dir(None, None, None), "/tmp");
    assert_eq!(dir(Some(""), Some(""), Some("")), "/tmp");
    // Exactly one trailing `/` goes away, but only past the root.
    assert_eq!(dir(Some("/tmp/x/"), None, None), "/tmp/x");
    assert_eq!(dir(Some("/tmp/x//"), None, None), "/tmp/x/");
    assert_eq!(dir(Some("///"), None, None), "//");
    assert_eq!(dir(Some("//"), None, None), "/");
    assert_eq!(dir(Some("/"), None, None), "/");
    // Relative values pass through unchanged, like node.
    assert_eq!(dir(Some("relative"), None, None), "relative");
}
