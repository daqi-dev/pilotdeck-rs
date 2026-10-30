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
    resources: Resources,
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
            resources: Resources::new(
                if crate::mutants::on("ws-caps-default-connections") {
                    1
                } else {
                    MAX_CONNECTIONS
                },
                if crate::mutants::on("ws-caps-default-input") {
                    1
                } else {
                    MAX_TOTAL_INPUT_BYTES
                },
            ),
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
                resources,
            } = self;
            let mut connections = JoinSet::new();
            tokio::pin!(stop);
            let accept_error = loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => break None,
                    // Reap before accept: a permanently ready listener must not
                    // accumulate completed task records outside the live cap.
                    _ = connections.join_next(), if !connections.is_empty() && !crate::mutants::on("ws-caps-unreaped-records") => {},
                    accepted = listener.accept() => match accepted {
                        Ok((socket, _)) => {
                            let Ok(permit) = resources.connections.clone().try_acquire_owned() else {
                                // No task/HTTP parser is created for an excess peer.
                                drop(socket);
                                continue;
                            };
                            let host = host.clone();
                            let token = token.clone();
                            let version = version.clone();
                            let input = resources.input.clone();
                            connections.spawn(async move {
                                let _permit = if crate::mutants::on("ws-caps-early-connection-release") {
                                    drop(permit);
                                    None
                                } else {
                                    Some(permit)
                                };
                                if let Err(error) = serve_socket(socket, host, token, version, input).await {
                                    eprintln!("native WS connection: {error}");
                                }
                                if crate::mutants::on("ws-caps-connection-leak") {
                                    std::mem::forget(_permit);
                                }
                            });
                            #[cfg(test)]
                            resources.task_high_watermark.fetch_max(connections.len(), std::sync::atomic::Ordering::SeqCst);
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
const MAX_CONNECTIONS: usize = 64;
const MAX_TOTAL_INPUT_BYTES: usize = 256 * 1024 * 1024;

struct Resources {
    connections: Arc<Semaphore>,
    input: Arc<Semaphore>,
    #[cfg(test)]
    task_high_watermark: Arc<std::sync::atomic::AtomicUsize>,
}
impl Resources {
    fn new(connections: usize, input: usize) -> Self {
        Self {
            connections: Arc::new(Semaphore::new(
                if crate::mutants::on("ws-caps-connections") {
                    connections + 1
                } else {
                    connections
                },
            )),
            input: Arc::new(Semaphore::new(input)),
            #[cfg(test)]
            task_high_watermark: Arc::default(),
        }
    }
}

/// Charge backing storage, not just unread length: Decoder::split_to may
/// retain the original allocation after a frame is consumed. Declaration order
/// drops the decoder before its lease, including task cancellation and errors.
struct InputBuffer {
    decoder: Decoder,
    allocation: Option<OwnedSemaphorePermit>,
    budget: Arc<Semaphore>,
}
impl InputBuffer {
    fn new(budget: Arc<Semaphore>) -> Self {
        Self {
            decoder: Decoder::default(),
            allocation: None,
            budget,
        }
    }

    fn append(&mut self, bytes: &[u8]) -> bool {
        let needed = self.decoder.buffer.len() + bytes.len();
        if needed > self.decoder.buffer.capacity() {
            let capacity = needed
                .next_power_of_two()
                .min(MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES);
            // Keep the old allocation charged until the replacement is built
            // and installed. A slow peer never waits for another peer's lease.
            if crate::mutants::on("ws-caps-early-input-refund") {
                self.allocation = None;
            }
            if crate::mutants::on("ws-caps-split-input-refund")
                && self
                    .allocation
                    .as_ref()
                    .is_some_and(|permit| permit.num_permits() > self.decoder.buffer.capacity())
            {
                self.allocation = None;
            }
            let charged = if crate::mutants::on("ws-caps-input-budget") {
                0
            } else {
                capacity
            };
            let Ok(permit) = self.budget.clone().try_acquire_many_owned(charged as u32) else {
                return false;
            };
            let mut buffer = bytes::BytesMut::with_capacity(capacity);
            buffer.extend_from_slice(&self.decoder.buffer);
            self.decoder.buffer = buffer;
            self.allocation = Some(permit);
        }
        self.decoder.buffer.extend_from_slice(bytes);
        true
    }

    fn reclaim_empty(&mut self) {
        if !self.decoder.buffer.is_empty() && crate::mutants::on("ws-caps-refund-tail") {
            self.allocation = None;
        }
        if self.decoder.buffer.is_empty() && !crate::mutants::on("ws-caps-retain-empty") {
            // Called only after the decoded Frame (including its wire) drops.
            self.decoder = Decoder::default();
            self.allocation = None;
        }
    }
}
impl Drop for InputBuffer {
    fn drop(&mut self) {
        if crate::mutants::on("ws-caps-input-drop-leak") {
            std::mem::forget(self.allocation.take());
        }
    }
}
const MAX_OUTPUT_PAYLOAD: usize = MAX_INPUT_PAYLOAD;
const MAX_OUTPUT_BYTES: usize = MAX_OUTPUT_PAYLOAD + frame::MAX_SERVER_HEADER_BYTES;
const MAX_OUTPUT_FRAMES: usize = 64;
const MAX_PENDING_REQUESTS: usize = 64;
// TS has no flush timer. Bound our resource-safety difference with scheduling
// slack plus one second per remaining MiB, including the in-flight frame.
// A full 100 MiB frame + header receives 131s, not a fixed 1s deadline.
const CLOSE_BUDGET: Duration = Duration::from_secs(30);
const CLOSE_BYTES_PER_SECOND: usize = 1024 * 1024;

enum Output {
    Frame(Vec<u8>, OwnedSemaphorePermit),
    Close(Vec<u8>),
    End,
}
struct Text {
    output: mpsc::Sender<Output>,
    bytes: Arc<Semaphore>,
    end_slot: Mutex<Option<mpsc::OwnedPermit<Output>>>,
    close_slot: Mutex<Option<mpsc::OwnedPermit<Output>>>,
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
        // These permits cannot be consumed by ordinary frames. Keep them until
        // the one Close and the one End have been enqueued, in FIFO order.
        let (output, outgoing) = mpsc::channel(slots + 2);
        let end_slot = output.clone().try_reserve_owned().expect("new End slot");
        let close_slot = output.clone().try_reserve_owned().expect("new Close slot");
        let (closing, closing_rx) = watch::channel(None);
        let text = Arc::new(Self {
            output,
            end_slot: Mutex::new(Some(end_slot)),
            close_slot: Mutex::new(Some(close_slot)),
            bytes: Arc::new(Semaphore::new(bytes)),
            closing,
            closed: Mutex::new(false),
        });
        (text, outgoing, closing_rx)
    }
    fn close_budget(&self) -> Duration {
        if crate::mutants::on("native-ws-short-close-budget") {
            return Duration::from_secs(1);
        }
        if crate::mutants::on("native-ws-ignore-flush-bytes") {
            return CLOSE_BUDGET;
        }
        let accepted = MAX_OUTPUT_BYTES.saturating_sub(self.bytes.available_permits());
        CLOSE_BUDGET + Duration::from_secs(accepted.div_ceil(CLOSE_BYTES_PER_SECOND) as u64)
    }
    fn deadline(&self, immediate: bool) {
        let candidate = tokio::time::Instant::now()
            + if immediate {
                Duration::ZERO
            } else {
                self.close_budget()
            };
        self.closing.send_if_modified(|current| {
            if current.is_none_or(|old| {
                (immediate || crate::mutants::on("native-ws-shorten-eof-deadline"))
                    && candidate < old
            }) || crate::mutants::on("native-ws-reset-close-deadline")
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
    fn queue(&self, opcode: u8, payload: &[u8]) {
        let max_payload = if crate::mutants::on("native-ws-small-output-limit") {
            1024 * 1024
        } else {
            MAX_OUTPUT_PAYLOAD
        };
        let length = payload
            .len()
            .checked_add(frame::MAX_SERVER_HEADER_BYTES)
            .and_then(|n| u32::try_from(n).ok());
        let permit = length.and_then(|n| self.bytes.clone().try_acquire_many_owned(n).ok());
        let slot = self.output.try_reserve();
        if let (Some(permit), Ok(slot), true) = (permit, slot, payload.len() <= max_payload) {
            slot.send(Output::Frame(frame::server_frame(opcode, payload), permit));
        } else {
            // Sync host callbacks cannot await byte capacity. Reject overload
            // with a reserved close frame; a non-reading peer still has a deadline.
            self.queue_close(1009, "output limit exceeded");
        }
    }
    fn queue_close(&self, code: u16, reason: &str) {
        if self.output.capacity() == 0 && crate::mutants::on("native-ws-full-close-signal") {
            return;
        }
        self.deadline(false);
        if let Some(slot) = self.close_slot.lock().expect("native close slot").take() {
            let mut payload = code.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            let wire = frame::server_frame(8, &payload);
            if !crate::mutants::on("native-ws-drop-output-close") {
                slot.send(Output::Close(wire));
            }
        }
    }
    fn ping(&self, payload: &[u8]) {
        // TS replies to ping even after application close within a data batch.
        self.queue(10, payload);
    }
    fn end(&self) {
        *self.closed.lock().expect("native text state") = true;
        self.deadline(false);
        if crate::mutants::on("native-ws-drop-full-end") {
            let _ = self.output.try_send(Output::End);
        } else if let Some(slot) = self.end_slot.lock().expect("native end slot").take() {
            slot.send(Output::End);
        }
    }
}
impl GatewayTextConnection for Text {
    fn send_text(&self, message: String) {
        let closed = self.closed.lock().expect("native text state");
        if !*closed && self.closing.borrow().is_none() {
            self.queue(1, message.as_bytes());
        }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut closed = self.closed.lock().expect("native text state");
        if !*closed {
            *closed = true;
            self.queue_close(code, reason);
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
    input: Arc<Semaphore>,
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
    transport_with_budget(socket, host, token, version, input).await
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
        (MAX_INPUT_PAYLOAD + frame::MAX_CLIENT_HEADER_BYTES).saturating_sub(buffered)
    };
    available.min(remaining)
}

// The saturated state is defensive: an ordinary complete valid frame has
// already been consumed by the inner decode loop before the next read.
fn read_capacity(text: &Text, buffered: usize, available: usize) -> usize {
    let capacity = input_capacity(buffered, available);
    if capacity == 0 {
        let code = if crate::mutants::on("native-ws-buffer-close-code") {
            1000
        } else {
            1009
        };
        text.close(code, "frame too large");
    }
    capacity
}

async fn finish_write(
    write: impl Future<Output = std::io::Result<()>>,
    closing: watch::Receiver<Option<tokio::time::Instant>>,
) -> std::io::Result<()> {
    let timed_out = || {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "native WS writer close deadline exceeded",
        )
    };
    tokio::select! {
        biased;
        // Deterministically invert precedence only in the fixture mutant.
        _ = closing_deadline(closing.clone()), if crate::mutants::on("native-ws-writer-priority") => Err(timed_out()),
        result = write => result,
        _ = closing_deadline(closing), if !crate::mutants::on("native-ws-writer-deadline") => Err(timed_out()),
    }
}

async fn write_output<W: tokio::io::AsyncWrite + Unpin>(
    mut writer: W,
    mut outgoing: mpsc::Receiver<Output>,
    closing: watch::Receiver<Option<tokio::time::Instant>>,
) -> std::io::Result<()> {
    finish_write(
        async {
            while let Some(item) = outgoing.recv().await {
                match item {
                    Output::Frame(wire, permit) => {
                        if crate::mutants::on("native-ws-cancel-leaks-permit") {
                            std::mem::forget(permit);
                            writer.write_all(&wire).await?;
                        } else {
                            writer.write_all(&wire).await?;
                            if crate::mutants::on("native-ws-leak-output-permit") {
                                std::mem::forget(permit);
                            }
                        }
                    }
                    Output::Close(wire) => {
                        writer.write_all(&wire).await?;
                        break;
                    }
                    Output::End => break,
                }
            }
            writer.shutdown().await
        },
        closing,
    )
    .await
}

#[cfg(test)]
async fn transport<S>(
    socket: S,
    host: TurnGateway,
    token: String,
    version: String,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    transport_with_budget(
        socket,
        host,
        token,
        version,
        Arc::new(Semaphore::new(MAX_TOTAL_INPUT_BYTES)),
    )
    .await
}

async fn transport_with_budget<S>(
    socket: S,
    host: TurnGateway,
    token: String,
    version: String,
    input_budget: Arc<Semaphore>,
) -> Result<(), ProxyError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(socket);
    let (text, outgoing, closing_rx) = Text::channel();
    let connection = host.connect(text.clone(), token, version);
    let mut writes = JoinSet::new();
    writes.spawn(write_output(writer, outgoing, closing_rx.clone()));
    let mut requests = Vec::new();
    let mut buffered = InputBuffer::new(input_budget);
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
            let capacity = read_capacity(&text, buffered.decoder.buffer.len(), input.len());
            if capacity == 0 {
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
            if !buffered.append(&input[..count]) {
                let code = if crate::mutants::on("ws-caps-input-close-code") { 1000 } else { 1009 };
                text.close(code, "aggregate input limit exceeded");
                break;
            }
            loop {
                let frame = match buffered.decoder.next_limited(max_payload) {
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
            buffered.reclaim_empty();
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
    let retained_input = if crate::mutants::on("ws-caps-retain-input-during-drain") {
        Some(buffered)
    } else {
        drop(buffered);
        None
    };
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
    drop(retained_input);
    drop(retained_reader);
    closed.map_err(request_error)?;
    Ok(())
}
fn request_error(error: crate::runtime::connection::RequestError) -> std::io::Error {
    std::io::Error::other(String::from_utf16_lossy(error.message.units()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod caps_tests;
