//! RemoteGateway stream semantics for the legacy CliChannel (not SDK query).
use super::super::gateway_probe;
use super::{
    Failure,
    paths::{self, Env},
    render,
};
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::json;
use std::{path::Path, time::Duration};
use tokio::{
    net::TcpStream,
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config, tungstenite::Message,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub(super) struct Remote {
    socket: Socket,
}
fn is_field(frame: &JsValue, key: &str, expected: &str) -> bool {
    frame
        .get(key)
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
}
pub(super) async fn connect(env: &Env, cwd: &Path) -> Result<Option<Remote>, Failure> {
    if !gateway_probe::healthy().await {
        return Ok(None);
    }
    let Ok(path) = paths::gateway_token_path(env, cwd) else {
        return Ok(None);
    };
    // Reuse the SDK file-source policy: ordinary IO/empty tokens keep the
    // legacy fallback, while special/oversized files are explicit errors and
    // must not silently start a different local host.
    let token = super::super::sdk::token::read(&path)?;
    if token.is_empty() {
        return Ok(None);
    }
    Ok(connect_with_token(token).await)
}
async fn connect_with_token(token: String) -> Option<Remote> {
    let (mut socket, _) = connect_async_with_config(
        "ws://127.0.0.1:18789/ws",
        Some(pilotdeck_cli::ws_config::websocket_config()),
        false,
    )
    .await
    .ok()?;
    socket.send(Message::Text(json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"clientName":"cli","clientVersion":"0.1.0","token":token}).to_string().into())).await.ok()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let frame = timeout_at(deadline, socket.next()).await.ok()??.ok()?;
        if Instant::now() >= deadline || frame.is_close() {
            return None;
        }
        if !frame.is_text() {
            continue;
        }
        let value = parse_json(frame.to_text().ok()?).ok()?;
        if is_field(&value, "type", "hello_ok") {
            return Some(Remote { socket });
        }
    }
}
impl Remote {
    /// Poll the socket while waiting for another stdin line; tungstenite queues
    /// automatic pong frames on read and flushes them explicitly here.
    pub(super) async fn idle(&mut self) -> Result<(), Failure> {
        let frame = self
            .socket
            .next()
            .await
            .ok_or_else(|| Failure::Error("Gateway WebSocket closed.".into()))?
            .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
        if frame.is_close() {
            return Err(Failure::Error("Gateway WebSocket closed.".into()));
        }
        if frame.is_ping() {
            self.socket
                .flush()
                .await
                .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
        }
        // Return after each frame so the outer select can service stdin even
        // when the peer continuously supplies immediately-ready notifications.
        Ok(())
    }
    pub(super) async fn submit(&mut self, session: &str, prompt: &str) -> Result<(), Failure> {
        let id = uuid::Uuid::new_v4().to_string();
        let frame = json!({"type":"request","id":id,"method":"submit_turn",
            "params":{"sessionKey":session,"channelKey":"cli","message":prompt,"canPrompt":false}});
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
        loop {
            let frame = self
                .socket
                .next()
                .await
                .ok_or_else(|| Failure::Error("Gateway WebSocket closed.".into()))?
                .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
            if frame.is_close() {
                return Err(Failure::Error("Gateway WebSocket closed.".into()));
            }
            if frame.is_ping() {
                self.socket
                    .flush()
                    .await
                    .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
                continue;
            }
            if !frame.is_text() {
                continue;
            }
            let value = parse_json(
                frame
                    .to_text()
                    .map_err(|_| Failure::Error("Invalid Gateway frame.".into()))?,
            )
            .map_err(|_| Failure::Error("Invalid Gateway frame.".into()))?;
            if !is_field(&value, "id", &id) {
                continue;
            }
            // Admission can reject submit_turn with an ordinary response and
            // leave the connection open. Waiting for an event final after this
            // terminal error would keep the CLI alive indefinitely.
            if is_field(&value, "type", "response")
                && value.get("ok").and_then(JsValue::as_bool) == Some(false)
            {
                let message = value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(JsValue::as_string)
                    .map(|message| String::from_utf16_lossy(message.units()))
                    .unwrap_or_else(|| "Gateway stream failed.".into());
                return Err(Failure::Error(message));
            }
            if !is_field(&value, "type", "event") {
                continue;
            }
            if value.get("final").and_then(JsValue::as_bool) == Some(true) {
                return Ok(());
            }
            let event = value
                .get("event")
                .ok_or_else(|| Failure::Error("Gateway event is missing.".into()))?;
            render::write(event)?;
        }
    }
    pub(super) async fn close(&mut self) {
        let _ = timeout(Duration::from_secs(1), self.socket.close(None)).await;
    }
}
