//! Test-only stimuli shared by the two private caller replays. No product policy.
#![allow(dead_code)]
use crate::runtime::{
    model::http::{BodyFailure, ModelBody},
    network::{Cancellation, ExchangeFuture},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Frame};
use pilotdeck_protocol::JsString;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Wake, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Notify, watch},
    time::Instant,
};

pub const RAW: &str = include_str!("../fixtures/network-timing/oracle.json");
pub const URL: &str = "https://fixture.test/start";
pub struct MatrixBody {
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
pub enum Scripted {
    Pending,
    Ready(Result<Response<ModelBody>, JsString>),
    Timer(Result<Response<ModelBody>, JsString>),
}
impl Scripted {
    pub fn exchange<E: Send + 'static>(
        self,
        error: fn(JsString) -> E,
    ) -> ExchangeFuture<Result<Response<ModelBody>, E>> {
        match self {
            Self::Pending => ExchangeFuture::io(std::future::pending()),
            Self::Ready(result) => ExchangeFuture::ready(result.map_err(error)),
            Self::Timer(result) => {
                ExchangeFuture::after(Duration::from_millis(20), result.map_err(error))
            }
        }
    }
}
#[derive(Default)]
struct Reached {
    ready: AtomicBool,
    notify: Notify,
}
impl Reached {
    fn set(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
    async fn wait(&self) {
        while !self.ready.load(Ordering::SeqCst) {
            self.notify.notified().await;
        }
    }
}
#[derive(Default)]
struct Jobs(Mutex<Vec<tokio::task::JoinHandle<()>>>);
impl Drop for Jobs {
    fn drop(&mut self) {
        for job in self.0.get_mut().unwrap() {
            job.abort();
        }
    }
}
#[derive(Clone)]
struct Wire {
    endpoint: String,
    reached: Arc<Reached>,
    release: watch::Receiver<bool>,
    jobs: Arc<Jobs>,
}
#[derive(Clone)]
pub struct Transport {
    pub phase: String,
    event: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    wire: Option<Wire>,
}
impl Transport {
    pub fn is_io(&self) -> bool {
        self.wire.is_some()
    }
    pub fn scripted(&self, request: Request<Full<Bytes>>) -> Scripted {
        assert_eq!(
            request.uri().host(),
            Some("fixture.test"),
            "fixture routing boundary"
        );
        let target = request.uri().path().to_owned();
        let attempt = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(target.clone());
            requests.iter().filter(|p| p.as_str() == "/start").count()
        };
        let retry = self.phase.starts_with("retry-");
        let redirect = self.phase == "redirect-hop" && target == "/start";
        let failed_retry = retry && attempt == 1;
        let body_phase = self.phase == "body";
        let pending_headers = !retry && !redirect && !body_phase;
        if pending_headers && self.event == "idle" {
            return Scripted::Pending;
        }
        let result = if (pending_headers && self.event == "rejected")
            || (failed_retry && self.phase == "retry-error")
        {
            Err("fetch failed".into())
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
                } else if failed_retry {
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
    pub fn send<E: Send + 'static>(
        &self,
        mut request: Request<Full<Bytes>>,
        error: fn(JsString) -> E,
    ) -> ExchangeFuture<Result<Response<ModelBody>, E>> {
        let Some(wire) = &self.wire else {
            return self.scripted(request).exchange(error);
        };
        assert_eq!(
            request.uri().host(),
            Some("fixture.test"),
            "fixture routing boundary"
        );
        self.requests
            .lock()
            .unwrap()
            .push(request.uri().path().to_owned());
        *request.uri_mut() = format!("{}{}", wire.endpoint, request.uri().path())
            .parse()
            .unwrap();
        let (send, receive) = tokio::sync::oneshot::channel();
        let reached = wire.reached.clone();
        let idle = self.event == "idle";
        wire.jobs.0.lock().unwrap().push(tokio::spawn(async move {
            let client =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .build_http();
            let result = match client.request(request).await {
                Ok(response) => {
                    let (parts, body) = response.into_parts();
                    let bytes = body
                        .collect()
                        .await
                        .expect("loopback complete body")
                        .to_bytes();
                    Ok(Response::from_parts(
                        parts,
                        Full::new(bytes).map_err(|never| match never {}).boxed(),
                    ))
                }
                Err(_) => Err(JsString::from("fetch failed")),
            };
            let _ = send.send(result);
            if !idle {
                reached.set();
            }
        }));
        let mut release = wire.release.clone();
        ExchangeFuture::io(async move {
            while !*release.borrow_and_update() {
                release.changed().await.expect("wire gate sender");
            }
            receive.await.expect("wire driver outcome").map_err(error)
        })
    }
}
pub async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Option<F::Output> {
    std::future::poll_fn(|cx| {
        Poll::Ready(match future.as_mut().poll(cx) {
            Poll::Ready(v) => Some(v),
            Poll::Pending => None,
        })
    })
    .await
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("private loopback replay deadline")
}
fn counted(row: &Value, mut value: Value, transport: &Transport) -> Value {
    let requests = transport.requests.lock().unwrap();
    let ts_requests = row["requests"].as_array().expect("TS request trace");
    // A registered retry difference may change length. Calls/retries below are
    // still compared strictly; every actual hop must retain its TS target.
    let expected: Vec<_> = (0..requests.len())
        .map(|index| {
            ts_requests
                .get(index)
                .unwrap_or_else(|| ts_requests.last().expect("TS issued request"))
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(*requests, expected, "private request trace: {}", row["id"]);
    value["calls"] = json!(requests.len());
    value["retries"] = json!(
        requests
            .iter()
            .filter(|p| p.as_str() == "/start")
            .count()
            .checked_sub(1)
            .expect("caller issued request")
    );
    value
}
pub fn ok(content: String) -> Value {
    json!({"name":null,"message":null,"delivery":"ok","content":content})
}
pub fn error(name: &str, message: &str) -> Value {
    json!({"name":name,"message":message,"delivery":"throw","content":null})
}
#[derive(Default)]
struct TimerWake(Mutex<Option<Instant>>);
impl Wake for TimerWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.lock().unwrap().get_or_insert_with(Instant::now);
    }
}
fn poll_recorded<F: Future>(mut future: Pin<&mut F>, wake: &Arc<TimerWake>) -> Poll<F::Output> {
    future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(&Waker::from(
            wake.clone(),
        )))
}
async fn advance_to(at: Instant) {
    if Instant::now() < at {
        tokio::time::advance(at - Instant::now()).await;
    }
}
fn timing(row: &Value) -> (u64, i64, bool) {
    let phase = row["phase"].as_str().unwrap();
    let event = row["event"].as_str().unwrap();
    let order = row["order"].as_str().unwrap();
    let expected = if phase.starts_with("retry-") {
        500
    } else if event != "idle" {
        20
    } else if row["path"] == "hooks" || phase == "body" {
        600000
    } else if row["path"] == "mcp_get" {
        10000
    } else {
        60000
    };
    let timing = &row["timing"];
    assert_eq!(
        timing["requested"], expected,
        "private requested duration contract"
    );
    assert_eq!(
        timing["effective"],
        if event == "idle" { 40 } else { 20 },
        "private effective duration contract"
    );
    let gap = match order {
        "continuation-first" => 5,
        "parent-first" => -5,
        "tie-parent-first" | "tie-parent-last" => 0,
        _ => panic!("unknown order"),
    };
    assert_eq!(timing["parentGap"], gap, "private parent gap contract");
    assert_eq!(
        timing["parentRegisteredFirst"],
        order != "tie-parent-last",
        "private registration contract"
    );
    assert_eq!(
        timing["target"],
        if phase.starts_with("retry-") {
            "retry"
        } else if event == "idle" {
            "watchdog"
        } else {
            "completion"
        },
        "private target contract"
    );
    assert_eq!(
        timing["source"],
        if phase.starts_with("retry-") {
            "baseline retry timer"
        } else if event == "idle" && row["path"] != "hooks" && phase != "body" {
            "baseline request timeout"
        } else if event == "idle" {
            "fixture-injected watchdog"
        } else {
            "fixture-injected completion"
        },
        "private timer source contract"
    );
    (expected, gap, order != "tie-parent-last")
}
pub async fn replay<F, Fut>(row: &Value, run: F) -> Value
where
    F: Fn(Transport, Cancellation) -> Fut,
    Fut: Future<Output = Value>,
{
    if row["phase"] == "io-headers" {
        return replay_io(row, run).await;
    }
    let (operation, gap, parent_first) = timing(row);
    for candidate in 0..4096 {
        let transport = Transport {
            phase: row["phase"].as_str().unwrap().into(),
            event: row["event"].as_str().unwrap().into(),
            requests: Arc::default(),
            wire: None,
        };
        let signal = Cancellation::new();
        let start = Instant::now();
        let mut future = Box::pin(run(transport.clone(), signal.clone()));
        let retry = transport.phase.starts_with("retry-");
        let mut result = if retry {
            let wake = Arc::new(TimerWake::default());
            assert!(
                poll_recorded(future.as_mut(), &wake).is_pending(),
                "retry starts pending"
            );
            assert_eq!(
                transport.requests.lock().unwrap().len(),
                1,
                "retry predecessor only"
            );
            // Await the earliest known timer directly: manually advancing late
            // could coalesce a 501ms candidate into the calibration's wake tick.
            tokio::time::sleep_until(start + Duration::from_millis(500)).await;
            let reference = Instant::now();
            assert!(
                (500..=501).contains(&(reference - start).as_millis()),
                "bounded timer tick calibration"
            );
            if *wake.0.lock().unwrap() != Some(reference) {
                continue;
            }
            println!(
                "RETRY_SAMPLE {} candidate={} calibrated_ms={}",
                row["id"],
                candidate + 1,
                (reference - start).as_millis()
            );
            None
        } else {
            poll_once(future.as_mut()).await
        };
        let parent = operation.checked_add_signed(gap).unwrap();
        if result.is_none() && (gap > 0 || (gap == 0 && !parent_first)) {
            advance_to(start + Duration::from_millis(operation)).await;
            result = poll_once(future.as_mut()).await;
            if result.is_none() {
                advance_to(start + Duration::from_millis(parent)).await;
            }
        } else if result.is_none() {
            advance_to(start + Duration::from_millis(operation + 1)).await;
        }
        if result.is_none() {
            signal.cancel_at(start + Duration::from_millis(parent));
            result = poll_once(future.as_mut()).await;
        }
        if result.is_none() {
            tokio::time::advance(Duration::from_millis(600001)).await;
            result = poll_once(future.as_mut()).await;
        }
        return counted(
            row,
            result.expect("bounded private matrix must settle"),
            &transport,
        );
    }
    panic!("bounded retry sampling exhausted without a real 500ms candidate");
}
async fn replay_io<F, Fut>(row: &Value, run: F) -> Value
where
    F: Fn(Transport, Cancellation) -> Fut,
    Fut: Future<Output = Value>,
{
    let event = row["event"].as_str().unwrap().to_owned();
    let order = row["order"].as_str().unwrap();
    let reached = Arc::new(Reached::default());
    let jobs = Arc::new(Jobs::default());
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    jobs.0.lock().unwrap().push(tokio::spawn({ let reached = reached.clone(); let event = event.clone(); async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|v| v == b"\r\n\r\n") {
            let mut buffer = [0;1024]; let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0, "HTTP request reached real peer"); request.extend_from_slice(&buffer[..count]);
        }
        if event == "idle" { reached.set(); std::future::pending::<()>().await; }
        else if event == "rejected" { stream.write_all(b"not an HTTP response\r\n").await.unwrap(); }
        else { stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\nFIRST").await.unwrap(); }
    }}));
    let (release, waiting) = watch::channel(false);
    let transport = Transport {
        phase: "io-headers".into(),
        event: event.clone(),
        requests: Arc::default(),
        wire: Some(Wire {
            endpoint,
            reached: reached.clone(),
            release: waiting,
            jobs: jobs.clone(),
        }),
    };
    let signal = Cancellation::new();
    let mut future = Box::pin(run(transport.clone(), signal.clone()));
    assert!(
        poll_once(future.as_mut()).await.is_none(),
        "wire held before outcome"
    );
    bounded(reached.wait()).await;
    let mut result = None;
    match order {
        "continuation-first" => {
            release.send_replace(true);
            result = poll_once(future.as_mut()).await;
            if event == "fulfilled" {
                assert!(result.is_some(), "real fulfilled I/O continuation drained");
            }
            signal.cancel_synchronously();
        }
        "parent-first" => {
            signal.cancel_synchronously();
            result = poll_once(future.as_mut()).await;
            release.send_replace(true);
        }
        "tie-parent-last" => {
            release.send_replace(true);
            signal.cancel_synchronously();
        }
        "tie-parent-first" => {
            signal.cancel_synchronously();
            release.send_replace(true);
        }
        _ => panic!("unknown I/O order"),
    }
    if result.is_none() {
        result = poll_once(future.as_mut()).await;
    }
    let value = counted(
        row,
        result.expect("bounded I/O must settle on completion or parent"),
        &transport,
    );
    let handles = std::mem::take(&mut *jobs.0.lock().unwrap());
    for handle in handles {
        handle.abort();
        let _ = bounded(handle).await;
    }
    value
}

/// Exercise every new metadata predicate and every requested path in memory.
pub fn negative_contracts() {
    let oracle: Value = serde_json::from_str(RAW).unwrap();
    let mut count = 0;
    for row in oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| matches!(r["path"].as_str(), Some("mcp_get" | "mcp_post" | "hooks")))
    {
        if row["phase"] != "io-headers" {
            timing(row);
            for (field, anchor) in [
                ("requested", "private requested duration contract"),
                ("effective", "private effective duration contract"),
                ("parentGap", "private parent gap contract"),
                ("parentRegisteredFirst", "private registration contract"),
                ("target", "private target contract"),
                ("source", "private timer source contract"),
            ] {
                let mut changed = row.clone();
                changed["timing"][field] = Value::Null;
                let panic = std::panic::catch_unwind(|| timing(&changed))
                    .expect_err("timing metadata mutant must fail");
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap();
                assert!(message.contains(anchor), "bound timing contract assertion");
                timing(row);
                count += 1;
            }
        }
        let paths: Vec<String> = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect();
        let transport = Transport {
            phase: row["phase"].as_str().unwrap().into(),
            event: row["event"].as_str().unwrap().into(),
            requests: Arc::new(Mutex::new(paths.clone())),
            wire: None,
        };
        counted(row, ok("FIRST".into()), &transport);
        // Use a fresh transport: an expected panic while holding its mutex
        // poisons only the mutant, never the restored positive observation.
        let bad = Transport {
            requests: Arc::new(Mutex::new(vec!["/wrong".into()])),
            ..transport.clone()
        };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            counted(row, ok("FIRST".into()), &bad)
        }))
        .expect_err("request trace mutant must fail");
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(
            message.contains("private request trace"),
            "bound trace contract assertion"
        );
        counted(row, ok("FIRST".into()), &transport);
        count += 1;
    }
    println!("PRIVATE_CONTRACT_NEGATIVE {count} rejected; restored PASS");
}
