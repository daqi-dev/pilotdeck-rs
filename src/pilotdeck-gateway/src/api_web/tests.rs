//! #1348: byte-level replay of the `api-web` golden fixture (every cell one
//! test), the two raw-byte pins the replay normalization cannot express, and
//! the bidirectional HTTP/WS visibility checks. The fixture was recorded
//! against the real TS stack (`startGatewayServer` with a scripted gateway
//! stub keyed on the fixture root, `describeProject` throwing for `boom`);
//! here the same requests go through the production dispatch #2354 wired:
//! the listener's `native_http::serve_with_body` with the api-web group
//! mounted inside the router config, so every cell exercises the same branch
//! the native listener serves (the fallthrough 404s included).
use super::*;
use crate::{http_proxy, native_http, test_baseline};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use serde_json::Value;
use std::{
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

// #1766: the response-normalization parser this file used to carry as the
// third copy of the #1209/#1350 rules, extracted to the shared module; the
// api-web body masks below stay here.
#[path = "../../tests/common/replay_normalize.rs"]
mod replay_normalize;
use replay_normalize::find;

const ORACLE: &str = include_str!("../../tests/fixtures/api-web/oracle.json");
/// #1891: kept at the shared 10 s — it only bounds the in-process
/// `TcpStream::connect` to this test's own listener, never a real sidecar.
const DEADLINE: Duration = Duration::from_secs(10);
/// #1891: liveness watchdog for the first response byte, not an assertion
/// about the response. This capture loop is the verbatim sibling of the one
/// the #978 fix relaxed in `module_host::tests`, where a host running eight
/// concurrent lib suites starved the serving task past the old ten seconds
/// (8/48 loaded copies reddened "produced no bytes and stayed open"); the
/// byte oracle and the closed flag are unchanged, and a connection that truly
/// never answers still reds after this margin.
const FIRST_BYTE: Duration = Duration::from_secs(60);
/// The recorder's quiet window: a response is complete after a quarter second
/// of silence while the connection stays open.
const QUIET: Duration = Duration::from_millis(250);

fn oracle() -> Value {
    test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1348 oracle JSON")
}

struct Cell {
    id: String,
    request: Vec<u8>,
    response: String,
    closed: bool,
}

fn cell(id: &str, encoded_root: &str) -> Cell {
    let value = oracle();
    let cells = value["cells"].as_array().expect("#1348 oracle cells array");
    for entry in cells {
        if entry["id"].as_str() == Some(id) {
            return Cell {
                id: id.to_owned(),
                request: entry["request"]
                    .as_str()
                    .expect("#1348 oracle cell request")
                    .replace("<root>", encoded_root)
                    .into_bytes(),
                response: entry["response"]
                    .as_str()
                    .expect("#1348 oracle cell response")
                    .to_owned(),
                closed: entry["closed"]
                    .as_bool()
                    .expect("#1348 oracle cell closed flag"),
            };
        }
    }
    panic!("#1348 cell {id} is missing from the committed oracle");
}

// ------------------------------------------------------------------ fixture

/// The file bodies the oracle's `fixture` block names by byte count: every
/// content the read/tree cells serve, byte-for-byte what the generator wrote.
fn file_body(path: &str) -> Vec<u8> {
    match path {
        "aaa.txt" => b"alpha\n".to_vec(),
        "bbb.js" => b"console.log(2)\n".to_vec(),
        "binary.png" => vec![0, 1, 2, 0xFF],
        "data.bin" => b"hi".to_vec(),
        "empty.txt" => Vec::new(),
        "UPPER.TXT" => b"upper\n".to_vec(),
        "config.env" => b"KEY=1\n".to_vec(),
        "README" => b"no extension\n".to_vec(),
        "big.bin" => vec![0x61; 1024 * 1024 + 1],
        "space name.txt" => b"space\n".to_vec(),
        "sub/child.txt" => b"child\n".to_vec(),
        other => panic!("#1348 unknown fixture file {other:?}"),
    }
}

/// The two mtime literals the oracle's fixture block names.
fn pinned_times(iso: &str) -> rustix::fs::Timestamps {
    let (seconds, nanoseconds) = match iso {
        "2020-01-02T03:04:05.006Z" => (1_577_934_245, 6_000_000),
        "2021-02-03T04:05:06.789Z" => (1_612_325_106, 789_000_000),
        other => panic!("#1348 unknown fixture mtime {other:?}"),
    };
    let stamp = rustix::fs::Timespec {
        tv_sec: seconds,
        tv_nsec: nanoseconds as _,
    };
    rustix::fs::Timestamps {
        last_access: stamp,
        last_modification: stamp,
    }
}

fn pin_mtime(path: &Path, iso: &str) {
    let times = pinned_times(iso);
    rustix::fs::utimensat(rustix::fs::CWD, path, &times, rustix::fs::AtFlags::empty())
        .unwrap_or_else(|error| panic!("#1348 pin mtime of {}: {error}", path.display()));
}

/// The fixture tree of one replay: the temp parent (realpath'd like the
/// generator's `mkdtemp(realpathSync(tmpdir()), …)`) and the `proj` root,
/// created from the oracle's own fixture block with every mtime pinned in
/// the generator's order — files first, directories reversed, the root last.
fn fixture() -> (PathBuf, PathBuf) {
    let scratch = std::env::temp_dir().join(format!("pdrs-1348-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch).expect("#1348 create the fixture scratch");
    let parent = scratch.canonicalize().expect("#1348 realpath the scratch");
    let root = parent.join("proj");
    let recorded = oracle();
    let fixture = &recorded["fixture"];
    let directories = fixture["directories"]
        .as_array()
        .expect("#1348 oracle fixture directories");
    let files = fixture["files"]
        .as_array()
        .expect("#1348 oracle fixture files");
    fs::create_dir_all(&root).expect("#1348 create the fixture root");
    for entry in directories {
        let path = entry["path"]
            .as_str()
            .expect("#1348 oracle fixture directory path");
        fs::create_dir_all(root.join(path))
            .unwrap_or_else(|error| panic!("#1348 create the fixture directory {path}: {error}"));
    }
    for entry in files {
        let path = entry["path"]
            .as_str()
            .expect("#1348 oracle fixture file path");
        let expected = entry["bytes"]
            .as_u64()
            .expect("#1348 oracle fixture file bytes");
        let body = file_body(path);
        assert_eq!(
            body.len() as u64,
            expected,
            "#1348 fixture file {path} byte count matches the oracle"
        );
        let target = root.join(path);
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)
                .unwrap_or_else(|error| panic!("#1348 create the parent of {path}: {error}"));
        }
        fs::write(&target, body)
            .unwrap_or_else(|error| panic!("#1348 write the fixture file {path}: {error}"));
    }
    for entry in fixture["symlinks"]
        .as_array()
        .expect("#1348 oracle fixture symlinks")
    {
        let path = entry["path"]
            .as_str()
            .expect("#1348 oracle fixture symlink path");
        let target = entry["target"]
            .as_str()
            .expect("#1348 oracle fixture symlink target");
        // The recorder's `utimes` follows links, so the link itself is never
        // pinned; the cells read the target through `stat`, which follows too.
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, root.join(path))
            .unwrap_or_else(|error| panic!("#1348 create the fixture symlink {path}: {error}"));
        #[cfg(not(unix))]
        let _ = (path, target);
    }
    for entry in files {
        let path = entry["path"]
            .as_str()
            .expect("#1348 oracle fixture file path");
        let iso = entry["modifiedAt"]
            .as_str()
            .expect("#1348 oracle fixture file mtime");
        pin_mtime(&root.join(path), iso);
    }
    for entry in directories.iter().rev() {
        let path = entry["path"]
            .as_str()
            .expect("#1348 oracle fixture directory path");
        let iso = entry["modifiedAt"]
            .as_str()
            .expect("#1348 oracle fixture directory mtime");
        pin_mtime(&root.join(path), iso);
    }
    pin_mtime(&root, "2021-02-03T04:05:06.789Z");
    (parent, root)
}

/// `encodeURIComponent` of the fixture root: the request target carries
/// this spelling of the root, the same set the recorder expanded `<root>`
/// into.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// -------------------------------------------------------------------- catalog

fn js_object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}

/// The scripted gateway stub the fixture recorded against: one fixed project
/// (the fixture root) and a `describeProject` that throws for `boom`.
/// `list_fails` is the rejecting `listProjects` of residual #2 in
/// `specs/gateway-parity/diffs/A8.4-API-WEB-001.md` — the fixture never
/// recorded that path, so the pin test is the only thing that holds it.
struct ScriptedCatalog {
    root: String,
    list_fails: bool,
}

impl ProjectCatalog for ScriptedCatalog {
    fn list_projects(&self) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>> {
        if self.list_fails {
            return Box::pin(std::future::ready(Err(io::Error::other(
                "catalog exploded",
            ))));
        }
        let root = self.root.clone();
        Box::pin(async move {
            let project = js_object(vec![
                ("projectKey", JsValue::from(root.as_str())),
                ("name", JsValue::from("proj")),
                ("fullPath", JsValue::from(root.as_str())),
                ("sessionCount", JsValue::Number(2.0)),
                ("lastActivity", JsValue::Number(1_234_567_890.0)),
                ("createdAt", JsValue::Number(1_234_500_000.0)),
            ]);
            Ok(js_object(vec![("projects", JsValue::Array(vec![project]))]))
        })
    }

    fn describe_project(
        &self,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>> {
        if key == "boom" {
            return Box::pin(std::future::ready(Err(io::Error::other(
                "catalog exploded",
            ))));
        }
        let key = key.to_owned();
        Box::pin(async move {
            Ok(js_object(vec![
                ("projectKey", JsValue::from(key.as_str())),
                ("name", JsValue::from("described")),
                ("fullPath", JsValue::from(key.as_str())),
                ("sessionCount", JsValue::Number(2.0)),
            ]))
        })
    }
}

// ------------------------------------------------------------------- harness

/// Bind one native listener and write the raw request bytes to it, then
/// capture the raw response bytes like the TS recorder did (quiet window,
/// EOF, or socket error ends the capture). #2354: the api-web group rides
/// inside the router config and the handler is the listener's own
/// `serve_with_body` entry — the same production dispatch
/// `native_ws::serve_socket` calls, not a test-only composition.
async fn exchange(
    config: Config,
    native_config: native_http::Config,
    request: &[u8],
    id: &str,
) -> (Vec<u8>, bool) {
    let native_config = native_http::Config {
        api_web: Some(config),
        ..native_config
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1348 bind listener");
    let bound = listener.local_addr().expect("#1348 listener local address");
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("#1348 accept connection");
            let native_config = native_config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = tokio::sync::watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |request, _lease| {
                        let native_config = native_config.clone();
                        async move { native_http::serve_with_body(request, &native_config).await }
                    },
                    stop,
                    None,
                    None,
                )
                .await;
                if let Err(error) = served {
                    // A handler Err (the describe-catch mutant) is the
                    // deliberate zero-byte close.
                    eprintln!("#1348 api-web connection error: {error}");
                }
            });
        }
    });
    let mut stream = timeout(DEADLINE, TcpStream::connect(bound))
        .await
        .expect("#1348 connect deadline")
        .expect("#1348 connect to api-web listener");
    stream
        .write_all(request)
        .await
        .expect("#1348 write request bytes");
    let captured = capture(&mut stream, id).await;
    server_task.abort();
    captured
}

async fn capture(stream: &mut TcpStream, id: &str) -> (Vec<u8>, bool) {
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
        "#1348 cell {id} produced no bytes and stayed open"
    );
    (out, closed)
}

/// The router inputs of one replay: the scripted catalog over the same
/// fixture root the requests name.
fn replay_config(root: &str) -> Config {
    Config {
        token: "fixture-token".to_owned(),
        catalog: Arc::new(ScriptedCatalog {
            root: root.to_owned(),
            list_fails: false,
        }),
    }
}

fn native_config() -> native_http::Config {
    native_http::Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
        webhook: None,
        control: None,
        api_web: None,
        approvals: None,
        public_host: None,
    }
}

/// One cell through the fixture replay: capture the raw response and compare
/// the normalized bytes and the closed flag against the golden oracle. The
/// oracle stores its latin1-mapped text, so both sides are compared as the
/// wire bytes they map to 1:1.
async fn replay(id: &str) {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let cell = cell(id, &percent_encode(&root_text));
    // The oracle recorded `files-read-round` right after `files-write-round`
    // on the same tree, as two separate requests; the replay writes the same
    // bytes the write cell wrote before reading them back.
    if id == "files-read-round" {
        fs::write(root.join("round.txt"), b"round trip\n")
            .expect("#1348 seed the round-trip file the prior cell wrote");
    }
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        &cell.request,
        &cell.id,
    )
    .await;
    assert_eq!(
        normalize(&raw, false, Some(&root)),
        normalize(&latin1_bytes(&cell.response), false, None),
        "#1348 cell {} normalized response",
        cell.id
    );
    assert_eq!(
        closed, cell.closed,
        "#1348 cell {} connection closed flag",
        cell.id
    );
    // The four judgment families the #1348 RESULT quotes side by side with
    // the oracle text: hit (`projects-ok`), miss (`api-web-other`), method
    // mismatch (`projects-post`) and non-canonical path — the encoded
    // separator (`files-encoded-slash`) plus the bare prefix that the
    // component must decline so the native fallback answers
    // (`api-web-no-slash`). Only the machine root is masked; every other byte
    // is the wire's own. Cargo captures this unless the run passes
    // --nocapture.
    if matches!(
        id,
        "projects-ok"
            | "api-web-other"
            | "projects-post"
            | "files-encoded-slash"
            | "api-web-no-slash"
    ) {
        let masked = latin1_string(&raw)
            .replace(&root_text, "<root>")
            .replace(&percent_encode(&root_text), "<root>");
        println!("#1348 cell {id} rust response bytes: {masked}");
    }
    let _ = fs::remove_dir_all(&parent);
}

macro_rules! replay_tests {
    ($($function:ident => $id:literal),* $(,)?) => {
        /// Every cell id the macro instantiates; a test asserts this plus the
        /// pinned ids is exactly the oracle's cell set, so neither list can
        /// drift alone.
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
    projects_ok => "projects-ok",
    projects_no_auth => "projects-no-auth",
    projects_wrong_token => "projects-wrong-token",
    projects_lower_bearer => "projects-lower-bearer",
    projects_bearer_tab => "projects-bearer-tab",
    projects_bearer_trailing_space => "projects-bearer-trailing-space",
    projects_basic => "projects-basic",
    projects_empty_auth => "projects-empty-auth",
    projects_duplicate_bad_first => "projects-duplicate-bad-first",
    projects_duplicate_good_first => "projects-duplicate-good-first",
    projects_head => "projects-head",
    projects_post => "projects-post",
    projects_put => "projects-put",
    projects_trailing_slash => "projects-trailing-slash",
    api_web_bare => "api-web-bare",
    api_web_no_slash => "api-web-no-slash",
    api_web_other => "api-web-other",
    files_tree_post => "files-tree-post",
    files_unknown_sub => "files-unknown-sub",
    files_encoded_slash => "files-encoded-slash",
    files_write_get => "files-write-get",
    files_read_head => "files-read-head",
    project_describe => "project-describe",
    project_describe_trailing_slash => "project-describe-trailing-slash",
    project_describe_post => "project-describe-post",
    project_describe_badpct => "project-describe-badpct",
    project_describe_plus => "project-describe-plus",
    project_describe_throws => "project-describe-throws",
    files_tree_root => "files-tree-root",
    files_tree_sub => "files-tree-sub",
    files_tree_empty_path => "files-tree-empty-path",
    files_tree_dot => "files-tree-dot",
    files_tree_dotdot_inside => "files-tree-dotdot-inside",
    files_tree_missing => "files-tree-missing",
    files_tree_file => "files-tree-file",
    files_tree_escape => "files-tree-escape",
    files_tree_absolute => "files-tree-absolute",
    files_tree_dotdot_prefix => "files-tree-dotdot-prefix",
    files_read_text => "files-read-text",
    files_read_dot => "files-read-dot",
    files_read_dotdot_inside => "files-read-dotdot-inside",
    files_read_two_paths => "files-read-two-paths",
    files_read_plus_space => "files-read-plus-space",
    files_read_encoded_path => "files-read-encoded-path",
    files_read_binary_ext => "files-read-binary-ext",
    files_read_no_ext => "files-read-no-ext",
    files_read_upper_ext => "files-read-upper-ext",
    files_read_env => "files-read-env",
    files_read_empty_file => "files-read-empty-file",
    files_read_large => "files-read-large",
    files_read_symlink => "files-read-symlink",
    files_read_missing_query => "files-read-missing-query",
    files_read_empty_path => "files-read-empty-path",
    files_read_escape => "files-read-escape",
    files_read_absolute => "files-read-absolute",
    files_read_nonexistent => "files-read-nonexistent",
    files_read_dir => "files-read-dir",
    files_read_bad_percent_path => "files-read-bad-percent-path",
    files_write_text => "files-write-text",
    files_write_base64 => "files-write-base64",
    files_write_base64_lenient => "files-write-base64-lenient",
    files_write_nested => "files-write-nested",
    files_write_extra_field => "files-write-extra-field",
    files_write_bad_json => "files-write-bad-json",
    files_write_empty_body => "files-write-empty-body",
    files_write_missing_path => "files-write-missing-path",
    files_write_empty_path => "files-write-empty-path",
    files_write_nonstring_path => "files-write-nonstring-path",
    files_write_nonstring_content => "files-write-nonstring-content",
    files_write_bad_encoding => "files-write-bad-encoding",
    files_write_null_encoding => "files-write-null-encoding",
    files_write_escape => "files-write-escape",
    files_write_absolute => "files-write-absolute",
    files_write_dotdot_prefix => "files-write-dotdot-prefix",
    files_write_round => "files-write-round",
    files_read_round => "files-read-round",
    keep_alive_reuse => "keep-alive-reuse",
    conn_close => "conn-close",
}

/// The cells pinned raw instead of replayed: their bytes carry a registered
/// transport diff no normalization may swallow.
const PINNED_CELL_IDS: &[&str] = &["http10"];

/// #1348: the replay list plus the pinned list is the oracle's cell set; a
/// cell added to the oracle without a test (or vice versa) fails here.
#[test]
fn replay_cells_cover_exactly_the_oracle() {
    let recorded = oracle();
    let mut oracle_ids: Vec<&str> = recorded["cells"]
        .as_array()
        .expect("#1348 oracle cells array")
        .iter()
        .map(|cell| cell["id"].as_str().expect("#1348 oracle cell id"))
        .collect();
    oracle_ids.sort_unstable();
    let mut covered: Vec<&str> = REPLAY_CELL_IDS.to_vec();
    covered.extend_from_slice(PINNED_CELL_IDS);
    covered.sort_unstable();
    assert_eq!(
        covered, oracle_ids,
        "#1348 replay cell ids and oracle cell ids are the same set"
    );
}

/// #1348: an HTTP/1.0 request is answered with Node's own HTTP/1.1 status
/// line (oracle cell `http10`), while hyper writes its `HTTP/1.0` line — the
/// registered A8.4-API-WEB-001 entry; everything else — the
/// `Connection: close`, the absent framing, the close-delimited body, the
/// closed socket — matches the recorded bytes.
#[tokio::test]
async fn http10_close_delimited_response() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let cell = cell("http10", &percent_encode(&root_text));
    let (bytes, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        &cell.request,
        &cell.id,
    )
    .await;
    let raw = latin1_string(&bytes);
    assert!(
        raw.starts_with("HTTP/1.0 200 OK\r\n"),
        "#1348 pin http10: hyper answers with a 1.0 status line (Node's 1.1 line is the registered diff): {raw:?}"
    );
    assert!(
        raw.contains("\r\nConnection: close\r\n")
            && !raw.to_ascii_lowercase().contains("keep-alive"),
        "#1348 pin http10: the 1.0 response is close with no keep-alive like Node: {raw:?}"
    );
    // #1947 structural pin (ledger A8.3-CH-WEBHOOK-004-R2): this lane's reply
    // is a streamed body — close-delimited already — so there is no `Full`
    // construction point to mutate and no switch can kill this assertion; it
    // pins the existing framing against drift.
    assert!(
        !raw.to_ascii_lowercase().contains("\r\ncontent-length:"),
        "#1947 pin http10: the 1.0 reply is close-delimited, no content-length: {raw:?}"
    );
    let expected = format!(
        "{{\"projects\":[{{\"projectKey\":\"{root_text}\",\"name\":\"proj\",\"fullPath\":\"{root_text}\",\"sessionCount\":2,\"lastActivity\":1234567890,\"createdAt\":1234500000}}]}}"
    );
    assert!(
        raw.ends_with(&expected),
        "#1348 pin http10: the 1.0 body is close-delimited, not chunked: {raw:?}"
    );
    assert!(
        closed,
        "#1348 pin http10: the 1.0 response closes the connection like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1348: a files-branch project key whose percent-encoding is malformed is
/// an unhandled rejection in TS — `decodeURIComponent` sits outside the
/// branch's try, so the connection hangs with zero bytes. Rust answers the
/// 500 `internal_error` instead (the registered A8.4-API-WEB-001 diff).
#[tokio::test]
async fn project_key_malformed_percent_is_500() {
    let config = replay_config("unused");
    let request = b"GET /api/web/projects/%E0%A4%A/files/tree HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(config, native_config(), request, "malformed-key").await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1348 pin malformed project key: the files branch answers the 500, not the TS hang: {response:?}"
    );
    assert!(
        response
            .contains("{\"error\":{\"code\":\"internal_error\",\"message\":\"URI malformed\"}}"),
        "#1348 pin malformed project key: the 500 carries the decode error: {response:?}"
    );
    assert!(
        !closed,
        "#1348 pin malformed project key: the connection does not hang"
    );
}

/// #1348 residual #2 (`specs/gateway-parity/diffs/A8.4-API-WEB-001.md`): TS
/// `httpRouter.ts:39-43` awaits `gateway.listProjects()` with no try/catch and
/// `GatewayServer.ts:64-65` discards the handler promise, so a rejection never
/// reaches the response. Measured on `node v22.22.0` with the same handler
/// shape (`_tmp/20261106-issue-1348/ts-probe3/`, raw logs kept there): zero
/// response bytes in all three configurations — connection held open while the
/// process lives under a swallowing listener, closed within ~5 ms under Node's
/// default throw mode and under the CLI's `shutdownAndExit(1)` listener. Rust
/// answers the generic 500 instead of reproducing either.
#[tokio::test]
async fn catalog_list_failure_is_500() {
    let config = Config {
        token: "fixture-token".to_owned(),
        catalog: Arc::new(ScriptedCatalog {
            root: "unused".to_owned(),
            list_fails: true,
        }),
    };
    let request = b"GET /api/web/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(config, native_config(), request, "list-failure").await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1348 pin listProjects rejection: a rejecting catalog answers the mapped 500, not the TS zero-byte rejection: {response:?}"
    );
    assert!(
        response
            .contains("{\"error\":{\"code\":\"internal_error\",\"message\":\"catalog exploded\"}}"),
        "#1348 pin listProjects rejection: the 500 carries the catalog error text: {response:?}"
    );
    assert!(
        !closed,
        "#1348 pin listProjects rejection: the connection stays usable after the 500"
    );
}

// ------------------------------------------------------- #2354/#1765 wiring

/// #1765: the llhttp-equivalent admission gates run before the api-web route
/// group — a method outside Node's `http.METHODS` table against
/// `/api/web/projects` answers the parser's bare 400 and closes the socket,
/// exactly the TS bytes the probe recorded (Node v22.22.0,
/// `_tmp/20261026-issue-2354/ts-probe/probe-output.txt`, cell
/// `gate-bad-method`), never the group's own JSON.
#[tokio::test]
async fn gate_bad_method_is_parser_bare_400() {
    let request =
        b"FOO /api/web/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(
        replay_config("unused"),
        native_config(),
        request,
        "gate-bad-method",
    )
    .await;
    assert_eq!(
        normalize(&raw, true, None),
        normalize(
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
            true,
            None
        ),
        "#1765 the bad-method gate answers the parser bare 400 before the route group"
    );
    assert!(
        closed,
        "#1765 the bad-method gate closes the connection like Node's parser"
    );
}

/// #1765: an HTTP/1.1 request without a `Host` header against
/// `/api/web/projects` answers the parser's headless 400 — `Connection:
/// close`, a Date and a chunked empty body, then the close (probe cell
/// `gate-no-host`).
#[tokio::test]
async fn gate_missing_host_is_headless_400() {
    let request = b"GET /api/web/projects HTTP/1.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(
        replay_config("unused"),
        native_config(),
        request,
        "gate-no-host",
    )
    .await;
    assert_eq!(
        normalize(&raw, false, None),
        normalize(
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\ndate: <masked>\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            false,
            None
        ),
        "#1765 the missing-Host gate answers the headless 400 before the route group"
    );
    assert!(
        closed,
        "#1765 the missing-Host gate closes the connection like Node's parser"
    );
}

/// #1765: a non-ASCII byte in the request target is the parser's bare 400
/// even on the api-web path (probe cell `gate-non-ascii-target`).
#[tokio::test]
async fn gate_non_ascii_target_is_parser_bare_400() {
    let request =
        b"GET /api/web/proj\xe9cts HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(
        replay_config("unused"),
        native_config(),
        request,
        "gate-non-ascii-target",
    )
    .await;
    assert_eq!(
        normalize(&raw, true, None),
        normalize(
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
            true,
            None
        ),
        "#1765 the non-ASCII target gate answers the parser bare 400 before the route group"
    );
    assert!(
        closed,
        "#1765 the non-ASCII target gate closes the connection like Node's parser"
    );
}

/// #2153 fix-r1 (review r1): the `content-length: 0` exemption is the two
/// parser bare-400 gates' only — the replay normalizes every oracle cell
/// with the drop off, so a cell recorded as the parser's bare 400 (whose
/// hyper bytes carry the line the TS golden does not) would replay red at
/// its own comparison. This guard fails first, with the cell's id, so the
/// exemption set stays a decision made here instead of a mask the replay
/// quietly re-widens.
#[test]
fn no_oracle_cell_needs_the_content_length_exemption() {
    let recorded = oracle();
    let bare: Vec<&str> = recorded["cells"]
        .as_array()
        .expect("#1348 oracle cells array")
        .iter()
        .filter(|cell| {
            cell["response"]
                .as_str()
                .expect("#1348 oracle cell response")
                .starts_with("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
        })
        .map(|cell| cell["id"].as_str().expect("#1348 oracle cell id"))
        .collect();
    assert_eq!(
        bare,
        Vec::<&str>::new(),
        "#2153 no oracle cell is a parser bare 400 — the content-length exemption belongs to the gate tests only, re-register it here before recording one"
    );
}

/// #2153: the exemption the two gate tests pass is the only masking left —
/// an injected `content-length: 0` on any replayed response must survive
/// normalization (an extra hyper length line replays red), while the
/// registered bare 400's line stays masked.
#[test]
fn only_the_registered_gates_mask_a_content_length_zero() {
    let head = "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\nConnection: keep-alive";
    let plain = format!("{head}\r\n\r\n");
    let injected = format!("{head}\r\ncontent-length: 0\r\n\r\n");
    assert!(
        normalize(injected.as_bytes(), false, None) != normalize(plain.as_bytes(), false, None),
        "#2153 an extra content-length: 0 on a replayed response must not normalize away"
    );
    // The registered direction stays masked: hyper's line on the bare 400 is
    // the one exemption left, so the TS-shaped bytes still compare equal.
    let bare = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n";
    let hyper_shaped = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\ncontent-length: 0\r\n\r\n";
    assert_eq!(
        normalize(hyper_shaped.as_bytes(), true, None),
        normalize(bare.as_bytes(), true, None),
        "#2153 the registered gate exemption still masks hyper's content-length: 0"
    );
}

/// #2153: the raw bytes behind the gates' exemption and the replay's
/// verbatim length lines: the two parser bare 400s carry hyper's
/// `content-length: 0` (the registered direction — a rejection that stopped
/// carrying the line fails here instead of replaying green), and the two
/// HEAD 404 cells carry no content-length at all (the unregistered direction
/// an extra line must not normalize away on). The gate requests are the
/// gates' own bytes; the HEAD cells go through the same recorded-request
/// exchange the replay uses.
#[tokio::test]
async fn content_length_zero_stays_scoped_to_the_registered_gates() {
    for (request, id) in [
        (
            b"FOO /api/web/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n".as_slice(),
            "gate-bad-method",
        ),
        (
            b"GET /api/web/proj\xe9cts HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n".as_slice(),
            "gate-non-ascii-target",
        ),
    ] {
        let (raw, closed) = exchange(
            replay_config("unused"),
            native_config(),
            request,
            id,
        )
        .await;
        let lower = latin1_string(&raw).to_ascii_lowercase();
        assert!(
            lower.contains("\r\ncontent-length: 0\r\n"),
            "#2153 pin {id}: the registered bare 400 carries content-length: 0: {lower:?}"
        );
        assert!(
            closed,
            "#2153 pin {id}: the registered bare 400 closes the connection"
        );
    }
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    for id in ["projects-head", "files-read-head"] {
        let cell = cell(id, &percent_encode(&root_text));
        let (raw, _) = exchange(
            replay_config(&root_text),
            native_config(),
            &cell.request,
            id,
        )
        .await;
        let lower = latin1_string(&raw).to_ascii_lowercase();
        assert!(
            !lower.contains("\r\ncontent-length:"),
            "#2153 pin {id}: the HEAD 404 carries no content-length at all: {lower:?}"
        );
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #1765 reference cell: a malformed percent escape is ASCII, so it is *not*
/// a parse-time 400 — WHATWG path parsing keeps the invalid sequence and the
/// group answers its own 404 JSON naming the raw path (probe cell
/// `gate-pct-target`), the same family as the `api-web-other` cell.
#[tokio::test]
async fn gate_malformed_percent_target_reaches_the_router() {
    let request =
        b"GET /api/web/pro%jects HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n";
    let (raw, closed) = exchange(
        replay_config("unused"),
        native_config(),
        request,
        "gate-pct-target",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "#1765 the malformed-percent target reaches the router, not the parser: {response:?}"
    );
    assert!(
        response.contains("No /api/web route for /api/web/pro%jects"),
        "#1765 the group's 404 names the raw path: {response:?}"
    );
    assert!(
        !closed,
        "#1765 the router 404 keeps the connection open like Node"
    );
}

/// #2354: mounting the api-web group must not hijack the router's own
/// `/health` cell — the group declines paths outside its prefix, and the
/// plain router answers exactly as before the wiring.
#[tokio::test]
async fn health_is_not_hijacked_by_the_api_web_group() {
    let request = b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
    let (raw, closed) = exchange(
        replay_config("unused"),
        native_config(),
        request,
        "health-guard",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 200") && response.contains("{\"ok\":true}"),
        "#2354 /health answers the router's own cell with the group mounted: {response:?}"
    );
    assert!(
        !closed,
        "#2354 /health keeps the connection open like before the wiring"
    );
}

// ------------------------------------------------------ #1764 write refusals

/// #1764: a write whose target names the project root (`path: "."`) answers
/// Node's own EISDIR text — measured on node v22.22.0
/// (`_tmp/20261026-issue-2354/ts-probe/probe-output.txt`, cell
/// `write-root-dot`): `EISDIR: illegal operation on a directory, open
/// '<root>'`. The pre-#1764 replica answered a bare InvalidInput here.
#[tokio::test]
async fn write_to_the_project_root_answers_node_eisdir() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let body = r#"{"path":".","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-root-dot",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1764 the root write answers the 500: {response:?}"
    );
    assert!(
        response.contains(&format!(
            "{{\"error\":{{\"code\":\"internal_error\",\"message\":\"EISDIR: illegal operation on a directory, open '{root_text}'\"}}}}"
        )),
        "#1764 the root write answers Node's EISDIR text with the target path: {response:?}"
    );
    assert!(
        !closed,
        "#1764 the EISDIR 500 keeps the connection open like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1764: a write whose target names an existing directory (probe cell
/// `write-existing-dir`) is the same EISDIR family, with the directory's own
/// path in the text.
#[tokio::test]
async fn write_to_an_existing_directory_answers_node_eisdir() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let body = r#"{"path":"sub","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-existing-dir",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1764 the directory write answers the 500: {response:?}"
    );
    assert!(
        response.contains(&format!(
            "{{\"error\":{{\"code\":\"internal_error\",\"message\":\"EISDIR: illegal operation on a directory, open '{root_text}/sub'\"}}}}"
        )),
        "#1764 the directory write answers Node's EISDIR text with the target path: {response:?}"
    );
    assert!(
        !closed,
        "#1764 the EISDIR 500 keeps the connection open like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1764: a path with a NUL byte is refused by Node's fs argument validation
/// before any syscall — the received value rendered by `util.inspect`, NUL
/// as the literal `\x00` (probe cell `write-nul-path`; the quoting table is
/// measured in `node-nul-render*.txt`). The pre-#1764 replica answered an
/// EINVAL here.
#[tokio::test]
async fn write_to_a_nul_path_answers_node_argument_validation() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let body = r#"{"path":"a\u0000b.txt","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-nul-path",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1764 the NUL-path write answers the 500: {response:?}"
    );
    assert!(
        response.contains(&format!(
            "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '{root_text}/a\\\\x00b.txt'"
        )),
        "#1764 the NUL-path write answers Node's argument-validation text: {response:?}"
    );
    assert!(
        !closed,
        "#1764 the validation 500 keeps the connection open like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1764: a NUL below an existing directory (probe cell `write-nul-nested`)
/// is the same validation refusal with the joined target path — TS's
/// `mkdir(dirname)` runs first and only the write's own argument carries the
/// NUL.
#[tokio::test]
async fn write_to_a_nested_nul_path_answers_node_argument_validation() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let body = r#"{"path":"sub/a\u0000b.txt","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-nul-nested",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#1764 the nested NUL-path write answers the 500: {response:?}"
    );
    assert!(
        response.contains(&format!(
            "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '{root_text}/sub/a\\\\x00b.txt'"
        )),
        "#1764 the nested NUL-path write names the joined target: {response:?}"
    );
    assert!(
        !closed,
        "#1764 the validation 500 keeps the connection open like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2354 fix-r1: a NUL path carrying both quote kinds renders the shared
/// `node_io` renderer's backtick tier — node v22.22.0 measured,
/// `_tmp/20261026-issue-2354/fix-r1/ts-probe/node-write-refusal.txt`:
/// ``Received `/…/a'b"c\x00d.txt` `` — the local single-quote subset this
/// file used to carry answered `'…\'…'` instead.
#[test]
fn write_nul_path_with_both_quotes_renders_the_backtick_tier() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2354 the fixture root is UTF-8"));
    let result = service.write_file(
        "a'b\"c\u{0}d.txt",
        &pilotdeck_protocol::JsString::from("x"),
        false,
    );
    match result {
        Err(files::FilesError::MessageUnits(message)) => assert_eq!(
            message.units(),
            format!(
                "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received `{root_text}/a'b\"c\\x00d.txt`"
            )
            .encode_utf16()
            .collect::<Vec<_>>()
            .as_slice(),
            "#2354 both quote kinds render the backtick tier like node v22.22.0"
        ),
        Err(_) => panic!("#2354 a NUL path answers the argument-validation message"),
        Ok(()) => panic!("#2354 a NUL path must be refused"),
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #2354 fix-r1: a NUL path past the error constructor's 128 UTF-16 unit cut
/// truncates the received value at 128 units and appends `...` — node v22.22.0
/// measured (`fix-r1/ts-probe/node-write-refusal.txt`: the received part of a
/// 200-unit path is exactly 131 units), where the local subset used to render
/// the whole path.
#[test]
fn write_nul_path_beyond_128_units_truncates_like_node() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2354 the fixture root is UTF-8"));
    let result = service.write_file(
        &format!("{}\u{0}.txt", "N".repeat(126)),
        &pilotdeck_protocol::JsString::from("x"),
        false,
    );
    // The measured rule, re-derived for the expectation: the inspected value
    // is quote + root + `/` + the escaped leaf (126 N's, the four-unit `\x00`
    // escape, `.txt`), cut at 128 UTF-16 units, plus `...`.
    let mut inspected: Vec<u16> = format!("'{root_text}/{}\\x00.txt'", "N".repeat(126))
        .encode_utf16()
        .collect();
    inspected.truncate(128);
    match result {
        Err(files::FilesError::MessageUnits(message)) => assert_eq!(
            message.units(),
            {
                let mut expected: Vec<u16> = "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received "
                    .encode_utf16()
                    .collect();
                expected.extend_from_slice(&inspected);
                expected.extend("...".encode_utf16());
                expected
            }
            .as_slice(),
            "#2354 the received value is cut at 128 UTF-16 units like node v22.22.0"
        ),
        Err(_) => panic!("#2354 a NUL path answers the argument-validation message"),
        Ok(()) => panic!("#2354 a NUL path must be refused"),
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #2455: a NUL path whose non-BMP character straddles the 128-unit cut.
/// Node's slice cuts between the halves of the surrogate pair, so the message
/// keeps a lone high surrogate, and `JSON.stringify` renders it as the
/// well-formed `\ud83d` escape — node v22.22.0 measured,
/// `_tmp/20261108-p2batch-c1/s2-ts-probe/probe-output.txt` (response hex
/// tail `…5c75643833642e2e2e` = `\ud83d...`; the pre-fix replica folded the
/// lone surrogate to U+FFFD, `…efbfbd2e2e2e`).
#[test]
fn write_nul_path_surrogate_pair_straddling_the_cut_keeps_the_lone_surrogate() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2455 the fixture root is UTF-8"));
    // The inspected value is quote + root + `/` + an N-run + the pair; the
    // run length puts the pair's high surrogate at unit 127, so the 128-unit
    // cut keeps it and drops the low half.
    let run = 125usize
        .checked_sub(root_text.encode_utf16().count())
        .expect("#2455 the fixture root is short enough to place the cut");
    let relative = format!("{}😀\u{0}.txt", "N".repeat(run));
    let result = service.write_file(&relative, &pilotdeck_protocol::JsString::from("x"), false);
    let message = match result {
        Err(files::FilesError::MessageUnits(message)) => message,
        Err(_) => panic!("#2455 a NUL path answers the argument-validation message"),
        Ok(()) => panic!("#2455 a NUL path must be refused"),
    };
    let expected: Vec<u16> = format!(
        "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '{root_text}/{}",
        "N".repeat(run)
    )
    .encode_utf16()
    .chain([0xd83d])
    .chain("...".encode_utf16())
    .collect();
    assert_eq!(
        message.units(),
        expected.as_slice(),
        "#2455 the cut keeps the lone high surrogate and drops the low half, like node v22.22.0"
    );
    // The rendered body keeps JSON.stringify's well-formed escape for the
    // lone surrogate instead of a U+FFFD byte.
    let body = super::error_body_units("internal_error", &message);
    assert!(
        body.contains(r"\ud83d"),
        "#2455 the wire keeps the well-formed lone-surrogate escape: {body:?}"
    );
    assert!(
        !body
            .as_bytes()
            .windows(3)
            .any(|window| window == [0xef, 0xbf, 0xbd]),
        "#2455 the wire never folds the lone surrogate to U+FFFD: {body:?}"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2456: a project key pointing at a nonexistent absolute path plus
/// `path: "."`. TS's `mkdir(dirname(target), {recursive: true})` creates
/// only the root's *ancestors*, and the write's own open then creates the
/// root as a regular file and answers 200 — node v22.22.0 measured,
/// `_tmp/20261108-p2batch-c1/s3-ts-probe/probe-output.txt` (status 200,
/// `{"ok":true}`, side effect REGULAR FILE size 1, parents directories).
/// The pre-fix armed walk `mkdir`-ed the root itself and answered the
/// EISDIR 500 with the root created as a directory instead.
#[test]
fn write_dot_on_a_missing_root_creates_the_root_file_like_node() {
    let (parent, _root) = fixture();
    let fresh = parent.join("does").join("not").join("exist");
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#2456 the fresh root is UTF-8"));
    service
        .write_file(".", &pilotdeck_protocol::JsString::from("x"), false)
        .expect("#2456 a `path: \".\"` write on a missing root creates the root file like node");
    let stats = fs::metadata(&fresh).expect("#2456 the root exists after the write");
    assert!(
        stats.is_file(),
        "#2456 the missing root becomes the write's own regular file, not a directory"
    );
    assert_eq!(
        stats.len(),
        1,
        "#2456 the write's bytes are the root file's"
    );
    assert!(
        fs::metadata(parent.join("does"))
            .expect("#2456 the ancestors exist")
            .is_dir(),
        "#2456 the mkdir(dirname) half created the ancestors as directories"
    );
    assert!(
        fs::metadata(&fresh)
            .expect("#2456 re-stat the root")
            .is_file(),
        "#2456 the finished root is still a file, not a directory"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2456, the HTTP surface: the same missing-root `path: "."` write answers
/// 200 with the fixed `{"ok":true}` body and keeps the connection open, and
/// the bytes land in the root file itself.
#[tokio::test]
async fn write_dot_on_a_missing_root_answers_200_like_node() {
    let (parent, _root) = fixture();
    let fresh = parent.join("missing-root");
    let root_text = fresh.to_string_lossy().into_owned();
    let body = r#"{"path":".","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-missing-root-dot",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "#2456 the missing-root dot write answers the 200: {response:?}"
    );
    assert!(
        response.contains("{\"ok\":true}"),
        "#2456 the 200 carries the fixed body: {response:?}"
    );
    assert!(
        !closed,
        "#2456 the write 200 keeps the connection open like Node"
    );
    let stats = fs::metadata(&fresh).expect("#2456 the root exists after the write");
    assert!(
        stats.is_file(),
        "#2456 the side effect is a regular file at the key: {stats:?}"
    );
    assert_eq!(
        fs::read(&fresh).expect("#2456 read the created root file"),
        b"x",
        "#2456 the write's bytes are the root file's"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2455 adjacent cell, the registered residual of
/// `specs/gateway-parity/diffs/A8.4-API-WEB-002-R3.md`: a path whose lone
/// surrogate was already folded to U+FFFD at the handler's JS-to-fs boundary
/// (the JSON spelling `"a\ud83d\u0000b.txt"`). Node's validation inspects the
/// raw JS string and renders the lone surrogate as the ASCII `\ud83d` escape;
/// this replica's message keeps the boundary's U+FFFD byte. The
/// pair-straddling cut cell itself is fixed above (`MessageUnits`), this
/// input-lone-surrogate cell stays at the registered bytes.
#[test]
fn write_nul_path_with_a_boundary_fffd_answers_the_registered_residual() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2455 the fixture root is UTF-8"));
    let result = service.write_file(
        "a\u{FFFD}\u{0}b.txt",
        &pilotdeck_protocol::JsString::from("x"),
        false,
    );
    let message = match result {
        Err(files::FilesError::MessageUnits(message)) => message,
        Err(_) => panic!("#2455 a NUL path answers the argument-validation message"),
        Ok(()) => panic!("#2455 a NUL path must be refused"),
    };
    let text = String::from_utf16_lossy(message.units());
    assert!(
        text.contains(&format!("{root_text}/a\u{FFFD}\\x00b.txt")),
        "#2455 the inspected value keeps the boundary's U+FFFD, per the R3 residual: {text:?}"
    );
    assert!(
        !text.contains(r"\ud83d"),
        "#2455 the registered residual keeps the FFFD form, not Node's ASCII escape: {text:?}"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2450 fix-r2 (boundary#1), the residual-one nail on the actual wire: the
/// JSON spelling `"a\ud83d\u0000b.txt"` rides the whole registered line —
/// the body parser keeps the lone surrogate as a UTF-16 unit, the handler's
/// JS-to-fs boundary (`from_utf16_lossy`) folds it to U+FFFD, the refusal
/// renders through the units body encoder — so the response proves the
/// residual's own bytes instead of feeding the post-fold spelling to the
/// service directly. R3 registers this replica answering the FFFD byte where
/// Node's validation inspects the raw JS string; the direct-call test above
/// stays green if line 299 ever changes, this one does not.
#[tokio::test]
async fn write_nul_path_with_a_boundary_fffd_over_http_answers_the_registered_residual() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    let body = r#"{"path":"a\ud83d\u0000b.txt","content":"x"}"#;
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, closed) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "write-nul-fffd-residual",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 500"),
        "#2450 fix-r2 the lone-surrogate NUL write answers the 500 on the wire: {response:?}"
    );
    // The residual's registered bytes: the boundary's U+FFFD as raw UTF-8
    // (`efbfbd`) inside the inspected value, not Node's ASCII `\ud83d`
    // escape — asserted on the wire bytes, both spellings of the fold. The
    // NUL's `\x00` render arrives JSON-quoted, so the wire carries two
    // backslashes before the x00.
    // The whole window must survive `invalid_arg_value_message`'s 128-unit
    // inspect truncation: inspected (with quotes) is len(root) + 14 UTF-16
    // units, so the fixture root must stay at or below 114 units — the
    // direct-call nail at the truncation threshold shares this bound, and a
    // longer TMPDIR would cut the `\x00b.txt'` tail off the message.
    let expected: Vec<u8> = format!("Received '{root_text}/a")
        .into_bytes()
        .into_iter()
        .chain([0xef, 0xbf, 0xbd])
        .chain(b"\\\\x00b.txt'".iter().copied())
        .collect();
    assert!(
        raw.windows(expected.len()).any(|window| window == expected),
        "#2450 fix-r2 the wire keeps the boundary's U+FFFD byte where R3 registers the residual: {response:?}"
    );
    assert!(
        !raw.windows(6).any(|window| window == b"\\ud83d"),
        "#2450 fix-r2 the residual's wire never spells the lone surrogate as the ASCII escape: {response:?}"
    );
    assert!(
        !closed,
        "#2450 fix-r2 the residual 500 keeps the connection open like Node"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2456 adjacent cell, the registered residual of
/// `specs/gateway-parity/diffs/A8.4-API-WEB-002-R3.md`: with the root an
/// existing regular file (the shape a missing-root `path: "."` write now
/// leaves), TS's `writeFile(target)` overwrites the file and answers 200; the
/// armed walk's root open answers its registered `ENOTDIR … open '<root>'`
/// diagnostic instead, per 002's root-open registration as corrected by R3.
#[cfg(unix)]
#[test]
fn write_dot_on_a_file_root_answers_the_registered_enotdir_residual() {
    let (parent, _root) = fixture();
    let fresh = parent.join("file-root");
    fs::write(&fresh, b"old").expect("#2456 create the regular-file root");
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#2456 the file root is UTF-8"));
    let result = service.write_file(".", &pilotdeck_protocol::JsString::from("y"), false);
    match result {
        Err(files::FilesError::Message(message)) => assert_eq!(
            message,
            format!(
                "ENOTDIR: not a directory, open '{}'",
                fresh.to_string_lossy()
            ),
            "#2456 the file-root dot write keeps the registered root-open diagnostic verbatim"
        ),
        Err(_) => panic!("#2456 a file root answers the root-open diagnostic"),
        Ok(()) => panic!("#2456 the file-root residual answers 500, not the TS overwrite"),
    }
    assert_eq!(
        fs::read(&fresh).expect("#2456 read the file root"),
        b"old",
        "#2456 the refused write leaves the file root's bytes alone"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #2450 fix-r2 (concurrency#1): the missing-root `path: "."` create pins
/// its deepest-existing ancestor *after* the missing-root open failed, so a
/// root built as a directory inside that window (a racing `mkdir`) leaves the
/// create's tail empty. The write must then answer the same EISDIR
/// `write_root_itself` gives an existing directory root — Node's own
/// `writeFile` open refusal — instead of feeding the empty relative path to
/// the leaf open and rendering `UNKNOWN: unknown error`, which Node never
/// produces.
#[cfg(unix)]
#[test]
fn write_dot_when_the_root_becomes_a_directory_inside_the_window_answers_eisdir() {
    let (parent, _root) = fixture();
    let fresh = parent.join("late-root");
    let racing_root = fresh.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "create-root" {
            fs::create_dir_all(&racing_root)
                .expect("#2450 fix-r2 create the root directory inside the window");
        }
    });
    let service = files::ProjectFileService::new(
        fresh
            .to_str()
            .expect("#2450 fix-r2 the racing root is UTF-8"),
    );
    let result = service.write_file(".", &pilotdeck_protocol::JsString::from("y"), false);
    match result {
        Err(files::FilesError::Message(message)) => assert_eq!(
            message,
            format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                fresh.to_string_lossy()
            ),
            "#2450 fix-r2 a root that became a directory inside the window answers Node's EISDIR, not the empty-path UNKNOWN: {message:?}"
        ),
        Err(_) => panic!("#2450 fix-r2 a racing directory root answers Node's EISDIR text"),
        Ok(()) => panic!("#2450 fix-r2 a racing directory root must be refused"),
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #2568: a root whose existence keeps flipping between the missing-root
/// open and the ancestor pin. The pre-fix `write_root_itself` ↔
/// `create_root_file` pair expressed "decide again" as an unbounded
/// back-call — the same flip repeated crashed the process on a stack
/// overflow (measured: `_tmp/20261109-p2batch-h/s4-overflow-probe.out`).
/// The bounded re-decide loop answers the fail-closed boundary refusal
/// instead, and the write never reports success over a root it could not
/// settle a verdict on.
#[cfg(unix)]
#[test]
fn write_dot_on_a_root_that_keeps_flipping_answers_the_bounded_refusal() {
    let (parent, _root) = fixture();
    let fresh = parent.join("flip-root");
    let flipping = fresh.clone();
    let _guard = files::test_hook::set(move |phase| match phase {
        // Between one decision and the next open the root vanishes again …
        "root-redecide" => {
            let _ = fs::remove_dir(&flipping);
        }
        // … and inside the pin window it is a directory again, so the tail
        // is empty and the write has to decide again.
        "create-root" => {
            fs::create_dir_all(&flipping).expect("#2568 create the root inside the window");
        }
        _ => {}
    });
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#2568 the flipping root is UTF-8"));
    let result = service.write_file(".", &pilotdeck_protocol::JsString::from("y"), false);
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, ".",
            "#2568 the exhausted re-decide answers the boundary refusal on the requested path"
        ),
        Err(_) => panic!("#2568 a root that keeps flipping answers the boundary refusal"),
        Ok(()) => panic!("#2568 a root that keeps flipping must never be written"),
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #2354 fix-r1: Node's EISDIR names the literal target — node v22.22.0
/// measured (`fix-r1/ts-probe/node-write-refusal.txt`): a write whose path
/// goes through an in-root directory symlink answers `open '<root>/link'`,
/// not the canonical `<root>/sub` the armed walk resolves the link to.
#[cfg(unix)]
#[test]
fn write_to_an_in_root_directory_link_reports_the_literal_path() {
    let (parent, root) = fixture();
    let root_text = root.to_string_lossy().into_owned();
    std::os::unix::fs::symlink(root.join("sub"), root.join("link"))
        .expect("#2354 create the in-root directory link");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2354 the fixture root is UTF-8"));
    let result = service.write_file("link", &pilotdeck_protocol::JsString::from("x"), false);
    match result {
        Err(files::FilesError::Message(message)) => assert_eq!(
            message,
            format!("EISDIR: illegal operation on a directory, open '{root_text}/link'"),
            "#2354 the EISDIR refusal names the literal target like node v22.22.0"
        ),
        Err(_) => panic!("#2354 a directory link target answers Node's EISDIR text"),
        Ok(()) => panic!("#2354 a directory link target must be refused"),
    }
    let _ = fs::remove_dir_all(&parent);
}

// ------------------------------------------------- file service boundary pins

/// #1691: the tree's `modifiedAt` is the transport's shared
/// `format_epoch_iso`, Node v22.22.0's `Date#toISOString` for the whole
/// epoch domain — every literal below is the measured Node output
/// (`_tmp/20261026-issue-2354/ts-probe/node-iso-clip.txt`), including both
/// TimeClip bounds and the extended-year spellings the local formatter this
/// file used to carry was deleted for. The extreme years cannot round-trip
/// through a real file's mtime (the filesystem clamps them), so the pin is
/// the direct call.
#[test]
fn modified_at_epoch_domain_is_the_transport_formatter() {
    for (epoch_ms, expected) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (-8_640_000_000_000_000, "-271821-04-20T00:00:00.000Z"),
        (8_640_000_000_000_000, "+275760-09-13T00:00:00.000Z"),
        (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        (-62_198_755_200_000, "-000001-01-01T00:00:00.000Z"),
        (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
    ] {
        assert_eq!(
            pilotdeck_transport::system_clock::format_epoch_iso(epoch_ms),
            expected,
            "#1691 epoch {epoch_ms} renders Node's toISOString literal"
        );
    }
}

/// `JSON.stringify` of a service value, for the pins below.
fn json_of(value: &JsValue) -> String {
    stringify_json(value)
        .expect("#1348 stringify a file service value")
        .expect("#1348 a file service value is never undefined")
}

/// #1348 boundary audit P2: the component header claims `resolveSafe` rejects
/// symlinks that point outside the project root, but neither the fixture
/// (its only link, `inside-link`, points inside) nor any negative control
/// exercises that branch — the `api-web-boundary-off` switch turns all three
/// gates off at once, so dropping just the realpath gate stays green. Pin it
/// directly.
#[cfg(unix)]
#[test]
fn read_rejects_a_symlink_that_leaves_the_root() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1348 create the directory outside the root");
    fs::write(outside.join("secret.txt"), b"outside the root\n")
        .expect("#1348 write the file outside the root");
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt"))
        .expect("#1348 link the outside file into the root");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1348 the fixture root is UTF-8"));
    match service.read_file("escape.txt") {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "escape.txt",
            "#1348 boundary pin: the escaping path is reported verbatim"
        ),
        Err(_) => panic!(
            "#1348 boundary pin: a symlink leaving the root answers workspace_boundary, not a read error"
        ),
        Ok(_) => panic!("#1348 boundary pin: a symlink leaving the root must not be readable"),
    }
}

/// #1348 boundary audit P2: `readFile` serves content while
/// `stats.size > maxReadBytes` (TS `projectFiles.ts:162` uses `>` as well), so
/// a file of exactly 1 MiB still returns its content and one byte more does
/// not. The fixture only has `big.bin` = 1 MiB + 1, which cannot tell `>` from
/// `>=`; pin the boundary.
#[test]
fn read_limit_is_exclusive_at_one_mib() {
    let (_parent, root) = fixture();
    fs::write(root.join("exact.txt"), vec![b'a'; 1024 * 1024])
        .expect("#1348 write the file at the read limit");
    fs::write(root.join("over.txt"), vec![b'b'; 1024 * 1024 + 1])
        .expect("#1348 write the file one byte over the read limit");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1348 the fixture root is UTF-8"));

    let exact = json_of(
        &service
            .read_file("exact.txt")
            .unwrap_or_else(|_| panic!("#1348 read the file at the limit")),
    );
    assert!(
        exact.contains("\"size\":1048576,"),
        "#1348 boundary pin: the exact-limit file reports its size: {exact}"
    );
    assert!(
        exact.contains("\"content\":\"aaaa"),
        "#1348 boundary pin: a file of exactly maxReadBytes still returns its content (the check is `>`): {exact}"
    );

    let over = json_of(
        &service
            .read_file("over.txt")
            .unwrap_or_else(|_| panic!("#1348 read the file over the limit")),
    );
    assert!(
        over.contains("\"size\":1048577,"),
        "#1348 boundary pin: the over-limit file reports its size: {over}"
    );
    assert!(
        !over.contains("\"content\""),
        "#1348 boundary pin: one byte over maxReadBytes answers metadata only: {over}"
    );
    assert!(
        over.contains("\"binary\":false"),
        "#1348 boundary pin: the over-limit text file keeps its text encoding: {over}"
    );
}

// --------------------------------------------- #1675 boundary escape probes

/// #1675 probe (symlink): a directory symlink planted inside the root must
/// not carry a write outside the workspace. The lexical write resolve never
/// looks at the link, so today this escapes deterministically.
#[cfg(unix)]
#[test]
fn write_rejects_a_symlink_that_leaves_the_root() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    std::os::unix::fs::symlink(&outside, root.join("out-link"))
        .expect("#1675 link the outside directory into the root");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(
        "out-link/escaped.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("escaped.txt"));
    assert!(
        leaked.is_err(),
        "#1675 mutated: a symlink carried the write out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "out-link/escaped.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => {
            panic!("#1675 a symlink leaving the root answers workspace_boundary, not a write error")
        }
        Ok(()) => panic!("#1675 a write through a symlink leaving the root must be rejected"),
    }
}

/// #1675 probe (`..`): a dot-dot path is rejected by the lexical gate before
/// any syscall, so the parent file must keep its baseline bytes.
#[test]
fn write_rejects_dotdot_that_leaves_the_root() {
    let (parent, root) = fixture();
    let outside = parent.join("dotdot.txt");
    fs::write(&outside, b"before\n").expect("#1675 write the file above the root");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(
        "../dotdot.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    assert_eq!(
        fs::read(&outside).expect("#1675 read the file above the root"),
        b"before\n",
        "#1675 a `..` write must not reach the file above the root"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "../dotdot.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a `..` path answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 a `..` write must be rejected"),
    }
}

/// #1675 probe (absolute): an absolute path is rejected by the lexical gate,
/// so the target file is never created.
#[test]
fn write_rejects_an_absolute_path() {
    let (_parent, root) = fixture();
    let outside = std::env::temp_dir().join(format!("pdrs-1675-{}", uuid::Uuid::new_v4()));
    let raw = outside
        .to_str()
        .expect("#1675 the temp path is UTF-8")
        .to_owned();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(&raw, &pilotdeck_protocol::JsString::from("pwned\n"), false);
    assert!(
        !outside.exists(),
        "#1675 an absolute write must not create a file outside the root"
    );
    match result {
        Err(files::FilesError::Workspace { raw: reported }) => assert_eq!(
            reported, raw,
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 an absolute path answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 an absolute write must be rejected"),
    }
}

/// #1675 probe (write race window): the component is swapped for a symlink
/// after the lexical check and before the write, inside the hook the file
/// service exposes for exactly this assertion.
#[cfg(unix)]
#[test]
fn write_rejects_a_component_swapped_inside_the_window() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(root.join("swap")).expect("#1675 create the in-root directory");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    let link_root = root.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "write" {
            fs::remove_dir(link_root.join("swap")).expect("#1675 remove the in-root directory");
            std::os::unix::fs::symlink(&link_target, link_root.join("swap"))
                .expect("#1675 install the racing symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(
        "swap/raced.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("raced.txt"));
    assert!(
        leaked.is_err(),
        "#1675 mutated: the race window carried the write out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "swap/raced.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a raced component answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 a write through a raced component must be rejected"),
    }
}

/// #1675 probe (root race window): the project root itself is renamed away
/// and a link to the outside directory takes its place after the realpath
/// gate and before the write pins the root descriptor.
#[cfg(unix)]
#[test]
fn write_rejects_a_root_swapped_for_a_link() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    let link_root = root.clone();
    let parked_root = parent.join("parked");
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "write" {
            fs::rename(&link_root, &parked_root).expect("#1675 park the checked root");
            std::os::unix::fs::symlink(&link_target, &link_root)
                .expect("#1675 install the racing root symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(
        "raced.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("raced.txt"));
    assert!(
        leaked.is_err(),
        "#1675 mutated: the swapped root carried the write out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "raced.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a swapped root answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 a write through a swapped root must be rejected"),
    }
}

/// #1675 probe (write race window, leaf): the checked file is swapped for a
/// symlink to the outside directory after the lexical/realpath check and
/// before the write, so the failing gate is the leaf `O_NOFOLLOW` open in
/// `overwrite_at` rather than the component walk above it.
#[cfg(unix)]
#[test]
fn write_rejects_a_leaf_swapped_inside_the_window() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    fs::write(root.join("race.txt"), b"inside\n").expect("#1675 write the in-root file");
    let link_root = root.clone();
    let link_target = outside.join("escaped.txt");
    let _guard = files::test_hook::set(move |phase| {
        if phase == "write" {
            fs::remove_file(link_root.join("race.txt")).expect("#1675 remove the in-root file");
            std::os::unix::fs::symlink(&link_target, link_root.join("race.txt"))
                .expect("#1675 install the racing leaf symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.write_file(
        "race.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("escaped.txt"));
    assert!(
        leaked.is_err(),
        "#1675 mutated: the race window carried the leaf write out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "race.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a raced leaf answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 a write through a raced leaf must be rejected"),
    }
}

/// #2354 fix-r1 probe (write race window, middle component → an outside
/// DIRECTORY leaf): the in-root directory on the path is swapped for a symlink
/// to the outside directory after the realpath gate and before the write, and
/// the leaf it names there is a directory. The EISDIR precheck this file used
/// to carry would have followed the raced link with a path stat, classified
/// the outside directory leaf, and answered the EISDIR 500 — an oracle on the
/// outside leaf's shape — instead of the boundary refusal the window owes
/// (#1675/A8.4-API-WEB-002). The refusal now comes from the write's own
/// NOFOLLOW walk, so the window answers 403 for every swapped shape.
#[cfg(unix)]
#[test]
fn write_rejects_a_component_swapped_to_an_outside_directory_leaf() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(root.join("swap")).expect("#2354 create the in-root directory");
    fs::create_dir_all(&outside).expect("#2354 create the directory outside the root");
    // The leaf beyond the swapped link is a directory: the follow-stat the
    // precheck used to run would classify it (EISDIR) — the walk must not.
    fs::create_dir_all(outside.join("raced.txt")).expect("#2354 create the outside directory leaf");
    let link_root = root.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "write" {
            fs::remove_dir(link_root.join("swap")).expect("#2354 remove the in-root directory");
            std::os::unix::fs::symlink(&link_target, link_root.join("swap"))
                .expect("#2354 install the racing directory symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#2354 the fixture root is UTF-8"));
    let result = service.write_file(
        "swap/raced.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("raced.txt"));
    assert!(
        leaked.is_err(),
        "#2354 mutated: the race window carried the write out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "swap/raced.txt",
            "#2354 the escaping path is reported verbatim"
        ),
        Err(_) => panic!(
            "#2354 a raced component to an outside directory answers workspace_boundary, not a write error"
        ),
        Ok(()) => panic!("#2354 a write through a raced component must be rejected"),
    }
    let _ = fs::remove_dir_all(&parent);
}

/// #1675 probe (missing root, ancestor race): the root does not exist yet, and
/// an existing ancestor of it is swapped for a link to the outside directory
/// after the lexical/realpath check and before the write. Recursive path mkdir
/// follows that link and builds the root outside the workspace; the pinned
/// root open refuses it. The swap rides the `write` phase, so the control that
/// turns the descriptor walk off exercises the same swap.
#[cfg(unix)]
#[test]
fn write_rejects_a_missing_root_below_a_swapped_ancestor() {
    let (parent, _root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    let mid = parent.join("mid");
    fs::create_dir_all(&mid).expect("#1675 create the in-root ancestor");
    let fresh = mid.join("proj");
    let link_mid = mid.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "write" {
            fs::remove_dir(&link_mid).expect("#1675 remove the in-root ancestor");
            std::os::unix::fs::symlink(&link_target, &link_mid)
                .expect("#1675 install the racing ancestor symlink");
        }
    });
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#1675 the fresh root is UTF-8"));
    let result = service.write_file(
        "nested/a.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    let leaked = fs::read_to_string(outside.join("proj/nested/a.txt"));
    assert!(
        leaked.is_err(),
        "#1675 mutated: the swapped ancestor carried the created root out of the workspace: {leaked:?}"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "nested/a.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a swapped ancestor answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 a write must not create the root through a swapped ancestor"),
    }
}

/// #1675 regression probe (missing root, ancestor swapped *inside* the create):
/// the root open has already failed with ENOENT, and an ancestor is swapped
/// before the pinned create walks it. `create_dir_all` + a post-hoc
/// `canonicalize` comparison (the shape this PR replaced) follows the link,
/// builds the root outside, and then compares two equally poisoned values —
/// this probe fails on that implementation and passes on the pinned one. The
/// seam only exists on the descriptor path, so the switch-off build answers
/// `Ok` here by construction: this pin is deliberately **not** bound to
/// `api-web-nofollow-off`.
#[cfg(unix)]
#[test]
fn write_rejects_a_missing_root_below_an_ancestor_swapped_after_the_open() {
    let (parent, _root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    let mid = parent.join("mid");
    fs::create_dir_all(&mid).expect("#1675 create the in-root ancestor");
    let fresh = mid.join("proj");
    let link_mid = mid.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "create-root" {
            fs::remove_dir(&link_mid).expect("#1675 remove the in-root ancestor");
            std::os::unix::fs::symlink(&link_target, &link_mid)
                .expect("#1675 install the racing ancestor symlink");
        }
    });
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#1675 the fresh root is UTF-8"));
    let result = service.write_file(
        "nested/a.txt",
        &pilotdeck_protocol::JsString::from("pwned\n"),
        false,
    );
    assert!(
        fs::read_to_string(outside.join("proj/nested/a.txt")).is_err(),
        "#1675 the pinned create must not build the root through a swapped ancestor"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "nested/a.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a swapped ancestor answers workspace_boundary, not a write error"),
        Ok(()) => panic!("#1675 the pinned create must refuse a swapped ancestor"),
    }
}

/// #2450 fix-r2 (concurrency#2 / boundary#4), the `path: "."` spelling of the
/// swapped-ancestor probe above: the missing-root dot write reaches
/// `create_root_file`'s own pin, whose deepest-existing NOFOLLOW walk must
/// refuse an ancestor swapped for a link to outside the workspace inside the
/// window — the boundary refusal on the `.` spelling, with the root file
/// never landing outside.
#[cfg(unix)]
#[test]
fn write_dot_rejects_a_missing_root_below_an_ancestor_swapped_after_the_open() {
    let (parent, _root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#2450 fix-r2 create the directory outside the root");
    let mid = parent.join("mid");
    fs::create_dir_all(&mid).expect("#2450 fix-r2 create the in-root ancestor");
    let fresh = mid.join("proj");
    let link_mid = mid.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "create-root" {
            fs::remove_dir(&link_mid).expect("#2450 fix-r2 remove the in-root ancestor");
            std::os::unix::fs::symlink(&link_target, &link_mid)
                .expect("#2450 fix-r2 install the racing ancestor symlink");
        }
    });
    let service = files::ProjectFileService::new(
        fresh
            .to_str()
            .expect("#2450 fix-r2 the fresh root is UTF-8"),
    );
    let result = service.write_file(".", &pilotdeck_protocol::JsString::from("pwned\n"), false);
    assert!(
        fs::read_to_string(outside.join("proj")).is_err(),
        "#2450 fix-r2 the pinned create must not write the root file through a swapped ancestor"
    );
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, ".",
            "#2450 fix-r2 the dot write's escaping path is reported verbatim"
        ),
        Err(_) => {
            panic!("#2450 fix-r2 a swapped ancestor answers workspace_boundary, not a write error")
        }
        Ok(()) => panic!("#2450 fix-r2 the pinned create must refuse a swapped ancestor"),
    }
}

/// #1675 regression probe: the project root does not exist yet, and the write
/// creates it like TS's `mkdir(dirname(target), {recursive: true})`.
#[cfg(unix)]
#[test]
fn write_creates_a_missing_project_root() {
    let (parent, _root) = fixture();
    let fresh = parent.join("fresh");
    let service =
        files::ProjectFileService::new(fresh.to_str().expect("#1675 the fresh root is UTF-8"));
    let result = service.write_file(
        "nested/a.txt",
        &pilotdeck_protocol::JsString::from("hello\n"),
        false,
    );
    assert!(
        result.is_ok(),
        "#1675 a write below a missing root creates it"
    );
    assert_eq!(
        fs::read_to_string(fresh.join("nested/a.txt")).expect("#1675 read the written file"),
        "hello\n",
        "#1675 the write landed inside the created root"
    );
}

/// #1675 regression probe (missing root below a symlinked ancestor): the root
/// path reaches the fixture through a link, so the lexical spelling and the
/// canonical one disagree — the shape macOS `/tmp` / `/var` gives every real
/// request. Creating the root must still succeed and land at the canonical
/// location, instead of answering `workspace_boundary` because the containment
/// check compared the canonical target with a lexical root.
#[cfg(unix)]
#[test]
fn write_creates_a_missing_root_below_a_symlinked_ancestor() {
    let (parent, _root) = fixture();
    let real = parent.join("real");
    fs::create_dir_all(&real).expect("#1675 create the real ancestor");
    std::os::unix::fs::symlink(&real, parent.join("alias")).expect("#1675 install the alias link");
    let aliased = parent.join("alias").join("fresh");
    let service =
        files::ProjectFileService::new(aliased.to_str().expect("#1675 the aliased root is UTF-8"));
    let result = service.write_file(
        "nested/a.txt",
        &pilotdeck_protocol::JsString::from("hello\n"),
        false,
    );
    assert!(
        result.is_ok(),
        "#1675 a write below a missing root reached through a link creates it"
    );
    assert_eq!(
        fs::read_to_string(real.join("fresh/nested/a.txt")).expect("#1675 read the written file"),
        "hello\n",
        "#1675 the write landed at the canonical root"
    );
}

/// #1675 probe (tree race window): the listed directory is swapped for a link
/// to the outside directory after the realpath gate and before the pinned
/// enumeration.
#[cfg(unix)]
#[test]
fn read_tree_rejects_a_directory_swapped_inside_the_window() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    fs::write(outside.join("secret.txt"), b"top secret\n")
        .expect("#1675 write the file outside the root");
    fs::create_dir_all(root.join("listing")).expect("#1675 create the listed directory");
    let link_root = root.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "tree" {
            fs::remove_dir(link_root.join("listing")).expect("#1675 remove the listed directory");
            std::os::unix::fs::symlink(&link_target, link_root.join("listing"))
                .expect("#1675 install the racing symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.read_tree("listing");
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "listing",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a raced directory answers workspace_boundary, not a scandir error"),
        Ok(_) => panic!("#1675 mutated: the race window listed outside the workspace"),
    }
}

/// #1675 probe (read race window): the checked file is swapped for a symlink
/// after the realpath/stat checks and before the content read.
#[cfg(unix)]
#[test]
fn read_rejects_a_file_swapped_inside_the_window() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    fs::write(outside.join("secret.txt"), b"top secret\n")
        .expect("#1675 write the file outside the root");
    fs::write(root.join("race.txt"), b"inside\n").expect("#1675 write the in-root file");
    let link_root = root.clone();
    let link_target = outside.join("secret.txt");
    let _guard = files::test_hook::set(move |phase| {
        if phase == "read" {
            fs::remove_file(link_root.join("race.txt")).expect("#1675 remove the in-root file");
            std::os::unix::fs::symlink(&link_target, link_root.join("race.txt"))
                .expect("#1675 install the racing symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.read_file("race.txt");
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "race.txt",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a raced read answers workspace_boundary, not a read error"),
        Ok(value) => panic!(
            "#1675 mutated: a raced read served content from outside the workspace: {}",
            json_of(&value)
        ),
    }
}

/// #1675 probe (middle component, deterministic): a directory inside the root
/// is a link to a directory outside it and the request names a file *below*
/// that link, so the escaping component is neither the first nor the last.
/// The realpath gate answers 403 before any descriptor is opened, which is why
/// this pin stays green with the descriptor walk switched off: what it fixes
/// is the shape of the answer, not the walk. The walk itself is pinned by the
/// raced form below and by `write_rejects_a_symlink_that_leaves_the_root`
/// (also a middle component, and bound to the switch).
#[cfg(unix)]
#[test]
fn read_rejects_a_symlink_in_a_middle_component() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    fs::write(outside.join("passwd"), b"outside bytes\n")
        .expect("#1675 write the file outside the root");
    std::os::unix::fs::symlink(&outside, root.join("etc")).expect("#1675 install the middle link");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.read_file("etc/passwd");
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "etc/passwd",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a middle link answers workspace_boundary, not a read error"),
        Ok(value) => panic!(
            "#1675 a middle symlink must not be followed: {}",
            json_of(&value)
        ),
    }
}

/// #1675 probe (middle component, race window): the directory in the middle of
/// the request is an ordinary in-root directory when the realpath gate runs and
/// is swapped for a link to the outside directory before the read. Only the
/// per-component `openat` walk refuses this — a last-segment `O_NOFOLLOW`, or a
/// canonicalize-then-open-by-path, follows the swapped middle component and
/// serves the outside file.
#[cfg(unix)]
#[test]
fn read_rejects_a_middle_component_swapped_inside_the_window() {
    let (parent, root) = fixture();
    let outside = parent.join("outside");
    fs::create_dir_all(&outside).expect("#1675 create the directory outside the root");
    fs::write(outside.join("passwd"), b"outside bytes\n")
        .expect("#1675 write the file outside the root");
    fs::create_dir_all(root.join("etc")).expect("#1675 create the in-root middle directory");
    fs::write(root.join("etc/passwd"), b"inside bytes\n").expect("#1675 write the in-root file");
    let link_root = root.clone();
    let link_target = outside.clone();
    let _guard = files::test_hook::set(move |phase| {
        if phase == "read" {
            fs::remove_dir_all(link_root.join("etc")).expect("#1675 remove the middle directory");
            std::os::unix::fs::symlink(&link_target, link_root.join("etc"))
                .expect("#1675 install the racing middle symlink");
        }
    });
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    let result = service.read_file("etc/passwd");
    match result {
        Err(files::FilesError::Workspace { raw }) => assert_eq!(
            raw, "etc/passwd",
            "#1675 the escaping path is reported verbatim"
        ),
        Err(_) => panic!("#1675 a raced middle component answers workspace_boundary"),
        Ok(value) => panic!(
            "#1675 mutated: the race window resolved a middle component outside the workspace: {}",
            json_of(&value)
        ),
    }
}

// --------------------------------- #1675 in-root links keep the TS behavior

/// #1675 counter-probe (file link): TS `resolveSafeWriting` never realpaths, so
/// `writeFileSync` follows a link that stays inside the root. The hardening
/// must keep that write-through; only a link that leaves the root is refused.
#[cfg(unix)]
#[test]
fn write_follows_a_link_that_stays_inside_the_root() {
    let (_parent, root) = fixture();
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    service
        .write_file(
            "inside-link",
            &pilotdeck_protocol::JsString::from("through the link\n"),
            false,
        )
        .map_err(|error| match error {
            files::FilesError::Workspace { raw } => {
                format!("#1675 an in-root link was refused as a boundary escape: {raw}")
            }
            files::FilesError::Message(text) => format!("#1675 the in-root write failed: {text}"),
            files::FilesError::MessageUnits(text) => {
                format!("#1675 the in-root write failed: {text:?}")
            }
        })
        .expect("#1675 an in-root link keeps TS's write-through");
    assert_eq!(
        fs::read(root.join("aaa.txt")).expect("#1675 read the link target"),
        b"through the link\n",
        "#1675 the bytes land on the link target"
    );
    assert!(
        fs::symlink_metadata(root.join("inside-link"))
            .expect("#1675 lstat the link")
            .file_type()
            .is_symlink(),
        "#1675 the write does not replace the link with a regular file"
    );
}

/// #1675 counter-probe (directory link): a directory link inside the root still
/// receives a new file below it, like TS recursive `mkdir` + `writeFile`.
#[cfg(unix)]
#[test]
fn write_creates_below_a_directory_link_that_stays_inside_the_root() {
    let (_parent, root) = fixture();
    std::os::unix::fs::symlink(root.join("sub"), root.join("sub-link"))
        .expect("#1675 link the in-root directory");
    let service =
        files::ProjectFileService::new(root.to_str().expect("#1675 the fixture root is UTF-8"));
    service
        .write_file(
            "sub-link/linked.txt",
            &pilotdeck_protocol::JsString::from("inside the root\n"),
            false,
        )
        .map_err(|error| match error {
            files::FilesError::Workspace { raw } => {
                format!("#1675 an in-root directory link was refused as a boundary escape: {raw}")
            }
            files::FilesError::Message(text) => format!("#1675 the in-root write failed: {text}"),
            files::FilesError::MessageUnits(text) => {
                format!("#1675 the in-root write failed: {text:?}")
            }
        })
        .expect("#1675 an in-root directory link keeps TS's behavior");
    assert_eq!(
        fs::read(root.join("sub/linked.txt")).expect("#1675 read the created file"),
        b"inside the root\n",
        "#1675 the file appears below the link target"
    );
}

// ----------------------------------------------------------------- normalize

/// The bytes both sides are compared in: the shared #1209 parser
/// (#1766, `replay_normalize`) plus the two api-web body masks — both
/// spellings of the fixture root fold to `<root>` and a directory's `size`
/// digit run folds to `0` (filesystem-dependent). `parser_admission` scopes
/// the `content-length: 0` drop (#2153) to the parser bare-400 gates — the
/// only responses whose hyper bytes carry the line the TS golden does not —
/// so every replayed cell's length lines compare verbatim; the
/// `no_oracle_cell_needs_the_content_length_exemption` guard holds that
/// empty set against the oracle.
fn normalize(raw: &[u8], parser_admission: bool, root: Option<&Path>) -> Vec<u8> {
    let parser_admission = if crate::mutants::on("api-web-replay-cl0-unscoped") {
        true
    } else if crate::mutants::on("api-web-replay-cl0-off") {
        false
    } else {
        parser_admission
    };
    replay_normalize::normalize(raw, parser_admission, &|body| mask_body(body, root))
}

/// The api-web body masks: both spellings of the fixture root fold to
/// `<root>` (the encoded form first, like the recorder), and a directory's
/// `size` digits fold to `0`. The oracle side already carries `<root>` and
/// the folded zero, so it only runs the digit fold.
fn mask_body(body: &[u8], root: Option<&Path>) -> Vec<u8> {
    let Some(root) = root else {
        return mask_directory_sizes(body);
    };
    let root = root.to_string_lossy();
    let encoded = percent_encode(&root);
    let once = replace_all(body, encoded.as_bytes(), b"<root>");
    let twice = replace_all(&once, root.as_bytes(), b"<root>");
    mask_directory_sizes(&twice)
}

fn mask_directory_sizes(raw: &[u8]) -> Vec<u8> {
    let marker: &[u8] = b"\"type\":\"directory\",\"size\":";
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = find(rest, marker) {
        out.extend_from_slice(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];
        let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
        out.push(b'0');
        rest = &rest[digits..];
    }
    out.extend_from_slice(rest);
    out
}

fn replace_all(raw: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = find(rest, needle) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(replacement);
        rest = &rest[at + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// The wire bytes the oracle's latin1-mapped response text names: every char
/// is one byte 0–255, so the map is 1:1.
fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars().map(|char| char as u8).collect()
}

fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

// ------------------------------------------------------------ visibility

/// A home with the fixture root registered under `home/projects/p1/.cwd`,
/// the marker registry both the WS RPCs and the native catalog read.
fn registered_home(parent: &Path, root: &Path) -> PathBuf {
    let home = parent.join("home");
    let project = home.join("projects").join("p1");
    fs::create_dir_all(&project).expect("#1348 create the registered project directory");
    fs::write(
        project.join(".cwd"),
        format!("{}\n", root.to_string_lossy()),
    )
    .expect("#1348 write the project marker");
    home
}

/// #1348: a file written through the HTTP surface is the file the WS lane
/// reads — the HTTP write lands on the same project root the
/// `project_file_read` RPC resolves through its registered marker, and the
/// request routes locally, not through a test-only entry.
#[tokio::test]
async fn http_write_visible_to_ws_read() {
    let (parent, root) = fixture();
    let home = registered_home(&parent, &root);
    let metadata = crate::metadata::Metadata {
        home,
        cwd: parent.clone(),
        files: crate::metadata::NativeFiles,
    };
    let root_text = root.to_string_lossy().into_owned();
    let body = "{\"path\":\"vis.txt\",\"content\":\"hello\"}";
    let request = format!(
        "POST /api/web/projects/{}/files/write HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        percent_encode(&root_text),
        body.len(),
        body
    );
    let (raw, _) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "ws-visibility",
    )
    .await;
    let written = latin1_string(&raw);
    assert!(
        written.starts_with("HTTP/1.1 200") && written.contains("{\"ok\":true}"),
        "#1348 the visibility write succeeds: {written:?}"
    );
    let request = js_object(vec![
        ("type", JsValue::from("request")),
        ("id", JsValue::from("1")),
        ("method", JsValue::from("project_file_read")),
        (
            "params",
            js_object(vec![
                ("projectKey", JsValue::from(root_text.as_str())),
                ("path", JsValue::from("vis.txt")),
            ]),
        ),
    ]);
    assert!(
        crate::dispatch::is_local(&request),
        "#1348 the WS lane routes project_file_read locally"
    );
    let crate::dispatch::Route::LocalResponse(bytes) =
        crate::dispatch::route_with_metadata(&request, Some(&metadata))
    else {
        panic!("#1348 the WS lane must answer the read locally");
    };
    let response = String::from_utf8(bytes).expect("#1348 the WS response is UTF-8");
    assert!(
        response.contains("\"content\":\"hello\"") && response.contains("\"encoding\":\"utf-8\""),
        "#1348 the WS read sees the HTTP-written file: {response}"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1348: a file that exists on the WS side of the same root — the state the
/// WS file tools leave behind — is what the HTTP read serves.
#[tokio::test]
async fn ws_side_file_visible_to_http_read() {
    let (parent, root) = fixture();
    fs::write(root.join("ws.txt"), b"from the ws side\n").expect("#1348 seed the WS-side file");
    let root_text = root.to_string_lossy().into_owned();
    let request = format!(
        "GET /api/web/projects/{}/files/read?path=ws.txt HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n",
        percent_encode(&root_text)
    );
    let (raw, _) = exchange(
        replay_config(&root_text),
        native_config(),
        request.as_bytes(),
        "ws-visibility",
    )
    .await;
    let response = latin1_string(&raw);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "#1348 the HTTP read serves the WS-side file: {response:?}"
    );
    assert!(
        response.contains("\"content\":\"from the ws side\\n\"")
            && response.contains("\"encoding\":\"utf8\""),
        "#1348 the HTTP read returns the WS-side bytes: {response:?}"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #1348: the production catalog the HTTP surface will be wired to (#1305)
/// lists the same bytes the WS `list_projects` RPC lists, over the same
/// marker-registered home.
#[tokio::test]
async fn http_catalog_matches_ws_list_projects() {
    let (parent, root) = fixture();
    let home = registered_home(&parent, &root);
    let metadata = crate::metadata::Metadata {
        home: home.clone(),
        cwd: parent.clone(),
        files: crate::metadata::NativeFiles,
    };
    let native = NativeProjectCatalog::new(home, parent.clone());
    let http_list = native
        .list_projects()
        .await
        .expect("#1348 the native catalog lists projects");
    let ws_list = metadata
        .list_projects()
        .expect("#1348 the WS metadata lists projects");
    let listed = stringify_json(&ws_list).expect("#1348 stringify the list");
    assert_eq!(
        stringify_json(&http_list).expect("#1348 stringify the catalog"),
        listed,
        "#1348 the HTTP catalog and the WS list_projects serve the same bytes"
    );
    assert!(
        listed
            .as_deref()
            .is_some_and(|text| text.contains(&*root.to_string_lossy())),
        "#1348 both surfaces list the marker-registered project"
    );
    let _ = fs::remove_dir_all(&parent);
}

/// #996-style regeneration guard for the fixture; see the fixture README for
/// the pinned Node invocation.
#[test]
#[ignore = "requires fixed PDRS_BASELINE and Node v22.22.0"]
fn regenerates_identically() {
    test_baseline::regenerates_identically("api-web");
}
