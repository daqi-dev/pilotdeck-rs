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

async fn exchange(port: u16, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(request).await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "response"))??;
    Ok(response)
}

/// The server may legitimately close while a client is still writing an
/// oversized body; only the response matters.
async fn exchange_lenient(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let _ = stream.write_all(request).await;
    let mut response = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
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
/// keep-alive stream is left idle, not with unread response bytes.
async fn read_response(stream: &mut TcpStream) -> Vec<u8> {
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await.expect("response head");
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
    stream.read_exact(&mut body).await.expect("response body");
    response.extend_from_slice(&body);
    response
}

/// Reads only the response head, leaving any body bytes unread. HEAD
/// responses declare `Content-Length` but never send the body.
async fn read_head<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> String {
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await.expect("response head");
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
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "the listener still accepts after close()"
    );
    let _ = std::fs::remove_dir_all(directory);
}

/// Node's observable idle keep-alive deadline is `keepAliveTimeout` (5 s) plus
/// `server.keepAliveTimeoutBuffer` (1 s), not the bare 5 s property and not the
/// 60 s `headersTimeout`. A generated Node v22.22.0 server closes at 6,003 ms
/// (probe record in `_tmp/20261025-issue-679/probe`). The timer is shortened
/// here; the default constants are pinned separately.
#[tokio::test]
async fn idle_keep_alive_connections_close_at_keep_alive_timeout() {
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

/// `server.close()` during an in-flight request used to produce
/// `connection: close` (hyper's `graceful_shutdown`) next to our stale
/// `Keep-Alive: timeout=5` (`.../probe/rust-shutdown.log`). The response must
/// be one coherent close.
#[tokio::test]
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
    let (server, port) = start_limited(
        store.clone() as Arc<dyn LiveGatewayUserDialogStore>,
        ServeLimits::default(),
    )
    .await;
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
    let close_task = tokio::spawn(async move { server.close().await });
    // The connection future is parked on `spawn_blocking`, so the shutdown
    // branch is polled while the request is still in flight. This is an
    // ordering yield, not a deadline assertion.
    tokio::time::sleep(Duration::from_millis(100)).await;
    store.released.store(true, Ordering::SeqCst);
    store.condvar.notify_all();

    let response = tokio::time::timeout(Duration::from_secs(5), read_response(&mut stream))
        .await
        .expect("the in-flight response never arrived");
    assert_eq!(status(&response), 200, "{response:?}");
    let head = String::from_utf8_lossy(&response).to_ascii_lowercase();
    assert!(
        head.contains("connection: close"),
        "hyper disables keep-alive on shutdown, so the response must say close: {head}"
    );
    assert!(
        !head.contains("keep-alive:"),
        "a close response must not keep the stale Keep-Alive advertisement: {head}"
    );
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
        .await
        .expect("the shutdown connection stayed open");
    assert_eq!(
        read.unwrap(),
        0,
        "expected EOF after the in-flight response"
    );
    tokio::time::timeout(Duration::from_secs(5), close_task)
        .await
        .expect("close() hung after the in-flight request")
        .unwrap();
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

/// A header deadline that expires after `server.close()` started must become
/// Node's silent `socket.destroy()` EOF, not a fabricated 408 (Node clears its
/// connections checker on close and never writes 408 during shutdown).
#[tokio::test]
async fn shutdown_folds_header_deadline_into_silent_eof() {
    let timer = Arc::new(KeepAliveTimer::new(
        Duration::from_millis(20),
        Duration::from_secs(1),
    ));
    let past = tokio::time::Instant::now() + Duration::from_millis(50);
    assert_eq!(timer.expired(past), ExpiredTimer::Headers);
    timer.begin_shutdown();
    assert_eq!(timer.expired(past), ExpiredTimer::Idle);
}

/// Replays Node's `maxHeaderSize` accounting on raw bytes: the request line is
/// excluded, every `name: value\r\n` field line counts, and an incomplete head
/// is only rejected once a line cannot fit. Both directions matter: a guard
/// that counts the request line rejects requests Node accepts, and one that
/// waits for `header_bytes > max` only after the head completes lets hyper's
/// parser answer first.
#[test]
fn header_guard_replays_node_field_budget() {
    let mut guard = HeaderGuard::new(16);
    assert!(
        !guard.push(b"GET / HTTP/1.1\r\n"),
        "request line is excluded"
    );
    assert!(!guard.push(b"A: b\r\n"), "6 field bytes fit");
    assert!(!guard.push(b"B: c\r\n"), "12 field bytes fit");
    assert!(!guard.push(b"\r\n"), "head completes under the limit");
    assert!(
        !guard.push(&[b'x'; 100]),
        "bytes after the head are body/next request, not this head"
    );

    // A follow-up head resets the budget at the next Idle phase.
    guard.begin(TimeoutPhase::Idle);
    assert!(!guard.push(b"GET / HTTP/1.1\r\n"));
    assert!(
        guard.push(b"X: 012345678901\r\n"),
        "17 field bytes exceed the 16-byte budget"
    );

    // A single field line that cannot fit is rejected before its terminator,
    // so hyper's read buffer cannot grow past the guard.
    let mut guard = HeaderGuard::new(16);
    assert!(!guard.push(b"GET / HTTP/1.1\r\n"));
    assert!(
        guard.push(&[b'a'; 17]),
        "a field line already longer than the budget cannot fit"
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
#[tokio::test]
async fn partial_headers_receive_node_408_bytes() {
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
    assert_eq!(status(&response), 408, "{response:?}");
    assert!(
        String::from_utf8_lossy(&response)
            .to_ascii_lowercase()
            .contains("connection: close"),
        "the body-timeout response must close the connection"
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
        .expect("requestTimeout did not fire")
        .unwrap();
    assert_eq!(status(&response), 408, "slow body must time out");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    assert!(body.is_empty(), "Node sends no body with 408");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "Node closes the connection after 408"
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
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
/// the 16 KiB header cap must still arrive whole.
#[tokio::test]
async fn large_list_responses_are_not_truncated_by_the_header_buffer_cap() {
    let (store, directory) = file_store("large-response");
    let prompt = "a".repeat(64 * 1024);
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
        payload.len() > 64 * 1024,
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
