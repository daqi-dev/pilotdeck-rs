//! Node fetch content decoding at the shared socket boundary (#867).
//! The response retains its wire headers; only data frames are decoded.
use super::{BodyFailure, ModelBody, TransportFailure, TransportResult};
use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder};
use bytes::{Buf, Bytes};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Method, Response};
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncBufRead, AsyncRead, BufReader, ReadBuf};

type Reader = Pin<Box<dyn AsyncBufRead + Send + Sync>>;
// Node v22.22.0 / undici 6.23.0 rejects the sixth coding before parsing
// individual tokens (even unknown ones) or allocating any decoder.
const MAX_CONTENT_ENCODINGS: usize = 5;

pub(super) fn decode_response(
    method: &Method,
    response: Response<ModelBody>,
    follow_redirects: bool,
) -> TransportResult {
    if method == Method::HEAD || matches!(response.status().as_u16(), 101 | 204 | 205 | 304) {
        return Ok(response.map(|_| {
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed()
        }));
    }
    // fetch_redirect::follow marks its requests; direct transports also serve
    // manual clients. Undici skips decoding intermediate redirect responses,
    // including the coding count, before dropping their possibly endless body.
    // An empty Location is falsy in undici and must still pass the count check.
    let location = response
        .headers()
        .get_all(hyper::header::LOCATION)
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()))
        .collect::<Vec<_>>()
        .join(", ");
    let will_follow = follow_redirects
        && matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
        && !location.is_empty();
    if will_follow {
        return Ok(response);
    }
    let encoding = response
        .headers()
        .get_all(hyper::header::CONTENT_ENCODING)
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()))
        .collect::<Vec<_>>()
        .join(",");
    let encodings: Vec<_> = encoding
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    if encodings.len() > MAX_CONTENT_ENCODINGS {
        return Err(TransportFailure::fetch_failed(None));
    }
    // undici abandons the entire decoder chain on any unknown coding (identity
    // and empty tokens included), rather than partially decoding the body.
    if !encodings
        .iter()
        .all(|s| matches!(s.as_str(), "gzip" | "x-gzip" | "deflate" | "br"))
    {
        return Ok(response);
    }
    Ok(response.map(|body| {
        let progress = Arc::new(AtomicBool::new(false));
        let mut reader: Reader = Box::pin(BodyReader {
            progress: progress.clone(),
            body,
            bytes: Bytes::new(),
        });
        for encoding in encodings.iter().rev() {
            reader = Box::pin(BufReader::new(DecodeReader {
                input: Some(reader),
                decoder: None,
                done: false,
                kind: match encoding.as_str() {
                    "br" => Kind::Brotli,
                    "deflate" => Kind::Deflate,
                    _ => Kind::Gzip,
                },
            }));
        }
        DecodedBody {
            reader,
            progress,
            done: false,
        }
        .boxed()
    }))
}

// No spawned tasks or queues: dropping this chain drops the Incoming body.
struct BodyReader {
    progress: Arc<AtomicBool>,
    body: ModelBody,
    bytes: Bytes,
}
impl AsyncBufRead for BodyReader {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let this = self.get_mut();
        while this.bytes.is_empty() {
            match ready!(Pin::new(&mut this.body).poll_frame(cx)) {
                Some(Ok(frame)) => {
                    if let Ok(bytes) = frame.into_data() {
                        if !bytes.is_empty() {
                            this.progress.store(true, Ordering::Relaxed);
                        }
                        this.bytes = bytes;
                    }
                }
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => break,
            }
        }
        Poll::Ready(Ok(&this.bytes))
    }
    fn consume(self: Pin<&mut Self>, amount: usize) {
        self.get_mut().bytes.advance(amount);
    }
}
impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let bytes = ready!(self.as_mut().poll_fill_buf(cx))?;
        let amount = bytes.len().min(buf.remaining());
        buf.put_slice(&bytes[..amount]);
        self.consume(amount);
        Poll::Ready(Ok(()))
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Gzip,
    Deflate,
    Brotli,
}
// Keep access to the undecoded input after a codec reaches its own EOF:
// a complete compression member does not finish an unfinished HTTP body.
enum Decoder {
    Gzip(GzipDecoder<Reader>),
    Zlib(ZlibDecoder<Reader>),
    Deflate(DeflateDecoder<Reader>),
    Brotli(BrotliDecoder<Reader>),
}
impl Decoder {
    fn input(&mut self) -> &mut Reader {
        match self {
            Self::Gzip(d) => d.get_mut(),
            Self::Zlib(d) => d.get_mut(),
            Self::Deflate(d) => d.get_mut(),
            Self::Brotli(d) => d.get_mut(),
        }
    }
    fn read(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self {
            Self::Gzip(d) => Pin::new(d).poll_read(cx, buf),
            Self::Zlib(d) => Pin::new(d).poll_read(cx, buf),
            Self::Deflate(d) => Pin::new(d).poll_read(cx, buf),
            Self::Brotli(d) => Pin::new(d).poll_read(cx, buf),
        }
    }
}
struct DecodeReader {
    input: Option<Reader>,
    decoder: Option<Decoder>,
    done: bool,
    kind: Kind,
}
impl AsyncRead for DecodeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(input) = this.input.as_mut() {
            let bytes = ready!(input.as_mut().poll_fill_buf(cx))?;
            // undici creates a decoder only after the first body bytes. Empty
            // compressed responses are empty, even with a Brotli header.
            let Some(&first) = bytes.first() else {
                return Poll::Ready(Ok(()));
            };
            let input = this.input.take().expect("decoder input is present");
            this.decoder = Some(match this.kind {
                Kind::Gzip => {
                    let mut decoder = GzipDecoder::new(input);
                    decoder.multiple_members(true);
                    Decoder::Gzip(decoder)
                }
                // Node's InflateStream chooses zlib vs raw from CMF's method.
                Kind::Deflate if first & 0x0f == 8 => Decoder::Zlib(ZlibDecoder::new(input)),
                Kind::Deflate => Decoder::Deflate(DeflateDecoder::new(input)),
                Kind::Brotli => Decoder::Brotli(BrotliDecoder::new(input)),
            });
        }
        let decoder = this.decoder.as_mut().expect("decoder initialized");
        if !this.done {
            let before = buf.filled().len();
            match ready!(decoder.read(cx, buf)) {
                Ok(()) => {}
                // Node sets finishFlush=Z_SYNC_FLUSH for all three codecs.
                // compression-codecs 0.4.46 reports a truncated flate finish
                // as this specific BufError; corruption and IO errors differ.
                Err(error)
                    if error.kind() == io::ErrorKind::UnexpectedEof
                        || (error.kind() == io::ErrorKind::Other
                            && error.to_string() == "unexpected BufError") => {}
                Err(error) => return Poll::Ready(Err(error)),
            }
            if buf.filled().len() > before {
                return Poll::Ready(Ok(()));
            }
            this.done = true;
        }
        loop {
            let input = decoder.input();
            let bytes = ready!(input.as_mut().poll_fill_buf(cx))?;
            let amount = bytes.len();
            if amount == 0 {
                return Poll::Ready(Ok(()));
            }
            input.as_mut().consume(amount);
        }
    }
}
struct DecodedBody {
    reader: Reader,
    progress: Arc<AtomicBool>,
    done: bool,
}
impl Body for DecodedBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyFailure>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        let mut storage = [0; 8192];
        let mut buf = ReadBuf::new(&mut storage);
        this.progress.store(false, Ordering::Relaxed);
        let result = match this.reader.as_mut().poll_read(cx, &mut buf) {
            Poll::Ready(result) => result,
            Poll::Pending if this.progress.swap(false, Ordering::Relaxed) => {
                // No text yet, but actual wire bytes reset undici's body idle
                // timer. read_frame skips this marker for its application idle.
                return Poll::Ready(Some(Ok(Frame::data(Bytes::new()))));
            }
            Poll::Pending => return Poll::Pending,
        };
        match result {
            Ok(()) if buf.filled().is_empty() => {
                this.done = true;
                Poll::Ready(None)
            }
            Ok(()) => Poll::Ready(Some(Ok(Frame::data(Bytes::copy_from_slice(buf.filled()))))),
            Err(_) => {
                this.done = true;
                Poll::Ready(Some(Err(BodyFailure::Terminated)))
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.done
    }
}
