//! Real background registry around an explicitly selected networkFetch child.
use super::*;
use pilotdeck_gateway::runtime::{
    background::{BackgroundSubagentRuntime, StartBackgroundSubagentInput, SubagentRun},
    tools::builtin::web::network::{BodyError, RetryOptions},
};
use pilotdeck_protocol::JsString;
use std::{sync::atomic::AtomicUsize, task::Waker};

#[derive(Default)]
pub(super) struct Gate {
    allowed: AtomicBool,
    polls: AtomicUsize,
    ready: AtomicBool,
    waker: Mutex<Option<Waker>>,
}
impl Gate {
    pub(super) async fn step(&self) {
        let previous = self.polls.load(Ordering::SeqCst);
        self.allowed.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
        for _ in 0..64 {
            tokio::task::yield_now().await;
            if self.polls.load(Ordering::SeqCst) > previous {
                return;
            }
        }
        panic!("background child did not reach its deterministic poll gate");
    }
}
pub(super) struct Stepped<F> {
    pub(super) future: Pin<Box<F>>,
    pub(super) gate: Arc<Gate>,
}
impl<F: Future> Future for Stepped<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<F::Output> {
        *self.gate.waker.lock().unwrap() = Some(cx.waker().clone());
        if !self.gate.allowed.swap(false, Ordering::SeqCst) {
            return Poll::Pending;
        }
        let result = self.future.as_mut().poll(cx);
        self.gate.ready.store(result.is_ready(), Ordering::SeqCst);
        self.gate.polls.fetch_add(1, Ordering::SeqCst);
        result
    }
}
fn input(run: SubagentRun) -> StartBackgroundSubagentInput {
    StartBackgroundSubagentInput {
        kind: None,
        session_id: "session".into(),
        parent_turn_id: "turn".into(),
        subagent_id: "child".into(),
        subagent_type: "test".into(),
        observed_subagent_id: None,
        run,
    }
}

// Resolve the already-queued redirect traversal synchronously, then retain
// the single target timer on the returned ExchangeFuture. The asserted ready
// traversal must not introduce another pending await or conceal I/O.
struct ChildTransport(MatrixTransport);
impl ModelHttpTransport for ChildTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let timer = std::cell::Cell::new(false);
        let idle = std::cell::Cell::new(false);
        let mut traversal = Box::pin(fetch_redirect::follow(
            request.uri().to_string().parse().unwrap(),
            hyper::Method::GET,
            hyper::HeaderMap::new(),
            Bytes::new(),
            || TransportFailure {
                message: "fetch failed".into(),
                cause_code: None,
            },
            |request| {
                let source = self.0.scripted(request);
                let timer = &timer;
                let idle = &idle;
                async move {
                    match source {
                        Scripted::Pending => {
                            idle.set(true);
                            std::future::pending().await
                        }
                        Scripted::Ready(result) => result,
                        Scripted::Timer(result) => {
                            assert!(!timer.replace(true), "single final transport timer");
                            result
                        }
                    }
                }
            },
        ));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match traversal.as_mut().poll(&mut context) {
            Poll::Ready(result) if timer.get() => {
                TransportFuture::after(Duration::from_millis(20), result)
            }
            Poll::Ready(result) => TransportFuture::ready(result),
            Poll::Pending => {
                assert!(
                    idle.get(),
                    "only declared never-completing transport may remain pending"
                );
                TransportFuture::io(std::future::pending())
            }
        }
    }
}
pub(super) async fn registry_settled(runtime: &BackgroundSubagentRuntime, id: &JsString) {
    for _ in 0..1024 {
        if runtime.get(id).unwrap().status.is_terminal() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("registry did not settle after child completion");
}
async fn bounded_wait<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("background done notification deadline")
}
async fn child(transport: MatrixTransport, signal: ToolCancellation) -> Value {
    let context = context(signal);
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
    match network_fetch(&ChildTransport(transport), &request, &options).await {
        Err(value) => error_value(&value),
        Ok(mut response) => match response.read_body(1024, Some(&context)).await {
            Ok(bytes) => ok(String::from_utf8(bytes).unwrap()),
            Err(BodyError::Aborted) => error(Some("AbortError"), "This operation was aborted"),
            Err(BodyError::Terminated) => error(Some("TypeError"), "terminated"),
            Err(BodyError::TooLarge) => panic!("small body"),
        },
    }
}
pub(super) async fn replay(row: &Value) -> Value {
    if row["phase"] == "lifecycle" {
        return lifecycle(row).await;
    }
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MatrixTransport {
        path: "background".into(),
        parent_first: row["order"] != "tie-parent-last",
        phase: row["phase"].as_str().unwrap().into(),
        event: row["event"].as_str().unwrap().into(),
        requests: requests.clone(),
    };
    let gate = Arc::new(Gate::default());
    let observed = Arc::new(Mutex::new(None));
    let runtime = BackgroundSubagentRuntime::default();
    let task = runtime.start(input(Box::new({
        let gate = gate.clone();
        let observed = observed.clone();
        move |signal| {
            Box::pin(Stepped {
                gate,
                future: Box::pin(async move {
                    let result = child(transport, signal).await;
                    let returned = if result["delivery"] == "throw" {
                        Err(JsString::from(result["message"].as_str().unwrap()))
                    } else {
                        Ok(())
                    };
                    *observed.lock().unwrap() = Some(result);
                    returned
                }),
            })
        }
    })));
    assert_eq!(task.status.as_str(), "pending");
    gate.step().await;
    assert!(
        !requests.lock().unwrap().is_empty(),
        "background reached network"
    );
    let operation = row["timing"]["requested"].as_u64().unwrap();
    let order = row["order"].as_str().unwrap();
    let parent = operation
        .checked_add_signed(row["timing"]["parentGap"].as_i64().unwrap())
        .unwrap();
    if matches!(order, "continuation-first" | "tie-parent-last") {
        tokio::time::advance(Duration::from_millis(operation)).await;
        gate.step().await;
        if gate.ready.load(Ordering::SeqCst) {
            registry_settled(&runtime, &task.task_id).await;
        }
        tokio::time::advance(Duration::from_millis(parent - operation)).await;
    } else {
        tokio::time::advance(Duration::from_millis(parent)).await;
    }
    let stopped = runtime.stop(&task.task_id);
    if observed.lock().unwrap().is_none() {
        gate.step().await;
    }
    for _ in 0..4 {
        if observed.lock().unwrap().is_some() {
            break;
        }
        tokio::time::advance(Duration::from_millis(600001)).await;
        gate.step().await;
    }
    assert!(
        observed.lock().unwrap().is_some(),
        "bounded background network must settle"
    );
    assert!(!bounded_wait(runtime.stop_and_wait(&task.task_id)).await);
    let final_task = runtime.get(&task.task_id).unwrap();
    let mut result = observed.lock().unwrap().take().unwrap();
    result["task"] = json!({"status":final_task.status.as_str(),"error":final_task.error.map(|e|e.to_utf8().unwrap()),"ended":final_task.ended_at.is_some(),"stopped":stopped});
    let requests = requests.lock().unwrap();
    result["calls"] = json!(requests.len());
    result["retries"] = json!(requests.iter().filter(|p| p.as_str() == "/start").count() - 1);
    result
}

async fn lifecycle(row: &Value) -> Value {
    let event = row["event"].as_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicUsize::new(1000));
    let runtime = BackgroundSubagentRuntime::new(Some(Arc::new(move || {
        clock.fetch_add(1, Ordering::SeqCst) as f64
    })));
    let gate = Arc::new(Gate::default());
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let pending = event == "running-uncooperative";
    let rejected = event == "settled-rejected";
    let task = runtime.start(input(Box::new({
        let calls = calls.clone();
        let gate = gate.clone();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(Stepped {
                gate,
                future: Box::pin(async move {
                    if pending {
                        wait.await.unwrap();
                    }
                    if rejected {
                        Err("fixture child failure".into())
                    } else {
                        Ok(())
                    }
                }),
            })
        }
    })));
    assert_eq!(task.status.as_str(), "pending");
    let stopped;
    let mut pending_after_stop = false;
    let mut after_stop = Value::Null;
    let mut ended_after_stop = None;
    if event == "pending-stop" {
        stopped = runtime.stop(&task.task_id);
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "pending stop must not start child"
        );
    } else if pending {
        gate.step().await;
        let mut waiting = Box::pin(runtime.stop_and_wait(&task.task_id));
        pending_after_stop = poll_once(waiting.as_mut()).await.is_none();
        assert!(
            pending_after_stop,
            "stop_and_wait must remain pending until child settles"
        );
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(
            poll_once(waiting.as_mut()).await.is_none(),
            "stop_and_wait must remain pending until child settles"
        );
        let stopped_task = runtime.get(&task.task_id).unwrap();
        ended_after_stop = stopped_task.ended_at;
        after_stop = json!({"status":stopped_task.status.as_str(),"ended":stopped_task.ended_at.is_some(),"error":stopped_task.error.map(|e|e.to_utf8().unwrap())});
        release.send(()).unwrap();
        gate.step().await;
        stopped = bounded_wait(waiting).await;
    } else {
        gate.step().await;
        registry_settled(&runtime, &task.task_id).await;
        stopped = runtime.stop(&task.task_id);
    }
    assert!(!bounded_wait(runtime.stop_and_wait(&task.task_id)).await);
    let final_task = runtime.get(&task.task_id).unwrap();
    json!({"calls":calls.load(Ordering::SeqCst),"status":final_task.status.as_str(),"error":final_task.error.map(|e|e.to_utf8().unwrap()),"ended":final_task.ended_at.is_some(),"stopped":stopped,"pendingAfterStop":pending_after_stop,"afterStop":after_stop,"endedUnchanged":if pending {json!(ended_after_stop==final_task.ended_at)}else{Value::Null}})
}
