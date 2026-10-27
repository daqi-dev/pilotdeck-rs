//! #886 real HTTP replays of the actual Node v22.22.0 SDK transport corpus.
use super::*;
use crate::runtime::mcp::protocol::ResultSchema;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
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
