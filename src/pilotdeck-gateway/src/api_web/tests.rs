//! #1348: byte-level replay of the `api-web` golden fixture (every cell one
//! test), the two raw-byte pins the replay normalization cannot express, and
//! the bidirectional HTTP/WS visibility checks. The fixture was recorded
//! against the real TS stack (`startGatewayServer` with a scripted gateway
//! stub keyed on the fixture root, `describeProject` throwing for `boom`);
//! here the same requests go through the native hyper listener with the
//! api-web component composed in front of the #1209 router, which serves the
//! fallthrough 404s the TS server answers.
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

const ORACLE: &str = include_str!("../../tests/fixtures/api-web/oracle.json");
const DEADLINE: Duration = Duration::from_secs(10);
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
struct ScriptedCatalog {
    root: String,
}

impl ProjectCatalog for ScriptedCatalog {
    fn list_projects(&self) -> Pin<Box<dyn Future<Output = io::Result<JsValue>> + Send + '_>> {
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
/// EOF, or socket error ends the capture). The api-web component is composed
/// in front of the #1209 router, which answers the fallthrough 404s.
async fn exchange(
    config: Config,
    native_config: native_http::Config,
    request: &[u8],
    id: &str,
) -> (Vec<u8>, bool) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("#1348 bind listener");
    let bound = listener.local_addr().expect("#1348 listener local address");
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("#1348 accept connection");
            let config = config.clone();
            let native_config = native_config.clone();
            tokio::spawn(async move {
                // The production path always carries a stop signal; a sender
                // that is dropped on the spot is the no-stop case.
                let (_, stop) = tokio::sync::watch::channel(false);
                let served = http_proxy::serve_connection_native_with_shutdown(
                    socket,
                    move |mut request, _lease| {
                        let config = config.clone();
                        let native_config = native_config.clone();
                        async move {
                            match serve(&mut request, &config).await {
                                Some(answer) => answer,
                                None => native_http::serve(&request, &native_config).await,
                            }
                        }
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
        }),
    }
}

fn native_config() -> native_http::Config {
    native_http::Config {
        token: "fixture-token".into(),
        loopback: true,
        static_root: None,
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
        normalize(&raw, Some(&root)),
        normalize(&latin1_bytes(&cell.response), None),
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

// ----------------------------------------------------------------- normalize

/// The bytes both sides are compared in, the #1209 rules — the Date value
/// masked, header names case-folded (hyper renders Title-Case, TS's
/// `content-type` is literal lowercase — the registered transport diff), the
/// empty-body `content-length: 0` line dropped, chunk sizes case-folded —
/// plus the two api-web masks: both spellings of the fixture root fold to
/// `<root>` and a directory's `size` digit run folds to `0` (filesystem-
/// dependent). A chunked body is re-framed as one chunk whose size names the
/// masked content, because the oracle's recorded chunk sizes name the
/// pre-mask bytes — the same recomputation on both sides makes them agree.
fn normalize(raw: &[u8], root: Option<&Path>) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    'responses: while !rest.is_empty() {
        let Some(head_end) = find(rest, b"\r\n\r\n") else {
            out.extend_from_slice(rest);
            break;
        };
        let head = &rest[..head_end];
        let mut chunked = false;
        let mut length = None;
        for line in split(head, b"\r\n").skip(1) {
            let Some((name, value)) = split_once(line, b":") else {
                continue;
            };
            let name = trim_ascii(name).to_ascii_lowercase();
            if name == b"transfer-encoding"
                && find(&value.to_ascii_lowercase(), b"chunked").is_some()
            {
                chunked = true;
            } else if name == b"content-length" {
                length = std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.trim().parse::<usize>().ok());
            }
        }
        rest = &rest[head_end + 4..];
        if chunked {
            let Some(body) = chunked_content(&mut rest) else {
                emit_head(&mut out, head);
                out.extend_from_slice(rest);
                break 'responses;
            };
            let body = mask_body(&body, root);
            emit_head(&mut out, head);
            out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\r\n0\r\n\r\n");
            continue 'responses;
        }
        match length {
            Some(size) if rest.len() >= size => {
                let body = mask_body(&rest[..size], root);
                rest = &rest[size..];
                emit_head(&mut out, head);
                out.extend_from_slice(&body);
            }
            _ => {
                emit_head(&mut out, head);
                out.extend_from_slice(rest);
                break 'responses;
            }
        }
    }
    out
}

/// The head lines under the comparison rules; the status line is verbatim
/// (both stacks write canonical casing, and the one code without a canonical
/// reason never appears on this surface).
fn emit_head(out: &mut Vec<u8>, head: &[u8]) {
    for (index, line) in split(head, b"\r\n").enumerate() {
        if index == 0 {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            continue;
        }
        let Some((name, value)) = split_once(line, b":") else {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
            continue;
        };
        if name.eq_ignore_ascii_case(b"date") {
            out.extend_from_slice(b"date: <masked>\r\n");
        } else if name.eq_ignore_ascii_case(b"content-length") && trim_ascii(value) == b"0" {
            // hyper's implicit empty-body length.
        } else {
            out.extend_from_slice(&name.to_ascii_lowercase());
            out.extend_from_slice(b":");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\r\n");
}

/// One chunked body's content, single-chunk (both stacks write exactly one
/// data chunk per `writeHead`+`end` / single-frame response): the section
/// between the size line after the head and the terminal `0` chunk. The
/// oracle's recorded chunk sizes name the pre-mask content, so a size-line
/// walk cannot find the boundary there — the terminal marker can, and no
/// body carries a CRLF for it to hide inside. `None` when the bytes end
/// before the terminal chunk.
fn chunked_content(rest: &mut &[u8]) -> Option<Vec<u8>> {
    const TERMINAL: &[u8] = b"\r\n0\r\n\r\n";
    let end = find(rest, TERMINAL)?;
    let section = &rest[..end];
    // The first line is the (stale) size; the content is the rest.
    let content = split_once(section, b"\r\n").map_or(section, |(_, after)| after);
    *rest = &rest[end + TERMINAL.len()..];
    Some(content.to_vec())
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

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Split on every `separator`, the `str::split` shape (a trailing separator
/// yields a final empty slice; an empty haystack yields nothing).
fn split<'a>(haystack: &'a [u8], separator: &[u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = haystack;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match find(rest, separator) {
            Some(at) => {
                let (line, tail) = rest.split_at(at);
                rest = &tail[separator.len()..];
                Some(line)
            }
            None => {
                let line = rest;
                rest = &[];
                Some(line)
            }
        }
    })
}

fn split_once<'a>(haystack: &'a [u8], separator: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    find(haystack, separator).map(|at| {
        let (before, after) = haystack.split_at(at);
        (before, &after[separator.len()..])
    })
}

/// Trim ASCII whitespace from both ends.
fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &bytes[start..end]
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
