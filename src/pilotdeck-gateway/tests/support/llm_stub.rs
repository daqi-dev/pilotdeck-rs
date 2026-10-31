//! Reusable loopback HTTP replay. Listener and all connections have one owner.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Request, Response,
    body::{Body, Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    net::TcpListener,
    sync::Notify,
    task::{JoinHandle, JoinSet},
};

struct NeverBody;
impl Body for NeverBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Pending
    }
}
pub struct LlmStub {
    pub origin: String,
    pub requests: Arc<Mutex<Vec<Value>>>,
    pub active: Arc<AtomicUsize>,
    pub closed: Arc<Notify>,
    shutdown: Arc<Notify>,
    // #917 fix-r1: one gate per script step. A `"kind":"gate"` step records
    // the request (so the caller can prove it is in flight) and then parks
    // the response until `release(step_index)`; all other steps ignore it.
    gates: Arc<Mutex<Vec<Arc<Notify>>>>,
    task: Option<JoinHandle<()>>,
}
impl LlmStub {
    pub async fn start(steps: Vec<Value>, clock: Arc<AtomicU64>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listen");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(Notify::new());
        let shutdown = Arc::new(Notify::new());
        let records = requests.clone();
        let active_count = active.clone();
        let closed_notice = closed.clone();
        let stopping = shutdown.clone();
        let gates = Arc::new(Mutex::new(
            steps
                .iter()
                .map(|_| Arc::new(Notify::new()))
                .collect::<Vec<_>>(),
        ));
        let steps = Arc::new(steps);
        let task_gates = gates.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let (socket,_)=accepted.expect("accept");
                        let records=records.clone();let steps=steps.clone();let clock=clock.clone();let active_count=active_count.clone();let closed_notice=closed_notice.clone();let gates=task_gates.clone();
                        active_count.fetch_add(1,Ordering::SeqCst);
                        connections.spawn(async move {
                            let _connection=Connection{active:active_count,closed:closed_notice};
                            let service=service_fn(move |req:Request<Incoming>| {
                                let records=records.clone();let steps=steps.clone();let clock=clock.clone();let gates=gates.clone();
                                async move {
                                    let (parts,body)=req.into_parts();
                                    let bytes=body.collect().await.map_err(std::io::Error::other)?.to_bytes();
                                    let mut headers=serde_json::Map::new();
                                    for (key,value) in &parts.headers { headers.insert(key.to_string(),json!(value.as_bytes().iter().map(|&b|char::from(b)).collect::<String>())); }
                                    headers.insert("host".into(),json!("$HOST"));
                                    let index={let mut records=records.lock().unwrap();let index=records.len(); records.push(json!({"method":parts.method.as_str(),"url":parts.uri.to_string(),"headers":headers,"body":String::from_utf8(bytes.to_vec()).unwrap(),"at":clock.load(Ordering::SeqCst)}));index};
                                    let step=steps.get(index).or_else(||steps.last()).expect("script step");
                                    if step["kind"]=="disconnect" {return Err(std::io::Error::other("script disconnect"));}
                                    if step["kind"]=="headers-hang" {std::future::pending::<()>().await;}
                                    if step["kind"]=="gate" { let gate=gates.lock().unwrap().get(index).cloned(); if let Some(gate)=gate { gate.notified().await; } }
                                    let body:BoxBody<Bytes,Infallible>=if step["kind"]=="body-hang" {NeverBody.boxed()} else {Full::new(Bytes::from(step["body"].as_str().unwrap_or("").to_owned())).boxed()};
                                    Ok::<_,std::io::Error>(Response::builder().status(step["status"].as_u64().unwrap_or(200) as u16).header("content-type","application/json").body(body).unwrap())
                                }
                            });
                            let _=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket),service).await;
                        });
                    }
                    _=stopping.notified()=>break,
                    Some(_)=connections.join_next(),if !connections.is_empty()=>{}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            origin,
            requests,
            active,
            closed,
            shutdown,
            gates,
            task: Some(task),
        }
    }
    /// Release a parked `"kind":"gate"` step so its response is sent. Steps
    /// are indexed by request order; a request past the end of the script
    /// falls back to the last step, so it has no gate and this is a no-op.
    pub fn release(&self, index: usize) {
        if let Some(gate) = self.gates.lock().unwrap().get(index) {
            gate.notify_one();
        }
    }
    pub async fn close(mut self) {
        self.shutdown.notify_one();
        self.task.take().unwrap().await.expect("server shutdown");
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
    }
}
struct Connection {
    active: Arc<AtomicUsize>,
    closed: Arc<Notify>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.closed.notify_one();
    }
}
impl Drop for LlmStub {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
