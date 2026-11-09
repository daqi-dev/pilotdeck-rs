use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use pilotdeck_protocol::frame::{self, Decoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Socket {
    io: TokioIo<hyper::upgrade::Upgraded>,
    decoder: Decoder,
    fragments: Option<(u8, Vec<u8>)>,
}
impl Socket {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let endpoint =
            url::Url::parse(endpoint).map_err(|_| failure("Invalid Feishu WebSocket URL"))?;
        let scheme = match endpoint.scheme() {
            "ws" => "http",
            "wss" => "https",
            _ => return Err(failure("Invalid Feishu WebSocket scheme")),
        };
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(failure("Feishu WebSocket URL contains userinfo"));
        }
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
        let request = hyper::Request::builder()
            .uri(uri)
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", key)
            .body(Full::new(Bytes::new()))
            .map_err(|_| failure("Invalid Feishu WebSocket request"))?;
        let mut response = client
            .request(request)
            .await
            .map_err(|_| failure("Feishu WebSocket connection failed"))?;
        if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS
            || response
                .headers()
                .get("sec-websocket-accept")
                .is_none_or(|value| value.as_bytes() != expected.as_bytes())
        {
            return Err(failure("Feishu WebSocket upgrade rejected"));
        }
        let io = hyper::upgrade::on(&mut response)
            .await
            .map_err(|_| failure("Feishu WebSocket upgrade failed"))?;
        Ok(Self {
            io: TokioIo::new(io),
            decoder: Default::default(),
            fragments: None,
        })
    }
    async fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let code = match payload.len() {
            0..126 => payload.len() as u8,
            126..=65535 => 126,
            _ => 127,
        };
        self.io
            .write_all(&frame::client_frame(0x80 | opcode, code, payload))
            .await
            .map_err(|_| failure("Feishu WebSocket write failed"))
    }
    pub async fn binary(&mut self, payload: &[u8]) -> Result<()> {
        self.send(2, payload).await
    }
    pub async fn close(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(1), self.send(8, &1000u16.to_be_bytes()))
            .await;
    }
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if self.decoder.buffer.len() > 16_777_216 {
                return Err(failure("Feishu WebSocket message too large"));
            }
            let Some(frame) = self
                .decoder
                .next_server()
                .map_err(|_| failure("Invalid Feishu WebSocket frame"))?
            else {
                if self
                    .io
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .map_err(|_| failure("Feishu WebSocket read failed"))?
                    == 0
                {
                    return Ok(None);
                }
                continue;
            };
            match frame.opcode {
                8 => return Ok(None),
                9 => {
                    self.send(10, &frame.payload).await?;
                    continue;
                }
                10 => continue,
                1 | 2 => {
                    if self.fragments.is_some() {
                        return Err(failure("Unexpected Feishu fragmented frame"));
                    }
                    self.fragments = Some((frame.opcode, frame.payload));
                }
                0 => {
                    let Some((_, payload)) = self.fragments.as_mut() else {
                        return Err(failure("Unexpected Feishu continuation"));
                    };
                    if payload.len() + frame.payload.len() > 16_777_216 {
                        return Err(failure("Feishu WebSocket message too large"));
                    }
                    payload.extend_from_slice(&frame.payload);
                }
                _ => return Err(failure("Invalid Feishu WebSocket opcode")),
            }
            if frame.wire[0] & 0x80 == 0 {
                continue;
            }
            let (_, payload) = self.fragments.take().unwrap();
            return Ok(Some(payload));
        }
    }
}
