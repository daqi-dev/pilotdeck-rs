//! #886 real HTTP replays of the actual Node v22.22.0 SDK transport corpus.
use super::*;
use crate::runtime::mcp::protocol::ResultSchema;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};

const ORACLE: &str = include_str!("../../../tests/fixtures/mcp-http-encoding/oracle.json");

fn text<'a>(v: &'a Value, tag: &str) -> &'a str {
    v.as_str()
        .unwrap_or_else(|| panic!("{tag}: fixture string"))
}
fn hex(v: &Value, tag: &str) -> Vec<u8> {
    let wire = text(v, tag);
    assert_eq!(wire.len() % 2, 0, "{tag}: complete hex bytes");
    wire.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let s = std::str::from_utf8(pair)
                .unwrap_or_else(|error| panic!("{tag}: hex text: {error}"));
            u8::from_str_radix(s, 16).unwrap_or_else(|error| panic!("{tag}: hex byte: {error}"))
        })
        .collect()
}
async fn bounded<F: std::future::Future>(future: F, tag: &str) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .unwrap_or_else(|error| panic!("{tag}: loopback watchdog: {error}"))
}
struct Server {
    url: Url,
    task: JoinHandle<()>,
    requested: Option<oneshot::Receiver<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        // Aborting the listener drops its JoinSet and every owned socket.
        self.task.abort();
    }
}
async fn respond(
    mut socket: TcpStream,
    case: Value,
    tag: &'static str,
    requested: Arc<Mutex<Option<oneshot::Sender<()>>>>,
) {
    let mut request = Vec::new();
    let end = loop {
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request read: {error}"));
        assert!(n > 0, "{tag}: request headers not truncated");
        request.extend_from_slice(&buf[..n]);
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..end]);
    let first = headers
        .lines()
        .next()
        .unwrap_or_else(|| panic!("{tag}: request line"));
    let get = first.starts_with("GET ");
    let redirect = first.contains(" /mcp ");
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length").then(|| {
                value
                    .trim()
                    .parse::<usize>()
                    .unwrap_or_else(|error| panic!("{tag}: request length: {error}"))
            })
        })
        .unwrap_or(0);
    while request.len() < end + length {
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request payload: {error}"));
        assert!(n > 0, "{tag}: request payload not truncated");
        request.extend_from_slice(&buf[..n]);
    }
    let mode = text(&case["mode"], tag);
    if mode == "get-sse" && !get {
        socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap_or_else(|error| panic!("{tag}: accepted POST: {error}"));
        if String::from_utf8_lossy(&request[end..]).contains("tools/list")
            && let Some(requested) = lock(&requested).take()
        {
            let _ = requested.send(());
        }
        return;
    }
    if mode == "legacy-error" && get {
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: endpoint\ndata: /post\n\n")
            .await.unwrap_or_else(|error| panic!("{tag}: legacy endpoint: {error}"));
        // The transport closes this stream after the POST result. No sleep or
        // reconnect race participates in the assertion.
        let mut buf = [0; 1];
        let _ = bounded(socket.read(&mut buf), tag).await;
        return;
    }
    if mode == "redirect-json" && redirect {
        socket.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /target\r\nContent-Encoding: gzip,gzip,gzip,gzip,gzip,gzip\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx")
            .await.unwrap_or_else(|error| panic!("{tag}: redirect write: {error}"));
        return;
    }
    let body = hex(&case["wire"], tag);
    let status = if mode.ends_with("-error") {
        "400 Bad Request"
    } else {
        "200 OK"
    };
    let content = if mode.ends_with("sse") || mode == "legacy-start" {
        "text/event-stream"
    } else {
        "application/json"
    };
    let encoding = text(&case["encoding"], tag);
    let encoding = if encoding.is_empty() {
        String::new()
    } else {
        format!("Content-Encoding: {encoding}\r\n")
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content}\r\n{encoding}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("{tag}: response headers: {error}"));
    socket
        .write_all(&body)
        .await
        .unwrap_or_else(|error| panic!("{tag}: response body: {error}"));
}
async fn server(case: &Value, tag: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("{tag}: bind: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("{tag}: address: {error}"));
    let url = Url::parse(&format!("http://{address}/mcp"))
        .unwrap_or_else(|error| panic!("{tag}: URL: {error}"));
    let case = case.clone();
    let (requested, request_seen) = oneshot::channel();
    let requested = Arc::new(Mutex::new(Some(requested)));
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted.unwrap_or_else(|error| panic!("{tag}: accept: {error}"));
                    connections.spawn(respond(socket, case.clone(), tag, requested.clone()));
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result.unwrap_or_else(|| panic!("{tag}: connection task"))
                        .unwrap_or_else(|error| panic!("{tag}: connection panic: {error}"));
                }
            }
        }
    });
    Server {
        url,
        task,
        requested: Some(request_seen),
    }
}
fn error_shape(error: McpClientError) -> Value {
    let code = match error.code() {
        Some(McpErrorCode::Client(code)) => json!(code.as_str()),
        Some(McpErrorCode::Number(n)) => json!(*n as i64),
        Some(McpErrorCode::Text(s)) => json!(s),
        Some(McpErrorCode::Errno(s)) => json!(s),
        None => Value::Null,
    };
    json!({"error": {"name": error.name(), "code": code, "message": error.message()}})
}
async fn replay(mode: &str, tag: &'static str, layers_only: bool) {
    let corpus: Value =
        serde_json::from_str(ORACLE).unwrap_or_else(|error| panic!("{tag}: corpus JSON: {error}"));
    let cases = corpus["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("{tag}: corpus cases"));
    let mut checked = 0;
    for case in cases.iter().filter(|case| case["mode"] == mode) {
        let name = text(&case["name"], tag);
        if layers_only && !matches!(name, "five" | "six" | "unknown-six") {
            continue;
        }
        // The one supplemental legacy corruption gap is replayed and
        // reported by mcp_legacy_corrupt_gap_is_observed below, not accepted.
        if name == "corrupt" && mode == "legacy-start" {
            continue;
        }
        let mut server = server(case, tag).await;
        let kind = if mode.starts_with("legacy") {
            HttpKind::LegacySse
        } else {
            HttpKind::Streamable
        };
        let wire = Wire::http(HttpLink::new(
            kind,
            server.url.clone(),
            vec![],
            2000.0,
            2000.0,
        ));
        let link = wire
            .http_link()
            .unwrap_or_else(|| panic!("{tag}: HTTP link"));
        let actual = match bounded(link.start(&wire), tag).await {
            Err(error) => error_shape(error),
            Ok(()) if mode == "legacy-start" => json!({"started": true}),
            Ok(()) if mode == "legacy-error" => match bounded(
                link.send_legacy(
                    parse_json(r#"{"jsonrpc":"2.0","id":0,"method":"tools/list"}"#)
                        .unwrap_or_else(|error| panic!("{tag}: request JSON: {error:?}")),
                ),
                tag,
            )
            .await
            {
                Ok(()) => json!({"sent": true}),
                Err(error) => error_shape(error),
            },
            Ok(()) if mode == "get-sse" => {
                let request_wire = wire.clone();
                let pending = tokio::spawn(async move {
                    request_wire
                        .request(
                            "tools/list",
                            None,
                            500.0,
                            ResultSchema::ListTools,
                            None,
                            None,
                        )
                        .await
                });
                bounded(
                    server
                        .requested
                        .take()
                        .unwrap_or_else(|| panic!("{tag}: request barrier")),
                    tag,
                )
                .await
                .unwrap_or_else(|error| panic!("{tag}: request registered: {error}"));
                bounded(
                    link.send(
                        &wire,
                        parse_json(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                            .unwrap_or_else(|error| panic!("{tag}: initialized JSON: {error:?}")),
                    ),
                    tag,
                )
                .await
                .unwrap_or_else(|error| panic!("{tag}: initialized send: {error}"));
                match bounded(pending, tag)
                    .await
                    .unwrap_or_else(|error| panic!("{tag}: request task: {error}"))
                {
                    Ok(value) => json!({"value": serde_json::from_str::<Value>(&frame_text(&value)
                        .unwrap_or_else(|error| panic!("{tag}: GET result JSON: {error}")))
                        .unwrap_or_else(|error| panic!("{tag}: GET result parse: {error}"))}),
                    Err(error) => error_shape(error),
                }
            }
            Ok(()) => match bounded(
                wire.request(
                    "tools/list",
                    None,
                    500.0,
                    ResultSchema::ListTools,
                    None,
                    None,
                ),
                tag,
            )
            .await
            {
                Ok(value) => json!({"value": serde_json::from_str::<Value>(&frame_text(&value)
                    .unwrap_or_else(|error| panic!("{tag}: result JSON: {error}")))
                    .unwrap_or_else(|error| panic!("{tag}: result parse: {error}"))}),
                Err(error) => error_shape(error),
            },
        };
        wire.close().await;
        assert_eq!(actual, case["result"], "{tag}: {mode}/{name}");
        checked += 1;
    }
    let expected = if layers_only {
        3
    } else if mode == "legacy-start" {
        10
    } else {
        11
    };
    assert_eq!(
        checked, expected,
        "{tag}: every intended golden cell was replayed"
    );
}
#[tokio::test]
async fn mcp_json_decodes() {
    replay("json", "#886 JSON decoding", false).await;
}
#[tokio::test]
async fn mcp_sse_decodes() {
    replay("post-sse", "#886 SSE decoding", false).await;
}
#[tokio::test]
async fn mcp_legacy_sse_decodes() {
    replay("legacy-start", "#886 legacy SSE decoding", false).await;
}
#[tokio::test]
async fn mcp_streamable_error_decodes() {
    replay("streamable-error", "#886 streamable error decoding", false).await;
}
#[tokio::test]
async fn mcp_legacy_error_decodes() {
    replay("legacy-error", "#886 legacy error decoding", false).await;
}
#[tokio::test]
async fn mcp_redirect_decodes() {
    replay("redirect-json", "#886 redirect decoding", false).await;
}
#[tokio::test]
async fn mcp_encoding_limit() {
    replay("json", "#886 encoding layer cap", true).await;
}
#[test]
fn mcp_encoding_provenance() {
    crate::test_baseline::oracle(ORACLE);
}
#[test]
#[ignore = "requires real Node v22.22.0 and PDRS_BASELINE"]
fn mcp_encoding_regenerates_identically() {
    crate::test_baseline::regenerates_identically("mcp-http-encoding");
}

#[tokio::test]
async fn mcp_get_sse_decodes() {
    replay("get-sse", "#886 GET SSE decoding", false).await;
}

/// Supplemental evidence, explicitly NOT an accepted TS parity result.
/// Changing shared decoder error detail or legacy reconnect lifetime is
/// outside the visibility-only #867 reuse authorized for this change.
#[tokio::test]
async fn mcp_legacy_corrupt_gap_is_observed() {
    let tag = "#886 legacy corrupt diagnostic";
    let corpus: Value =
        serde_json::from_str(ORACLE).expect("#886 legacy corrupt diagnostic: corpus");
    let case = corpus["cases"]
        .as_array()
        .expect("#886 legacy corrupt diagnostic: cases")
        .iter()
        .find(|case| case["mode"] == "legacy-start" && case["name"] == "corrupt")
        .expect("#886 legacy corrupt diagnostic: retained TS case");
    let server = server(case, tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::LegacySse,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    let link = wire
        .http_link()
        .expect("#886 legacy corrupt diagnostic: HTTP link");
    let response = bounded(
        network_fetch(
            &link.client,
            None,
            Method::GET,
            &server.url,
            &HeaderList::default(),
            None,
            Fetch::Plain,
            &link.aborted,
            false,
        ),
        tag,
    )
    .await
    .expect("#886 legacy corrupt diagnostic: headers accepted");
    let (body, continuation) = response.into_body();
    let (ready, mut outcome) = oneshot::channel();
    let mut ready = Some(ready);
    bounded(
        legacy_stream(
            &Arc::downgrade(&wire),
            body,
            &link.aborted,
            &server.url,
            &mut ready,
            continuation,
        ),
        tag,
    )
    .await;
    assert!(
        matches!(outcome.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
        "#886 legacy corrupt diagnostic: existing legacy body failure leaves start unsettled"
    );
    assert_eq!(
        case["result"]["error"]["message"],
        "SSE error: TypeError: terminated: incorrect header check",
        "#886 legacy corrupt diagnostic: TS rejects immediately"
    );
    println!(
        "#886 SUPPLEMENTAL GAP legacy-start/corrupt: TS={} Rust=start remains unsettled after body read failure (existing reconnect loop)",
        case["result"]
    );
    wire.close().await;
}

// #1114: the detached-continuation dispatch-order contract. The baseline
// queues each fire-and-forget send's first step as a microtask before the
// awaited promise resolves, so the fetch is dispatched before the caller's
// next operation — a `close()` immediately after a send must never swallow
// the request line. Each test counts `detach` calls on its own thread (a
// current_thread runtime keeps the inline poll and the spawned remainder
// there) and asserts the count at the moment the modeled JS continuation
// must already have run.

/// What an accepted connection delivered before the client went away. The
/// closed byte-level criterion of the `#1114` GET tests: zero bytes, a
/// partial line, and a complete request line stay distinguishable — a
/// whole-header read that returns `None` on EOF would discard exactly that
/// distinction (the fix-r2 reading could not prove which it had seen).
enum Delivered {
    /// EOF before any byte arrived.
    Eof,
    /// Bytes, but no CRLF before EOF: the raw partial content.
    Partial(Vec<u8>),
    /// A complete first line, its CRLF excluded.
    Line(String),
}
impl std::fmt::Debug for Delivered {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Delivered::Eof => formatter.write_str("EOF before any byte"),
            Delivered::Partial(bytes) => write!(
                formatter,
                "partial {} byte(s): {:?}",
                bytes.len(),
                String::from_utf8_lossy(bytes)
            ),
            Delivered::Line(line) => write!(formatter, "line {line:?}"),
        }
    }
}

/// Reads only up to the first CRLF — the request line — and reports exactly
/// what arrived before any EOF, plus the bytes that already followed the line
/// in the same segment (for the caller's header/body continuation).
async fn read_request_line(socket: &mut TcpStream, tag: &'static str) -> (Delivered, Vec<u8>) {
    let mut arrived = Vec::new();
    loop {
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request line read: {error}"));
        if n == 0 {
            return (
                if arrived.is_empty() {
                    Delivered::Eof
                } else {
                    Delivered::Partial(arrived)
                },
                Vec::new(),
            );
        }
        arrived.extend_from_slice(&buf[..n]);
        if let Some(end) = arrived.windows(2).position(|w| w == b"\r\n") {
            let rest = arrived.split_off(end + 2);
            return (
                Delivered::Line(String::from_utf8_lossy(&arrived[..end]).into_owned()),
                rest,
            );
        }
    }
}

/// Reads one request (headers plus Content-Length body); `None` once the
/// client went away. `started` carries bytes already read before this call
/// (a request line plus whatever followed it in the same segment).
async fn read_request_from(
    socket: &mut TcpStream,
    started: Vec<u8>,
    tag: &'static str,
) -> Option<String> {
    let mut request = started;
    let end = loop {
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request read: {error}"));
        if n == 0 {
            return None;
        }
        request.extend_from_slice(&buf[..n]);
    };
    let headers = String::from_utf8_lossy(&request[..end]);
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length").then(|| {
                value
                    .trim()
                    .parse::<usize>()
                    .unwrap_or_else(|error| panic!("{tag}: request length: {error}"))
            })
        })
        .unwrap_or(0);
    while request.len() < end + length {
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request payload: {error}"));
        if n == 0 {
            return None;
        }
        request.extend_from_slice(&buf[..n]);
    }
    Some(String::from_utf8_lossy(&request).into_owned())
}

/// Reads one request (headers plus Content-Length body); `None` once the
/// client went away.
async fn read_request(socket: &mut TcpStream, tag: &'static str) -> Option<String> {
    read_request_from(socket, Vec::new(), tag).await
}

/// Accepts connections, never answers, and reports every complete request:
/// a pending request can only end by its own timeout, and the sends the
/// dispatch-order contract promises are observed on the wire, not only in
/// the `detach` counter.
async fn recording_silent_server(tag: &'static str) -> (Server, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("{tag}: bind: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("{tag}: address: {error}"));
    let url = Url::parse(&format!("http://{address}/mcp"))
        .unwrap_or_else(|error| panic!("{tag}: URL: {error}"));
    let (reports, lines) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) =
                        accepted.unwrap_or_else(|error| panic!("{tag}: accept: {error}"));
                    let reports = reports.clone();
                    connections.spawn(async move {
                        while let Some(request) = read_request(&mut socket, tag).await {
                            if reports.send(request).await.is_err() {
                                return;
                            }
                        }
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result
                        .unwrap_or_else(|| panic!("{tag}: connection task"))
                        .unwrap_or_else(|error| panic!("{tag}: connection panic: {error}"));
                }
            }
        }
    });
    (
        Server {
            url,
            task,
            requested: None,
        },
        lines,
    )
}

/// Answers the first request with `first` (a raw JSON body) and every later
/// request with 202, so a scripted server-request reply terminates instead
/// of looping, and reports every request it read.
async fn first_then_accepted(
    first: &'static str,
    tag: &'static str,
) -> (Server, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("{tag}: bind: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("{tag}: address: {error}"));
    let url = Url::parse(&format!("http://{address}/mcp"))
        .unwrap_or_else(|error| panic!("{tag}: URL: {error}"));
    let answered = Arc::new(Mutex::new(false));
    let (reports, lines) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) =
                        accepted.unwrap_or_else(|error| panic!("{tag}: accept: {error}"));
                    let answered = answered.clone();
                    let reports = reports.clone();
                    connections.spawn(async move {
                        let Some(request) = read_request(&mut socket, tag).await else {
                            return;
                        };
                        if reports.send(request).await.is_err() {
                            return;
                        }
                        let response = if !std::mem::replace(&mut *lock(&answered), true) {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{first}",
                                first.len(),
                            )
                        } else {
                            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\
                             Connection: close\r\n\r\n"
                                .to_owned()
                        };
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .unwrap_or_else(|error| panic!("{tag}: scripted response: {error}"));
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result
                        .unwrap_or_else(|| panic!("{tag}: connection task"))
                        .unwrap_or_else(|error| panic!("{tag}: connection panic: {error}"));
                }
            }
        }
    });
    (
        Server {
            url,
            task,
            requested: None,
        },
        lines,
    )
}

/// Answers the first request (the `notifications/initialized` POST) with 202
/// plus `Connection: close` — the SSE GET must open its own connection — and
/// on that connection reports exactly what it delivered before the abort
/// closed it: zero bytes, a partial line, or the complete request line. The
/// report itself (either way) is the observable of the GET having entered the
/// network exchange.
async fn get_then_connect_server(tag: &'static str) -> (Server, mpsc::Receiver<Delivered>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("{tag}: bind: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("{tag}: address: {error}"));
    let url = Url::parse(&format!("http://{address}/mcp"))
        .unwrap_or_else(|error| panic!("{tag}: URL: {error}"));
    let (reports, connections_seen) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let mut first = true;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) =
                        accepted.unwrap_or_else(|error| panic!("{tag}: accept: {error}"));
                    if !std::mem::replace(&mut first, false) {
                        // The GET's own connect; the client-side abort closes
                        // it soon after, but the accept queue keeps it. What
                        // it delivered before that close — zero bytes, a
                        // partial line, or the complete line — is the closed
                        // criterion (ledger A6.5-MCP-DETACHED-DISPATCH-002).
                        let (delivered, _) = read_request_line(&mut socket, tag).await;
                        if reports.send(delivered).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    connections.spawn(async move {
                        if read_request(&mut socket, tag).await.is_none() {
                            return;
                        }
                        socket
                            .write_all(
                                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\
                                 Connection: close\r\n\r\n",
                            )
                            .await
                            .unwrap_or_else(|error| panic!("{tag}: accepted POST: {error}"));
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result
                        .unwrap_or_else(|| panic!("{tag}: connection task"))
                        .unwrap_or_else(|error| panic!("{tag}: connection panic: {error}"));
                }
            }
        }
    });
    (
        Server {
            url,
            task,
            requested: None,
        },
        connections_seen,
    )
}

/// Answers every POST with a keep-alive 202 (no `Connection: close`), so the
/// SSE GET can ride the very connection its triggering POST used, and reports
/// what any connection's first request line delivered: the complete line, or
/// the terminal zero-byte/partial state when the client went away first.
/// This is the shape of the original #1114 CI flake — its mock server keeps
/// connections alive — and the shape in which the TS baseline guarantees the
/// line (fix-r3 probe C, pooled mode: Node v22.22.0, 50/50 runs, the line
/// landing after `close()` had already returned).
async fn pooled_line_server(tag: &'static str) -> (Server, mpsc::Receiver<Delivered>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("{tag}: bind: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("{tag}: address: {error}"));
    let url = Url::parse(&format!("http://{address}/mcp"))
        .unwrap_or_else(|error| panic!("{tag}: URL: {error}"));
    let (reports, lines) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) =
                        accepted.unwrap_or_else(|error| panic!("{tag}: accept: {error}"));
                    let reports = reports.clone();
                    connections.spawn(async move {
                        loop {
                            let (delivered, rest) = read_request_line(&mut socket, tag).await;
                            let line = match delivered {
                                Delivered::Line(line) => line,
                                // The client went away (or stopped mid-line)
                                // before any GET: report the terminal state —
                                // the caller must be able to tell a missing
                                // dispatch from a swallowed line.
                                terminal @ (Delivered::Eof | Delivered::Partial(_)) => {
                                    let _ = reports.send(terminal).await;
                                    return;
                                }
                            };
                            if line.starts_with("GET ") {
                                let _ = reports.send(Delivered::Line(line)).await;
                                return;
                            }
                            // The initialized POST: finish reading it, then
                            // answer a keep-alive 202 so the GET can ride
                            // this same connection.
                            let mut started = line.into_bytes();
                            started.extend_from_slice(b"\r\n");
                            started.extend_from_slice(&rest);
                            if read_request_from(&mut socket, started, tag).await.is_none() {
                                return;
                            }
                            socket
                                .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
                                .await
                                .unwrap_or_else(|error| panic!("{tag}: keep-alive 202: {error}"));
                        }
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    result
                        .unwrap_or_else(|| panic!("{tag}: connection task"))
                        .unwrap_or_else(|error| panic!("{tag}: connection panic: {error}"));
                }
            }
        }
    });
    (
        Server {
            url,
            task,
            requested: None,
        },
        lines,
    )
}

/// #1114: a 202 for `notifications/initialized` must dispatch the SSE GET
/// before `send()` resolves, so a `close()` that immediately follows cannot
/// abort an undispatched GET (the CI flake dropped the line from the server
/// log entirely).
#[tokio::test]
async fn dispatch_get_before_close() {
    let tag = "#1114 GET dispatch";
    let case = json!({"mode": "get-sse", "wire": "", "encoding": ""});
    let server = server(&case, tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    let link = wire
        .http_link()
        .unwrap_or_else(|| panic!("{tag}: HTTP link"));
    super::reset_detached_dispatches();
    bounded(
        link.send(
            &wire,
            parse_json(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap_or_else(|error| panic!("{tag}: initialized JSON: {error:?}")),
        ),
        tag,
    )
    .await
    .unwrap_or_else(|error| panic!("{tag}: initialized send: {error}"));
    // `McpClient.close()` awaits nothing between the send settling and the
    // synchronous abort.
    link.close();
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 the SSE GET is dispatched before close aborts it"
    );
}

/// #1114 fix-r1: the dispatch guarantee itself, observed past the abort gate.
/// A `close()` that fires after `detach` returns but before the spawned
/// remainder is ever scheduled must find the GET already inside the network
/// exchange (the baseline had the fetch called before `send()` resolved), so
/// the server observes the GET's own TCP connection; a bare spawn leaves the
/// request undispatched at the gate and the connection never opens. The
/// bound only turns a missing dispatch into this assertion's failure — on
/// this code the report arrives in micro-seconds.
///
/// #1114 fix-r2: the server also reads what that connection delivered: EOF
/// before any byte, 10/10 runs — the honest record of this interleaving.
///
/// #1114 fix-r3: the read is closed up to the review's criterion (zero
/// bytes, a partial line, a complete line stay distinguishable) and the
/// delivered state is asserted, not just printed. On this fresh-connection
/// interleaving the TS baseline also delivers zero bytes — fix-r3 probe C,
/// fresh mode: Node v22.22.0, 50/50 runs, the GET connection accepted with
/// zero bytes and closed — an abort that lands while the connect is still
/// in flight retracts the request there too. Rust matching that is parity;
/// the line-landing guarantee belongs to the pooled shape
/// (`dispatch_get_line_lands_before_close` below), where TS lands it 50/50.
#[tokio::test]
async fn dispatch_get_connects_before_close() {
    let tag = "#1114 GET exchange entry";
    let (server, mut connections) = get_then_connect_server(tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    let link = wire
        .http_link()
        .unwrap_or_else(|| panic!("{tag}: HTTP link"));
    super::reset_detached_dispatches();
    bounded(
        link.send(
            &wire,
            parse_json(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap_or_else(|error| panic!("{tag}: initialized JSON: {error:?}")),
        ),
        tag,
    )
    .await
    .unwrap_or_else(|error| panic!("{tag}: initialized send: {error}"));
    // Deterministic barrier: the spawned remainder has not been polled yet
    // (a synchronous sleep never yields this runtime, so nothing spawned
    // runs), so the close below is still strictly before the remainder's
    // first poll — and strictly after the inline first poll that initiated
    // the GET's own connect. The sleep only lets the kernel finish that
    // connect's TCP handshake: without it the accept race was measured at
    // 1.0–5.9%/2000 (immediate socket drop, load-dependent) vs
    // 1999–2000/2000 (≥200µs before the drop), and the suite's parallel
    // load lands inside that window ~3% of runs — the barrier's 200ms is
    // three orders of magnitude above the measured need. The TS baseline's
    // slower teardown always lets the handshake finish — probe C fresh
    // mode: 50/50 runs with the connection accepted and zero bytes — so
    // this barrier observes the same server-visible state.
    std::thread::sleep(Duration::from_millis(200));
    link.close();
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 the SSE GET is dispatched before close aborts it"
    );
    let delivered = tokio::time::timeout(Duration::from_secs(10), connections.recv())
        .await
        .ok()
        .flatten();
    crate::negative_assert!(
        delivered.is_some(),
        "#1114 the SSE GET enters the network exchange before close aborts it"
    );
    let delivered = delivered.expect("#1114 GET exchange entry: report arrived");
    let shape = match &delivered {
        Delivered::Eof => "eof",
        Delivered::Partial(_) => "partial",
        Delivered::Line(_) => "line",
    };
    println!("{tag}: the aborted fresh GET connection delivered: {delivered:?}");
    crate::negative_assert_eq!(
        shape,
        "eof",
        "#1114 the fresh-connection GET delivers zero bytes before close, matching the TS baseline"
    );
}

/// #1114 fix-r3: the byte-level guarantee in the shape where the baseline
/// actually gives it — the shape of the original CI flake. The POST's 202
/// keeps the connection alive, so the dispatched GET rides the pooled socket;
/// its request line must land even though `close()` follows the send with no
/// scheduler turn in between (TS probe C, pooled mode: Node v22.22.0, 50/50
/// runs, the line landing after `close()` had already returned; the original
/// issue's 500/500 runs). The `#1114` CI flake itself was this shape: the
/// mock server keeps connections alive, and the missing GET line was the
/// undispatched window-A abort, not the in-flight one.
#[tokio::test]
async fn dispatch_get_line_lands_before_close() {
    let tag = "#1114 GET pooled line";
    let (server, mut connections) = pooled_line_server(tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    let link = wire
        .http_link()
        .unwrap_or_else(|| panic!("{tag}: HTTP link"));
    super::reset_detached_dispatches();
    bounded(
        link.send(
            &wire,
            parse_json(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap_or_else(|error| panic!("{tag}: initialized JSON: {error:?}")),
        ),
        tag,
    )
    .await
    .unwrap_or_else(|error| panic!("{tag}: initialized send: {error}"));
    // Deterministic barrier: the spawned remainder has not been scheduled
    // yet, so this close is strictly after the inline first poll that
    // dispatched the GET onto the pooled connection.
    link.close();
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 the SSE GET is dispatched before close aborts it"
    );
    let delivered = tokio::time::timeout(Duration::from_secs(10), connections.recv())
        .await
        .ok()
        .flatten();
    let landed = matches!(&delivered, Some(Delivered::Line(line)) if line.starts_with("GET /mcp"));
    println!("{tag}: the pooled GET connection delivered: {delivered:?}");
    crate::negative_assert!(
        landed,
        "#1114 the pooled-socket GET request line lands before close aborts it"
    );
}

/// #1114 fix-r1: a continuation that is already complete on the inline first
/// poll must not be spawned at all — polling a finished async block panics.
/// Direct `detach` probe: one poll is all a Ready continuation may ever see.
#[tokio::test]
async fn dispatch_ready_continuation_is_not_polled_again() {
    // A hand-written future (not an async block): a spawned re-poll of an
    // async block panics before any code runs, so the counter would stay 1
    // and miss the mutant; this one increments on every poll, legal or not.
    struct EveryPoll(Arc<std::sync::atomic::AtomicU32>);
    impl std::future::Future for EveryPoll {
        type Output = ();
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(())
        }
    }
    let observed = Arc::new(std::sync::atomic::AtomicU32::new(0));
    super::reset_detached_dispatches();
    detach(EveryPoll(observed.clone()));
    // Enough scheduler turns for an (unwanted) spawned remainder to run.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    crate::negative_assert_eq!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#1114 a Ready continuation is not polled again"
    );
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 the Ready dispatch was counted once"
    );
}

/// #1114 fix-r1: the same contract at a real detached site. A legacy
/// transport with no endpoint rejects the send before its first suspension,
/// so `send_detached`'s continuation is Ready on the inline poll; spawning
/// it re-polls the finished async block and panics
/// ("`async fn` resumed after completion").
///
/// #1114 fix-r3: the panic is observed through the spawned remainder's join
/// handle (`detach` stashes it for tests) — the awaited handle is the only
/// process-hook-free observable of a task panic. This replaces the
/// process-level `PanicRecorder` of fix-r2, whose `restore` had its own
/// lock-order hazard and needed a concurrent-panic regression to guard it;
/// with no hook installed at all, that deadlock class no longer exists.
#[tokio::test]
async fn dispatch_ready_send_detached_does_not_panic() {
    let tag = "#1114 Ready send_detached";
    let wire = Wire::http(HttpLink::new(
        HttpKind::LegacySse,
        Url::parse("http://127.0.0.1:1/mcp").unwrap_or_else(|error| panic!("{tag}: URL: {error}")),
        vec![],
        2000.0,
        2000.0,
    ));
    super::reset_detached_dispatches();
    wire.send_detached(
        parse_json(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#)
            .unwrap_or_else(|error| panic!("{tag}: cancel JSON: {error:?}")),
    );
    // The pre-aborted branch of the same contract: a close that precedes the
    // send leaves the abort gate rejecting `network_fetch` synchronously, so
    // this continuation is also Ready on the inline poll.
    let closed = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        Url::parse("http://127.0.0.1:1/mcp").unwrap_or_else(|error| panic!("{tag}: URL: {error}")),
        vec![],
        2000.0,
        2000.0,
    ));
    closed
        .http_link()
        .unwrap_or_else(|| panic!("{tag}: HTTP link"))
        .close();
    closed.send_detached(
        parse_json(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#)
            .unwrap_or_else(|error| panic!("{tag}: closed cancel JSON: {error:?}")),
    );
    // Enough scheduler turns for an (unwanted) spawned remainder to run and
    // panic, and for its handle to settle.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let mut panicked = Vec::new();
    for task in super::take_detached_tasks() {
        match bounded(task, tag).await {
            Ok(()) => {}
            // The watchdog timeout above already panicked its own named
            // message; only a task panic or cancellation lands here.
            Err(join) if join.is_panic() => panicked.push(format!("{join}")),
            Err(join) => panic!("{tag}: detached task cancelled: {join}"),
        }
    }
    if !panicked.is_empty() {
        println!("{tag}: panicked detached tasks: {panicked:?}");
    }
    crate::negative_assert!(
        panicked.is_empty(),
        "#1114 a first-poll-Ready detached continuation is not spawned and polled again"
    );
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        2,
        "#1114 both Ready sends were counted"
    );
}

/// #1114 fix-r1: `Protocol.request` fires `transport.send()` before
/// returning the promise, but its timeout is armed even earlier
/// (`_setupTimeout` precedes `transport.send`), so the request POST keeps
/// its background send — only `Protocol.cancel`'s `notifications/cancelled`
/// dispatches inline — and both still reach the wire.
#[tokio::test]
async fn dispatch_request_background_and_cancel_inline() {
    let tag = "#1114 request background and cancel dispatch";
    let (server, mut lines) = recording_silent_server(tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    super::reset_detached_dispatches();
    let outcome = bounded(
        wire.request("tools/list", None, 1.0, ResultSchema::ListTools, None, None),
        tag,
    )
    .await;
    let error = match outcome {
        Err(error) => error,
        Ok(value) => panic!("{tag}: silent server cannot satisfy a request: {value:?}"),
    };
    assert!(
        error.message().contains("Request timed out"),
        "{tag}: request ends by its own timeout, not by a send failure"
    );
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 only the timeout cancel is dispatched inline; the request POST keeps its background send"
    );
    // The background request POST and the inline cancel both reach the wire:
    // the narrowing must not swallow either send.
    let mut saw_request = false;
    let mut saw_cancel = false;
    bounded(
        async {
            while !(saw_request && saw_cancel) {
                let line = lines
                    .recv()
                    .await
                    .unwrap_or_else(|| panic!("{tag}: request line stream closed"));
                saw_request |= line.contains("tools/list");
                saw_cancel |= line.contains("notifications/cancelled");
            }
        },
        tag,
    )
    .await;
    crate::negative_assert!(
        saw_request,
        "#1114 the request POST reaches the wire from its background send"
    );
    crate::negative_assert!(
        saw_cancel,
        "#1114 the timeout cancel reaches the wire from its inline dispatch"
    );
}

/// #1114: a server request inside a response body gets its `Method not
/// found` reply dispatched inside the body-parse continuation, before the
/// caller's request future resolves — and the reply reaches the wire.
#[tokio::test]
async fn dispatch_reply_before_resolve() {
    let tag = "#1114 reply dispatch";
    let body = concat!(
        r#"[{"jsonrpc":"2.0","id":0,"result":{"tools":[]}},"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"sampling/createMessage","params":{}}]"#,
    );
    let (server, mut lines) = first_then_accepted(body, tag).await;
    let wire = Wire::http(HttpLink::new(
        HttpKind::Streamable,
        server.url.clone(),
        vec![],
        2000.0,
        2000.0,
    ));
    super::reset_detached_dispatches();
    let outcome = bounded(
        wire.request(
            "tools/list",
            None,
            2000.0,
            ResultSchema::ListTools,
            None,
            None,
        ),
        tag,
    )
    .await;
    let value = match outcome {
        Ok(value) => value,
        Err(error) => panic!("{tag}: scripted request failed: {error:?}"),
    };
    let text = frame_text(&value).unwrap_or_else(|error| panic!("{tag}: result JSON: {error:?}"));
    assert!(text.contains("tools"), "{tag}: scripted tools/list result");
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        1,
        "#1114 the reply to a server request is dispatched inline"
    );
    let reply = bounded(
        async {
            loop {
                let line = lines
                    .recv()
                    .await
                    .unwrap_or_else(|| panic!("{tag}: reply line stream closed"));
                if line.contains("Method not found") {
                    return line;
                }
            }
        },
        tag,
    )
    .await;
    println!(
        "{tag}: reply line reached the wire: {}",
        reply.split('\r').next().unwrap_or_default()
    );
    crate::negative_assert!(
        reply.contains("Method not found"),
        "#1114 the reply to a server request reaches the wire"
    );
}
