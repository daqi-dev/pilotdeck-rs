use super::*;

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
        Box::pin(async move {
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
    assert_eq!(checked, 26);
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
            response.body(body).unwrap(),
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
        Box::pin(async move { Ok(Response::builder().status(status).body(stream).unwrap()) })
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
fn stream_config() -> (JsValue, JsValue) {
    let request = parse_json(r#"{"provider":"p","model":"m","messages":[]}"#).unwrap();
    let config = parse_json(
        r#"{"providers":{"p":{"id":"p","protocol":"anthropic","url":"http://127.0.0.1:8080/v1","apiKey":"k","headers":{},
        "retry":{"baseDelayMs":1,"maxDelayMs":1},
        "models":{"m":{"id":"m","capabilities":{"supportsStreaming":true,"maxOutputTokens":8},"multimodal":{"input":["text"]}}}}}}"#,
    )
    .unwrap();
    (request, config)
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
    STRING_LIMIT.with(|cell| cell.set(limit));
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
    STRING_LIMIT.with(|cell| cell.set(super::super::count_http::MAX_STRING_LENGTH));
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
    STRING_LIMIT.with(|cell| cell.set(3));
    let mut buffer = vec![1, 2];
    assert!(append_js_string(&mut buffer, &[3]).is_ok());
    assert!(append_js_string(&mut buffer, &[4]).is_err());
    assert_eq!(buffer, [1, 2, 3], "unchanged after the throw");
    STRING_LIMIT.with(|cell| cell.set(super::super::count_http::MAX_STRING_LENGTH));
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
    let previous = STRING_LIMIT.with(std::cell::Cell::get);
    STRING_LIMIT.with(|limit| limit.set(4));
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
    STRING_LIMIT.with(|limit| limit.set(previous));
}
