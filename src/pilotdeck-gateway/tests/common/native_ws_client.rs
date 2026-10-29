//! #749 raw WS driver: accepts only the server frames exercised by live_e2e.
#[path = "native_ws_sample.rs"]
mod native_ws_sample;
use native_ws_sample::RFC6455_SAMPLE_NONCE;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Duration, timeout},
};
pub struct Client(TcpStream);
impl Client {
    pub async fn connect(address: SocketAddr) -> Self {
        let mut socket = TcpStream::connect(address)
            .await
            .expect("#749 live TCP connect");
        socket.write_all(format!("GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n").as_bytes()).await.expect("#749 live upgrade write");
        let mut header = vec![];
        timeout(Duration::from_secs(5), async {
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.expect("#749 live upgrade byte"));
            }
        })
        .await
        .expect("#749 live upgrade deadline");
        assert!(
            String::from_utf8_lossy(&header).starts_with("HTTP/1.1 101"),
            "#749 live upgraded status"
        );
        Self(socket)
    }
    async fn frame(&mut self, opcode: u8, payload: &[u8]) {
        let mut frame = vec![0x80 | opcode];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            assert!(
                payload.len() <= u16::MAX as usize,
                "#749 bounded test request"
            );
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        let mask = [1, 2, 3, 4];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.0
            .write_all(&frame)
            .await
            .expect("#749 live frame write");
    }
    pub async fn send(&mut self, value: Value) {
        self.frame(1, value.to_string().as_bytes()).await;
    }
    async fn read(&mut self) -> (u8, Vec<u8>) {
        timeout(Duration::from_secs(45), async {
            let first = self.0.read_u8().await.expect("#749 live frame first");
            let size = self.0.read_u8().await.expect("#749 live frame size");
            assert_eq!(size & 0x80, 0, "#749 live frame unmasked");
            let size = match size {
                126 => self.0.read_u16().await.expect("#749 live 16-bit size") as usize,
                127 => usize::try_from(self.0.read_u64().await.expect("#749 live 64-bit size"))
                    .expect("#749 live size usize"),
                n => n as usize,
            };
            assert!(size < 16 * 1024 * 1024, "#749 bounded live response");
            let mut bytes = vec![0; size];
            self.0
                .read_exact(&mut bytes)
                .await
                .expect("#749 live frame payload");
            (first, bytes)
        })
        .await
        .expect("#749 live response deadline")
    }
    pub async fn read_json(&mut self) -> Value {
        let (opcode, bytes) = self.read().await;
        assert_eq!(opcode, 0x81, "#749 live text frame");
        serde_json::from_slice(&bytes).expect("#749 live response JSON")
    }
    pub async fn close(&mut self) {
        self.frame(8, &[]).await;
        let (opcode, _) = self.read().await;
        assert_eq!(opcode, 0x88, "#749 live close reply");
    }
}
