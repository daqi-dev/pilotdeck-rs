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
fn unported_protocols_fail_before_http() {
    let request = parse_json(r#"{"provider":"p","model":"m","messages":[]}"#).unwrap();
    let config = parse_json(
        r#"{"providers":{"p":{"id":"p","protocol":"openai","url":"http://x","apiKey":"k","headers":{},
        "models":{"m":{"id":"m","capabilities":{"supportsStreaming":true},"multimodal":{"input":["text"]}}}}}}"#,
    )
    .unwrap();
    let (provider, model) = providers::validate_model_request(&request, &config).unwrap();
    let error = providers::build_model_request(&request, &provider, &model).unwrap_err();
    assert!(
        matches!(error, ModelStreamError::Request { ref code, .. } if code.equals_str("protocol_not_ported"))
    );
}
