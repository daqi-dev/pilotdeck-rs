//! Real loopback transport, gated only after the wire result is observed.
use super::background_timing::{Gate, Stepped};
use super::*;
use pilotdeck_gateway::runtime::{
    background::{BackgroundSubagentRuntime, StartBackgroundSubagentInput},
    tools::builtin::web::network::{BodyError, RetryOptions},
};
use pilotdeck_protocol::JsString;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Notify, watch},
};

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
#[derive(Clone)]
struct WireTransport {
    endpoint: String,
    reached: Arc<Reached>,
    release: watch::Receiver<bool>,
    requests: Arc<Mutex<Vec<String>>>,
    drivers: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    idle: bool,
}
impl ModelHttpTransport for WireTransport {
    fn send(&self, mut request: Request<Full<Bytes>>) -> TransportFuture {
        assert_eq!(request.uri().host(), Some("fixture.test"));
        self.requests
            .lock()
            .unwrap()
            .push(request.uri().path().to_string());
        *request.uri_mut() = format!("{}{path}", self.endpoint, path = request.uri().path())
            .parse()
            .unwrap();
        let (send, receive) = tokio::sync::oneshot::channel();
        let reached = self.reached.clone();
        let idle = self.idle;
        self.drivers.lock().unwrap().push(tokio::spawn(async move {
            // Fixture transport has no proxy/config/environment access.
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
                Err(_) => Err(TransportFailure {
                    message: "fetch failed".into(),
                    cause_code: None,
                }),
            };
            let _ = send.send(result);
            if !idle {
                reached.set();
            }
        }));
        let mut release = self.release.clone();
        TransportFuture::io(async move {
            while !*release.borrow_and_update() {
                release.changed().await.expect("wire gate sender");
            }
            receive.await.expect("wire driver outcome")
        })
    }
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("loopback replay deadline")
}
async fn child(
    path: &str,
    transport: WireTransport,
    signal: ToolCancellation,
    private: network_source::Cancellation,
) -> Value {
    let context = context(signal);
    if path == "web_fetch" {
        return web(transport, &context).await;
    }
    if path == "background" {
        let options = FetchOptions {
            timeout_ms: 60000.0,
            parent: Some(&context),
            retry: RetryOptions {
                max_retries: 1,
                ..WEB_RETRY
            },
            random: Arc::new(|| 0.0),
        };
        let request = HttpRequest {
            method: "GET",
            url: URL.into(),
            headers: vec![],
            body: None,
        };
        return match network_fetch(&transport, &request, &options).await {
            Err(value) => error_value(&value),
            Ok(mut response) => match response.read_body(1024, Some(&context)).await {
                Ok(bytes) => ok(String::from_utf8(bytes).unwrap()),
                Err(BodyError::Aborted) => error(Some("AbortError"), "This operation was aborted"),
                Err(BodyError::Terminated) => error(Some("TypeError"), "terminated"),
                Err(BodyError::TooLarge) => panic!("small body"),
            },
        };
    }
    assert_eq!(path, "fetch_redirect");
    let before_abort = Arc::new(AtomicBool::new(false));
    let result = fetch_redirect::follow(
        URL.parse().unwrap(),
        hyper::Method::GET,
        hyper::HeaderMap::new(),
        Bytes::new(),
        || error(Some("TypeError"), "fetch failed"),
        |request| {
            let sent = transport.send(request);
            let before_abort = before_abort.clone();
            let private = private.clone();
            async move {
                match network_source::exchange(
                    network_source::ExchangeFuture::io(sent),
                    private.cancelled(),
                    || private.abort_event_at(),
                    None,
                )
                .await
                {
                    network_source::Exchange::Response(Ok(response), first) => {
                        before_abort.store(first, Ordering::SeqCst);
                        Ok(response)
                    }
                    network_source::Exchange::Response(Err(_), _) => {
                        error_result("TypeError", "fetch failed")
                    }
                    network_source::Exchange::Aborted => {
                        error_result("AbortError", "This operation was aborted")
                    }
                    network_source::Exchange::Timeout { .. } => panic!("no fixture timeout"),
                }
            }
        },
    )
    .await;
    let response = match result {
        Ok(response) => response,
        Err(value) => return value,
    };
    match fetch_redirect::read_body_with_signal(
        response.into_body(),
        1024,
        Some(network_source::UNDICI_BODY_TIMEOUT),
        Some(&private),
        before_abort.load(Ordering::SeqCst),
    )
    .await
    {
        Ok(bytes) => ok(String::from_utf8(bytes).unwrap()),
        Err(fetch_redirect::BodyError::Aborted) => {
            error(Some("AbortError"), "This operation was aborted")
        }
        Err(_) => error(Some("TypeError"), "terminated"),
    }
}
fn error_result<T>(name: &str, message: &str) -> Result<T, Value> {
    Err(error(Some(name), message))
}
pub(super) async fn replay(row: &Value) -> Value {
    let path = row["path"].as_str().unwrap().to_owned();
    let event = row["event"].as_str().unwrap().to_owned();
    let order = row["order"].as_str().unwrap();
    let reached = Arc::new(Reached::default());
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn({
        let reached = reached.clone();
        let event = event.clone();
        async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|v| v == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "HTTP request reached real peer");
                request.extend_from_slice(&buffer[..count]);
            }
            if event == "idle" {
                reached.set();
                std::future::pending::<()>().await;
            } else if event == "rejected" {
                stream.write_all(b"not an HTTP response\r\n").await.unwrap();
            } else {
                stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\nFIRST").await.unwrap();
            }
        }
    });
    let (release, waiting) = watch::channel(false);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let drivers = Arc::new(Mutex::new(Vec::new()));
    let transport = WireTransport {
        endpoint,
        reached: reached.clone(),
        release: waiting,
        requests: requests.clone(),
        drivers: drivers.clone(),
        idle: event == "idle",
    };
    let gate = Arc::new(Gate::default());
    let observed = Arc::new(Mutex::new(None));
    let parent = ToolCancellation::new();
    let private = network_source::Cancellation::new();
    let runtime = BackgroundSubagentRuntime::default();
    let work = {
        let observed = observed.clone();
        let gate = gate.clone();
        let private = private.clone();
        let path = path.clone();
        move |signal| -> pilotdeck_gateway::runtime::background::SubagentFuture {
            Box::pin(Stepped {
                gate,
                future: Box::pin(async move {
                    let value = child(&path, transport, signal, private).await;
                    let result = if value["delivery"] == "throw" {
                        Err(JsString::from(value["message"].as_str().unwrap()))
                    } else {
                        Ok(())
                    };
                    *observed.lock().unwrap() = Some(value);
                    result
                }),
            })
        }
    };
    let (task, job) = if path == "background" {
        (
            Some(runtime.start(StartBackgroundSubagentInput {
                kind: None,
                session_id: "session".into(),
                parent_turn_id: "turn".into(),
                subagent_id: "child".into(),
                subagent_type: "test".into(),
                observed_subagent_id: None,
                run: Box::new(work),
            })),
            None,
        )
    } else {
        (None, Some(tokio::spawn(work(parent.clone()))))
    };
    gate.step().await;
    bounded(reached.wait()).await;
    let abort = || {
        if let Some(task) = &task {
            runtime.stop(&task.task_id)
        } else {
            parent.cancel_synchronously();
            private.cancel_synchronously();
            true
        }
    };
    let stopped;
    if order == "continuation-first" {
        release.send_replace(true);
        gate.step().await;
        if event == "fulfilled" {
            assert!(
                observed.lock().unwrap().is_some(),
                "real fulfilled I/O continuation drained"
            );
            if let Some(task) = &task {
                background_timing::registry_settled(&runtime, &task.task_id).await;
            }
        }
        stopped = abort();
    } else if order == "parent-first" {
        stopped = abort();
        gate.step().await;
        release.send_replace(true);
    } else if order == "tie-parent-last" {
        release.send_replace(true);
        stopped = abort();
    } else {
        stopped = abort();
        release.send_replace(true);
    }
    if observed.lock().unwrap().is_none() {
        gate.step().await;
    }
    assert!(
        observed.lock().unwrap().is_some(),
        "bounded I/O must settle on completion or parent"
    );
    let mut result = observed.lock().unwrap().take().unwrap();
    if let Some(task) = task {
        assert!(!bounded(runtime.stop_and_wait(&task.task_id)).await);
        let task = runtime.get(&task.task_id).unwrap();
        result["task"] = json!({"status":task.status.as_str(),"error":task.error.map(|e|e.to_utf8().unwrap()),"ended":task.ended_at.is_some(),"stopped":stopped});
    }
    if let Some(job) = job {
        let _ = bounded(job).await.unwrap();
    }
    server.abort();
    let _ = server.await;
    let handles = std::mem::take(&mut *drivers.lock().unwrap());
    for handle in handles {
        handle.abort();
        let _ = handle.await;
    }
    let requests = requests.lock().unwrap();
    result["calls"] = json!(requests.len());
    result["retries"] = json!(requests.iter().filter(|p| p.as_str() == "/start").count() - 1);
    result
}
