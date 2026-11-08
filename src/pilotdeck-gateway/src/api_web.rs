//! #1348 `/api/web` route group: the web-UI HTTP surface of TS
//! `adapters/web/httpRouter.ts` (baseline cc7c4e6), with the file endpoints
//! backed by the `projectFiles.ts` replica in [`files`]. `serve` answers
//! `None` for every request whose WHATWG path is not under `/api/web/`,
//! like TS `handleWebApiRequest` returning `false` — the caller keeps
//! routing. #2354 wires the group into `native_http::serve_with_body`, the
//! production listener entry, behind the same llhttp-equivalent admission
//! gates every other route answers (#1765).
//!
//! Two registered divergences (specs/gateway-parity/diffs/A8.4-API-WEB-001.md):
//! a files-branch project key whose percent-encoding is malformed makes TS
//! `decodeURIComponent` throw outside the branch's try, so the rejection is
//! unhandled and the connection hangs — Rust answers a 500 `internal_error`
//! instead; and the hyper HTTP/1.0 status line of the A8.4-NATIVE-HTTP-001
//! entry applies to the `http10` cell. Byte-level header/chunk-case effects
//! of the hyper codec are the same entry's and are normalized by the replay.
//!
//! One security divergence (#1675, `specs/gateway-parity/diffs/A8.4-API-WEB-002.md`,
//! approved by its R1 correction, `A8.4-API-WEB-002-R1`): the file service
//! reads, writes and enumerates through the A4-FILE-TOCTOU-001 NOFOLLOW
//! descriptor walk, so a component swapped after the workspace gate fails
//! closed with 403 `workspace_boundary` where TS follows it out of the root.
mod files;
#[cfg(test)]
mod tests;

use crate::{Body, metadata};
use bytes::Bytes;
use http_body_util::BodyExt;
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use std::{future::Future, io, path::PathBuf, pin::Pin, sync::Arc, time::SystemTime};

const UNAUTHORIZED_BODY: &str =
    "{\"error\":{\"code\":\"unauthorized\",\"message\":\"missing or invalid token\"}}";
const MISSING_PATH_BODY: &str =
    "{\"error\":{\"code\":\"missing_path\",\"message\":\"?path required\"}}";
const INVALID_BODY: &str = "{\"error\":{\"code\":\"invalid_body\",\"message\":\"Expected { path, content[, encoding] }\"}}";
const WRITE_OK_BODY: &str = "{\"ok\":true}";

/// The `options.gateway` seam of the TS router: `listProjects` and
/// `describeProject` are async in TS, so the trait hands out boxed futures.
/// A `describe_project` error models the TS throw the router renders as
/// `project_not_found`.
pub(crate) trait ProjectCatalog: Send + Sync {
    fn list_projects(&self) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>>;
    fn describe_project(
        &self,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>>;
}

/// The production catalog over the native metadata registry. Its work is
/// synchronous (the `metadata` module's contract); the boxed future keeps
/// the seam async like TS.
pub(crate) struct NativeProjectCatalog {
    metadata: metadata::Metadata<metadata::NativeFiles>,
}

impl NativeProjectCatalog {
    pub(crate) fn new(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            metadata: metadata::Metadata {
                home,
                cwd,
                files: metadata::NativeFiles,
            },
        }
    }
    /// The blocking-pool copy of the registry. `Metadata<NativeFiles>` is two
    /// paths over the stateless filesystem backend, so the snapshot costs two
    /// PathBuf allocations and satisfies `spawn_blocking`'s `'static` bound
    /// without reshaping `Metadata` or borrowing `self` across the pool.
    fn snapshot_metadata(&self) -> metadata::Metadata<metadata::NativeFiles> {
        metadata::Metadata {
            home: self.metadata.home.clone(),
            cwd: self.metadata.cwd.clone(),
            files: metadata::NativeFiles,
        }
    }
}

impl ProjectCatalog for NativeProjectCatalog {
    fn list_projects(&self) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>> {
        // #2450: the registry walk is synchronous std::fs (readdir plus a
        // stat and marker read per project), so it runs on the blocking pool
        // like the files branch's #1740 — a slow disk stalls no reactor
        // thread. The pool sees a snapshot of the same home/cwd and answers
        // the same bytes; only the executing thread changes. A join failure
        // (panic or shutdown) surfaces as the catalog error the caller
        // already renders as the generic 500.
        let metadata = self.snapshot_metadata();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || metadata.list_projects())
                .await
                .map_err(|error| io::Error::other(format!("api-web catalog task: {error}")))?
        })
    }
    fn describe_project(
        &self,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>> {
        // Same #2450 move as `list_projects`; the key travels as the same
        // JsString the registry compares against.
        let metadata = self.snapshot_metadata();
        let key = JsString::from(key);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || metadata.describe_project(&key))
                .await
                .map_err(|error| io::Error::other(format!("api-web catalog task: {error}")))?
        })
    }
}

/// The router's inputs: the bearer token and the project catalog.
#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) token: String,
    pub(crate) catalog: Arc<dyn ProjectCatalog>,
}

/// Route one request through the `/api/web` group. `None` means the request
/// is not for this surface and the caller keeps routing, like TS returning
/// `false`; `Some(Err)` is a zero-response connection close (a request body
/// that dies mid-read — TS's catch tries a 500 onto an already-dead socket,
/// which Node discards).
pub(crate) async fn serve(
    request: &mut hyper::Request<hyper::body::Incoming>,
    config: &Config,
) -> Option<Result<hyper::Response<Body>, io::Error>> {
    let version = request.version();
    let target = request
        .uri()
        .path_and_query()
        .map_or("", |value| value.as_str());
    let url = whatwg_url(target)?;
    let path = url.path().to_owned();
    if !path.starts_with("/api/web/") {
        return None;
    }
    let query = url.query().unwrap_or_default().to_owned();
    let keep_alive = version == hyper::Version::HTTP_11 && !request_wants_close(request);
    // The bearer check runs before any routing: an unauthorized POST is the
    // 401, not a 404.
    let authorized = crate::mutants::on("api-web-auth-off")
        || request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .is_some_and(|value| {
                bearer_capture(value.as_bytes())
                    .is_some_and(|capture| capture == config.token.as_bytes())
            });
    if !authorized {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::UNAUTHORIZED,
                UNAUTHORIZED_BODY,
                keep_alive,
            ),
            version,
        )));
    }
    if path == "/api/web/projects"
        && (request.method() == hyper::Method::GET
            || crate::mutants::on("api-web-projects-method-off"))
    {
        // Residual #2 of specs/gateway-parity/diffs/A8.4-API-WEB-001.md: TS
        // awaits `listProjects()` with no try/catch, so a rejection writes no
        // response at all (measured: zero bytes, connection either held open
        // or closed, depending on the host's unhandled-rejection listener).
        // Rust answers the generic 500 instead of reproducing the hang.
        return Some(Ok(finalize(
            match config.catalog.list_projects().await {
                Ok(projects) => send_json_value(hyper::StatusCode::OK, &projects, keep_alive),
                Err(error) => send_json(
                    hyper::StatusCode::INTERNAL_SERVER_ERROR,
                    &error_body("internal_error", &error.to_string()),
                    keep_alive,
                ),
            },
            version,
        )));
    }
    let matched = project_match(&path);
    // Describe: a key with no sub-path (or just "/"), GET only.
    if let Some((key_raw, sub)) = &matched
        && !key_raw.is_empty()
        && matches!(sub.as_deref(), None | Some("/"))
        && request.method() == hyper::Method::GET
    {
        let described = match decode_uri_component(key_raw) {
            Ok(key) => config.catalog.describe_project(&key).await,
            // decodeURIComponent runs inside the describe try, so its
            // URIError takes the same 404 as a catalog throw.
            Err(()) => Err(io::Error::other("URI malformed")),
        };
        let catch_off = crate::mutants::on("api-web-describe-catch-off");
        return Some(match described {
            Ok(summary) => Ok(finalize(
                send_json_value(hyper::StatusCode::OK, &summary, keep_alive),
                version,
            )),
            Err(error) if catch_off => Err(error),
            Err(error) => Ok(finalize(
                send_json(
                    hyper::StatusCode::NOT_FOUND,
                    &error_body("project_not_found", &error.to_string()),
                    keep_alive,
                ),
                version,
            )),
        });
    }
    // Files: a key with a real sub-path.
    if let Some((key_raw, Some(sub))) = &matched
        && !key_raw.is_empty()
    {
        // Registered diff: this decodeURIComponent sits outside the branch's
        // try in TS, so a malformed key is an unhandled rejection there (the
        // connection hangs); Rust answers a 500.
        let Ok(key) = decode_uri_component(key_raw) else {
            return Some(Ok(finalize(
                send_json(
                    hyper::StatusCode::INTERNAL_SERVER_ERROR,
                    &error_body("internal_error", "URI malformed"),
                    keep_alive,
                ),
                version,
            )));
        };
        return Some(
            match files_branch(request, sub, &key, &query, &path, keep_alive).await {
                Ok(response) => Ok(finalize(response, version)),
                Err(error) => Err(error),
            },
        );
    }
    Some(Ok(finalize(
        send_json(
            hyper::StatusCode::NOT_FOUND,
            &not_found_body(&path),
            keep_alive,
        ),
        version,
    )))
}

/// The three file endpoints plus the branch-local 400s; `None` falls through
/// to the router's final 404 (path from the WHATWG pathname, like TS).
/// #1740: every file-service call runs on the blocking pool — the TS service
/// is synchronous `fs` at its core, and the replica's descriptor walk is the
/// same std::fs work, so no reactor thread stalls on disk. The service is
/// built from the same project key and answers the same values, byte for
/// byte; only the executing thread changes.
async fn files_branch(
    request: &mut hyper::Request<hyper::body::Incoming>,
    sub: &str,
    key: &str,
    query: &str,
    path: &str,
    keep_alive: bool,
) -> Result<hyper::Response<Body>, io::Error> {
    let outcome = if sub == "/files/tree" && request.method() == hyper::Method::GET {
        // searchParams.get("path") ?? "."
        let path = query_param(query, "path").unwrap_or_else(|| ".".to_owned());
        Some(
            run_files_job(key, move |service| service.read_tree(&path))
                .await?
                .map(|value| send_json_value(hyper::StatusCode::OK, &value, keep_alive)),
        )
    } else if sub == "/files/read" && request.method() == hyper::Method::GET {
        // The falsy check makes a missing AND an empty path the 400.
        match query_param(query, "path").filter(|value| !value.is_empty()) {
            None => {
                return Ok(send_json(
                    hyper::StatusCode::BAD_REQUEST,
                    MISSING_PATH_BODY,
                    keep_alive,
                ));
            }
            Some(path) => Some(
                run_files_job(key, move |service| service.read_file(&path))
                    .await?
                    .map(|value| send_json_value(hyper::StatusCode::OK, &value, keep_alive)),
            ),
        }
    } else if sub == "/files/write" && request.method() == hyper::Method::POST {
        let body = read_json_body(request.body_mut()).await?;
        match write_shape(&body) {
            None => {
                return Ok(send_json(
                    hyper::StatusCode::BAD_REQUEST,
                    INVALID_BODY,
                    keep_alive,
                ));
            }
            Some((path, content, base64)) => {
                // Node hands the JS string straight to fs, which encodes lone
                // surrogates as the replacement at the boundary.
                let path = String::from_utf16_lossy(path.units());
                Some(
                    run_files_job(key, move |service| {
                        service.write_file(&path, &content, base64)
                    })
                    .await?
                    .map(|()| send_json(hyper::StatusCode::OK, WRITE_OK_BODY, keep_alive)),
                )
            }
        }
    } else {
        None
    };
    Ok(match outcome {
        Some(Ok(response)) => response,
        Some(Err(files::FilesError::Workspace { raw })) => send_json(
            hyper::StatusCode::FORBIDDEN,
            &error_body(
                "workspace_boundary",
                &format!("Path escapes workspace root: {raw}"),
            ),
            keep_alive,
        ),
        Some(Err(files::FilesError::Message(message))) => send_json(
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            &error_body("internal_error", &message),
            keep_alive,
        ),
        // #2455: the message keeps its UTF-16 units (the 128-unit cut of the
        // NUL refusal can split a surrogate pair), so the body goes through
        // the well-formed JSON encoder and the wire keeps Node's `\ud83d`
        // escape for the lone surrogate.
        Some(Err(files::FilesError::MessageUnits(message))) => send_json(
            hyper::StatusCode::INTERNAL_SERVER_ERROR,
            &error_body_units("internal_error", &message),
            keep_alive,
        ),
        None => send_json(
            hyper::StatusCode::NOT_FOUND,
            &not_found_body(path),
            keep_alive,
        ),
    })
}

/// Build the file service from the project key and run one synchronous call
/// on the blocking pool (#1740). A join failure (the service panicked or the
/// runtime shut down) is the handler `Err` that closes the connection with no
/// bytes, like any other zero-response failure.
async fn run_files_job<T>(
    key: &str,
    job: impl FnOnce(files::ProjectFileService) -> Result<T, files::FilesError> + Send + 'static,
) -> Result<Result<T, files::FilesError>, io::Error>
where
    T: Send + 'static,
{
    let key = key.to_owned();
    tokio::task::spawn_blocking(move || job(files::ProjectFileService::new(&key)))
        .await
        .map_err(|error| io::Error::other(format!("api-web files task: {error}")))
}

/// The write branch's 400 gate: `path` a non-empty string, `content` a
/// string, and `encoding` — when present — one of the two literals.
fn write_shape(body: &JsValue) -> Option<(JsString, JsString, bool)> {
    let fields = body.as_object()?;
    let path = fields.get("path")?.as_string()?;
    if path.is_empty() {
        return None;
    }
    let content = fields.get("content")?.as_string()?;
    let base64 = match fields.get("encoding") {
        None | Some(JsValue::Undefined) => false,
        Some(JsValue::String(encoding)) => {
            if encoding.equals_str("base64") {
                true
            } else if encoding.equals_str("utf8") {
                false
            } else {
                return None;
            }
        }
        Some(_) => return None,
    };
    Some((path.clone(), content.clone(), base64))
}

/// `readJsonBody`: the concatenated body through `JSON.parse`; an empty body
/// and a parse error are both `undefined` (the write branch's 400).
async fn read_json_body(body: &mut hyper::body::Incoming) -> Result<JsValue, io::Error> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| io::Error::other("api-web request body read failed"))?;
        if let Ok(data) = frame.into_data() {
            bytes.extend_from_slice(&data);
        }
    }
    if bytes.is_empty() {
        return Ok(JsValue::Undefined);
    }
    Ok(
        pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&bytes))
            .unwrap_or(JsValue::Undefined),
    )
}

/// The `Authorization` header through `/^Bearer\s+(.+)$/i`, in Node's
/// latin1 header domain: the scheme is case-insensitive, the separator is
/// one or more JS whitespace bytes (`\t\n\v\f\r `, 0xA0), and the capture is
/// one or more non-line-terminator bytes to the end. Greedy `\s+` backtracks
/// when only whitespace follows, so the latest viable capture start wins.
fn bearer_capture(header: &[u8]) -> Option<&[u8]> {
    fn whitespace(byte: u8) -> bool {
        matches!(byte, 0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0)
    }
    if header.len() < 7 || !header[..6].eq_ignore_ascii_case(b"Bearer") {
        return None;
    }
    if !whitespace(header[6]) {
        return None;
    }
    let run = 6 + header[6..]
        .iter()
        .take_while(|&&byte| whitespace(byte))
        .count();
    (7..=run)
        .rev()
        .map(|start| &header[start..])
        .find(|tail| !tail.is_empty() && !tail.contains(&0x0A) && !tail.contains(&0x0D))
}

/// `PROJECT_RE` over the WHATWG pathname, `^/api/web/projects(?:/([^/]+)(\/.*?)?)?$`:
/// `("", None)` is the bare collection match, `None` is no match, and the
/// sub-path group keeps its leading slash ("/files/tree").
fn project_match(path: &str) -> Option<(&str, Option<&str>)> {
    if path == "/api/web/projects" {
        return Some(("", None));
    }
    let rest = path.strip_prefix("/api/web/projects/")?;
    let (key, sub) = match rest.split_once('/') {
        Some((key, _)) => (key, Some(&rest[key.len()..])),
        None => (rest, None),
    };
    if key.is_empty() {
        return None;
    }
    Some((key, sub))
}

/// `decodeURIComponent`: strict percent-decoding — an invalid escape or a
/// decoded byte sequence that is not UTF-8 is the `URI malformed` throw.
fn decode_uri_component(raw: &str) -> Result<String, ()> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some((high, low)) = bytes
                .get(index + 1)
                .and_then(|high| hex(*high))
                .zip(bytes.get(index + 2).and_then(|low| hex(*low)))
            else {
                return Err(());
            };
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

/// `url.searchParams.get(name)`: the first value of the first matching pair,
/// WHATWG urlencoded decoding (plus-to-space, lenient percent-decoding).
fn query_param(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// `new URL(request.url, "http://127.0.0.1")`: dot segments collapse,
/// `%2e` is a dot, fragments and queries split off.
/// Sibling: keep in sync with `whatwg_path` in `module_host.rs` and
/// `src/native_http.rs`.
fn whatwg_url(target: &str) -> Option<url::Url> {
    static BASE: std::sync::OnceLock<url::Url> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| url::Url::parse("http://127.0.0.1").expect("valid base URL"));
    url::Url::options().base_url(Some(base)).parse(target).ok()
}

/// Node answers an HTTP/1.0 request with a `Connection: close` response on
/// a closed socket; an explicit 1.0 response version keeps hyper in its
/// HTTP/1.0 branch — the #1209 trick whose `HTTP/1.0` status line against
/// Node's `HTTP/1.1` is the registered A8.4-NATIVE-HTTP-001 diff.
fn finalize(mut response: hyper::Response<Body>, version: hyper::Version) -> hyper::Response<Body> {
    if version == hyper::Version::HTTP_10 {
        *response.version_mut() = hyper::Version::HTTP_10;
    }
    response
}

/// A TS `writeHead` + `end` JSON response: unknown-length body (chunked,
/// because `writeHead` commits the headers before `end` knows the size),
/// the literal content-type, Node's Date, and the keep-alive pair, in
/// Node's header order.
fn send_json(status: hyper::StatusCode, body: &str, keep_alive: bool) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder().status(status);
    builder = builder
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    builder = connection_headers(builder, keep_alive);
    builder
        .body(streamed(Some(Bytes::from(body.to_owned()))))
        .expect("#1348 valid transport response")
}

/// `sendJson` of a catalog or file value: `JSON.stringify` once, so the key
/// order and number forms are the TS ones; top-level `undefined` ends with
/// no bytes.
fn send_json_value(
    status: hyper::StatusCode,
    value: &JsValue,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let body = stringify_json(value)
        .expect("#1348 a JSON value is serializable")
        .unwrap_or_default();
    send_json(status, &body, keep_alive)
}

fn connection_headers(
    builder: hyper::http::response::Builder,
    keep_alive: bool,
) -> hyper::http::response::Builder {
    if keep_alive {
        builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5")
    } else {
        builder.header(hyper::header::CONNECTION, "close")
    }
}

/// Node's `Connection: close` request detection, copied from the #1209
/// router: every `Connection` header value is split on commas and a `close`
/// token (case-insensitive, trimmed) asks for a close.
/// Sibling: keep in sync with `request_wants_close` in `module_host.rs` and
/// `src/native_http.rs`.
fn request_wants_close(request: &hyper::Request<hyper::body::Incoming>) -> bool {
    request
        .headers()
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// The `errorBody` helper: `error instanceof Error ? error.message :
/// String(error)` is a plain string here, so only the quoting is shared.
fn error_body(code: &str, message: &str) -> String {
    format!(
        "{{\"error\":{{\"code\":{},\"message\":{}}}}}",
        json_text(code),
        json_text(message)
    )
}

/// `error_body` for a message that keeps its UTF-16 units (#2455): Node's
/// error message is a JS string, and a lone surrogate — the NUL refusal's
/// 128-unit cut splitting a surrogate pair — serializes as the well-formed
/// `\ud83d` escape, not the U+FFFD a `from_utf16_lossy` fold would emit.
/// For a surrogate-free message the bytes equal `error_body`'s.
fn error_body_units(code: &str, message: &JsString) -> String {
    format!(
        "{{\"error\":{{\"code\":{},\"message\":{}}}}}",
        json_text(code),
        pilotdeck_protocol::quote_json_string(message)
    )
}

fn not_found_body(path: &str) -> String {
    error_body("not_found", &format!("No /api/web route for {path}"))
}

/// `JSON.stringify` of a bare string: quoting and escapes for one value.
fn json_text(value: &str) -> String {
    pilotdeck_protocol::quote_json_string(&JsString::from(value))
}

/// Unknown-length single-frame body: the chunked framing a `writeHead` +
/// `end` response gets, like the #1209 `StreamedBody`.
struct StreamedBody(Option<Bytes>);
impl hyper::body::Body for StreamedBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        std::task::Poll::Ready(
            self.0
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

fn streamed(body: Option<Bytes>) -> Body {
    StreamedBody(body).boxed()
}
