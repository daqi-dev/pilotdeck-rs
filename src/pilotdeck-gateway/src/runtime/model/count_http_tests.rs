use super::*;
use crate::host::modules::budget::{EvaluationOptions, RequestBudgetEvaluator};
use crate::runtime::model::token_accounting::TokenAccountingRuntime;
use crate::test_baseline as baseline;
use std::sync::{Arc, Mutex};
#[path = "../../../tests/common/token_count_http.rs"]
mod fixture;

struct Scripted(Arc<Mutex<fixture::Exchange>>);
impl ProviderCountTransport for Scripted {
    fn post(&self, request: CountHttpRequest) -> HostFuture<JsValue> {
        fixture::guard(&request, &self.0);
        let exchange = self.0.clone();
        Box::pin(fetch_count(request, move |request| {
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
