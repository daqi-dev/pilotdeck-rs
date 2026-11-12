//! Shared native HTTP response construction and body framing.
use crate::{Body, ProxyError, http_proxy};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::SystemTime,
};

/// The HTTP/1.0 close framing every native response lane shares.
pub(super) fn finish(
    version: hyper::Version,
    mut response: hyper::Response<Body>,
) -> Result<hyper::Response<Body>, io::Error> {
    if version == hyper::Version::HTTP_10 {
        // Node v22.22.0 answers every HTTP/1.0 request with `Connection:
        // close` and a closed socket, even one asking to keep alive (probe
        // `probe-fixr1.out` A). An explicit 1.0 response version keeps hyper's
        // `fix_keep_alive` (proto/h1/conn.rs) in its HTTP/1.0 branch, which
        // disables the connection's keep-alive instead of replacing that
        // `Connection: close` with `keep-alive` because the request asked to
        // keep alive.
        *response.version_mut() = hyper::Version::HTTP_10;
    }
    Ok(response)
}

/// llhttp's bare parser-level rejection: 400, `Connection: close`, no Date,
/// no body bytes. hyper adds `content-length: 0` for the empty sized body
/// (registered transport diff); the connection still closes.
pub(super) fn parser_bad_request() -> hyper::Response<Body> {
    let mut response =
        hyper::Response::new(if crate::mutants::on("native-http-parser-400-cl0-off") {
            // #2153 negative control: the streamed empty body drops hyper's
            // implicit `content-length: 0`, the registered-direction drift the
            // api-web gate pin holds.
            streamed(None)
        } else {
            http_proxy::empty()
        });
    *response.status_mut() = hyper::StatusCode::BAD_REQUEST;
    response
        .headers_mut()
        .insert(hyper::header::CONNECTION, "close".parse().expect("header"));
    response
}

/// Node's canned 431 (`_http_server.js` `requestHeaderFieldsTooLargeResponse`):
/// `HTTP/1.1 431 Request Header Fields Too Large`, `Connection: close`, no
/// Date, no body bytes — the same shape as the bare 400 (hyper's
/// `content-length: 0` transport diff included).
pub(crate) fn request_header_fields_too_large() -> hyper::Response<Body> {
    let mut response = hyper::Response::new(if crate::mutants::on("native-http-431-cl0-off") {
        // #2153 negative control: the streamed empty body drops hyper's
        // implicit `content-length: 0`, the registered-direction drift the
        // budget replay's raw pin holds.
        streamed(None)
    } else {
        http_proxy::empty()
    });
    *response.status_mut() = hyper::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE;
    response
        .headers_mut()
        .insert(hyper::header::CONNECTION, "close".parse().expect("header"));
    response
}

/// Node's missing-Host rejection on HTTP/1.1: 400 with `Connection: close`,
/// a Date, and a chunked empty body.
pub(super) fn headless_bad_request() -> hyper::Response<Body> {
    let mut response = hyper::Response::new(streamed(None));
    *response.status_mut() = hyper::StatusCode::BAD_REQUEST;
    let headers = response.headers_mut();
    headers.insert(hyper::header::CONNECTION, "close".parse().expect("header"));
    headers.insert(
        hyper::header::DATE,
        httpdate::fmt_http_date(SystemTime::now())
            .parse()
            .expect("header"),
    );
    response
}

/// The final 404: `text/plain; charset=utf-8`, body `not found`.
pub(super) fn not_found(keep_alive: bool) -> hyper::Response<Body> {
    let message = if crate::mutants::on("native-http-404-message") {
        "Not Found"
    } else {
        "not found"
    };
    handler_response(
        hyper::StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        message.to_owned(),
        keep_alive,
    )
}

/// A TS `writeHead` + `end` response: unknown-length body (chunked), the
/// literal `content-type`, Node's `Date`, and either Node's default keep-alive
/// pair or `Connection: close` for close/HTTP/1.0 requests, in Node's exact
/// header order.
pub(crate) fn handler_response(
    status: hyper::StatusCode,
    content_type: &str,
    body: String,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    if keep_alive {
        builder = builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5");
    } else {
        builder = builder.header(hyper::header::CONNECTION, "close");
    }
    builder
        .body(streamed(Some(Bytes::from(body))))
        .expect("valid response")
}

/// A whole-reply body Node sizes up front (`end(string)`): hyper frames it
/// with `content-length`, never chunked.
fn sized_body(body: Option<String>) -> Body {
    Full::new(body.map(Bytes::from).unwrap_or_default())
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}

/// A body whose size hyper cannot know (#1947): everything else delegates, so
/// the frames and the end-of-stream flag are unchanged. Sibling of the
/// webhook channel's `Unsized` (#1684), wrapping the one sized body this
/// router builds for an HTTP/1.0 reply (the webhook branch's
/// `handler_response_with_length`).
pub(super) struct Unsized(pub(super) Body);

impl hyper::body::Body for Unsized {
    type Data = Bytes;
    type Error = crate::ProxyError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::SizeHint::default()
    }
}

/// A TS `writeHead(status, headers)` followed by `end(body)` with the whole
/// body in one call: Node sizes the reply, so unlike the streaming
/// [`handler_response`] this is `content-length` framed. The webhook
/// channel's `sendJson` reply (`Content-Type: application/json`).
pub(crate) fn handler_response_with_length(
    status: hyper::StatusCode,
    content_type: &str,
    body: String,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    if keep_alive {
        builder = builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5");
    } else {
        builder = builder.header(hyper::header::CONNECTION, "close");
    }
    builder
        .body(sized_body(Some(body)))
        .expect("valid response")
}

/// A TS `statusCode = <status>; end(body)` response with no explicit header:
/// like [`handler_response_with_length`] minus the `content-type` line, so
/// Node's implicit `Date` comes first. Used by the webhook channel's plain
/// 404 (`res.statusCode = 404; res.end("Not Found")`).
pub(crate) fn node_plain_response(
    status: hyper::StatusCode,
    body: Option<String>,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder().status(status).header(
        hyper::header::DATE,
        httpdate::fmt_http_date(SystemTime::now()),
    );
    if keep_alive {
        builder = builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5");
    } else {
        builder = builder.header(hyper::header::CONNECTION, "close");
    }
    builder.body(sized_body(body)).expect("valid response")
}

/// Unknown-length single-frame body: hyper cannot know the size, so it frames
/// the response chunked like Node's `writeHead` + `end`.
struct StreamedBody(Option<Bytes>);
impl hyper::body::Body for StreamedBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        Poll::Ready(
            self.0
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

pub(crate) fn streamed(body: Option<Bytes>) -> Body {
    StreamedBody(body)
        .map_err(|error: ProxyError| error)
        .boxed()
}

/// `JSON.stringify` for one string value: quote, backslash, and the C0
/// escapes JSON.stringify emits, other characters passed through.
pub(super) fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}
