//! Explicitly started, loopback-only native WS listener over the serial turn host.
//! There is no upstream or implicit environment/home discovery here. Embeddings
//! construct GatewayServices on TurnGateway and own their persistent resources.
use crate::{
    ProxyError, acceptor,
    frame::{self, Decoder},
    http_proxy,
    runtime::{connection::GatewayTextConnection, turn::gateway::TurnGateway},
};
use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
};

/// Native is opt-in through this constructor; no default port is claimed.
/// Port zero is intended for isolated probes. Deployed ports must be >= 1024.
pub struct NativeWsServer {
    listener: TcpListener,
    host: TurnGateway,
    token: String,
    version: String,
}
impl NativeWsServer {
    pub async fn bind(
        address: SocketAddr,
        host: TurnGateway,
        token: String,
        version: String,
    ) -> Result<Self, ProxyError> {
        if !crate::mutants::on("native-ws-bind-policy")
            && (address.ip() != Ipv4Addr::LOCALHOST
                || (address.port() != 0 && address.port() < 1024))
        {
            return Err("native WS requires 127.0.0.1 and a high port (or 0)".into());
        }
        if !crate::mutants::on("native-ws-token-policy") && token.trim().is_empty() {
            return Err("native WS requires an explicit nonempty token".into());
        }
        Ok(Self {
            listener: TcpListener::bind(address).await?,
            host,
            token,
            version,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Stop accepting on `stop`, then drain existing sockets and their owned
    /// requests. Like TS server.close(), existing WS clients may finish their
    /// work. Unlike its unbounded Promise, the isolated runner has an explicit
    /// drain budget: on expiry it returns an error and shuts down the host.
    /// The budget also covers incomplete HTTP handshakes and peers that never
    /// finish. A timeout is never reported as successful quiescence.
    pub fn run(
        self,
        stop: impl Future<Output = ()>,
        drain_budget: Duration,
    ) -> impl Future<Output = Result<(), ProxyError>> {
        let cleanup = StopHost(
            self.host.clone(),
            if crate::mutants::on("native-ws-unbounded-cancel") {
                Duration::MAX
            } else {
                drain_budget
            },
        );
        async move {
            let _host_cleanup = cleanup;
            let Self {
                listener,
                host,
                token,
                version,
            } = self;
            let mut connections = JoinSet::new();
            tokio::pin!(stop);
            let accept_error = loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => break None,
                    accepted = listener.accept() => match accepted {
                        Ok((socket, _)) => {
                            let host = host.clone();
                            let token = token.clone();
                            let version = version.clone();
                            connections.spawn(async move {
                                if let Err(error) = serve_socket(socket, host, token, version).await {
                                    eprintln!("native WS connection: {error}");
                                }
                            });
                        }
                        Err(error) if matches!(error.kind(), std::io::ErrorKind::InvalidInput | std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported) => break Some(error),
                        Err(error) => {
                            eprintln!("native WS accept: {error}");
                            tokio::select! {
                                _ = &mut stop => break None,
                                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                            }
                        }
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            };
            drop(listener);
            if crate::mutants::on("native-ws-skip-drain") {
                return Ok(());
            }
            let budget = if crate::mutants::on("native-ws-drain-no-deadline")
                || crate::mutants::on("native-ws-handshake-no-deadline")
            {
                Duration::from_secs(3600)
            } else {
                drain_budget
            };
            let drained = tokio::time::timeout(budget, async {
                while connections.join_next().await.is_some() {}
            })
            .await
            .is_ok();
            // Aborting transport waiters never cancels business jobs on the serial
            // host. Its shutdown cancels owned turns and waits for finally fences.
            if crate::mutants::on("native-ws-handshake-leak") {
                connections.detach_all();
            } else {
                connections.abort_all();
                while connections.join_next().await.is_some() {}
            }
            host.shutdown(drain_budget)
                .await
                .map_err(std::io::Error::other)?;
            if !drained && !crate::mutants::on("native-ws-drain-false-success") {
                return Err("native WS drain deadline exceeded".into());
            }
            if let Some(error) = accept_error {
                return Err(error.into());
            }
            Ok(())
        }
    }
}

struct StopHost(TurnGateway, Duration);
impl Drop for StopHost {
    fn drop(&mut self) {
        if !crate::mutants::on("native-ws-cancel-leak") {
            self.0.request_shutdown(self.1);
        }
    }
}

// Resource safety differences from TS's unbounded custom WS implementation.
const MAX_INPUT_PAYLOAD: usize = 100 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_FRAMES: usize = 64;
const MAX_PENDING_REQUESTS: usize = 64;
const CLOSE_BUDGET: Duration = Duration::from_secs(1);

enum Output {
    Frame(Vec<u8>, OwnedSemaphorePermit),
    Close(Vec<u8>, OwnedSemaphorePermit),
    End,
}
struct Text {
    output: mpsc::Sender<Output>,
    bytes: Arc<Semaphore>,
    closing: watch::Sender<Option<tokio::time::Instant>>,
    // Linearize close/send callbacks on the host without blocking that host.
    closed: Mutex<bool>,
}
impl Text {
    fn channel() -> (
        Arc<Self>,
        mpsc::Receiver<Output>,
        watch::Receiver<Option<tokio::time::Instant>>,
    ) {
        let slots = if crate::mutants::on("native-ws-output-slots") {
            65536
        } else {
            MAX_OUTPUT_FRAMES
        };
        let bytes = if crate::mutants::on("native-ws-output-bytes") {
            1024 * MAX_OUTPUT_BYTES
        } else {
            MAX_OUTPUT_BYTES
        };
        let (output, outgoing) = mpsc::channel(slots);
        let (closing, closing_rx) = watch::channel(None);
        let text = Arc::new(Self {
            output,
            bytes: Arc::new(Semaphore::new(bytes)),
            closing,
            closed: Mutex::new(false),
        });
        (text, outgoing, closing_rx)
    }
    fn deadline(&self, immediate: bool) {
        let candidate = tokio::time::Instant::now()
            + if immediate {
                Duration::ZERO
            } else {
                CLOSE_BUDGET
            };
        self.closing.send_if_modified(|current| {
            if current.is_none_or(|old| candidate < old)
                || crate::mutants::on("native-ws-reset-close-deadline")
            {
                *current = Some(candidate);
                true
            } else {
                false
            }
        });
    }

    // Reserve both dimensions before frame encoding. A permit stays attached
    // through write_all, so dequeue does not hide a blocked in-flight frame.
    fn queue(&self, opcode: u8, payload: &[u8], close: bool) {
        let length = payload
            .len()
            .checked_add(10)
            .and_then(|n| u32::try_from(n).ok());
        let permit = length.and_then(|n| self.bytes.clone().try_acquire_many_owned(n).ok());
        let slot = self.output.try_reserve();
        if let (Some(permit), Ok(slot)) = (permit, slot) {
            let wire = frame::server_frame(opcode, payload);
            slot.send(if close {
                Output::Close(wire, permit)
            } else {
                Output::Frame(wire, permit)
            });
        } else {
            // A full queue must not swallow the shutdown signal. The peer may
            // not read, so force close without promising a deliverable frame.
            if !close || !crate::mutants::on("native-ws-full-close-signal") {
                self.deadline(true);
            }
        }
    }
    fn ping(&self, payload: &[u8]) {
        // TS replies to ping even after application close within a data batch.
        self.queue(10, payload, false);
    }
    fn end(&self) {
        *self.closed.lock().expect("native text state") = true;
        self.deadline(false);
        let _ = self.output.try_send(Output::End);
    }
}
impl GatewayTextConnection for Text {
    fn send_text(&self, message: String) {
        let closed = self.closed.lock().expect("native text state");
        if !*closed && self.closing.borrow().is_none() {
            self.queue(1, message.as_bytes(), false);
        }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut closed = self.closed.lock().expect("native text state");
        if !*closed {
            *closed = true;
            self.deadline(false);
            let mut payload = code.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            self.queue(8, &payload, true);
        }
    }
}

async fn closing_deadline(mut closing: watch::Receiver<Option<tokio::time::Instant>>) {
    loop {
        let deadline = *closing.borrow_and_update();
        if let Some(deadline) = deadline {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return,
                result = closing.changed() => if result.is_err() { return; },
            }
        } else if closing.changed().await.is_err() {
            return;
        }
    }
}

async fn serve_socket(
    socket: TcpStream,
    host: TurnGateway,
    token: String,
    version: String,
) -> Result<(), ProxyError> {
    let (upgrade, upgraded) = oneshot::channel();
    let upgrade = Mutex::new(Some(upgrade));
    http_proxy::serve_connection_with(socket, |mut request, lease| {
        let response = if http_proxy::is_upgrade(&request) {
            match (
                request.uri().path(),
                acceptor::websocket_accept(request.headers()),
                lease,
            ) {
                ("/ws", Some(key), Some(lease)) => {
                    if let Some(upgrade) = upgrade.lock().expect("native upgrade state").take() {
                        let _ = upgrade.send((hyper::upgrade::on(&mut request), lease));
                    }
                    Ok(acceptor::websocket_response(Default::default(), &key))
                }
                _ if crate::mutants::on("native-ws-invalid-upgrade") => {
                    Ok(acceptor::websocket_response(Default::default(), "invalid"))
                }
                _ => Err(std::io::Error::other("invalid native WS upgrade")),
            }
        } else {
            let mut response = hyper::Response::new(http_proxy::empty());
            *response.status_mut() = hyper::StatusCode::NOT_FOUND;
            Ok(response)
        };
        std::future::ready(response)
    })
    .await?;
    drop(upgrade);
    let Ok((upgraded, lease)) = upgraded.await else {
        return Ok(());
    };
    let socket = http_proxy::upgraded_stream::<TcpStream>(upgraded.await?)?;
    drop(lease);
    transport(socket, host, token, version).await
}

// Poll the host completion futures directly: a finished host job must not
// occupy a slot just because a spawned local waiter has not been scheduled.
async fn reap_requests<F: Future + Unpin>(requests: &mut Vec<F>, wait: bool) {
    std::future::poll_fn(|cx| {
        let mut completed = false;
        let mut index = 0;
        while index < requests.len() {
            if std::pin::Pin::new(&mut requests[index]).poll(cx).is_ready() {
                drop(requests.swap_remove(index));
                completed = true;
            } else {
                index += 1;
            }
        }
        if !wait || completed || requests.is_empty() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

fn input_capacity(buffered: usize, available: usize) -> usize {
    let remaining = if crate::mutants::on("native-ws-buffer-limit") {
        usize::MAX
    } else {
        (MAX_INPUT_PAYLOAD + 14).saturating_sub(buffered)
    };
    available.min(remaining)
}

async fn transport<S>(
    socket: S,
    host: TurnGateway,
    token: String,
    version: String,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (text, mut outgoing, closing_rx) = Text::channel();
    let connection = host.connect(text.clone(), token, version);
    let mut writes = JoinSet::new();
    let writer_closing = closing_rx.clone();
    writes.spawn(async move {
        let write = async {
            while let Some(item) = outgoing.recv().await {
                match item {
                    Output::Frame(wire, permit) => {
                        writer.write_all(&wire).await?;
                        if crate::mutants::on("native-ws-leak-output-permit") { std::mem::forget(permit); }
                    },
                    Output::Close(wire, _permit) => {
                        writer.write_all(&wire).await?;
                        break;
                    }
                    Output::End => break,
                }
            }
            writer.shutdown().await
        };
        tokio::select! {
            result = write => result,
            _ = closing_deadline(writer_closing), if !crate::mutants::on("native-ws-writer-deadline") =>
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "native WS writer close deadline exceeded")),
        }
    });
    let mut requests = Vec::new();
    let mut decoder = Decoder::default();
    let mut input = [0; 8192];
    let mut writer_done = false;
    let reap_completed = !crate::mutants::on("native-ws-retain-completed-requests");
    let max_payload = if crate::mutants::on("native-ws-input-limit") {
        u64::MAX
    } else {
        MAX_INPUT_PAYLOAD as u64
    };
    let result: Result<(), ProxyError> = async {
        loop {
            // TS handleData starts on each new data callback. A protocol close
            // ends that batch, but leaves buffered tail bytes for the next one.
            let capacity = input_capacity(decoder.buffer.len(), input.len());
            if capacity == 0 {
                text.close(1009, "frame too large");
                break;
            }
            let count = tokio::select! {
                result = writes.join_next(), if !writer_done => {
                    if let Some(result) = result { result??; }
                    writer_done = true;
                    continue;
                }
                _ = closing_deadline(closing_rx.clone()), if !crate::mutants::on("native-ws-reader-deadline") => {
                    return Err("native WS connection close deadline exceeded".into());
                }
                read = reader.read(&mut input[..capacity]) => read?,
                _ = reap_requests(&mut requests, true), if reap_completed && !requests.is_empty() => continue,
            };
            if count == 0 {
                break;
            }
            decoder.buffer.extend_from_slice(&input[..count]);
            loop {
                let frame = match decoder.next_limited(max_payload) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(frame::DecodeError::TooLarge) => {
                        let code = if crate::mutants::on("native-ws-input-close-code") { 1000 } else { 1009 };
                        text.close(code, "frame too large");
                        break;
                    }
                    Err(error) => return Err(std::io::Error::other(format!("invalid WS frame: {error:?}")).into()),
                };
                match acceptor::action(&frame) {
                    acceptor::Action::Text => {
                        if reap_completed { reap_requests(&mut requests, false).await; }
                        if requests.len() >= MAX_PENDING_REQUESTS && !crate::mutants::on("native-ws-pending-limit") {
                            text.deadline(true);
                            return Err("native WS pending request limit exceeded".into());
                        }
                        let message = String::from_utf8_lossy(&frame.payload).into_owned();
                        if !crate::mutants::on("native-ws-drop-text") {
                            requests
                                .push(connection.enqueue(message).await.map_err(request_error)?);
                        }
                        if crate::mutants::on("native-ws-drop-application-tail")
                            && *text.closed.lock().expect("native text state")
                        {
                            break;
                        }
                    }
                    acceptor::Action::Ping => {
                        text.ping(&frame.payload);
                    }
                    acceptor::Action::Close => {
                        text.close(1000, "");
                        if !crate::mutants::on("native-ws-after-close") {
                            break;
                        }
                    }
                    acceptor::Action::Ignore => {}
                }
            }
            if crate::mutants::on("native-ws-drop-closed-read")
                && *text.closed.lock().expect("native text state")
            {
                break;
            }
            if crate::mutants::on("native-ws-writer-blocks-read") && !requests.is_empty() {
                if let Some(written) = writes.join_next().await {
                    written??;
                }
                break;
            }
        }
        Ok(())
    }
    .await;
    drop(decoder);
    // Release the socket reader independently of uncompleted business jobs.
    let retained_reader = if crate::mutants::on("native-ws-retain-reader") {
        Some(reader)
    } else {
        drop(reader);
        None
    };
    text.end();
    // The writer's own deadline drops its socket half even when the host's
    // disconnect/owned requests never finish. The server bounds business drain.
    let closed = if crate::mutants::on("native-ws-skip-disconnect") {
        Ok(())
    } else {
        connection.closed().await
    };
    while !requests.is_empty() {
        reap_requests(&mut requests, true).await;
    }
    while let Some(written) = writes.join_next().await {
        written??;
    }
    result?;
    drop(retained_reader);
    closed.map_err(request_error)?;
    Ok(())
}
fn request_error(error: crate::runtime::connection::RequestError) -> std::io::Error {
    std::io::Error::other(String::from_utf16_lossy(error.message.units()))
}

#[cfg(test)]
mod tests;
