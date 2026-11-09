//! Gateway WebSocket receive budget used by both CLI clients.
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const MAX_RECEIVE_BYTES: usize = 128 * 1024 * 1024;

pub fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_RECEIVE_BYTES))
        .max_message_size(Some(MAX_RECEIVE_BYTES))
}
