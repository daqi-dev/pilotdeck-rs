//! Non-upgrade HTTP routing for the native listener: `/health`, loopback
//! `/auth/local-token`, static fallback, and the plain 404, aligned with the
//! TS `GatewayServer.handleHttpRequest` (fixture `native-http-basic`).
//! `/api/web`, public approvals, and module-host stay with TS, and the
//! default proxy routing is untouched. Transport byte-level divergences
//! (header name case, chunk-size case, the bare 400's `content-length: 0`,
//! the HTTP/1.0 status line echo) and parse-layer divergences (target
//! charset, the fragment-stripping under-count of hyper's `Uri`, the
//! OWS-padding reach of hyper's read buffer) are registered in the
//! gateway-parity diff ledger; Node's 16384-byte URL+header span budget
//! is replicated here rather than registered.
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
    let mut response = route(request, config).await?;
    if request.version() == hyper::Version::HTTP_10 {
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

async fn route<B>(
    request: &hyper::Request<B>,
    config: &Config,
) -> Result<hyper::Response<Body>, io::Error> {
    if !crate::mutants::on("native-http-method-allowlist-off")
        && !LLHTTP_METHODS.contains(&request.method().as_str())
    {
        return Ok(parser_bad_request());
    }
    // llhttp validates the request-target bytes before method dispatch, so a
    // non-ASCII target is a bare 400 even for CONNECT; authority-form targets
    // live in `uri()`'s authority while origin/absolute-form targets live in
    // its path-and-query, so both parts are what is checked (the scheme
    // grammar is ASCII-only by construction).
    if !crate::mutants::on("native-http-target-bytes-off")
        && (request
            .uri()
            .authority()
            .is_some_and(|authority| authority.as_str().bytes().any(|byte| byte >= 0x80))
            || request
                .uri()
                .path_and_query()
                .is_some_and(|path| path.as_str().bytes().any(|byte| byte >= 0x80)))
    {
        return Ok(parser_bad_request());
    }
    // llhttp validates the request-target grammar during the URL scan, so a
    // bad form (authority-form for non-CONNECT, non-alpha scheme) is a bare
    // 400 even when the target is huge, while a huge but grammatical target
    // (a CONNECT authority included) crosses the 16 KiB span budget first.
    // The boundary itself is inclusive: Node's TrackHeader overflows at
    // exactly 16384 accumulated span bytes (probe `probe-fixr3-c.out` C1:
    // tracked 16383 stays served, 16384 is the 431), which is what the
    // off-by-one mutant below removes.
    let tracked = tracked_header_bytes(request);
    let over_budget = if crate::mutants::on("native-http-header-budget-boundary") {
        tracked > 16_384
    } else {
        tracked >= 16_384
    };
    let budget_exceeded = !crate::mutants::on("native-http-header-budget-off") && over_budget;
    if request.method() == hyper::Method::CONNECT {
        if budget_exceeded {
            return Ok(request_header_fields_too_large());
        }
        // TS destroys the socket with no bytes; a service error makes hyper
        // close the connection without writing a response.
        return Err(io::Error::other("CONNECT rejected by native HTTP routing"));
    }
    let target = request
        .uri()
        .path_and_query()
        .map_or("", |value| value.as_str());
    if !crate::mutants::on("native-http-target-form-off")
        && (!llhttp_scheme_alpha(request.uri().scheme()) || !llhttp_target_form(target))
    {
        return Ok(parser_bad_request());
    }
    if budget_exceeded {
        return Ok(request_header_fields_too_large());
    }
    if request.version() == hyper::Version::HTTP_11
        && !request.headers().contains_key(hyper::header::HOST)
    {
        return Ok(headless_bad_request());
    }
    let keep_alive = !crate::mutants::on("native-http-connection-close-off")
        && request.version() == hyper::Version::HTTP_11
        && !request_wants_close(request);
    let path = if crate::mutants::on("native-http-normalization-off") {
        request.uri().path().to_owned()
    } else if request.uri().scheme().is_some() {
        // Absolute-form: `new URL(request.url)` sees the full target, whose
        // `//` paths are not the scheme-relative collapse the bare
        // path-and-query would resolve to.
        whatwg_path(&request.uri().to_string())
    } else {
        whatwg_path(target)
    };
    if path == "/health" {
        return Ok(handler_response(
            hyper::StatusCode::OK,
            "application/json",
            "{\"ok\":true}".to_owned(),
            keep_alive,
        ));
    }
    if path == "/auth/local-token" {
        if config.loopback || crate::mutants::on("native-http-token-loopback-skip") {
            return Ok(handler_response(
                hyper::StatusCode::OK,
                "application/json",
                format!("{{\"token\":{}}}", json_string(&config.token)),
                keep_alive,
            ));
        }
        return Ok(not_found(keep_alive));
    }
    let static_root = config.static_root.as_deref().map(|root| {
        if crate::mutants::on("native-http-root-normalize-off") {
            root.to_path_buf()
        } else {
            normalize_root(root)
        }
    });
    if let Some(root) = &static_root
        && let Some(response) = serve_static(root, &path, keep_alive).await
    {
        return Ok(response);
    }
    Ok(not_found(keep_alive))
}

/// Node v22.22.0 answers `Connection: close` requests and every HTTP/1.0
/// request with `Connection: close` and no `Keep-Alive` header, closing the
/// socket after the response (probe `probe-fixr1.out` A).
fn request_wants_close<B>(request: &hyper::Request<B>) -> bool {
    request
        .headers()
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// llhttp accepts only all-alphabetic request-target schemes (`a1://`, `ab+c:`
/// are parse-time 400s in TS); http::Uri's own scheme grammar is wider, so the
/// stricter rule is re-imposed here on the parsed scheme.
fn llhttp_scheme_alpha(scheme: Option<&hyper::http::uri::Scheme>) -> bool {
    scheme.is_none_or(|scheme| {
        scheme
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic())
    })
}

/// llhttp's request-target shape rule for the path-and-query: origin-form
/// (leading `/`) or asterisk (`*`). Absolute-form is admitted by the scheme
/// check in [`serve`]; anything else — `%2Fhealth`, authority-form without
/// CONNECT — is a bare parser-level 400 in TS.
fn llhttp_target_form(target: &str) -> bool {
    target.starts_with('/') || target == "*"
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

/// The byte budget Node v22.22.0 actually enforces (node_http_parser.cc
/// `TrackHeader`, probes `probe-fixr2.out` A/B and `probe-fixr3-c.out` C1):
/// the current message's URL span plus every header-name and header-value
/// span accumulate (separators and CRLF excluded; leading value OWS skipped,
/// trailing value OWS counted; reset per message and per chunk), and reaching
/// 16384 is the parser's header overflow — the canned 431 below. hyper
/// trims a value's trailing OWS before this runs, so a trailing-OWS-padded
/// value under-counts here, and hyper also strips the fragment from the
/// parsed URI (both registered residuals in the parity ledger).
fn tracked_header_bytes<B>(request: &hyper::Request<B>) -> usize {
    request.uri().to_string().len()
        + request
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
}

/// Node's canned 431 (`_http_server.js` `requestHeaderFieldsTooLargeResponse`):
/// `HTTP/1.1 431 Request Header Fields Too Large`, `Connection: close`, no
/// Date, no body bytes — the same shape as the bare 400 (hyper's
/// `content-length: 0` transport diff included).
fn request_header_fields_too_large() -> hyper::Response<Body> {
    let mut response = hyper::Response::new(http_proxy::empty());
    *response.status_mut() = hyper::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE;
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
fn not_found(keep_alive: bool) -> hyper::Response<Body> {
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
fn handler_response(
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

/// A streaming file response with the same framing as
/// `createReadStream().pipe(response)`.
fn file_response(
    content_type: &str,
    file: tokio::fs::File,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder()
        .status(hyper::StatusCode::OK)
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
async fn serve_static(root: &Path, path: &str, keep_alive: bool) -> Option<hyper::Response<Body>> {
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
    Some(file_response(
        content_type_for(&file_path),
        file,
        keep_alive,
    ))
}

/// `resolve(staticRoot)` (staticAssets.ts:14): lexical fold of `.`/`..` and
/// trailing separators, made absolute against the process directory for a
/// relative root. No `canonicalize`, so symlinks stay the caller's choice.
/// A root of `/` is kept: TS's guard then 404s everything (the required
/// prefix would be `//`), and a failed cwd lookup maps to `/` so nothing
/// escapes through an empty prefix.
fn normalize_root(root: &Path) -> PathBuf {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(root)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
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
