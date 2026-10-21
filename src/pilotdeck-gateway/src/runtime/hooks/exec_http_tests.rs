use super::*;
use crate::runtime::network::ExchangeFuture;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Response,
    body::{Body, Frame},
};
use std::{convert::Infallible, task::Poll};

async fn poll_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}
async fn poll_ready<F: Future>(mut future: Pin<&mut F>) -> F::Output {
    std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Ready(result) => Poll::Ready(result),
        Poll::Pending => panic!("must settle without paused-clock auto-advance"),
    })
    .await
}
fn options(signal: &Cancellation) -> HttpHookOptions {
    HttpHookOptions {
        url: "http://example.test/".into(),
        headers: None,
        allowed_env_vars: None,
        hook_input: JsObject::new(),
        env: None,
        signal: Some(signal.clone()),
    }
}
fn response() -> Response<Full<Bytes>> {
    Response::new(Full::new(Bytes::from_static(b"ok")))
}

#[tokio::test(start_paused = true)]
async fn hook_headers_order_response_timeout_and_parent() {
    for (response_ms, parent_ms, sync, expected) in [
        (60, 80, false, "ok"),
        (100, 120, false, "fetch failed"),
        (110, 90, false, "This operation was aborted"),
        (60, 80, true, "This operation was aborted"),
    ] {
        let signal = Cancellation::new();
        let started = tokio::time::Instant::now();
        let executor = HttpHookExecutor::with_timeout(Duration::from_millis(100));
        let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
            ExchangeFuture::after(Duration::from_millis(response_ms), Ok(response()))
        }));
        poll_pending(pending.as_mut()).await;
        tokio::time::advance(Duration::from_millis(130)).await;
        if sync {
            signal.abort();
        } else {
            signal.cancel_at(started + Duration::from_millis(parent_ms));
        }
        let result = poll_ready(pending.as_mut()).await;
        if expected == "ok" {
            assert_eq!(result.unwrap().stdout, JsString::from("ok"));
        } else {
            assert_eq!(result.unwrap_err(), JsString::from(expected));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn hook_exchange_preserves_timer_sources_and_fulfilled_promises() {
    let signal = Cancellation::new();
    let started = tokio::time::Instant::now();
    let mut pending = Box::pin(http_exchange(
        ExchangeFuture::after(Duration::from_millis(60), "ok"),
        Some(&signal),
        Some(started + Duration::from_millis(100)),
    ));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(130)).await;
    signal.cancel_at(started + Duration::from_millis(80));
    assert_eq!(poll_ready(pending.as_mut()).await.unwrap().0, "ok");
    let signal = Cancellation::new();
    signal.abort();
    assert_eq!(
        http_exchange(ExchangeFuture::ready("fulfilled"), Some(&signal), None)
            .await
            .unwrap()
            .0,
        "fulfilled"
    );
}

struct DelayedBody(Pin<Box<Sleep>>);
impl Body for DelayedBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        std::task::ready!(self.get_mut().0.as_mut().poll(cx));
        Poll::Ready(None)
    }
}
struct StalledBody;
impl Body for StalledBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Pending
    }
}

#[tokio::test(start_paused = true)]
async fn hook_body_sync_abort_beats_overdue_body_completion() {
    let signal = Cancellation::new();
    let executor = HttpHookExecutor::new();
    let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
        ExchangeFuture::ready(Ok(Response::new(DelayedBody(Box::pin(
            tokio::time::sleep(Duration::from_millis(60)),
        )))))
    }));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(80)).await;
    signal.abort();
    assert_eq!(
        poll_ready(pending.as_mut()).await.unwrap_err(),
        JsString::from("This operation was aborted")
    );
}

#[tokio::test(start_paused = true)]
async fn hook_body_stall_uses_global_body_timeout() {
    let signal = Cancellation::new();
    let executor = HttpHookExecutor::new();
    let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
        ExchangeFuture::ready(Ok(Response::new(StalledBody)))
    }));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(599_999)).await;
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        poll_ready(pending.as_mut()).await.unwrap_err(),
        JsString::from("terminated")
    );
}

/// Dispatch each redirect via the same shared arbiter. A 307 body is dropped
/// unread, while the second hop keeps its independent headers deadline/source.
#[tokio::test(start_paused = true)]
async fn hook_redirect_keeps_second_hop_timer_and_abort_order() {
    let signal = Cancellation::new();
    let executor = HttpHookExecutor::with_timeout(Duration::from_millis(100));
    let mut calls = 0;
    let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
        calls += 1;
        if calls == 1 {
            ExchangeFuture::ready(Ok(Response::builder()
                .status(307)
                .header("location", "/next")
                .body(StalledBody.boxed())
                .unwrap()))
        } else {
            ExchangeFuture::after(
                Duration::from_millis(60),
                Ok(Response::new(Full::new(Bytes::new()).boxed())),
            )
        }
    }));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(80)).await;
    signal.abort();
    assert_eq!(
        poll_ready(pending.as_mut()).await.unwrap_err(),
        JsString::from("This operation was aborted")
    );
    drop(pending);
    assert_eq!(calls, 2);
}

#[tokio::test(start_paused = true)]
async fn hook_winning_headers_cannot_hide_a_stalled_body_after_abort() {
    let signal = Cancellation::new();
    let executor = HttpHookExecutor::new();
    let started = tokio::time::Instant::now();
    let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
        ExchangeFuture::after(Duration::from_millis(60), Ok(Response::new(StalledBody)))
    }));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(100)).await;
    signal.cancel_at(started + Duration::from_millis(80));
    assert_eq!(
        poll_ready(pending.as_mut()).await.unwrap_err(),
        JsString::from("This operation was aborted")
    );
}

#[tokio::test(start_paused = true)]
async fn hook_body_timeout_precedes_later_parent_event() {
    let signal = Cancellation::new();
    let executor = HttpHookExecutor::new();
    let started = tokio::time::Instant::now();
    let mut pending = Box::pin(executor.execute_with(options(&signal), |_| {
        ExchangeFuture::ready(Ok(Response::new(StalledBody)))
    }));
    poll_pending(pending.as_mut()).await;
    tokio::time::advance(Duration::from_millis(700_000)).await;
    signal.cancel_at(started + Duration::from_millis(650_000));
    assert_eq!(
        poll_ready(pending.as_mut()).await.unwrap_err(),
        JsString::from("terminated")
    );
}
