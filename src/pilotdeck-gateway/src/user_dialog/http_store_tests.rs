//! Unit-level goldens for the parts of `http_store` that are not reachable
//! through the public wire API: constructor validation and handler creation.
//!
//! TS source: `resources/PilotDeck/src/gateway/user-dialog/HttpGatewayUserDialogStore.ts`
//! at `cc7c4e6`. The oracle is produced by the real TS class (see
//! `tests/fixtures/user-dialog-http-store/generate.mts`); nothing here is
//! hand-written.

use super::*;
use crate::user_dialog::StoreOptions;
use serde_json::Value;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const ORACLE: &str = include_str!("../../tests/fixtures/user-dialog-http-store/oracle.json");

/// Node v22.22.0 `_http_server.js` constants, captured from a real `node:http`
/// server over a raw socket (`_tmp/20261025-issue-699/node/probe-header-lifecycle.mjs`).
/// 51 bytes: status line + `Connection: close`, no `content-length` / `date`.
const NODE_408: &[u8] =
    include_bytes!("../../tests/fixtures/user-dialog-http-store/header-lifecycle/node-408.bin");
/// 67 bytes, same shape as [`NODE_408`].
const NODE_431: &[u8] =
    include_bytes!("../../tests/fixtures/user-dialog-http-store/header-lifecycle/node-431.bin");
/// 47 bytes: Node's default `clientError` answer for an incomplete request
/// (`HPE_INVALID_EOF_STATE`), captured from a peer that sent a partial head or
/// body and then `shutdown(SHUT_WR)` (fix-r2 probe `probe-half-close.mjs`).
const NODE_400: &[u8] =
    include_bytes!("../../tests/fixtures/user-dialog-http-store/header-lifecycle/node-400.bin");

fn oracle() -> Value {
    serde_json::from_str(ORACLE).expect("fixture oracle.json is valid JSON")
}

fn options(url: &str, token: &str, timeout_ms: Option<f64>) -> HttpGatewayUserDialogStoreOptions {
    HttpGatewayUserDialogStoreOptions {
        url: url.into(),
        authorization_token: token.into(),
        timeout_ms,
    }
}

fn number(text: &str) -> f64 {
    match text {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => text.parse().expect("fixture number"),
    }
}

/// `store.endpoint.href` / `new URL('clear', store.endpoint).href` for every
/// constructor URL, plus the exact TS error text for rejected URLs.
#[test]
fn constructors_match_typescript_endpoints_and_errors() {
    let oracle = oracle();
    let mut checked = 0;
    for case in oracle["constructors"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let input = &case["input"];
        if name.starts_with("url ") {
            let url = input.as_str().unwrap();
            let store =
                HttpGatewayUserDialogStore::new(options(url, "test-dialog-store-token", None));
            if kind == "endpoint" {
                let store = store.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(
                    store.endpoint.as_str(),
                    case["href"].as_str().unwrap(),
                    "{name}: href"
                );
                assert_eq!(
                    store.endpoint.join("clear").unwrap().as_str(),
                    case["resolved"].as_str().unwrap(),
                    "{name}: resolved URL"
                );
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
            continue;
        }
        if name.starts_with("timeoutMs ") {
            let timeout = match input["kind"].as_str().unwrap() {
                // Rust's `Option<f64>` cannot express a string `timeoutMs`;
                // the TS error is recorded but this state is type-level.
                "string" => continue,
                "undefined" | "null" => None,
                "number" => Some(number(input["text"].as_str().unwrap())),
                other => panic!("{name}: unknown timeout input {other}"),
            };
            let store = HttpGatewayUserDialogStore::new(options(
                "http://h/",
                "test-dialog-store-token",
                timeout,
            ));
            if kind == "timeout" {
                let store = store.unwrap_or_else(|error| panic!("{name}: {error}"));
                let expected = case["ms"].as_f64().unwrap();
                assert_eq!(store.timeout_ms, expected, "{name}: timeoutMs property");
                // Node clamps a setTimeout delay above 2^31-1 to 1 ms; TS still
                // exposes the unclamped property, which is what we compare above.
                let effective = if expected > 2_147_483_647.0 {
                    1
                } else {
                    expected as u64
                };
                assert_eq!(
                    store.timeout,
                    Duration::from_millis(effective),
                    "{name}: effective timer"
                );
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
            continue;
        }
        if name.starts_with("token ") {
            let input = &case["input"];
            // `undefined` is not expressible with `String`.
            if input["kind"].as_str().unwrap() == "undefined" {
                continue;
            }
            let token = input["value"].as_str().unwrap();
            let store = HttpGatewayUserDialogStore::new(options("http://h/", token, None));
            if kind == "token" {
                assert!(store.is_ok(), "{name}: accepted");
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
        }
    }
    // 29 recorded constructor cases; the string timeout and undefined token are
    // type-level skips, the remaining 27 are asserted here.
    assert_eq!(checked, 27, "all expressible constructor cases are covered");
}

/// TS `requireLiveStore` checks all twelve methods at construction. Rust's
/// `LiveGatewayUserDialogStore` trait makes a partial implementation
/// unrepresentable, so only the token guard is exercised at runtime; the
/// `missing-*` recordings document the TS-only runtime check that the type
/// system replaces.
#[test]
fn handler_construction_matches_typescript() {
    let oracle = oracle();
    let store: Arc<dyn LiveGatewayUserDialogStore> = Arc::new(
        FileGatewayUserDialogStore::new(StoreOptions::new("/pdrs-http-store-test"))
            .expect("absolute test directory"),
    );
    let mut checked = 0;
    let mut missing_checked = 0;
    for case in oracle["handlerConstruction"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if let Some(method) = case["missing"].as_str() {
            assert_eq!(
                case["ok"], false,
                "{name}: TS rejects a store without {method}()"
            );
            assert_eq!(
                case["message"].as_str().unwrap(),
                format!("Gateway user-dialog store HTTP service requires {method}().")
            );
            // A complete Rust store always constructs.
            assert!(
                create_gateway_user_dialog_store_http_handler(
                    store.clone(),
                    "test-dialog-store-token"
                )
                .is_ok()
            );
            missing_checked += 1;
            continue;
        }
        let input = &case["input"];
        if input["kind"].as_str().unwrap() == "undefined" {
            continue;
        }
        let token = input["value"].as_str().unwrap();
        let result = create_gateway_user_dialog_store_http_handler(store.clone(), token);
        match case["ok"].as_bool().unwrap() {
            true => assert!(result.is_ok(), "{name}: accepted"),
            false => assert_eq!(
                result.unwrap_err().to_string(),
                case["message"].as_str().unwrap(),
                "{name}: error message"
            ),
        }
        checked += 1;
    }
    assert_eq!(checked, 3, "expressible handler token cases");
    assert_eq!(missing_checked, 3, "missing-method recordings");
}

// ---------------------------------------------------------------------------
// Resource bounds, timeouts and the shared channel/store trait
// ---------------------------------------------------------------------------

const TOKEN: &str = "test-dialog-store-token";

fn test_key() -> StoreKey {
    StoreKey {
        project_root: "/workspace/project".into(),
        pilot_home: "/workspace/home".into(),
        session_id: "sdk:http-dialog-store".into(),
    }
}

fn test_dialog() -> StoredDialog {
    StoredDialog {
        request: parse_json(
            r#"{"type":"user_dialog_request","requestId":"r1","dialogKind":"form","toolCallId":"t1","toolName":"request_user_form","prompt":"p"}"#,
        )
        .unwrap(),
        created_at: "2026-01-01T00:00:00.000Z".into(),
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "pdrs-http-store-{tag}-{}-{unique}",
        std::process::id()
    ))
}

fn file_store(tag: &str) -> (Arc<dyn LiveGatewayUserDialogStore>, std::path::PathBuf) {
    let directory = temp_dir(tag);
    let store = FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap();
    (Arc::new(store), directory)
}

async fn start_limited(
    store: Arc<dyn LiveGatewayUserDialogStore>,
    limits: ServeLimits,
) -> (GatewayUserDialogStoreHttpServer, u16) {
    let server = start_gateway_user_dialog_store_http_server_with_limits(
        store,
        TOKEN,
        Some("127.0.0.1"),
        Some(0),
        limits,
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "loopback listen failed ({error}); this sandbox blocks TcpListener::bind. \
             Outside the sandbox rerun: cargo test -p pilotdeck-gateway --lib \
             user_dialog::http_store -- --nocapture"
        )
    });
    let port = url::Url::parse(server.url())
        .unwrap()
        .port()
        .expect("ephemeral port");
    (server, port)
}

fn http_request(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn http_post(body: &[u8]) -> Vec<u8> {
    http_request(
        "POST",
        &format!("{STORE_PATH_PREFIX}list"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
            ("Connection", "close"),
        ],
        body,
    )
}

fn http_put(body: &[u8]) -> Vec<u8> {
    http_request(
        "POST",
        &format!("{STORE_PATH_PREFIX}put"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
            ("Connection", "close"),
        ],
        body,
    )
}

fn http_get(close: bool) -> Vec<u8> {
    let mut request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n{}",
        if close { "Connection: close\r\n" } else { "" }
    );
    request.push_str("\r\n");
    request.into_bytes()
}

fn put_body() -> Vec<u8> {
    concat!(
        r#"{"version":1,"key":{"projectRoot":"/p","pilotHome":"/h","sessionId":"s"},"#,
        r#""dialog":{"request":{"type":"user_dialog_request","requestId":"r1","dialogKind":"form","#,
        r#""toolCallId":"t1","toolName":"request_user_form","prompt":"p"},"#,
        r#""createdAt":"2026-01-01T00:00:00.000Z"}}"#
    )
    .as_bytes()
    .to_vec()
}

fn status(response: &[u8]) -> u16 {
    let head = String::from_utf8_lossy(response);
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP response: {head:?}"))
}

/// One response phase may legitimately take seconds on a loaded host: the list
/// handler serializes ~18 MiB before hyper can write the response head. Four
/// concurrent runs measured 3.5 s head waits, and a saturated host exceeded the
/// old 5 s per-read bound (4/4 negative-control runs failed at iteration 11).
/// This is a bounded **failure** budget, not a protocol semantic: Node has no
/// response-generation deadline (`headersTimeout` only covers the request
/// head), so a slow handler must not be mistaken for a product hang. A genuine
/// stall still terminates the test with the stage and bytes read.
const RESPONSE_PHASE_BUDGET: Duration = Duration::from_secs(60);

/// Start the failure budget for one response phase (a head or a body). The
/// deadline is absolute so repeated reads cannot reset it; callers that own a
/// tighter assertion (an expected close, a product timeout) keep their own
/// shorter bound on the outside.
fn response_phase_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + RESPONSE_PHASE_BUDGET
}

async fn exchange(port: u16, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(request).await?;
    let mut response = Vec::new();
    tokio::time::timeout_at(response_phase_deadline(), stream.read_to_end(&mut response))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no complete response within {RESPONSE_PHASE_BUDGET:?}"),
            )
        })??;
    Ok(response)
}

/// The server may legitimately close while a client is still writing an
/// oversized body; only the response matters, so a write error is ignored. The
/// response read is still bounded by the same phase budget as [`exchange`];
/// tolerating a failed write is orthogonal to bounding the read.
async fn exchange_lenient(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let _ = stream.write_all(request).await;
    let mut response = Vec::new();
    let _ =
        tokio::time::timeout_at(response_phase_deadline(), stream.read_to_end(&mut response)).await;
    response
}

async fn exchange_within(port: u16, request: &[u8], limit: Duration) -> std::io::Result<Vec<u8>> {
    tokio::time::timeout(limit, exchange(port, request))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no response"))?
}

/// Like [`exchange_lenient`], but reports whether the server actually ended the
/// connection (EOF or reset) instead of leaving it idle until `limit`.
async fn exchange_until_eof(port: u16, request: &[u8], limit: Duration) -> (Vec<u8>, bool) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let _ = tokio::time::timeout(limit, stream.write_all(request)).await;
    let mut response = Vec::new();
    let closed = tokio::time::timeout(limit, stream.read_to_end(&mut response))
        .await
        .is_ok();
    (response, closed)
}

/// A chunked request; the body is sent as `chunks` so `Limited` can reject it
/// mid-stream (there is no Content-Length for the precheck to catch).
fn chunked_request(
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    chunks: &[&[u8]],
) -> Vec<u8> {
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Transfer-Encoding: chunked\r\n\r\n");
    let mut bytes = request.into_bytes();
    for chunk in chunks {
        bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        bytes.extend_from_slice(chunk);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"0\r\n\r\n");
    bytes
}

fn assert_close_without_keep_alive(response: &[u8], expected: u16, note: &str) {
    assert_eq!(status(response), expected, "{note}: {response:?}");
    let head = String::from_utf8_lossy(response).to_ascii_lowercase();
    assert!(
        head.contains("connection: close"),
        "{note}: expected an explicit close, got {head}"
    );
    assert!(
        !head.contains("keep-alive:"),
        "{note}: a close response must not advertise Keep-Alive: {head}"
    );
}

/// Reads exactly one HTTP message (head plus Content-Length body) so a
/// keep-alive stream is left idle, not with unread response bytes. The whole
/// message shares one phase deadline; an outer, tighter assertion bound still
/// wins because it is applied by the caller.
async fn read_response(stream: &mut TcpStream) -> Vec<u8> {
    let deadline = response_phase_deadline();
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => panic!("response head read failed: {error}"),
            Err(_) => panic!(
                "response head never arrived within {RESPONSE_PHASE_BUDGET:?} \
                 (read {} bytes so far)",
                response.len()
            ),
        };
        if read == 0 {
            panic!(
                "EOF before the response head completed: {:?}",
                String::from_utf8_lossy(&response)
            );
        }
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&response).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    match tokio::time::timeout_at(deadline, stream.read_exact(&mut body)).await {
        Ok(Ok(read)) => assert_eq!(read, length, "response body short read"),
        Ok(Err(error)) => panic!("response body read failed: {error}"),
        Err(_) => panic!(
            "response body stalled within {RESPONSE_PHASE_BUDGET:?} \
             (expected {length} bytes)"
        ),
    }
    response.extend_from_slice(&body);
    response
}

/// Reads only the response head, leaving any body bytes unread. HEAD
/// responses declare `Content-Length` but never send the body. The read is
/// bounded by the same phase budget as every other response reader, so a
/// stalled head fails the test instead of hanging the test binary.
async fn read_head<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> String {
    let deadline = response_phase_deadline();
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => panic!("response head read failed: {error}"),
            Err(_) => panic!(
                "response head never arrived within {RESPONSE_PHASE_BUDGET:?} \
                 (read {} bytes so far: {:?})",
                response.len(),
                String::from_utf8_lossy(&response)
            ),
        };
        if read == 0 {
            panic!(
                "EOF before the response head completed: {:?}",
                String::from_utf8_lossy(&response)
            );
        }
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            return String::from_utf8_lossy(&response).into_owned();
        }
    }
}

fn http_head() -> Vec<u8> {
    format!("HEAD {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").into_bytes()
}

/// Asserts the exact terminal bytes Node writes for a connection-level error,
/// then that the server closes without appending anything. A silent EOF (the
/// pre-#699 Rust behaviour) fails on the `read_exact`, and hyper's own
/// 431/408 shape (extra `content-length` / `date`, lower-case names) fails on
/// the byte comparison.
async fn expect_terminal_bytes(stream: &mut TcpStream, expected: &[u8], note: &str) {
    let mut actual = vec![0u8; expected.len()];
    tokio::time::timeout(Duration::from_secs(3), stream.read_exact(&mut actual))
        .await
        .unwrap_or_else(|_| panic!("{note}: no {}-byte response within 3 s", expected.len()))
        .unwrap_or_else(|error| panic!("{note}: short response: {error}"));
    assert_eq!(
        actual,
        expected,
        "{note}: Rust bytes differ from Node\n rust={:?}\n node={:?}",
        String::from_utf8_lossy(&actual),
        String::from_utf8_lossy(expected)
    );
    let mut extra = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut extra))
        .await
        .unwrap_or_else(|_| panic!("{note}: connection stayed open after the terminal response"));
    assert_eq!(
        read.expect("extra read"),
        0,
        "{note}: unexpected byte after the terminal response"
    );
}

/// Equal-length tokens must still be compared byte by byte. Last-byte and
/// middle-byte substitutions both have the same length as the real token.
#[test]
fn auth_rejects_equal_length_wrong_tokens() {
    let (store, directory) = file_store("auth");
    let handler = create_gateway_user_dialog_store_http_handler(store, TOKEN).unwrap();
    let url = format!("http://localhost{STORE_PATH_PREFIX}list");
    let request = |authorization: Option<&str>| HttpStoreHttpRequest {
        method: "POST".into(),
        url: url.clone(),
        headers: authorization
            .map(|value| vec![("authorization".to_string(), value.to_string())])
            .unwrap_or_default(),
        body: b"{}".to_vec(),
    };
    let last_byte = format!("Bearer {}m", &TOKEN[..TOKEN.len() - 1]);
    let middle_byte = format!("Bearer {}X{}", &TOKEN[..5], &TOKEN[6..]);
    for wrong in [
        last_byte.as_str(),
        middle_byte.as_str(),
        "Bearer test-dialog-store-toke",
        "Bearer test-dialog-store-token-extra",
        "test-dialog-store-token",
    ] {
        assert_ne!(
            wrong,
            format!("Bearer {TOKEN}"),
            "{wrong}: not a wrong token"
        );
        assert_eq!(
            handler.handle(request(Some(wrong))).status,
            401,
            "{wrong}: must be rejected"
        );
    }
    // The same length check must not reject the correct token: `{}` reaches the
    // JSON parser and returns its 400 instead of 401.
    assert_eq!(
        handler
            .handle(request(Some(&format!("Bearer {TOKEN}"))))
            .status,
        400,
        "correct token passed authentication"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// Node's `maxHeaderSize` default is 16 KiB and its parser writes exactly the
/// 67-byte `_http_server.js` constant. hyper's canned parser 431 (which adds
/// `content-length: 0` / `date` and lower-cases the header name) is not this.
#[tokio::test]
async fn oversized_headers_receive_node_431_bytes() {
    let (store, directory) = file_store("headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let padding = "a".repeat(DEFAULT_MAX_HEADER_BYTES);
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {padding}\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    expect_terminal_bytes(&mut stream, NODE_431, "431 oversized header").await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Frozen-ledger anchor (`specs/gateway-parity/accepted-diffs.md:575` and
/// `:603`). fix-r1 renamed the old `oversized_headers_are_rejected_like_node`
/// to `oversized_headers_receive_node_431_bytes`; the ledger is byte-frozen, so
/// this name must keep existing. It keeps the original assertions (431, empty
/// body, `connection: close`) and additionally pins Node's exact 67 bytes, so
/// the name is no longer a "like Node" over-claim.
#[tokio::test]
async fn oversized_headers_are_rejected_like_node() {
    let (store, directory) = file_store("headers-ledger-anchor");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let padding = "a".repeat(DEFAULT_MAX_HEADER_BYTES);
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {padding}\r\nConnection: close\r\n\r\n"
    );
    let response = exchange(port, request.as_bytes()).await.unwrap();
    assert_eq!(
        status(&response),
        431,
        "oversized header must be 431: {:?}",
        String::from_utf8_lossy(&response)
    );
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    assert!(body.is_empty(), "Node sends no body with 431");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "Node closes the connection after 431"
    );
    assert_eq!(
        response, NODE_431,
        "Node's 431 is the 67-byte constant, not hyper's content-length/date shape"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Negative control for the header guard: 15,000 bytes stays under Node's
/// 16 KiB field budget, so the request must reach the handler (405 for GET)
/// instead of being rejected by an over-eager pre-reader. This is the same
/// control the Node probe records as `initial-under-limit-control`.
#[tokio::test]
async fn under_limit_headers_are_not_431() {
    let (store, directory) = file_store("headers-under-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let padding = "a".repeat(15_000);
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {padding}\r\nConnection: close\r\n\r\n"
    );
    let response = exchange(port, request.as_bytes()).await.unwrap();
    assert_eq!(
        status(&response),
        405,
        "an under-limit header must reach the handler: {}",
        String::from_utf8_lossy(&response)
    );
    assert!(
        !response.starts_with(NODE_431),
        "under-limit header was answered with Node's 431 bytes"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node v22.22.0's runtime `maxHeadersCount` default is `null` (unlimited); the
/// documented 2000 is not applied. 2,400 one-byte fields fit the 16 KiB header
/// budget and must not be rejected by a count cap. This pins that behaviour so
/// a future "align to docs 2000" change goes red.
#[tokio::test]
async fn many_small_headers_are_not_capped_at_the_documented_two_thousand() {
    let (store, directory) = file_store("many-headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let headers = "a:\r\n".repeat(2400);
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Connection: close\r\n\r\n"
    );
    // A count-cap regression makes hyper abort the parse before a response can
    // be read, so tolerate a reset and assert on whatever status arrived.
    let response = exchange_lenient(port, request.as_bytes()).await;
    assert!(
        !response.is_empty(),
        "server closed without a response (a 2,000-field cap rejects the request before the handler)"
    );
    assert_eq!(
        status(&response),
        405,
        "2,400 sub-16KiB fields must reach the handler, not 431"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node's `maxHeaderSize` counts the request target plus field name and value
/// bytes, not the raw `name: value\r\n` line. 2,000 fields of `aaaaa: x`
/// (20,000 raw bytes but only 12,000 counted bytes) are inside the 16 KiB
/// budget, so the guard must not pre-reject them; 2,730 fields are over the
/// budget and must get Node's exact 67-byte 431. A guard that counts raw line
/// bytes fails the first half (it answers 431), which is the #699 review
/// counter-example.
#[tokio::test]
async fn header_guard_counts_name_and_value_like_node() {
    let (store, directory) = file_store("header-name-value-count");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let pass = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n{}Connection: close\r\n\r\n",
        "aaaaa: x\r\n".repeat(2_000)
    );
    let response = exchange(port, pass.as_bytes()).await.unwrap();
    assert_eq!(
        status(&response),
        405,
        "2,000x `aaaaa: x` count 12,000 bytes and must reach the handler: {}",
        String::from_utf8_lossy(&response)
    );

    let fail = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n{}Connection: close\r\n\r\n",
        "aaaaa: x\r\n".repeat(2_730)
    );
    let (response, closed) =
        exchange_until_eof(port, fail.as_bytes(), Duration::from_secs(5)).await;
    assert_eq!(
        response, NODE_431,
        "2,730x `aaaaa: x` count 16,380 bytes plus the target/Host/Connection"
    );
    assert!(closed, "the 431 must end the connection");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node rejects once the counted total *reaches* `maxHeaderSize` (16,384).
/// The fixed target/Host/Connection bytes are 34 + 13 + 15 = 62; 2,719
/// `aaaaa: x` fields count 16,314 and one `X-Pad: aa` counts 7, so the pass
/// case is exactly 16,383. One more value byte makes 16,384 and must get the
/// 67-byte 431. `aaaaa: x` is used instead of `a:` so the field count stays
/// under hyper's 4,096-slot `max_headers` backstop (a separate, registered
/// difference).
#[tokio::test]
async fn header_guard_boundary_matches_node_16384() {
    let (store, directory) = file_store("header-boundary");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let request = |extra: &str| {
        format!(
            "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n{}X-Pad: {extra}\r\nConnection: close\r\n\r\n",
            "aaaaa: x\r\n".repeat(2_719)
        )
    };
    let pass = request("aa");
    let (response, closed) =
        exchange_until_eof(port, pass.as_bytes(), Duration::from_secs(5)).await;
    assert!(closed, "Connection: close must end the pass case");
    assert_eq!(
        status(&response),
        405,
        "16,383 counted bytes are inside Node's budget: {}",
        String::from_utf8_lossy(&response)
    );

    let fail = request("aaa");
    let (response, closed) =
        exchange_until_eof(port, fail.as_bytes(), Duration::from_secs(5)).await;
    assert_eq!(response, NODE_431, "16,384 counted bytes must be 431");
    assert!(closed, "the 431 must end the connection");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node counts the request target (`on_url`) as part of `maxHeaderSize`.
/// `GET /<p*16377> HTTP/1.1` has a 16,378-byte target; with `Host: h` (5) the
/// total is 16,383 and the request reaches the handler (404 for the unknown
/// path). One more `p` makes 16,384 and must get the 67-byte 431. The Node
/// goldens for the identical bytes are `target-16383` / `target-16384` in
/// `node-header-budget.json`.
#[tokio::test]
async fn request_target_counts_toward_node_header_budget() {
    let (store, directory) = file_store("header-target");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let request =
        |pad: usize| format!("GET /{} HTTP/1.1\r\nHost: h\r\n\r\n", "p".repeat(pad)).into_bytes();

    let mut pass = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    pass.write_all(&request(16_377)).await.unwrap();
    let head = read_head(&mut pass).await;
    assert_eq!(
        status(head.as_bytes()),
        405,
        "16,383 counted bytes must reach the handler: {head}"
    );
    drop(pass);

    let mut fail = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    fail.write_all(&request(16_378)).await.unwrap();
    expect_terminal_bytes(&mut fail, NODE_431, "target pushes the head to 16,384").await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// The pre-fix guard only charged *completed* lines, so a head with exactly
/// 16,384 raw header bytes followed by a long unterminated line let hyper's
/// 32 KiB `max_buf_size` fire first: the client got hyper's 123-byte 431
/// (`content-length: 0`, `date`, lower-case name) instead of Node's 67-byte
/// constant. Node's counted total crosses 16,384 while the line is still
/// incomplete, so the guard must answer with [`NODE_431`].
#[tokio::test]
async fn header_guard_beats_hyper_buffer_cap_at_the_boundary() {
    let (store, directory) = file_store("header-buffer-boundary");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    // 1,637 * 10 + 5 = 16,375 raw field bytes plus `Host: h\r\n` (9) is
    // exactly the 16,384-byte completed-line total the old guard let through;
    // only 9,824 of them are counted bytes.
    let completed = format!("{}bb:\r\n", "aaaaa: x\r\n".repeat(1_637));
    assert_eq!(completed.len() + "Host: h\r\n".len(), 16_384);
    let mut request =
        format!("GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\n{completed}X-Pad: ")
            .into_bytes();
    // 16,368 value bytes, no CRLF: the head is still incomplete when hyper's
    // old 32 KiB buffer filled, but Node's count already exceeded 16,384.
    request.extend(std::iter::repeat_n(b'a', 16_368));
    let (response, closed) = exchange_until_eof(port, &request, Duration::from_secs(5)).await;
    assert_eq!(
        response,
        NODE_431,
        "guard bytes, not hyper's 123-byte 431: {:?}",
        String::from_utf8_lossy(&response)
    );
    assert!(closed, "the 431 must end the connection");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Registered resource bound (R3): Node does not count leading whitespace
/// after the colon and accepts a 90 KB `X: <spaces>a` header, but hyper has to
/// buffer every raw byte before parsing. The guard's raw cap rejects before
/// hyper's parser can write its own 431 shape; the early 431 is the registered
/// unapproved difference, the 67 bytes are Node's.
#[tokio::test]
async fn header_guard_raw_cap_uses_node_431_bytes() {
    let (store, directory) = file_store("header-raw-cap");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\nX: {}a\r\nConnection: close\r\n\r\n",
        " ".repeat(90_000)
    );
    let (response, closed) =
        exchange_until_eof(port, request.as_bytes(), Duration::from_secs(5)).await;
    assert_eq!(response, NODE_431, "the guard must own the 431 bytes");
    assert!(closed, "the 431 must end the connection");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Before the fix, `close()` only stopped the accept loop and left idle
/// keep-alive connections (and their permits) alive for `headersTimeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_closes_idle_keep_alive_connections() {
    let (store, directory) = file_store("close-idle");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    idle.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut idle).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    // This task owns the original listener. Its identity survives port reuse by
    // another test, unlike a fresh TCP connect after close() releases the port.
    let accept_loop = server.task.as_ref().unwrap().abort_handle();
    let closing = tokio::spawn(async move { server.close().await });
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), idle.read(&mut byte))
        .await
        .expect("close() left an idle keep-alive connection open");
    assert_eq!(read.unwrap(), 0, "expected EOF on the idle connection");
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .expect("close() did not finish")
        .unwrap();
    assert!(
        accept_loop.is_finished(),
        "the original listener task is still running after close()"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// Node's observable idle keep-alive deadline is `keepAliveTimeout` (5 s) plus
/// `server.keepAliveTimeoutBuffer` (1 s), not the bare 5 s property and not the
/// 60 s `headersTimeout`. A generated Node v22.22.0 server closes at 6,003 ms
/// (probe record in `_tmp/20261025-issue-679/probe`). The timer is shortened
/// here; the default constants are pinned separately.
async fn assert_idle_keep_alive_connection_closes() {
    let (store, directory) = file_store("idle-timeout");
    let limits = ServeLimits {
        headers_timeout: Duration::from_secs(5),
        keep_alive_timeout: Duration::from_millis(200),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    idle.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut idle).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    let started = Instant::now();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), idle.read(&mut byte))
        .await
        .expect("an idle keep-alive connection outlived keepAliveTimeout");
    assert_eq!(read.unwrap(), 0, "expected EOF at keepAliveTimeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the keep-alive timer did not fire promptly: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn idle_keep_alive_connections_close_at_keep_alive_timeout() {
    assert_idle_keep_alive_connection_closes().await;
}

/// Frozen-ledger anchor (`accepted-diffs.md:576`, `:603`). Before #679 an idle
/// keep-alive connection was bounded only by `headersTimeout`, which is where
/// the name comes from; #679 gave it its own `keepAliveTimeout` and renamed
/// this test to [`idle_keep_alive_connections_close_at_keep_alive_timeout`].
/// The ledger is byte-frozen, so the old name stays as a wrapper for the
/// current (Node-aligned) idle-timer assertion.
#[tokio::test]
async fn idle_keep_alive_connections_close_at_headers_timeout() {
    assert_idle_keep_alive_connection_closes().await;
}

/// End-to-end default check: Node v22.22.0 (keepAliveTimeout 5000 ms plus the
/// default 1000 ms buffer) closed an idle keep-alive socket at 6,003 ms in the
/// recorded probe. The bare 5 s property or the 60 s header timeout would both
/// be wrong here.
#[tokio::test]
async fn default_idle_keep_alive_connection_closes_after_six_seconds() {
    let (store, directory) = file_store("idle-default-timeout");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut idle = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    idle.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut idle).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    let started = Instant::now();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(8), idle.read(&mut byte))
        .await
        .expect("the default idle keep-alive connection did not close");
    assert_eq!(read.unwrap(), 0, "expected EOF at the default deadline");
    assert!(
        started.elapsed() >= Duration::from_millis(5_500),
        "closed before Node's keepAliveTimeout + buffer: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node's keep-alive timer is an inactivity timer (`socket.setTimeout`): a
/// byte from the client resets it. Without the reset, the connection would
/// close 300 ms after the first response instead of 300 ms after the partial
/// follow-up head; the 400 ms probe is longer than the old deadline and shorter
/// than the fresh one.
#[tokio::test]
async fn partial_next_request_resets_keep_alive_inactivity_timer() {
    let (store, directory) = file_store("idle-reset");
    let limits = ServeLimits {
        // Original deadline t=2 s; the partial head arrives at t=1 s and moves
        // it to t=3 s. The 2.5 s no-EOF check is 0.5 s past the old deadline
        // and 0.5 s before the new one, so load cannot turn the assertion red.
        headers_timeout: Duration::from_secs(5),
        keep_alive_timeout: Duration::from_secs(2),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    tokio::time::sleep(Duration::from_secs(1)).await;
    stream
        .write_all(b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\nX-Partial: ")
        .await
        .unwrap();

    let mut byte = [0u8; 1];
    let early = tokio::time::timeout(Duration::from_millis(1_500), stream.read(&mut byte)).await;
    assert!(
        early.is_err(),
        "keep-alive inactivity was not reset by the partial head: {early:?}"
    );
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
        .await
        .expect("the reset keep-alive timer never fired");
    assert_eq!(read.unwrap(), 0, "expected EOF after the reset timer");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node starts a fresh `headersTimeout` at the first byte of a follow-up
/// request and answers with the 51-byte 408; the idle timer alone would close
/// silently at 2 s here. The exact bytes prove the header branch, not the
/// keep-alive `socket.destroy()` branch.
#[tokio::test]
async fn next_request_headers_timeout_starts_at_first_byte() {
    let (store, directory) = file_store("next-headers-timeout");
    let limits = ServeLimits {
        headers_timeout: Duration::from_millis(300),
        keep_alive_timeout: Duration::from_secs(2),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    stream
        .write_all(b"GET /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\nX-Partial: ")
        .await
        .unwrap();
    let started = Instant::now();
    expect_terminal_bytes(&mut stream, NODE_408, "408 partial follow-up head").await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the header timer did not fire promptly: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// The keep-alive timer must not run while a request is in flight: a body that
/// arrives after more than `keepAliveTimeout` must still be accepted, until the
/// separate `requestTimeout` bound.
#[tokio::test]
async fn keep_alive_timeout_does_not_close_active_request() {
    let (store, directory) = file_store("active-request");
    let limits = ServeLimits {
        headers_timeout: Duration::from_secs(2),
        request_timeout: Duration::from_secs(2),
        keep_alive_timeout: Duration::from_millis(150),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    let body = put_body();
    let head = format!(
        "POST {STORE_PATH_PREFIX}put HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(&body[..10]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream.write_all(&body[10..]).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), read_response(&mut stream))
        .await
        .expect("the keep-alive timer killed an active request body");
    assert_eq!(status(&response), 200, "{response:?}");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node v22.22.0 advertises `Connection: keep-alive` plus
/// `Keep-Alive: timeout=<floor(keepAliveTimeout / 1000)>` on persistent HTTP/1.1
/// responses, omits `Keep-Alive` when the property is 0, and sends neither for
/// `Connection: close` or HTTP/1.0 (probe:
/// `_tmp/20261025-issue-679/fix-r1/node-keepalive/probe.out.txt`).
#[test]
fn keep_alive_advertisement_matches_node_v22() {
    let plain = HeaderMap::new();
    let advertised =
        |version, headers: &HeaderMap, timeout| keep_alive_advertisement(version, headers, timeout);
    assert_eq!(
        advertised(Version::HTTP_11, &plain, Duration::from_secs(5)),
        Some(KeepAliveHeader {
            timeout_seconds: Some(5)
        })
    );
    assert_eq!(
        advertised(Version::HTTP_11, &plain, Duration::from_millis(1_500)),
        Some(KeepAliveHeader {
            timeout_seconds: Some(1)
        }),
        "Node floors 1500 ms to timeout=1"
    );
    assert_eq!(
        advertised(Version::HTTP_11, &plain, Duration::from_millis(200)),
        Some(KeepAliveHeader {
            timeout_seconds: Some(0)
        }),
        "Node advertises timeout=0 for a positive sub-second value"
    );
    assert_eq!(
        advertised(Version::HTTP_11, &plain, Duration::ZERO),
        Some(KeepAliveHeader {
            timeout_seconds: None
        }),
        "keepAliveTimeout=0 keeps the connection but omits Keep-Alive"
    );
    assert_eq!(
        advertised(Version::HTTP_10, &plain, Duration::from_secs(5)),
        None,
        "Node answers HTTP/1.0 with Connection: close"
    );
    let mut close = HeaderMap::new();
    close.insert(
        header::CONNECTION,
        header::HeaderValue::from_static("keep-alive, CLOSE"),
    );
    assert_eq!(
        advertised(Version::HTTP_11, &close, Duration::from_secs(5)),
        None
    );

    // Explicit close responses (431/408) must keep their close pair.
    let mut response = empty_response(StatusCode::REQUEST_TIMEOUT);
    add_keep_alive_headers(
        &mut response,
        Some(KeepAliveHeader {
            timeout_seconds: Some(5),
        }),
    );
    assert_eq!(response.headers().get(header::CONNECTION).unwrap(), "close");
    assert!(response.headers().get("keep-alive").is_none());
}

/// End-to-end advertisement: the persistent 405 response and HEAD response
/// carry Node's pair, while `Connection: close` keeps the close response clean.
#[tokio::test]
async fn persistent_responses_advertise_keep_alive_like_node() {
    let (store, directory) = file_store("keep-alive-header");
    let (server, port) = start_limited(store, ServeLimits::default()).await;

    let mut persistent = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    persistent.write_all(&http_get(false)).await.unwrap();
    let head = read_head(&mut persistent).await.to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 405"), "{head}");
    assert!(head.contains("connection: keep-alive"), "{head}");
    assert!(head.contains("keep-alive: timeout=5"), "{head}");

    let mut closing = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    closing.write_all(&http_get(true)).await.unwrap();
    let head = read_head(&mut closing).await.to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 405"), "{head}");
    assert!(head.contains("connection: close"), "{head}");
    assert!(
        !head.contains("keep-alive:"),
        "a close response must not advertise keep-alive: {head}"
    );

    let mut head_request = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    head_request.write_all(&http_head()).await.unwrap();
    let head = read_head(&mut head_request).await.to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 405"), "{head}");
    assert!(head.contains("connection: keep-alive"), "{head}");
    assert!(head.contains("keep-alive: timeout=5"), "{head}");

    drop((persistent, closing, head_request));
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

// ---------------------------------------------------------------------------
// Responses that return before the request body is consumed
// ---------------------------------------------------------------------------
//
// The TS server collects the whole body before it calls the handler, so Node
// drains an unread body with `_dump` and keeps the connection alive
// (`_tmp/20261025-issue-679/fix-r2/probe/node-real-server.out.txt`: a full
// 1 MiB body keeps `Connection: keep-alive` + `Keep-Alive: timeout=5` and a
// second request succeeds on 401/404; Node rejects GET-with-body with 500 but
// still keeps the connection, while Rust precheck answers 405). Rust's early
// rejection deliberately does not
// read that body (`A7.4-HTTPSTORE-AUTH-BEFORE-BODY-001`), and hyper closes a
// connection whose body it could not drain in one poll. These responses
// therefore carry an explicit `Connection: close` instead of advertising a
// connection that is about to die (`.../probe/rust-raw-before.log` shows the old
// `keep-alive` + `timeout=5` followed by EOF for every case below).

const UNREAD_BODY_BYTES: usize = 1024 * 1024;

async fn early_response_raw(tag: &str, request: &[u8], expected: u16) {
    let (store, directory) = file_store(tag);
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let (response, closed) = exchange_until_eof(port, request, Duration::from_secs(10)).await;
    assert_close_without_keep_alive(&response, expected, tag);
    assert!(
        closed,
        "{tag}: the server left the connection open after an unread-body response"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

async fn early_response_with_body(
    tag: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    expected: u16,
) {
    let request = http_request(method, path, headers, body);
    early_response_raw(tag, &request, expected).await;
}

#[tokio::test]
async fn method_rejection_with_unread_body_closes_connection() {
    let body = vec![b'a'; UNREAD_BODY_BYTES];
    early_response_with_body(
        "unread-405",
        "GET",
        &format!("{STORE_PATH_PREFIX}list"),
        &[],
        &body,
        405,
    )
    .await;
}

#[tokio::test]
async fn unknown_operation_with_unread_body_closes_connection() {
    let body = vec![b'a'; UNREAD_BODY_BYTES];
    early_response_with_body("unread-404", "POST", "/x", &[], &body, 404).await;
}

#[tokio::test]
async fn unauthorized_with_unread_body_closes_connection() {
    let body = vec![b'a'; UNREAD_BODY_BYTES];
    early_response_with_body(
        "unread-401",
        "POST",
        &format!("{STORE_PATH_PREFIX}list"),
        &[("Content-Type", "application/json")],
        &body,
        401,
    )
    .await;
    // The review's other interleaving: the peer promised a body but has not
    // sent it yet. precheck still answers, and the connection must close.
    let withheld = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Content-Type: application/json\r\nContent-Length: {UNREAD_BODY_BYTES}\r\n\r\n"
    );
    early_response_raw("unread-401-withheld", withheld.as_bytes(), 401).await;
}

/// The Content-Length precheck rejects a > `max_body_bytes` body without
/// reading it, both when the body is absent and when the peer has already sent
/// it.
#[tokio::test]
async fn oversized_content_length_rejection_closes_connection() {
    let body = vec![b'a'; 3 * 1024 * 1024]; // default cap is 2 MiB
    early_response_with_body(
        "unread-413-content-length",
        "POST",
        &format!("{STORE_PATH_PREFIX}list"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
        ],
        &body,
        413,
    )
    .await;
    // The review's exact interleaving: Content-Length says over the cap but the
    // body has not arrived at all.
    let withheld = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        3 * 1024 * 1024
    );
    early_response_raw("unread-413-withheld", withheld.as_bytes(), 413).await;
}

/// `Limited` stops at the first byte over the cap, so the rest of a chunked
/// body is still unread even though the response is built inside the body
/// reader. Node has no cap; the early 413 is the registered body-limit
/// deviation.
#[tokio::test]
async fn oversized_chunked_body_rejection_closes_connection() {
    let chunk = vec![b'a'; 1024 * 1024];
    let request = chunked_request(
        "POST",
        &format!("{STORE_PATH_PREFIX}list"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
        ],
        &[&chunk, &chunk, &chunk],
    );
    let (store, directory) = file_store("unread-413-chunked");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let (response, closed) = exchange_until_eof(port, &request, Duration::from_secs(10)).await;
    assert_close_without_keep_alive(&response, 413, "unread-413-chunked");
    assert!(closed, "the connection stayed open after a chunked 413");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node v22.22.0 answers HTTP/1.0 with `Connection: close` even when the
/// request carries `Connection: keep-alive` (measured against the real TS
/// server: `.../probe/node-real-server.out.txt`, plus a focused probe showing
/// `res.shouldKeepAlive === true` but `Connection: close` on the wire). Rust
/// must not let hyper turn the parsed keep-alive into an advertisement.
#[tokio::test]
async fn http_10_requests_close_like_node() {
    let (store, directory) = file_store("http-1.0-close");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    for request in [
        "GET /x HTTP/1.0\r\nHost: h\r\nConnection: keep-alive\r\n\r\n",
        "GET /x HTTP/1.0\r\nHost: h\r\n\r\n",
    ] {
        let (response, closed) =
            exchange_until_eof(port, request.as_bytes(), Duration::from_secs(5)).await;
        assert_close_without_keep_alive(&response, 405, request);
        assert!(closed, "{request:?}: HTTP/1.0 connection stayed open");
    }
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node v22.22.0 S1: `server.close()` inside the handler, before the
/// response head is decided, still advertises keep-alive and the same socket
/// serves the next request; the connection then closes at the idle deadline
/// (`lifecycle-matrix/node-lifecycle-matrix.json`,
/// `s1_close_in_handler_before_head`; fix-r1 probe
/// `_tmp/20261026-issue-702/fix-r1/node-s1-probe.out.txt`).
///
/// The test name is historical and is referenced by
/// `A7.4-HTTPSTORE-KEEPALIVE-001-R2/R3`, so it is kept; fix-r1 corrected the
/// assertion from the old Rust-only `connection: close` deviation to Node's
/// bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_inflight_request_does_not_advertise_keep_alive() {
    let directory = temp_dir("shutdown-advertisement");
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(BlockingStore {
        inner: FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap(),
        entered: Mutex::new(0),
        released: AtomicBool::new(false),
        condvar: Condvar::new(),
        entered_tx,
    });
    let limits = ServeLimits {
        // The observable Node idle deadline is keepAliveTimeout + buffer;
        // shorten both so the post-close idle close is asserted, not waited
        // out. The advertisement uses keepAliveTimeout itself (timeout=1).
        keep_alive_timeout: Duration::from_secs(1),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let (server, port) =
        start_limited(store.clone() as Arc<dyn LiveGatewayUserDialogStore>, limits).await;
    let body = put_body();
    let request = http_request(
        "POST",
        &format!("{STORE_PATH_PREFIX}put"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(&request).await.unwrap();
    // The blocking store proves the request is in flight before shutdown. No
    // deadline race: without this signal the store would not release.
    tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
        .await
        .expect("the request never reached dispatch")
        .expect("entered channel closed");
    let accept_loop = server.task.as_ref().unwrap().abort_handle();
    let close_task = tokio::spawn(async move { server.close().await });
    // The connection future is parked on `spawn_blocking`, so the shutdown
    // branch is polled while the request is still in flight and the response
    // head has not been decided yet. This is an ordering yield, not a deadline
    // assertion.
    tokio::time::sleep(Duration::from_millis(100)).await;
    store.released.store(true, Ordering::SeqCst);
    store.condvar.notify_all();

    let response = tokio::time::timeout_at(response_phase_deadline(), read_response(&mut stream))
        .await
        .expect("the in-flight response never arrived");
    assert_eq!(status(&response), 200, "{response:?}");
    let head = String::from_utf8_lossy(&response).to_ascii_lowercase();
    assert!(
        head.contains("connection: keep-alive") && head.contains("keep-alive: timeout=1"),
        "Node keeps the in-flight advertisement even after close(): {head}"
    );
    assert!(
        !head.contains("connection: close"),
        "close() must not turn the in-flight advertisement into a close: {head}"
    );

    // The same socket must still serve the next request, exactly like Node's
    // `s1_close_in_handler_before_head` second request.
    let second_request = http_request(
        "POST",
        &format!("{STORE_PATH_PREFIX}list"),
        &[
            ("Authorization", &format!("Bearer {TOKEN}")),
            ("Content-Type", "application/json"),
        ],
        &list_body(),
    );
    stream.write_all(&second_request).await.unwrap();
    let second = tokio::time::timeout_at(response_phase_deadline(), read_response(&mut stream))
        .await
        .expect("the follow-up request was not served after close()");
    assert_eq!(status(&second), 200, "{second:?}");
    let second_head = String::from_utf8_lossy(&second).to_ascii_lowercase();
    assert!(
        second_head.contains("connection: keep-alive")
            && second_head.contains("keep-alive: timeout=1"),
        "the post-close request is still on the advertised keep-alive connection: {second_head}"
    );

    // Node keeps the connection until the idle deadline and only then ends it;
    // close() resolves with that EOF.
    let started = Instant::now();
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte))
        .await
        .expect("the post-close keep-alive connection did not reach its idle deadline");
    assert_eq!(read.unwrap(), 0, "expected EOF at the idle deadline");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the idle timer did not fire promptly: {:?}",
        started.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(5), close_task)
        .await
        .expect("close() hung after the in-flight request")
        .unwrap();
    assert!(
        accept_loop.is_finished(),
        "the original listener task is still running after close()"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// HEAD responses are written as `Encoder::length(0)` and hyper drops the
/// (non-empty) `KeepAliveBody` without polling it, so only the body's `Drop`
/// can arm the idle timer. Without it both connections stay in
/// `TimeoutPhase::Request` with no deadline and pin their permits forever.
#[tokio::test]
async fn head_requests_arm_idle_timer_and_release_permits() {
    let (store, directory) = file_store("head-idle");
    let limits = ServeLimits {
        // The idle deadline is 1 s, not 200 ms: the permits are first held by
        // incomplete request heads, so the overflow precondition is bounded by
        // the 30 s headersTimeout instead of racing the idle deadline.
        headers_timeout: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(1),
        keep_alive_timeout_buffer: Duration::ZERO,
        max_connections: 2,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut incomplete = http_head();
    incomplete.truncate(incomplete.len() - 2); // no final CRLF: head stays incomplete
    let mut heads = Vec::new();
    for _ in 0..2 {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(&incomplete).await.unwrap();
        heads.push(stream);
    }
    // Retry rather than assuming the two sockets were accepted first; both
    // permits are provably held once a fresh connection answers 503.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let overflow = exchange(port, &http_get(true)).await.unwrap();
        if status(&overflow) == 503 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the incomplete HEAD heads never held both permits"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for stream in &mut heads {
        stream.write_all(b"\r\n").await.unwrap();
        let head = read_head(stream).await;
        assert!(
            head.starts_with("HTTP/1.1 405"),
            "HEAD must reach precheck: {head:?}"
        );
        assert!(
            head.to_ascii_lowercase().contains("content-length:"),
            "HEAD keeps the entity length in the head: {head:?}"
        );
    }
    // The HEAD responses have no body on the wire, so only the body's Drop can
    // arm the idle timer for each connection.
    for stream in &mut heads {
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
            .await
            .expect("a HEAD keep-alive connection outlived the idle timer");
        assert_eq!(read.unwrap(), 0, "expected EOF on the idle HEAD connection");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = exchange(port, &http_get(true)).await.unwrap();
        if status(&response) == 405 {
            break;
        }
        assert_eq!(status(&response), 503);
        assert!(
            Instant::now() < deadline,
            "HEAD connections never released their permits"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// An empty body (`is_end_stream()` true when the response is built) is never
/// stored by hyper's dispatcher, so `poll_frame` is not called on that path
/// either. This serves a 204 over the production `TimeoutStream` + timer and
/// asserts the idle deadline both closes the socket and releases the permit.
#[tokio::test]
async fn empty_response_body_arms_idle_timer_without_a_body_poll() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_secs(5),
        Duration::from_millis(200),
        Arc::new(AtomicBool::new(false)),
    ));
    let (client, server_io) = tokio::io::duplex(8 * 1024);
    let service_timer = timer.clone();
    let service = service_fn(move |_request: Request<Incoming>| {
        let timer = service_timer.clone();
        async move {
            timer.request_started();
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(KeepAliveBody::new(Full::new(Bytes::new()), timer))
                    .expect("static response"),
            )
        }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(None::<Duration>)
        .serve_connection(
            TokioIo::new(TimeoutStream::new(
                server_io,
                timer,
                DEFAULT_MAX_HEADER_BYTES,
            )),
            service,
        );
    let permits = Arc::new(Semaphore::new(1));
    let permit = permits.clone().try_acquire_owned().unwrap();
    let task = tokio::spawn(async move {
        let _permit = permit;
        connection
            .await
            .expect("connection ended after the idle timeout");
    });

    let mut client = client;
    // The idle deadline is armed when the server writes/drops the response,
    // which happens after this point. Taking `started` before the write makes
    // the lower bound independent of client-side scheduling after `read_head`.
    let started = Instant::now();
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: h\r\n\r\n")
        .await
        .unwrap();
    let head = read_head(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 204"), "{head:?}");
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
        .await
        .expect("an empty-body keep-alive connection outlived the idle timer");
    assert_eq!(read.unwrap(), 0, "expected EOF after the idle timeout");
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the connection closed before the advertised idle deadline: {:?}",
        started.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("the connection task did not end after the idle timeout")
        .unwrap();
    assert!(
        permits.try_acquire().is_ok(),
        "the connection permit was not released"
    );
}

/// A peer that has already shut down its read half cannot observe the EOF; the
/// permit is the only observable proof that the idle timer still closed the
/// HEAD connection. Without the `Drop` arm this connection would pin the only
/// permit forever.
#[tokio::test]
async fn client_read_shutdown_still_releases_head_connection_permit() {
    let (store, directory) = file_store("head-client-read-shutdown");
    let limits = ServeLimits {
        // Same decoupling as the HEAD test: hold the permit with an incomplete
        // head (30 s headersTimeout), then let the 1 s idle deadline close it.
        headers_timeout: Duration::from_secs(30),
        keep_alive_timeout: Duration::from_secs(1),
        keep_alive_timeout_buffer: Duration::ZERO,
        max_connections: 1,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut incomplete = http_head();
    incomplete.truncate(incomplete.len() - 2);
    stream.write_all(&incomplete).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if status(&exchange(port, &http_get(true)).await.unwrap()) == 503 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the incomplete HEAD head never held the only permit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stream.write_all(b"\r\n").await.unwrap();
    let head = read_head(&mut stream).await;
    assert!(head.starts_with("HTTP/1.1 405"), "{head:?}");
    let stream = stream.into_std().expect("into_std");
    stream
        .shutdown(std::net::Shutdown::Read)
        .expect("shutdown read half");

    // The peer can no longer observe EOF, so permit release is the event that
    // proves the idle timer closed the connection.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = exchange(port, &http_get(true)).await.unwrap();
        if status(&response) == 405 {
            break;
        }
        assert_eq!(status(&response), 503);
        assert!(
            Instant::now() < deadline,
            "a read-shutdown HEAD connection never released its permit"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(stream);
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// `finished` must keep `response_finished` exactly once per response body: a
/// fully polled body that is dropped later must not postpone the deadline.
#[tokio::test]
async fn dropping_a_fully_polled_body_does_not_rearm_the_idle_deadline() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_secs(5),
        Duration::from_secs(1),
        Arc::new(AtomicBool::new(false)),
    ));
    timer.request_started();
    let mut body = KeepAliveBody::new(Full::new(Bytes::from_static(b"x")), timer.clone());
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        if frame.is_none() {
            break;
        }
    }
    let deadline = timer.nearest_deadline().expect("idle deadline");
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(body);
    assert_eq!(
        timer.nearest_deadline(),
        Some(deadline),
        "Drop re-armed the idle timer after a fully polled body"
    );
}

/// Pin the measured Node v22.22.0 defaults: 5 s keepAliveTimeout plus the 1 s
/// buffer (6 s observable idle deadline), 60 s headersTimeout and 300 s
/// requestTimeout.
#[test]
fn default_connection_timeouts_match_node_v22() {
    assert_eq!(DEFAULT_KEEP_ALIVE_TIMEOUT, Duration::from_secs(5));
    assert_eq!(DEFAULT_KEEP_ALIVE_TIMEOUT_BUFFER, Duration::from_secs(1));
    assert_eq!(DEFAULT_HEADERS_TIMEOUT, Duration::from_secs(60));
    assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(300));
    let limits = ServeLimits::default();
    assert_eq!(
        limits.keep_alive_timeout + limits.keep_alive_timeout_buffer,
        Duration::from_secs(6)
    );
    assert_eq!(NODE_408.len(), 51, "Node requestTimeoutResponse");
    assert_eq!(
        NODE_431.len(),
        67,
        "Node requestHeaderFieldsTooLargeResponse"
    );
}

/// #702 S2/S3 boundary: a close decision must use bytes that actually reached
/// the socket. hyper can report the body as finished while the whole body is
/// still in its write buffer, so `response_finished` alone is not enough.
#[test]
fn keep_alive_close_race_uses_wire_bytes_not_body_completion() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_secs(5),
        Duration::from_secs(1),
        Arc::new(AtomicBool::new(false)),
    ));
    {
        let mut lifecycle = timer.lifecycle.lock().unwrap();
        lifecycle.advertised_keep_alive = true;
        lifecycle.response_write.expected_body = 5;
    }
    // Head only: the advertisement is on the wire but the body is not.
    timer.record_response_write(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n");
    assert!(
        timer.close_raced_with_advertised_keep_alive(),
        "a partially written advertised response must not be gracefully closed"
    );
    timer.record_response_write(b"hello");
    assert!(
        !timer.close_raced_with_advertised_keep_alive(),
        "a fully flushed advertisement may be closed as an idle connection"
    );
    // close after the response is on the wire: response_finished must not
    // defer the close even though shutdown started.
    timer.shutdown_started.store(true, Ordering::SeqCst);
    timer.response_finished();
    assert!(
        !timer.close_raced_with_advertised_keep_alive(),
        "close after a fully flushed response is an idle-connection close"
    );
    // close while the body was only pulled into hyper's buffer: the wire
    // tracker still says not flushed, so the advertisement must be honoured.
    {
        let mut lifecycle = timer.lifecycle.lock().unwrap();
        lifecycle.advertised_keep_alive = true;
        lifecycle.close_after_response = false;
        lifecycle.response_write = ResponseWrite {
            expected_body: 5,
            ..ResponseWrite::default()
        };
    }
    timer.response_finished();
    assert!(
        timer.close_raced_with_advertised_keep_alive(),
        "a body-complete event before the bytes leave must still defer the close"
    );
    // S1: close() while a request is in flight, before the head is decided.
    // No advertisement exists yet, so the "advertised && !flushed" test is
    // false; `request_in_flight` alone must defer graceful_shutdown.
    {
        let mut lifecycle = timer.lifecycle.lock().unwrap();
        *lifecycle = ConnectionLifecycle::default();
    }
    timer.request_started();
    assert!(
        timer.close_raced_with_advertised_keep_alive(),
        "an in-flight request must defer close even before its head is decided"
    );
    timer.response_finished();
    assert!(
        !timer.close_raced_with_advertised_keep_alive(),
        "after the response ends, close() is an idle-connection close"
    );
    // close() found the connection idle: the service closure must see this
    // decision and answer close instead of advertising keep-alive.
    timer.mark_graceful_shutdown();
    assert!(
        timer.lifecycle.lock().unwrap().graceful_shutdown_started,
        "the graceful-shutdown decision must be visible to the service closure"
    );
}

/// A header deadline that expires after `server.close()` started must become
/// Node's silent `socket.destroy()` EOF, not a fabricated 408 (Node clears its
/// connections checker on close and never writes 408 during shutdown).
#[tokio::test]
async fn shutdown_folds_header_deadline_into_silent_eof() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_millis(20),
        Duration::from_secs(1),
        Arc::new(AtomicBool::new(false)),
    ));
    let past = tokio::time::Instant::now() + Duration::from_millis(50);
    assert_eq!(timer.expired(past), ExpiredTimer::Headers);
    timer.begin_shutdown();
    assert_eq!(timer.expired(past), ExpiredTimer::Idle);
}

/// Replays Node v22.22.0's `maxHeaderSize` accounting: the request target,
/// every field name, and every field value count; method, spaces, `:`, CRLF
/// and leading value whitespace do not; the running total rejects at `>=`
/// (16,383 pass, 16,384 fail). The total includes an incomplete line, so a
/// head cannot grow past the budget while waiting for its terminator.
#[test]
fn header_guard_replays_node_field_budget() {
    // The request target counts, but the method/version/request-line
    // separators do not: target `/123456789012` is 13 bytes.
    let mut guard = HeaderGuard::new(16);
    assert!(
        !guard.push(b"LONGMETHOD /123456789012 HTTP/1.1\r\n"),
        "only the 13 target bytes are counted"
    );
    assert!(!guard.push(b"A: b\r\n"), "A + b = 2, running total 15");
    // `B: c` starts with the 16th counted byte (the field name); the limit is
    // `>=`, so this push already rejects.
    assert!(guard.push(b"B: c\r\n"), "16 counted bytes reach the limit");

    // Leading value whitespace is excluded and an all-whitespace value counts
    // nothing; trailing whitespace belongs to the value and does count.
    let mut guard = HeaderGuard::new(16);
    assert!(!guard.push(b"GET / HTTP/1.1\r\n"), "target '/' = 1");
    assert!(
        !guard.push(b"X:      ab   \r\n"),
        "X + `ab   ` = 6 counted, total 7"
    );
    assert!(
        !guard.push(b"Y:        \r\n"),
        "name Y counts 1 and an all-whitespace value counts 0, total 8"
    );
    assert!(!guard.push(&[b'A'; 7]), "7 more field bytes, total 15");
    assert!(guard.push(&[b'A'; 1]), "the 16th counted byte rejects");

    // An incomplete line is charged as it arrives, not only when it ends.
    let mut guard = HeaderGuard::new(16);
    assert!(!guard.push(b"GET / HTTP/1.1\r\n"), "target '/' = 1");
    assert!(!guard.push(b"X: "), "X = 2");
    assert!(
        !guard.push(&[b'a'; 13]),
        "13 value bytes bring the total to 15"
    );
    assert!(
        guard.push(&[b'a'; 1]),
        "the 16th counted byte rejects before the line ends"
    );

    // The raw cap is a separate bound above the densest Node-legal head; a
    // head padded with uncounted leading whitespace still cannot grow hyper's
    // buffer without bound.
    let max = DEFAULT_MAX_HEADER_BYTES;
    assert_eq!(header_guard_raw_limit(max), 4 * max + 2 * 8 * 1024);
    assert!(header_guard_raw_limit(max) < hyper_buffer_bytes(max));
    let mut guard = HeaderGuard::new(max);
    assert!(!guard.push(b"GET / HTTP/1.1\r\n"));
    assert!(!guard.push(b"X:"), "colon starts the value");
    let padding = vec![b' '; 4 * max + 2 * 8 * 1024];
    assert!(
        guard.push(&padding),
        "uncounted leading whitespace is still bounded"
    );
}

/// A mutation that changes the default host to `0.0.0.0` must be caught: every
/// other test passes an explicit host.
#[tokio::test]
async fn default_host_listens_on_loopback() {
    let (store, directory) = file_store("default-host");
    let server = start_gateway_user_dialog_store_http_server(store, TOKEN, None, Some(0))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "loopback listen failed ({error}); this sandbox blocks TcpListener::bind. \
                 Outside the sandbox rerun: cargo test -p pilotdeck-gateway --lib \
                 user_dialog::http_store -- --nocapture"
            )
        });
    assert!(
        server.url().starts_with("http://127.0.0.1:"),
        "default host must be loopback: {}",
        server.url()
    );
    let port = url::Url::parse(server.url()).unwrap().port().unwrap();
    let response = exchange(port, &http_get(true)).await.unwrap();
    assert_eq!(status(&response), 405);
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Auth is checked before a body byte is read. With a huge Content-Length and
/// a 200 ms request timeout, reading first would answer 408 after the timeout;
/// the correct order answers 401 immediately.
#[tokio::test]
async fn anonymous_oversized_body_is_rejected_before_reading() {
    let (store, directory) = file_store("auth-before-body");
    let limits = ServeLimits {
        request_timeout: Duration::from_millis(200),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let request = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\n\
         Content-Length: 4294967296\r\nConnection: close\r\n\r\n"
    );
    let started = Instant::now();
    let response = exchange_within(port, request.as_bytes(), Duration::from_secs(1))
        .await
        .expect("anonymous oversized request was not answered from the headers");
    assert_eq!(status(&response), 401, "auth must run before the body");
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "the server waited for the body: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// S2×E1T1: a partial first request head must receive Node's exact 51-byte
/// 408 (`requestTimeoutResponse`), not the pre-#699 zero-byte EOF.
async fn assert_partial_headers_receive_node_408() {
    let (store, directory) = file_store("headers-timeout");
    let limits = ServeLimits {
        headers_timeout: Duration::from_millis(200),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\n")
        .await
        .unwrap();
    let started = Instant::now();
    expect_terminal_bytes(&mut stream, NODE_408, "408 partial first head").await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "headersTimeout did not fire promptly: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn partial_headers_receive_node_408_bytes() {
    assert_partial_headers_receive_node_408().await;
}

/// Frozen-ledger anchor (`accepted-diffs.md:576`, `:603`). fix-r1 renamed this
/// test to [`partial_headers_receive_node_408_bytes`]; the ledger is
/// byte-frozen, so the old name stays as a wrapper. The assertion still means
/// "a partial head is closed after `headersTimeout`", now with Node's 51 bytes
/// in front of the EOF.
#[tokio::test]
async fn partial_headers_are_closed_after_headers_timeout() {
    assert_partial_headers_receive_node_408().await;
}

/// S1×E1T1: Node arms `headersTimeout` on an accepted connection even before
/// the first byte (probe `initial-zero-bytes`); the Rust timer must do the same
/// and answer with the same 408 bytes.
#[tokio::test]
async fn idle_connection_receives_node_408_bytes() {
    let (store, directory) = file_store("zero-byte-headers-timeout");
    let limits = ServeLimits {
        headers_timeout: Duration::from_millis(200),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    expect_terminal_bytes(&mut stream, NODE_408, "408 zero-byte connection").await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Negative control for the 408 branch: a complete request inside
/// `headersTimeout` is answered normally, so the bytes above are not emitted
/// for every connection.
#[tokio::test]
async fn complete_request_within_headers_timeout_is_not_408() {
    let (store, directory) = file_store("headers-timeout-control");
    let limits = ServeLimits {
        headers_timeout: Duration::from_millis(300),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let response = exchange_within(port, &http_get(true), Duration::from_secs(1))
        .await
        .expect("a complete request waited for headersTimeout");
    assert_eq!(status(&response), 405, "{response:?}");
    assert!(
        !response.starts_with(NODE_408),
        "a complete request must not receive the 408 bytes"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// S2×E2T2: the header guard resets after a keep-alive response, so an
/// oversized follow-up head also gets Node's exact 67-byte 431.
#[tokio::test]
async fn next_request_oversized_headers_receive_node_431_bytes() {
    let (store, directory) = file_store("next-oversized-headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(&http_get(false)).await.unwrap();
    let response = read_response(&mut stream).await;
    assert!(response.starts_with(b"HTTP/1.1 405"), "{response:?}");

    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {}\r\nConnection: close\r\n\r\n",
        "a".repeat(DEFAULT_MAX_HEADER_BYTES)
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    expect_terminal_bytes(&mut stream, NODE_431, "431 oversized follow-up head").await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// S2×E3: a peer that closes mid-head is not a headersTimeout. The server must
/// not fabricate a 408 (there is no socket left to write it to) and must keep
/// serving new connections.
#[tokio::test]
async fn client_close_mid_headers_sends_no_408_and_server_recovers() {
    let (store, directory) = file_store("client-close-headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut partial = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    partial
        .write_all(b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\nX-Partial: ")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(partial);

    let response = exchange_within(port, &http_get(true), Duration::from_secs(1))
        .await
        .expect("the server stopped answering after a client close");
    assert_eq!(status(&response), 405, "{response:?}");
    assert!(
        !response.starts_with(NODE_408),
        "a client close must not be reported as a timeout"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// S2×E4: Node's `server.close()` clears the connections-checking interval and
/// does not destroy a connection whose parser already consumed a partial head
/// (probe `short-shutdown-initial-partial`, still open after 6 s), so it never
/// fabricates a 408 on shutdown. hyper's graceful shutdown has the same shape
/// here; the test pins "no bytes, no 408" and lets the peer close to finish.
#[tokio::test]
async fn shutdown_does_not_turn_a_partial_head_into_408() {
    let (store, directory) = file_store("shutdown-partial-headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\nX-Partial: ")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let closing = tokio::spawn(async move { server.close().await });
    let mut byte = [0u8; 1];
    match tokio::time::timeout(Duration::from_millis(250), stream.read(&mut byte)).await {
        Ok(Ok(0)) => {}
        Ok(Ok(read)) => panic!("shutdown wrote {read} unexpected byte(s): {byte:?}"),
        Ok(Err(error)) => panic!("shutdown read failed instead of hanging/closing: {error}"),
        Err(_) => {}
    }
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .expect("close() did not finish after the peer closed a partial head")
        .unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

/// S1×E4: a zero-byte connection at shutdown never receives a 408. Node's own
/// `close()` races the parser (RST before registration, otherwise the cleared
/// checker leaves it open); both are byte-free, so this accepts EOF, a reset,
/// or "still open until the peer closes".
#[tokio::test]
async fn shutdown_zero_byte_connection_sends_no_408() {
    let (store, directory) = file_store("shutdown-zero-byte");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let closing = tokio::spawn(async move { server.close().await });
    let mut byte = [0u8; 1];
    match tokio::time::timeout(Duration::from_millis(250), stream.read(&mut byte)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(read)) => panic!("shutdown wrote {read} unexpected byte(s): {byte:?}"),
        Err(_) => {}
    }
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .expect("close() did not finish after the peer closed a zero-byte connection")
        .unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

/// S3×E3: a peer that closes mid-body is not a headersTimeout. No 408 may be
/// fabricated and the server must still serve new connections.
#[tokio::test]
async fn client_close_mid_body_sends_no_408_and_server_recovers() {
    let (store, directory) = file_store("client-close-body");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut partial = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST {STORE_PATH_PREFIX}put HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: 100\r\n\r\nhello"
    );
    partial.write_all(head.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(partial);

    let response = exchange_within(port, &http_get(true), Duration::from_secs(1))
        .await
        .expect("the server stopped answering after a mid-body client close");
    assert_eq!(status(&response), 405, "{response:?}");
    assert!(
        !response.starts_with(NODE_408),
        "a client close must not be reported as a timeout"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Send `request`, half-close the write side (`shutdown(SHUT_WR)`) and read
/// everything the server writes until it closes. This is the raw-socket shape
/// Node's TCP peers use (`nc`): the peer only stops writing, its read half
/// stays open, so the server must still answer.
async fn half_close_exchange(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    if !request.is_empty() {
        stream.write_all(request).await.unwrap();
    }
    stream.shutdown().await.expect("shutdown write half");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
        .await
        .expect("the server never closed after the peer half-closed")
        .unwrap();
    response
}

/// GLM fix-r2 P1: a peer FIN is a read-side half-close, not "connection
/// closed". fix-r1 set one `closed` flag on read EOF, so `poll_write` answered
/// every later response with BrokenPipe and a half-closed peer saw 0 bytes.
///
/// Node v22.22.0 (probe `_tmp/20261025-issue-699/fix-r2/node/probe-half-close.mjs`)
/// sends 0 bytes for an empty connection, the 47-byte parser-level 400 for an
/// incomplete head or body, and the normal response for a complete request.
/// The complete-request cell is the positive control: the incomplete-response
/// branch must not swallow a request that has nothing left to read.
#[tokio::test]
async fn half_closed_peers_receive_node_bytes() {
    let (store, directory) = file_store("half-close");
    let (server, port) = start_limited(store, ServeLimits::default()).await;

    // S0: no byte of a request arrived -> Node closes silently.
    let empty = half_close_exchange(port, b"").await;
    assert!(
        empty.is_empty(),
        "an empty half-closed connection must stay silent: {empty:?}"
    );

    // S1: some bytes of a request head arrived, but never the blank line.
    let partial_head = half_close_exchange(
        port,
        b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\nAuthoriz",
    )
    .await;
    assert_eq!(
        partial_head,
        NODE_400,
        "Node answers a partial head + FIN with the parser's 400: {:?}",
        String::from_utf8_lossy(&partial_head)
    );

    // S2: the head completed, the Content-Length body did not.
    let half_body = half_close_exchange(
        port,
        format!(
            "POST {STORE_PATH_PREFIX}put HTTP/1.1\r\nHost: 127.0.0.1\r\n\
             Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
             Content-Length: 100\r\n\r\nhello"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        half_body,
        NODE_400,
        "Node answers an incomplete body + FIN with the parser's 400 \
         (hyper's 500 must not be appended): {:?}",
        String::from_utf8_lossy(&half_body)
    );

    // S3 positive control: a complete request that half-closes still reaches
    // the handler and gets the real response, exactly like Node.
    let complete = half_close_exchange(port, &http_post(&list_body())).await;
    assert_eq!(
        status(&complete),
        200,
        "a complete request + FIN must still get its response: {:?}",
        String::from_utf8_lossy(&complete)
    );
    assert!(
        complete.ends_with(br#"{"version":1,"result":[]}"#),
        "the complete response body must reach the half-closed peer: {:?}",
        String::from_utf8_lossy(&complete)
    );
    assert!(
        !complete.starts_with(NODE_400),
        "a complete request must not be answered with the incomplete-request 400"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// S3×E4: `close()` waits for the in-flight request; Rust's `requestTimeout`
/// still bounds an incomplete body and writes a 408 before closing. Node's
/// `close()` clears its checker and hangs instead (registered difference: the
/// body 408 shape/termination is outside #699's header scope).
#[tokio::test]
async fn shutdown_with_partial_body_finishes_at_request_timeout() {
    let (store, directory) = file_store("shutdown-partial-body");
    let limits = ServeLimits {
        request_timeout: Duration::from_millis(200),
        headers_timeout: Duration::from_secs(5),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST {STORE_PATH_PREFIX}put HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: 100\r\n\r\nhello"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let closing = tokio::spawn(async move { server.close().await });
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .expect("requestTimeout did not fire during shutdown")
        .unwrap();
    // #1232: the bytes are Node's 51-byte 408 (probe `body-timeout-default`);
    // the termination boundary itself (Rust writes it, Node's cleared checker
    // hangs) stays the registered shutdown difference from #699.
    assert_eq!(
        response, NODE_408,
        "#1232 the shutdown body-timeout must be Node's 51-byte 408"
    );
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .expect("close() did not finish after the body timeout")
        .unwrap();
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn slow_request_bodies_are_closed_after_request_timeout() {
    let (store, directory) = file_store("request-timeout");
    let limits = ServeLimits {
        request_timeout: Duration::from_millis(200),
        headers_timeout: Duration::from_secs(5),
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let request = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Length: 100\r\nConnection: close\r\n\r\nhello"
    );
    let response = tokio::time::timeout(Duration::from_secs(3), exchange(port, request.as_bytes()))
        .await
        .expect("#1232 requestTimeout did not fire")
        .expect("#1232 the body-timeout exchange must return response bytes");
    // #1232 M1 (probe `body-timeout-default`): Node's `requestTimeout`
    // writes the exact 51-byte `requestTimeoutResponse`, not hyper's
    // encoding with `content-length` / `date`.
    assert_eq!(
        response, NODE_408,
        "#1232 the body requestTimeout must be Node's 51-byte 408"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

// ---------------------------------------------------------------------------
// #1232: the pipelined-head matrix. Every cell below replays a shape probed
// against the real TS server (Node v22.22.0,
// `_tmp/20261026-issue-1232/fix-r1/node/probe-408-431-shape.mjs`,
// `probe-408-431-shape.json`); the expected wire bytes in the comments are
// the probe's recorded output, never hand-written.
// ---------------------------------------------------------------------------

/// A `POST list` head whose body arrives separately (the pipelined-matrix
/// first request).
fn pipelined_list_head(body_len: usize) -> String {
    format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {body_len}\r\n\r\n"
    )
}

/// A `POST list` head with a chunked body.
fn pipelined_chunked_head() -> String {
    format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Transfer-Encoding: chunked\r\n\r\n"
    )
}

/// The follow-up head of every pipelined cell: `X-Pad` carries the padding,
/// the body is empty, so an under-limit head is a well-formed request that
/// answers 400 (`invalid_json`, empty body), exactly like the probe's.
fn pipelined_follow_up_head(padding: usize, content_length: usize) -> String {
    format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nX-Pad: {}\r\nContent-Length: {content_length}\r\n\r\n",
        "a".repeat(padding)
    )
}

/// One chunk of a chunked body, with optional chunk extensions.
fn chunk_frame(body: &[u8], extension: &str) -> Vec<u8> {
    let mut frame = format!("{:x}{extension}\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    frame.extend_from_slice(b"\r\n");
    frame
}

/// #1232 M2 (probe `pipelined-oversized-body-late`, 67 bytes, EOF): the next
/// request head is pipelined while the first request is still in its body
/// phase. Node's parser scans the bytes the moment they land and destroys the
/// socket with only the 67-byte 431 on it — the response the in-flight
/// request would have produced never reaches the wire (the probe recorded the
/// list handler running, so only the bytes were suppressed, not the handler).
#[tokio::test]
async fn pipelined_oversized_head_during_body_writes_node_431_only() {
    let (store, directory) = file_store("pipelined-431-body-late");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_list_head(body.len()).as_bytes())
        .await
        .expect("#1232 write the first request head");
    // Let the server enter the body phase of request 1 — the phase the old
    // guard refused to scan.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(&[body.as_slice(), oversized.as_bytes()].concat())
        .await
        .expect("#1232 write the body and the oversized follow-up head");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 a pipelined oversized head during a body answers with only the Node 431",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 M2 (probe `pipelined-oversized-single-burst`, 67 bytes, EOF): the
/// whole exchange — head, body, and the oversized follow-up head — arrives in
/// one write, like a client that has already queued the next request.
#[tokio::test]
async fn pipelined_oversized_head_single_burst_writes_node_431_only() {
    let (store, directory) = file_store("pipelined-431-single-burst");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(
            &[
                pipelined_list_head(body.len()).into_bytes(),
                body.clone(),
                oversized.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the single-burst pipelined exchange");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 a single-burst pipelined oversized head answers with only the Node 431",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 M2 (probe `pipelined-oversized-with-body`, 67 bytes, EOF): the
/// oversized follow-up head itself declares a body that keeps dribbling after
/// the head, so the server has unread bytes in flight when it answers.
#[tokio::test]
async fn pipelined_oversized_head_with_declared_body_writes_node_431_only() {
    let (store, directory) = file_store("pipelined-431-with-body");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_list_head(body.len()).as_bytes())
        .await
        .expect("#1232 write the first request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut declaring = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 1000).into_bytes();
    declaring.push(b'x');
    stream
        .write_all(&[body.as_slice(), &declaring].concat())
        .await
        .expect("#1232 write the body and the body-declaring oversized follow-up head");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 a pipelined oversized head that declares a body answers with only the Node 431",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 M2 (probe `chunked-pipelined-oversized-body-late`, 67 bytes, EOF):
/// the same pipelined shape over a `Transfer-Encoding: chunked` body; the
/// guard must track the chunk framing to find the follow-up head.
#[tokio::test]
async fn chunked_pipelined_oversized_head_writes_node_431_only() {
    let (store, directory) = file_store("chunked-pipelined-431");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_chunked_head().as_bytes())
        .await
        .expect("#1232 write the chunked request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(
            &[
                chunk_frame(&body, ""),
                b"0\r\n\r\n".to_vec(),
                oversized.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the chunked body and the oversized follow-up head");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 a pipelined oversized head behind a chunked body answers with only the Node 431",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 (probe `chunked-oversized-trailer`, 67 bytes, EOF, the list handler
/// never ran): Node counts chunked trailer fields toward the same
/// `maxHeaderSize` budget as the head, so an oversized trailer is the 67-byte
/// 431 and the still-incomplete request never gets its response.
#[tokio::test]
async fn chunked_oversized_trailer_writes_node_431_only() {
    let (store, directory) = file_store("chunked-oversized-trailer");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(
            &[
                pipelined_chunked_head().into_bytes(),
                chunk_frame(&body, ""),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the chunked request head and body");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let trailer = format!(
        "0\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(DEFAULT_MAX_HEADER_BYTES)
    );
    stream
        .write_all(trailer.as_bytes())
        .await
        .expect("#1232 write the oversized chunked trailer");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 an oversized chunked trailer is the Node 431 on the message's own budget",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 (probe `chunkext-pipelined-oversized-body-late`, 67 bytes, EOF):
/// chunk extensions do not blind the framing walk — llhttp skips them, and so
/// must the guard, or the follow-up head would ride into hyper unaccounted.
#[tokio::test]
async fn chunk_extensions_do_not_blind_the_pipelined_guard() {
    let (store, directory) = file_store("chunkext-pipelined-431");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_chunked_head().as_bytes())
        .await
        .expect("#1232 write the chunked request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(
            &[
                chunk_frame(&body, ";ext=1"),
                b"0\r\n\r\n".to_vec(),
                oversized.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the extended chunk, its terminator, and the oversized follow-up head");
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 chunk extensions must not hide a pipelined oversized head",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 M3 negative control (probe `pipelined-under-limit-body-late`, 449
/// bytes, connection still open): an under-limit follow-up head is served on
/// the same connection — response 1 (200), response 2 (400 `invalid_json`,
/// the head declared an empty body) — and a third request proves the
/// connection stayed usable.
#[tokio::test]
async fn pipelined_under_limit_head_is_served_and_connection_reusable() {
    let (store, directory) = file_store("pipelined-under-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_list_head(body.len()).as_bytes())
        .await
        .expect("#1232 write the first request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let under_limit = pipelined_follow_up_head(15_000, 0);
    stream
        .write_all(&[body.as_slice(), under_limit.as_bytes()].concat())
        .await
        .expect("#1232 write the body and the under-limit follow-up head");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 an under-limit pipelined head must not disturb the in-flight response: {first:?}"
    );
    let second = read_response(&mut stream).await;
    assert_eq!(
        status(&second),
        400,
        "#1232 the under-limit follow-up head must be served (400 invalid_json): {second:?}"
    );
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body].concat())
        .await
        .expect("#1232 write the third request");
    let third = read_response(&mut stream).await;
    assert_eq!(
        status(&third),
        200,
        "#1232 the connection must stay usable after the pipelined exchange: {third:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 M3 chunked negative control (probe
/// `chunked-pipelined-under-limit-body-late`, 449 bytes, open): the chunked
/// twin of [`pipelined_under_limit_head_is_served_and_connection_reusable`].
#[tokio::test]
async fn chunked_pipelined_under_limit_head_is_served() {
    let (store, directory) = file_store("chunked-pipelined-under-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(pipelined_chunked_head().as_bytes())
        .await
        .expect("#1232 write the chunked request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let under_limit = pipelined_follow_up_head(15_000, 0);
    stream
        .write_all(
            &[
                chunk_frame(&body, ""),
                b"0\r\n\r\n".to_vec(),
                under_limit.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the chunked body and the under-limit follow-up head");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 the chunked in-flight response must complete: {first:?}"
    );
    let second = read_response(&mut stream).await;
    assert_eq!(
        status(&second),
        400,
        "#1232 the chunked under-limit follow-up must be served: {second:?}"
    );
    let plain = list_body();
    stream
        .write_all(&[pipelined_list_head(plain.len()).into_bytes(), plain].concat())
        .await
        .expect("#1232 write the third request");
    let third = read_response(&mut stream).await;
    assert_eq!(
        status(&third),
        200,
        "#1232 the chunked connection must stay usable: {third:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 (probe `dup-cl-pipelined-oversized-body-late`): hyper accepts
/// duplicate equal `Content-Length` values (`role.rs` only rejects differing
/// values) while Node's parser answers the duplicate with its own 47-byte
/// 400 — a registered framing difference, so the guard steps aside on it and
/// the follow-up head reaches hyper's parser unaccounted. What this test pins
/// is the `serve` backstop's bytes: the oversized head still gets Node's
/// 67-byte 431 — after response 1, which was already flushed (Node instead
/// answers 47 bytes of 400 and no response 1) — and not hyper's 123-byte
/// encoding.
#[tokio::test]
async fn duplicate_content_length_pipelined_head_hits_node_431_backstop() {
    let (store, directory) = file_store("dup-cl-pipelined-431");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    let duplicate_length = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {n}\r\nContent-Length: {n}\r\n\r\n",
        n = body.len()
    );
    stream
        .write_all(duplicate_length.as_bytes())
        .await
        .expect("#1232 write the duplicate-length request head");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(&[body.as_slice(), oversized.as_bytes()].concat())
        .await
        .expect("#1232 write the body and the oversized follow-up head");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 hyper serves the duplicate-equal-Content-Length request (registered): {first:?}"
    );
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1232 the backstop must still answer an oversized head with Node's 431 bytes",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232: Node's `maxHeaderSize` accounting is per message, so the budget the
/// first head consumed must be given back after its body, and the framed body
/// itself must never be charged to it. The body here is 20 K of head-shaped
/// `aa: bb` field lines (invalid JSON, so both answers are 400): a guard that
/// forgot the `Content-Length` framing, or that kept one connection-wide
/// budget, trips on it and answers the 67-byte 431 instead.
#[tokio::test]
async fn pipelined_head_budget_resets_per_message() {
    let (store, directory) = file_store("budget-reset");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = "aa: bb\r\n".repeat(2_500);
    let first = pipelined_follow_up_head(10_000, body.len());
    let second = pipelined_follow_up_head(10_000, 0);
    stream
        .write_all(&[first.into_bytes(), body.into_bytes(), second.into_bytes()].concat())
        .await
        .expect("#1232 write the two pipelined messages and the body between them");
    let served_first = read_response(&mut stream).await;
    assert_eq!(
        status(&served_first),
        400,
        "#1232 the first ~10 K head plus its 20 K body must be served, not 431: {served_first:?}"
    );
    let served_second = read_response(&mut stream).await;
    assert_eq!(
        status(&served_second),
        400,
        "#1232 the second head must start from a fresh per-message budget, not 431: {served_second:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 (probe `large-body-served-and-connection-reusable`, responses 400
/// then 200, connection open): body bytes are not header bytes — a 40 KB body
/// is served (400 `invalid_json` for the non-JSON payload), never the 431,
/// and the connection stays reusable.
#[tokio::test]
async fn large_bodies_do_not_trip_the_header_guard() {
    let (store, directory) = file_store("large-body");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let big = vec![b'x'; 40_000];
    let head = format!(
        "POST {STORE_PATH_PREFIX}put HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        big.len()
    );
    stream
        .write_all(&[head.into_bytes(), big].concat())
        .await
        .expect("#1232 write the 40 KB body request");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        400,
        "#1232 a 40 KB body must be served, not counted against the header budget: {first:?}"
    );
    let body = list_body();
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body].concat())
        .await
        .expect("#1232 write the follow-up request after the large body");
    let second = read_response(&mut stream).await;
    assert_eq!(
        status(&second),
        200,
        "#1232 the connection must stay usable after a large body: {second:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 fix-r1 (probe `keepalive-second-te-multitoken-large-chunk`: Node
/// answers 400 `invalid_json` and keeps the connection reusable): a
/// keep-alive connection's second request carries
/// `Transfer-Encoding: gzip, chunked` and a single 48 KB chunk. Node's llhttp
/// serves that framing, so it must never see the 431. The old guard stepped
/// aside at the multi-token value, then kept scanning the same read chunk
/// (and re-armed mid-message on the next read) with a reset per-message
/// state, so the chunk's `aa: bb` bytes were counted as head bytes and the
/// connection got a false 67-byte 431 before hyper could answer (P1-1).
#[tokio::test]
async fn keep_alive_multi_token_te_large_chunked_body_is_served() {
    let (store, directory) = file_store("keepalive-te-multitoken");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body].concat())
        .await
        .expect("#1232 write the first request on the keep-alive connection");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 the first request must be served before the multi-token one: {first:?}"
    );
    // 48 KB of head-shaped `aa: bb` field lines inside one chunk: exactly the
    // bytes a guard that kept scanning with a reset state would count as the
    // next head.
    let chunk = "aa: bb\r\n".repeat(6_000);
    let second = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Transfer-Encoding: gzip, chunked\r\n\r\n"
    );
    stream
        .write_all(
            &[
                second.into_bytes(),
                chunk_frame(chunk.as_bytes(), ""),
                b"0\r\n\r\n".to_vec(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the multi-token chunked second request");
    let served_second = read_response(&mut stream).await;
    assert_ne!(
        status(&served_second),
        431,
        "#1232 a keep-alive multi-token Transfer-Encoding request with a large chunk must be served, not 431: {served_second:?}"
    );
    let body = list_body();
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body].concat())
        .await
        .expect("#1232 write the follow-up request on the same connection");
    let third = read_response(&mut stream).await;
    assert_eq!(
        status(&third),
        200,
        "#1232 the connection must stay usable after a served multi-token Transfer-Encoding request: {third:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 fix-r1 (P1-1, review cell b): a keep-alive connection's second
/// request carries duplicate equal `Content-Length: 42000` values and a
/// 42 KB body. Node's llhttp rejects the duplicate framing with its 47-byte
/// 400, and hyper serves it (registered), so the 431 must never appear. The
/// head is split across two writes: the duplicate values land in the first,
/// and the rest of the head plus the body only after the first response was
/// read — the interleave where the old guard's mid-message re-arm
/// (`begin(Idle/NextHeaders)` seeing `!active`) reset its state and then
/// counted the arriving body bytes as a fresh head.
#[tokio::test]
async fn keep_alive_duplicate_content_length_large_body_is_served() {
    let (store, directory) = file_store("keepalive-dup-cl");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    let chunk = "aa: bb\r\n".repeat(5_250);
    let padding = "a".repeat(2_000);
    let head_prefix = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {n}\r\nContent-Length: {n}\r\nX-Pad: {half}",
        n = chunk.len(),
        half = &padding[..1_000]
    );
    stream
        .write_all(
            &[
                pipelined_list_head(body.len()).into_bytes(),
                body,
                head_prefix.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1232 write the first request and the second head's prefix");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 the first request must be served before the duplicate-length one: {first:?}"
    );
    // The rest of the head (still inside the X-Pad value) plus the whole body
    // land only now, after the first response: the re-arm used to fire here.
    let second = format!("{}\r\n\r\n", &padding[1_000..]);
    stream
        .write_all(&[second.into_bytes(), chunk.into_bytes()].concat())
        .await
        .expect("#1232 write the rest of the head and the 42 KB body");
    let served_second = read_response(&mut stream).await;
    assert_ne!(
        status(&served_second),
        431,
        "#1232 a keep-alive duplicate-Content-Length request with a large body must be served, not 431: {served_second:?}"
    );
    let body = list_body();
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body].concat())
        .await
        .expect("#1232 write the follow-up request on the same connection");
    let third = read_response(&mut stream).await;
    assert_eq!(
        status(&third),
        200,
        "#1232 the connection must stay usable after a served duplicate-Content-Length request: {third:?}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1277 (#1232 r2 P2-1, probe `te-multitoken-then-oversized-pipelined-under-cap`
/// / `-over-cap`): after request A steps the guard aside at its multi-token
/// `Transfer-Encoding: gzip, chunked`, a pipelined oversized head B that rides
/// the SAME single write. Node's parser scans the bytes the moment they land
/// and destroys the socket with only the 67-byte 431 — the probe recorded A's
/// list handler running (`serverInvocations.list` = 1), so only the bytes were
/// suppressed, and Node answers its own 67-byte 431 for every oversized head,
/// past hyper's buffer cap included (the over-cap cell re-probed the same 67
/// bytes behind a 200 KB head). Rust (measured, registered in the
/// `…-SHAPE-001-R5` diff entry): hyper writes A's served response before it
/// dispatches B, so the wire is A's 200 followed by Node's exact 67-byte 431 —
/// the step-aside family's single-burst priority difference, pinned here byte
/// for byte.
#[tokio::test]
async fn pipelined_oversized_after_step_aside_te_single_burst_writes_served_response_then_node_431()
{
    let (store, directory) = file_store("step-aside-single-burst");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1277 connect to the store under test");
    let body = list_body();
    stream
        .write_all(&[pipelined_list_head(body.len()).into_bytes(), body.clone()].concat())
        .await
        .expect("#1277 write the first request on the keep-alive connection");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1277 the first request must be served before the multi-token one: {first:?}"
    );
    // A's multi-token value steps the guard aside mid-head; A's own body is a
    // valid chunked list request, so a served A answers 200. B rides the same
    // write, exactly the single burst the probe used.
    let second = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Transfer-Encoding: gzip, chunked\r\n\r\n"
    );
    let oversized = pipelined_follow_up_head(DEFAULT_MAX_HEADER_BYTES, 0);
    stream
        .write_all(
            &[
                second.into_bytes(),
                chunk_frame(&body, ""),
                b"0\r\n\r\n".to_vec(),
                oversized.into_bytes(),
            ]
            .concat(),
        )
        .await
        .expect("#1277 write the multi-token request and the oversized head in one burst");
    let served = read_response(&mut stream).await;
    assert_eq!(
        status(&served),
        200,
        "#1277 the stepped-aside multi-token request is served before the oversized head's 431: {served:?}"
    );
    expect_terminal_bytes(
        &mut stream,
        NODE_431,
        "#1277 the single-burst oversized head behind a stepped-aside request ends the connection with the Node 431",
    )
    .await;
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 fix-r1 (P2-2, registered): after a step-aside the guard stays aside
/// for the connection's remainder, so a head hyper cannot finish parsing
/// inside its buffer cap is answered by hyper's own 431 shape, not the 67
/// Node bytes (`serve`'s `header_bytes` backstop never sees the request).
/// hyper's cap is `max_buf_size` (`hyper_buffer_bytes(16384) = 90,112`), but
/// the cap check runs only *between* reads while the adaptive read strategy
/// doubles its chunk 8/16/32/64 KiB (#1277: the derivation in the R5 entry
/// replaces this file's earlier "cap + one read chunk" wording — the buffer
/// can reach the cumulative 8+16+32+64 KiB = 122,880 bytes before the check
/// fires, not 90,112 + 64 KiB): a head up to 90,112 raw bytes always parses
/// and reaches the backstop with Node's bytes, one in (90,112, 122,880]
/// only when arrival granularity fills whole read chunks, and one past
/// 122,880 always errors at the cap with hyper's own shape. Node writes its
/// 67-byte 431 for every oversized head — the registered unapproved
/// difference this test pins.
///
/// #1277 (#1232 r2 P2): the 200 KB head is written while the response is
/// read concurrently (`into_split`) — the server answers and closes well
/// before the client finishes writing, and a client that only starts reading
/// afterwards can meet an RST that discards the 431 bytes still queued in
/// its receive buffer (an intermittent empty-response failure on kernels
/// that flush on reset; this host kept the bytes 20/20, the hardening removes
/// the ordering hazard rather than the observed flake).
#[tokio::test]
async fn oversized_head_over_hyper_buffer_after_step_aside_writes_hyper_431() {
    let (store, directory) = file_store("step-aside-hyper-431");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("#1232 connect to the store under test");
    let body = list_body();
    let duplicate_length = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {n}\r\nContent-Length: {n}\r\n\r\n",
        n = body.len()
    );
    stream
        .write_all(duplicate_length.as_bytes())
        .await
        .expect("#1232 write the duplicate-length request head");
    stream
        .write_all(&body)
        .await
        .expect("#1232 write the duplicate-length request body");
    let first = read_response(&mut stream).await;
    assert_eq!(
        status(&first),
        200,
        "#1232 hyper serves the duplicate-equal-Content-Length request: {first:?}"
    );
    // Raw size ≈ 200 KB: past the 122,880-byte cumulative bound of the
    // doubling reads, so hyper's partial parse at the cap errors before the
    // head can complete, whatever the arrival granularity.
    let oversized = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nX-Pad: {}\r\nContent-Length: 0\r\n\r\n",
        "a".repeat(200_000)
    );
    // The write and the read run concurrently: the server stops reading at
    // its buffer cap, answers, and closes with unread bytes still in its
    // receive queue, so the client may see the connection torn down while
    // this write is still in flight (its error is expected and ignored).
    let (mut reader, mut writer) = stream.into_split();
    let write = tokio::spawn(async move {
        let _ = writer.write_all(oversized.as_bytes()).await;
    });
    let mut response = Vec::new();
    loop {
        let mut buffer = [0u8; 4_096];
        let read = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buffer))
            .await
            .expect("#1232 the stepped-aside connection ends within 5 s");
        match read {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error) if response.is_empty() => panic!(
                "#1232 the hyper 431 bytes were lost before the first read ({error}); the concurrent reader must see them"
            ),
            Err(_) => break,
        }
    }
    let _ = write.await;
    assert!(
        response != NODE_431,
        "#1232 after a step-aside an oversized head beyond hyper's buffer cap is hyper's own registered 431 shape, not Node's bytes: {:?}",
        String::from_utf8_lossy(&response)
    );
    assert!(
        String::from_utf8_lossy(&response).starts_with("HTTP/1.1 431"),
        "#1232 the stepped-aside answer is still a 431: {:?}",
        String::from_utf8_lossy(&response)
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #1232 unit cell for the `Content-Length` body walk: the guard skips exactly
/// the framed body bytes, and the head after them is charged to a fresh
/// per-message budget.
#[test]
fn header_guard_walks_content_length_bodies_to_the_next_head() {
    let mut guard = HeaderGuard::new(64);
    // Counted: target `/x` (2) + `Host` + `h` (5) + `Content-Length` + `4`
    // (15) = 22, under the budget.
    assert!(
        !guard.push(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\n\r\n"),
        "#1232 the first head stays under the budget"
    );
    assert!(
        !guard.push(b"abcd"),
        "#1232 the four framed body bytes are walked, not counted"
    );
    assert!(
        !guard.push(b"POST /x HTTP/1.1\r\nHost: h\r\nX-Pad: "),
        "#1232 the head after the body is counted on a fresh budget"
    );
    assert!(
        !guard.push(&[b'a'; 30]),
        "#1232 30 more value bytes stay under the fresh budget (42 of 64)"
    );
    assert!(
        guard.push(&[b'a'; 30]),
        "#1232 the follow-up head trips its own budget (72 of 64), not the previous message's"
    );
}

/// #1232 unit cell for the chunked walk: chunk sizes, extensions, and the
/// terminal chunk are framing; trailer fields count toward the same budget as
/// the head (probe `chunked-oversized-trailer`).
#[test]
fn header_guard_walks_chunked_bodies_extensions_and_trailers() {
    let mut guard = HeaderGuard::new(64);
    // Counted: 2 (target) + 5 (`Host: h`) + 17 + 7 (`Transfer-Encoding:
    // chunked`) = 31, under the budget.
    assert!(
        !guard.push(b"POST /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n"),
        "#1232 the chunked head stays under the budget"
    );
    assert!(
        !guard.push(b"4;ext=1\r\nabcd\r\n0\r\n"),
        "#1232 chunk sizes and extensions are framing, not counted bytes"
    );
    assert!(
        !guard.push(b"X-Pad: "),
        "#1232 a trailer field name starts on the message's own budget"
    );
    assert!(
        guard.push(&[b'a'; 40]),
        "#1232 trailer value bytes are counted (31 + 5 + 40 over 64)"
    );
}

/// #1232 fix-r1 unit cell for the step-aside set: duplicate `Content-Length`
/// and multi-token `Transfer-Encoding` are framings where hyper and Node
/// disagree, so the guard must stop accounting — immediately, in the middle
/// of the read chunk it tripped in — and stay aside for the connection's
/// remainder. hyper greedily buffers past the current request, so its pull
/// point can already sit inside the next message while the timer still
/// reports `Idle`/`NextHeaders`: no phase-driven re-arm can be proven to sit
/// at a message boundary, and `serve`'s `header_bytes` backstop owns the
/// follow-up heads instead.
#[test]
fn header_guard_steps_aside_on_untrackable_framing() {
    let mut guard = HeaderGuard::new(64);
    assert!(
        !guard
            .push(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\n"),
        "#1232 a duplicate Content-Length does not itself trip the budget"
    );
    assert!(
        !guard.active,
        "#1232 a duplicate Content-Length must step the guard aside"
    );
    assert!(
        !guard.push(&[b'a'; 200]),
        "#1232 a stepped-aside guard answers nothing, even far past the budget"
    );
    // The step-aside covers the rest of the connection, not only the head it
    // tripped in: a later message (the bytes hyper is about to pull) is never
    // re-scanned as a head from the guard's stop offset.
    assert!(
        !guard.push(b"POST /x HTTP/1.1\r\nHost: h\r\nX-Pad: "),
        "#1232 a stepped-aside guard never re-arms, so nothing trips it again"
    );
    assert!(
        !guard.push(&[b'a'; 100]),
        "#1232 a stepped-aside guard never re-arms, so nothing trips it again"
    );
    assert!(
        !guard.active,
        "#1232 the guard stays stepped aside for the connection's remainder"
    );

    let mut guard = HeaderGuard::new(64);
    let mut message =
        b"POST /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: gzip, chunked\r\n\r\n".to_vec();
    // The chunked body rides in the same read chunk as the head that stepped
    // the guard aside: scanning on with a reset per-message state is exactly
    // how the probe's legal 42 KB chunk got its false 431.
    message.extend(b"aa: bb\r\n".repeat(20));
    assert!(
        !guard.push(&message),
        "#1232 the bytes behind an untrackable head in the same read chunk are not counted as head bytes"
    );
    assert!(
        !guard.active,
        "#1232 a multi-token Transfer-Encoding must step the guard aside"
    );
}

/// #1232 fix-r1 (opus-4, probe `empty-content-length`: 47-byte 400 + close):
/// an empty `Content-Length:` value is not `0`. Node's llhttp and hyper both
/// reject the value, so the guard steps aside instead of treating the next
/// byte as the start of a new head — the bytes behind it would be counted as
/// head bytes and a served request would get a false 431.
#[test]
fn header_guard_steps_aside_on_empty_content_length() {
    let mut guard = HeaderGuard::new(64);
    let mut message = b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length:\r\n\r\n".to_vec();
    message.extend(b"aa: bb\r\n".repeat(20));
    assert!(
        !guard.push(&message),
        "#1232 the bytes behind an empty Content-Length value are not counted as head bytes"
    );
    assert!(
        !guard.active,
        "#1232 an empty Content-Length value must step the guard aside"
    );
    // `Content-Length: 0` stays a tracked zero-length body: the next byte
    // starts the next message's head.
    let mut guard = HeaderGuard::new(64);
    assert!(
        !guard.push(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\n\r\n"),
        "#1232 a zero Content-Length does not itself trip the budget"
    );
    assert!(
        guard.active,
        "#1232 a zero Content-Length is a legal framing the guard keeps tracking"
    );
}

/// #1232 fix-r1 (P2-4): a zero-capacity `poll_read` while the read-ahead
/// buffer is at its cap must park, not treat the empty `filled` read as the
/// peer's EOF — the latched `ahead_eof` would later surface a fake 47-byte
/// 400 (or a truncated request) for bytes that were still in flight.
#[tokio::test]
async fn zero_room_poll_does_not_latch_a_fake_eof() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_secs(3_600),
        Duration::from_secs(3_600),
        Arc::new(AtomicBool::new(false)),
    ));
    let (mut client, server_io) = tokio::io::duplex(READ_AHEAD_BYTES * 2);
    let mut stream = TimeoutStream::new(server_io, timer, DEFAULT_MAX_HEADER_BYTES);
    // A `Content-Length`-framed body larger than the read-ahead cap: the guard
    // walks it as body bytes, so `ahead` fills to the cap without tripping.
    client
        .write_all(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 9999999\r\n\r\n")
        .await
        .expect("#1232 write the framed head");
    client
        .write_all(&vec![b'x'; READ_AHEAD_BYTES + 512])
        .await
        .expect("#1232 write more than the read-ahead cap of body bytes");
    let mut room = [0u8; 1];
    stream
        .read_exact(&mut room)
        .await
        .expect("#1232 the first byte must be delivered");
    assert_eq!(
        room[0], b'P',
        "#1232 the delivered byte is the head's first"
    );
    // The caller has no room and `ahead` is full: the poll must park without
    // latching an EOF off the empty read.
    let mut nothing: [u8; 0] = [];
    let mut read_buf = ReadBuf::new(&mut nothing);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let poll = std::pin::Pin::new(&mut stream).poll_read(&mut context, &mut read_buf);
    assert!(
        matches!(poll, std::task::Poll::Pending),
        "#1232 a zero-room poll with a full read-ahead buffer parks, it does not answer"
    );
    assert!(
        !stream.ahead_eof,
        "#1232 a zero-capacity read is not the peer's EOF: the waiting read-ahead bytes must stay delivered, not fake a 400"
    );
}

/// #1277 (#1232 r2 P2, the wake half of the P2-4 park): a parked zero-room
/// read has no room to deliver into, so it never polls the inner socket —
/// the wake sources that exist are the timer's in-band events (any write or
/// lifecycle transition calls `KeepAliveTimer::wake`). hyper itself never
/// polls with a zero-capacity buffer (`poll_read_from_io` always reserves an
/// adaptive chunk), so this is the residual contract for any caller that
/// does: a real waker records the park without a self-wake (which would spin
/// the executor), the next write activity must wake it, and the poll after
/// the wake — once the caller has room again — delivers the waiting bytes.
#[tokio::test]
async fn zero_room_park_is_woken_by_connection_activity() {
    struct RecordingWake {
        woke: Arc<AtomicBool>,
    }
    impl std::task::Wake for RecordingWake {
        fn wake(self: Arc<Self>) {
            self.woke.store(true, Ordering::SeqCst);
        }
    }
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_secs(3_600),
        Duration::from_secs(3_600),
        Arc::new(AtomicBool::new(false)),
    ));
    let (mut client, server_io) = tokio::io::duplex(READ_AHEAD_BYTES * 2);
    let mut stream = TimeoutStream::new(server_io, Arc::clone(&timer), DEFAULT_MAX_HEADER_BYTES);
    // The same fill as the P2-4 test: a framed body larger than the
    // read-ahead cap, walked as body bytes, so `ahead` sits at its cap.
    client
        .write_all(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 9999999\r\n\r\n")
        .await
        .expect("#1277 write the framed head");
    client
        .write_all(&vec![b'x'; READ_AHEAD_BYTES + 512])
        .await
        .expect("#1277 write more than the read-ahead cap of body bytes");
    let mut first = [0u8; 1];
    stream
        .read_exact(&mut first)
        .await
        .expect("#1277 the first byte must be delivered");
    assert_eq!(
        first[0], b'P',
        "#1277 the delivered byte is the head's first"
    );
    let woke = Arc::new(AtomicBool::new(false));
    let waker = std::task::Waker::from(Arc::new(RecordingWake {
        woke: Arc::clone(&woke),
    }));
    let mut context = std::task::Context::from_waker(&waker);
    let mut nothing: [u8; 0] = [];
    let mut read_buf = ReadBuf::new(&mut nothing);
    let poll = std::pin::Pin::new(&mut stream).poll_read(&mut context, &mut read_buf);
    assert!(
        matches!(poll, std::task::Poll::Pending),
        "#1277 a zero-room poll with a full read-ahead buffer parks, it does not answer"
    );
    assert!(
        !stream.ahead_eof,
        "#1277 the parked zero-capacity read does not latch a fake EOF"
    );
    assert!(
        !woke.load(Ordering::SeqCst),
        "#1277 parking the zero-room read must not self-wake (that would spin the executor)"
    );
    // The next in-band event on the connection — here hyper flushing part of
    // the in-flight response — must wake the parked read.
    timer.write_activity();
    assert!(
        woke.load(Ordering::SeqCst),
        "#1277 the connection's next write activity must wake the parked zero-room read"
    );
    let mut room = [0u8; 4];
    let mut read_buf = ReadBuf::new(&mut room);
    let poll = std::pin::Pin::new(&mut stream).poll_read(&mut context, &mut read_buf);
    assert!(
        matches!(poll, std::task::Poll::Ready(Ok(()))),
        "#1277 a woken zero-room poll, once the caller has room again, must deliver instead of staying parked"
    );
    assert_eq!(
        read_buf.filled(),
        b"OST ",
        "#1277 the delivered bytes are the head's next ones"
    );
}

#[tokio::test]
async fn connection_cap_answers_overflow_with_503_and_recovers() {
    let (store, directory) = file_store("connections");
    let limits = ServeLimits {
        max_connections: 1,
        ..ServeLimits::default()
    };
    let (server, port) = start_limited(store, limits).await;
    let mut first = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    first.write_all(&http_get(false)).await.unwrap();
    let first_response = read_response(&mut first).await;
    assert!(
        first_response.starts_with(b"HTTP/1.1 405"),
        "{}",
        String::from_utf8_lossy(&first_response)
    );

    let started = Instant::now();
    let overflow = exchange_within(port, &http_get(true), Duration::from_secs(1))
        .await
        .expect("a full server must answer immediately, not block accept");
    assert_eq!(status(&overflow), 503, "overflow must be an immediate 503");
    assert!(started.elapsed() < Duration::from_secs(1));

    drop(first);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let response = exchange_within(port, &http_get(true), Duration::from_secs(1))
            .await
            .expect("released permit did not wake the server");
        if status(&response) == 405 {
            break;
        }
        assert_eq!(status(&response), 503);
        assert!(
            Instant::now() < deadline,
            "the released permit was never reused"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// 64 half-open requests (no terminating blank line) hold every connection
/// permit. A legitimate request must still get an immediate answer; before
/// this fix the accept loop waited on the semaphore and the request queued for
/// `headersTimeout` (~60 s). A second assertion proves the server recovers as
/// soon as one attacker connection closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_connections_do_not_block_the_accept_loop() {
    let (store, directory) = file_store("overflow");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut held = Vec::new();
    for _ in 0..DEFAULT_MAX_CONNECTIONS {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"POST /v1/gateway-user-dialog-store/list HTTP/1.1\r\nHost: h\r\n")
            .await
            .unwrap();
        held.push(stream);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let overflow = loop {
        let started = Instant::now();
        let response = exchange_within(port, &http_put(&put_body()), Duration::from_secs(1))
            .await
            .expect("accept loop stopped answering while the connection cap was full");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an overflow request waited for a permit"
        );
        match status(&response) {
            503 => break response,
            200 => {
                assert!(
                    Instant::now() < deadline,
                    "the connection cap was never reached"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            other => panic!("unexpected status {other}"),
        }
    };
    assert_eq!(status(&overflow), 503);

    held.pop();
    let mut served = false;
    while Instant::now() < deadline + Duration::from_secs(1) {
        let response = exchange_within(port, &http_put(&put_body()), Duration::from_secs(1))
            .await
            .expect("server stopped answering after a permit was released");
        match status(&response) {
            200 => {
                served = true;
                break;
            }
            503 => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(served, "a released permit never served a request again");
    drop(held);
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn request_body_limit_rejects_oversized_content_length_before_reading() {
    let (store, directory) = file_store("cl-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    for (name, authorization) in [("authenticated", Some(TOKEN)), ("anonymous", None)] {
        let request = match authorization {
            Some(token) => format!(
                "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\nAuthorization: Bearer {token}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                2 * DEFAULT_MAX_BODY_BYTES
            ),
            None => format!(
                "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                2 * DEFAULT_MAX_BODY_BYTES
            ),
        };
        // No body is ever written: the status must arrive from the header alone.
        let response = exchange(port, request.as_bytes()).await.unwrap();
        let expected = if authorization.is_some() { 413 } else { 401 };
        assert_eq!(status(&response), expected, "{name}: early status");
    }
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn request_body_limit_rejects_oversized_chunked_bodies() {
    let (store, directory) = file_store("chunked-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let size = DEFAULT_MAX_BODY_BYTES + 1;
    let mut request = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: h\r\nAuthorization: Bearer {TOKEN}\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{size:x}\r\n"
    )
    .into_bytes();
    request.extend(std::iter::repeat_n(b'a', size));
    request.extend_from_slice(b"\r\n0\r\n\r\n");
    let response = exchange_lenient(port, &request).await;
    assert_eq!(
        status(&response),
        413,
        "chunked body over the cap must be 413"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn request_body_at_the_limit_is_accepted() {
    let (store, directory) = file_store("exact-limit");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let mut body = put_body();
    assert!(body.len() < DEFAULT_MAX_BODY_BYTES);
    body.resize(DEFAULT_MAX_BODY_BYTES, b' ');
    let response = exchange(port, &http_post(&body)).await.unwrap();
    assert_eq!(
        status(&response),
        200,
        "a body of exactly the limit must be accepted"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// The production transport decodes responses through `read_body` with Node's
/// `MAX_STRING_LENGTH`; a test-only small bound reaches the same rejection.
#[tokio::test]
async fn client_maps_oversized_response_bodies_to_invalid_response() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback listen failed; rerun outside the sandbox");
    let address = listener.local_addr().unwrap();
    let reply = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Drain the request first: closing with unread bytes would send an RST
        // and the client would report `fetch failed` instead of the body error.
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        let mut body = br#"{"version":1,"result":[]}"#.to_vec();
        body.resize(100, b' ');
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        let _ = stream.shutdown().await;
    });
    let transport: Arc<dyn HttpStoreTransport> =
        Arc::new(HyperHttpStoreTransport::with_max_body_bytes(64));
    let client = HttpGatewayUserDialogStore::with_transport(
        options(&format!("http://{address}/"), TOKEN, None),
        transport,
    )
    .unwrap();
    let error = client.list(&test_key()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Gateway user-dialog store list returned an invalid response."
    );
    reply.await.unwrap();
}

/// A raw server that sends a complete response head and then fails the body in
/// the requested way. The TS client swallows every body failure
/// (`response.json().catch(() => undefined)`) and decides from the status; the
/// Rust transport must produce the same observable outcome.
async fn body_failure_server(
    status: u16,
    failure: &str,
    limit: usize,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback listen failed; rerun outside the sandbox");
    let port = listener.local_addr().unwrap().port();
    let failure = failure.to_string();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
        }
        let declared = if failure == "oversized" {
            limit + 1
        } else {
            100
        };
        let reason = if status == 200 {
            "OK"
        } else {
            "Internal Server Error"
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {declared}\r\nConnection: close\r\n\r\n"
        );
        if stream.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let partial = br#"{"version":1"#;
        match failure.as_str() {
            "disconnect" => {
                let _ = stream.write_all(partial).await;
                tokio::time::sleep(Duration::from_millis(30)).await;
                let _ = stream.shutdown().await;
            }
            "stall" => {
                let _ = stream.write_all(partial).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            "oversized" => {
                let chunk = [b' '; 128];
                let mut sent = 0;
                while sent < declared {
                    let take = (declared - sent).min(chunk.len());
                    if stream.write_all(&chunk[..take]).await.is_err() {
                        break;
                    }
                    sent += take;
                }
            }
            other => panic!("unknown body failure {other}"),
        }
    });
    (port, task)
}

/// The six real-TS body-failure recordings: {200, 500} x {disconnect, stall,
/// oversized}. TS answers 2xx with "returned an invalid response." and non-2xx
/// with `failed: HTTP n`, because the body error only makes the body undefined.
#[tokio::test]
async fn client_body_failures_follow_typescript_status_order() {
    let oracle = oracle();
    let cases = oracle["clientBodyFailures"]
        .as_array()
        .expect("clientBodyFailures fixture");
    assert_eq!(cases.len(), 6, "all six body-failure recordings replay");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let status = case["status"].as_u64().unwrap() as u16;
        let failure = case["failure"].as_str().unwrap();
        let limit = 64;
        let (port, task) = body_failure_server(status, failure, limit).await;
        let transport: Arc<dyn HttpStoreTransport> = if failure == "oversized" {
            Arc::new(HyperHttpStoreTransport::with_max_body_bytes(limit))
        } else {
            Arc::new(HyperHttpStoreTransport::default())
        };
        let client = HttpGatewayUserDialogStore::with_transport(
            options(
                &format!("http://127.0.0.1:{port}/"),
                TOKEN,
                case["timeoutMs"].as_f64(),
            ),
            transport,
        )
        .unwrap();
        let error = client
            .clear(&test_key())
            .await
            .expect_err("a failed body must never yield a value");
        assert_eq!(
            error.to_string(),
            case["outcome"]["message"].as_str().unwrap(),
            "{name}: TS-compatible status-first message"
        );
        task.abort();
        let _ = task.await;
    }
}

/// Registered difference CLIENT-BODY-UNITS-001: the Rust bound counts UTF-8
/// bytes while TS `Response.text()` is bounded by UTF-16 code units. This body
/// is over 64 bytes but under 64 code units, so Rust rejects it as an invalid
/// response where TS would parse it. The test pins the documented direction.
#[tokio::test]
async fn client_body_limit_counts_utf8_bytes_not_utf16_units() {
    let text = format!(r#"{{"version":1,"result":null,"x":"{}"}}"#, "é".repeat(20));
    assert!(text.len() > 64, "the fixture exceeds the byte bound");
    assert!(
        text.chars().count() < 64,
        "the fixture stays under the 64 code-unit bound"
    );
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback listen failed; rerun outside the sandbox");
    let port = listener.local_addr().unwrap().port();
    let reply = text.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            reply.len()
        );
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(reply.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    let transport: Arc<dyn HttpStoreTransport> =
        Arc::new(HyperHttpStoreTransport::with_max_body_bytes(64));
    let client = HttpGatewayUserDialogStore::with_transport(
        options(&format!("http://127.0.0.1:{port}/"), TOKEN, None),
        transport,
    )
    .unwrap();
    let error = client
        .clear(&test_key())
        .await
        .expect_err("the byte bound must reject a body TS would accept");
    assert_eq!(
        error.to_string(),
        "Gateway user-dialog store clear returned an invalid response."
    );
}

/// `LiveGatewayUserDialogStore` is the same TS interface with every optional
/// method required, so a live store is directly usable by the channel.
#[test]
fn live_store_upcasts_to_the_channel_store() {
    let (live, directory) = file_store("upcast");
    let channel_store: Arc<dyn UserDialogStore> = live;
    let key = test_key();
    channel_store.put(&key, &test_dialog()).unwrap();
    channel_store.remove(&key, &"r1".into()).unwrap();
    assert!(channel_store.remove(&key, &"missing".into()).is_ok());
    let _ = std::fs::remove_dir_all(directory);
}

fn list_body() -> Vec<u8> {
    concat!(
        r#"{"version":1,"key":{"projectRoot":"/workspace/project","#,
        r#""pilotHome":"/workspace/home","sessionId":"sdk:http-dialog-store"}}"#
    )
    .as_bytes()
    .to_vec()
}

/// `max_buf_size` also caps hyper's write buffer; a list response larger than
/// `hyper_buffer_bytes` (4 x 16 KiB + 3 x 8 KiB with defaults) must still
/// arrive whole.
#[tokio::test]
async fn large_list_responses_are_not_truncated_by_the_header_buffer_cap() {
    let (store, directory) = file_store("large-response");
    let prompt = "a".repeat(128 * 1024);
    let request = parse_json(&format!(
        r#"{{"type":"user_dialog_request","requestId":"r1","dialogKind":"form","toolCallId":"t1","toolName":"request_user_form","prompt":"{prompt}"}}"#
    ))
    .unwrap();
    store
        .put(
            &test_key(),
            &StoredDialog {
                request,
                created_at: "2026-01-01T00:00:00.000Z".into(),
            },
        )
        .unwrap();
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let response = exchange(port, &http_post(&list_body())).await.unwrap();
    assert_eq!(status(&response), 200);
    let body = String::from_utf8_lossy(&response);
    let payload = body.split("\r\n\r\n").nth(1).unwrap_or_default();
    assert!(
        payload.len() > 128 * 1024,
        "large list response was truncated at {} bytes",
        payload.len()
    );
    assert!(payload.contains(&prompt), "large response body is complete");
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

struct BlockingStore {
    inner: FileGatewayUserDialogStore,
    entered: Mutex<usize>,
    released: AtomicBool,
    condvar: Condvar,
    entered_tx: tokio::sync::mpsc::UnboundedSender<()>,
}
impl UserDialogStore for BlockingStore {
    fn put(&self, key: &StoreKey, dialog: &StoredDialog) -> Result<()> {
        let mut entered = self.entered.lock().unwrap();
        *entered += 1;
        let _ = self.entered_tx.send(());
        while !self.released.load(Ordering::SeqCst) {
            entered = self.condvar.wait(entered).unwrap();
        }
        drop(entered);
        self.inner.put(key, dialog)
    }
    fn remove(&self, key: &StoreKey, request_id: &JsString) -> Result<()> {
        self.inner.remove(key, request_id)
    }
}
impl LiveGatewayUserDialogStore for BlockingStore {
    fn list(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        self.inner.list(key)
    }
    fn clear(&self, key: &StoreKey) -> Result<()> {
        self.inner.clear(key)
    }
    fn list_live(&self, key: &StoreKey) -> Result<Vec<StoredDialog>> {
        self.inner.list_live(key)
    }
    fn claim_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        ttl_ms: f64,
        lease_id: Option<&JsString>,
    ) -> Result<LeaseClaim> {
        self.inner.claim_live(key, request_id, ttl_ms, lease_id)
    }
    fn release_live(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: &JsString,
    ) -> Result<bool> {
        self.inner.release_live(key, request_id, lease_id)
    }
    fn submit_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        lease_id: Option<&JsString>,
        result: &DialogAnswer,
    ) -> Result<bool> {
        self.inner
            .submit_live_answer(key, request_id, lease_id, result)
    }
    fn take_live_answer(
        &self,
        key: &StoreKey,
        request_id: &JsString,
    ) -> Result<Option<StoredAnswer>> {
        self.inner.take_live_answer(key, request_id)
    }
    fn claim_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<OwnerClaim> {
        self.inner
            .claim_live_owner(key, request_id, owner_id, ttl_ms)
    }
    fn renew_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
        ttl_ms: f64,
    ) -> Result<bool> {
        self.inner
            .renew_live_owner(key, request_id, owner_id, ttl_ms)
    }
    fn release_live_owner(
        &self,
        key: &StoreKey,
        request_id: &JsString,
        owner_id: &JsString,
    ) -> Result<bool> {
        self.inner.release_live_owner(key, request_id, owner_id)
    }
}

/// Two handlers blocked inside `FileGatewayUserDialogStore::put` must not stop
/// the accept loop. A current-thread runtime makes the old direct call
/// deterministic: the first blocking `put` freezes every task.
#[tokio::test(flavor = "current_thread")]
async fn concurrent_blocking_puts_do_not_stall_the_accept_loop() {
    let directory = temp_dir("blocking-puts");
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(BlockingStore {
        inner: FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap(),
        entered: Mutex::new(0),
        released: AtomicBool::new(false),
        condvar: Condvar::new(),
        entered_tx,
    });
    let (server, port) = start_limited(
        store.clone() as Arc<dyn LiveGatewayUserDialogStore>,
        ServeLimits::default(),
    )
    .await;
    let body = put_body();
    let first = tokio::spawn({
        let request = http_put(&body);
        async move { exchange(port, &request).await }
    });
    let second = tokio::spawn({
        let request = http_put(&body);
        async move { exchange(port, &request).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        entered_rx.recv().await.expect("first put entered");
        entered_rx.recv().await.expect("second put entered");
    })
    .await
    .expect("two concurrent puts did not both reach the store");

    let probe = tokio::time::timeout(Duration::from_secs(1), exchange(port, &http_get(true)))
        .await
        .expect("accept loop stopped while store.put blocked")
        .unwrap();
    assert_eq!(
        status(&probe),
        405,
        "accept loop still answers new connections"
    );

    store.released.store(true, Ordering::SeqCst);
    store.condvar.notify_all();
    assert_eq!(status(&first.await.unwrap().unwrap()), 200);
    assert_eq!(status(&second.await.unwrap().unwrap()), 200);
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// Node `server.close()` closes idle connections but lets an in-flight request
/// finish, and its callback runs only after every connection has ended. The
/// old implementation returned as soon as the accept loop stopped while the
/// request task kept running (and could still write to the store).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_waits_for_in_flight_requests() {
    let directory = temp_dir("close-in-flight");
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(BlockingStore {
        inner: FileGatewayUserDialogStore::new(StoreOptions::new(directory.clone())).unwrap(),
        entered: Mutex::new(0),
        released: AtomicBool::new(false),
        condvar: Condvar::new(),
        entered_tx,
    });
    let (server, port) = start_limited(
        store.clone() as Arc<dyn LiveGatewayUserDialogStore>,
        ServeLimits::default(),
    )
    .await;
    let request = http_put(&put_body());
    let put = tokio::spawn(async move { exchange(port, &request).await });
    tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
        .await
        .expect("the put never reached the store");

    let finished = Arc::new(AtomicBool::new(false));
    let flag = finished.clone();
    let closing = tokio::spawn(async move {
        server.close().await;
        flag.store(true, Ordering::SeqCst);
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !finished.load(Ordering::SeqCst),
        "close() returned while the in-flight request was still running"
    );

    store.released.store(true, Ordering::SeqCst);
    store.condvar.notify_all();
    let response = tokio::time::timeout(Duration::from_secs(3), put)
        .await
        .expect("the in-flight request was abandoned by close()")
        .unwrap()
        .unwrap();
    assert_eq!(
        status(&response),
        200,
        "an in-flight request must complete before close() returns"
    );
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .expect("close() hung after the in-flight request finished")
        .unwrap();
    assert!(finished.load(Ordering::SeqCst));
    let _ = std::fs::remove_dir_all(directory);
}

// ---------------------------------------------------------------------------
// #702: shutdown vs keep-alive advertisement, and early-close RST
// ---------------------------------------------------------------------------

/// Read the response head (through the blank line) plus any body bytes that
/// arrived with it.
async fn read_response_head(stream: &mut TcpStream, note: &str) -> (Vec<u8>, Vec<u8>) {
    let deadline = response_phase_deadline();
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            return (
                buffer[..position + 4].to_vec(),
                buffer[position + 4..].to_vec(),
            );
        }
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => panic!("{note}: response head read failed: {error}"),
            Err(_) => panic!(
                "{note}: response head never arrived within {RESPONSE_PHASE_BUDGET:?} \
                 (read {} bytes so far)",
                buffer.len()
            ),
        };
        assert!(
            read > 0,
            "{note}: EOF before the response head: {:?}",
            String::from_utf8_lossy(&buffer)
        );
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn response_content_length(head: &[u8]) -> usize {
    let head = String::from_utf8_lossy(head);
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or_else(|| panic!("response has no content-length: {head}"))
}

/// Read exactly `length` body bytes, carrying over any bytes read with the head.
async fn read_exact_body(
    stream: &mut TcpStream,
    mut body: Vec<u8>,
    length: usize,
    note: &str,
) -> Vec<u8> {
    let deadline = response_phase_deadline();
    let mut chunk = [0u8; 8192];
    while body.len() < length {
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => panic!("{note}: response body read failed: {error}"),
            Err(_) => panic!(
                "{note}: response body stalled at {}/{length} within {RESPONSE_PHASE_BUDGET:?}",
                body.len()
            ),
        };
        assert!(
            read > 0,
            "{note}: EOF before the response body completed: {}/{length}",
            body.len()
        );
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    body
}

/// #702 P1: once a response head advertising `Connection: keep-alive` has
/// reached the client, `server.close()` while the large body is still flushing
/// must not turn that advertisement into a lie. Node keeps the in-flight
/// connection and serves a follow-up request on it
/// (`lifecycle-matrix/node-lifecycle-matrix.json`,
/// `s3_body_flushing_close_reuses_same_socket`).
///
/// The 40 iterations are the measured trigger rate: before the fix every
/// iteration ended in EOF after the body, so the follow-up request was lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_large_response_reuses_advertised_keep_alive() {
    let (store, directory) = file_store("shutdown-large-keep-alive");
    // Twelve ~1.5 MiB dialogs make the list response ~18 MiB, larger than the
    // loopback socket buffers observed on macOS, so hyper still has body
    // frames queued when the client reads the head. A 2-6 MiB response can
    // otherwise be buffered completely before `close()`, which Node also
    // treats as an idle connection and destroys (ad-hoc probe output:
    // `_tmp/20261026-issue-702/node/node-fast-buffered.out.txt`).
    let prompt = "a".repeat(1500 * 1024);
    for index in 0..12 {
        let request = parse_json(&format!(
            r#"{{"type":"user_dialog_request","requestId":"r{index}","dialogKind":"form","toolCallId":"t{index}","toolName":"request_user_form","prompt":"{prompt}"}}"#
        ))
        .unwrap();
        store
            .put(
                &test_key(),
                &StoredDialog {
                    request,
                    created_at: "2026-01-01T00:00:00.000Z".into(),
                },
            )
            .unwrap();
    }
    let limits = ServeLimits {
        keep_alive_timeout: Duration::from_secs(2),
        keep_alive_timeout_buffer: Duration::ZERO,
        ..ServeLimits::default()
    };
    let mut observed = Vec::new();
    for iteration in 0..40 {
        let note = format!(
            "shutdown_during_large_response_reuses_advertised_keep_alive iteration {iteration}"
        );
        let (server, port) = start_limited(store.clone(), limits).await;
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = http_request(
            "POST",
            &format!("{STORE_PATH_PREFIX}list"),
            &[
                ("Authorization", &format!("Bearer {TOKEN}")),
                ("Content-Type", "application/json"),
            ],
            &list_body(),
        );
        stream.write_all(&request).await.unwrap();
        let (head, leftover) = read_response_head(&mut stream, &note).await;
        let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
        assert!(
            head_text.contains("connection: keep-alive"),
            "{note}: the persistent response must advertise keep-alive: {head_text}"
        );
        let length = response_content_length(&head);
        assert!(
            length > 12 * 1024 * 1024,
            "{note}: the response body must be large enough to still be flushing: {length}"
        );
        let close_task = tokio::spawn(async move { server.close().await });
        // close() is observed while the body is still in the write path; the
        // client deliberately has not read a body byte yet.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let body = read_exact_body(&mut stream, leftover, length, &note).await;
        assert_eq!(body.len(), length, "{note}: complete body");
        // The advertised connection must still accept the follow-up request.
        stream
            .write_all(&http_get(false))
            .await
            .unwrap_or_else(|error| panic!("{note}: follow-up write failed: {error}"));
        let follow_note = format!("{note}: follow-up request");
        let (follow_head, follow_leftover) = read_response_head(&mut stream, &follow_note).await;
        assert_eq!(
            status(&follow_head),
            405,
            "{note}: the advertised keep-alive connection was closed before a follow-up request"
        );
        let follow_text = String::from_utf8_lossy(&follow_head).to_ascii_lowercase();
        assert!(
            follow_text.contains("connection: keep-alive"),
            "{note}: Node keeps advertising keep-alive on the in-flight socket: {follow_text}"
        );
        // Node's probe reaches the deadline with the connection still open:
        // close() waits for the keep-alive idle deadline (2 s here), then the
        // socket ends. Consume the 405 body first so the idle timer is armed
        // by the response finish, not by unread bytes.
        let follow_body = read_exact_body(
            &mut stream,
            follow_leftover,
            response_content_length(&follow_head),
            &format!("{note}: follow-up body"),
        )
        .await;
        assert!(!follow_body.is_empty(), "{note}: follow-up body missing");
        // Node's probe ends with the connection still open at the client
        // deadline; this 40x case only asserts the advertisement survives the
        // close. The idle-close timing is covered once by the S1 test.
        drop(stream);
        tokio::time::timeout(Duration::from_secs(5), close_task)
            .await
            .unwrap_or_else(|_| panic!("{note}: close() hung"))
            .unwrap();
        observed.push(length);
    }
    assert_eq!(observed.len(), 40, "40 iteration trigger-rate sample");
    let _ = std::fs::remove_dir_all(directory);
}

/// #702 P2: a rejection written before the request body is read must survive a
/// peer that dribbles that body. hyper's `poll_drain_or_close_read` gives up
/// after one poll and closes the read half; closing the socket with unread
/// bytes sends an RST that can discard the already-written 413. The fix drains
/// the unread body for a bounded window after the response, like
/// `reject_over_capacity`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_close_response_survives_byte_at_a_time_sender() {
    let (store, directory) = file_store("early-close-slow-sender");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let declared = 3 * 1024 * 1024;
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();
    let head = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
         Content-Length: {declared}\r\n\r\n"
    );
    writer.write_all(head.as_bytes()).await.unwrap();
    // Leave a backlog of unread body in the server's receive queue, then keep
    // dribbling. The client deliberately does not read the response until the
    // server has already closed: an RST at that point discards the queued 413.
    writer.write_all(&[b'x'; 512 * 1024]).await.unwrap();
    let writer_task = tokio::spawn(async move {
        let chunk = [b'x'; 1024];
        loop {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    let reader_task = tokio::spawn(async move {
        let mut response = Vec::new();
        let result =
            tokio::time::timeout(Duration::from_secs(8), reader.read_to_end(&mut response)).await;
        (result, response)
    });
    let (result, response) = reader_task.await.unwrap();
    writer_task.abort();
    let result = result.unwrap_or_else(|_| {
        panic!(
            "early_close_response_survives_byte_at_a_time_sender: response never completed; \
             got {} bytes: {:?}",
            response.len(),
            String::from_utf8_lossy(&response)
        )
    });
    result.unwrap_or_else(|error| {
        panic!(
            "early_close_response_survives_byte_at_a_time_sender: the early close response was \
             reset before delivery ({error}); got {} bytes: {:?}",
            response.len(),
            String::from_utf8_lossy(&response)
        )
    });
    assert_eq!(
        status(&response),
        413,
        "the early rejection must survive a slow sender: {:?}",
        String::from_utf8_lossy(&response)
    );
    let text = String::from_utf8_lossy(&response).to_ascii_lowercase();
    assert!(
        text.contains("connection: close") && !text.contains("keep-alive:"),
        "the early rejection must be a coherent close: {text}"
    );
    assert!(
        text.contains("payload_too_large"),
        "the early rejection body must be delivered: {text}"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
}

/// #702 S4×E1: `server.close()` while an early `Connection: close` response
/// has been written but the request body is still unread. Node keeps draining
/// the body and the already-written response survives
/// (`lifecycle-matrix/node-lifecycle-matrix.json`, `s4_early_401_close`). Rust
/// must answer the 401 completely and then end the connection; the bounded
/// drain covers the same unread-body close path as the slow-sender test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_close_response_survives_shutdown_with_unread_body() {
    let (store, directory) = file_store("early-close-shutdown");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let declared = 3 * 1024 * 1024;
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();
    let head = format!(
        "POST {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\n\
         Content-Type: application/json\r\nContent-Length: {declared}\r\n\r\n"
    );
    writer.write_all(head.as_bytes()).await.unwrap();
    writer.write_all(&[b'x'; 64 * 1024]).await.unwrap();
    // Let the unauthenticated precheck write its 401 before close() lands.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let close_task = tokio::spawn(async move { server.close().await });
    let writer_task = tokio::spawn(async move {
        let chunk = [b'x'; 1024];
        loop {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let mut response = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(8), reader.read_to_end(&mut response))
        .await
        .expect("early_close_response_survives_shutdown_with_unread_body: response never ended");
    writer_task.abort();
    read.unwrap_or_else(|error| {
        panic!(
            "early_close_response_survives_shutdown_with_unread_body: response was reset ({error}); \
             got {} bytes: {:?}",
            response.len(),
            String::from_utf8_lossy(&response)
        )
    });
    assert_eq!(
        status(&response),
        401,
        "the early 401 must survive close(): {:?}",
        String::from_utf8_lossy(&response)
    );
    let text = String::from_utf8_lossy(&response).to_ascii_lowercase();
    assert!(
        text.contains("connection: close") && !text.contains("keep-alive:"),
        "the early 401 must be a coherent close: {text}"
    );
    assert!(
        text.contains("unauthorized"),
        "the early 401 body must be delivered: {text}"
    );
    tokio::time::timeout(Duration::from_secs(5), close_task)
        .await
        .expect("early_close_response_survives_shutdown_with_unread_body: close() hung")
        .unwrap();
    let _ = std::fs::remove_dir_all(directory);
}
