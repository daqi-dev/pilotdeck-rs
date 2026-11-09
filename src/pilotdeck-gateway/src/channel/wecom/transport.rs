//! Owned websocket request/reply transport. Callbacks feed the channel owner;
//! no turn, permission decision or account state is created here.
use super::wire::{self, Frame};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type PendingResponse = (Arc<()>, oneshot::Sender<Result<JsValue, String>>);
#[derive(Default)]
struct PendingState {
    closed: bool,
    requests: HashMap<JsString, PendingResponse>,
}
type Pending = Arc<Mutex<PendingState>>;
struct Reservation {
    pending: Pending,
    id: JsString,
    token: Arc<()>,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending
            .requests
            .get(&self.id)
            .is_some_and(|(token, _)| Arc::ptr_eq(token, &self.token))
        {
            pending.requests.remove(&self.id);
        }
    }
}
pub(crate) struct Transport {
    sink: Arc<tokio::sync::Mutex<Sink>>,
    pending: Pending,
    reader: tokio::task::JoinHandle<()>,
}
impl Transport {
    /// Caller supplies the exact callback queue owner and must drain/stop it.
    pub(crate) async fn connect(
        url: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<JsValue>), String> {
        let (ws, _) = tokio::time::timeout(Duration::from_secs(20), connect_async(url))
            .await
            .map_err(|_| "WeCom WebSocket connect timeout".to_string())?
            .map_err(|e| e.to_string())?;
        let (sink, mut stream) = ws.split();
        let sink = Arc::new(tokio::sync::Mutex::new(sink));
        let reader_sink = sink.clone();
        let pending: Pending = Arc::new(Mutex::new(PendingState::default()));
        let reader_pending = pending.clone();
        // TS dispatches callbacks without an artificial queue capacity.
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(Ok(frame)) = stream.next().await {
                let text = match frame {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    Message::Ping(bytes) => {
                        let pong = tokio::time::timeout(Duration::from_secs(2), async {
                            reader_sink.lock().await.send(Message::Pong(bytes)).await
                        })
                        .await;
                        if !matches!(pong, Ok(Ok(()))) {
                            break;
                        }
                        continue;
                    }
                    _ => continue,
                };
                let Ok(payload) = parse_json(text.as_str()) else {
                    continue;
                };
                match wire::classify(&payload) {
                    Ok(Frame::Callback) => {
                        if tx.send(payload).is_err() {
                            break;
                        }
                    }
                    Ok(Frame::Response {
                        request_id,
                        payload,
                    }) => {
                        let reply = reader_pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .requests
                            .remove(&request_id);
                        if let Some((_, reply)) = reply {
                            let _ = reply.send(Ok(payload.clone()));
                        }
                    }
                    _ => {}
                }
            }
            let mut pending = reader_pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.closed = true;
            for (_, (_, reply)) in pending.requests.drain() {
                let _ = reply.send(Err("WeCom connection interrupted".into()));
            }
        });
        Ok((
            Self {
                sink,
                pending,
                reader,
            },
            rx,
        ))
    }
    /// Register before send so an immediate peer response cannot race it.
    pub(crate) async fn request(
        &self,
        id: JsString,
        payload: JsValue,
        timeout: Duration,
    ) -> Result<JsValue, String> {
        let (tx, rx) = oneshot::channel();
        let token = Arc::new(());
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if pending.closed {
                return Err("WeCom connection interrupted".into());
            }
            if pending.requests.contains_key(&id) {
                return Err("WeCom request id already pending".into());
            }
            pending.requests.insert(id.clone(), (token.clone(), tx));
        }
        let _reservation = Reservation {
            pending: self.pending.clone(),
            id,
            token,
        };
        let text = stringify_json(&payload)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "WeCom request is undefined".to_string())?;
        self.sink
            .lock()
            .await
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| e.to_string())?;
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "Timeout waiting for WeCom response".to_string())?
            .map_err(|_| "WeCom connection interrupted".to_string())?
    }
    pub(crate) async fn send(&self, payload: JsValue) -> Result<(), String> {
        let text = stringify_json(&payload)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "WeCom request is undefined".to_string())?;
        self.sink
            .lock()
            .await
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| e.to_string())
    }
    pub(crate) async fn stop(mut self) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.closed = true;
            for (_, (_, reply)) in pending.requests.drain() {
                let _ = reply.send(Err("WeCom adapter stopped".into()));
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            self.sink.lock().await.close().await
        })
        .await;
        self.reader.abort();
        let _ = (&mut self.reader).await;
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.reader.abort();
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.closed = true;
        for (_, (_, reply)) in pending.requests.drain() {
            let _ = reply.send(Err("WeCom connection interrupted".into()));
        }
    }
}

/// Shared connection slot lets in-flight turns use the newly connected socket
/// after the owner has authenticated a replacement.
pub(crate) struct ActiveTransport {
    current: tokio::sync::RwLock<Option<Arc<Transport>>>,
}
impl ActiveTransport {
    pub(crate) fn new(connection: Transport) -> Self {
        Self {
            current: tokio::sync::RwLock::new(Some(Arc::new(connection))),
        }
    }
    pub(crate) async fn request(
        &self,
        id: JsString,
        payload: JsValue,
        timeout: Duration,
    ) -> Result<JsValue, String> {
        let connection = self
            .current
            .read()
            .await
            .clone()
            .ok_or_else(|| "WeCom connection interrupted".to_string())?;
        connection.request(id, payload, timeout).await
    }
    pub(crate) async fn send(&self, payload: JsValue) -> Result<(), String> {
        let connection = self
            .current
            .read()
            .await
            .clone()
            .ok_or_else(|| "WeCom connection interrupted".to_string())?;
        connection.send(payload).await
    }
    pub(crate) async fn replace(&self, connection: Option<Transport>) {
        *self.current.write().await = connection.map(Arc::new);
    }
    pub(crate) async fn stop(&self) {
        if let Some(connection) = self.current.write().await.take()
            && let Ok(connection) = Arc::try_unwrap(connection)
        {
            connection.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn old_reservation_does_not_remove_reused_request_id() {
        let pending: Pending = Arc::new(Mutex::new(PendingState::default()));
        let id: JsString = "same-id".into();
        let old = Arc::new(());
        let reservation = Reservation {
            pending: pending.clone(),
            id: id.clone(),
            token: old,
        };
        let (sender, _receiver) = oneshot::channel();
        pending
            .lock()
            .unwrap()
            .requests
            .insert(id.clone(), (Arc::new(()), sender));
        drop(reservation);
        assert!(pending.lock().unwrap().requests.contains_key(&id));
    }

    #[tokio::test]
    async fn request_after_reader_exit_fails_without_waiting_for_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
            peer.close(None).await.unwrap();
        });
        let (transport, mut callbacks) = Transport::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), callbacks.recv())
            .await
            .unwrap();
        let request = wire::subscribe("r".into(), "bot".into(), "secret".into(), "device".into());
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            transport.request("r".into(), request, Duration::from_secs(30)),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), "WeCom connection interrupted");
        transport.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn loopback_reply_is_correlated_and_callback_is_delivered() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
            let message = peer.next().await.unwrap().unwrap().into_text().unwrap();
            let request = parse_json(message.as_str()).unwrap();
            assert_eq!(
                request.get("cmd").and_then(JsValue::as_string),
                Some(&JsString::from("aibot_subscribe"))
            );
            peer.send(Message::Text(
                r#"{"headers":{"req_id":"r"},"body":{"errcode":0}}"#.into(),
            ))
            .await
            .unwrap();
            peer.send(Message::Text(
                r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"c"},"body":{}}"#.into(),
            ))
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        let (transport, mut callbacks) = Transport::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        let request = wire::subscribe("r".into(), "bot".into(), "secret".into(), "device".into());
        let ack = transport
            .request("r".into(), request, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(
            ack.get("body").and_then(|b| b.get("errcode")),
            Some(&JsValue::Number(0.0))
        );
        let callback = tokio::time::timeout(Duration::from_secs(2), callbacks.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            wire::classify(&callback).unwrap(),
            Frame::Callback
        ));
        transport.stop().await;
        server.await.unwrap();
    }
}
