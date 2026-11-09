use super::{Result, failure};
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use pilotdeck_protocol::frame::Decoder;
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) struct Socket {
    stream: TokioIo<hyper::upgrade::Upgraded>,
    decoder: Decoder,
    fragment: Option<(u8, Vec<u8>)>,
}
impl Socket {
    pub async fn connect(endpoint: &str, origin: Option<&str>) -> Result<Self> {
        let endpoint =
            url::Url::parse(endpoint).map_err(|_| failure("Invalid Channel WebSocket URL"))?;
        if !matches!(endpoint.scheme(), "ws" | "wss")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(failure("Channel WebSocket requires ws or wss URL"));
        }
        let scheme = if endpoint.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        let uri = format!(
            "{scheme}:{}",
            &endpoint.as_str()[endpoint.scheme().len() + 1..]
        );
        let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
        let expected = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
        let connector = crate::runtime::model::http::https_connector(
            crate::runtime::model::http::webpki_client_config(),
        );
        let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build(connector);
        let mut request = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", key);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let request = request
            .body(Full::new(Bytes::new()))
            .map_err(|_| failure("Invalid Channel WebSocket request"))?;
        let connect = async {
            let mut response = client
                .request(request)
                .await
                .map_err(|_| failure("Channel WebSocket connection failed"))?;
            if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS
                || response
                    .headers()
                    .get("sec-websocket-accept")
                    .is_none_or(|value| value.as_bytes() != expected.as_bytes())
            {
                return Err(failure("Server rejected WebSocket upgrade"));
            }
            let stream = hyper::upgrade::on(&mut response)
                .await
                .map_err(|_| failure("Channel WebSocket upgrade failed"))?;
            Ok(Self {
                stream: TokioIo::new(stream),
                decoder: Default::default(),
                fragment: None,
            })
        };
        tokio::time::timeout(Duration::from_secs(20), connect)
            .await
            .map_err(|_| failure("Channel WebSocket connection timed out"))?
    }
    async fn frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut wire = vec![0x80 | opcode];
        match payload.len() {
            0..126 => wire.push(0x80 | payload.len() as u8),
            126..=65535 => {
                wire.push(0xfe);
                wire.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            }
            _ => {
                wire.push(0xff);
                wire.extend_from_slice(&(payload.len() as u64).to_be_bytes());
            }
        }
        let mask_id = uuid::Uuid::new_v4();
        let mask = &mask_id.as_bytes()[..4];
        wire.extend_from_slice(mask);
        wire.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        self.stream
            .write_all(&wire)
            .await
            .map_err(|_| failure("Channel WebSocket write failed"))
    }
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.frame(1, text.as_bytes()).await
    }
    pub async fn send(&mut self, value: Value) -> Result<()> {
        self.frame(1, value.to_string().as_bytes()).await
    }
    pub async fn close(&mut self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            self.frame(8, &1000u16.to_be_bytes()),
        )
        .await;
    }
    pub async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            if self.decoder.buffer.len() > 16_777_216 {
                return Err(failure("Channel WebSocket frame too large"));
            }
            let Some(frame) = self
                .decoder
                .next_server()
                .map_err(|_| failure("Invalid Channel WebSocket frame"))?
            else {
                if self
                    .stream
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .map_err(|_| failure("Channel WebSocket read failed"))?
                    == 0
                {
                    return Ok(None);
                }
                continue;
            };
            let final_frame = frame.wire[0] & 0x80 != 0;
            match frame.opcode {
                8 => {
                    self.close().await;
                    return Ok(None);
                }
                9 => {
                    self.frame(10, &frame.payload).await?;
                    continue;
                }
                10 => continue,
                0 => {
                    let Some((_, bytes)) = self.fragment.as_mut() else {
                        return Err(failure("Unexpected WebSocket continuation"));
                    };
                    if bytes.len() + frame.payload.len() > 16_777_216 {
                        return Err(failure("Channel WebSocket message too large"));
                    }
                    bytes.extend_from_slice(&frame.payload);
                    if !final_frame {
                        continue;
                    }
                }
                1 | 2 => {
                    if self.fragment.is_some() {
                        return Err(failure("Unexpected fragmented WebSocket message"));
                    }
                    self.fragment = Some((frame.opcode, frame.payload));
                    if !final_frame {
                        continue;
                    }
                }
                _ => return Err(failure("Invalid Channel WebSocket opcode")),
            }
            let (opcode, bytes) = self.fragment.take().unwrap();
            if opcode != 1 {
                continue;
            }
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return Ok(Some(value));
            }
        }
    }
}
