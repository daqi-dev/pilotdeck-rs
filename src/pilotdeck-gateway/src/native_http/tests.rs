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
/// #1891: exempt from the first-byte relaxation on purpose — the remaining
/// `DEADLINE` uses bound in-process listener operations only (the local TCP
/// connect, the accept-loop join after `stop`, and the listener's own drain
/// budget), which never wait for a sidecar; the first-byte capture below has
/// its own liveness constant.
const DEADLINE: Duration = Duration::from_secs(5);
/// #1891: liveness watchdog for the first response byte, not an assertion
/// about the response. The sibling loop in `module_host::tests` reddened
/// "produced no bytes and stayed open" on 8/48 loaded copies under #978 with
/// twice this five-second window; the byte oracle and the closed flag are
/// unchanged, and a connection that truly never answers still reds after
/// this margin.
const FIRST_BYTE: Duration = Duration::from_secs(60);
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

/// #2153: the cells whose replay needs the `content-length: 0` exemption —
/// exactly the bare parser-level 400s of A8.4-NATIVE-HTTP-001 item 3, the
/// cells where TS writes no length and hyper adds `content-length: 0`
/// (measured: `no-host` is chunked with an empty terminator on both sides
/// and carries none). Like api-server's `parser_admission` cells the list is
/// explicit, so a cell that grows a length line anywhere else replays red
/// instead of normalizing away; the
/// `content_length_zero_stays_scoped_to_the_registered_rejections` pin
/// holds the raw bytes behind the mask in both directions.
const PARSER_ADMISSION_CELLS: &[&str] = &[
    "bad-method-foo",
    "bad-method-get",
    "bad-method-getx",
    "non-ascii-path",
    "non-ascii-query",
    "garbage",
    "pct-target",
    "authority-form",
    "abs-scheme-nonalpha",
    "empty-request",
    "connect-nonascii",
];

/// #2776/#2789: the predicate that grants the `content-length: 0` exemption.
/// The replay's selection point goes through this one function, so widening
/// it (the pre-#2153 blanket shape) fails the coverage guard's
/// through-the-predicate comparison instead of silently re-widening the mask.
fn parser_admission(id: &str) -> bool {
    if crate::mutants::on("native-http-cl0-wide") {
        // #2776 negative control: the exemption list spread to every cell,
        // which the guard below must reject.
        return true;
    }
    PARSER_ADMISSION_CELLS.contains(&id)
}

/// The two deterministic static roots of the fixture generator, contents never
/// changing between runs.
struct Fixture {
    parent: PathBuf,
    root: PathBuf,
    root2: PathBuf,
}

/// #1383: the first static root's fixture files, relative to the root. One
/// shared table: `roots()` writes from it and the case-variant completeness
/// guard (`only_the_registered_cell_has_a_case_variant_fixture_hit`) reads
/// it, so a file added here that case-collides with a routed path fails that
/// guard instead of silently shifting the replay's premise. The recorder's
/// copy of this list (`tests/fixtures/native-http-basic/generate.mts`) is the
/// frozen original; it is not machine-compared with this one, so both sides
/// must be edited together.
const ROOT_FILES: &[(&str, &str)] = &[
    ("index.html", "<!doctype html><html>index</html>\n"),
    ("app.js", "console.log(1)\n"),
    ("style.css", "body{}\n"),
    ("data.json", "{\"a\":1}\n"),
    ("logo.svg", "<svg/>\n"),
    ("bin.bin", "0,1,2,255,254\n"),
    ("noext", "no extension\n"),
    ("upper.TXT", "upper\n"),
    ("sub/page.html", "sub page\n"),
    ("enc%20oded", "encoded-name\n"),
];

/// #1383: the second (index-less) static root's files, shared the same way
/// as [`ROOT_FILES`].
const ROOT2_FILES: &[(&str, &str)] = &[("only.txt", "only file\n")];

fn roots() -> Fixture {
    let parent = std::env::temp_dir().join(format!("pdrs-1209-{}", uuid::Uuid::new_v4()));
    let root = parent.join("root");
    let root2 = parent.join("root2");
    for (files, base) in [(ROOT_FILES, &root), (ROOT2_FILES, &root2)] {
        for (relative, content) in files {
            let path = base.join(relative);
            // Sub-directories come from the relative path (`sub/page.html`).
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory).expect("#1209 fixture directory");
            }
            std::fs::write(path, content).expect("#1209 fixture static file");
        }
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
        webhook: None,
        api_web: None,
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
                    None,
                    None,
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
    while start.elapsed() < FIRST_BYTE {
        let wait = if out.is_empty() {
            // Saturating: the loop condition above re-reads `elapsed`, so on a
            // loaded host it can cross the cap between the two reads and the
            // plain subtraction would panic on the very stall this watcher
            // must tolerate (#978 boundary review).
            FIRST_BYTE.saturating_sub(start.elapsed())
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
    // #1891: receipt-wired form of the assert; the text and the pass/fail
    // condition are unchanged, so the shrink mutant still has to die on this
    // exact oracle instead of on timing.
    crate::negative_assert!(
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
    let admitted = parser_admission(&cell.id);
    assert_eq!(
        normalize(&raw, admitted),
        normalize(&expected, admitted),
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
/// `node_join` + `contained` + `is_file` lookup `serve_static` performs for
/// that request, so the probe decides with the exact evidence the router
/// itself will use. Sharing the route also means the probe only requires
/// the fixture root to be normalized — `roots()` builds it from
/// `temp_dir()`, absolute and free of `..`.
/// The `native-http-static-case-probe-inverted` negative-control switch
/// flips the answer, which must fail the `static-upper-lower` replay at its
/// own assertion on every host.
fn serves_upper_txt(root: &std::path::Path) -> bool {
    let candidate = node_join(root, "/upper.txt");
    let lookup = contained(root, &candidate)
        && std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file());
    if crate::mutants::on("native-http-static-case-probe-inverted") {
        !lookup
    } else {
        lookup
    }
}

/// #1383: `expected_response` borrows the `static-upper` golden on a
/// case-insensitive host under the premise that `.txt` stays out of
/// `CONTENT_TYPES` — otherwise the borrowed bytes' content-type would no
/// longer be what the host serves for the resolved `upper.TXT` file. The
/// premise is pinned explicitly instead of holding by coincidence. Binds the
/// negative control `native-http-content-type-txt`, which simulates the
/// mapping gaining a `.txt` entry.
#[test]
fn static_upper_borrow_premise_holds() {
    let golden = cell("static-upper").response;
    assert!(
        golden.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1383 the borrowed static-upper golden is a 200"
    );
    assert!(
        golden.contains("content-type: application/octet-stream\r\n"),
        "#1383 the borrowed static-upper golden carries the octet-stream content type"
    );
    assert!(
        golden.ends_with("\r\n\r\n6\r\nupper\n\r\n0\r\n\r\n"),
        "#1383 the borrowed static-upper golden is the upper\\n bytes in one size-6 chunk"
    );
    let upper_fixture = ROOT_FILES
        .iter()
        .find(|(file, _)| *file == "upper.TXT")
        .expect("#1383 the first root carries the upper.TXT fixture")
        .1;
    assert_eq!(
        upper_fixture, "upper\n",
        "#1383 borrow premise: the upper.TXT the host resolves serves the golden's chunk payload"
    );
    assert_eq!(
        content_type_for(std::path::Path::new("upper.txt")),
        "application/octet-stream",
        "#1383 borrow premise: CONTENT_TYPES has no .txt, so upper.txt keeps the golden's application/octet-stream content type"
    );
    assert_eq!(
        content_type_for(std::path::Path::new("upper.TXT")),
        "application/octet-stream",
        "#1383 borrow premise: .TXT misses the case-sensitive extension map and stays octet-stream"
    );
}

/// #1383: every routed request as `(id, server, WHATWG-path)`, one entry per
/// request in the cell — a pipelined cell (`pipeline`,
/// `unread-body-pipeline`) contributes both of its requests, not only the
/// first. The target goes through the production `whatwg_path()`, the path
/// the static fallback would look up, never an independent normalization.
/// Cells without a parseable target (`empty-request`, `garbage`) contribute
/// none; a parse failure maps to `/` like the router's own fallback.
fn routed_cells() -> Vec<(String, String, String)> {
    let recorded = oracle();
    let mut cells = Vec::new();
    for entry in recorded["cells"]
        .as_array()
        .expect("#1209 oracle cells array")
    {
        let request = entry["request"]
            .as_str()
            .expect("#1209 oracle cell request");
        let id = entry["id"]
            .as_str()
            .expect("#1209 oracle cell id")
            .to_owned();
        let server = entry["server"]
            .as_str()
            .expect("#1209 oracle cell server")
            .to_owned();
        for target in request_targets(request) {
            cells.push((id.clone(), server.clone(), whatwg_path(target)));
        }
    }
    if crate::mutants::on("native-http-case-variant-extra-cell") {
        cells.push(("static-appjs-upper".into(), "a".into(), "/APP.JS".into()));
    }
    cells
}

/// #1383: every request target in one recorded cell blob, pipelined requests
/// included. Each request line ends with its ` HTTP/1.x` version token, so
/// the target is the last whitespace-delimited token before it — which holds
/// both for a standalone request line and for a request pipelined straight
/// onto an unread body (`unread-body-pipeline` glues `GET /app.js` to the
/// body bytes). A body that happened to carry the same token shape would
/// only add a path to the check: the guard errs towards failing loudly,
/// never towards passing silently.
fn request_targets(request: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = request;
    while let Some(at) = rest.find(" HTTP/1.") {
        if let Some(target) = rest[..at].split_whitespace().last() {
            targets.push(target);
        }
        rest = &rest[at + " HTTP/1.".len()..];
    }
    targets
}

/// #1383: the file key `serve_static` would look up for one already
/// normalized path — the production `node_join` fold (empty segments and `.`
/// dropped, `..` pops) behind the same `/` → `/index.html` mapping, so the
/// guard compares what the filesystem is asked for, not the request string.
/// The probe root is named after the fixture root it stands in for, so a `..`
/// that leaves the root and re-enters it by name folds identically. `None`
/// when `contained` rejects the candidate: `serve_static` then stats nothing
/// from the root, so no fixture file can be reached.
fn lookup_key(path: &str, root_name: &str) -> Option<String> {
    let probe_root = std::path::Path::new("/pdrs-1383-fixture").join(root_name);
    let pathname = if path == "/" { "/index.html" } else { path };
    let candidate = node_join(&probe_root, pathname);
    if !contained(&probe_root, &candidate) {
        return None;
    }
    let relative = candidate.strip_prefix(&probe_root).ok()?;
    Some(relative.to_string_lossy().into_owned())
}

/// #1383: a routed path hits a fixture file as a case variant when the key
/// the static root would stat equals the file name ignoring ASCII case but
/// not byte for byte — the only shape whose served bytes depend on the host
/// filesystem's case sensitivity.
fn case_variant_hit(path: &str, root_name: &str, files: &[(&str, &str)]) -> bool {
    let Some(key) = lookup_key(path, root_name) else {
        return false;
    };
    files
        .iter()
        .any(|(file, _)| key.eq_ignore_ascii_case(file) && key.as_bytes() != file.as_bytes())
}

/// #1383: the fixture root name and file table of one recorded server,
/// mirroring [`config_for`] — `a`/`c` serve the first root, `b` the
/// index-less one, `d` has no static root. An unregistered server panics
/// instead of silently defaulting to the first root.
fn files_for(server: &str) -> Option<(&'static str, &'static [(&'static str, &'static str)])> {
    match server {
        "a" | "c" => Some(("root", ROOT_FILES)),
        "b" => Some(("root2", ROOT2_FILES)),
        "d" => None,
        other => panic!("#1383 unregistered fixture server {other:?}"),
    }
}

/// #1383: the routed cells whose path case-collides with that server's
/// fixture table (the mapping lives in [`files_for`]).
fn case_variant_ids(cells: &[(String, String, String)]) -> Vec<String> {
    cells
        .iter()
        .filter(|(_, server, path)| {
            files_for(server)
                .is_some_and(|(root_name, files)| case_variant_hit(path, root_name, files))
        })
        .map(|(id, _, _)| id.clone())
        .collect()
}

/// #1383: `static-upper-lower` is the only cell whose result depends on the
/// host filesystem — every other routed request, folded through the
/// production `node_join` the way `serve_static` folds it, either matches a
/// fixture file byte-for-byte or has no case-variant file to collide with.
/// The fixture tables are the same constants `roots()` writes from, so a
/// file added there that case-collides with a routed path fails here. Binds
/// the negative control `native-http-case-variant-extra-cell`, which routes
/// one more case-variant cell (`/APP.JS` against `app.js`).
#[test]
fn only_the_registered_cell_has_a_case_variant_fixture_hit() {
    assert_eq!(
        case_variant_ids(&routed_cells()),
        ["static-upper-lower"],
        "#1383 static-upper-lower is the only routed cell whose path case-collides with a fixture file"
    );
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
/// literal lowercase — the registered transport diff), chunk sizes are
/// case-folded (hyper writes `B`, Node writes `b` — registered transport
/// diff), and — on the `parser_admission` cells only, the bare parser-level
/// 400s registered as A8.4-NATIVE-HTTP-001 item 3 — hyper's
/// `content-length: 0` is dropped. Every other length line compares
/// verbatim, so an unregistered cell that grows one replays red (#2153,
/// the same scoping api-server's #1683 fix gave its `actual_headers`).
/// Status lines are compared verbatim — both stacks write canonical
/// `HTTP/1.1 200 OK` casing — and chunk bodies are never folded.
fn normalize(raw: &str, parser_admission: bool) -> String {
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
            } else if (parser_admission || crate::mutants::on("native-http-cl0-mask-always"))
                && name.eq_ignore_ascii_case("content-length")
                && value.trim() == "0"
            {
                // hyper's implicit empty-body length on the registered bare
                // 400s only.
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
        webhook: None,
        api_web: None,
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

/// #1316: the A7.4 user-dialog HTTP store entry is not yet open on the
/// native listener. TS never mounts HttpGatewayUserDialogStore on the gateway
/// port either (it would be the generic 404 there), but the same-domain entry
/// must answer explicitly, not with the unknown-path 404.
#[tokio::test]
async fn user_dialog_store_entry_is_explicitly_not_yet_open() {
    let mut config = config_for("e", &roots());
    config.static_root = None;
    let response = route("GET", "/v1/gateway-user-dialog-store/dialogs", &config).await;
    assert_eq!(
        u16::from(response.status()),
        503,
        "#1316 user-dialog store entry is a 503"
    );
    assert_eq!(
        content_type_of(&response),
        "application/json",
        "#1316 not-yet-open content type"
    );
    assert_eq!(
        body_of(response).await,
        "{\"error\":\"not_yet_open\",\"store\":\"gateway-user-dialog-store\"}",
        "#1316 not-yet-open body"
    );
    // The prefix boundary: a path that merely contains the marker does not
    // match; only the whole store prefix opens the explicit entry.
    let response = route("GET", "/v1/gateway-user-dialog-storex", &config).await;
    assert_eq!(
        u16::from(response.status()),
        404,
        "#1316 a sibling path keeps the ordinary 404"
    );
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

/// #2153: the `content-length: 0` exemption `normalize` grants is scoped to
/// `PARSER_ADMISSION_CELLS` only, so the raw bytes have to be pinned in both
/// directions or the mask could hide a drift: a registered rejection that
/// stopped carrying the line (the rejection disappearing into a bare close)
/// and an unregistered answer that grew one (the extra length line the
/// replay must catch) both compare green through the mask alone. Every
/// exchange uses the cell's own recorded request bytes, like the replay.
#[tokio::test]
async fn content_length_zero_stays_scoped_to_the_registered_rejections() {
    let fixture = roots();
    for id in PARSER_ADMISSION_CELLS {
        let cell = cell(id);
        let (bare, closed) = exchange(&cell.server, &cell.request, &cell.id, &fixture).await;
        assert!(
            bare.to_ascii_lowercase()
                .contains("\r\ncontent-length: 0\r\n"),
            "#2153 pin {id}: the registered bare 400 carries content-length: 0: {bare:?}"
        );
        assert!(
            closed,
            "#2153 pin {id}: the registered bare 400 closes the connection"
        );
    }
    for id in ["health-get", "static-appjs", "root2-missing", "health-head"] {
        let cell = cell(id);
        let (raw, _) = exchange(&cell.server, &cell.request, &cell.id, &fixture).await;
        assert!(
            !raw.to_ascii_lowercase().contains("\r\ncontent-length:"),
            "#2153 pin {id}: the unregistered response carries no content-length at all: {raw:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&fixture.parent);
}

/// #2153: the exemption `PARSER_ADMISSION_CELLS` grants is the only one left
/// — an injected `content-length: 0` on the unregistered health cell must
/// survive normalization (an extra hyper length line replays red), while
/// the registered bare 400's line stays masked.
#[test]
fn only_the_registered_cells_mask_a_content_length_zero() {
    let health = cell("health-get");
    let injected = health
        .response
        .replacen("\r\n\r\n", "\r\ncontent-length: 0\r\n\r\n", 1);
    assert_ne!(
        normalize(&injected, false),
        normalize(&health.response, false),
        "#2153 an injected content-length: 0 on the unregistered health cell must not normalize away"
    );
    // The registered direction stays masked: hyper's line on the bare 400 is
    // the one exemption left, so the TS-shaped bytes still compare equal.
    let bare = cell("bad-method-foo");
    let hyper_shaped = bare
        .response
        .replacen("\r\n\r\n", "\r\ncontent-length: 0\r\n\r\n", 1);
    assert_eq!(
        normalize(&hyper_shaped, true),
        normalize(&bare.response, true),
        "#2153 the registered bare-400 exemption still masks hyper's content-length: 0"
    );
}

/// #2153 fix-r1 (review r1): the exemption list and the oracle describe the
/// same cell set, so neither can drift alone — the list is exactly the
/// oracle's bare parser-level 400s (`HTTP/1.1 400 Bad Request` + the
/// `Connection: close` line and nothing else before the empty body; the
/// missing-Host 400 carries its own `date` and chunked terminator, so it is
/// naturally excluded). A cell that gains or loses the bare-400 shape fails
/// here with its id instead of a generic replay red, and a stale list entry
/// points at the cell to re-check. #2776/#2789 add the second leg the
/// predicate-widening mutant needs: the admitted set, read through the same
/// `fn parser_admission` the replay's selection point uses, is also exactly
/// the oracle's bare 400s, so widening the predicate fails here too.
#[test]
fn parser_admission_cells_cover_exactly_the_oracle_bare_400s() {
    let recorded = oracle();
    let cells = recorded["cells"]
        .as_array()
        .expect("#1209 oracle cells array");
    let mut oracle_bare: Vec<&str> = cells
        .iter()
        .filter(|cell| {
            cell["response"]
                .as_str()
                .expect("#1209 oracle cell response")
                .starts_with("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
        })
        .map(|cell| cell["id"].as_str().expect("#1209 oracle cell id"))
        .collect();
    oracle_bare.sort_unstable();
    let mut listed = PARSER_ADMISSION_CELLS.to_vec();
    listed.sort_unstable();
    assert_eq!(
        listed, oracle_bare,
        "#2153 PARSER_ADMISSION_CELLS and the oracle's bare-400 cells are the same set"
    );
    // #2776/#2789: through the same `fn parser_admission` the replay's
    // selection point reads, so widening the predicate fails here instead of
    // re-widening the mask (the constant comparison above stays the
    // stale-entry leg).
    let mut admitted: Vec<&str> = cells
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1209 oracle cell id"))
        .filter(|id| parser_admission(id))
        .collect();
    admitted.sort_unstable();
    assert_eq!(
        admitted, oracle_bare,
        "#2153 the exemption predicate admits exactly the oracle's bare-400 cells"
    );
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
        let lower = raw.to_ascii_lowercase();
        assert!(
            raw.starts_with("HTTP/1.0 200 OK\r\n"),
            "#1209 pin {id}: HTTP/1.0 is answered with a 1.0 status line (registered diff vs Node's 1.1 line): {raw:?}"
        );
        assert!(
            raw.contains("\r\nConnection: close\r\n") && !lower.contains("keep-alive"),
            "#1209 pin {id}: HTTP/1.0 is answered close with no keep-alive: {raw:?}"
        );
        // #1947 structural pin (ledger A8.3-CH-WEBHOOK-004-R2): this cell's
        // reply is a streamed body — close-delimited already — so the lane's
        // framing switch (`native-http-http10-framing-off`, bound to the
        // webhook-json pin) cannot kill this assertion; it pins the existing
        // framing against drift.
        assert!(
            !lower.contains("\r\ncontent-length:"),
            "#1947 pin {id}: the HTTP/1.0 reply is close-delimited, no content-length: {raw:?}"
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

/// #1947: the `/feishu/webhook` reply is `respondJson` — `writeHead` +
/// `end(JSON.stringify(body))`, the one sized body this router builds — and
/// on HTTP/1.0 Node's reply is close-delimited with no `content-length`, so
/// the sized body must not leak hyper's `content-length` onto the 1.0 reply
/// (the framing fix mirrors the webhook channel's #1684 finalize). hyper's
/// `HTTP/1.0` status line stays the registered diff, so this is a raw pin on
/// the listener's own `serve_with_body` dispatch.
#[tokio::test]
async fn http10_webhook_json_is_close_delimited() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1947 bind webhook listener");
    let bound = listener
        .local_addr()
        .expect("#1947 webhook listener address");
    let config = Config {
        token: String::new(),
        loopback: false,
        static_root: None,
        webhook: Some(Arc::new(|| {
            Some(Arc::new(|_body: String| {
                Box::pin(async { (200u16, "{\"ok\":true}".to_owned()) })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = WebhookReply> + Send>>
            }))
        })),
        api_web: None,
    };
    let server_task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |request, _lease| {
                        let config = config.clone();
                        async move { serve_with_body(request, &config).await }
                    },
                    stop,
                    None,
                    None,
                )
                .await;
                if let Err(error) = served {
                    eprintln!("#1947 webhook connection error: {error}");
                }
            });
        }
    });
    for (id, connection) in [
        ("pin-http10-webhook", ""),
        ("pin-http10-webhook-keep", "Connection: keep-alive\r\n"),
    ] {
        let mut stream = timeout(DEADLINE, TcpStream::connect(bound))
            .await
            .expect("#1947 connect deadline")
            .expect("#1947 connect to webhook listener");
        stream
            .write_all(&latin1_bytes(&format!(
                "POST /feishu/webhook HTTP/1.0\r\nHost: 127.0.0.1\r\n{connection}Content-Length: 2\r\n\r\n{{}}"
            )))
            .await
            .expect("#1947 write webhook request");
        let (raw, closed) = capture(&mut stream, id).await;
        assert!(
            raw.starts_with("HTTP/1.0 200 OK\r\n"),
            "#1947 pin {id}: the 1.0 webhook reply carries hyper's 1.0 status line (registered diff): {raw:?}"
        );
        assert!(
            raw.contains("\r\nConnection: close\r\n")
                && !raw.to_ascii_lowercase().contains("keep-alive"),
            "#1947 pin {id}: the 1.0 webhook reply closes with no keep-alive: {raw:?}"
        );
        assert!(
            !raw.to_ascii_lowercase().contains("\r\ncontent-length:")
                && !raw.to_ascii_lowercase().contains("\r\ntransfer-encoding:"),
            "#1947 pin {id}: the 1.0 webhook reply is close-delimited, no content-length: {raw:?}"
        );
        assert!(
            raw.ends_with("{\"ok\":true}"),
            "#1947 pin {id}: the webhook body runs to the connection end: {raw:?}"
        );
        assert!(
            closed,
            "#1947 pin {id}: the 1.0 webhook reply closes the connection like TS"
        );
    }
    server_task.abort();
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
    // #2153: neither cell is a registered bare 400, so the length lines
    // compare verbatim.
    assert_eq!(
        normalize(&raw, false),
        normalize(&health.response, false),
        "#1209 native listener health cell matches TS"
    );
    assert!(!closed, "#1209 native listener keeps the health connection");

    let appjs = cell("static-appjs");
    let (raw, closed) = exchange_on(address, &appjs.request, &appjs.id).await;
    assert_eq!(
        normalize(&raw, false),
        normalize(&appjs.response, false),
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
        webhook: None,
        api_web: None,
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
    // The absolute-form target with a path and query exercises the
    // no-reallocation span arithmetic in `tracked_header_bytes`
    // (`scheme + "://" + authority + path-and-query`); with a real path the
    // sum renders byte-identical to `to_string()` (uri-probe, #1292), so the
    // change is allocation-only there — these cells pin the budget boundary
    // for the fragmented form under the existing
    // `native-http-header-budget-boundary` mutant.
    let (raw, _) = exchange(
        "a",
        format!(
            "GET http://h/health?x={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "a".repeat(16352)
        )
        .as_bytes(),
        "pin-budget-absolute-parts",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1292 the fragmented absolute-form span (tracked 16383) stays served like the to_string() render: {raw:?}"
    );
    let (raw, closed) = exchange(
        "a",
        format!(
            "GET http://h/health?x={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "a".repeat(16353)
        )
        .as_bytes(),
        "pin-budget-absolute-parts-431",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1292 the fragmented absolute-form span (tracked 16384) is the TS 431 like the to_string() render: {raw:?}"
    );
    assert!(
        closed,
        "#1292 the fragmented absolute-form span 431 closes the connection like TS"
    );
    // The absolute-form empty-path + query target `http://h?x=<pad>` is the
    // one accepted form where the segmented sum and `to_string()` diverge:
    // `PathAndQuery::as_str()` carries no implicit slash, so the sum is the
    // raw target span on the wire (11 + pad) — exactly what Node's
    // TrackHeader counts — while `to_string()` renders `http://h/?x=<pad>`
    // one byte wider (probe `probe-p2.out`). The 16383 cell is the
    // `native-http-uri-realloc` mutant's kill: under the to_string()
    // re-render it lands on 16384 and 431s a target TS serves. Below the
    // budget this lane answers the empty path with the static index where
    // TS's router answers 404 (probe `probe-p2.out` p2a) — the budget
    // boundary, not the route, is what replicates.
    let (raw, _) = exchange(
        "a",
        format!(
            "GET http://h?x={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "a".repeat(16359)
        )
        .as_bytes(),
        "pin-budget-absolute-query",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1292 the segmented empty-path + query span (tracked 16383, the raw target on the wire) stays under the budget — the to_string() re-render (16384) would 431 it; this lane's index answer vs TS's 404 is routing, not budget: {raw:?}"
    );
    let (raw, closed) = exchange(
        "a",
        format!(
            "GET http://h?x={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "a".repeat(16360)
        )
        .as_bytes(),
        "pin-budget-absolute-query-431",
        &fixture,
    )
    .await;
    assert!(
        raw.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"),
        "#1292 the segmented empty-path + query span (tracked 16384) is the TS 431: {raw:?}"
    );
    assert!(
        closed,
        "#1292 the empty-path + query span 431 closes the connection like TS"
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

/// #1292: wait until the connection's serving loop has consumed the stop
/// (the `stop_observed` hook `serve_connection_native_with_shutdown`
/// offers), so the test's next step is deterministically after the stop —
/// the replacement for the fixed 100/200 ms sleeps these tests used to race.
/// A stop that never reaches the connection fails at the caller's own
/// message rather than an unrelated later assertion.
async fn wait_stop_observed(observed: &AtomicUsize, message: &str) {
    let started = Instant::now();
    while observed.load(Ordering::SeqCst) == 0 {
        assert!(started.elapsed() < DEADLINE, "{message}");
        tokio::time::sleep(Duration::from_millis(5)).await;
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
    let stop_count = server.stop_observed();
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
    // #1292: the late request is written only once the connection's
    // serving loop has consumed the stop (the `stop_observed` hook), not
    // after a fixed sleep racing the loop.
    wait_stop_observed(
        &stop_count,
        "#1292 the late-request connection's stop was never observed by its serving loop",
    )
    .await;
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
    // #1292: the test observes the connection's serving loop consuming the
    // stop (the `stop_observed` hook) instead of racing a fixed sleep.
    let stop_count = Arc::new(AtomicUsize::new(0));
    let stop_count_in_serve = stop_count.clone();
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
            Some(stop_count_in_serve),
            None,
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
    // #1292: the probe runs only after the serving loop has consumed the
    // stop (the `stop_observed` hook), so the not-idle decision is the one
    // being probed rather than a stop that has not landed yet.
    wait_stop_observed(
        &stop_count,
        "#1292 the in-flight connection's stop was never observed by its serving loop",
    )
    .await;
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

/// #1250: the probe/tail helpers' shared internal advance — both
/// `probe_still_open` and `eof_after_idle_close` arm an
/// `IDLE_CLOSE_HELPER_READ_WINDOW_MS` read, then advance virtual time by
/// this step before awaiting it (tokio polls the inner read before the
/// delay, so an EOF latched during the advance wins). The advance must
/// stay strictly greater than the read window (the const assert below):
/// the helper crosses the read's timeout with the advance, so a
/// still-open read resolves `Elapsed` while the latched EOF wins. The
/// #1292 lane's two inline helpers (`headers_timeout_408_arrives` /
/// `stream_ends_silently_at_grid_point`) deliberately keep their own
/// 60 ms literals each — the 002-family literal collection is follow-up
/// #2380, out of this change's scope. The family's window arithmetic
/// below and the same-instant cell's half-close instant are both written
/// in terms of this step, so the (5.99, 6.09] s judgment window stays
/// recomputable from named constants alone.
const IDLE_CLOSE_HELPER_ADVANCE_MS: u64 = 60;

/// #1250: the probe/tail helpers' shared read window — the `timeout` each
/// helper arms on its read before its `IDLE_CLOSE_HELPER_ADVANCE_MS`
/// advance. Only the #1250 family's two helpers use it: the #1292 lane's
/// own 50 ms literals stay inline (002 family, follow-up #2380).
const IDLE_CLOSE_HELPER_READ_WINDOW_MS: u64 = 50;

/// #1250: Node's effective keep-alive idle-close deadline —
/// `keepAliveTimeout` 5 s plus the fixed 1 s `keepAliveTimeoutBuffer`
/// (6 s). Every probe/tail window constant below is derived from this
/// line, and the const asserts after them turn any drift between the
/// probe instant, the deadline and the race step into a compile failure
/// instead of a silently moved (5.99, 6.09] s window.
const IDLE_CLOSE_DEADLINE_MS: u64 = 6_000;

/// #1250: the 6 s keep-alive idle-close family's shared probe and tail
/// windows. The arithmetic, recomputable from the named constants: the
/// probe instant is `IDLE_CLOSE_DEADLINE_MS - 10` (10 ms short of the
/// deadline), reached as `IDLE_CLOSE_PROBE_ADVANCE_MS` — the deadline
/// minus `probe_still_open`'s internal `IDLE_CLOSE_HELPER_ADVANCE_MS`
/// (6_000 - 60 - 10 = 5_930 ms) — plus that same helper advance
/// (5_930 + 60 = 5_990 ms), and the late-close bound is
/// `IDLE_CLOSE_TAIL_WINDOW` plus `eof_after_idle_close`'s internal
/// `IDLE_CLOSE_HELPER_ADVANCE_MS` (40 + 60 = 100 ms past the probe
/// instant, 6.090 s) — a judgment window of (5.99, 6.09] s: a close
/// earlier than that fails the still-open probe's EOF, one later fails
/// the tail read's still-open. Every cell of the family must use these,
/// or constants derived from them, instead of per-cell literals — the
/// #1292 lane's loose 200 ms tails and the pre-#1250 700 ms tails were
/// exactly that drift, silently widening the window.
const IDLE_CLOSE_PROBE_ADVANCE_MS: u64 = IDLE_CLOSE_DEADLINE_MS - IDLE_CLOSE_HELPER_ADVANCE_MS - 10;
const IDLE_CLOSE_PROBE_ADVANCE: Duration = Duration::from_millis(IDLE_CLOSE_PROBE_ADVANCE_MS);
const IDLE_CLOSE_TAIL_WINDOW: Duration = Duration::from_millis(40);

/// #1250: the same-instant cell's half-close instant — 10 ms shy of the
/// deadline (`IDLE_CLOSE_DEADLINE_MS - 10`), which is exactly the windowed
/// cells' probe instant (`IDLE_CLOSE_PROBE_ADVANCE_MS` plus the probe
/// helper's `IDLE_CLOSE_HELPER_ADVANCE_MS`), derived rather than
/// re-spelled so the cell cannot drift from them when either source
/// constant changes (`Duration` addition is not const, so the sum is
/// spelled in milliseconds).
const IDLE_CLOSE_PROBE_INSTANT_MS: u64 = IDLE_CLOSE_DEADLINE_MS - 10;
const IDLE_CLOSE_PROBE_INSTANT: Duration = Duration::from_millis(IDLE_CLOSE_PROBE_INSTANT_MS);

/// #1250: the same-instant cell's single crossing advance — 20 ms, i.e.
/// 10 ms past the 6.000 s deadline, so the armed idle timer and the peer
/// EOF both become ready inside the one `advance`. Kept independent of
/// the window constants on purpose: that cell pins the same-instant race
/// (whichever `select!` arm the runtime polls first), not a probe window.
const IDLE_CLOSE_RACE_STEP_MS: u64 = 20;
const IDLE_CLOSE_RACE_STEP: Duration = Duration::from_millis(IDLE_CLOSE_RACE_STEP_MS);

/// #1250: the family's window invariants as compile-time checks (u64
/// comparisons only — `Duration` arithmetic is not const): the probe
/// instant stays short of the deadline while the race step still crosses
/// the deadline within one advance of it, and the helpers' advance stays
/// strictly greater than their read window (the helper must cross the
/// read's timeout with the advance).
const _: () = assert!(
    IDLE_CLOSE_PROBE_INSTANT_MS < IDLE_CLOSE_DEADLINE_MS
        && IDLE_CLOSE_DEADLINE_MS < IDLE_CLOSE_PROBE_INSTANT_MS + IDLE_CLOSE_RACE_STEP_MS
);
const _: () = assert!(IDLE_CLOSE_HELPER_ADVANCE_MS > IDLE_CLOSE_HELPER_READ_WINDOW_MS);

/// #1250: probe for data/EOF at the current virtual time. The
/// `IDLE_CLOSE_HELPER_READ_WINDOW_MS` timeout is armed before its own
/// `IDLE_CLOSE_HELPER_ADVANCE_MS` advance, so a still-open read resolves
/// `Elapsed`, while an EOF latched by a close during the advance wins —
/// tokio polls the inner future before the delay.
async fn probe_still_open(client: &mut tokio::io::DuplexStream, what: &str) {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let mut probe = [0u8; 1];
    let pending = timeout(
        Duration::from_millis(IDLE_CLOSE_HELPER_READ_WINDOW_MS),
        client.read(&mut probe),
    );
    tokio::time::advance(Duration::from_millis(IDLE_CLOSE_HELPER_ADVANCE_MS)).await;
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
/// read is itself bounded by an `IDLE_CLOSE_HELPER_READ_WINDOW_MS` timeout
/// and one more `IDLE_CLOSE_HELPER_ADVANCE_MS` advance: a connection still
/// open past the expected deadline fails here with the cell's own message
/// instead of hanging until the harness wall guard.
async fn eof_after_idle_close(client: &mut tokio::io::DuplexStream, elapsed: Duration, what: &str) {
    tokio::time::advance(elapsed).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let mut probe = [0u8; 1];
    let pending = timeout(
        Duration::from_millis(IDLE_CLOSE_HELPER_READ_WINDOW_MS),
        client.read(&mut probe),
    );
    tokio::time::advance(Duration::from_millis(IDLE_CLOSE_HELPER_ADVANCE_MS)).await;
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
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
        ));
        client
            .write_all(&latin1_bytes("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1250 write the health request");
        let response = read_response(&mut client, "health").await;
        assert!(
            response.to_ascii_lowercase().contains("keep-alive: timeout=5\r"),
            "#1250 the advertisement stays the bare keepAliveTimeout: {response:?}"
        );
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the idle connection stays open at +5.99 s: Node closes at keepAliveTimeout + the fixed 1 s keepAliveTimeoutBuffer (6 s), never at the bare 5 s or a shortened buffer",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
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
            None,
            None,
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
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the streaming connection stays open at +5.99 s after its body ended: Node arms the idle deadline at response finish, not at handler return",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
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
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
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
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the second response's connection stays open at +5.99 s past the refreshed deadline",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
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
            webhook: None,
            api_web: None,
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
            None,
            None,
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
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the served connection after a held handler stays open at +5.99 s",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "the idle close lands at 6 s after the held handler's response, not any later",
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
/// served. The first head's 60 s `headersTimeout` bound itself is #1292's
/// `unserved_head_headers_timeout_408s_like_ts`; this cell stays inside it.
/// Binds the negative control `native-http-unserved-read-arms-idle`.
#[tokio::test(start_paused = true)]
async fn idle_state_no_request_stays_open() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
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
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the segmented-head connection stays open at +5.99 s after its second response",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "the idle close lands at 6 s after the segmented-head reuse response, not any later",
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
            None,
            None,
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
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "the pipelined connection stays open at +5.99 s after the second response",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "the idle close lands at 6 s after the second pipelined response, not any later",
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

/// #1366 (matrix S4×E5, the sub-grid #1250 fix-r1 deferred): a client that
/// half-closes while the response body is held mid-stream — the
/// release-channel hold of `idle_close_waits_for_streaming_body` — ends the
/// connection through the connection future itself: the still-readable
/// half-closed client receives the rest of the released body, the connection
/// future resolves `Ok` and joins without a single virtual advance (no timer
/// is armed or consulted), and the in-flight slot the held body kept is gone
/// with it — the terminator plus the EOF leave nothing pending on the socket.
/// TS matrix note (Node v22.22.0 probe
/// `_tmp/20261108-issue-1366-r1/probe-s4e5-held-body-halfclose.out`, bare
/// `http.createServer` = the TS GatewayServer's configuration): Node destroys
/// the server socket at the FIN itself (+14 ms, response truncated — the held
/// bytes never reach the client), while this lane completes the held body to
/// the still-reading half-closed peer before the EOF; the connection-future
/// outcome both share — `Ok`, no hang, no residual timer — is what this cell
/// pins (the byte-level truncation difference is recorded in #1366's RESULT,
/// outside this test-only change's scope). Aligned cell; no dedicated
/// mutant: a slot the held body's end does not release is not observable
/// here — the connection future ends at the peer EOF regardless of the
/// counter, and the per-connection `in_flight` dies with the task — while the
/// S4×E3/E4 cell `idle_close_waits_for_streaming_body` (bound to
/// `native-http-inflight-body-off`) is the one that fires on it (读码推断:
/// `IdleBody::release` / `InFlightRequest::drop`, the `in_flight` `Arc` is
/// created per connection).
#[tokio::test(start_paused = true)]
async fn client_close_during_held_body_resolves_without_timer() {
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
            None,
            None,
        ));
        client
            .write_all(&latin1_bytes("GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
            .await
            .expect("#1366 write the held-body request");
        let mut buffer = Vec::new();
        read_until(&mut buffer, &mut client, "part1-", "#1366 held body first chunk").await;
        client
            .shutdown()
            .await
            .expect("#1366 half-close the connection during the held body");
        release_tx.send_replace(true);
        // No advance(): like the S5×E5 cell, the connection future must end
        // without any timer — the body's completion plus the peer EOF alone.
        let served = task.await.expect("#1366 held-body close task join");
        assert!(
            served.is_ok(),
            "#1366 the held-body connection future resolves Ok once the half-closed peer ends it: {served:?}"
        );
        // The in-flight slot the held body kept is released on the body's own
        // end, so drain the still-readable half-closed client to the EOF: the
        // terminator proves the body ended (the path that drops the slot), the
        // EOF that nothing is left pending on the socket. The 50 ms timeout is
        // only the failure bound — the bytes are already buffered, so no
        // virtual time moves.
        let mut tail = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            match timeout(Duration::from_millis(50), client.read(&mut chunk)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(count)) => tail.extend_from_slice(&chunk[..count]),
                Ok(Err(error)) => panic!("#1366 held-body tail read: {error}"),
                Err(_) => panic!(
                    "#1366 the half-closed client never reached EOF after the connection ended"
                ),
            }
        }
        let tail = latin1_string(&tail);
        assert!(
            tail.ends_with("part2\r\n0\r\n\r\n"),
            "#1366 the held body completed to the half-closed client before the EOF: {tail:?}"
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
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
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
/// client going away becoming ready at the same virtual moment — at the
/// family probe's instant (`IDLE_CLOSE_PROBE_INSTANT`, 10 ms shy of the 6 s
/// deadline) the client half-closes, then one `IDLE_CLOSE_RACE_STEP` advance
/// makes the idle timer and the peer EOF ready together. Whichever select!
/// arm the runtime polls first, the serve future resolves `Ok` and never
/// hangs (aligned cell; no dedicated mutant: both arms converge on the same
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
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1250 write the health request");
        read_response(&mut client, "same-instant health").await;
        tokio::time::advance(IDLE_CLOSE_PROBE_INSTANT).await;
        client
            .shutdown()
            .await
            .expect("#1250 half-close 10 ms shy of the idle deadline");
        // Both the idle timer (deadline at +6 s) and the connection future
        // (peer EOF) become ready inside this single advance.
        tokio::time::advance(IDLE_CLOSE_RACE_STEP).await;
        let served = task.await.expect("#1250 same-instant task join");
        assert!(
            served.is_ok(),
            "#1250 the same-instant deadline/client-close connection resolves Ok either way: {served:?}"
        );
    })
    .await;
}

/// #1292: read the 51-byte headersTimeout 408 and the EOF that follows it,
/// inside the same windowed probe pattern as `probe_still_open`: a missing
/// 408 (the `native-http-headers-timeout-off` mutant) fails at this cell's
/// own message instead of hanging the harness.
async fn headers_timeout_408_arrives(client: &mut tokio::io::DuplexStream, what: &str) {
    let expected = "HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n";
    let mut buffer = Vec::new();
    while buffer.len() < expected.len() {
        let mut chunk = [0u8; 64];
        let pending = timeout(Duration::from_millis(50), client.read(&mut chunk));
        tokio::time::advance(Duration::from_millis(60)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        match pending.await {
            Ok(Ok(0)) => panic!(
                "#1292 {what}: EOF before the 408 arrived (buffer so far: {:?})",
                latin1_string(&buffer)
            ),
            Ok(Ok(count)) => buffer.extend_from_slice(&chunk[..count]),
            Ok(Err(error)) => panic!("#1292 {what}: {error}"),
            Err(_no_408) => {
                panic!("#1292 {what}: the 51-byte 408 never arrived at the headersTimeout deadline")
            }
        }
    }
    let text = latin1_string(&buffer);
    assert!(
        text == expected,
        "#1292 {what}: the bytes are exactly Node's canned 51-byte 408, got {text:?}"
    );
    let mut tail = [0u8; 1];
    let pending = timeout(Duration::from_millis(50), client.read(&mut tail));
    tokio::time::advance(Duration::from_millis(60)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    match pending.await {
        Ok(Ok(0)) => {}
        other => panic!(
            "#1292 {what}: the connection closes after the 408 like Node's destroy(), got {other:?}"
        ),
    }
}

/// #1292 fix-r2: the mid-stream close-only expiry — the stream ends at the
/// deadline's grid landing with no 408 bytes in front of the EOF, Node's
/// `socketOnError` destroying a socket whose in-flight response already sent
/// its head (`probe-g2.out`/`probe-c2.out`). A stray 408 (the
/// `native-http-408-into-started-response` mutant) fails with the bytes read;
/// a missing close (the `native-http-mid-stream-head-anchor-off` and
/// `native-http-headers-timeout-busy-blocks-head` mutants) fails at the
/// window budget.
async fn stream_ends_silently_at_grid_point(client: &mut tokio::io::DuplexStream, what: &str) {
    let mut buffer = Vec::new();
    for _ in 0..16 {
        let mut chunk = [0u8; 64];
        let pending = timeout(Duration::from_millis(50), client.read(&mut chunk));
        tokio::time::advance(Duration::from_millis(60)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        match pending.await {
            Ok(Ok(0)) => {
                assert!(
                    buffer.is_empty(),
                    "#1292 {what}: the close-only end must carry no bytes, got {:?}",
                    latin1_string(&buffer)
                );
                return;
            }
            Ok(Ok(count)) => buffer.extend_from_slice(&chunk[..count]),
            Ok(Err(error)) => panic!("#1292 {what}: {error}"),
            Err(_open) => {}
        }
    }
    panic!("#1292 {what}: the streaming connection was never closed at the grid point");
}

/// #1292: the connection future's own result once the 408 close (or a peer
/// disconnect, the same event in production) ended the connection: a head
/// that had started arriving is reported as `IncompleteMessage`, a connection
/// that never sent a byte ends as a clean close. The wire bytes and the
/// close are what the Node parity pins; this is only the local shape of the
/// same end.
fn ended_by_408_close(served: Result<(), hyper::Error>, what: &str) {
    match served {
        Ok(()) => {}
        Err(error) if error.is_incomplete_message() => {}
        other => panic!("#1292 {what}, got {other:?}"),
    }
}

/// #1517: probe at the current virtual time while the response tail is stuck
/// in hyper's write buffer — unlike `probe_still_open`, the stuck bytes are
/// the expected reading (the client is the backpressure), and only an EOF
/// fails: the tail's close owns the connection at its grid landing, never
/// before it.
async fn probe_open_with_stuck_tail(
    buffer: &mut Vec<u8>,
    client: &mut tokio::io::DuplexStream,
    what: &str,
) {
    let mut chunk = [0u8; 64];
    let pending = timeout(Duration::from_millis(50), client.read(&mut chunk));
    tokio::time::advance(Duration::from_millis(60)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    match pending.await {
        Ok(Ok(0)) => panic!("#1517 {what}"),
        Ok(Ok(count)) => buffer.extend_from_slice(&chunk[..count]),
        Ok(Err(error)) => panic!("#1517 {what}: {error}"),
        Err(_open) => {}
    }
}

/// #1517: read everything the connection still offers until its close,
/// inside the same windowed probe pattern as `stream_ends_silently_at_grid_point`,
/// but with bytes allowed — the verdict is what they are: Node's destroy
/// leaves the flushed prefix in front of the EOF and never the 51-byte 408
/// (`probe-1517.out` T1/T2), and it cuts the tail mid-chunk, so the chunked
/// terminator never arrives either. A stray 408 (the
/// `native-http-408-into-unflushed-tail` mutant) fails with the bytes read.
async fn drain_to_close_without_408(client: &mut tokio::io::DuplexStream, mut buffer: Vec<u8>) {
    let mut closed = false;
    for _ in 0..24 {
        let mut chunk = [0u8; 256];
        let pending = timeout(Duration::from_millis(50), client.read(&mut chunk));
        tokio::time::advance(Duration::from_millis(60)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        match pending.await {
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(count)) => buffer.extend_from_slice(&chunk[..count]),
            Ok(Err(error)) => panic!("#1517 the unflushed-tail drain: {error}"),
            Err(_open) => {}
        }
    }
    assert!(
        closed,
        "#1517 the unflushed-tail drain: the connection was never closed after its grid landing (buffer so far: {:?})",
        latin1_string(&buffer)
    );
    let text = latin1_string(&buffer);
    assert!(
        text.starts_with("HTTP/1.1 200 OK\r\n"),
        "#1517 the unflushed-tail response head is the flushed 200, got {text:?}"
    );
    assert!(
        text.contains("part1-"),
        "#1517 the flushed prefix carries the first chunk, got {text:?}"
    );
    assert!(
        !text.contains("HTTP/1.1 408"),
        "#1517 the unflushed-tail expiry: the unflushed tail never carries the 51-byte 408 — Node destroys the started response instead, got {text:?}"
    );
    assert!(
        !text.ends_with("0\r\n\r\n"),
        "#1517 the unflushed tail is cut at the close like Node's destroy(), the chunked terminator never arrives, got {text:?}"
    );
}

/// #1292 (matrix M-7 rows 1–2, probes `b1-T1a.out`/`b1-T1b.out`): Node's
/// `headersTimeout` (60 s default) bounds the still-unserved first head — a
/// partial head and even a zero-byte connection are answered with the
/// 51-byte 408 `Connection: close` and closed; nothing is answered before
/// the deadline. Binds the negative control
/// `native-http-headers-timeout-off` (the timer disabled, so the 408 never
/// arrives).
#[tokio::test(start_paused = true)]
async fn unserved_head_headers_timeout_408s_like_ts() {
    manual_clock(async {
        // T1a: a partial first head.
        {
            let (server, mut client) = tokio::io::duplex(4096);
            let (_stop, stop_rx) = watch::channel(false);
            let config = Config {
                token: "fixture-token".into(),
                loopback: true,
                static_root: None,
                webhook: None,
                api_web: None,
            };
            let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
                server,
                move |request, _lease| {
                    let config = config.clone();
                    async move { serve(&request, &config).await }
                },
                stop_rx,
                None,
                None,
            ));
            client
                .write_all(&latin1_bytes(
                    "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
                ))
                .await
                .expect("#1292 write the partial first head");
            // Let the serve task run once before the advance: the deadline is
            // anchored at the head's first byte (fix-r2 `probe-c1.out` —
            // Node's `on_message_begin`; here it coincides with the
            // connection's start), so advancing first would push it out to
            // advance + 60 s and the 408 would never land.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_millis(59_800)).await;
            probe_still_open(
                &mut client,
                "#1292 the partial first head stays unanswered before the 60 s headersTimeout",
            )
            .await;
            tokio::time::advance(Duration::from_millis(200)).await;
            headers_timeout_408_arrives(&mut client, "partial first head").await;
            let served = task.await.expect("#1292 partial-head task join");
            ended_by_408_close(
                served,
                "the partial-head connection future ends at the 408 close",
            );
        }
        // T1b: a connection that never sends a byte.
        {
            let (server, mut client) = tokio::io::duplex(4096);
            let (_stop, stop_rx) = watch::channel(false);
            let config = Config {
                token: "fixture-token".into(),
                loopback: true,
                static_root: None,
                webhook: None,
                api_web: None,
            };
            let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
                server,
                move |request, _lease| {
                    let config = config.clone();
                    async move { serve(&request, &config).await }
                },
                stop_rx,
                None,
                None,
            ));
            // Let the serve task run once before the advance: the deadline is
            // armed at the connection's first poll (a zero-byte connection
            // has no first byte to anchor, so Node counts the 60 s from the
            // connection's accept — `b1-T1b.out`), so advancing first would
            // push it out to advance + 60 s and the 408 would never land.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_millis(59_800)).await;
            probe_still_open(
                &mut client,
                "#1292 the zero-byte connection stays unanswered before the 60 s headersTimeout",
            )
            .await;
            tokio::time::advance(Duration::from_millis(200)).await;
            headers_timeout_408_arrives(&mut client, "zero-byte connection").await;
            let served = task.await.expect("#1292 zero-byte task join");
            ended_by_408_close(
                served,
                "the zero-byte connection future ends at the 408 close",
            );
        }
    })
    .await;
}

/// #1292 (matrix M-7 row 2, probe `b1-T2.out`): a served connection whose
/// follow-up head arrives and then goes silent is ended by the 6 s
/// keep-alive idle close, never by a 408 — the deadline re-armed for the
/// follow-up head (fix-r1, 60 s) is far behind the idle close no further
/// segment refreshes.
#[tokio::test(start_paused = true)]
async fn served_follow_up_head_silent_ends_at_idle_close() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            None,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the health request");
        read_response(&mut client, "follow-up-head health").await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the partial follow-up head");
        // Let the serve task read the segment before the advance: the read
        // refreshes the idle deadline at this instant, and the close must land
        // at last activity + 6 s with no 408 in front of it — the re-armed
        // deadline (head first byte + 60 s) never gets near a silent head.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "#1292 the silent partial follow-up head stays unanswered: the 6 s idle close owns it",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "#1292 the served connection ends at the 6 s idle close with no 408 first, not any later",
        )
        .await;
        let served = task.await.expect("#1292 follow-up-head task join");
        assert!(
            served.is_ok(),
            "#1292 the follow-up-head connection future resolves Ok once the idle close ended it: {served:?}"
        );
    })
    .await;
}

/// #1292 fix-r1 (matrix M4, probes `probe-p11.out`/`probe-p12.out`): Node
/// re-arms `headersTimeout` per request — after a served first response, a
/// follow-up head that keeps dribbling one byte per second never lets the
/// 6 s idle close land (every segment refreshes it,
/// `node-dribble-valid-segments.out`) yet is answered with the 51-byte 408
/// at the head's first byte + 60 s. The follow-up head starts 5 s into the
/// connection's life, so the re-armed deadline (+65 s) is distinct from the
/// pre-fix-r1 arm (connection start + 60 s): the probes bracket the old
/// arm's instant before crossing the re-armed one. Like the real TS server,
/// the 408 never lands at the deadline itself — the connections checker's
/// 30 s grid (anchored at the listener's bind) quantizes +65 s up to the
/// next grid point (+90 s), and the probes at the deadline and at +80 s pin
/// that nothing fires in between. Binds the negative control
/// `native-http-headers-rearm-off` (the per-request re-arm removed, so the
/// once-armed connection-start deadline is consumed by the served first
/// request and the dribble hangs past every deadline with no 408) and kills
/// `native-http-headers-grid-off` at the +65 s probe (deadline mode would
/// answer the 408 exactly there).
#[tokio::test(start_paused = true)]
async fn served_follow_up_head_dribble_408s_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the first health request");
        read_response(&mut client, "dribble first health").await;
        // The follow-up head starts 5 s in — inside the 6 s keep-alive
        // window, late enough that its re-armed deadline (first byte + 60 s
        // = +65 s) is measurable against the pre-fix-r1 connection-start arm
        // (+60 s).
        tokio::time::advance(Duration::from_secs(5)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the partial follow-up head");
        // Let the serve task read the first segment: it re-arms the deadline
        // at the head's first byte — the request's own headersTimeout start.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // Dribble one byte per second: each segment refreshes the idle
        // deadline (never its 6 s close) but must not defer the re-armed
        // 60 s deadline. The writes are error-tolerant: the grid-off
        // negative control (deadline mode) closes the connection exactly at
        // the +65 s iteration, and the cell's verdict must come from the
        // probes below, not from a failed dribble write.
        for second in 6..=89u64 {
            tokio::time::advance(Duration::from_secs(1)).await;
            let _ = client.write_all(&latin1_bytes("X")).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            if second == 59 {
                // t = +59 s: still unanswered just before the pre-fix-r1
                // arm's instant (the re-armed deadline is +65 s).
                probe_still_open(
                    &mut client,
                    "#1292 the dribbling follow-up head stays unanswered just before the pre-fix-r1 arm's 60 s instant",
                )
                .await;
            }
            if second == 61 {
                // t = +61 s: past the connection-start instant with the
                // re-armed deadline still pending — only the per-request
                // re-arm keeps a timer alive here at all.
                probe_still_open(
                    &mut client,
                    "#1292 the dribbling follow-up head stays unanswered past the pre-fix-r1 arm's 60 s instant",
                )
                .await;
            }
            if second == 65 {
                // t = +65 s: the re-armed deadline itself. Nothing fires
                // here — the 408 quantizes up to the next checker grid
                // point (+90 s), never landing at the deadline.
                probe_still_open(
                    &mut client,
                    "#1292 the dribbling follow-up head stays unanswered at the re-armed deadline itself: the 408 lands on the next connections-checker grid point (+90 s), never at the deadline",
                )
                .await;
            }
            if second == 80 {
                // t = +80 s: between the deadline and its grid landing.
                probe_still_open(
                    &mut client,
                    "#1292 the dribbling follow-up head stays unanswered between the re-armed deadline and its grid landing",
                )
                .await;
            }
        }
        // t ≈ +89.2 s (the probes each add 60 ms): still nothing, just
        // before the grid landing.
        tokio::time::advance(Duration::from_millis(600)).await;
        probe_still_open(
            &mut client,
            "#1292 the dribbling follow-up head stays unanswered just before the +90 s grid landing",
        )
        .await;
        tokio::time::advance(Duration::from_millis(200)).await;
        headers_timeout_408_arrives(&mut client, "dribbled follow-up head").await;
        let served = task.await.expect("#1292 dribble task join");
        ended_by_408_close(
            served,
            "the dribble connection future ends at the 408 close",
        );
    })
    .await;
}

/// #1292 fix-r1 (ADDENDUM, probe `probe-p12.out` cells p12a/p12b): the
/// 408's landing is the connections checker's grid — a 30 s interval
/// anchored at the listener's bind — not the connection's own start and
/// not the deadline itself. A connection whose partial head begins 15 s
/// after the bind carries a +75 s deadline that must not land at +75 s:
/// it quantizes up to the anchor's next grid point (+90 s). The probes
/// bracket the deadline (+74 s) and the gap before the landing (+89 s):
/// nothing may answer in between. Binds the negative control
/// `native-http-headers-grid-off` (deadline mode: the 408 fires at +75 s
/// and the +89 s probe reads it).
#[tokio::test(start_paused = true)]
async fn headers_timeout_lands_on_checker_grid_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        // The connection begins 15 s after the listener's bind: its own
        // deadline (+75 s) is off the anchor's phase, so only the bind
        // grid can land the 408 at +90 s — a per-connection 30 s alignment
        // or the deadline itself would answer earlier.
        tokio::time::advance(Duration::from_secs(15)).await;
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the phase-shifted partial head");
        // Let the serve task run once before the advances: the deadline is
        // armed at the connection's first poll, so advancing first would
        // push it out and the 408 would never land.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t = +74 s: still unanswered just before the deadline.
        tokio::time::advance(Duration::from_millis(59_400)).await;
        probe_still_open(
            &mut client,
            "#1292 the phase-shifted partial head stays unanswered just before its +75 s deadline",
        )
        .await;
        // t = +89 s: past the deadline, still nothing — the landing is the
        // anchor's next grid point at +90 s, not the deadline.
        tokio::time::advance(Duration::from_millis(14_900)).await;
        probe_still_open(
            &mut client,
            "#1292 the phase-shifted partial head stays unanswered past its deadline and before its +90 s grid landing",
        )
        .await;
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "phase-shifted partial head").await;
        let served = task.await.expect("#1292 phase-shifted task join");
        ended_by_408_close(
            served,
            "the phase-shifted connection future ends at the 408 close",
        );
    })
    .await;
}

/// #1292 fix-r2 (review #1, probe `probe-c1.out`): the connection's *first*
/// request head anchors its own `headersTimeout` at its first byte — Node's
/// `on_message_begin` — not at the connection's accept. Connect, stay silent
/// for 30 s, then send an incomplete head: the head's deadline (+90 s) lands
/// exactly on the checker's grid, so the strictly-greater comparison holds it
/// for the next point and the 51-byte 408 arrives at +120 s (`probe-c1.out`:
/// Node answered at connect + 120.001 s, head age 90.001 s). The probes
/// bracket the deadline instant, the grid point both anchors' deadlines
/// quantize around, and the landing. Binds the negative control
/// `native-http-first-head-anchor-off` (the pre-fix-r2 connect anchor for the
/// first head: its on-grid +60 s deadline lands the 408 at +90 s, where the
/// probe reads it) and kills `native-http-headers-grid-inclusive` (ceil
/// semantics fire the head's on-grid +90 s deadline at +90 s itself).
#[tokio::test(start_paused = true)]
async fn first_head_delayed_anchors_at_its_first_byte_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        // Let the serve task arm the connect-anchored deadline first (t = 0),
        // so the 30 s silence that follows is part of the head's wait, not of
        // the connection setup.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(30)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the delayed partial first head");
        // Let the serve task read the segment: the head's first byte is its
        // own headersTimeout start (+30 s + 60 s = +90 s, on-grid → the
        // strictly-greater comparison lands it at +120 s).
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t = +59.4 s: before either anchor's deadline instant.
        tokio::time::advance(Duration::from_millis(29_340)).await;
        probe_still_open(
            &mut client,
            "#1292 the delayed first head stays unanswered before the 60 s deadline instants",
        )
        .await;
        // t = +90.6 s: past the connect anchor's on-grid landing (+90 s) and
        // past the head's own on-grid deadline (+90 s) — both must stay
        // silent; only the next point answers.
        tokio::time::advance(Duration::from_millis(31_200)).await;
        probe_still_open(
            &mut client,
            "#1292 the delayed first head stays unanswered past both the head's on-grid +90 s deadline and the connect anchor's +90 s landing",
        )
        .await;
        // t = +119.4 s: just before the head's own grid landing.
        tokio::time::advance(Duration::from_millis(28_740)).await;
        probe_still_open(
            &mut client,
            "#1292 the delayed first head stays unanswered just before its +120 s grid landing",
        )
        .await;
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "delayed first head").await;
        let served = task.await.expect("#1292 delayed-first-head task join");
        ended_by_408_close(
            served,
            "the delayed-first-head connection future ends at the 408 close",
        );
    })
    .await;
}

/// #1292 fix-r2 (review #2, probes `probe-g2.out`/`probe-c2.out`): a follow-up
/// head that starts arriving while the previous response is still streaming is
/// bounded from its own first byte — and when that deadline expires
/// mid-stream, Node's `socketOnError` destroys the socket without the
/// 51-byte 408, because the in-flight response has already sent its head
/// (`!_httpMessage._headerSent`). Here the response holds mid-body from +1 s
/// — where the half head arrives — and the head's deadline (+61 s) quantizes
/// to the +90 s grid point: the stream must end there silently, with no 408
/// bytes in front of the EOF. Binds the negative control
/// `native-http-mid-stream-head-anchor-off` (no mid-stream anchor: nothing
/// ever fires and the close-only window budget fails), kills
/// `native-http-headers-timeout-busy-blocks-head` (the in-flight check
/// disarming the started head's deadline: the stream is never closed) and
/// `native-http-408-into-started-response` (the 408 written into the started
/// response: the EOF read sees its bytes).
#[tokio::test(start_paused = true)]
async fn mid_stream_head_timeout_closes_stream_silently_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
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
            None,
            Some(grid_anchor),
        ));
        client
            .write_all(&latin1_bytes(
                "GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the streaming request");
        let mut buffer = Vec::new();
        read_until(&mut buffer, &mut client, "part1-", "streaming first chunk").await;
        // t = +1 s: the follow-up head's first byte, while the response body
        // is held mid-stream. Its deadline (+61 s) quantizes to +90 s.
        tokio::time::advance(Duration::from_millis(1_000)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the mid-stream half head");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t ≈ +60.5 s: just before the mid-stream head's own deadline.
        tokio::time::advance(Duration::from_millis(59_440)).await;
        probe_still_open(
            &mut client,
            "#1292 the streaming response is not interrupted before the mid-stream head's deadline",
        )
        .await;
        // t ≈ +89.4 s: past the deadline, the stream still runs — the landing
        // is the +90 s grid point.
        tokio::time::advance(Duration::from_millis(28_780)).await;
        probe_still_open(
            &mut client,
            "#1292 the streaming response runs on past the deadline, until its +90 s grid landing",
        )
        .await;
        stream_ends_silently_at_grid_point(
            &mut client,
            "the mid-stream head's deadline closes the streaming connection without a 408",
        )
        .await;
        // The release sender outlives the connection (it drops at scope end):
        // dropping it earlier would fail the held body's wait instead of
        // leaving the grid-point close as the cell's verdict.
        let _release_hold = release_tx;
        let served = task.await.expect("#1292 mid-stream-head task join");
        ended_by_408_close(
            served,
            "the mid-stream-head connection future ends at the silent close",
        );
    })
    .await;
}

/// #1292 fix-r2 (review #2 corollary, probe `probe-g4.out`): the same
/// mid-stream half head against a response that has written nothing — Node's
/// `socketOnError` guard is `_headerSent`, not "a response exists": the
/// 51-byte 408 is written at the +90 s grid landing. The handler holds the
/// first request's response from +0 s; the half head arrives at +1 s and is
/// still the one bounded (its deadline +61 s → +90 s). Re-kills
/// `native-http-mid-stream-head-anchor-off` and
/// `native-http-headers-timeout-busy-blocks-head` (no 408 ever arrives).
#[tokio::test(start_paused = true)]
async fn held_response_mid_stream_head_still_408s_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (_release_tx, release_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                let mut release = release_rx.clone();
                async move {
                    release
                        .wait_for(|released| *released)
                        .await
                        .expect("#1292 hold signal intact");
                    serve(&request, &config).await
                }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the held request");
        // Let the serve task parse the head before the advance: the handler
        // holds with no bytes written, so the in-flight response has not sent
        // its head.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t = +1 s: the follow-up head's first byte, while the held request
        // is still in service. Its deadline (+61 s) quantizes to +90 s.
        tokio::time::advance(Duration::from_millis(1_000)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the mid-stream half head");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t ≈ +60.5 s: just before the mid-stream head's own deadline.
        tokio::time::advance(Duration::from_millis(59_440)).await;
        probe_still_open(
            &mut client,
            "#1292 the held response is not interrupted before the mid-stream head's deadline",
        )
        .await;
        // t ≈ +89.4 s: past the deadline, still nothing — the landing is the
        // +90 s grid point.
        tokio::time::advance(Duration::from_millis(28_780)).await;
        probe_still_open(
            &mut client,
            "#1292 the held request stays unanswered past the deadline, until its +90 s grid landing",
        )
        .await;
        tokio::time::advance(Duration::from_millis(600)).await;
        headers_timeout_408_arrives(&mut client, "mid-stream head against a held response").await;
        let served = task.await.expect("#1292 held-response task join");
        ended_by_408_close(
            served,
            "the held-response connection future ends at the 408 close",
        );
    })
    .await;
}

/// #1292 fix-r2 (DS r2 leftover ①, three-request pipeline): the middle
/// request's response streams across the third head's deadline — the third
/// request's half head anchors at its own first byte (+25 s → +85 s → the
/// +90 s grid point), and when the second response finishes at +88 s the
/// re-arm must keep that deadline (Node's `last_message_start_` was set at the
/// head's first byte; the response ending does not move it): the 51-byte 408
/// arrives at +90 s, before the response-2 finish's idle close (+94 s). The
/// pre-fix-r2 re-arm-from-finish would push the deadline to +148 s → +150 s,
/// and the idle close would own the connection at +94 s with no 408 at all.
/// Binds the negative control `native-http-rearm-resets-started-head` and
/// re-kills `native-http-mid-stream-head-anchor-off` (no mid-stream anchor:
/// the finish at +88 s arms from its own instant instead).
#[tokio::test(start_paused = true)]
async fn pipelined_mid_stream_head_keeps_own_deadline_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                let release = release_rx.clone();
                async move {
                    if request.uri().path() == "/held" {
                        hyper::Response::builder()
                            .status(hyper::StatusCode::OK)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(held_body("part1-", "part2", release))
                            .map_err(std::io::Error::other)
                    } else {
                        serve(&request, &config).await
                    }
                }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        // Request 1: served and read at t ≈ 0.
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the first health request");
        read_response(&mut client, "pipeline first health").await;
        // t = +20 s: request 2, whose response streams one chunk and then
        // holds mid-body.
        tokio::time::advance(Duration::from_secs(20)).await;
        client
            .write_all(&latin1_bytes(
                "GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the streaming second request");
        let mut buffer = Vec::new();
        read_until(&mut buffer, &mut client, "part1-", "pipeline second chunk").await;
        // t = +25 s: the third request's half head, while the second response
        // is held mid-stream. Its deadline (+85 s) quantizes to +90 s.
        tokio::time::advance(Duration::from_secs(5)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the mid-stream third half head");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t ≈ +85.5 s: the third head's own deadline instant — nothing fires
        // there, the landing is the +90 s grid point.
        tokio::time::advance(Duration::from_millis(60_380)).await;
        probe_still_open(
            &mut client,
            "#1292 the second response streams on past the third head's deadline instant",
        )
        .await;
        // t = +88 s: release the second response. Its finish must keep the
        // third head's deadline, not re-arm from its own instant — the idle
        // close it arms (+94 s) must never get near the +90 s landing.
        tokio::time::advance(Duration::from_millis(2_560)).await;
        release_tx.send_replace(true);
        let mut tail = Vec::new();
        read_until(&mut tail, &mut client, "0\r\n\r\n", "pipeline second end").await;
        // t ≈ +89.2 s: just before the grid landing, with the second response
        // fully delivered.
        tokio::time::advance(Duration::from_millis(1_200)).await;
        probe_still_open(
            &mut client,
            "#1292 the finished pipeline stays unanswered just before the third head's +90 s grid landing",
        )
        .await;
        tokio::time::advance(Duration::from_millis(740)).await;
        headers_timeout_408_arrives(&mut client, "pipelined mid-stream third head").await;
        let served = task.await.expect("#1292 pipeline task join");
        ended_by_408_close(
            served,
            "the pipeline connection future ends at the 408 close",
        );
    })
    .await;
}

/// #1517 (the #1292 fix-r2 grid's one uncovered cell): the previous response's
/// body is complete — every frame handed to hyper, the in-flight slot gone —
/// but its tail has not flushed: the client never read, so the 1 KiB duplex
/// keeps the 8 KiB last chunk stuck in hyper's write buffer and the gate's
/// `poll_flush` never returned. Node's expiry guard is the response's own
/// state — `_httpMessage` stays attached until `finish`, which needs the
/// flush (`probe-1517.out` T1: `end()` at +88 s, `writableLength` 2 097 167
/// still unflushed at the +90 s expiry, and the destroy carries no 408; T2
/// pins the same bytes on the real `GatewayServer`) — so the close is silent
/// and the client reads the flushed prefix and then EOF. Binds the negative
/// control `native-http-408-into-unflushed-tail` (the clear back to the
/// in-flight slot alone — the pre-#1517 behavior — so the 408 is armed ahead
/// of the stuck tail and this cell reads its bytes).
#[tokio::test(start_paused = true)]
async fn unflushed_response_tail_headers_timeout_closes_silently_like_ts() {
    manual_clock(async {
        let grid_anchor = tokio::time::Instant::now();
        // The 1 KiB duplex is the cell's point: the 8 KiB last chunk can never
        // flush into it, so the response ends with its tail stuck in hyper's
        // write buffer — the paused probe client of `probe-1517.out` T1.
        let (server, mut client) = tokio::io::duplex(1024);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let last = "x".repeat(8192);
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |_request, _lease| {
                let release = release_rx.clone();
                let last = last.clone();
                async move {
                    hyper::Response::builder()
                        .status(hyper::StatusCode::OK)
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .body(held_body("part1-", &last, release))
                        .map_err(std::io::Error::other)
                }
            },
            stop_rx,
            None,
            Some(grid_anchor),
        ));
        client
            .write_all(&latin1_bytes(
                "GET /held HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1517 write the streaming request");
        // The flushed prefix: the head and the first chunk land before
        // anything can stall.
        let mut buffer = Vec::new();
        read_until(
            &mut buffer,
            &mut client,
            "part1-",
            "unflushed-tail first chunk",
        )
        .await;
        // t = +1 s: the follow-up head's first byte, while the response body
        // is still held. Its deadline (+61 s) quantizes to +90 s.
        tokio::time::advance(Duration::from_millis(1_000)).await;
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1517 write the mid-stream half head");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t ≈ +60.4 s: just before the mid-stream head's own deadline.
        tokio::time::advance(Duration::from_millis(59_440)).await;
        probe_still_open(
            &mut client,
            "#1517 the held response is not interrupted before the mid-stream head's deadline",
        )
        .await;
        // t = +88.4 s: release the last chunk. The body ends — every frame is
        // handed to hyper, the in-flight slot is gone — but the tail cannot
        // flush into the full duplex, so the response stays unflushed exactly
        // like Node's backpressured `writableLength`.
        tokio::time::advance(Duration::from_millis(28_000)).await;
        release_tx.send_replace(true);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // t ≈ +89.4 s: past the deadline, before the +90 s grid landing — the
        // stuck tail's bytes are the expected reading, only an early close
        // fails.
        tokio::time::advance(Duration::from_millis(1_000)).await;
        probe_open_with_stuck_tail(
            &mut buffer,
            &mut client,
            "the unflushed tail keeps the connection open until its +90 s grid landing",
        )
        .await;
        // Past the landing: Node destroys the started-but-unflushed response
        // without the 408 — the client reads the flushed prefix and then EOF,
        // and the close cuts the tail mid-chunk.
        tokio::time::advance(Duration::from_millis(600)).await;
        drain_to_close_without_408(&mut client, buffer).await;
        // The release sender outlives the connection (it drops at scope end):
        // dropping it earlier would fail the held body's wait instead of
        // leaving the grid-point close as the cell's verdict.
        let _release_hold = release_tx;
        let served = task.await.expect("#1517 unflushed-tail task join");
        ended_by_408_close(
            served,
            "the unflushed-tail connection future ends at the silent close",
        );
    })
    .await;
}

/// #1292 fix-r1 (matrix M6): while a request is in flight — here a pipelined
/// second request whose response body is held mid-stream — no
/// headersTimeout exists: Node's `requestTimeout` (300 s, registered as not
/// replicated) governs a busy socket, so crossing the re-armed deadline's
/// instant mid-flight answers neither a 408 nor a close, and the held
/// response is delivered normally afterwards.
#[tokio::test(start_paused = true)]
async fn in_flight_request_disarms_rearmed_timeout() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let release_rx = release_rx;
        // Both requests are `/health` (the read helper pins the 200); the
        // counter holds only the second one mid-service.
        let request_count = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                let mut release = release_rx.clone();
                let count = request_count.clone();
                async move {
                    if count.fetch_add(1, Ordering::Relaxed) == 1 {
                        release
                            .wait_for(|released| *released)
                            .await
                            .expect("#1292 hold signal intact");
                    }
                    serve(&request, &config).await
                }
            },
            stop_rx,
            None,
            None,
        ));
        // Pipelined: both requests are on the wire before any response is
        // read, so the second head is parsed from hyper's buffer while the
        // first response streams.
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\nGET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the pipelined requests");
        read_response(&mut client, "pipelined first health").await;
        // Cross the first response's re-armed deadline (its finish + 60 s)
        // while the second request is still in flight: nothing may answer.
        tokio::time::advance(Duration::from_millis(60_500)).await;
        probe_still_open(
            &mut client,
            "#1292 the in-flight second request stays unanswered past the re-armed deadline: requestTimeout owns a busy socket",
        )
        .await;
        release_tx.send_replace(true);
        read_response(&mut client, "held second health").await;
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "#1292 the second response's connection stays open at +5.99 s past its finish",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "#1292 the pipelined connection ends at the 6 s idle close, not any later",
        )
        .await;
        let served = task.await.expect("#1292 pipelined task join");
        assert!(
            served.is_ok(),
            "#1292 the pipelined connection future resolves Ok once the idle close ended it: {served:?}"
        );
    })
    .await;
}

/// #1292 fix-r1 (review P2, opus boundary): the served disarm — once the
/// awaited head has parsed, the request owns the socket: Node's parser
/// leaves the checker's active list on `on_message_complete`, so neither a
/// served socket nor one whose response is still streaming is ever
/// header-timeout-eligible. Here the first request's response is held
/// mid-stream across the deadline armed at the connection's start (+60 s):
/// no 408 may interrupt it, and the response completes normally once
/// released; the connection then ends at its own 6 s idle close. Binds the
/// negative control `native-http-headers-timeout-ignores-served` (the
/// served/busy disarm removed, so the 408 fires at +60 s straight into the
/// held response and the probe reads it).
#[tokio::test(start_paused = true)]
async fn held_response_gets_no_headers_timeout() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (_stop, stop_rx) = watch::channel(false);
        let (release_tx, release_rx) = watch::channel(false);
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let release_rx = release_rx;
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                let mut release = release_rx.clone();
                async move {
                    release
                        .wait_for(|released| *released)
                        .await
                        .expect("#1292 hold signal intact");
                    serve(&request, &config).await
                }
            },
            stop_rx,
            None,
            None,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ))
            .await
            .expect("#1292 write the held health request");
        // Let the serve task parse the head before the advance: the parse
        // itself is what takes the socket off the checker's active list
        // (served_heads passes the disarm watermark), so the deadline armed
        // at the connection's start must already be inert.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        // Cross the armed deadline (+60 s) mid-response: nothing may answer.
        tokio::time::advance(Duration::from_millis(60_500)).await;
        probe_still_open(
            &mut client,
            "#1292 the held first response gets no 408 past its headersTimeout: the parsed request owns the socket",
        )
        .await;
        release_tx.send_replace(true);
        read_response(&mut client, "held first health").await;
        tokio::time::advance(IDLE_CLOSE_PROBE_ADVANCE).await;
        probe_still_open(
            &mut client,
            "#1292 the held response's connection stays open at +5.99 s past its finish",
        )
        .await;
        eof_after_idle_close(
            &mut client,
            IDLE_CLOSE_TAIL_WINDOW,
            "#1292 the held-response connection ends at the 6 s idle close, not any later",
        )
        .await;
        let served = task.await.expect("#1292 held-response task join");
        assert!(
            served.is_ok(),
            "#1292 the held-response connection future resolves Ok once the idle close ended it: {served:?}"
        );
    })
    .await;
}

/// #1292 (matrix M-7 row 4, probe `b3-T3d.out`): a stop disarms the
/// headersTimeout — `server.close()` clears Node's connections checker, so
/// the still-unserved partial head stays unanswered past the deadline
/// instead of getting its 408.
#[tokio::test(start_paused = true)]
async fn stop_disarms_headers_timeout() {
    manual_clock(async {
        let (server, mut client) = tokio::io::duplex(4096);
        let (stop_tx, stop_rx) = watch::channel(false);
        let stop_count = Arc::new(AtomicUsize::new(0));
        let config = Config {
            token: "fixture-token".into(),
            loopback: true,
            static_root: None,
            webhook: None,
            api_web: None,
        };
        let task = tokio::spawn(http_proxy::serve_connection_native_with_shutdown(
            server,
            move |request, _lease| {
                let config = config.clone();
                async move { serve(&request, &config).await }
            },
            stop_rx,
            Some(stop_count.clone()),
            None,
        ));
        client
            .write_all(&latin1_bytes(
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Len",
            ))
            .await
            .expect("#1292 write the partial head before the stop");
        stop_tx.send_replace(true);
        // A stop needs scheduling, not time: yield until the serving loop has
        // consumed it (the `stop_observed` hook), without drifting the
        // paused clock toward the deadline.
        let mut yields = 0;
        while stop_count.load(Ordering::SeqCst) == 0 {
            yields += 1;
            assert!(
                yields < 10_000,
                "#1292 the partial-head connection's stop was never observed by its serving loop"
            );
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(60_500)).await;
        probe_still_open(
            &mut client,
            "#1292 the stopped partial head stays unanswered past the headersTimeout: close() cleared Node's connections checker",
        )
        .await;
        // One more segment after the stop: a read on a stopped connection
        // must not re-arm the deadline either (the re-arm consults the stop
        // channel), so the connection stays unanswered past a further
        // deadline's worth of time. The write is error-tolerant in case the
        // stopped lane ever closes early — the verdict is the probe's.
        let _ = client.write_all(&latin1_bytes("X")).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(60_500)).await;
        probe_still_open(
            &mut client,
            "#1292 the stopped partial head stays unanswered past a post-stop segment: the stop blocks the re-arm too",
        )
        .await;
        drop(client);
        let served = task.await.expect("#1292 stop-disarm task join");
        ended_by_408_close(served, "the stopped partial-head connection future ends at its peer EOF");
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
