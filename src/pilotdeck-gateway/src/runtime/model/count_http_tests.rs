use super::*;
use crate::host::modules::budget::{EvaluationOptions, RequestBudgetEvaluator};
use crate::runtime::model::token_accounting::TokenAccountingRuntime;
use crate::test_baseline as baseline;
use std::sync::{Arc, Mutex};
#[path = "../../../tests/common/token_count_http.rs"]
mod fixture;

struct Scripted(Arc<Mutex<fixture::Exchange>>);
impl ProviderCountTransport for Scripted {
    fn post(&self, request: CountHttpRequest) -> CountFuture {
        fixture::guard(&request, &self.0);
        let exchange = self.0.clone();
        CountFuture::io(fetch_count(request, move |request| {
            let exchange = exchange.clone();
            async move {
                let origin = format!(
                    "{}://{}",
                    request.uri().scheme_str().unwrap(),
                    request.uri().authority().unwrap()
                );
                Ok(fixture::respond(request, &origin, &exchange).await)
            }
        }))
    }
}
#[tokio::test]
async fn real_ts_fetch_redirects_and_json_errors_with_scripted_wire() {
    let exchange = Arc::new(Mutex::new(fixture::Exchange::default()));
    fixture::replay(exchange.clone(), Arc::new(Scripted(exchange))).await;
}

// Receiving Location is enough to follow; a server may never finish the body
// of that response. Polling it would turn a valid redirect into a timeout.
enum ReplyBody {
    Redirect,
    Final(Full<Bytes>),
}
impl Body for ReplyBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Redirect => panic!("must not wait for redirect body"),
            Self::Final(body) => std::pin::Pin::new(body).poll_frame(cx),
        }
    }
}
#[tokio::test]
async fn redirect_drops_unfinished_body_before_following_location() {
    let mut count = 0;
    let value = fetch_count(
        CountHttpRequest {
            url: "http://127.0.0.1:41001/start".into(),
            headers: JsObject::new(),
            body: JsValue::Null,
        },
        move |_| {
            count += 1;
            std::future::ready(Ok(if count == 1 {
                Response::builder()
                    .status(307)
                    .header("location", "/final")
                    .body(ReplyBody::Redirect)
                    .unwrap()
            } else {
                assert_eq!(count, 2);
                Response::new(ReplyBody::Final(Full::new(Bytes::from_static(
                    b"{\"input_tokens\":73}",
                ))))
            }))
        },
    )
    .await
    .unwrap();
    assert_eq!(
        value.get("input_tokens").and_then(JsValue::as_number),
        Some(73.0)
    );
}

struct StalledBody(Arc<std::sync::atomic::AtomicBool>);
impl Body for StalledBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        std::task::Poll::Pending
    }
}

#[tokio::test(start_paused = true)]
async fn count_body_stall_uses_global_body_timeout() {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering},
        task::Poll,
    };
    let polled = Arc::new(AtomicBool::new(false));
    let mut request = Box::pin(fetch_count(
        CountHttpRequest {
            url: "http://example.test/count".into(),
            headers: JsObject::new(),
            body: JsValue::Null,
        },
        |_| std::future::ready(Ok(Response::new(StalledBody(polled.clone())))),
    ));
    // Serialization runs in the bounded CPU pool. Keep the root task runnable
    // until the real production body reader has armed its idle timer.
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        if polled.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
    tokio::time::advance(std::time::Duration::from_millis(599_999)).await;
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    let error = std::future::poll_fn(|cx| match request.as_mut().poll(cx) {
        Poll::Ready(result) => Poll::Ready(result.unwrap_err()),
        Poll::Pending => panic!("count body must terminate at 600000 ms"),
    })
    .await;
    assert_eq!(error.module_error.unwrap().message, "terminated".into());
}
