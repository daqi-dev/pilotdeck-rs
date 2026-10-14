//! exp010 recording primitives shared by the B1 relay. No payload parsing.
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use hyper::{
    body::{Body, Frame, SizeHint},
    HeaderMap,
};
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
struct Writer {
    file: Box<dyn Write + Send>,
    seq: u64,
}
pub struct Recorder {
    writer: Mutex<Writer>,
    started: Instant,
    pub failed: AtomicBool,
}
impl Recorder {
    pub fn open(path: &str) -> std::io::Result<Arc<Self>> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Arc::new(Self {
            writer: Mutex::new(Writer {
                file: Box::new(file),
                seq: 0,
            }),
            started: Instant::now(),
            failed: AtomicBool::new(false),
        }))
    }
    // Each event is acknowledged only after write + flush; there is no lossy queue.
    pub fn write(&self, mut event: Value) -> Result<(), Error> {
        let arrived = self.started.elapsed().as_nanos().to_string();
        let result = (|| -> Result<(), Error> {
            let mut writer = self.writer.lock().map_err(|_| "record writer poisoned")?;
            writer.seq += 1;
            event["seq"] = json!(writer.seq);
            event["arrival_ns"] = json!(arrived);
            writeln!(writer.file, "{event}")?;
            writer.file.flush()?;
            Ok(())
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
        result
    }
}
fn secret(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-refreshed-token"
            | "x-api-key"
            | "api-key"
    ) || name == "token"
        || name.ends_with("-token")
}
pub fn headers(headers: &HeaderMap, redact: bool) -> Value {
    // HeaderMap iteration includes each value, unlike collect-to-dict.
    json!(headers
        .iter()
        .map(|(name, value)| {
            let bytes: &[u8] = if redact && secret(name.as_str()) {
                b"<redacted>"
            } else {
                value.as_bytes()
            };
            json!({"name": name.as_str(), "value_b64": STANDARD.encode(bytes)})
        })
        .collect::<Vec<_>>())
}
pub fn query(raw: Option<&str>) -> String {
    raw.unwrap_or("")
        .split('&')
        .map(|part| {
            let (name, _) = part.split_once('=').unwrap_or((part, ""));
            // Percent-encoded token names must not bypass redaction.
            let mut decoded = Vec::new();
            let mut bytes = name.as_bytes().iter().copied();
            while let Some(b) = bytes.next() {
                if b == b'%' {
                    let pair = [bytes.next().unwrap_or(b'?'), bytes.next().unwrap_or(b'?')];
                    if let Ok(hex) = std::str::from_utf8(&pair) {
                        if let Ok(v) = u8::from_str_radix(hex, 16) {
                            decoded.push(v);
                            continue;
                        }
                    }
                }
                decoded.push(b);
            }
            if secret(&String::from_utf8_lossy(&decoded).to_ascii_lowercase()) {
                format!("{name}=<redacted>")
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub struct RecordedBody<B> {
    inner: B,
    recorder: Arc<Recorder>,
    id: u64,
    direction: &'static str,
    offset: u64,
    exact_length: Option<u64>,
    ended: bool,
}
impl<B: Body> RecordedBody<B> {
    pub fn new(inner: B, recorder: Arc<Recorder>, id: u64, direction: &'static str) -> Self {
        // Hyper may drop a body as soon as Content-Length bytes have been read,
        // without polling EOF. Keep the original hint, not the remaining size.
        let exact_length = inner.size_hint().exact();
        let mut body = Self {
            inner,
            recorder,
            id,
            direction,
            offset: 0,
            exact_length,
            ended: false,
        };
        if body.inner.is_end_stream() {
            body.ended = body.recorder.write(body.event("http_end")).is_ok();
        }
        body
    }
}
impl<B> RecordedBody<B> {
    fn event(&self, kind: &str) -> Value {
        json!({"kind": kind, "id": self.id, "direction": self.direction, "offset": self.offset})
    }
}
impl<B> Body for RecordedBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        let mut event;
        match &polled {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(Ok(frame))) => {
                event = this.event("http_chunk");
                if let Some(data) = frame.data_ref() {
                    event["body_b64"] = json!(STANDARD.encode(data));
                    event["bytes"] = json!(data.len());
                    this.offset += data.len() as u64;
                }
                if let Some(trailers) = frame.trailers_ref() {
                    event["trailers"] = headers(trailers, this.direction == "request");
                }
            }
            Poll::Ready(Some(Err(error))) => {
                event = this.event("http_body_error");
                event["error"] = json!(error.to_string());
                this.ended = true;
            }
            Poll::Ready(None) => {
                event = this.event("http_end");
                this.ended = true;
            }
        }
        if let Err(error) = this.recorder.write(event) {
            this.ended = true;
            return Poll::Ready(Some(Err(error)));
        }
        // The final data frame can signal EOF without a subsequent None poll.
        if !this.ended && this.inner.is_end_stream() {
            this.ended = true;
            if let Err(error) = this.recorder.write(this.event("http_end")) {
                return Poll::Ready(Some(Err(error)));
            }
        }
        polled
            .map(|option| option.map(|result| result.map_err(|error| -> Error { Box::new(error) })))
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl<B> Drop for RecordedBody<B> {
    fn drop(&mut self) {
        if !self.ended {
            let kind = if self.exact_length == Some(self.offset) {
                "http_end"
            } else {
                "http_aborted"
            };
            let _ = self.recorder.write(self.event(kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicates_bytes_and_redaction() {
        let mut map = HeaderMap::new();
        map.append("set-cookie", "a=1".parse().unwrap());
        map.append("set-cookie", "b=2".parse().unwrap());
        map.append(
            "x-bytes",
            hyper::header::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(headers(&map, false).as_array().unwrap().len(), 3);
        assert_eq!(headers(&map, false)[2]["value_b64"], "/w==");
        assert_eq!(
            query(Some("%74oken=secret&x=1&x=2")),
            "%74oken=<redacted>&x=1&x=2"
        );
        assert!(!headers(&map, true).to_string().contains("YT0x"));
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected disk failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn writer_failure_is_observable() {
        let recorder = Recorder {
            writer: Mutex::new(Writer {
                file: Box::new(Broken),
                seq: 0,
            }),
            started: Instant::now(),
            failed: AtomicBool::new(false),
        };
        assert!(recorder.write(json!({"kind":"ws_frame"})).is_err());
        assert!(recorder.failed.load(Ordering::SeqCst));
    }
}

// Observe the transport as well as tungstenite messages. This preserves FIN,
// opcode, fragmentation, masking and automatically generated control replies.
// No byte is changed. Four legs are distinguished; S3 can select one boundary.
pub struct WireTap<T> {
    inner: T,
    recorder: Option<Arc<Recorder>>,
    read: WireFrames,
    write: WireFrames,
}
struct WireFrames {
    pending: Vec<u8>,
    connection: u64,
    leg: String,
}
impl WireFrames {
    fn ingest(&mut self, bytes: &[u8], recorder: &Recorder) -> std::io::Result<()> {
        self.pending.extend_from_slice(bytes);
        loop {
            let b = &self.pending;
            if b.len() < 2 {
                return Ok(());
            }
            let masked = b[1] & 0x80 != 0;
            let len7 = b[1] & 0x7f;
            let extra = match len7 {
                126 => 2,
                127 => 8,
                _ => 0,
            };
            if b.len() < 2 + extra {
                return Ok(());
            }
            let length = match len7 {
                126 => u16::from_be_bytes([b[2], b[3]]) as u64,
                127 => u64::from_be_bytes(b[2..10].try_into().expect("length checked")),
                n => n as u64,
            };
            if length > 16 * 1024 * 1024 {
                return Err(std::io::Error::other("WS recording frame exceeds 16 MiB"));
            }
            let head = 2 + extra + if masked { 4 } else { 0 };
            let total = head + length as usize;
            if b.len() < total {
                return Ok(());
            }
            let mut payload = b[head..total].to_vec();
            if masked {
                let mask = &b[2 + extra..head];
                for (i, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[i % 4];
                }
            }
            recorder.write(json!({"kind":"ws_frame", "connection":self.connection, "leg":self.leg,
                "dir":if self.leg == "upstream-read" || self.leg == "downstream-write" { "u2c" } else { "c2u" },
                "fin":b[0]&0x80 != 0, "rsv":(b[0]>>4)&7, "opcode":b[0]&15, "masked":masked,
                "bytes":length, "payload_b64":STANDARD.encode(&payload), "wire_b64":STANDARD.encode(&b[..total])}))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            self.pending.drain(..total);
        }
    }
}
impl<T> WireTap<T> {
    pub fn new(inner: T, recorder: Option<Arc<Recorder>>, connection: u64, leg: &str) -> Self {
        Self {
            inner,
            recorder,
            read: WireFrames {
                pending: Vec::new(),
                connection,
                leg: format!("{leg}-read"),
            },
            write: WireFrames {
                pending: Vec::new(),
                connection,
                leg: format!("{leg}-write"),
            },
        }
    }
}
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for WireTap<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) {
            if let Some(recorder) = &this.recorder {
                if let Err(error) = this.read.ingest(&buf.filled()[before..], recorder) {
                    return Poll::Ready(Err(error));
                }
            }
        }
        polled
    }
}
impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for WireTap<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(count)) = polled {
            if let Some(recorder) = &this.recorder {
                if let Err(error) = this.write.ingest(&bytes[..count], recorder) {
                    return Poll::Ready(Err(error));
                }
            }
        }
        polled
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<T> Drop for WireTap<T> {
    fn drop(&mut self) {
        if let Some(recorder) = &self.recorder {
            for state in [&self.read, &self.write] {
                if !state.pending.is_empty() {
                    let _ = recorder
                        .write(json!({"kind":"ws_partial", "connection":state.connection,
                        "leg":state.leg, "wire_b64":STANDARD.encode(&state.pending)}));
                }
            }
        }
    }
}

#[cfg(test)]
mod stream_controls {
    use super::*;
    use http_body_util::{BodyExt, StreamBody};
    #[derive(Clone)]
    struct Memory(Arc<Mutex<Vec<u8>>>);
    impl Write for Memory {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn memory_log() -> (Arc<Recorder>, Memory) {
        let memory = Memory(Arc::new(Mutex::new(Vec::new())));
        (
            Arc::new(Recorder {
                writer: Mutex::new(Writer {
                    file: Box::new(memory.clone()),
                    seq: 0,
                }),
                started: Instant::now(),
                failed: AtomicBool::new(false),
            }),
            memory,
        )
    }
    fn records(memory: &Memory) -> Vec<Value> {
        String::from_utf8(memory.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    #[tokio::test]
    async fn final_data_frame_records_end_before_drop() {
        let (log, memory) = memory_log();
        let source = http_body_util::Full::new(Bytes::from_static(b"complete"));
        let mut body = RecordedBody::new(source, log, 44, "response");
        assert!(body.frame().await.unwrap().unwrap().is_data());
        // No EOF poll: this is how Hyper consumes a fixed-length body.
        let events = records(&memory);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["kind"], "http_chunk");
        assert_eq!(events[1]["kind"], "http_end");
        assert_eq!(events[1]["offset"], 8);
        assert!(body.ended);
        drop(body);
        assert_eq!(records(&memory).len(), 2);
    }
    struct FixedLengthBody {
        remaining: Bytes,
        exact: bool,
    }
    impl Body for FixedLengthBody {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            let this = self.get_mut();
            if this.remaining.is_empty() {
                Poll::Ready(None)
            } else {
                // Two frames; no is_end_stream override, as allowed by Body.
                let size = this.remaining.len().min(2);
                Poll::Ready(Some(Ok(Frame::data(this.remaining.split_to(size)))))
            }
        }
        fn size_hint(&self) -> SizeHint {
            if self.exact {
                SizeHint::with_exact(self.remaining.len() as u64)
            } else {
                SizeHint::default()
            }
        }
    }
    #[tokio::test]
    async fn drop_after_content_length_is_end_but_partial_or_unknown_is_abort() {
        for (exact, frames, terminal) in [
            (true, 2, "http_end"),
            (true, 1, "http_aborted"),
            (false, 2, "http_aborted"),
        ] {
            let (log, memory) = memory_log();
            let source = FixedLengthBody {
                remaining: Bytes::from_static(b"abcd"),
                exact,
            };
            let mut body = RecordedBody::new(source, log, 45, "request");
            for _ in 0..frames {
                assert!(body.frame().await.unwrap().unwrap().is_data());
            }
            assert!(!body.ended);
            // Simulate Hyper dropping at Content-Length, never polling None.
            drop(body);
            let events = records(&memory);
            assert_eq!(events.len(), frames + 1);
            assert_eq!(events.last().unwrap()["kind"], terminal);
            assert_eq!(events.last().unwrap()["offset"], (frames * 2) as u64);
        }
    }
    #[tokio::test]
    async fn hyper_fixed_length_response_drop_records_end() {
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (log, memory) = memory_log();
        let (mut client_io, server_io) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let service = service_fn(move |_| {
                let body = RecordedBody::new(
                    FixedLengthBody {
                        remaining: Bytes::from_static(b"abcd"),
                        exact: true,
                    },
                    log.clone(),
                    46,
                    "response",
                );
                async move {
                    Ok::<_, std::convert::Infallible>(
                        hyper::Response::builder()
                            .header("content-length", "4")
                            .body(body)
                            .unwrap(),
                    )
                }
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server_io), service)
                .await
                .unwrap();
        });
        client_io
            .write_all(b"GET / HTTP/1.1\r\nHost: memory.test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client_io.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        assert!(response.ends_with(b"\r\n\r\nabcd"));
        let events = records(&memory);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["kind"], "http_chunk");
        assert_eq!(events[1]["kind"], "http_chunk");
        assert_eq!(events[2]["kind"], "http_end");
        assert_eq!(events[2]["offset"], 4);
    }
    #[tokio::test]
    async fn body_offsets_trailers_eof_and_abort() {
        let (log, memory) = memory_log();
        let mut trailers = HeaderMap::new();
        trailers.append("x-last", "one".parse().unwrap());
        trailers.append("x-last", "two".parse().unwrap());
        let source = StreamBody::new(futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(Frame::data(Bytes::from_static(b"a"))),
            Ok(Frame::data(Bytes::from_static(b"bc"))),
            Ok(Frame::trailers(trailers)),
        ]));
        let mut body = RecordedBody::new(source, log.clone(), 42, "response");
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "a"
        );
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "bc"
        );
        assert!(body.frame().await.unwrap().unwrap().is_trailers());
        assert!(body.frame().await.is_none());
        drop(body);
        let events = records(&memory);
        assert_eq!(
            events
                .iter()
                .map(|e| e["offset"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [0, 1, 3, 3]
        );
        assert_eq!(events[2]["trailers"].as_array().unwrap().len(), 2);
        assert_eq!(events[3]["kind"], "http_end");
        let source = http_body_util::Full::new(Bytes::from_static(b"interrupted"));
        drop(RecordedBody::new(source, log, 43, "request"));
        assert_eq!(records(&memory).last().unwrap()["kind"], "http_aborted");
    }
    #[test]
    fn fragmented_wire_masking_and_controls() {
        let (log, memory) = memory_log();
        let mut parser = WireFrames {
            pending: Vec::new(),
            connection: 7,
            leg: "downstream-read".into(),
        };
        // Non-FIN text + masked continuation + Ping in one transport stream,
        // split at unrelated byte boundaries.
        let mut wire = Vec::new();
        wire.extend_from_slice(&[
            0x01,
            0x01,
            b'a',
            0x80,
            0x82,
            1,
            2,
            3,
            4,
            b'b' ^ 1,
            b'c' ^ 2,
            0x89,
            0x01,
            b'p',
        ]);
        for fragment in wire.chunks(3) {
            parser.ingest(fragment, &log).unwrap();
        }
        let events = records(&memory);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["fin"], false);
        assert_eq!(events[0]["opcode"], 1);
        assert_eq!(events[1]["opcode"], 0);
        assert_eq!(events[1]["payload_b64"], STANDARD.encode(b"bc"));
        assert_eq!(events[2]["opcode"], 9);
        assert_eq!(
            events
                .iter()
                .map(|e| e["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(parser.pending.is_empty());
    }
    #[test]
    fn wire_length_bound_fails_without_allocating_payload() {
        let (log, _) = memory_log();
        let mut parser = WireFrames {
            pending: Vec::new(),
            connection: 1,
            leg: "upstream-read".into(),
        };
        let mut header = vec![0x82, 127];
        header.extend_from_slice(&(17_u64 * 1024 * 1024).to_be_bytes());
        assert!(parser.ingest(&header, &log).is_err());
    }
}

#[cfg(test)]
mod upgrade_control {
    use super::*;
    use http_body_util::Empty;
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn hyper_upgrade_preserves_first_frame_in_header_read() {
        // In-memory duplex, no loopback listener: the upstream writes the 101
        // and its first WS frame together. Hyper must retain the tail in Upgrade.
        let (client_io, mut server_io) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let byte = server_io.read_u8().await.unwrap();
                request.push(byte);
            }
            server_io.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n\x81\x03abc").await.unwrap();
            server_io.shutdown().await.unwrap();
        });
        let (mut client, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(client_io))
                .await
                .unwrap();
        let connection = tokio::spawn(connection.with_upgrades());
        let request = hyper::Request::builder()
            .uri("http://upstream.test/ws")
            .header("host", "upstream.test")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let mut response = client.send_request(request).await.unwrap();
        assert_eq!(response.status(), hyper::StatusCode::SWITCHING_PROTOCOLS);
        let upgraded = hyper::upgrade::on(&mut response).await.unwrap();
        let mut stream = TokioIo::new(upgraded);
        let mut frame = [0; 5];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            stream.read_exact(&mut frame),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&frame, b"\x81\x03abc");
        server.await.unwrap();
        connection.await.unwrap().unwrap();
    }
}
