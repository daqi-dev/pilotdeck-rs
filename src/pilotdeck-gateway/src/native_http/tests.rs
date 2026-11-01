//! #1209: byte-level replay of the `native-http-basic` golden fixture (every
//! cell one test), the registered transport-diff pins, and the route-level
//! checks the compile-time negative controls bind to. The default proxy
//! routing is untouched and covered by its own tests; only the native
//! listener's non-upgrade branch routes here.
use super::*;
use crate::{
    http_proxy,
    runtime::{
        connection::{Call, RequestError},
        turn::gateway::{self, GatewayRequests},
    },
    test_baseline,
};
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::Value;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};

const ORACLE: &str = include_str!("../../tests/fixtures/native-http-basic/oracle.json");
const DEADLINE: Duration = Duration::from_secs(5);
/// The same quiet window the TS recorder used: a response is complete after a
/// quarter second of silence while the connection stays open.
const QUIET: Duration = Duration::from_millis(250);

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1209 oracle JSON")
}

struct Cell {
    id: String,
    server: String,
    request: Vec<u8>,
    response: String,
    closed: bool,
}

fn cell(id: &str) -> Cell {
    let value = oracle();
    let cells = value["cells"].as_array().expect("#1209 oracle cells array");
    for entry in cells {
        if entry["id"].as_str() == Some(id) {
            return Cell {
                id: id.to_owned(),
                server: entry["server"]
                    .as_str()
                    .expect("#1209 oracle cell server")
                    .to_owned(),
                request: latin1_bytes(
                    entry["request"]
                        .as_str()
                        .expect("#1209 oracle cell request"),
                ),
                response: entry["response"]
                    .as_str()
                    .expect("#1209 oracle cell response")
                    .to_owned(),
                closed: entry["closed"]
                    .as_bool()
                    .expect("#1209 oracle cell closed flag"),
            };
        }
    }
    panic!("#1209 cell {id} is missing from the committed oracle");
}

/// The two deterministic static roots of the fixture generator, contents never
/// changing between runs.
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    root2: PathBuf,
}

fn roots() -> Fixture {
    let parent = std::env::temp_dir().join(format!("pdrs-1209-{}", uuid::Uuid::new_v4()));
    let root = parent.join("root");
    let root2 = parent.join("root2");
    std::fs::create_dir_all(root.join("sub")).expect("#1209 fixture root directory");
    std::fs::create_dir_all(&root2).expect("#1209 fixture root2 directory");
    let files = [
        (
            root.join("index.html"),
            "<!doctype html><html>index</html>\n",
        ),
        (root.join("app.js"), "console.log(1)\n"),
        (root.join("style.css"), "body{}\n"),
        (root.join("data.json"), "{\"a\":1}\n"),
        (root.join("logo.svg"), "<svg/>\n"),
        (root.join("bin.bin"), "0,1,2,255,254\n"),
        (root.join("noext"), "no extension\n"),
        (root.join("upper.TXT"), "upper\n"),
        (root.join("sub").join("page.html"), "sub page\n"),
        (root.join("enc%20oded"), "encoded-name\n"),
        (root2.join("only.txt"), "only file\n"),
    ];
    for (path, content) in files {
        std::fs::write(path, content).expect("#1209 fixture static file");
    }
    Fixture {
        parent,
        root,
        root2,
    }
}

/// The routing config of one fixture server: `c` is the TS non-loopback case
/// (the recorder bound 127.0.0.2, but the address is not part of any golden
/// bytes, so the replay binds 127.0.0.1 everywhere and only the routing
/// config carries `loopback: false`), `b` serves the index-less root, `d`
/// has no static root.
fn config_for(server: &str, fixture: &Fixture) -> Config {
    let static_root = match server {
        "a" | "c" => Some(fixture.root.clone()),
        "b" => Some(fixture.root2.clone()),
        _ => None,
    };
    Config {
        token: "fixture-token".into(),
        loopback: server != "c",
        static_root,
    }
}

fn listener_address(_server: &str) -> &str {
    "127.0.0.1:0"
}

/// Bind one native listener with the cell's routing config and write the raw
/// request bytes to it, then capture the raw response bytes like the TS
/// recorder did (quiet window, EOF, or socket error ends the capture).
async fn exchange(server: &str, request: &[u8], id: &str, fixture: &Fixture) -> (String, bool) {
    let address = listener_address(server);
    let listener = TcpListener::bind(address)
        .await
        .expect("#1209 bind listener");
    let config = config_for(server, fixture);
    let bound = listener.local_addr().expect("#1209 listener local address");
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("#1209 accept connection");
            let config = config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = tokio::sync::watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |request, _lease| {
                        let config = config.clone();
                        async move { serve(&request, &config).await }
                    },
                    stop,
                )
                .await;
                if let Err(error) = served {
                    // A handler Err (CONNECT) is a deliberate zero-byte close.
                    eprintln!("#1209 native connection error: {error}");
                }
            });
        }
    });
    let mut stream = timeout(DEADLINE, TcpStream::connect(bound))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to native listener");
    stream
        .write_all(request)
        .await
        .expect("#1209 write request bytes");
    let captured = capture(&mut stream, id).await;
    server_task.abort();
    captured
}

async fn capture(stream: &mut TcpStream, id: &str) -> (String, bool) {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 8192];
    let start = Instant::now();
    let mut closed = false;
    while start.elapsed() < DEADLINE {
        let wait = if out.is_empty() {
            DEADLINE - start.elapsed()
        } else {
            QUIET
        };
        match timeout(wait, stream.read(&mut buffer)).await {
            Err(_elapsed) if !out.is_empty() => break,
            Err(_elapsed) => continue,
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(read)) => out.extend_from_slice(&buffer[..read]),
            // The TS recorder ignored socket errors and kept the bytes that
            // had arrived, so a reset while the response drains is a close.
            Ok(Err(_reset)) => {
                closed = true;
                break;
            }
        }
    }
    assert!(
        !out.is_empty() || closed,
        "#1209 cell {id} produced no bytes and stayed open"
    );
    (latin1_string(&out), closed)
}

/// One cell through the fixture replay: capture the raw response and compare
/// the normalized bytes and the closed flag against the golden oracle.
async fn replay(id: &str) {
    let cell = cell(id);
    let fixture = roots();
    let (raw, closed) = exchange(&cell.server, &cell.request, &cell.id, &fixture).await;
    assert_eq!(
        normalize(&raw),
        normalize(&cell.response),
        "#1209 cell {} normalized response",
        cell.id
    );
    assert_eq!(
        closed, cell.closed,
        "#1209 cell {} connection closed flag",
        cell.id
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

macro_rules! replay_tests {
    ($($function:ident => $id:literal),* $(,)?) => {
        /// Every cell id the macro instantiates; a test asserts this is exactly
        /// the oracle's cell set, so neither list can drift alone.
        const REPLAY_CELL_IDS: &[&str] = &[$($id),*];
        $(
            #[tokio::test]
            async fn $function() {
                replay($id).await;
            }
        )*
    };
}

replay_tests! {
    health_get => "health-get",
    health_head => "health-head",
    health_post => "health-post",
    health_put => "health-put",
    health_delete => "health-delete",
    health_options => "health-options",
    health_trace => "health-trace",
    health_patch => "health-patch",
    health_propfind => "health-propfind",
    health_mkcol => "health-mkcol",
    health_msearch => "health-msearch",
    health_purge => "health-purge",
    health_search => "health-search",
    health_acl => "health-acl",
    health_report => "health-report",
    health_mkcalendar => "health-mkcalendar",
    health_query => "health-query",
    health_fragment => "health-fragment",
    health_dot => "health-dot",
    health_dotdot => "health-dotdot",
    health_pct_dotdot => "health-pct-dotdot",
    health_pct_upper => "health-pct-upper",
    health_backslash => "health-backslash",
    health_dblslash => "health-dblslash",
    health_absolute => "health-absolute",
    health_upper_path => "health-upper-path",
    health_trailing => "health-trailing",
    health_conn_close => "health-conn-close",
    head_conn_close => "head-conn-close",
    token_get => "token-get",
    token_head => "token-head",
    token_post => "token-post",
    token_query => "token-query",
    token_authorization => "token-authorization",
    token_trailing => "token-trailing",
    token_localhost => "token-localhost",
    token_remote => "token-remote",
    health_remote => "health-remote",
    static_remote => "static-remote",
    static_index => "static-index",
    static_index_head => "static-index-head",
    static_appjs => "static-appjs",
    static_appjs_query => "static-appjs-query",
    static_css => "static-css",
    static_json => "static-json",
    static_svg => "static-svg",
    static_bin => "static-bin",
    static_noext => "static-noext",
    static_upper => "static-upper",
    static_upper_lower => "static-upper-lower",
    static_sub => "static-sub",
    static_dir => "static-dir",
    static_missing => "static-missing",
    static_encoded => "static-encoded",
    static_fragment => "static-fragment",
    static_absolute_dblslash => "static-absolute-dblslash",
    traversal_parent => "traversal-parent",
    traversal_encoded => "traversal-encoded",
    traversal_pct => "traversal-pct",
    traversal_backslash => "traversal-backslash",
    traversal_pct_appjs => "traversal-pct-appjs",
    traversal_backslash_appjs => "traversal-backslash-appjs",
    traversal_dotsegments => "traversal-dotsegments",
    traversal_null => "traversal-null",
    root2_missing => "root2-missing",
    root2_index => "root2-index",
    root2_only => "root2-only",
    root2_only_head => "root2-only-head",
    root2_health => "root2-health",
    root2_token => "root2-token",
    nostatic_missing => "nostatic-missing",
    nostatic_index => "nostatic-index",
    bad_method_foo => "bad-method-foo",
    bad_method_get => "bad-method-get",
    bad_method_getx => "bad-method-getx",
    non_ascii_path => "non-ascii-path",
    non_ascii_query => "non-ascii-query",
    no_host => "no-host",
    garbage => "garbage",
    pct_target => "pct-target",
    authority_form => "authority-form",
    abs_scheme_nonalpha => "abs-scheme-nonalpha",
    empty_request => "empty-request",
    star => "star",
    connect => "connect",
    connect_nonascii => "connect-nonascii",
    pipeline => "pipeline",
    unread_body_pipeline => "unread-body-pipeline",
}

/// #1209: the replay list and the oracle describe the same cell set; a cell
/// added to the oracle without a replay test (or vice versa) fails here.
#[test]
fn replay_cells_cover_exactly_the_oracle() {
    let recorded = oracle();
    let mut oracle_ids: Vec<&str> = recorded["cells"]
        .as_array()
        .expect("#1209 oracle cells array")
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1209 oracle cell id"))
        .collect();
    oracle_ids.sort_unstable();
    let mut replayed: Vec<&str> = REPLAY_CELL_IDS.to_vec();
    replayed.sort_unstable();
    assert_eq!(
        replayed, oracle_ids,
        "#1209 replay cell ids and oracle cell ids are the same set"
    );
}

/// #1209: the method allowlist is the recorded llhttp table (`http.METHODS`
/// on the pinned Node), not an independent transcription.
#[test]
fn llhttp_methods_match_the_oracle() {
    let value = oracle();
    let recorded = value["methods"]
        .as_array()
        .expect("#1209 oracle methods array")
        .iter()
        .map(|method| method.as_str().expect("#1209 oracle method entry"))
        .collect::<Vec<_>>();
    assert_eq!(
        LLHTTP_METHODS.as_slice(),
        recorded.as_slice(),
        "#1209 llhttp method table equals the recorded http.METHODS"
    );
}

/// The bytes both sides are compared in: the Date value is masked, header
/// names are case-folded (hyper renders Title-Case, TS's `content-type` is
/// literal lowercase — the registered transport diff), the bare 400's
/// `content-length: 0` line is dropped (registered transport diff), and
/// chunk sizes are case-folded (hyper writes `B`, Node writes `b` —
/// registered transport diff). Status lines are compared verbatim — both
/// stacks write canonical `HTTP/1.1 200 OK` casing — and chunk bodies are
/// never folded.
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    'responses: while !rest.is_empty() {
        let Some(head_end) = rest.find("\r\n\r\n") else {
            out.push_str(rest);
            break;
        };
        let head = &rest[..head_end];
        let mut chunked = false;
        let mut length = None;
        for line in head.split("\r\n").skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if name == "content-length" {
                length = value.trim().parse::<usize>().ok();
            }
        }
        for (index, line) in head.split("\r\n").enumerate() {
            if index == 0 {
                out.push_str(line);
                out.push_str("\r\n");
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                out.push_str(line);
                out.push_str("\r\n");
                continue;
            };
            if name.eq_ignore_ascii_case("date") {
                out.push_str("date: <masked>\r\n");
            } else if name.eq_ignore_ascii_case("content-length") && value.trim() == "0" {
                // hyper's implicit empty-body length on bare 400s.
            } else {
                out.push_str(&name.to_ascii_lowercase());
                out.push(':');
                out.push_str(value);
                out.push_str("\r\n");
            }
        }
        out.push_str("\r\n");
        rest = &rest[head_end + 4..];
        if chunked {
            loop {
                let Some(line_end) = rest.find("\r\n") else {
                    out.push_str(rest);
                    break 'responses;
                };
                let size_text = rest[..line_end].split(';').next().unwrap_or("");
                let size = usize::from_str_radix(size_text.trim(), 16)
                    .unwrap_or_else(|_| panic!("#1209 chunk size {size_text:?} is not hex"));
                out.push_str(&size_text.to_ascii_lowercase());
                out.push_str("\r\n");
                rest = &rest[line_end + 2..];
                if rest.len() < size + 2 {
                    out.push_str(rest);
                    break 'responses;
                }
                out.push_str(&rest[..size]);
                out.push_str("\r\n");
                rest = &rest[size + 2..];
                if size == 0 {
                    continue 'responses;
                }
            }
        }
        match length {
            Some(size) if rest.len() >= size => {
                out.push_str(&rest[..size]);
                rest = &rest[size..];
                continue 'responses;
            }
            _ => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| {
            u8::try_from(u32::from(character))
                .unwrap_or_else(|_| panic!("#1209 oracle char {character:?} is outside latin1"))
        })
        .collect()
}

fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

fn request(method: &str, target: &str) -> hyper::Request<()> {
    hyper::Request::builder()
        .method(method)
        .uri(target)
        .header("host", "127.0.0.1")
        .body(())
        .unwrap_or_else(|error| panic!("#1209 probe request {method} {target}: {error}"))
}

async fn route(method: &str, target: &str, config: &Config) -> hyper::Response<Body> {
    serve(&request(method, target), config)
        .await
        .expect("#1209 native route served")
}

async fn body_of(response: hyper::Response<Body>) -> String {
    latin1_string(
        &response
            .into_body()
            .collect()
            .await
            .expect("#1209 response body")
            .to_bytes(),
    )
}

fn content_type_of(response: &hyper::Response<Body>) -> String {
    response
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .expect("#1209 content-type header")
        .to_str()
        .expect("#1209 content-type is ASCII")
        .to_owned()
}

/// #1209: a non-loopback binding must not leak the token (TS 404s it before
/// the static fallback); the loopback binding returns it verbatim.
#[tokio::test]
async fn local_token_requires_loopback_binding() {
    let mut remote = config_for("c", &roots());
    remote.static_root = None;
    let response = route("GET", "/auth/local-token", &remote).await;
    assert_eq!(
        u16::from(response.status()),
        404,
        "#1209 non-loopback local-token is a 404"
    );
    assert_eq!(
        body_of(response).await,
        "not found",
        "#1209 non-loopback local-token body"
    );
    let local = Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
    };
    let response = route("GET", "/auth/local-token", &local).await;
    assert_eq!(
        u16::from(response.status()),
        200,
        "#1209 loopback local-token is a 200"
    );
    assert_eq!(
        body_of(response).await,
        "{\"token\":\"fixture-token\"}",
        "#1209 loopback local-token returns the token"
    );
}

/// #1209: URL normalization collapses the traversal before the static root;
/// the escaped candidate never names a file outside the root.
#[tokio::test]
async fn traversal_collapses_before_the_static_root() {
    let fixture = roots();
    let config = config_for("a", &fixture);
    let response = route("GET", "/sub/../../app.js", &config).await;
    assert_eq!(
        u16::from(response.status()),
        200,
        "#1209 traversal normalizes to the target file"
    );
    assert_eq!(
        content_type_of(&response),
        "text/javascript; charset=utf-8",
        "#1209 traversal serves the app.js content type"
    );
    assert_eq!(
        body_of(response).await,
        "console.log(1)\n",
        "#1209 traversal serves app.js bytes"
    );
}

/// #1209: the containment guard behind the URL layer. WHATWG normalization
/// makes `..` unreachable from a real request, so this is defense in depth
/// against a normalization regression, tested directly.
#[tokio::test]
async fn static_guard_rejects_escape() {
    let fixture = roots();
    let escaped = serve_static(&fixture.root, "/../outside.html", true).await;
    assert!(escaped.is_none(), "#1209 containment guard rejects escape");
    let served = serve_static(&fixture.root, "/app.js", true).await;
    assert!(served.is_some(), "#1209 contained path still serves");
}

/// #1209: the final 404's exact status, content type, and body bytes.
#[tokio::test]
async fn not_found_body_matches_ts() {
    let config = config_for("d", &roots());
    let response = route("GET", "/missing", &config).await;
    assert_eq!(
        u16::from(response.status()),
        404,
        "#1209 unmatched route is a 404"
    );
    assert_eq!(
        content_type_of(&response),
        "text/plain; charset=utf-8",
        "#1209 404 content type"
    );
    assert_eq!(body_of(response).await, "not found", "#1209 404 body bytes");
}

/// #1209: an authority-form target without CONNECT is llhttp's bare 400;
/// http::Uri accepts it and httparse would dispatch it, so the router's own
/// target-form rule is the only rejection on this path (unlike `%2Fhealth`,
/// which hyper's URI parser already refuses before the handler).
#[tokio::test]
async fn authority_form_target_rejected() {
    let config = config_for("d", &roots());
    let response = route("GET", "127.0.0.1:1", &config).await;
    assert_eq!(
        u16::from(response.status()),
        400,
        "#1209 authority-form target is a bare 400"
    );
    assert_eq!(
        response
            .headers()
            .get(hyper::header::CONNECTION)
            .and_then(|value| value.to_str().ok()),
        Some("close"),
        "#1209 authority-form target closes the connection"
    );
}

/// #1209: a method outside the llhttp table is a bare parser-level 400 that
/// closes the connection; httparse would dispatch it to the router.
#[tokio::test]
async fn method_outside_llhttp_table_rejected() {
    let config = config_for("d", &roots());
    let response = route("FOO", "/health", &config).await;
    assert_eq!(
        u16::from(response.status()),
        400,
        "#1209 unknown method is a bare 400"
    );
    assert_eq!(
        response
            .headers()
            .get(hyper::header::CONNECTION)
            .and_then(|value| value.to_str().ok()),
        Some("close"),
        "#1209 unknown method closes the connection"
    );
}

/// #1209: hyper's actual bytes on the registered transport diffs, so the
/// replay normalization cannot hide a diff that is not in the ledger:
/// Title-Case header names, uppercase chunk-size hex, and the bare 400's
/// `content-length: 0`.
#[tokio::test]
async fn registered_transport_diffs_stay_pinned() {
    let fixture = roots();
    let health = exchange(
        "a",
        &latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        "pin-health",
        &fixture,
    )
    .await
    .0;
    assert!(
        health.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nDate: "),
        "#1209 pin: native responses render Title-Case header names"
    );
    assert!(
        health.ends_with("\r\n\r\nB\r\n{\"ok\":true}\r\n0\r\n\r\n"),
        "#1209 pin: hyper renders uppercase chunk sizes"
    );
    let bare = exchange(
        "a",
        &latin1_bytes("FOO /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        "pin-bare",
        &fixture,
    )
    .await
    .0;
    assert!(
        bare.contains("\r\nContent-Length: 0\r\n\r\n"),
        "#1209 pin: the bare 400 carries content-length: 0"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: raw-byte pins the replay normalization cannot swallow: the bare
/// parser-level 400s (FOO, garbage) carry no Date (only the missing-Host 400
/// does) and close the connection, and a HEAD response carries neither
/// Content-Length nor Transfer-Encoding (Node omits both rather than
/// describing a body it will not write).
#[tokio::test]
async fn rejection_and_head_bytes_stay_pinned() {
    let fixture = roots();
    for id in ["pin-foo", "pin-garbage"] {
        let request = if id == "pin-foo" {
            "FOO /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
        } else {
            "GARBAGE\r\n\r\n"
        };
        let (bare, closed) = exchange("a", &latin1_bytes(request), id, &fixture).await;
        assert!(
            bare.starts_with("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n")
                && !bare.contains("\r\nDate:"),
            "#1209 pin {id}: the bare 400 has no Date: {bare:?}"
        );
        assert!(
            closed,
            "#1209 pin {id}: the bare 400 closes the connection like TS"
        );
    }
    let head = exchange(
        "a",
        &latin1_bytes("HEAD /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        "pin-head",
        &fixture,
    )
    .await
    .0;
    assert!(
        head.starts_with("HTTP/1.1 200 OK\r\n")
            && !head.contains("\r\n\r\n0")
            && !head.to_ascii_lowercase().contains("content-length")
            && !head.to_ascii_lowercase().contains("transfer-encoding"),
        "#1209 pin: the HEAD response has no body framing: {head:?}"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: `Connection: close` reaches every response family, not just
/// `/health`: the static file response and the final 404 are also answered
/// `Connection: close` with no Keep-Alive and a closed socket, like Node v22
/// (the `health-conn-close`/`head-conn-close` golden cells cover the handler
/// family; probe `probe-fixr1.out` A).
#[tokio::test]
async fn connection_close_reaches_every_response_family() {
    let fixture = roots();
    for (id, target) in [
        ("pin-static-close", "/app.js"),
        ("pin-404-close", "/missing"),
    ] {
        let (raw, closed) = exchange(
            "a",
            &latin1_bytes(&format!(
                "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )),
            id,
            &fixture,
        )
        .await;
        assert!(
            raw.contains("\r\nConnection: close\r\n")
                && !raw.to_ascii_lowercase().contains("keep-alive"),
            "#1209 pin {id}: the close request is answered close with no keep-alive: {raw:?}"
        );
        assert!(
            closed,
            "#1209 pin {id}: the close request closes the connection like TS"
        );
    }
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: HTTP/1.0 (and any close request) is answered `Connection: close`
/// without a Keep-Alive header, like Node v22 — including an HTTP/1.0 request
/// that asks for keep-alive (probe `probe-fixr1.out` A `1.0+keep`); the body
/// is close-delimited and the socket closes. hyper writes a `HTTP/1.0` status
/// line where Node writes `HTTP/1.1` — a registered transport diff, which is
/// why this is a raw pin and not a golden cell.
#[tokio::test]
async fn http10_close_delimited_response() {
    let fixture = roots();
    for (id, connection) in [
        ("pin-http10", ""),
        ("pin-http10-keep", "Connection: keep-alive\r\n"),
    ] {
        let (raw, closed) = exchange(
            "a",
            &latin1_bytes(&format!(
                "GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n{connection}\r\n"
            )),
            id,
            &fixture,
        )
        .await;
        assert!(
            raw.starts_with("HTTP/1.0 200 OK\r\n"),
            "#1209 pin {id}: HTTP/1.0 is answered with a 1.0 status line (registered diff vs Node's 1.1 line): {raw:?}"
        );
        assert!(
            raw.contains("\r\nConnection: close\r\n")
                && !raw.to_ascii_lowercase().contains("keep-alive"),
            "#1209 pin {id}: HTTP/1.0 is answered close with no keep-alive: {raw:?}"
        );
        assert!(
            raw.ends_with("{\"ok\":true}"),
            "#1209 pin {id}: the HTTP/1.0 body is close-delimited, not chunked: {raw:?}"
        );
        assert!(
            closed,
            "#1209 pin {id}: the HTTP/1.0 response closes the connection like TS"
        );
    }
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

struct Unused;
impl GatewayRequests for Unused {
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Ok(JsValue::Null) })
    }
    fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async { Ok(JsValue::Null) })
    }
}

/// #1209: the native listener's non-upgrade branch reaches the same router
/// the replay tests exercise, on the real accept loop, including the
/// opt-in static root.
#[tokio::test]
async fn native_listener_delegates_plain_http_to_the_router() {
    let fixture = roots();
    let host = gateway::tests::native_ws_fixture(Arc::new(Unused));
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1209 native listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1209 bind native listener")
    .with_static_root(fixture.root.clone());
    let address = server
        .local_addr()
        .expect("#1209 native listener local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        DEADLINE,
    ));

    let health = cell("health-get");
    let (raw, closed) = exchange_on(address, &health.request, &health.id).await;
    assert_eq!(
        normalize(&raw),
        normalize(&health.response),
        "#1209 native listener health cell matches TS"
    );
    assert!(!closed, "#1209 native listener keeps the health connection");

    let appjs = cell("static-appjs");
    let (raw, closed) = exchange_on(address, &appjs.request, &appjs.id).await;
    assert_eq!(
        normalize(&raw),
        normalize(&appjs.response),
        "#1209 native listener static cell matches TS"
    );
    assert!(!closed, "#1209 native listener keeps the static connection");

    stop.send(()).expect("#1209 stop native listener");
    timeout(DEADLINE, task)
        .await
        .expect("#1209 native listener joins")
        .expect("#1209 native listener task")
        .expect("#1209 native listener drain");
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// The `native_listener` variant of [`exchange`] against an already-bound
/// address.
async fn exchange_on(address: SocketAddr, request: &[u8], id: &str) -> (String, bool) {
    let mut stream = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the bound listener");
    stream
        .write_all(request)
        .await
        .expect("#1209 write request bytes");
    let captured = capture(&mut stream, id).await;
    let _ = stream.shutdown().await;
    captured
}

/// #1209: a trailing-separator static root serves after the same lexical
/// fold TS's `resolve(staticRoot)` performs; without the fold the containment
/// guard's `root + sep` prefix never matches the joined candidate and every
/// path 404s.
#[tokio::test]
async fn trailing_separator_root_still_serves() {
    let fixture = roots();
    let config = Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: Some(PathBuf::from(format!("{}/", fixture.root.display()))),
    };
    let response = route("GET", "/app.js", &config).await;
    assert_eq!(
        u16::from(response.status()),
        200,
        "#1209 trailing-separator root serves after normalization"
    );
    assert_eq!(
        body_of(response).await,
        "console.log(1)\n",
        "#1209 trailing-separator root serves the app.js bytes"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: Node v22.22.0 has no header-count limit — the binding constraint is
/// the 16384-byte URL+header span budget (probe `probe-fixr2.out` A serves
/// 2049/2500/8000 `a: b` headers, 16020 tracked bytes at the top end); the
/// native builder's count cap must never bind before that budget.
#[tokio::test]
async fn header_counts_within_ts_bounds_stay_served() {
    let fixture = roots();
    for count in [2049, 2500] {
        let mut request = String::from("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        for _ in 0..count {
            request.push_str("a: b\r\n");
        }
        request.push_str("\r\n");
        let (raw, _) = exchange("a", request.as_bytes(), "pin-headers-count", &fixture).await;
        assert!(
            raw.starts_with("HTTP/1.1 200 OK\r\n"),
            "#1209 {count} small headers stay served like TS: {raw:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: Node v22.22.0's actual size limit, replicated at the router: the
/// current message's URL span plus every header-name and header-value span
/// accumulate (separators and CRLF excluded), and 16384 is the canned 431
/// with `Connection: close` and no Date (probe `probe-fixr2.out` A/B/C and
/// `probe-fixr2-b2.out`: single header 16300 tracked 16321 → 200, 16384
/// tracked 16405 → 431, the exact boundary value-16362/16363; URL
/// 16350 tracked 16373 → 200, 20 KiB → 431). The 431's `content-length: 0`
/// is the registered transport diff of the bare-400 family.
#[tokio::test]
async fn header_budget_431s_like_ts() {
    let fixture = roots();
    let single = |size: usize| {
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nx: {}\r\n\r\n",
            "a".repeat(size)
        )
    };
    let (raw, _) = exchange("a", single(16300).as_bytes(), "pin-budget-16300", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 a 16300-byte single header (tracked 16321) stays served like TS: {raw:?}"
    );
    for size in [16384, 20 * 1024] {
        let (raw, closed) =
            exchange("a", single(size).as_bytes(), "pin-budget-431", &fixture).await;
        assert!(
            raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
            "#1209 a {size}-byte single header is the TS 431: {raw:?}"
        );
        assert!(
            raw.contains("\r\nConnection: close\r\n") && !raw.contains("\r\nDate:"),
            "#1209 the {size}-byte 431 is the canned close response with no Date: {raw:?}"
        );
        assert!(
            closed,
            "#1209 the {size}-byte 431 closes the connection like TS"
        );
    }
    let url = |size: usize| {
        format!(
            "GET /health?x={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "a".repeat(size)
        )
    };
    let (raw, _) = exchange("a", url(16000).as_bytes(), "pin-budget-url-200", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 a 16000-byte URL (tracked 16023) stays served like TS: {raw:?}"
    );
    let (raw, closed) = exchange(
        "a",
        url(20 * 1024).as_bytes(),
        "pin-budget-url-431",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1209 a 20KiB URL is the TS 431: {raw:?}"
    );
    assert!(
        closed,
        "#1209 the URL-budget 431 closes the connection like TS"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: llhttp accepts `<` in the request target (probe
/// `probe-fixr1.out` C serves `/health?x=<`); hyper's URI parser rejects the
/// byte and the connection answers 400 — registered diff A8.4-NATIVE-HTTP-002,
/// pending approval.
#[tokio::test]
async fn angle_bracket_target_stays_pinned() {
    let fixture = roots();
    let (raw, _) = exchange(
        "a",
        &latin1_bytes("GET /health?x=< HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        "pin-angle-bracket",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 400"),
        "#1209 `<` in the query stays a hyper 400 (registered diff vs TS 200): {raw:?}"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: Node's server.close() closes idle keep-alive connections (probe
/// `probe-fixr1.out` B); stop must drain them instead of burning the whole
/// budget and reporting a deadline error.
#[tokio::test]
async fn stop_closes_idle_keep_alive_connections() {
    let host = gateway::tests::native_ws_fixture(Arc::new(Unused));
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1209 native listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1209 bind native listener");
    let address = server
        .local_addr()
        .expect("#1209 native listener local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(2),
    ));

    let mut idle = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    idle.write_all(&latin1_bytes(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    ))
    .await
    .expect("#1209 write the health request");
    let (response, closed) = capture(&mut idle, "stop-idle").await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 the idle connection served health first: {response:?}"
    );
    assert!(
        !closed,
        "#1209 the keep-alive connection stays open before stop"
    );

    stop.send(()).expect("#1209 stop native listener");
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the idle listener")
        .expect("#1209 stop task");
    assert!(
        drained.is_ok(),
        "#1209 stop closes idle keep-alive connections within the drain budget: {drained:?}"
    );
    let mut buffer = [0u8; 1];
    match timeout(QUIET, idle.read(&mut buffer)).await {
        Ok(Ok(0)) => {}
        other => panic!("#1209 the idle connection reached EOF after stop: {other:?}"),
    }
}

/// #1209: Node's server.close() does not close a connection that never sent
/// bytes (probe `probe-fixr1.out` B: close() resolved only once that peer
/// disconnected); the bounded runner surfaces the same wait as the drain
/// error instead of hanging forever.
#[tokio::test]
async fn stop_waits_for_never_requested_connections() {
    let host = gateway::tests::native_ws_fixture(Arc::new(Unused));
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1209 native listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1209 bind native listener");
    let address = server
        .local_addr()
        .expect("#1209 native listener local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
    ));

    let _silent = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    // Let the accept loop take the connection before stop, so the drain is
    // held by the connection task and not by the listener's backlog.
    tokio::time::sleep(Duration::from_millis(100)).await;

    stop.send(()).expect("#1209 stop native listener");
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the silent listener")
        .expect("#1209 stop task");
    assert!(
        drained.is_err(),
        "#1209 a never-requested connection keeps the drain open like TS server.close(): {drained:?}"
    );
}

/// #1209: Node's server.close() answers a request that arrives only after the
/// stop with the normal keep-alive response and never force-closes that
/// connection (probe `probe-fixr2.out` D1/D3: 200 + `Connection: keep-alive`,
/// the socket lives until its own idle timeout and close() resolves only
/// then). The runner surfaces that unbounded wait as the bounded drain error.
#[tokio::test]
async fn stop_serves_late_request_and_waits_like_ts_close() {
    let host = gateway::tests::native_ws_fixture(Arc::new(Unused));
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1209 native listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1209 bind native listener");
    let address = server
        .local_addr()
        .expect("#1209 native listener local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
    ));

    let mut late = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    // Let the accept loop take the connection before stop, so the drain is
    // held by the connection task and not by the listener's backlog.
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(()).expect("#1209 stop native listener");
    tokio::time::sleep(Duration::from_millis(100)).await;
    late.write_all(&latin1_bytes(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    ))
    .await
    .expect("#1209 write the late request");
    let (response, closed) = capture(&mut late, "stop-late").await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 the late request is still served after stop: {response:?}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("connection: keep-alive"),
        "#1209 the late response keeps the connection alive like TS server.close(): {response:?}"
    );
    assert!(
        !closed,
        "#1209 the late-request connection is not force-closed after the response, like TS server.close()"
    );
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the late-request listener")
        .expect("#1209 stop task");
    let error = drained.expect_err(
        "#1209 a connection that only requested after stop keeps the drain open like TS server.close()",
    );
    assert!(
        error.to_string().contains("drain"),
        "#1209 the drain deadline is the bounded equivalent of TS close() waiting: {error}"
    );
}

/// #1209: the same Node behavior for a request that was in flight when
/// server.close() arrived (probe `probe-fixr2.out` D2: the half-sent request
/// line is completed after the stop, answered 200 + keep-alive, never
/// force-closed); again the drain deadline is the bounded equivalent.
#[tokio::test]
async fn stop_serves_in_flight_request_and_waits_like_ts_close() {
    let host = gateway::tests::native_ws_fixture(Arc::new(Unused));
    let server = crate::native_ws::NativeWsServer::bind(
        "127.0.0.1:0".parse().expect("#1209 native listen address"),
        host,
        "fixture-token".into(),
        "probe".into(),
    )
    .await
    .expect("#1209 bind native listener");
    let address = server
        .local_addr()
        .expect("#1209 native listener local address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
    ));

    let mut inflight = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    inflight
        .write_all(&latin1_bytes("GET /heal"))
        .await
        .expect("#1209 write the partial request line");
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(()).expect("#1209 stop native listener");
    inflight
        .write_all(&latin1_bytes("th HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
        .await
        .expect("#1209 complete the request line");
    let (response, closed) = capture(&mut inflight, "stop-inflight").await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 the in-flight request is still served after stop: {response:?}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("connection: keep-alive"),
        "#1209 the in-flight response keeps the connection alive like TS server.close(): {response:?}"
    );
    assert!(
        !closed,
        "#1209 the in-flight connection is not force-closed after the response, like TS server.close()"
    );
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the in-flight listener")
        .expect("#1209 stop task");
    let error = drained.expect_err(
        "#1209 a connection whose request was in flight at stop keeps the drain open like TS server.close()",
    );
    assert!(
        error.to_string().contains("drain"),
        "#1209 the drain deadline is the bounded equivalent of TS close() waiting: {error}"
    );
}

/// #996-style regeneration guard for the fixture; see the fixture README for
/// the pinned Node invocation.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    test_baseline::regenerates_identically("native-http-basic");
}
