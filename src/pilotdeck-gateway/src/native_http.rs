//! Non-upgrade HTTP routing for the native listener: `/health`, loopback
//! `/auth/local-token`, the `/api/web` route group (#2354), static fallback,
//! and the plain 404, aligned with the TS `GatewayServer.handleHttpRequest`
//! (fixtures `native-http-basic` and `api-web`). Public approvals and
//! module-host share this production listener; default proxy routing is untouched.
//! Transport byte-level divergences
//! (header name case, chunk-size case, the bare 400's `content-length: 0`,
//! the HTTP/1.0 status line echo) and parse-layer divergences (target
//! charset, the fragment-stripping under-count of hyper's `Uri`, the
//! OWS-padding reach of hyper's read buffer) are registered in the
//! gateway-parity diff ledger; Node's 16384-byte URL+header span budget
//! is replicated here rather than registered.
use crate::{Body, ProxyError, http_proxy};
use bytes::Bytes;
pub(crate) mod control;
use http_body_util::{BodyExt, Full};
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

/// What one `/feishu/webhook` request body produced: the status and the JSON
/// text `respondJson` writes.
pub(crate) type WebhookReply = (u16, String);

/// A live webhook channel's body entry — TS
/// `activeFeishu.handleWebhook(request, response, body)` minus the HTTP write.
pub(crate) type WebhookHandler = std::sync::Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = WebhookReply> + Send>>
        + Send
        + Sync,
>;

/// Per-request lookup of that entry. Resolving per request (rather than
/// capturing one handler) is the TS `activeFeishu?.handleWebhook(…) ?? false`
/// read: a reload replaces the channel, and `stop` clears it, so the route
/// must see the channel that is active *now*. `None` falls through to the
/// rest of the router.
pub(crate) type WebhookResolver = std::sync::Arc<dyn Fn() -> Option<WebhookHandler> + Send + Sync>;

/// Routing inputs for one non-upgrade request. `loopback` mirrors the TS
/// bind-host check (`127.0.0.1` or `localhost`); `static_root` mirrors
/// `staticAssetsPath`. The production native listener is always loopback
/// and serves no static root, so those cells replay through the router.
#[derive(Clone, Default)]
pub(crate) struct Config {
    pub token: String,
    pub loopback: bool,
    pub static_root: Option<PathBuf>,
    /// `options.feishuWebhook` (GatewayServer). The `ANY /feishu/webhook`
    /// route reads the whole request body before dispatching, exactly
    /// `readBody(request)` + `await options.feishuWebhook(...)`.
    pub webhook: Option<WebhookResolver>,
    pub control: Option<control::ControlHandler>,
    /// #2354: the `/api/web` route group (TS `handleWebApiRequest` +
    /// `options.gateway`). `None` keeps the pre-#2354 surface, the shape
    /// every non-gateway listener and frozen fixture still pins.
    pub api_web: Option<crate::api_web::Config>,
    pub approvals: Option<crate::module_host::approvals::Config>,
    /// Public capability callbacks, authenticated by the production host.
    pub public_host: Option<crate::module_host::Config>,
}

/// Route one parsed non-upgrade request to its TS-equivalent response.
/// Only method, version, headers, and URI are consulted, never the body; the
/// body-carrying entries are [`serve_with_body`] (the listener, whose
/// `/api/web`, approvals and module-host routes also read their bodies).
pub(crate) async fn serve<B>(
    request: &hyper::Request<B>,
    config: &Config,
) -> Result<hyper::Response<Body>, io::Error> {
    finish(request.version(), route(request, config, None).await?)
}

/// Route one request whose body may belong to `/feishu/webhook` or the
/// `/api/web`, approvals or module-host routes, collecting only for a live route.
/// Every other path is served exactly like [`serve`] and its body is never
/// read — the streaming request body stays untouched for the routes that do
/// not look at it.
pub(crate) async fn serve_with_body(
    mut request: hyper::Request<hyper::body::Incoming>,
    config: &Config,
) -> Result<hyper::Response<Body>, io::Error> {
    let version = request.version();
    if let Some(approvals) = &config.approvals {
        if let Some(rejection) = node_parser_admission(&request)? {
            return finish(version, rejection);
        }
        if let Some(response) = crate::module_host::approvals::serve(&mut request, approvals).await
        {
            return finish(version, response?);
        }
    }
    if let Some(public_host) = &config.public_host {
        if let Some(rejection) = node_parser_admission(&request)? {
            return finish(version, rejection);
        }
        if let Some(response) = crate::module_host::serve(&mut request, public_host).await {
            return finish(version, response?);
        }
    }
    if config.control.is_some()
        && matches!(
            request_path(&request).as_str(),
            "/_control/context" | "/_control/restart"
        )
    {
        if let Some(rejection) = node_parser_admission(&request)? {
            return finish(version, rejection);
        }
        return control::serve(request, config).await;
    }
    // #2354: the `/api/web` route group, exactly where TS's router mounts it —
    // after the `/feishu/webhook` prefix check, before static and the plain
    // 404 (GatewayServer.ts). The path prefixes are disjoint, so testing this
    // branch first is behaviorally identical to the TS order. TS checks
    // `url.pathname.startsWith("/api/web/")` before calling
    // `handleWebApiRequest`, and that handler re-checks its own prefix; both
    // checks are mirrored (`request_path` here, `serve` inside the group).
    if let Some(api_web) = &config.api_web
        && !crate::mutants::on("native-http-api-web-off")
        && request_path(&request).starts_with("/api/web/")
    {
        // #1765 hard constraint: the llhttp-equivalent admission gates run
        // before any route handler in TS (the parser is under the whole
        // server), so the group answers a bare 400 / headless 400, never its
        // own JSON. The fallthrough `serve` below re-runs the same pure check
        // for requests the group declines, exactly like the webhook branch.
        if !crate::mutants::on("api-web-admission-off")
            && let Some(rejection) = node_parser_admission(&request)?
        {
            return finish(version, rejection);
        }
        // `None` means "not for this surface" (a WHATWG-normalized path the
        // prefix check above did not catch); the caller keeps routing.
        if let Some(answer) = crate::api_web::serve(&mut request, api_web).await {
            return finish(version, answer?);
        }
    }
    let wants_body = config.webhook.is_some() && request_path(&request) == FEISHU_WEBHOOK_PATH;
    if !wants_body {
        return serve(&request, config).await;
    }
    // A request the parser rejects never reaches the handler, so its stream is
    // not drained either (llhttp decides before any body byte is read).
    if let Some(rejection) = node_parser_admission(&request)? {
        return finish(version, rejection);
    }
    let (parts, body) = request.into_parts();
    let mut body = body;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| io::Error::other(format!("request body: {error}")))?;
        if let Ok(data) = frame.into_data() {
            bytes.extend_from_slice(&data);
        }
    }
    // `Buffer.concat(chunks).toString("utf8")`: invalid sequences become U+FFFD.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let head = hyper::Request::from_parts(parts, ());
    finish(version, route(&head, config, Some(&text)).await?)
}

/// The HTTP/1.0 close framing every native response lane shares.
fn finish(
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

/// Node http.Server's parser-level admission, used by the native listeners that
/// back a TS `http.createServer` surface: the module-host router here, the
/// webhook channel's own listener, the api-server channel (#1683), and the
/// sms and user_dialog HTTP-store lanes (#2152). This is
/// not router policy: llhttp and `_http_server.js` enforce it before any request
/// handler runs, so every such listener has to call it. Not yet wired in
/// wecom_callback's private listener (#1774) — that listener predates this
/// gate and still diverges.
/// `Ok(None)` admits the request; `Err` is the zero-byte socket close a
/// CONNECT to a plain http.Server performs (no `connect` listener).
pub(crate) fn node_parser_admission<B>(
    request: &hyper::Request<B>,
) -> Result<Option<hyper::Response<Body>>, io::Error> {
    if !crate::mutants::on("native-http-method-allowlist-off")
        && !LLHTTP_METHODS.contains(&request.method().as_str())
    {
        return Ok(Some(parser_bad_request()));
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
        return Ok(Some(parser_bad_request()));
    }
    // llhttp validates the request-target grammar during the URL scan, so a
    // bad form (authority-form for non-CONNECT, non-alpha scheme) is a bare
    // 400 even when the target is huge, while a huge but grammatical target
    // (a CONNECT authority included) crosses the 16 KiB span budget first.
    // The boundary itself is inclusive: Node's TrackHeader overflows at
    // exactly 16384 accumulated span bytes (probe `probe-fixr3-c.out` C1:
    // tracked 16383 stays served, 16384 is the 431), which is what the
    // off-by-one mutant below removes.
    let budget_exceeded = over_header_budget(request);
    if request.method() == hyper::Method::CONNECT {
        if budget_exceeded {
            return Ok(Some(request_header_fields_too_large()));
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
        && (!llhttp_scheme_alpha(request.uri().scheme())
            || !llhttp_target_form(request.uri().scheme(), target))
    {
        return Ok(Some(parser_bad_request()));
    }
    if budget_exceeded {
        return Ok(Some(request_header_fields_too_large()));
    }
    if request.version() == hyper::Version::HTTP_11
        && !request.headers().contains_key(hyper::header::HOST)
    {
        return Ok(Some(headless_bad_request()));
    }
    Ok(None)
}

/// Node's keep-alive decision for a served response: HTTP/1.1 with no
/// `Connection: close` request token (probe `probe-fixr1.out` A).
pub(crate) fn node_keep_alive<B>(request: &hyper::Request<B>) -> bool {
    !crate::mutants::on("native-http-connection-close-off")
        && request.version() == hyper::Version::HTTP_11
        && !request_wants_close(request)
}

/// `/feishu/webhook`, the one path whose body the router reads
/// (`GatewayServer.ts`).
pub(crate) const FEISHU_WEBHOOK_PATH: &str = "/feishu/webhook";

/// The WHATWG pathname of the request target — `new URL(request.url ??
/// "/", "http://127.0.0.1").pathname`.
fn request_path<B>(request: &hyper::Request<B>) -> String {
    if crate::mutants::on("native-http-normalization-off") {
        return request.uri().path().to_owned();
    }
    if request.uri().scheme().is_some() {
        // Absolute-form: `new URL(request.url)` sees the full target, whose
        // `//` paths are not the scheme-relative collapse the bare
        // path-and-query would resolve to.
        return whatwg_path(&request.uri().to_string());
    }
    let target = request
        .uri()
        .path_and_query()
        .map_or("", |value| value.as_str());
    whatwg_path(target)
}

async fn route<B>(
    request: &hyper::Request<B>,
    config: &Config,
    webhook_body: Option<&str>,
) -> Result<hyper::Response<Body>, io::Error> {
    if let Some(rejection) = node_parser_admission(request)? {
        return Ok(rejection);
    }
    let keep_alive = node_keep_alive(request);
    let path = request_path(request);
    if path == "/health" {
        let mut response = handler_response(
            hyper::StatusCode::OK,
            "application/json",
            "{\"ok\":true}".to_owned(),
            keep_alive,
        );
        if crate::mutants::on("native-http-served-head-cl0")
            && request.method() == hyper::Method::HEAD
        {
            // #2776 negative control: an unregistered served response growing
            // a `content-length: 0` line is the exemption-spread drift the
            // narrowed replay must catch — the health-head cell compares the
            // line verbatim (the manual insert rides the HEAD response hyper
            // writes no framing header of its own for, the same shape the
            // api-web head-404 control uses).
            response
                .headers_mut()
                .insert(hyper::header::CONTENT_LENGTH, "0".parse().expect("header"));
        }
        return Ok(response);
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
    // `if (url.pathname === "/feishu/webhook" && options.feishuWebhook)`:
    // the body was collected before the path was known to match, so a request
    // without one (the plain `serve` entry) never invokes the channel and
    // falls through instead of being served as an empty body.
    if path == FEISHU_WEBHOOK_PATH
        && let (Some(resolve), Some(body)) = (&config.webhook, webhook_body)
        && let Some(handler) = resolve()
    {
        let (status, text) = handler(body.to_owned()).await;
        // `respondJson` is `res.writeHead(status, {content-type:
        // "application/json"}); res.end(JSON.stringify(body))` — a whole
        // reply Node sizes up front, so the body is not chunked.
        let mut response = handler_response_with_length(
            hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR),
            "application/json",
            text,
            keep_alive,
        );
        // #1947: on HTTP/1.0 the reply is close-delimited — Node's 1.0
        // replies carry no `content-length` — so the size stays unknown to
        // hyper, like the webhook channel's own `finalize` (#1684). The
        // parser-level rejections keep their registered `content-length: 0`
        // diff, so this wraps only the webhook branch.
        if request.version() == hyper::Version::HTTP_10
            && !crate::mutants::on("native-http-http10-framing-off")
        {
            let sized = std::mem::replace(response.body_mut(), streamed(None));
            *response.body_mut() = Unsized(sized).boxed();
        }
        return Ok(response);
    }
    // A7.4's user-dialog HTTP store entry is not yet open on this listener: TS
    // never mounts HttpGatewayUserDialogStore on the gateway port either, but
    // #1316 makes the same-domain entry explicit instead of a generic 404 so
    // clients can tell "not yet open" from an unknown path.
    if path.starts_with(crate::user_dialog::http_store::STORE_PATH_PREFIX) {
        return Ok(handler_response(
            hyper::StatusCode::SERVICE_UNAVAILABLE,
            "application/json",
            "{\"error\":\"not_yet_open\",\"store\":\"gateway-user-dialog-store\"}".to_owned(),
            keep_alive,
        ));
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
/// check in [`serve`] and its path may be empty, so a path-and-query that is
/// a bare `?query` is served like TS's 404 (fix-r1 probe `probe-p2.out`),
/// while a bare origin-form `?query` without a scheme stays the bare
/// parser-level 400 (same probe). Authority-form without CONNECT and
/// `%2Fhealth` are a bare parser-level 400 in TS too.
fn llhttp_target_form(scheme: Option<&hyper::http::uri::Scheme>, target: &str) -> bool {
    target.starts_with('/') || target == "*" || (scheme.is_some() && target.starts_with('?'))
}

/// The WHATWG path of the request target, exactly what `new URL(request.url,
/// "http://127.0.0.1").pathname` produces in TS: dot segments collapse,
/// backslashes separate, `%2e` is a dot, fragments and queries split off.
/// This is the traversal defense the static layer relies on.
pub(crate) fn whatwg_path(target: &str) -> String {
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
    // Node's TrackHeader counts the raw request-line target span plus every
    // header line's name and value bytes. hyper's `uri()` splits the target
    // into scheme/authority/path-and-query parts; re-rendering via
    // `to_string()` would allocate a fresh String per request, so the parts
    // are summed in place instead. For every accepted form but one the sum
    // renders byte-for-byte like `to_string()`: the exception is the
    // absolute-form empty-path-with-query target (`GET http://h?x=1`), where
    // `path_and_query()` keeps the raw `"?x=1"` while `Uri::Display` inserts
    // the implicit `/` — there the sum is Node's raw span and the re-render
    // is one byte over it (fix-r1 probe `probe-p2.out`: TS serves the 16383
    // raw span and 431s the 16384 one; the no-query empty-path target stays
    // the registered +1 residual either way, pinned by `pin-budget-absolute`).
    let uri = request.uri();
    let url_span = if crate::mutants::on("native-http-uri-realloc") {
        // The restored `to_string()` re-render the negative control kills:
        // one byte over the raw span on the empty-path+query form, so its
        // 16383 boundary cell 431s.
        uri.to_string().len()
    } else {
        uri.scheme().map_or(0, |value| value.as_str().len() + 3)
            + uri.authority().map_or(0, |value| value.as_str().len())
            + uri.path_and_query().map_or(0, |value| value.as_str().len())
    };
    url_span
        + request
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
}

/// The 16 KiB span-budget decision shared by both native lanes: Node's
/// TrackHeader counts every parsed request, upgrade requests included
/// (#1292 probe `b4-U1.out`: an over-budget `/ws` upgrade head is the same
/// 431 + close as the non-upgrade `b4-U2.out`), so the upgrade diversion
/// checks this before it diverts, instead of leaving upgrade heads outside
/// the budget. The boundary itself is cell-recorded on both lanes by the
/// `native-ws-upgrade-budget` fixture (#1351: tracked 16383 stays served,
/// 16384 is the 431, upgrade and non-upgrade alike).
pub(crate) fn over_header_budget<B>(request: &hyper::Request<B>) -> bool {
    let tracked = tracked_header_bytes(request);
    let over_budget = if crate::mutants::on("native-http-header-budget-boundary") {
        tracked > 16_384
    } else {
        tracked >= 16_384
    };
    !crate::mutants::on("native-http-header-budget-off") && over_budget
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
struct Unsized(Body);

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

pub(crate) fn streamed(body: Option<Bytes>) -> Body {
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
    if crate::mutants::on("native-http-content-type-txt") && extension == ".txt" {
        // #1383: simulates the mapping gaining a `.txt` entry, which would
        // break the premise `expected_response` borrows the `static-upper`
        // golden under — the pinned test fails at its own assertion.
        "text/plain; charset=utf-8"
    } else {
        CONTENT_TYPES
            .iter()
            .find(|(known, _)| *known == extension)
            .map_or("application/octet-stream", |(_, kind)| kind)
    }
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
