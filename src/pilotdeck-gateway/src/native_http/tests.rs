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
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
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
    let expected = expected_response(&cell, &fixture);
    assert_eq!(
        normalize(&raw),
        normalize(&expected),
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

/// #1359: the golden `static-upper-lower` cell (`GET /upper.txt` against a
/// fixture that only writes `upper.TXT`) is the one cell whose bytes depend
/// on the host filesystem: a case-sensitive filesystem misses the lowercase
/// lookup and TS falls back to the index (the recorded golden — the fixture
/// was recorded on Linux), while a case-insensitive one (macOS's default
/// APFS) resolves it to `upper.TXT` and TS serves the file. Measured on the
/// pinned baseline with Node v22.22.0: on a case-insensitive host the TS
/// response for `/upper.txt` is byte-identical to the recorded
/// `static-upper` golden (`_tmp/20261026-issue-1359/probe-macos.out`), so
/// Rust and TS agree on every host and only the golden bytes are
/// host-dependent. The expectation is therefore selected per host at
/// runtime, by probing the fixture root with the same lookup the router
/// performs — never a hard-coded cfg (registered in the parity ledger as
/// A8.4-NATIVE-HTTP-003). Every other cell's request path either matches a
/// fixture file exactly or has no case-variant file to collide with.
fn expected_response(replayed: &Cell, fixture: &Fixture) -> String {
    if replayed.id == "static-upper-lower" && serves_upper_txt(&fixture.root) {
        return cell("static-upper").response;
    }
    replayed.response.clone()
}

/// Whether the static root would serve `/upper.txt` by case-insensitive
/// resolution: the fixture wrote `upper.TXT`, and this is the same
/// `is_file(candidate)` lookup `serve_static` performs for that request, so
/// the probe decides with the exact evidence the router itself will use.
/// The `native-http-static-case-probe-inverted` negative-control switch
/// flips the answer, which must fail the `static-upper-lower` replay at its
/// own assertion on every host.
fn serves_upper_txt(root: &std::path::Path) -> bool {
    let lookup = root
        .join("upper.txt")
        .metadata()
        .is_ok_and(|metadata| metadata.is_file());
    if crate::mutants::on("native-http-static-case-probe-inverted") {
        !lookup
    } else {
        lookup
    }
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
/// 2049/2500/8000 `a: b` headers, 16020 tracked bytes at the top end; probe
/// `probe-fixr3-c.out` C4 serves 16363 one-byte-name empty-value headers at
/// tracked 16383 and 431s at 16364, tracked 16384); the native builder's count
/// cap must never bind before that budget, so it sits at 16384 headers, the
/// smallest value that cannot bind first.
#[tokio::test]
async fn header_counts_within_ts_bounds_stay_served() {
    let fixture = roots();
    for count in [2049, 2500, 8000] {
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
    for count in [16363, 16364] {
        let mut request = String::from("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        for _ in 0..count {
            request.push_str("a:\r\n");
        }
        request.push_str("\r\n");
        let (raw, _) = exchange("a", request.as_bytes(), "pin-headers-tiny", &fixture).await;
        if count == 16363 {
            assert!(
                raw.starts_with("HTTP/1.1 200 OK\r\n"),
                "#1209 16363 empty-value headers (tracked 16383) stay served like TS: {raw:?}"
            );
        } else {
            assert!(
                raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
                "#1209 16364 empty-value headers reach the byte budget like TS: {raw:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: Node v22.22.0's actual size limit, replicated at the router: the
/// current message's URL span plus every header-name and header-value span
/// accumulate (separators and CRLF excluded), and 16384 is the canned 431
/// with `Connection: close` and no Date (probes `probe-fixr2.out` A/B/C,
/// `probe-fixr2-b2.out` and `probe-fixr3-c.out` C1/C2/C3, all on the pinned
/// Node v22.22.0):
/// - single `x` value: tracked 21+size — 16362 → 16383 → 200, 16363 → 16384 → 431;
/// - trailing value OWS counts on the TS side (16350+13SP → 16384 → 431,
///   16350+12SP → 200), leading value OWS is skipped by both (13SP+16362 → 200);
/// - the header name counts in full (name-4+16360 → 16384 → 431, name-1+16360 → 200);
/// - the URL span counts like a header: `/health?x=` + size — 16360 → 16383 → 200,
///   16361 → 16384 → 431; 20 KiB → 431.
/// hyper trims a value's trailing OWS before the router runs, so the
/// trailing-OWS case under-counts here and stays 200 — the registered
/// TS-431/Rust-200 residual in ledger A8.4-NATIVE-HTTP-002. The absolute-form
/// empty-path target `http://h` is the reverse residual: Node's URL span is
/// the 8 bytes on the wire while the router re-renders `Uri` as `http://h/`,
/// so value-16361 lands tracked 16384 here (431) against TS's 404.
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
    let (raw, _) = exchange("a", single(16362).as_bytes(), "pin-budget-200", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 a 16362-byte single header (tracked 16383) stays served like TS: {raw:?}"
    );
    let (raw, closed) = exchange("a", single(16363).as_bytes(), "pin-budget-431", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1209 a 16363-byte single header (tracked 16384) is the TS 431: {raw:?}"
    );
    assert!(
        raw.contains("\r\nConnection: close\r\n") && !raw.contains("\r\nDate:"),
        "#1209 the boundary 431 is the canned close response with no Date: {raw:?}"
    );
    assert!(
        closed,
        "#1209 the boundary 431 closes the connection like TS"
    );
    // The 16384/20 KiB deep oversize cells live in their own test below.
    // Trailing value OWS counts on the TS side (16350+13 tracked 16384 → 431)
    // but hyper trims it, so the router under-counts and stays 200: the
    // registered residual, pinned here as the native behavior.
    let (raw, _) = exchange(
        "a",
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nx: {}{}\r\n\r\n",
            "a".repeat(16350),
            " ".repeat(13)
        )
        .as_bytes(),
        "pin-budget-owss",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 trailing value OWS is trimmed before the router counts it (registered residual vs TS 431): {raw:?}"
    );
    // Leading value OWS is skipped on both sides, so 13SP+16362 (tracked
    // 16383) stays served everywhere.
    let (raw, _) = exchange(
        "a",
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nx: {}{}\r\n\r\n",
            " ".repeat(13),
            "a".repeat(16362)
        )
        .as_bytes(),
        "pin-budget-owss-leading",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 leading value OWS is skipped like TS (tracked 16383): {raw:?}"
    );
    let names = |name: usize| {
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: {}\r\n\r\n",
            "x".repeat(name),
            "a".repeat(16360)
        )
    };
    let (raw, _) = exchange("a", names(1).as_bytes(), "pin-budget-name-1", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 a 1-byte name with a 16360-byte value (tracked 16381) stays served like TS: {raw:?}"
    );
    let (raw, _) = exchange("a", names(4).as_bytes(), "pin-budget-name-4", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1209 a 4-byte name with a 16360-byte value (tracked 16384) is the TS 431: {raw:?}"
    );
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
    let (raw, _) = exchange("a", url(16360).as_bytes(), "pin-budget-url-200", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 a 16360-byte URL (tracked 16383) stays served like TS: {raw:?}"
    );
    let (raw, closed) = exchange("a", url(16361).as_bytes(), "pin-budget-url-431", &fixture).await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1209 a 16361-byte URL (tracked 16384) is the TS 431: {raw:?}"
    );
    assert!(
        closed,
        "#1209 the URL-budget 431 closes the connection like TS"
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
    // The absolute-form empty-path target: Node's URL span is the 8 bytes on
    // the wire, so TS serves this (404 at tracked 16383, probe
    // `probe-fixr3-c3.out`), while the router re-renders `Uri` with the
    // implicit slash (9 bytes) and lands exactly on the budget — the
    // registered TS-404/Rust-431 residual.
    let (raw, _) = exchange(
        "a",
        format!(
            "GET http://h HTTP/1.1\r\nHost: 127.0.0.1\r\nx: {}\r\n\r\n",
            "a".repeat(16361)
        )
        .as_bytes(),
        "pin-budget-absolute",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1209 the re-rendered absolute-form URL span reaches the budget (registered residual vs TS 404): {raw:?}"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: the deep oversize cells of the same span budget — past the
/// inclusive 16384 boundary, a 16384-byte single `x` value (tracked 16405)
/// and a 20 KiB one stay 431 (probe `probe-fixr2-b2.out`). fix-r4 keeps them
/// in their own test: the `native-http-header-budget-off` mutant un-serves
/// the 16363 boundary cell too, so in the shared test its panic at that
/// boundary assertion would never reach this test's anchor.
#[tokio::test]
async fn header_budget_deep_oversize_431s_like_ts() {
    let fixture = roots();
    let single = |size: usize| {
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nx: {}\r\n\r\n",
            "a".repeat(size)
        )
    };
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

/// Wait for the accept loop to have taken a connection (its live-task
/// high-water mark), so a stop cannot race a peer that still sits in the
/// listener backlog.
async fn wait_accepted(accepted: &AtomicUsize, what: &str) {
    let started = Instant::now();
    while accepted.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < DEADLINE,
            "#1209 the {what} connection was never accepted by the native listener"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Wait for a watch flag the test's own handler sets.
async fn wait_flag(flag: &mut watch::Receiver<bool>, what: &str) {
    loop {
        if *flag.borrow_and_update() {
            return;
        }
        if flag.changed().await.is_err() {
            panic!("#1209 {what}");
        }
    }
}

/// Read the socket to EOF and return how long the close took after the
/// capture window: the native port closes at Node's effective keep-alive
/// idle deadline — `keepAliveTimeout` 5 s plus the fixed 1 s
/// `keepAliveTimeoutBuffer` (#1250) — and neither a stop nor a completed
/// response may close it earlier.
async fn eof_after_close(stream: &mut TcpStream, what: &str) -> Duration {
    let started = Instant::now();
    let mut buffer = [0u8; 1];
    match timeout(Duration::from_secs(7), stream.read(&mut buffer)).await {
        Ok(Ok(0)) => started.elapsed(),
        other => panic!("#1209 {what} reaches EOF at the keep-alive idle close: {other:?}"),
    }
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
/// disconnected); no request ever arrives, so the keep-alive idle close never
/// arms, and the bounded runner surfaces the same wait as the drain error
/// instead of hanging forever. The accepted-connection hook makes the stop
/// land after the accept loop has taken the peer.
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
    let accepted = server.accepted_connections();
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
    wait_accepted(&accepted, "silent").await;

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

/// #1209: Node's server.close() answers a request that arrives only after
/// the stop with the normal keep-alive response and never force-closes that
/// connection — the socket lives until its own keep-alive idle close, and
/// close() resolves once that ends it (probes `probe-fixr2.out` D1/D3 and
/// `probe-fixr3-a.out` D: 200 + `Connection: keep-alive`, EOF at the idle
/// timeout, close() resolving then). So `run()` must be `Ok(())` once the
/// idle close has ended the connection, and the client must read EOF at the
/// 5 s idle deadline, never right after the response.
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
    let accepted = server.accepted_connections();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        Duration::from_secs(8),
    ));

    let mut late = timeout(DEADLINE, TcpStream::connect(address))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    // The accept loop has taken the connection before the stop, so the
    // drain is held by the connection task and not by the backlog.
    wait_accepted(&accepted, "late").await;
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
    let elapsed = eof_after_close(&mut late, "late-request connection").await;
    assert!(
        elapsed >= Duration::from_secs(3),
        "#1209 the late-request connection is not force-closed after its response like TS server.close(): closed after {elapsed:?}"
    );
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the late-request listener")
        .expect("#1209 stop task");
    assert!(
        drained.is_ok(),
        "#1209 the late-request listener drains Ok once the idle close ends the connection: {drained:?}"
    );
}

/// #1209: the same Node behavior for a request that was genuinely in flight
/// — fully parsed and inside the handler — when server.close() arrived (probe
/// `probe-fixr3-b.out` B1: 200 + `Connection: keep-alive` + the Keep-Alive
/// header, the socket living ~5 s past the response until its own idle
/// close, close() resolving only then). The handler below holds the request
/// across the stop, so the stop lands in that exact state: no EOF while the
/// request is held, a keep-alive response on release, EOF at the 5 s idle
/// deadline rather than right after the response, and the serving future
/// resolving `Ok(())` once the idle close ends it.
#[tokio::test]
async fn stop_serves_in_flight_request_and_waits_like_ts_close() {
    let fixture = roots();
    let config = config_for("a", &fixture);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1209 bind listener");
    let bound = listener.local_addr().expect("#1209 listener local address");
    let (stop_tx, stop_rx) = watch::channel(false);
    let (entered_tx, mut entered_rx) = watch::channel(false);
    let (release_tx, release_rx) = watch::channel(false);
    let handler_config = config.clone();
    let entered = entered_tx.clone();
    let release = release_rx.clone();
    let serve = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("#1209 accept connection");
        http_proxy::serve_connection_native_with_shutdown(
            socket,
            move |request, _lease| {
                let config = handler_config.clone();
                let entered = entered.clone();
                let mut release = release.clone();
                async move {
                    // Genuinely in flight: parsed and inside the handler.
                    entered.send_replace(true);
                    loop {
                        if *release.borrow_and_update() {
                            break;
                        }
                        if release.changed().await.is_err() {
                            break;
                        }
                    }
                    serve(&request, &config).await
                }
            },
            stop_rx,
        )
        .await
    });

    let mut inflight = timeout(DEADLINE, TcpStream::connect(bound))
        .await
        .expect("#1209 connect deadline")
        .expect("#1209 connect to the native listener");
    inflight
        .write_all(&latin1_bytes(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        ))
        .await
        .expect("#1209 write the held request");
    wait_flag(
        &mut entered_rx,
        "the in-flight request never reached its handler",
    )
    .await;
    stop_tx.send_replace(true);
    // The stop leaves the in-flight request alone: no EOF while it is held.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut probe = [0u8; 1];
    match timeout(Duration::from_millis(300), inflight.read(&mut probe)).await {
        Err(_still_open) => {}
        other => panic!(
            "#1209 the in-flight connection is not force-closed at stop like TS server.close(): {other:?}"
        ),
    }
    release_tx.send_replace(true);
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
    let elapsed = eof_after_close(&mut inflight, "in-flight connection").await;
    assert!(
        elapsed >= Duration::from_secs(3),
        "#1209 the in-flight connection is not force-closed after its response like TS server.close(): closed after {elapsed:?}"
    );
    let served = timeout(Duration::from_secs(8), serve)
        .await
        .expect("#1209 the in-flight serve future joins")
        .expect("#1209 the in-flight serve task");
    assert!(
        served.is_ok(),
        "#1209 the in-flight connection future resolves Ok once the idle close ends it: {served:?}"
    );
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #1209: Node's keep-alive idle close is in force with or without a stop:
/// `server.keepAliveTimeout` defaults to 5 s (the TS `GatewayServer` does not
/// override it and advertises `Keep-Alive: timeout=5` — the bare value), and a
/// connection that made one request and went quiet closes 6 s after its
/// response: Node adds the fixed 1 s `keepAliveTimeoutBuffer` (probes
/// `probe-fixr3-a.out` A: EOF at +6007 ms; the #1250 probes read back
/// `keepAliveTimeout=5000`/`keepAliveTimeoutBuffer=1000` live). #1250 pins
/// the port to the same 6 s; `keep_alive_idle_close_lands_at_six_seconds`
/// pins the deadline deterministically on the paused clock, so this wall-clock
/// test only cross-checks the range. No stop is involved until after the EOF.
#[tokio::test]
async fn keep_alive_idle_close_matches_ts() {
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
        Duration::from_secs(8),
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
    let (response, closed) = capture(&mut idle, "idle-close").await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1209 the health request is served before the idle close: {response:?}"
    );
    assert!(
        !closed,
        "#1209 the keep-alive connection stays open after the response"
    );
    let elapsed = eof_after_close(&mut idle, "the served connection").await;
    assert!(
        elapsed >= Duration::from_secs(5),
        "#1209 the keep-alive connection is not closed early: the idle close lands at Node's keepAliveTimeout + the fixed 1 s keepAliveTimeoutBuffer (#1250), closed after {elapsed:?}"
    );
    stop.send(()).expect("#1209 stop native listener");
    let drained = timeout(Duration::from_secs(10), task)
        .await
        .expect("#1209 stop joins the idle-close listener")
        .expect("#1209 stop task");
    assert!(
        drained.is_ok(),
        "#1209 the listener drains Ok after the idle close: {drained:?}"
    );
}

// ── #1250: the keep-alive idle close's state × event × timing matrix, pinned
// on the paused tokio clock (duplex + `start_paused`): virtual time moves only
// through the explicit advances below, so every deadline is probed, never
// raced. The TS expectations come from the real-`GatewayServer` probes
// (Node v22.22.0, registered in ledger A8.4-NATIVE-HTTP-001-R1): the idle
// deadline is `keepAliveTimeout` 5 s plus the fixed 1 s
// `keepAliveTimeoutBuffer` (6 s), it arms at response `finish` — not at
// handler return — and every socket read segment refreshes it.

/// #1250: the virtual-clock harness from `native_ws::tests`: the wrapped
/// future is polled against yields of the current-thread runtime, so the
/// serve task makes progress between advances. The wall guard only diagnoses
/// a stalled future; it is never a timing result.
async fn manual_clock<F: std::future::Future>(future: F) -> F::Output {
    tokio::pin!(future);
    let wall = Instant::now();
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = tokio::task::yield_now() => {}
        }
        assert!(
            wall.elapsed() < Duration::from_secs(60),
            "#1250 manual-clock harness stalled: the serve future never resolved"
        );
    }
}

/// #1250: append to `buffer` until it contains `marker` — a response chunk or
/// the chunked terminator — failing with the cell's own message instead of a
/// bare IO error.
async fn read_until(
    buffer: &mut Vec<u8>,
    client: &mut tokio::io::DuplexStream,
    marker: &str,
    what: &str,
) {
    while !latin1_string(buffer).contains(marker) {
        let mut chunk = [0u8; 256];
        let read = client
            .read(&mut chunk)
            .await
            .unwrap_or_else(|error| panic!("#1250 {what}: {error}"));
        if read == 0 {
            panic!(
                "#1250 {what}: EOF before {marker:?} arrived (buffer so far: {:?})",
                latin1_string(buffer)
            );
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// #1250: read one full keep-alive response (head + chunked terminator). The
/// terminator is on the wire strictly after the body's end-of-frames — the
/// instant the idle deadline arms — so its arrival proves the deadline is
/// armed; no barrier beyond the read itself is needed.
async fn read_response(client: &mut tokio::io::DuplexStream, what: &str) -> String {
    let mut buffer = Vec::new();
    read_until(&mut buffer, client, "0\r\n\r\n", what).await;
    let text = latin1_string(&buffer);
    assert!(
        text.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1250 {what}: the 200 head arrived, got {text:?}"
    );
    text
}

/// #1250: probe for data/EOF at the current virtual time. The 50 ms timeout
/// window is armed before its own 60 ms advance, so a still-open read resolves
/// `Elapsed`, while an EOF latched by a close during the advance wins — tokio
/// polls the inner future before the delay.
async fn probe_still_open(client: &mut tokio::io::DuplexStream, what: &str) {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let mut probe = [0u8; 1];
    let pending = timeout(Duration::from_millis(50), client.read(&mut probe));
    tokio::time::advance(Duration::from_millis(60)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    match pending.await {
        Err(_quiet) => {}
        Ok(Ok(0)) => panic!("#1250 {what}"),
        Ok(Ok(count)) => panic!("#1250 {what}: {count} unexpected byte(s) arrived"),
        Ok(Err(error)) => panic!("#1250 {what}: {error}"),
    }
}

/// #1250: advance virtual time past the idle deadline and read the latched
/// EOF (a half-closed duplex keeps reading 0, so no window is needed). The
/// read is itself bounded by a 50 ms timeout and one more 60 ms advance: a
/// connection still open past the expected deadline fails here with the
/// cell's own message instead of hanging until the harness wall guard.
async fn eof_after_idle_close(client: &mut tokio::io::DuplexStream, elapsed: Duration, what: &str) {
    tokio::time::advance(elapsed).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let mut probe = [0u8; 1];
    let pending = timeout(Duration::from_millis(50), client.read(&mut probe));
    tokio::time::advance(Duration::from_millis(60)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    match pending.await {
        Ok(Ok(0)) => {}
        Ok(Ok(count)) => panic!("#1250 {what}: {count} unexpected byte(s) before the EOF"),
        Ok(Err(error)) => panic!("#1250 {what}: {error}"),
        Err(_still_open) => {
            panic!("#1250 {what}: the connection is still open past the idle deadline")
        }
    }
}

/// #1250: a response body the test holds mid-stream across the old idle
/// deadline: the first chunk goes out immediately, the last only after the test
/// flips `release` — Node arms the idle timer at response `finish`, so the gap
/// must not close the connection. The release wait is one boxed future polled
/// across the body's polls, so no wakeup can be lost.
struct HeldBody {
    first: Option<Bytes>,
    last: Option<Bytes>,
    wait: Option<Pin<Box<dyn std::future::Future<Output = ()> + Send + Sync>>>,
}
impl hyper::body::Body for HeldBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        let this = self.get_mut();
        if let Some(first) = this.first.take() {
            return Poll::Ready(Some(Ok(hyper::body::Frame::data(first))));
        }
        if let Some(wait) = this.wait.as_mut() {
            if wait.as_mut().poll(context).is_pending() {
                return Poll::Pending;
            }
            this.wait = None;
        }
        Poll::Ready(
            this.last
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

fn held_body(first: &str, last: &str, release: watch::Receiver<bool>) -> Body {
    let mut waiter = release.clone();
    HeldBody {
        first: Some(Bytes::copy_from_slice(first.as_bytes())),
        last: Some(Bytes::copy_from_slice(last.as_bytes())),
        wait: Some(Box::pin(async move {
            wait_flag(&mut waiter, "#1250 held-body release channel").await
        })),
    }
    .boxed()
}

/// #1250 (matrix S5×E2/E4, divergence 1): a connection that made one request
/// and went quiet closes at last activity + 6 s — `keepAliveTimeout` 5 s plus
/// the fixed 1 s `keepAliveTimeoutBuffer` — while the response still
/// advertises the bare `Keep-Alive: timeout=5`. The probe/eof windows pin
/// the deadline inside (5.99, 6.09] s, so neither a 500 ms buffer (5.5 s
/// close) nor a 1.1 s one passes. Binds the negative controls
/// `native-http-idle-close-buffer-off` (the pre-#1250 bare 5 s close) and
/// `native-http-idle-close-buffer-half` (a 500 ms buffer closes at 5.5 s,
/// inside the still-open probe's window).
#[tokio::test(start_paused = true)]
async fn keep_alive_idle_close_lands_at_six_seconds() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the health request");
        let response = read_response(&mut client, "health").await;
        assert!(
            response.to_ascii_lowercase().contains("keep-alive: timeout=5"),
            "#1250 the advertisement stays the bare keepAliveTimeout: {response:?}"
        );
        tokio::time::advance(Duration::from_millis(5_930)).await;
        probe_still_open(
            &mut client,
            "the idle connection stays open at +5.99 s: Node closes at keepAliveTimeout + the fixed 1 s keepAliveTimeoutBuffer (6 s), never at the bare 5 s or a shortened buffer",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(40),
            "the idle close lands at 6 s after the response, not any later",
        )
        .await;
        let served = task.await.expect("#1250 idle-close task join");
        assert!(
            served.is_ok(),
            "#1250 the connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S4×E3/E4, divergence 2): the idle deadline arms at the
/// response's end-of-frames, not at handler return — a body held mid-stream
/// for 8 s is not closed, and the close lands at body completion + 6 s. Binds
/// the negative control `native-http-inflight-body-off` (the pre-#1250
/// release-and-arm at handler return).
#[tokio::test(start_paused = true)]
async fn idle_close_waits_for_streaming_body() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |_request, _lease| {
                let release = release_rx.clone();
                async {
                    hyper::Response::builder()
                        .status(hyper::StatusCode::OK)
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .body(held_body("part1-", "part2", release))
                        .map_err(std::io::Error::other)
                }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes("GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the held-body request");
        let mut buffer = Vec::new();
        read_until(&mut buffer, &mut client, "part1-", "held body first chunk").await;
        tokio::time::advance(Duration::from_secs(8)).await;
        probe_still_open(
            &mut client,
            "the connection stays open while the response body is held mid-stream: Node arms the idle timer at response finish",
        )
        .await;
        release_tx.send_replace(true);
        let mut tail = Vec::new();
        read_until(&mut tail, &mut client, "0\r\n\r\n", "held body end").await;
        assert!(
            latin1_string(&tail).contains("part2"),
            "#1250 held body end: the final chunk arrived, got {:?}",
            latin1_string(&tail)
        );
        tokio::time::advance(Duration::from_millis(5_930)).await;
        probe_still_open(
            &mut client,
            "the streaming connection stays open at +5.99 s after its body ended: Node arms the idle deadline at response finish, not at handler return",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(40),
            "the idle close lands 6 s after the streaming body ended",
        )
        .await;
        let served = task.await.expect("#1250 held-body task join");
        assert!(
            served.is_ok(),
            "#1250 the held-body connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S2/S5×E1/E4, divergence 3): every read segment refreshes the
/// idle deadline — a follow-up head that arrives as a segment at +3 s survives
/// the previous response's +6 s deadline mid-arrival, and the connection then
/// closes at the last activity + 6 s. Binds the negative control
/// `native-http-read-refresh-off` (the pre-#1250 no-refresh close).
#[tokio::test(start_paused = true)]
async fn idle_close_refreshes_on_read_segments() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the first health request");
        read_response(&mut client, "first health").await;
        tokio::time::advance(Duration::from_secs(3)).await;
        client
            .write_all(&latin1_bytes("GE"))
            .await
            .expect("#1250 write the first head segment");
        probe_still_open(
            &mut client,
            "the half-arrived follow-up head keeps the connection open after its segment",
        )
        .await;
        tokio::time::advance(Duration::from_millis(3_000)).await;
        probe_still_open(
            &mut client,
            "the half-arrived follow-up head keeps the connection open past the pre-segment deadline: a read segment refreshes Node's idle deadline",
        )
        .await;
        client
            .write_all(&latin1_bytes(
                "T /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1250 write the completing head segments");
        read_response(&mut client, "second health").await;
        tokio::time::advance(Duration::from_millis(5_930)).await;
        probe_still_open(
            &mut client,
            "the second response's connection stays open at +5.99 s past the refreshed deadline",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(40),
            "the idle close lands 6 s after the last activity",
        )
        .await;
        let served = task.await.expect("#1250 segments task join");
        assert!(
            served.is_ok(),
            "#1250 the segmented connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S3×E2/E4): while the handler is executing, no idle close is
/// in force — the in-flight slot is held until the response body finishes, so
/// a handler held 10 s keeps the connection open and the close lands at
/// response completion + 6 s (aligned cell; no dedicated mutant).
#[tokio::test(start_paused = true)]
async fn handler_execution_blocks_idle_close() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let mut release = release_rx.clone();
                let config = config.clone();
                async move {
                    wait_flag(&mut release, "#1250 handler-held release channel").await;
                    serve(&request, &config).await
                }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the handler-held request");
        tokio::time::advance(Duration::from_secs(10)).await;
        probe_still_open(
            &mut client,
            "the connection stays open while the handler is executing: Node arms no idle timer before response finish",
        )
        .await;
        release_tx.send_replace(true);
        read_response(&mut client, "handler-held health").await;
        tokio::time::advance(Duration::from_millis(5_400)).await;
        probe_still_open(
            &mut client,
            "the served connection after a held handler stays open at +5.46 s",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(700),
            "the idle close lands 6 s after the held handler's response",
        )
        .await;
        let served = task.await.expect("#1250 handler-held task join");
        assert!(
            served.is_ok(),
            "#1250 the handler-held connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S1/S2-first×E1/E2/E4): the idle deadline is armed only by
/// request activity — a connection that sent nothing, nor only an incomplete
/// first head (which TS bounds by its 60 s `headersTimeout` instead), is
/// never closed by the keep-alive timer, and a first head completing after
/// more than 6 s of segmentation is still answered `Connection: keep-alive`
/// on a reusable socket (probe `node-first-head-segmented.out`: segment +
/// 8 s gap → open, completed head → keep-alive, second request served,
/// close at last quiet +~5.7 s). The yields after the partial write let the
/// serve task actually read the segment before the 20 s advance, so this
/// test kills a read-refresh that arms the timer before any request was
/// served. The `headersTimeout` divergence is registered in the ledger and
/// out of this issue's scope. Binds the negative control
/// `native-http-unserved-read-arms-idle`.
#[tokio::test(start_paused = true)]
async fn idle_state_no_request_stays_open() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
        ));
        tokio::time::advance(Duration::from_secs(30)).await;
        probe_still_open(
            &mut client,
            "the no-request connection stays open: the idle deadline is not armed before any response",
        )
        .await;
        client
            .write_all(&latin1_bytes("GET /hea"))
            .await
            .expect("#1250 write the partial first head");
        // Let the serve task consume the segment first: a refresh armed at
        // this read would have its deadline inside the next 20 s advance.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(20)).await;
        probe_still_open(
            &mut client,
            "the partial first head keeps the connection open inside Node's 60 s headers window: a read before any served request arms no keep-alive deadline",
        )
        .await;
        client
            .write_all(&latin1_bytes(
                "lth HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1250 complete the first head");
        let response = read_response(&mut client, "segmented first head").await;
        let lowered = response.to_ascii_lowercase();
        assert!(
            lowered.contains("connection: keep-alive") && !lowered.contains("connection: close"),
            "#1250 the segmented first head is answered keep-alive: Node arms no idle timer before the first response finish, got {response:?}"
        );
        client
            .write_all(&latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 reuse the segmented-head connection");
        let second = read_response(&mut client, "segmented-head reuse").await;
        assert!(
            second.starts_with("HTTP/1.1 200 OK\r\n"),
            "#1250 the second request on the segmented-head connection is served: {second:?}"
        );
        tokio::time::advance(Duration::from_millis(5_400)).await;
        probe_still_open(
            &mut client,
            "the segmented-head connection stays open at +5.46 s after its second response",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(700),
            "the idle close lands 6 s after the segmented-head reuse response",
        )
        .await;
        let served = task.await.expect("#1250 no-request task join");
        assert!(
            served.is_ok(),
            "#1250 the no-request connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S4×E1): a pipelined next request whose head arrives while
/// the previous response's body is still held does not trigger the idle close
/// — the read refreshes the deadline and the follow-up is served after the
/// body ends (aligned cell; no dedicated mutant).
#[tokio::test(start_paused = true)]
async fn pipelined_request_during_body() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |_request, _lease| {
                let release = release_rx.clone();
                async {
                    hyper::Response::builder()
                        .status(hyper::StatusCode::OK)
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .body(held_body("part1-", "part2", release))
                        .map_err(std::io::Error::other)
                }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes("GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the held-body request");
        let mut buffer = Vec::new();
        read_until(&mut buffer, &mut client, "part1-", "pipelined first chunk").await;
        client
            .write_all(&latin1_bytes("GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 pipeline the next request during the body");
        tokio::time::advance(Duration::from_secs(3)).await;
        probe_still_open(
            &mut client,
            "the pipelined next request during a held body does not trigger the idle close",
        )
        .await;
        release_tx.send_replace(true);
        // Both responses from one buffer: the follow-up can share the read
        // chunk that delivers the first terminator, so splitting the reads
        // would swallow its bytes.
        let mut tail = Vec::new();
        while latin1_string(&tail).matches("0\r\n\r\n").count() < 2 {
            let mut chunk = [0u8; 256];
            let read = client
                .read(&mut chunk)
                .await
                .unwrap_or_else(|error| panic!("#1250 pipelined responses: {error}"));
            if read == 0 {
                panic!(
                    "#1250 pipelined responses: EOF before the second terminator (buffer so far: {:?})",
                    latin1_string(&tail)
                );
            }
            tail.extend_from_slice(&chunk[..read]);
        }
        let tail = latin1_string(&tail);
        assert!(
            tail.contains("HTTP/1.1 200 OK") && tail.matches("part2").count() == 2,
            "#1250 the pipelined follow-up was served after the body ended: {tail:?}"
        );
        tokio::time::advance(Duration::from_millis(5_400)).await;
        probe_still_open(
            &mut client,
            "the pipelined connection stays open at +5.46 s after the second response",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            Duration::from_millis(700),
            "the idle close lands 6 s after the second pipelined response",
        )
        .await;
        let served = task.await.expect("#1250 pipelined task join");
        assert!(
            served.is_ok(),
            "#1250 the pipelined connection future resolves Ok once the idle close ends it: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S5×E5): a client that goes away during the idle window ends
/// the connection through the connection future itself — no timer is armed or
/// consulted — so the serve future joins without a single virtual advance.
#[tokio::test(start_paused = true)]
async fn client_close_ends_connection_without_timer() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1250 write the health request");
        read_response(&mut client, "client-close health").await;
        client
            .shutdown()
            .await
            .expect("#1250 half-close the idle connection");
        // No advance(): the peer end alone must resolve the serve future.
        let served = task.await.expect("#1250 client-close task join");
        assert!(
            served.is_ok(),
            "#1250 the client-closed connection future resolves Ok without any timer: {served:?}"
        );
    })
    .await;
}

/// #1250 (matrix S5×E2×E5 same instant): the idle deadline expiring and the
/// client going away becoming ready at the same virtual moment — 10 ms shy
/// of the 6 s deadline the client half-closes, then one 20 ms advance makes
/// the idle timer and the peer EOF ready together. Whichever select! arm the
/// runtime polls first, the serve future resolves `Ok` and never hangs
/// (aligned cell; no dedicated mutant: both arms converge on the same
/// outcome).
#[tokio::test(start_paused = true)]
async fn idle_deadline_and_client_close_same_instant_resolve_ok() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1250 write the health request");
        read_response(&mut client, "same-instant health").await;
        tokio::time::advance(Duration::from_millis(5_990)).await;
        client
            .shutdown()
            .await
            .expect("#1250 half-close 10 ms shy of the idle deadline");
        // Both the idle timer (deadline at +6 s) and the connection future
        // (peer EOF) become ready inside this single advance.
        tokio::time::advance(Duration::from_millis(20)).await;
        let served = task.await.expect("#1250 same-instant task join");
        assert!(
            served.is_ok(),
            "#1250 the same-instant deadline/client-close connection resolves Ok either way: {served:?}"
        );
    })
    .await;
}

/// #996-style regeneration guard for the fixture; see the fixture README for
/// the pinned Node invocation.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    test_baseline::regenerates_identically("native-http-basic");
}
