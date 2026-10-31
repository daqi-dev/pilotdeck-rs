//! Non-upgrade HTTP routing for the native listener: `/health`, loopback
//! `/auth/local-token`, static fallback, and the plain 404, aligned with the
//! TS `GatewayServer.handleHttpRequest` (fixture `native-http-basic`).
//! `/api/web`, public approvals, and module-host stay with TS, and the
//! default proxy routing is untouched. Transport byte-level divergences
//! (header name case, chunk-size case, the bare 400's `content-length: 0`)
//! are registered in the gateway-parity diff ledger.
use crate::{Body, ProxyError, http_proxy};
use bytes::Bytes;
use http_body_util::BodyExt;
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
    time::SystemTime,
};
use tokio::io::{AsyncRead, ReadBuf};

/// Node v22.22.0 `http.METHODS`, the exact llhttp method table recorded in
/// the golden oracle: llhttp rejects any other method token with a bare 400
/// and closes the socket, so the router must reject it too (httparse
/// accepts arbitrary tokens and dispatches them).
const LLHTTP_METHODS: [&str; 35] = [
    "ACL",
    "BIND",
    "CHECKOUT",
    "CONNECT",
    "COPY",
    "DELETE",
    "GET",
    "HEAD",
    "LINK",
    "LOCK",
    "M-SEARCH",
    "MERGE",
    "MKACTIVITY",
    "MKCALENDAR",
    "MKCOL",
    "MOVE",
    "NOTIFY",
    "OPTIONS",
    "PATCH",
    "POST",
    "PROPFIND",
    "PROPPATCH",
    "PURGE",
    "PUT",
    "QUERY",
    "REBIND",
    "REPORT",
    "SEARCH",
    "SOURCE",
    "SUBSCRIBE",
    "TRACE",
    "UNBIND",
    "UNLINK",
    "UNLOCK",
    "UNSUBSCRIBE",
];

/// The static extension map, exactly `staticAssets.ts`'s `CONTENT_TYPES`:
/// the lookup is case-sensitive (`.TXT` misses) and keyed on the final
/// served file's extension (the index fallback serves `.html`).
const CONTENT_TYPES: [(&str, &str); 5] = [
    (".html", "text/html; charset=utf-8"),
    (".js", "text/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".json", "application/json; charset=utf-8"),
    (".svg", "image/svg+xml"),
];

/// Routing inputs for one non-upgrade request. `loopback` mirrors the TS
/// bind-host check (`127.0.0.1` or `localhost`); `static_root` mirrors
/// `staticAssetsPath`. The production native listener is always loopback
/// and serves no static root, so those cells replay through the router.
#[derive(Clone)]
pub(crate) struct Config {
    pub token: String,
    pub loopback: bool,
    pub static_root: Option<PathBuf>,
}

/// Route one parsed non-upgrade request to its TS-equivalent response.
/// Only method, version, headers, and URI are consulted, never the body.
pub(crate) async fn serve<B>(
    request: &hyper::Request<B>,
    config: &Config,
) -> Result<hyper::Response<Body>, io::Error> {
    if !crate::mutants::on("native-http-method-allowlist-off")
        && !LLHTTP_METHODS.contains(&request.method().as_str())
    {
        return Ok(parser_bad_request());
    }
    if request.method() == hyper::Method::CONNECT {
        // TS destroys the socket with no bytes; a service error makes hyper
        // close the connection without writing a response.
        return Err(io::Error::other("CONNECT rejected by native HTTP routing"));
    }
    let target = request
        .uri()
        .path_and_query()
        .map_or("", |value| value.as_str());
    if !crate::mutants::on("native-http-target-form-off") && !llhttp_target_form(target) {
        return Ok(parser_bad_request());
    }
    if !crate::mutants::on("native-http-target-bytes-off")
        && target.bytes().any(|byte| byte >= 0x80)
    {
        return Ok(parser_bad_request());
    }
    if request.version() == hyper::Version::HTTP_11
        && !request.headers().contains_key(hyper::header::HOST)
    {
        return Ok(headless_bad_request());
    }
    let path = if crate::mutants::on("native-http-normalization-off") {
        request.uri().path().to_owned()
    } else {
        whatwg_path(target)
    };
    if path == "/health" {
        return Ok(handler_response(
            hyper::StatusCode::OK,
            "application/json",
            "{\"ok\":true}".to_owned(),
        ));
    }
    if path == "/auth/local-token" {
        if config.loopback || crate::mutants::on("native-http-token-loopback-skip") {
            return Ok(handler_response(
                hyper::StatusCode::OK,
                "application/json",
                format!("{{\"token\":{}}}", json_string(&config.token)),
            ));
        }
        return Ok(not_found());
    }
    if let Some(root) = &config.static_root
        && let Some(response) = serve_static(root, &path).await
    {
        return Ok(response);
    }
    Ok(not_found())
}

/// llhttp's request-target shape rule: origin-form (leading `/`), asterisk
/// (`*`), or absolute-form (`scheme "://" ...`). Anything else — `%2Fhealth`,
/// authority-form without CONNECT — is a bare parser-level 400 in TS.
fn llhttp_target_form(target: &str) -> bool {
    if target.starts_with('/') || target == "*" {
        return true;
    }
    target.split_once("://").is_some_and(|(scheme, _)| {
        scheme
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
            && scheme
                .bytes()
                .skip(1)
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}

/// The WHATWG path of the request target, exactly what `new URL(request.url,
/// "http://127.0.0.1").pathname` produces in TS: dot segments collapse,
/// backslashes separate, `%2e` is a dot, fragments and queries split off.
/// This is the traversal defense the static layer relies on.
fn whatwg_path(target: &str) -> String {
    static BASE: std::sync::OnceLock<url::Url> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| url::Url::parse("http://127.0.0.1").expect("valid base URL"));
    url::Url::options()
        .base_url(Some(base))
        .parse(target)
        .map_or_else(|_| "/".to_owned(), |parsed| parsed.path().to_owned())
}

/// llhttp's bare parser-level rejection: 400, `Connection: close`, no Date,
/// no body bytes. hyper adds `content-length: 0` for the empty sized body
/// (registered transport diff); the connection still closes.
fn parser_bad_request() -> hyper::Response<Body> {
    let mut response = hyper::Response::new(http_proxy::empty());
    *response.status_mut() = hyper::StatusCode::BAD_REQUEST;
    response
        .headers_mut()
        .insert(hyper::header::CONNECTION, "close".parse().expect("header"));
    response
}

/// Node's missing-Host rejection on HTTP/1.1: 400 with `Connection: close`,
/// a Date, and a chunked empty body.
fn headless_bad_request() -> hyper::Response<Body> {
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
fn not_found() -> hyper::Response<Body> {
    let message = if crate::mutants::on("native-http-404-message") {
        "Not Found"
    } else {
        "not found"
    };
    handler_response(
        hyper::StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        message.to_owned(),
    )
}

/// A TS `writeHead` + `end` response: unknown-length body (chunked), the
/// literal `content-type`, Node's `Date`, and Node's default keep-alive pair,
/// in Node's exact header order.
fn handler_response(
    status: hyper::StatusCode,
    content_type: &str,
    body: String,
) -> hyper::Response<Body> {
    hyper::Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        )
        .header(hyper::header::CONNECTION, "keep-alive")
        .header("keep-alive", "timeout=5")
        .body(streamed(Some(Bytes::from(body))))
        .expect("valid response")
}

/// A streaming file response with the same framing as
/// `createReadStream().pipe(response)`.
fn file_response(content_type: &str, file: tokio::fs::File) -> hyper::Response<Body> {
    hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        )
        .header(hyper::header::CONNECTION, "keep-alive")
        .header("keep-alive", "timeout=5")
        .body(FileBody { file, done: false }.boxed())
        .expect("valid response")
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

fn streamed(body: Option<Bytes>) -> Body {
    StreamedBody(body)
        .map_err(|error: ProxyError| error)
        .boxed()
}

/// Streams one frame per 64 KiB read, the cadence of Node's createReadStream
/// `highWaterMark`. Chunk boundaries of files larger than one buffer are
/// outside the golden matrix.
struct FileBody {
    file: tokio::fs::File,
    done: bool,
}
impl hyper::body::Body for FileBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        if self.done {
            return Poll::Ready(None);
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let mut read = ReadBuf::new(&mut buffer);
        match Pin::new(&mut self.file).poll_read(context, &mut read) {
            Poll::Ready(Ok(())) => {
                let read = read.filled().len();
                if read == 0 {
                    self.done = true;
                    Poll::Ready(None)
                } else {
                    buffer.truncate(read);
                    Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from(buffer)))))
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(Box::new(error) as ProxyError))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `serveStaticAsset` semantics for one already-normalized path: `/` maps to
/// `index.html`, a missing path or directory falls back to the root
/// `index.html`, and the containment guard rejects anything that would
/// escape the root (defense in depth behind the URL layer).
async fn serve_static(root: &Path, path: &str) -> Option<hyper::Response<Body>> {
    let pathname = if path == "/" { "/index.html" } else { path };
    let candidate = node_join(root, pathname);
    if !crate::mutants::on("native-http-containment-off") && !contained(root, &candidate) {
        return None;
    }
    let file_path = if is_file(&candidate).await {
        candidate
    } else {
        let fallback = node_join(root, "/index.html");
        if !is_file(&fallback).await {
            return None;
        }
        fallback
    };
    let file = tokio::fs::File::open(&file_path).await.ok()?;
    Some(file_response(content_type_for(&file_path), file))
}

/// Node's `resolve(join(root, pathname))`: lexical join where `.` keeps the
/// position and `..` pops, exactly what `path.resolve` folds so the
/// containment guard sees the final path a traversal would name.
fn node_join(root: &Path, pathname: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    for segment in pathname.split('/').filter(|segment| !segment.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                joined.pop();
            }
            _ => joined.push(segment),
        }
    }
    joined
}

/// `candidate !== root && !candidate.startsWith(root + sep)`, TS's own guard.
fn contained(root: &Path, candidate: &Path) -> bool {
    match (root.to_str(), candidate.to_str()) {
        (Some(root), Some(candidate)) => {
            candidate == root || candidate.starts_with(&format!("{root}/"))
        }
        // Non-UTF-8 paths never reach this point in practice; reject them.
        _ => false,
    }
}

async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
}

/// `CONTENT_TYPES[filePath.slice(filePath.lastIndexOf("."))]` including the
/// JavaScript quirk that a path without a dot slices the final character,
/// which never matches an extension key.
fn content_type_for(file_path: &Path) -> &'static str {
    let name = file_path.to_string_lossy();
    let extension = name.rfind('.').map_or("", |at| &name[at..]);
    CONTENT_TYPES
        .iter()
        .find(|(known, _)| *known == extension)
        .map_or("application/octet-stream", |(_, kind)| kind)
}

/// `JSON.stringify` for one string value: quote, backslash, and the C0
/// escapes JSON.stringify emits, other characters passed through.
fn json_string(value: &str) -> String {
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

#[cfg(test)]
mod tests;
