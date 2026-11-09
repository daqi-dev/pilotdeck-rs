use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use pilotdeck_protocol::frame::Decoder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Socket {
    stream: TokioIo<hyper::upgrade::Upgraded>,
    decoder: Decoder,
    fragment: Option<(u8, Vec<u8>)>,
}
impl Socket {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let endpoint =
            url::Url::parse(endpoint).map_err(|_| failure("Invalid Slack Socket Mode URL"))?;
        let local = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(endpoint.scheme() == "wss" || endpoint.scheme() == "ws" && local)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(failure("Slack Socket Mode requires WSS or loopback WS"));
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
        let request = hyper::Request::builder()
            .method("GET")
            .uri(uri)
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", key)
            .body(Full::new(Bytes::new()))
            .map_err(|_| failure("Invalid Slack Socket Mode request"))?;
        let connect = async {
            let mut response = client
                .request(request)
                .await
                .map_err(|_| failure("Slack Socket Mode connection failed"))?;
            if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS
                || response
                    .headers()
                    .get("sec-websocket-accept")
                    .is_none_or(|value| value.as_bytes() != expected.as_bytes())
            {
                return Err(failure("Slack rejected Socket Mode upgrade"));
            }
            let stream = hyper::upgrade::on(&mut response)
                .await
                .map_err(|_| failure("Slack Socket Mode upgrade failed"))?;
            Ok(Self {
                stream: TokioIo::new(stream),
                decoder: Default::default(),
                fragment: None,
            })
        };
        tokio::time::timeout(Duration::from_secs(20), connect)
            .await
            .map_err(|_| failure("Slack Socket Mode connection timed out"))?
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
            .map_err(|_| failure("Slack Socket Mode write failed"))
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
                return Err(failure("Slack Socket Mode frame too large"));
            }
            let Some(frame) = self
                .decoder
                .next_server()
                .map_err(|_| failure("Invalid Slack Socket Mode frame"))?
            else {
                if self
                    .stream
                    .read_buf(&mut self.decoder.buffer)
                    .await
                    .map_err(|_| failure("Slack Socket Mode read failed"))?
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
                        return Err(failure("Unexpected Slack continuation"));
                    };
                    if bytes.len() + frame.payload.len() > 16_777_216 {
                        return Err(failure("Slack Socket Mode message too large"));
                    }
                    bytes.extend_from_slice(&frame.payload);
                    if !final_frame {
                        continue;
                    }
                }
                1 | 2 => {
                    if self.fragment.is_some() {
                        return Err(failure("Unexpected fragmented Slack message"));
                    }
                    self.fragment = Some((frame.opcode, frame.payload));
                    if !final_frame {
                        continue;
                    }
                }
                _ => return Err(failure("Invalid Slack Socket Mode opcode")),
            }
            let (opcode, bytes) = self.fragment.take().unwrap();
            if opcode != 1 {
                continue;
            }
            let value = serde_json::from_slice(&bytes)
                .map_err(|_| failure("Invalid Slack Socket Mode JSON"))?;
            return Ok(Some(value));
        }
    }
}
