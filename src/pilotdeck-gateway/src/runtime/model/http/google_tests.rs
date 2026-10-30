use super::google::{send_google_request, throw_if_google_abort};
use super::tests::{TimedHeaders, poll_pending, poll_ready};
use super::*;

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
