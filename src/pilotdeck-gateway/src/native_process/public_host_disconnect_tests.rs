//! Real TCP disconnect against the module-host transport and real PDF parser.
//! The resolver/provider is a test fixture, not the production resolver.
use crate::{
    http_proxy,
    module_host::{
        self, Config, HostFailure, HostResponse, PublicHostPrincipal, PublicHostProvider,
        PublicHostResolver,
    },
    runtime::model::router::AbortFlag,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use pilotdeck_protocol::JsValue;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};

type HostResult = Result<HostResponse, HostFailure>;
type Event = (&'static str, String, usize);
const DEADLINE: Duration = Duration::from_secs(8);

struct Fixture {
    events: mpsc::UnboundedSender<Event>,
    release: Arc<(Mutex<bool>, Condvar)>,
}
impl PublicHostResolver for Fixture {
    fn resolve(
        &self,
        _: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn PublicHostProvider>, HostFailure>> + Send + '_>>
    {
        let provider = Arc::new(Self {
            events: self.events.clone(),
            release: self.release.clone(),
        });
        Box::pin(async move { Ok(provider as Arc<dyn PublicHostProvider>) })
    }
}
impl PublicHostProvider for Fixture {
    fn operations(&self) -> Vec<String> {
        vec!["file_parse".into()]
    }
    fn call(
        &self,
        _: &str,
        _: &JsValue,
        _: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = HostResult> + Send + '_>> {
        panic!("transport must propagate the per-request cancellation signal")
    }
    fn call_with_signal(
        &self,
        _: &str,
        input: &JsValue,
        principal: &PublicHostPrincipal,
        signal: AbortFlag,
    ) -> Pin<Box<dyn Future<Output = HostResult> + Send + '_>> {
        let input = input.clone();
        let id = principal.agent_id.to_utf8().unwrap();
        let events = self.events.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let observed = signal.clone();
            let observed_id = id.clone();
            let observer_events = events.clone();
            tokio::spawn(async move {
                observed.cancelled().await;
                let _ = observer_events.send(("cancel", observed_id, 0));
            });
            let worker_events = events.clone();
            let worker_id = id.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut pages = 0usize;
                let response = super::public_host_file::parse_observed_for_test(
                    &input,
                    Some(&signal),
                    &mut || {
                        pages += 1;
                        if pages == 2 {
                            let state = if signal.is_aborted() {
                                "cancelled"
                            } else {
                                "live"
                            };
                            let _ = worker_events.send((state, worker_id.clone(), pages));
                        }
                        let _ = worker_events.send(("page", worker_id.clone(), pages));
                        if pages == 1 {
                            let (lock, cv) = &*release;
                            let ready = lock.lock().unwrap();
                            let _ = cv
                                .wait_timeout_while(ready, DEADLINE, |ready| !*ready)
                                .unwrap();
                        }
                    },
                );
                let _ = worker_events.send(("worker_done", worker_id, response.status as usize));
                response
            })
            .await
            .expect("PDF worker joins");
            Ok(result)
        })
    }
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn server(config: Config) -> (std::net::SocketAddr, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted.unwrap();
                    let config = config.clone();
                    tasks.spawn(async move {
                        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
                        let _ = http_proxy::serve_connection_native_with_shutdown(socket, move |mut request, _lease| {
                            let config = config.clone();
                            async move { module_host::serve(&mut request, &config).await.expect("host route") }
                        }, stop, None, None).await;
                    });
                }
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
    });
    (address, Server(task))
}

async fn request(address: std::net::SocketAddr, id: &str, pdf: &[u8]) -> TcpStream {
    let body = serde_json::json!({"principal":{"pilotDeckUserId":"u","tenantId":"t","actorUserId":"a","agentId":id},"operation":"file_parse","input":{"filename":"two.pdf","content_base64":STANDARD.encode(pdf)}}).to_string();
    let request = format!(
        "POST /api/module-host/call HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut socket = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .unwrap()
        .unwrap();
    timeout(DEADLINE, socket.write_all(request.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    socket
}
async fn event(events: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    timeout(DEADLINE, events.recv())
        .await
        .expect("event deadline")
        .expect("event channel")
}
fn tiny_pdf(pages: &[&str]) -> Vec<u8> {
    let font = 3 + pages.len() * 2;
    let kids = (0..pages.len())
        .map(|i| format!("{} 0 R", 3 + i * 2))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", pages.len()),
    ];
    for (i, text) in pages.iter().enumerate() {
        let content = 4 + i * 2;
        let stream = format!("BT /F1 12 Tf 40 700 Td ({text}) Tj ET");
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>"));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    if !pages.is_empty() {
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
    }
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0usize];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    out
}

struct ReleaseOnDrop(Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_request_disconnect_cancels_only_its_pdf_worker() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_on_drop = ReleaseOnDrop(release.clone());
    let config = Config {
        token: "fixture".into(),
        capabilities: Some(Arc::new(Fixture {
            events: tx,
            release: release.clone(),
        })),
    };
    let (address, _server) = server(config).await;
    let pdf = tiny_pdf(&["FIRST PAGE 42", "SECOND PAGE 84"]);
    let disconnected = request(address, "disconnect", &pdf).await;
    let mut control = request(address, "control", &pdf).await;
    let mut first = vec![event(&mut rx).await, event(&mut rx).await];
    first.sort();
    assert_eq!(
        first,
        vec![
            ("page", "control".into(), 1),
            ("page", "disconnect".into(), 1)
        ]
    );
    // Both requests have reached the real PDF parser after the full HTTP body.
    drop(disconnected);
    assert_eq!(
        event(&mut rx).await,
        ("cancel", "disconnect".into(), 0),
        "the peer close must cancel its own live call before the barrier opens"
    );
    drop(release_on_drop);
    let mut observed = vec![];
    while !observed
        .iter()
        .any(|item: &Event| item.0 == "worker_done" && item.1 == "control")
        || !observed
            .iter()
            .any(|item: &Event| item.0 == "worker_done" && item.1 == "disconnect")
    {
        observed.push(event(&mut rx).await);
    }
    assert!(
        observed.contains(&("worker_done", "disconnect".into(), 499)),
        "{observed:?}"
    );
    assert!(
        !observed.contains(&("page", "disconnect".into(), 2)),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("page", "control".into(), 2)),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("worker_done", "control".into(), 200)),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("live", "control".into(), 2)),
        "control was cancelled before its second page: {observed:?}"
    );
    let mut response = Vec::new();
    timeout(DEADLINE, control.read_to_end(&mut response))
        .await
        .expect("control response deadline")
        .expect("control response");
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert!(
        response.contains("FIRST PAGE 42") && response.contains("SECOND PAGE 84"),
        "{response}"
    );
}
