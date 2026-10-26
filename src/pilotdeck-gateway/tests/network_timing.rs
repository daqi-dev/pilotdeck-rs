//! Real TS timing oracle. Private helpers are compiled from their original files,
//! not copied or made public. This keeps product sources pristine for mutations.
#[path = "common/baseline.rs"]
mod baseline;
#[allow(dead_code)]
#[path = "../src/runtime/network.rs"]
pub(crate) mod network_source;
#[path = "common/network_timing_registry.rs"]
mod registry;
mod runtime {
    pub(crate) use super::network_source as network;
}
#[allow(dead_code)]
#[path = "../src/fetch_redirect.rs"]
mod fetch_redirect;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Frame};
use pilotdeck_gateway::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::{
        model::http::{
            BodyFailure, ModelBody, ModelHttpTransport, TransportFailure, TransportFuture,
        },
        tools::{
            ToolCancellation, ToolRunContext,
            builtin::web::{
                UrlFetcher,
                cache::UrlCache,
                fetcher::{FETCH_TIMEOUT_MS, FetchError, FetchResult},
                network::{FetchOptions, HttpRequest, WEB_RETRY, network_fetch},
            },
        },
    },
};
use pilotdeck_protocol::JsObject;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

#[path = "common/network_timing_background.rs"]
mod background_timing;
#[path = "common/network_timing_io.rs"]
mod io_timing;

const RAW: &str = include_str!("fixtures/network-timing/oracle.json");
const URL: &str = "https://fixture.test/start";

struct MatrixBody {
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    event: String,
    done: bool,
}
impl hyper::body::Body for MatrixBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        if self.event == "idle" {
            return Poll::Pending;
        }
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(timer) = &mut self.timer {
            std::task::ready!(timer.as_mut().poll(cx));
        }
        self.done = true;
        Poll::Ready(Some(if self.event == "rejected" {
            Err(BodyFailure::Terminated)
        } else {
            Ok(Frame::data(Bytes::from_static(b"FIRST")))
        }))
    }
}

#[derive(Clone)]
struct MatrixTransport {
    path: String,
    parent_first: bool,
    phase: String,
    event: String,
    requests: Arc<Mutex<Vec<String>>>,
}
enum Scripted {
    Pending,
    Ready(Result<Response<ModelBody>, TransportFailure>),
    Timer(Result<Response<ModelBody>, TransportFailure>),
}
impl MatrixTransport {
    fn scripted(&self, request: Request<Full<Bytes>>) -> Scripted {
        let target = request.uri().path().to_string();
        let attempt = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(target.clone());
            requests
                .iter()
                .filter(|path| path.as_str() == "/start")
                .count()
        };
        let retry = self.phase.starts_with("retry-");
        let redirect = target == "/start"
            && (self.phase == "redirect-hop"
                || (retry && attempt > 1 && self.path == "fetch_redirect"));
        let retry_failure = retry && attempt == 1;
        let body_phase = matches!(self.phase.as_str(), "body" | "text-body");
        let pending_headers = !retry && !redirect && !body_phase;
        if pending_headers && self.event == "idle" {
            return Scripted::Pending;
        }
        let result = if (pending_headers && self.event == "rejected")
            || (retry_failure && self.phase == "retry-error")
        {
            Err(TransportFailure {
                message: "fetch failed".into(),
                cause_code: None,
            })
        } else {
            let body = MatrixBody {
                timer: body_phase.then(|| Box::pin(tokio::time::sleep(Duration::from_millis(20)))),
                event: if redirect {
                    "idle".into()
                } else if body_phase {
                    self.event.clone()
                } else {
                    "fulfilled".into()
                },
                done: false,
            }
            .boxed();
            let mut response = Response::builder()
                .status(if redirect {
                    302
                } else if retry_failure {
                    503
                } else {
                    200
                })
                .header("content-type", "text/plain");
            if redirect {
                response = response.header("location", "/next");
            }
            Ok(response.body(body).unwrap())
        };
        if pending_headers {
            Scripted::Timer(result)
        } else {
            Scripted::Ready(result)
        }
    }
}

impl ModelHttpTransport for MatrixTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        match self.scripted(request) {
            Scripted::Pending => TransportFuture::io(std::future::pending()),
            Scripted::Ready(result) => TransportFuture::ready(result),
            Scripted::Timer(result) => TransportFuture::after(Duration::from_millis(20), result),
        }
    }
}

fn context(signal: ToolCancellation) -> ToolRunContext {
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: Some(Arc::new(signal)),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}

fn ok(content: String) -> Value {
    json!({"name":null,"message":null,"delivery":"ok","content":content})
}
fn error(name: Option<&str>, message: &str) -> Value {
    json!({"name":name,"message":message,"delivery":"throw","content":null})
}

async fn web(transport: impl ModelHttpTransport + 'static, context: &ToolRunContext) -> Value {
    let mut fetcher = UrlFetcher::new(Arc::new(transport));
    fetcher.cache = Arc::new(UrlCache::default());
    fetcher.random = Arc::new(|| 0.0);
    match fetcher.get_url_markdown_content(URL, Some(context)).await {
        Ok(FetchResult::Fetched { entry, .. }) => ok(entry.content),
        Ok(FetchResult::Redirect { .. }) => panic!("same-origin fixture must follow"),
        Err(FetchError::Other(message)) => error(None, &message.to_utf8().unwrap()),
        Err(FetchError::Http(error)) => panic!("unexpected status: {error:?}"),
    }
}

/// Only used for the retry rows: every nested transport promise is ready,
/// so this must not hide a timer or actual I/O inside `microtask`.
struct FollowingTransport(MatrixTransport);
impl ModelHttpTransport for FollowingTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let transport = self.0.clone();
        TransportFuture::microtask(async move {
            fetch_redirect::follow(
                request.uri().to_string().parse().unwrap(),
                hyper::Method::GET,
                hyper::HeaderMap::new(),
                Bytes::new(),
                || TransportFailure {
                    message: "fetch failed".into(),
                    cause_code: None,
                },
                |request| {
                    std::future::ready(match transport.scripted(request) {
                        Scripted::Ready(result) => result,
                        _ => panic!("retry follow must contain only queued continuations"),
                    })
                },
            )
            .await
        })
    }
}

/// Bare follow owns redirect traversal, not header cancellation. Its injected
/// transport obeys the measured Node registration order. The body dispatcher
/// below is the real helper's fixed policy and is never selected by the test.
async fn redirect(
    transport: MatrixTransport,
    context: &ToolRunContext,
    signal: &network_source::Cancellation,
) -> Value {
    if transport.phase.starts_with("retry-") {
        let request = HttpRequest {
            method: "GET",
            url: URL.into(),
            headers: vec![],
            body: None,
        };
        let options = FetchOptions {
            timeout_ms: 60000.0,
            parent: Some(context),
            retry: WEB_RETRY,
            random: Arc::new(|| 0.0),
        };
        return match network_fetch(&FollowingTransport(transport), &request, &options).await {
            Err(value) => error_value(&value),
            Ok(mut response) => match response.read_body(1024, Some(context)).await {
                Ok(bytes) => ok(String::from_utf8(bytes).unwrap()),
                Err(value) => panic!("immediate retry body: {value:?}"),
            },
        };
    }
    let continuation = Arc::new(AtomicBool::new(false));
    let response: Result<Response<ModelBody>, Value> = fetch_redirect::follow(
        URL.parse().unwrap(),
        hyper::Method::GET,
        hyper::HeaderMap::new(),
        Bytes::new(),
        || error(Some("TypeError"), "fetch failed"),
        |request| {
            let transport = transport.clone();
            let continuation = continuation.clone();
            async move {
                let source = match transport.scripted(request) {
                    Scripted::Pending => network_source::ExchangeFuture::io(std::future::pending()),
                    Scripted::Ready(result) => network_source::ExchangeFuture::ready(result),
                    Scripted::Timer(result) => {
                        network_source::ExchangeFuture::after(Duration::from_millis(20), result)
                    }
                };
                let deadline = Some(tokio::time::Instant::now() + Duration::from_millis(600000));
                let outcome = if transport.parent_first {
                    network_source::exchange_with_prearmed_parent(
                        source,
                        signal.cancelled(),
                        || signal.abort_event_at(),
                        deadline,
                    )
                    .await
                } else {
                    network_source::exchange(
                        source,
                        signal.cancelled(),
                        || signal.abort_event_at(),
                        deadline,
                    )
                    .await
                };
                match outcome {
                    network_source::Exchange::Response(Ok(response), before_abort) => {
                        continuation.store(before_abort, Ordering::SeqCst);
                        Ok(response)
                    }
                    network_source::Exchange::Response(Err(_), _)
                    | network_source::Exchange::Timeout { .. } => {
                        Err(error(Some("TypeError"), "fetch failed"))
                    }
                    network_source::Exchange::Aborted => {
                        Err(error(Some("AbortError"), "This operation was aborted"))
                    }
                }
            }
        },
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => return error,
    };
    let idle = Some(Duration::from_millis(600000));
    let continuation = continuation.load(Ordering::SeqCst);
    let body = if transport.phase == "text-body" {
        fetch_redirect::read_text_body_with_signal(
            response.into_body(),
            1024,
            idle,
            Some(signal),
            continuation,
        )
        .await
    } else {
        fetch_redirect::read_body_with_signal(
            response.into_body(),
            1024,
            idle,
            Some(signal),
            continuation,
        )
        .await
    };
    match body {
        Ok(bytes) => ok(String::from_utf8(bytes).unwrap()),
        Err(fetch_redirect::BodyError::Aborted) => {
            error(Some("AbortError"), "This operation was aborted")
        }
        Err(fetch_redirect::BodyError::Terminated) => error(Some("TypeError"), "terminated"),
        Err(fetch_redirect::BodyError::TooLong) => panic!("small body"),
    }
}

fn error_value(value: &pilotdeck_gateway::runtime::model::http::NetworkFetchError) -> Value {
    error(Some("NetworkFetchError"), &value.message.to_utf8().unwrap())
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Option<F::Output> {
    std::future::poll_fn(|cx| {
        Poll::Ready(match future.as_mut().poll(cx) {
            Poll::Ready(v) => Some(v),
            Poll::Pending => None,
        })
    })
    .await
}
async fn replay(row: &Value, overdue: bool) -> Value {
    if row["phase"] == "io-headers" {
        return io_timing::replay(row).await;
    }
    if row["path"] == "background" {
        return background_timing::replay(row).await;
    }
    let phase = row["phase"].as_str().unwrap();
    let event = row["event"].as_str().unwrap();
    let path = row["path"].as_str().unwrap();
    let order = row["order"].as_str().unwrap();
    let timing = &row["timing"];
    let operation_ms = timing["requested"]
        .as_u64()
        .expect("requested milliseconds");
    let effective_ms = timing["effective"]
        .as_u64()
        .expect("effective milliseconds");
    let gap = timing["parentGap"].as_i64().expect("parent gap");
    let parent_first = timing["parentRegisteredFirst"]
        .as_bool()
        .expect("registration order");
    assert!(operation_ms > 0 && effective_ms > 0 && effective_ms <= operation_ms);
    assert_eq!(effective_ms, if event == "idle" { 40 } else { 20 });
    // Drive time from the oracle, but reject drift against the tested contract.
    let expected_requested_ms = if phase.starts_with("retry-") {
        WEB_RETRY.base_delay_ms as u64
    } else if event == "idle" && path == "web_fetch" && phase != "body" {
        FETCH_TIMEOUT_MS as u64
    } else if event == "idle" {
        network_source::UNDICI_BODY_TIMEOUT.as_millis() as u64
    } else {
        effective_ms
    };
    assert_eq!(
        operation_ms, expected_requested_ms,
        "requested duration contract"
    );
    assert_eq!(
        timing["source"],
        if phase.starts_with("retry-") {
            "baseline retry timer"
        } else if event == "idle" && path == "web_fetch" && phase != "body" {
            "baseline hop timeout"
        } else if event == "idle" {
            "fixture-injected watchdog"
        } else {
            "fixture-injected completion"
        }
    );
    assert_eq!(
        gap,
        match order {
            "continuation-first" => 5,
            "parent-first" => -5,
            "tie-parent-first" | "tie-parent-last" => 0,
            _ => panic!("unknown order"),
        }
    );
    assert_eq!(parent_first, order != "tie-parent-last");
    assert_eq!(
        timing["target"],
        if phase.starts_with("retry-") {
            "retry"
        } else if event == "idle" {
            "watchdog"
        } else {
            "completion"
        }
    );
    let parent_ms = operation_ms
        .checked_add_signed(gap)
        .expect("logical parent deadline");
    effective_ms
        .checked_add_signed(gap)
        .expect("captured parent deadline");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MatrixTransport {
        path: path.into(),
        parent_first,
        phase: phase.into(),
        event: event.into(),
        requests: requests.clone(),
    };
    let signal = ToolCancellation::new();
    let context = context(signal.clone());
    let private_signal = network_source::Cancellation::new();
    let pending = async {
        if path == "web_fetch" {
            web(transport, &context).await
        } else {
            redirect(transport, &context, &private_signal).await
        }
    };
    let mut pending = Box::pin(pending);
    let start = tokio::time::Instant::now();
    let early = poll_once(pending.as_mut()).await;
    let mut result = early;
    if result.is_none() && !overdue && (gap > 0 || (gap == 0 && !parent_first)) {
        tokio::time::advance(Duration::from_millis(operation_ms)).await;
        result = poll_once(pending.as_mut()).await;
        if result.is_none() {
            tokio::time::advance(Duration::from_millis(parent_ms - operation_ms)).await;
        }
    } else if result.is_none() {
        // Both timers overdue: exercise dispatch's ordering, not Tokio select order.
        tokio::time::advance(Duration::from_millis(operation_ms + 1)).await;
    }
    if result.is_none() {
        let at = start + Duration::from_millis(parent_ms);
        signal.cancel_at(at);
        private_signal.cancel_at(at);
        result = poll_once(pending.as_mut()).await;
    }
    if result.is_none() {
        tokio::time::advance(Duration::from_millis(600001)).await;
        result = poll_once(pending.as_mut()).await;
    }
    let mut result = result.expect("bounded matrix must settle");
    let requests = requests.lock().unwrap();
    result["calls"] = json!(requests.len());
    result["retries"] = json!(
        requests
            .iter()
            .filter(|path| path.as_str() == "/start")
            .count()
            - 1
    );
    result
}

async fn check(id: &str) {
    check_mode(id, false).await;
}
async fn check_mode(id: &str, overdue: bool) {
    baseline::oracle(RAW);
    let oracle: Value = serde_json::from_str(RAW).unwrap();
    let rows = oracle["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 336);
    let row = rows.iter().find(|row| row["id"] == id).expect("matrix id");
    let actual = replay(row, overdue).await;
    println!("{}", json!({"id":id,"ts":row["observable"],"rust":actual}));
    assert_eq!(actual, row["observable"], "network timing cell: {id}");
}

// The table is the only maintained Rust observation registry. Source attributes
// must match it exactly; a difference resolving (or changing) makes this red.
#[tokio::test(start_paused = true)]
async fn known_differences_remain_exact_and_ignored() {
    baseline::oracle(RAW);
    let oracle: Value = serde_json::from_str(RAW).unwrap();
    for entry in registry::validate(&oracle) {
        let id = entry["id"].as_str().unwrap();
        let row = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        if !matches!(
            row["path"].as_str(),
            Some("web_fetch" | "fetch_redirect" | "background")
        ) {
            continue;
        }
        let real_io = row["phase"] == "io-headers";
        if real_io {
            tokio::time::resume();
        }
        let actual = replay(row, false).await;
        if real_io {
            tokio::time::pause();
        }
        assert_eq!(actual, entry["rust"], "known difference changed: {id}");
    }
}

#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn network_timing_regenerates_identically() {
    baseline::regenerates_identically("network-timing");
}

// Every cell has a separate test, so a known difference never skips its siblings.

#[tokio::test(start_paused = true)]
async fn web_fetch_headers_fulfilled_continuation_first() {
    check("web_fetch/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_fulfilled_parent_first() {
    check("web_fetch/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn web_fetch_headers_fulfilled_tie_parent_first() {
    check("web_fetch/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_headers_fulfilled_tie_parent_last() {
    check("web_fetch/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_rejected_continuation_first() {
    check("web_fetch/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_rejected_parent_first() {
    check("web_fetch/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn web_fetch_headers_rejected_tie_parent_first() {
    check("web_fetch/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_rejected_tie_parent_last() {
    check("web_fetch/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_idle_continuation_first() {
    check("web_fetch/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_idle_parent_first() {
    check("web_fetch/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_idle_tie_parent_first() {
    check("web_fetch/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_headers_idle_tie_parent_last() {
    check("web_fetch/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_body_fulfilled_continuation_first() {
    check("web_fetch/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn web_fetch_body_fulfilled_parent_first() {
    check("web_fetch/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn web_fetch_body_fulfilled_tie_parent_first() {
    check("web_fetch/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_body_fulfilled_tie_parent_last() {
    check("web_fetch/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_body_rejected_continuation_first() {
    check("web_fetch/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn web_fetch_body_rejected_parent_first() {
    check("web_fetch/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn web_fetch_body_rejected_tie_parent_first() {
    check("web_fetch/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_body_rejected_tie_parent_last() {
    check("web_fetch/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_body_idle_continuation_first() {
    check("web_fetch/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn web_fetch_body_idle_parent_first() {
    check("web_fetch/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_body_idle_tie_parent_first() {
    check("web_fetch/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_body_idle_tie_parent_last() {
    check("web_fetch/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_redirect_hop_fulfilled_continuation_first() {
    check("web_fetch/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_fulfilled_parent_first() {
    check("web_fetch/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn web_fetch_redirect_hop_fulfilled_tie_parent_first() {
    check("web_fetch/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_redirect_hop_fulfilled_tie_parent_last() {
    check("web_fetch/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_rejected_continuation_first() {
    check("web_fetch/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_rejected_parent_first() {
    check("web_fetch/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn web_fetch_redirect_hop_rejected_tie_parent_first() {
    check("web_fetch/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_rejected_tie_parent_last() {
    check("web_fetch/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_idle_continuation_first() {
    check("web_fetch/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_idle_parent_first() {
    check("web_fetch/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_idle_tie_parent_first() {
    check("web_fetch/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_redirect_hop_idle_tie_parent_last() {
    check("web_fetch/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_status_fulfilled_continuation_first() {
    check("web_fetch/retry-status/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_retry_status_fulfilled_parent_first() {
    check("web_fetch/retry-status/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn web_fetch_retry_status_fulfilled_tie_parent_first() {
    check("web_fetch/retry-status/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_status_fulfilled_tie_parent_last() {
    check("web_fetch/retry-status/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_error_fulfilled_continuation_first() {
    check("web_fetch/retry-error/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_retry_error_fulfilled_parent_first() {
    check("web_fetch/retry-error/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn web_fetch_retry_error_fulfilled_tie_parent_first() {
    check("web_fetch/retry-error/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_error_fulfilled_tie_parent_last() {
    check("web_fetch/retry-error/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_fulfilled_continuation_first() {
    check("fetch_redirect/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_fulfilled_parent_first() {
    check("fetch_redirect/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_fulfilled_tie_parent_first() {
    check("fetch_redirect/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_fulfilled_tie_parent_last() {
    check("fetch_redirect/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_rejected_continuation_first() {
    check("fetch_redirect/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_rejected_parent_first() {
    check("fetch_redirect/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_rejected_tie_parent_first() {
    check("fetch_redirect/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_rejected_tie_parent_last() {
    check("fetch_redirect/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_idle_continuation_first() {
    check("fetch_redirect/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_idle_parent_first() {
    check("fetch_redirect/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_idle_tie_parent_first() {
    check("fetch_redirect/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_idle_tie_parent_last() {
    check("fetch_redirect/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_fulfilled_continuation_first() {
    check("fetch_redirect/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_fulfilled_parent_first() {
    check("fetch_redirect/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_fulfilled_tie_parent_first() {
    check("fetch_redirect/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_fulfilled_tie_parent_last() {
    check("fetch_redirect/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_rejected_continuation_first() {
    check("fetch_redirect/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_rejected_parent_first() {
    check("fetch_redirect/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_rejected_tie_parent_first() {
    check("fetch_redirect/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_rejected_tie_parent_last() {
    check("fetch_redirect/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_idle_continuation_first() {
    check("fetch_redirect/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_idle_parent_first() {
    check("fetch_redirect/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn fetch_redirect_body_idle_tie_parent_first() {
    check("fetch_redirect/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_body_idle_tie_parent_last() {
    check("fetch_redirect/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_fulfilled_continuation_first() {
    check("fetch_redirect/text-body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_fulfilled_parent_first() {
    check("fetch_redirect/text-body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_fulfilled_tie_parent_first() {
    check("fetch_redirect/text-body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_fulfilled_tie_parent_last() {
    check("fetch_redirect/text-body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_rejected_continuation_first() {
    check("fetch_redirect/text-body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_rejected_parent_first() {
    check("fetch_redirect/text-body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_rejected_tie_parent_first() {
    check("fetch_redirect/text-body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_rejected_tie_parent_last() {
    check("fetch_redirect/text-body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_idle_continuation_first() {
    check("fetch_redirect/text-body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_idle_parent_first() {
    check("fetch_redirect/text-body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn fetch_redirect_text_body_idle_tie_parent_first() {
    check("fetch_redirect/text-body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_text_body_idle_tie_parent_last() {
    check("fetch_redirect/text-body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_fulfilled_continuation_first() {
    check("fetch_redirect/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_fulfilled_parent_first() {
    check("fetch_redirect/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_fulfilled_tie_parent_first() {
    check("fetch_redirect/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_fulfilled_tie_parent_last() {
    check("fetch_redirect/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_rejected_continuation_first() {
    check("fetch_redirect/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_rejected_parent_first() {
    check("fetch_redirect/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_rejected_tie_parent_first() {
    check("fetch_redirect/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_rejected_tie_parent_last() {
    check("fetch_redirect/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_idle_continuation_first() {
    check("fetch_redirect/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_idle_parent_first() {
    check("fetch_redirect/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_idle_tie_parent_first() {
    check("fetch_redirect/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_redirect_hop_idle_tie_parent_last() {
    check("fetch_redirect/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_status_fulfilled_continuation_first() {
    check("fetch_redirect/retry-status/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_status_fulfilled_parent_first() {
    check("fetch_redirect/retry-status/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn fetch_redirect_retry_status_fulfilled_tie_parent_first() {
    check("fetch_redirect/retry-status/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_status_fulfilled_tie_parent_last() {
    check("fetch_redirect/retry-status/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_error_fulfilled_continuation_first() {
    check("fetch_redirect/retry-error/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_error_fulfilled_parent_first() {
    check("fetch_redirect/retry-error/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn fetch_redirect_retry_error_fulfilled_tie_parent_first() {
    check("fetch_redirect/retry-error/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_error_fulfilled_tie_parent_last() {
    check("fetch_redirect/retry-error/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_headers_rejected_tie_parent_last_overdue() {
    check_mode("fetch_redirect/headers/rejected/tie-parent-last", true).await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_headers_fulfilled_tie_parent_last_overdue() {
    check_mode("web_fetch/headers/fulfilled/tie-parent-last", true).await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_status_fulfilled_tie_parent_last_overdue() {
    check_mode("web_fetch/retry-status/fulfilled/tie-parent-last", true).await;
}

#[tokio::test(start_paused = true)]
async fn web_fetch_retry_error_fulfilled_tie_parent_last_overdue() {
    check_mode("web_fetch/retry-error/fulfilled/tie-parent-last", true).await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_status_fulfilled_tie_parent_last_overdue() {
    check_mode(
        "fetch_redirect/retry-status/fulfilled/tie-parent-last",
        true,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn fetch_redirect_retry_error_fulfilled_tie_parent_last_overdue() {
    check_mode("fetch_redirect/retry-error/fulfilled/tie-parent-last", true).await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_fulfilled_continuation_first() {
    check("background/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_fulfilled_parent_first() {
    check("background/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_fulfilled_tie_parent_first() {
    check("background/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_fulfilled_tie_parent_last() {
    check("background/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_rejected_continuation_first() {
    check("background/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_rejected_parent_first() {
    check("background/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_rejected_tie_parent_first() {
    check("background/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_rejected_tie_parent_last() {
    check("background/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_idle_continuation_first() {
    check("background/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_idle_parent_first() {
    check("background/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_idle_tie_parent_first() {
    check("background/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_headers_idle_tie_parent_last() {
    check("background/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_fulfilled_continuation_first() {
    check("background/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_fulfilled_parent_first() {
    check("background/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_fulfilled_tie_parent_first() {
    check("background/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_fulfilled_tie_parent_last() {
    check("background/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_rejected_continuation_first() {
    check("background/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_rejected_parent_first() {
    check("background/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_rejected_tie_parent_first() {
    check("background/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_rejected_tie_parent_last() {
    check("background/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_idle_continuation_first() {
    check("background/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_idle_parent_first() {
    check("background/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn background_body_idle_tie_parent_first() {
    check("background/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_body_idle_tie_parent_last() {
    check("background/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_fulfilled_continuation_first() {
    check("background/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_fulfilled_parent_first() {
    check("background/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_fulfilled_tie_parent_first() {
    check("background/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_fulfilled_tie_parent_last() {
    check("background/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_rejected_continuation_first() {
    check("background/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_rejected_parent_first() {
    check("background/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_rejected_tie_parent_first() {
    check("background/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_rejected_tie_parent_last() {
    check("background/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_idle_continuation_first() {
    check("background/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_idle_parent_first() {
    check("background/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_idle_tie_parent_first() {
    check("background/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_redirect_hop_idle_tie_parent_last() {
    check("background/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_status_fulfilled_continuation_first() {
    check("background/retry-status/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_status_fulfilled_parent_first() {
    check("background/retry-status/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_status_fulfilled_tie_parent_first() {
    check("background/retry-status/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_status_fulfilled_tie_parent_last() {
    check("background/retry-status/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_error_fulfilled_continuation_first() {
    check("background/retry-error/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_error_fulfilled_parent_first() {
    check("background/retry-error/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_error_fulfilled_tie_parent_first() {
    check("background/retry-error/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn background_retry_error_fulfilled_tie_parent_last() {
    check("background/retry-error/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn background_lifecycle_pending_stop_ordered() {
    check("background/lifecycle/pending-stop/ordered").await;
}

#[tokio::test(start_paused = true)]
async fn background_lifecycle_settled_fulfilled_ordered() {
    check("background/lifecycle/settled-fulfilled/ordered").await;
}

#[tokio::test(start_paused = true)]
async fn background_lifecycle_settled_rejected_ordered() {
    check("background/lifecycle/settled-rejected/ordered").await;
}

#[tokio::test(start_paused = true)]
async fn background_lifecycle_running_uncooperative_ordered() {
    check("background/lifecycle/running-uncooperative/ordered").await;
}

#[tokio::test]
async fn web_fetch_io_headers_fulfilled_continuation_first() {
    check("web_fetch/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_fulfilled_parent_first() {
    check("web_fetch/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_fulfilled_tie_parent_first() {
    check("web_fetch/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_fulfilled_tie_parent_last() {
    check("web_fetch/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_rejected_continuation_first() {
    check("web_fetch/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_rejected_parent_first() {
    check("web_fetch/io-headers/rejected/parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_rejected_tie_parent_first() {
    check("web_fetch/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_rejected_tie_parent_last() {
    check("web_fetch/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_idle_continuation_first() {
    check("web_fetch/io-headers/idle/continuation-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_idle_parent_first() {
    check("web_fetch/io-headers/idle/parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_idle_tie_parent_first() {
    check("web_fetch/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
#[ignore = "#456 差异：web FetchError::Other 未保留 TS 错误类型名"]
async fn web_fetch_io_headers_idle_tie_parent_last() {
    check("web_fetch/io-headers/idle/tie-parent-last").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_fulfilled_continuation_first() {
    check("fetch_redirect/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_fulfilled_parent_first() {
    check("fetch_redirect/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_fulfilled_tie_parent_first() {
    check("fetch_redirect/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_fulfilled_tie_parent_last() {
    check("fetch_redirect/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_rejected_continuation_first() {
    check("fetch_redirect/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_rejected_parent_first() {
    check("fetch_redirect/io-headers/rejected/parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_rejected_tie_parent_first() {
    check("fetch_redirect/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_rejected_tie_parent_last() {
    check("fetch_redirect/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_idle_continuation_first() {
    check("fetch_redirect/io-headers/idle/continuation-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_idle_parent_first() {
    check("fetch_redirect/io-headers/idle/parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_idle_tie_parent_first() {
    check("fetch_redirect/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
async fn fetch_redirect_io_headers_idle_tie_parent_last() {
    check("fetch_redirect/io-headers/idle/tie-parent-last").await;
}

#[tokio::test]
async fn background_io_headers_fulfilled_continuation_first() {
    check("background/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
async fn background_io_headers_fulfilled_parent_first() {
    check("background/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
async fn background_io_headers_fulfilled_tie_parent_first() {
    check("background/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
async fn background_io_headers_fulfilled_tie_parent_last() {
    check("background/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
async fn background_io_headers_rejected_continuation_first() {
    check("background/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
async fn background_io_headers_rejected_parent_first() {
    check("background/io-headers/rejected/parent-first").await;
}

#[tokio::test]
async fn background_io_headers_rejected_tie_parent_first() {
    check("background/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
async fn background_io_headers_rejected_tie_parent_last() {
    check("background/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
async fn background_io_headers_idle_continuation_first() {
    check("background/io-headers/idle/continuation-first").await;
}

#[tokio::test]
async fn background_io_headers_idle_parent_first() {
    check("background/io-headers/idle/parent-first").await;
}

#[tokio::test]
async fn background_io_headers_idle_tie_parent_first() {
    check("background/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
async fn background_io_headers_idle_tie_parent_last() {
    check("background/io-headers/idle/tie-parent-last").await;
}

#[test]
fn registry_negative_controls_reject_unwired_or_relabelled_cells() {
    registry::negative_controls(&serde_json::from_str(RAW).unwrap());
}
