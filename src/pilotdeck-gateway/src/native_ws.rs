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
    sync::{mpsc, oneshot},
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
            let drained = tokio::time::timeout(drain_budget, async {
                while connections.join_next().await.is_some() {}
            })
            .await
            .is_ok();
            // Aborting transport waiters never cancels business jobs on the serial
            // host. Its shutdown cancels owned turns and waits for finally fences.
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            host.shutdown(drain_budget)
                .await
                .map_err(std::io::Error::other)?;
            if !drained {
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

enum Output {
    Frame(Vec<u8>),
    Close(Vec<u8>),
    End,
}
struct Text {
    output: mpsc::UnboundedSender<Output>,
    // A single lock linearizes close with send_text, including callbacks on
    // the host. No application frames are queued after the close frame.
    closed: Mutex<bool>,
}
impl GatewayTextConnection for Text {
    fn send_text(&self, message: String) {
        let closed = self.closed.lock().expect("native text state");
        if !*closed {
            let _ = self
                .output
                .send(Output::Frame(frame::server_frame(1, message.as_bytes())));
        }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut closed = self.closed.lock().expect("native text state");
        if !*closed {
            *closed = true;
            let mut payload = code.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            let _ = self
                .output
                .send(Output::Close(frame::server_frame(8, &payload)));
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
    let (output, mut outgoing) = mpsc::unbounded_channel();
    let text = Arc::new(Text {
        output,
        closed: Mutex::new(false),
    });
    let connection = host.connect(text.clone(), token, version);
    let mut writes = JoinSet::new();
    writes.spawn(async move {
        while let Some(item) = outgoing.recv().await {
            match item {
                Output::Frame(wire) => writer.write_all(&wire).await?,
                Output::Close(wire) => {
                    writer.write_all(&wire).await?;
                    break;
                }
                Output::End => break,
            }
        }
        writer.shutdown().await
    });
    let mut requests = JoinSet::new();
    let mut decoder = Decoder::default();
    let mut input = [0; 8192];
    let mut writer_done = false;
    let result: Result<(), ProxyError> = async {
        loop {
            // TS handleData starts on each new data callback. A protocol close
            // ends that batch, but leaves buffered tail bytes for the next one.
            let count = tokio::select! {
                result = writes.join_next(), if !writer_done => {
                    if let Some(result) = result { result??; }
                    writer_done = true;
                    continue;
                }
                read = reader.read(&mut input) => read?,
                _ = requests.join_next(), if !requests.is_empty() => continue,
            };
            if count == 0 {
                break;
            }
            decoder.buffer.extend_from_slice(&input[..count]);
            loop {
                let Some(frame) = decoder
                    .next()
                    .map_err(|e| std::io::Error::other(format!("invalid WS frame: {e:?}")))?
                else {
                    break;
                };
                match acceptor::action(&frame) {
                    acceptor::Action::Text => {
                        let message = String::from_utf8_lossy(&frame.payload).into_owned();
                        if !crate::mutants::on("native-ws-drop-text") {
                            requests
                                .spawn(connection.enqueue(message).await.map_err(request_error)?);
                        }
                        if crate::mutants::on("native-ws-drop-application-tail")
                            && *text.closed.lock().expect("native text state")
                        {
                            break;
                        }
                    }
                    acceptor::Action::Ping => {
                        let _ = text
                            .output
                            .send(Output::Frame(frame::server_frame(10, &frame.payload)));
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
    // Release the socket reader independently of uncompleted business jobs.
    let retained_reader = if crate::mutants::on("native-ws-retain-reader") {
        Some(reader)
    } else {
        drop(reader);
        None
    };
    *text.closed.lock().expect("native text state") = true;
    let _ = text.output.send(Output::End);
    // The independent writer may be blocked; close/abort still reaches the
    // serial host. A server drain deadline owns transport cancellation.
    let closed = if crate::mutants::on("native-ws-skip-disconnect") {
        Ok(())
    } else {
        connection.closed().await
    };
    while requests.join_next().await.is_some() {}
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
