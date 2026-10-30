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

/// Reads one request (headers plus Content-Length body); `None` once the
/// client went away.
async fn read_request(socket: &mut TcpStream, tag: &'static str) -> Option<String> {
    let mut request = Vec::new();
    let end = loop {
        let mut buf = [0; 1024];
        let n = bounded(socket.read(&mut buf), tag)
            .await
            .unwrap_or_else(|error| panic!("{tag}: request read: {error}"));
        if n == 0 {
            return None;
        }
        request.extend_from_slice(&buf[..n]);
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
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
/// on that connection reports what it delivered before the abort closed it:
/// the request line, or `None` for EOF first. The report itself (either way)
/// is the observable of the GET having entered the network exchange.
async fn get_then_connect_server(tag: &'static str) -> (Server, mpsc::Receiver<Option<String>>) {
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
                        // it soon after, but the accept queue keeps it. Read
                        // what it delivered before that close: the line if
                        // the bytes made it out, `None` when the abort won
                        // the race first — the caller must not read the
                        // connection alone as the line having been sent
                        // (ledger A6.5-MCP-DETACHED-DISPATCH-002).
                        let delivered = read_request(&mut socket, tag)
                            .await
                            .map(|request| {
                                request
                                    .split("\r\n")
                                    .next()
                                    .unwrap_or_default()
                                    .to_owned()
                            });
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

// Records the panics of the current thread: a spawned remainder that was
// polled past completion ("`async fn` resumed after completion") must be
// observable at the assertion, not only as stderr noise.
#[cfg(test)]
thread_local! {
    static DETACHED_PANICS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
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
/// #1114 fix-r2: the server also reads what that connection delivered. On
/// this timing the request line itself does not land (10/10 runs below read
/// EOF before any byte) — the registered executor-model difference in ledger
/// A6.5-MCP-DETACHED-DISPATCH-002, still unresolved — so this test asserts
/// the exchange entry it can guarantee and prints the delivered state; a
/// connect-only pass must not be read as the GET line having been sent.
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
    // Deterministic barrier: the spawned remainder has not been scheduled
    // yet, so this close is strictly after the inline first poll.
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
    // The honest record of what the aborted GET connection delivered: on
    // this timing the line does not land (the ledger difference above).
    let line = delivered.flatten();
    println!(
        "{tag}: the aborted GET connection delivered: {}",
        line.as_deref().unwrap_or("EOF before a request line")
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

/// The hook a recorder chains to and eventually restores, handed to both the
/// chain and the restore through the recorder's mutex.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

/// Installs the `DETACHED_PANICS` recorder hook, chained to the hook that
/// was current at install time. The previous hook lives in a mutex because
/// the `Box` itself cannot be handed to both the chain and the restore; the
/// chain must keep forwarding, so another test's panic keeps its output
/// while this hook is installed.
struct PanicRecorder {
    previous: Arc<Mutex<Option<PanicHook>>>,
}

impl PanicRecorder {
    fn hook(chained: Arc<Mutex<Option<PanicHook>>>) -> PanicHook {
        Box::new(move |info| {
            DETACHED_PANICS.with(|panics| panics.borrow_mut().push(format!("{info}")));
            if let Some(previous) = chained.lock().expect("#1114 panic hook lock").as_ref() {
                previous(info);
            }
        })
    }

    /// Install over an explicit previous hook. The restore verification
    /// swaps through this so its rounds never hold a `take_hook`→`set_hook`
    /// gap with the default hook active while the probe storm runs.
    fn over(previous: PanicHook) -> Self {
        let previous = Arc::new(Mutex::new(Some(previous)));
        std::panic::set_hook(Self::hook(previous.clone()));
        Self { previous }
    }

    fn install() -> Self {
        Self::over(std::panic::take_hook())
    }

    /// #1114 fix-r2: take the previous hook out in its own statement, so
    /// the `MutexGuard` is released before `set_hook`. The old `if let`
    /// shape kept the guard alive through the branch body: a concurrently
    /// panicking thread runs the chained hook while std holds the global
    /// `HOOK` read lock (panicking.rs `rust_panic_with_hook`) and waits on
    /// the same mutex, while `set_hook` needs the `HOOK` write lock — a
    /// lock-order inversion that hangs the whole test binary.
    fn restore(&self) {
        let restore = self.previous.lock().expect("#1114 panic hook lock").take();
        if let Some(previous) = restore {
            std::panic::set_hook(previous);
        }
    }
}

/// #1114 fix-r1: the same contract at a real detached site. A legacy
/// transport with no endpoint rejects the send before its first suspension,
/// so `send_detached`'s continuation is Ready on the inline poll; spawning
/// it re-polls the finished async block and panics
/// ("`async fn` resumed after completion").
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
    let recorder = PanicRecorder::install();
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
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let panics = DETACHED_PANICS.with(|panics| std::mem::take(&mut *panics.borrow_mut()));
    if !panics.is_empty() {
        println!("{tag}: recorded panics: {panics:?}");
    }
    // Restored before the assertions: a failure must panic through the
    // normal hook, never wait on the recorder's mutex.
    recorder.restore();
    crate::negative_assert!(
        panics.is_empty(),
        "#1114 a first-poll-Ready detached continuation is not spawned and polled again"
    );
    crate::negative_assert_eq!(
        super::detached_dispatches(),
        2,
        "#1114 both Ready sends were counted"
    );
}

/// This scenario's probe payload: the filter hook below keeps the storm's
/// panics out of the captured output without silencing any other panic.
const HOOK_RESTORE_PROBE: &str = "#1114 hook-restore probe";
const HOOK_RESTORE_ASSERTION: &str =
    "#1114 restoring the panic hook completes while another thread panics";
/// Marks the re-executed child that runs the raw interleaving.
const HOOK_RESTORE_CHILD_ENV: &str = "PDRS_1114_HOOK_RESTORE_CHILD";
const HOOK_RESTORE_CHILD_TEST: &str = "runtime::mcp::http::encoding_tests::\
dispatch_panic_hook_restore_survives_concurrent_panics";

/// The raw interleaving, run in the re-executed child: a thread panics
/// continuously while the recorder is installed and restored many times.
/// On fixed code every loop here is count-bounded and the joins return; the
/// pre-fix restore wedges the process here (holding the recorder's mutex
/// across `set_hook` while a probe panic sits in the chained hook holding
/// the global `HOOK` read lock), which is exactly why the caller keeps it
/// in a child it can kill.
fn hook_restore_storm() {
    let tag = "#1114 panic hook restore child";
    // A filter hook below the storm: the default hook would print (and
    // backtrace) every probe panic. It forwards everything but this
    // scenario's own probe payload.
    let original = Arc::new(std::panic::take_hook());
    let filter = move || {
        let original = original.clone();
        Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
            if info.payload().downcast_ref::<&'static str>() != Some(&HOOK_RESTORE_PROBE) {
                original(info);
            }
        }) as Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>
    };
    std::panic::set_hook(filter());
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peer = {
        let probes = probes.clone();
        std::thread::spawn(move || {
            // Bounded storm: the cap keeps a pathological scheduler from
            // outliving the caller's bound.
            for seen in 0..300_000usize {
                let _ = std::panic::catch_unwind(|| std::panic::panic_any(HOOK_RESTORE_PROBE));
                probes.store(seen + 1, std::sync::atomic::Ordering::SeqCst);
                if seen % 512 == 0 {
                    // The recorder keeps every panic of its thread; keep the
                    // probe thread's record bounded.
                    DETACHED_PANICS.with(|panics| panics.borrow_mut().clear());
                }
            }
        })
    };
    // Storm hot before any swap runs: without this barrier the swaps can
    // finish before the first probe panic.
    while probes.load(std::sync::atomic::Ordering::SeqCst) < 4_096 {
        std::hint::spin_loop();
    }
    let swapping = std::thread::spawn(move || {
        for _ in 0..100_000 {
            // A fresh filter box per round, installed without `take_hook`:
            // the default hook must never be the one a storm panic reaches.
            let recorder = PanicRecorder::over(filter());
            recorder.restore();
        }
    });
    swapping
        .join()
        .unwrap_or_else(|_| panic!("{tag}: swapping thread panicked"));
    peer.join()
        .unwrap_or_else(|_| panic!("{tag}: probe thread panicked"));
    assert!(
        probes.load(std::sync::atomic::Ordering::SeqCst) >= 4_096,
        "{tag}: the concurrent panic storm ran"
    );
}

/// #1114 fix-r2: `PanicRecorder::restore` must not hold the recorder's mutex
/// across `set_hook`. Std runs a panic hook with the global `HOOK` read lock
/// held (panicking.rs `rust_panic_with_hook`), and the recorder's chained
/// hook waits on that same mutex, so the pre-fix shape — the `MutexGuard` of
/// an `if let` scrutinee alive through the branch — made `set_hook` wait for
/// the `HOOK` write lock with the mutex held. Any parallel test that panics
/// (`registry.rs` `shared_spawn_outcomes_and_cancelled_gate` panics by
/// design) then hangs the whole test binary.
///
/// The interleaving runs in a re-executed child (the #946 worker pattern)
/// because its failure mode is that wedge: a hung child is killed here and
/// becomes this test's named failure, while the parent — which never touches
/// the process's panic hook itself — always returns. The failure is a
/// returned `Err` with the receipt written first: a panic through the
/// recorder would be exactly the wait the regression causes.
#[tokio::test]
async fn dispatch_panic_hook_restore_survives_concurrent_panics() -> Result<(), String> {
    let tag = "#1114 panic hook restore";
    if std::env::var_os(HOOK_RESTORE_CHILD_ENV).is_some() {
        hook_restore_storm();
        return Ok(());
    }
    let mut child = tokio::process::Command::new(
        std::env::current_exe().unwrap_or_else(|error| panic!("{tag}: test executable: {error}")),
    )
    .arg(HOOK_RESTORE_CHILD_TEST)
    .arg("--exact")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .env(HOOK_RESTORE_CHILD_ENV, "1")
    // Only this test's own failure is the kill; the child runs the plain
    // interleaving and must not write this run's receipts.
    .env_remove("PDRS_NEGATIVE_RECEIPTS")
    .env_remove("PDRS_NEGATIVE_PARENT_TEST")
    .spawn()
    .unwrap_or_else(|error| panic!("{tag}: child spawn: {error}"));
    let survived = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(error)) => panic!("{tag}: child wait: {error}"),
        Err(_) => {
            // Wedged the way the pre-fix restore does. Kill and fail; the
            // bound, not the wedge, ends the regression.
            child
                .kill()
                .await
                .unwrap_or_else(|error| panic!("{tag}: child kill: {error}"));
            let _ = child.wait().await;
            false
        }
    };
    if !survived {
        crate::test_negative_receipt::record_failure(HOOK_RESTORE_ASSERTION);
        return Err(format!("{tag}: {HOOK_RESTORE_ASSERTION}"));
    }
    Ok(())
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
