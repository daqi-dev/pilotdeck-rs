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

/// Node's `maxHeaderSize` default is 16 KiB; hyper's read buffer replays 431.
#[tokio::test]
async fn oversized_headers_are_rejected_like_node() {
    let (store, directory) = file_store("headers");
    let (server, port) = start_limited(store, ServeLimits::default()).await;
    let padding = "a".repeat(DEFAULT_MAX_HEADER_BYTES);
    let request = format!(
        "GET {STORE_PATH_PREFIX}list HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Pad: {padding}\r\nConnection: close\r\n\r\n"
    );
    let response = exchange(port, request.as_bytes()).await.unwrap();
    assert_eq!(status(&response), 431, "oversized header must be 431");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    assert!(body.is_empty(), "Node sends no body with 431");
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "Node closes the connection after 431"
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

/// Node has no separate Rust-side keep-alive timer; the 60 s header timeout is
/// the registered fallback that eventually releases an idle connection's
/// permit. A short test timeout proves that fallback actually covers the gap
/// between two requests, not just a half-written first head.
#[tokio::test]
async fn idle_keep_alive_connections_close_at_headers_timeout() {
    let (store, directory) = file_store("idle-timeout");
    let limits = ServeLimits {
        headers_timeout: Duration::from_millis(200),
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
        .expect("an idle keep-alive connection outlived headersTimeout");
    assert_eq!(read.unwrap(), 0, "expected EOF at headersTimeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the fallback did not fire promptly: {:?}",
        started.elapsed()
    );
    server.close().await;
    let _ = std::fs::remove_dir_all(directory);
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

#[tokio::test]
async fn partial_headers_are_closed_after_headers_timeout() {
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
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte)).await;
    assert!(
        read.is_ok(),
        "half a header stayed open past headersTimeout (slowloris)"
    );
    assert_eq!(
        read.unwrap().unwrap(),
        0,
        "expected EOF after headersTimeout"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "headersTimeout did not fire promptly: {:?}",
        started.elapsed()
    );
    server.close().await;
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
