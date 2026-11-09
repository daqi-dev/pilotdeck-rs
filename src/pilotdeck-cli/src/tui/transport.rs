//! One bounded WebSocket owner for TUI streams and concurrent interaction RPCs.
use futures_util::{
    SinkExt, StreamExt,
    future::{Either, select},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config, tungstenite::Message,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Result<T> = std::result::Result<T, Error>;
const COMMANDS: usize = 32;
const PENDING: usize = 64;
const STREAMS: usize = 16;
const EVENTS: usize = 64;
const QUEUED_BYTES: usize = 4 * 1024 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub enum Error {
    Connection(String),
    Gateway(JsValue),
    ResourceLimit,
}
fn closed() -> Error {
    Error::Connection("Gateway WebSocket closed.".into())
}
struct Event {
    value: JsValue,
    _permit: OwnedSemaphorePermit,
}
struct Reply {
    result: Result<JsValue>,
    _permit: OwnedSemaphorePermit,
}
struct Terminal {
    error: Error,
    _permit: Option<OwnedSemaphorePermit>,
}
struct StreamSink {
    sender: mpsc::Sender<Event>,
    terminal: Arc<Mutex<Option<Terminal>>>,
}
pub struct EventStream {
    receiver: mpsc::Receiver<Event>,
    terminal: Arc<Mutex<Option<Terminal>>>,
}
impl EventStream {
    pub async fn next(&mut self) -> Option<Result<JsValue>> {
        match self.receiver.recv().await {
            Some(event) => Some(Ok(event.value)),
            None => self
                .terminal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .map(|terminal| Err(terminal.error)),
        }
    }
}
enum Command {
    Request {
        id: String,
        frame: String,
        _permit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<Reply>>,
    },
    Stream {
        id: String,
        frame: String,
        _permit: OwnedSemaphorePermit,
        reply: oneshot::Sender<Result<EventStream>>,
    },
    Close {
        reply: oneshot::Sender<Result<()>>,
    },
}
#[derive(Clone)]
pub struct Client {
    sender: mpsc::Sender<Command>,
    budget: Arc<Semaphore>,
}
fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn equals(value: &JsValue, expected: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(expected))
}
fn encoded(value: &JsValue) -> Result<String> {
    stringify_json(value)
        .map_err(|e| Error::Connection(e.to_string()))?
        .ok_or_else(|| Error::Connection("Invalid Gateway frame.".into()))
}
struct LocalFrame {
    text: String,
    _permit: OwnedSemaphorePermit,
}
struct LocalSink {
    sender: mpsc::Sender<LocalFrame>,
    bytes: Arc<Semaphore>,
    failed: watch::Sender<Option<Error>>,
}
impl pilotdeck_gateway::runtime::connection::GatewayTextConnection for LocalSink {
    fn send_text(&self, text: String) {
        let size = u32::try_from(text.len().max(1)).unwrap_or(u32::MAX);
        let Ok(permit) = self.bytes.clone().try_acquire_many_owned(size) else {
            self.failed.send_replace(Some(Error::ResourceLimit));
            return;
        };
        if self
            .sender
            .try_send(LocalFrame {
                text,
                _permit: permit,
            })
            .is_err()
        {
            self.failed.send_replace(Some(Error::ResourceLimit));
        }
    }
    fn close(&self, _: u16, _: &str) {
        self.failed.send_replace(Some(closed()));
    }
}
enum Endpoint {
    WebSocket(Box<Socket>),
    Hosted {
        host: pilotdeck_gateway::native_process::LocalHost,
        connection: pilotdeck_gateway::runtime::turn::gateway::HostedConnection,
        output: mpsc::Receiver<LocalFrame>,
        failed: watch::Receiver<Option<Error>>,
        fail_tx: watch::Sender<Option<Error>>,
        jobs: Arc<Semaphore>,
    },
}
impl Endpoint {
    async fn send(&mut self, frame: String, permit: OwnedSemaphorePermit) -> Result<()> {
        match self {
            Self::WebSocket(socket) => {
                let result = send(socket, frame).await;
                drop(permit);
                result
            }
            Self::Hosted {
                connection,
                jobs,
                fail_tx,
                ..
            } => {
                let job = jobs
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Error::ResourceLimit)?;
                let completion = timeout(
                    SEND_TIMEOUT,
                    connection.enqueue_with_input_and_job_permit(frame, Some(permit), Some(job)),
                )
                .await
                .map_err(|_| Error::Connection("Gateway send timed out.".into()))?
                .map_err(|e| Error::Connection(String::from_utf16_lossy(e.message.units())))?;
                let failure = fail_tx.clone();
                tokio::spawn(async move {
                    if completion.await.is_err() {
                        failure.send_replace(Some(closed()));
                    }
                });
                Ok(())
            }
        }
    }
    async fn next(
        &mut self,
    ) -> Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> {
        match self {
            Self::WebSocket(socket) => socket.next().await,
            Self::Hosted { output, failed, .. } => {
                if failed.borrow().is_some() {
                    return None;
                }
                tokio::select! {
                    item = output.recv() => item.map(|v| Ok(Message::Text(v.text.into()))),
                    _ = failed.changed() => None,
                }
            }
        }
    }
    async fn flush(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(socket) => flush(socket).await,
            Self::Hosted { .. } => Ok(()),
        }
    }
    async fn close(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(socket) => {
                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                Ok(())
            }
            Self::Hosted {
                host, connection, ..
            } => {
                let closed_result = timeout(SEND_TIMEOUT, connection.closed()).await;
                let shutdown_result = host.shutdown().await;
                shutdown_result.map_err(Error::Connection)?;
                closed_result
                    .map_err(|_| Error::Connection("Gateway close timed out.".into()))?
                    .map_err(|e| Error::Connection(String::from_utf16_lossy(e.message.units())))
            }
        }
    }
    fn failure(&self) -> Error {
        match self {
            Self::Hosted { failed, .. } => failed.borrow().clone().unwrap_or_else(closed),
            _ => closed(),
        }
    }
}
async fn send(socket: &mut Socket, frame: String) -> Result<()> {
    timeout(SEND_TIMEOUT, socket.send(Message::Text(frame.into())))
        .await
        .map_err(|_| Error::Connection("Gateway send timed out.".into()))?
        .map_err(|_| closed())
}
async fn flush(socket: &mut Socket) -> Result<()> {
    timeout(SEND_TIMEOUT, socket.flush())
        .await
        .map_err(|_| Error::Connection("Gateway flush timed out.".into()))?
        .map_err(|_| closed())
}
async fn within_hello<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let expired = || Error::Connection("Gateway hello timed out.".into());
    if Instant::now() >= deadline {
        return Err(expired());
    }
    let value = timeout_at(deadline, future)
        .await
        .map_err(|_| expired())??;
    if Instant::now() >= deadline {
        return Err(expired());
    }
    Ok(value)
}
impl Client {
    /// Connect to an already discovered URL with an explicitly supplied token.
    pub async fn connect(url: &str, token: &str) -> Result<Self> {
        let (mut socket, _) =
            connect_async_with_config(url, Some(crate::ws_config::websocket_config()), false)
                .await
                .map_err(|_| closed())?;
        let hello = object(vec![
            ("type", "hello".into()),
            (
                "protocolVersion",
                pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into(),
            ),
            ("clientName", "cli".into()),
            ("clientVersion", "0.1.0".into()),
            ("token", token.into()),
        ]);
        send(&mut socket, encoded(&hello)?).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Connection("Gateway hello timed out.".into()));
            }
            let message = within_hello(deadline, async {
                socket
                    .next()
                    .await
                    .ok_or_else(closed)?
                    .map_err(|_| closed())
            })
            .await?;
            if message.is_close() {
                return Err(closed());
            }
            if message.is_ping() {
                within_hello(deadline, flush(&mut socket)).await?;
                continue;
            }
            if let Message::Text(raw) = message {
                let value = parse_json(&raw)
                    .map_err(|_| Error::Connection("Invalid Gateway frame.".into()))?;
                if Instant::now() >= deadline {
                    return Err(Error::Connection("Gateway hello timed out.".into()));
                }
                if equals(&field(&value, "type"), "hello_ok") {
                    break;
                }
            }
        }
        let (sender, receiver) = mpsc::channel(COMMANDS);
        let budget = Arc::new(Semaphore::new(QUEUED_BYTES));
        tokio::spawn(pump(
            Endpoint::WebSocket(Box::new(socket)),
            receiver,
            Arc::clone(&budget),
        ));
        Ok(Self { sender, budget })
    }
    /// Attach to the in-process native owner without a listener or token file.
    pub async fn local(
        options: pilotdeck_gateway::native_process::LocalHostOptions,
        env: pilotdeck_gateway::config::Env,
    ) -> Result<Self> {
        let host = pilotdeck_gateway::native_process::LocalHost::start(options, env)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        let (out_tx, mut output) = mpsc::channel(EVENTS);
        let (fail_tx, failed) = watch::channel(None);
        let sink = Arc::new(LocalSink {
            sender: out_tx,
            bytes: Arc::new(Semaphore::new(QUEUED_BYTES)),
            failed: fail_tx.clone(),
        });
        let connection = match host.connect(sink, "cli").await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = host.shutdown().await;
                return Err(Error::Connection(String::from_utf16_lossy(
                    error.message.units(),
                )));
            }
        };
        let hello = match timeout(SEND_TIMEOUT, output.recv()).await {
            Ok(Some(first)) => parse_json(&first.text).ok(),
            _ => None,
        };
        if !hello
            .as_ref()
            .is_some_and(|hello| equals(&field(hello, "type"), "hello_ok"))
        {
            let _ = timeout(SEND_TIMEOUT, connection.closed()).await;
            let _ = host.shutdown().await;
            return Err(closed());
        }
        let (sender, receiver) = mpsc::channel(COMMANDS);
        let budget = Arc::new(Semaphore::new(QUEUED_BYTES));
        tokio::spawn(pump(
            Endpoint::Hosted {
                host,
                connection,
                output,
                failed,
                fail_tx,
                jobs: Arc::new(Semaphore::new(PENDING)),
            },
            receiver,
            budget.clone(),
        ));
        Ok(Self { sender, budget })
    }
    fn frame(
        &self,
        method: &str,
        params: JsValue,
    ) -> Result<(String, String, OwnedSemaphorePermit)> {
        let id = uuid::Uuid::new_v4().to_string();
        let frame = encoded(&object(vec![
            ("type", "request".into()),
            ("id", id.as_str().into()),
            ("method", method.into()),
            ("params", params),
        ]))?;
        let size = u32::try_from(frame.len().max(1)).map_err(|_| Error::ResourceLimit)?;
        let permit = Arc::clone(&self.budget)
            .try_acquire_many_owned(size)
            .map_err(|_| Error::ResourceLimit)?;
        Ok((id, frame, permit))
    }
    /// Sends an ordinary RPC. Response errors retain the server envelope.
    pub async fn request(&self, method: &str, params: JsValue) -> Result<JsValue> {
        let (id, frame, permit) = self.frame(method, params)?;
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Command::Request {
                id,
                frame,
                _permit: permit,
                reply,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::ResourceLimit,
                mpsc::error::TrySendError::Closed(_) => closed(),
            })?;
        response.await.map_err(|_| closed())??.result
    }
    /// Starts a stream without preventing concurrent permission/abort requests.
    pub async fn stream(&self, method: &str, params: JsValue) -> Result<EventStream> {
        let (id, frame, permit) = self.frame(method, params)?;
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Command::Stream {
                id,
                frame,
                _permit: permit,
                reply,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::ResourceLimit,
                mpsc::error::TrySendError::Closed(_) => closed(),
            })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn wait_closed(&self) {
        self.sender.closed().await;
    }
    pub async fn close(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        timeout(SEND_TIMEOUT, self.sender.send(Command::Close { reply }))
            .await
            .map_err(|_| Error::Connection("Gateway close timed out.".into()))?
            .map_err(|_| closed())?;
        timeout(SEND_TIMEOUT * 2 + Duration::from_secs(1), response)
            .await
            .map_err(|_| Error::Connection("Gateway close timed out.".into()))?
            .map_err(|_| closed())?
    }
}
enum Input {
    Command(Option<Command>),
    Socket(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
}
async fn pump(
    mut endpoint: Endpoint,
    mut receiver: mpsc::Receiver<Command>,
    budget: Arc<Semaphore>,
) {
    let mut pending: HashMap<String, oneshot::Sender<Result<Reply>>> = HashMap::new();
    let mut streams: HashMap<String, StreamSink> = HashMap::new();
    let mut prefer_socket = false;
    let failure = loop {
        pending.retain(|_, reply| !reply.is_closed());
        streams.retain(|_, stream| !stream.sender.is_closed());
        // Alternate which ready future wins, so command floods cannot starve ping/events.
        let input = if prefer_socket {
            match select(Box::pin(endpoint.next()), Box::pin(receiver.recv())).await {
                Either::Left((v, _)) => Input::Socket(v),
                Either::Right((v, _)) => Input::Command(v),
            }
        } else {
            match select(Box::pin(receiver.recv()), Box::pin(endpoint.next())).await {
                Either::Left((v, _)) => Input::Command(v),
                Either::Right((v, _)) => Input::Socket(v),
            }
        };
        prefer_socket = !prefer_socket;
        match input {
            Input::Command(Some(Command::Request {
                id,
                frame,
                reply,
                _permit: permit,
            })) => {
                if reply.is_closed() {
                    continue;
                }
                // A waiter may be cancelled while select is asleep, after
                // the loop's initial sweep. Count live owners at admission.
                pending.retain(|_, pending| !pending.is_closed());
                if pending.len() >= PENDING {
                    let _ = reply.send(Err(Error::ResourceLimit));
                    continue;
                }
                if let Err(error) = endpoint.send(frame, permit).await {
                    let _ = reply.send(Err(error.clone()));
                    break error;
                }
                pending.insert(id, reply);
            }
            Input::Command(Some(Command::Stream {
                id,
                frame,
                reply,
                _permit: permit,
            })) => {
                if reply.is_closed() {
                    continue;
                }
                streams.retain(|_, s| !s.sender.is_closed());
                if streams.len() >= STREAMS {
                    let _ = reply.send(Err(Error::ResourceLimit));
                    continue;
                }
                let (sender, receiver) = mpsc::channel(EVENTS);
                let terminal = Arc::new(Mutex::new(None));
                if let Err(error) = endpoint.send(frame, permit).await {
                    let _ = reply.send(Err(error.clone()));
                    break error;
                }
                streams.insert(
                    id,
                    StreamSink {
                        sender,
                        terminal: Arc::clone(&terminal),
                    },
                );
                let _ = reply.send(Ok(EventStream { receiver, terminal }));
            }
            Input::Command(Some(Command::Close { reply })) => {
                let result = endpoint.close().await;
                let _ = reply.send(result);
                break closed();
            }
            Input::Command(None) => {
                let _ = endpoint.close().await;
                break closed();
            }
            Input::Socket(message) => {
                let Some(Ok(message)) = message else {
                    break endpoint.failure();
                };
                if message.is_close() {
                    break closed();
                }
                if message.is_ping() {
                    if let Err(error) = endpoint.flush().await {
                        break error;
                    }
                    continue;
                }
                let Message::Text(raw) = message else {
                    continue;
                };
                let Ok(frame) = parse_json(&raw) else {
                    break Error::Connection("Invalid Gateway frame.".into());
                };
                let id = field(&frame, "id")
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                if equals(&field(&frame, "type"), "response") {
                    if let Some(reply) = pending.remove(&id) {
                        let size = u32::try_from(raw.len().max(1)).unwrap_or(u32::MAX);
                        let Ok(permit) = Arc::clone(&budget).try_acquire_many_owned(size) else {
                            let _ = reply.send(Err(Error::ResourceLimit));
                            break Error::ResourceLimit;
                        };
                        let result = if field(&frame, "ok").as_bool() == Some(true) {
                            Ok(field(&frame, "result"))
                        } else {
                            Err(Error::Gateway(field(&frame, "error")))
                        };
                        let _ = reply.send(Ok(Reply {
                            result,
                            _permit: permit,
                        }));
                    } else if field(&frame, "ok").as_bool() == Some(false)
                        && let Some(stream) = streams.get(&id)
                    {
                        // A completed stream can outlive its map entry. Keep
                        // its error charged until consumed or dropped, too.
                        let size = u32::try_from(raw.len().max(1)).unwrap_or(u32::MAX);
                        let Ok(permit) = Arc::clone(&budget).try_acquire_many_owned(size) else {
                            break Error::ResourceLimit;
                        };
                        *stream.terminal.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(Terminal {
                                error: Error::Gateway(field(&frame, "error")),
                                _permit: Some(permit),
                            });
                        streams.remove(&id);
                    }
                } else if equals(&field(&frame, "type"), "event") {
                    if field(&frame, "final").as_bool() == Some(true) {
                        streams.remove(&id);
                    } else if let Some(stream) = streams.get(&id) {
                        if stream.sender.is_closed() {
                            streams.remove(&id);
                            continue;
                        }
                        let size = u32::try_from(raw.len().max(1)).unwrap_or(u32::MAX);
                        let Ok(permit) = Arc::clone(&budget).try_acquire_many_owned(size) else {
                            break Error::ResourceLimit;
                        };
                        match stream.sender.try_send(Event {
                            value: field(&frame, "event"),
                            _permit: permit,
                        }) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                streams.remove(&id);
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => break Error::ResourceLimit,
                        }
                    }
                }
            }
        }
    };
    for (_, reply) in pending {
        let _ = reply.send(Err(failure.clone()));
    }
    for (_, stream) in streams {
        *stream.terminal.lock().unwrap_or_else(|p| p.into_inner()) = Some(Terminal {
            error: failure.clone(),
            _permit: None,
        });
    }
    let _ = endpoint.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[test]
    fn local_sink_reports_queue_and_byte_overflow() {
        let (sender, mut output) = mpsc::channel(1);
        let (failed, receiver) = watch::channel(None);
        let sink = LocalSink {
            sender,
            bytes: Arc::new(Semaphore::new(3)),
            failed,
        };
        use pilotdeck_gateway::runtime::connection::GatewayTextConnection;
        sink.send_text("abc".to_owned());
        sink.send_text("x".to_owned());
        assert!(matches!(&*receiver.borrow(), Some(Error::ResourceLimit)));
        assert_eq!(output.try_recv().unwrap().text, "abc");
    }
    #[tokio::test]
    async fn local_client_uses_shared_gateway_and_closes_owner() {
        let root = std::env::temp_dir().join(format!("pdrs-local-client-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n").unwrap();
        let mut env = pilotdeck_gateway::config::Env::new();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let options = pilotdeck_gateway::native_process::LocalHostOptions {
            home: home.clone(),
            project,
            sidecar: root.join("unused-sidecar"),
            sidecar_args: Vec::new(),
            drain_budget: Duration::from_secs(2),
        };
        let client = timeout(Duration::from_secs(10), Client::local(options, env))
            .await
            .unwrap()
            .unwrap();
        let result = timeout(
            Duration::from_secs(10),
            client.request("describe_server", JsValue::Object(JsObject::new())),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.get("capabilities").is_some());
        client.close().await.unwrap();
        assert!(!home.join("server-token").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn blocked_hello_flush_cannot_extend_original_deadline() {
        let deadline = Instant::now() + Duration::from_secs(10);
        tokio::time::advance(Duration::from_secs(9)).await;
        let result = within_hello(deadline, std::future::pending::<Result<()>>()).await;
        assert!(
            matches!(result, Err(Error::Connection(message)) if message == "Gateway hello timed out.")
        );
        assert_eq!(Instant::now(), deadline);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_queued_request_is_never_sent() {
        timeout(Duration::from_secs(3), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/ws", listener.local_addr().unwrap());
            let client_socket = async {
                connect_async_with_config(&url, Some(crate::ws_config::websocket_config()), false)
                    .await
                    .unwrap()
                    .0
            };
            let server_socket = async {
                let (socket, _) = listener.accept().await.unwrap();
                accept_async(socket).await.unwrap()
            };
            let (socket, mut peer) = tokio::join!(client_socket, server_socket);
            let (sender, receiver) = mpsc::channel(COMMANDS);
            let budget = Arc::new(Semaphore::new(QUEUED_BYTES));
            let permit = Arc::clone(&budget).try_acquire_many_owned(1).unwrap();
            let (reply, response) = oneshot::channel();
            drop(response);
            sender
                .send(Command::Request {
                    id: "cancelled".into(),
                    frame: "must not be sent".into(),
                    _permit: permit,
                    reply,
                })
                .await
                .unwrap();
            let (done, finished) = oneshot::channel();
            sender.send(Command::Close { reply: done }).await.unwrap();
            tokio::spawn(pump(
                Endpoint::WebSocket(Box::new(socket)),
                receiver,
                budget,
            ));
            let frame = peer.next().await.unwrap().unwrap();
            assert!(frame.is_close(), "cancelled command leaked a frame");
            finished.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
