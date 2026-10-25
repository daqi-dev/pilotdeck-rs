//! Replay of the real-TS MCP client oracle
//! (`tests/fixtures/mcp-client/generate.mts`). Every scenario runs the Rust
//! `McpClient` against the same scripted stdio server (`mock_server.mjs`)
//! the TS `McpClient` was recorded against, then compares, byte for byte and
//! in order: each step's result/error and status, and per spawned process the
//! argv/cwd/env it saw and every frame the client wrote to it. The pure
//! naming/sanitising/config cases replay the TS helper outputs.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::mcp::{
    client::{McpCallOptions, McpClient, McpClientOptions},
    naming::{
        build_mcp_tool_wire_name, parse_mcp_tool_wire_name, recursively_sanitize_unicode,
        sanitize_unicode_string, truncate_mcp_tool_description,
    },
    protocol::{
        McpEnvironment, McpServerSpec, McpTransportSpec, expand_mcp_string, load_mcp_server_config,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use scratch::Scratch;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/mcp-client/oracle.json");
const SCENARIOS: usize = 26;
const STEPS: usize = 115;

fn mock_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-client/mock_server.mjs")
        .to_string_lossy()
        .into_owned()
}

fn text(value: &JsValue) -> String {
    stringify_json(value)
        .expect("serializable")
        .expect("not undefined")
}

static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(&UNDEFINED)
}
fn array<'a>(value: &'a JsValue, what: &str) -> &'a [JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("{what} must be an array"),
    }
}
fn string(value: &JsValue, what: &str) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("{what} must be a string"))
        .to_utf8()
        .unwrap()
}
fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

/// The generator's `fill`/`scrub` pair over every string in a tree.
fn map_strings(value: &JsValue, map: &dyn Fn(&str) -> String) -> JsValue {
    match value {
        JsValue::String(text) => {
            JsValue::String(map(&String::from_utf16_lossy(text.units())).into())
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|v| map_strings(v, map)).collect())
        }
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.own_entries() {
                out.insert(key.clone(), map_strings(item, map));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}
/// Per-run HTTP endpoints as `(text, placeholder)`.
type Hosts = Vec<(String, &'static str)>;

fn fill(value: &JsValue, dir: &str, hosts: &Hosts) -> JsValue {
    let mock = mock_path();
    map_strings(value, &|text| {
        let mut out = text.replace("<dir>", dir).replace("<mock>", &mock);
        for (host, placeholder) in hosts {
            out = out.replace(placeholder, host);
        }
        out
    })
}
fn scrub(value: &JsValue, dir: &str, hosts: &Hosts) -> JsValue {
    let temp = regex::Regex::new(r"(pilotdeck-mcp-[^/]*-)[A-Za-z0-9]{6}").unwrap();
    // Only strings that are valid UTF-16 survive the lossy round trip; the
    // corpus keeps lone surrogates out of scrubbed strings.
    let scrubbed = |text: &str| {
        let mut out = text.replace(dir, "<dir>");
        for (host, placeholder) in hosts {
            out = out.replace(host.as_str(), placeholder);
        }
        temp.replace_all(&out, "${1}XXXXXX").into_owned()
    };
    match value {
        JsValue::String(text) if text.to_utf8().is_err() => value.clone(),
        JsValue::String(_) => map_strings(value, &scrubbed),
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|v| scrub(v, dir, hosts)).collect())
        }
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.own_entries() {
                out.insert(key.clone(), scrub(item, dir, hosts));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn pairs(value: &JsValue) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    Some(
        object
            .own_entries()
            .into_iter()
            .map(|(key, value)| (key.to_utf8().unwrap(), string(value, "env/header value")))
            .collect(),
    )
}

fn spec(value: &JsValue) -> McpServerSpec {
    let transport = string(field(value, "transport"), "transport");
    if transport != "stdio" {
        let url = string(field(value, "url"), "url");
        let headers = pairs(field(value, "headers"));
        return McpServerSpec {
            id: string(field(value, "id"), "id"),
            transport: match transport.as_str() {
                "streamable_http" => McpTransportSpec::StreamableHttp { url, headers },
                "sse" => McpTransportSpec::Sse { url, headers },
                other => panic!("unknown transport {other}"),
            },
            call_timeout_ms: field(value, "callTimeoutMs").as_number(),
        };
    }
    let args = match field(value, "args") {
        JsValue::Undefined => None,
        args => Some(
            array(args, "args")
                .iter()
                .map(|arg| string(arg, "arg"))
                .collect(),
        ),
    };
    McpServerSpec {
        id: string(field(value, "id"), "id"),
        transport: McpTransportSpec::Stdio {
            command: string(field(value, "command"), "command"),
            args,
            env: pairs(field(value, "env")),
            cwd: field(value, "cwd")
                .as_string()
                .map(|cwd| cwd.to_utf8().unwrap()),
            per_session: field(value, "perSession").as_bool().unwrap_or(false),
        },
        call_timeout_ms: field(value, "callTimeoutMs").as_number(),
    }
}

fn options(value: &JsValue) -> McpClientOptions {
    McpClientOptions {
        call_timeout_ms: field(value, "callTimeoutMs").as_number(),
        handshake_timeout_ms: field(value, "handshakeTimeoutMs").as_number(),
        ..McpClientOptions::default()
    }
}

fn outcome<T>(
    result: Result<T, pilotdeck_gateway::runtime::mcp::client::McpClientError>,
    ok: impl FnOnce(T) -> JsValue,
) -> Vec<(&'static str, JsValue)> {
    match result {
        Ok(value) => vec![("ok", ok(value))],
        Err(error) => vec![("error", error.to_value())],
    }
}

type Fields = Vec<(&'static str, JsValue)>;
type OpFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Fields> + Send>>;

fn run_op(client: McpClient, op: JsValue, dir: PathBuf) -> OpFuture {
    Box::pin(async move {
        if let Some(delay) = field(&op, "delayMs").as_number() {
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        }
        match string(field(&op, "op"), "op").as_str() {
            "status" => vec![("ok", JsValue::String(client.status().as_str().into()))],
            "start" => {
                let mut out = outcome(client.start().await, |()| JsValue::Null);
                if out[0].0 == "ok" {
                    out.push(("instructions", JsValue::String(client.instructions())));
                }
                out
            }
            "listTools" => outcome(client.list_tools().await, |tools| {
                JsValue::Array(tools.iter().map(|tool| tool.to_value()).collect())
            }),
            "callTool" => {
                let name = field(&op, "name").as_string().unwrap().clone();
                let timeout_ms = field(&op, "timeoutMs").as_number();
                let result = client
                    .call_tool(
                        &name,
                        field(&op, "args"),
                        McpCallOptions {
                            timeout_ms,
                            ..McpCallOptions::default()
                        },
                    )
                    .await;
                outcome(result, |result| result.to_value())
            }
            "close" => {
                client.close().await;
                vec![("ok", JsValue::Null)]
            }
            "perSessionDirs" => {
                let base = dir.join("tmp/nested");
                let listing = std::fs::read_dir(&base).ok().map(|entries| {
                    let mut names: Vec<String> = entries
                        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    JsValue::Array(
                        names
                            .into_iter()
                            .map(|name| JsValue::String(name.into()))
                            .collect(),
                    )
                });
                vec![("ok", listing.unwrap_or(JsValue::Null))]
            }
            "parallel" => {
                let handles: Vec<_> = array(field(&op, "ops"), "ops")
                    .iter()
                    .map(|inner| tokio::spawn(run_op(client.clone(), inner.clone(), dir.clone())))
                    .collect();
                let mut results = Vec::new();
                for handle in handles {
                    let fields = handle.await.unwrap();
                    results.push(object(fields));
                }
                vec![("ok", JsValue::Array(results))]
            }
            other => panic!("unknown op {other}"),
        }
    })
}

fn read_logs(dir: &Path) -> Vec<Vec<JsValue>> {
    let mut logs = Vec::new();
    for index in 0.. {
        let Ok(text) = std::fs::read_to_string(dir.join(format!("log-{index}.jsonl"))) else {
            break;
        };
        logs.push(
            text.lines()
                .filter(|line| !line.is_empty())
                .map(|line| parse_json(line).unwrap())
                .collect(),
        );
    }
    logs
}

async fn settle(dir: &Path, orphan: bool) {
    let has = |log: &Vec<JsValue>, event: &str| {
        log.iter().any(|entry| {
            field(entry, "event")
                .as_string()
                .is_some_and(|e| e.equals_str(event))
        })
    };
    for _ in 0..400 {
        let logs = read_logs(dir);
        let done = if orphan {
            !logs.is_empty() && logs.iter().all(|log| has(log, "frame"))
        } else {
            logs.iter().all(|log| has(log, "exit"))
        };
        if done {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("mock processes did not settle in {}", dir.display());
}

/// The mock in `--http` mode; dropping it ends its stdin and kills it.
struct HttpMock {
    child: tokio::process::Child,
    port: u16,
}
impl HttpMock {
    async fn start(dir: &Path) -> Self {
        use tokio::io::AsyncBufReadExt;
        let mut child = tokio::process::Command::new("node")
            .arg(mock_path())
            .arg("--http")
            .env("MOCK_DIR", dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn HTTP mock");
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let line = lines
            .next_line()
            .await
            .unwrap()
            .expect("HTTP mock port line");
        let port = line
            .strip_prefix("PORT ")
            .expect("PORT line")
            .parse()
            .unwrap();
        Self { child, port }
    }
}

/// A port nothing listens on.
fn refused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// `http.jsonl`, scrubbed and sorted by text like the generator.
fn read_http_log(dir: &Path, hosts: &Hosts) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("http.jsonl")).unwrap_or_default();
    let dir_text = dir.to_string_lossy();
    let mut entries: Vec<String> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| self::text(&scrub(&parse_json(line).unwrap(), &dir_text, hosts)))
        .collect();
    entries.sort();
    entries
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replays_ts_mcp_client_against_the_scripted_server() {
    // The scripted server is plain Node ESM; fail loudly rather than report
    // spawn errors as a behaviour diff.
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|output| output.status.success()),
        "the MCP client replay needs `node` (>= 18) on PATH to run mock_server.mjs"
    );
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(string(field(&oracle, "sdkVersion"), "sdkVersion"), "1.29.0");
    let scenarios = array(field(&oracle, "scenarios"), "scenarios");
    assert_eq!(scenarios.len(), SCENARIOS);
    let scratch = Scratch::new("mcp-client");
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let mut steps_seen = 0;
    for scenario in scenarios {
        let id = string(field(scenario, "id"), "id");
        let dir = root.join(&id);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        std::fs::write(dir.join("script.json"), text(field(scenario, "script"))).unwrap();
        let dir_text = dir.to_string_lossy().into_owned();
        let http = !field(field(scenario, "script"), "http").is_undefined();
        let mock = if http {
            Some(HttpMock::start(&dir).await)
        } else {
            None
        };
        let hosts: Hosts = match &mock {
            Some(mock) => vec![
                (format!("127.0.0.1:{}", mock.port), "<host>"),
                (format!("127.0.0.1:{}", refused_port()), "<refused>"),
            ],
            None => Vec::new(),
        };
        let client = McpClient::new(
            spec(&fill(field(scenario, "spec"), &dir_text, &hosts)),
            options(field(scenario, "options")),
        );
        let expected_steps = array(field(scenario, "steps"), "steps");
        for (index, expected) in expected_steps.iter().enumerate() {
            let op = field(expected, "step").clone();
            let mut fields = vec![("step", op.clone())];
            fields.extend(run_op(client.clone(), op, dir.clone()).await);
            fields.push(("status", JsValue::String(client.status().as_str().into())));
            let actual = scrub(&object(fields), &dir_text, &hosts);
            assert_eq!(
                text(&actual),
                text(expected),
                "scenario {id} step {index} differs"
            );
            steps_seen += 1;
        }
        if let Some(mut mock) = mock {
            let expected: Vec<String> = array(field(scenario, "http"), "http")
                .iter()
                .map(text)
                .collect();
            // Detached posts (ping replies) may still be in flight.
            let mut actual = read_http_log(&dir, &hosts);
            for _ in 0..200 {
                if actual.len() >= expected.len() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                actual = read_http_log(&dir, &hosts);
            }
            assert_eq!(actual, expected, "scenario {id} HTTP requests differ");
            drop(mock.child.stdin.take());
            let _ = mock.child.wait().await;
        } else {
            settle(&dir, field(scenario, "orphan").as_bool().unwrap_or(false)).await;
        }
        let processes: Vec<JsValue> = read_logs(&dir)
            .into_iter()
            .map(|log| {
                JsValue::Array(
                    log.iter()
                        .map(|entry| scrub(entry, &dir_text, &hosts))
                        .collect(),
                )
            })
            .collect();
        let expected = array(field(scenario, "processes"), "processes");
        assert_eq!(
            processes.len(),
            expected.len(),
            "scenario {id} process count"
        );
        for (index, (actual, expected)) in processes.iter().zip(expected).enumerate() {
            assert_eq!(
                text(actual),
                text(expected),
                "scenario {id} process {index} log differs"
            );
        }
    }
    assert_eq!(steps_seen, STEPS);
}

struct FixedEnvironment;
impl McpEnvironment for FixedEnvironment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        name.equals_str("PDRS_MCP_SET").then(|| "value".into())
    }
    fn home(&self) -> JsString {
        "/pdrs-home".into()
    }
}

#[test]
fn replays_ts_naming_sanitize_truncate_and_config() {
    let oracle = baseline::oracle(ORACLE);
    let pure = field(&oracle, "pure");
    let cases = |name: &str| array(field(pure, name), name);
    let counts: Vec<usize> = [
        "sanitize",
        "recursive",
        "wire",
        "parse",
        "truncate",
        "expand",
        "config",
    ]
    .iter()
    .map(|name| cases(name).len())
    .collect();
    assert_eq!(counts, [6, 4, 7, 7, 5, 11, 7]);
    let js = |value: &JsValue| value.as_string().unwrap().clone();
    for case in cases("sanitize") {
        let output = JsValue::String(sanitize_unicode_string(&js(field(case, "input"))));
        assert_eq!(text(&output), text(field(case, "output")));
    }
    for case in cases("recursive") {
        let output = recursively_sanitize_unicode(field(case, "input")).unwrap();
        assert_eq!(text(&output), text(field(case, "output")));
    }
    for case in cases("wire") {
        let output = build_mcp_tool_wire_name(&js(field(case, "server")), &js(field(case, "tool")));
        assert_eq!(text(&JsValue::String(output)), text(field(case, "output")));
    }
    for case in cases("parse") {
        let output = match parse_mcp_tool_wire_name(&js(field(case, "input"))) {
            Some((server, tool)) => object(vec![
                ("serverId", JsValue::String(server)),
                ("toolName", JsValue::String(tool)),
            ]),
            None => JsValue::Null,
        };
        assert_eq!(text(&output), text(field(case, "output")));
    }
    for case in cases("truncate") {
        let output = truncate_mcp_tool_description(&js(field(case, "input")));
        assert_eq!(text(&JsValue::String(output)), text(field(case, "output")));
    }
    for case in cases("expand") {
        let output = expand_mcp_string(&js(field(case, "input")), &FixedEnvironment);
        assert_eq!(text(&JsValue::String(output)), text(field(case, "output")));
    }
    let scratch = Scratch::new("mcp-config");
    for case in cases("config") {
        let id = string(field(case, "id"), "id");
        let dir = scratch.path().join(&id);
        let home = dir.join("home");
        let project = dir.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".pilotdeck")).unwrap();
        let write = |path: PathBuf, value: &JsValue| match value {
            JsValue::Undefined => {}
            JsValue::String(raw) => std::fs::write(path, raw.to_utf8().unwrap()).unwrap(),
            other => std::fs::write(path, text(other)).unwrap(),
        };
        write(home.join("mcp.json"), field(case, "global"));
        write(project.join(".pilotdeck/mcp.json"), field(case, "project"));
        if field(case, "globalDir").as_bool() == Some(true) {
            std::fs::create_dir(home.join("mcp.json")).unwrap();
        }
        let result = load_mcp_server_config(&project, &home, &FixedEnvironment).unwrap();
        let output = scrub(&result.to_value(), &dir.to_string_lossy(), &Vec::new());
        assert_eq!(
            text(&output),
            text(field(case, "output")),
            "config case {id}"
        );
    }
}

#[test]
fn deep_trees_report_range_error_instead_of_recursing() {
    let mut value = JsValue::String("\u{200B}".into());
    for _ in 0..100_000 {
        value = JsValue::Array(vec![value]);
    }
    assert!(recursively_sanitize_unicode(&value).is_err());
    let mut shallow = JsValue::String("x\u{200B}".into());
    for _ in 0..100 {
        shallow = JsValue::Array(vec![shallow]);
    }
    assert!(recursively_sanitize_unicode(&shallow).is_ok());
}

/// The generator is deterministic: two runs into fresh directories produce
/// the committed oracle byte for byte.
#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn generator_reproduces_the_committed_oracle_twice() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-client/generate.mts");
    for run in 0..2 {
        let scratch = Scratch::new("mcp-client-generate");
        let status = std::process::Command::new("node")
            .env("LANG", "en_US.UTF-8")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(&generator)
            .arg(scratch.path())
            .status()
            .expect("run node");
        assert!(status.success(), "generator run {run} failed");
        let generated = std::fs::read_to_string(scratch.path().join("oracle.json")).unwrap();
        assert!(
            generated == ORACLE,
            "generator run {run} differs from the committed oracle"
        );
    }
}

// ---------------------------------------------------------------------------
// Connect/close/reconnect interleavings. Ordering comes from the
// current-thread runtime's poll order (`join!` polls left to right) and from
// file gates the mock waits on (`wait_file`); bounded polling only observes
// events, it never decides who goes first.

const INIT_REPLY: &str = r#"{"send":{"jsonrpc":"2.0","id":"$id","result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"m","version":"1"}}}}"#;

fn race_client(dir: &Path, procs: &[String], handshake_timeout_ms: Option<f64>) -> McpClient {
    race_client_with_session(dir, procs, handshake_timeout_ms, false)
}

fn race_client_with_session(
    dir: &Path,
    procs: &[String],
    handshake_timeout_ms: Option<f64>,
    per_session: bool,
) -> McpClient {
    std::fs::write(
        dir.join("script.json"),
        format!(r#"{{"procs":[{}]}}"#, procs.join(",")),
    )
    .unwrap();
    McpClient::new(
        McpServerSpec {
            id: "mock".into(),
            transport: McpTransportSpec::Stdio {
                command: "node".into(),
                args: Some(vec![mock_path()]),
                env: Some(vec![
                    ("MOCK_DIR".into(), dir.to_string_lossy().into_owned()),
                    (
                        "PILOTDECK_SESSION_TMPDIR".into(),
                        dir.join("sessions").to_string_lossy().into_owned(),
                    ),
                ]),
                cwd: None,
                per_session,
            },
            call_timeout_ms: None,
        },
        McpClientOptions {
            handshake_timeout_ms,
            ..McpClientOptions::default()
        },
    )
}

fn logged(dir: &Path, index: usize, event: &str) -> bool {
    read_logs(dir).get(index).is_some_and(|log| {
        log.iter().any(|e| {
            field(e, "event")
                .as_string()
                .is_some_and(|k| k.equals_str(event))
        })
    })
}

async fn wait_logged(dir: &Path, index: usize, event: &str) {
    for _ in 0..400 {
        if logged(dir, index, event) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("process {index} never logged {event:?}");
}

fn open_gate(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "").unwrap();
}

const ABORTED: &str = "MCP handshake aborted: client closed during connect (server=mock)";

/// GLM r3 P1-1 (a): `close()` completes after `start()` published its
/// attempt but before the spawned connect task first ran. The attempt's
/// epoch is taken when it is published, so it is already stale and spawns
/// nothing.
#[tokio::test]
async fn close_before_the_connect_task_runs_aborts_it() {
    let scratch = Scratch::new("mcp-client-race-a");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client(&dir, &[format!(r#"{{"initialize":[{INIT_REPLY}]}}"#)], None);
    // `start` publishes and spawns (the task cannot run before this poll
    // yields); `close` then runs to completion without awaiting.
    let (started, ()) = tokio::join!(client.start(), client.close());
    assert_eq!(started.expect_err("superseded").message(), ABORTED);
    assert_eq!(client.status().as_str(), "idle");
    assert!(read_logs(&dir).is_empty(), "no process may be spawned");
    // The next start connects normally, with exactly one process.
    client.start().await.expect("fresh connect");
    assert_eq!(client.status().as_str(), "ready");
    client.close().await;
    settle(&dir, false).await;
    assert_eq!(read_logs(&dir).len(), 1);
}

/// gate2 P1-1 (intentional difference from TS, which installs it): `close()`
/// while the first handshake is in flight, then a second `start()`; the
/// first connection lands last (gated, not timed) and must be closed, not
/// installed over the second.
#[tokio::test]
async fn close_during_handshake_then_start_keeps_one_connection() {
    let scratch = Scratch::new("mcp-client-race-b");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client(
        &dir,
        &[
            format!(r#"{{"initialize":[{{"wait_file":"g0"}},{INIT_REPLY}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
        ],
        None,
    );
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.start().await }
    });
    wait_logged(&dir, 0, "frame").await;
    client.close().await;
    client.start().await.expect("second connection");
    open_gate(&dir, "g0");
    let first = first.await.unwrap().expect_err("first attempt is aborted");
    assert_eq!(first.message(), ABORTED);
    assert_eq!(client.status().as_str(), "ready");
    wait_logged(&dir, 0, "exit").await;
    assert!(
        !logged(&dir, 1, "exit"),
        "the live connection keeps running"
    );
    client.close().await;
    settle(&dir, false).await;
    assert_eq!(read_logs(&dir).len(), 2);
}

/// GLM r3 P1-1 (b): a `start()` arrives while `close()` is still awaiting
/// the old connection's shutdown. It must open a new connection (not join
/// the closed one's memo), and `close()`'s tail must not mark it idle.
#[tokio::test]
async fn start_while_close_awaits_the_old_connection_keeps_the_new_one() {
    let scratch = Scratch::new("mcp-client-race-c");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client(
        &dir,
        &[
            // proc 0 lingers after stdin ends until the gate opens.
            format!(r#"{{"initialize":[{INIT_REPLY}],"stdin-end":[{{"wait_file":"g0"}}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
        ],
        None,
    );
    client.start().await.expect("first connection");
    let ((), status_after_start) = tokio::join!(client.close(), async {
        client.start().await.expect("new connection");
        let status = client.status().as_str();
        open_gate(&dir, "g0");
        status
    });
    assert_eq!(status_after_start, "ready");
    assert_eq!(
        client.status().as_str(),
        "ready",
        "close() must not clobber"
    );
    assert!(logged(&dir, 0, "exit"), "close() awaited the old process");
    assert!(!logged(&dir, 1, "exit"), "the new connection is live");
    assert_eq!(read_logs(&dir).len(), 2);
    client.close().await;
    settle(&dir, false).await;
}

/// GLM r3 P2-1: a `start()` racing a session-expired reconnect while the old
/// connection closes. The reconnect drops the old slot first, so the racing
/// `start()` publishes the new attempt and the reconnect joins it: one new
/// process, and `start()` never reports success without a connection.
#[tokio::test]
async fn start_racing_a_reconnect_shares_one_new_connection() {
    let scratch = Scratch::new("mcp-client-race-d");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let expired = r#"{"send":{"jsonrpc":"2.0","id":"$id","error":{"code":-32000,"message":"Session expired"}}}"#;
    let after = r#"{"send":{"jsonrpc":"2.0","id":"$id","result":{"content":[{"type":"text","text":"after reconnect"}]}}}"#;
    let client = race_client(
        &dir,
        &[
            format!(
                r#"{{"initialize":[{INIT_REPLY}],"tools/call:x":[{expired}],"stdin-end":[{{"wait_file":"g0"}}]}}"#
            ),
            format!(r#"{{"initialize":[{INIT_REPLY}],"tools/call:x":[{after}]}}"#),
        ],
        None,
    );
    client.start().await.expect("first connection");
    let name = JsString::from("x");
    let args = JsValue::Undefined;
    let (called, connected) = tokio::join!(
        client.call_tool(&name, &args, McpCallOptions::default()),
        async {
            wait_logged(&dir, 0, "stdin-end").await;
            client.start().await.expect("racing start");
            let connected = client.status().as_str();
            open_gate(&dir, "g0");
            connected
        }
    );
    assert_eq!(
        connected, "ready",
        "start() returned before a connection existed"
    );
    let called = called.expect("call after reconnect");
    assert_eq!(
        text(&called.content),
        r#"[{"type":"text","text":"after reconnect"}]"#
    );
    assert_eq!(read_logs(&dir).len(), 2, "exactly one new connection");
    assert_eq!(client.status().as_str(), "ready");
    client.close().await;
    settle(&dir, false).await;
}

/// gate1 P1: a handshake that lands after the handshake timeout belongs to
/// no attempt; its connection is closed so the child is reaped.
#[tokio::test]
async fn late_handshake_after_timeout_is_closed() {
    let scratch = Scratch::new("mcp-client-race-e");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client(
        &dir,
        &[
            format!(r#"{{"initialize":[{{"wait_file":"g0"}},{INIT_REPLY}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
        ],
        Some(10_000.0),
    );
    let (started, ()) = tokio::join!(client.start(), async {
        // The first initialize is gated, so only the handshake timer can
        // finish this attempt. Advance it after the child receives the
        // request, then restore wall time for late close and the retry.
        // A fresh Node process under parallel test load must not have to
        // start within 200 ms just to exercise that unrelated retry.
        wait_logged(&dir, 0, "frame").await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::time::resume();
    });
    let error = started.expect_err("timed out");
    assert_eq!(
        error.message(),
        "MCP handshake timed out after 10000ms (server=mock)"
    );
    // The handshake can only finish now, strictly after the timeout.
    open_gate(&dir, "g0");
    for _ in 0..400 {
        if logged(&dir, 0, "exit") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        logged(&dir, 0, "exit"),
        "the late connection's process must exit"
    );
    let frames = read_logs(&dir)[0]
        .iter()
        .filter(|e| {
            field(e, "event")
                .as_string()
                .is_some_and(|k| k.equals_str("frame"))
        })
        .count();
    assert_eq!(frames, 2, "initialize, then notifications/initialized");
    client.start().await.expect("retry connects");
    client.close().await;
    settle(&dir, false).await;
    assert_eq!(read_logs(&dir).len(), 2);
}

fn logged_session_dir(dir: &Path, index: usize) -> PathBuf {
    let logs = read_logs(dir);
    let spawn = logs[index]
        .iter()
        .find(|event| {
            field(event, "event")
                .as_string()
                .is_some_and(|s| s.equals_str("spawn"))
        })
        .unwrap();
    array(field(spawn, "argv"), "argv")
        .iter()
        .find_map(|arg| {
            arg.as_string()?
                .to_utf8()
                .ok()?
                .strip_prefix("--user-data-dir=")
                .map(PathBuf::from)
        })
        .expect("per-session directory argument")
}

#[tokio::test]
async fn timed_out_handshake_keeps_its_directory_until_late_close_then_removes_it() {
    let scratch = Scratch::new("mcp-session-retry");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client_with_session(
        &dir,
        &[
            format!(r#"{{"initialize":[{{"wait_file":"old-init"}},{INIT_REPLY}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
        ],
        Some(1000.0),
        true,
    );
    assert!(
        client
            .start()
            .await
            .unwrap_err()
            .message()
            .contains("timed out")
    );
    wait_logged(&dir, 0, "spawn").await;
    let old = logged_session_dir(&dir, 0);
    client.start().await.unwrap();
    let new = logged_session_dir(&dir, 1);
    assert_ne!(old, new);
    assert!(old.exists(), "late handshake still owns its directory");
    assert!(new.exists());
    open_gate(&dir, "old-init");
    wait_logged(&dir, 0, "exit").await;
    // The exit log precedes the close task's directory removal.
    tokio::time::timeout(Duration::from_secs(5), async {
        while old.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("late close must remove the old directory");
    assert!(
        new.exists(),
        "old cleanup must not remove the new directory"
    );
    client.close().await;
    assert!(!new.exists());
}

#[tokio::test]
async fn reconnect_removes_the_previous_per_session_directory() {
    let scratch = Scratch::new("mcp-session-reconnect");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let expired = r#"{"send":{"jsonrpc":"2.0","id":"$id","error":{"code":-32000,"message":"Session expired"}}}"#;
    let reply = r#"{"send":{"jsonrpc":"2.0","id":"$id","result":{"content":[]}}}"#;
    let client = race_client_with_session(
        &dir,
        &[
            format!(r#"{{"initialize":[{INIT_REPLY}],"tools/call:x":[{expired}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}],"tools/call:x":[{reply}]}}"#),
        ],
        None,
        true,
    );
    client.start().await.unwrap();
    let old = logged_session_dir(&dir, 0);
    assert!(old.exists());
    client
        .call_tool(&"x".into(), &JsValue::Null, McpCallOptions::default())
        .await
        .unwrap();
    let new = logged_session_dir(&dir, 1);
    assert!(!old.exists(), "reconnect must not orphan the old directory");
    assert!(new.exists());
    client.close().await;
    assert!(!new.exists());
}

/// The failed initialize rejects immediately, but its original background
/// close still owns the child and directory while a retry becomes ready.
#[tokio::test(start_paused = true)]
async fn failed_handshake_retry_waits_for_old_close_before_removing_directory() {
    failed_handshake_directory_cleanup(false).await;
}

#[tokio::test(start_paused = true)]
async fn cancelled_close_keeps_failed_handshake_directory_cleanup_owned() {
    failed_handshake_directory_cleanup(true).await;
}

async fn failed_handshake_directory_cleanup(cancel_close: bool) {
    struct ClockHold(tokio::task::JoinHandle<()>);
    impl Drop for ClockHold {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _clock = ClockHold(tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    }));
    let scenario = async {
        let scratch = Scratch::new("mcp-failed-handshake-retry");
        let dir = std::fs::canonicalize(scratch.path()).unwrap();
        let bad_init = INIT_REPLY.replace("2025-06-18", "unsupported");
        let client = race_client_with_session(
            &dir,
            &[
                format!(
                    r#"{{"initialize":[{bad_init}],"stdin-end":[{{"wait_file":"old-exit"}}]}}"#
                ),
                format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
            ],
            None,
            true,
        );
        assert!(
            client
                .start()
                .await
                .unwrap_err()
                .message()
                .contains("not supported")
        );
        while !logged(&dir, 0, "stdin-end") {
            tokio::task::yield_now().await;
        }
        let old = logged_session_dir(&dir, 0);
        if cancel_close {
            let mut close = Box::pin(client.close());
            tokio::select! {
                biased;
                () = &mut close => panic!("close must wait for the gated old process"),
                () = tokio::task::yield_now() => {}
            }
            drop(close);
            assert!(
                old.exists(),
                "cancelling close must not delete a live child's directory"
            );
            open_gate(&dir, "old-exit");
            while !logged(&dir, 0, "exit") || old.exists() {
                tokio::task::yield_now().await;
            }
            return;
        }
        client.start().await.unwrap();
        let new = logged_session_dir(&dir, 1);
        assert_ne!(old, new);
        assert!(!logged(&dir, 0, "exit"), "old process is gated before exit");
        let old_survived_grace = old.exists();
        assert!(new.exists());
        // Model a last write by the still-running old process during grace.
        if old_survived_grace {
            std::fs::write(old.join("last-write"), "during close").unwrap();
        }
        open_gate(&dir, "old-exit");
        while !logged(&dir, 0, "exit") || old.exists() {
            tokio::task::yield_now().await;
        }
        assert!(
            new.exists(),
            "old cleanup must leave the retry directory alone"
        );
        client.close().await;
        assert!(!new.exists());
        // Assert the observation after releasing the child so even an
        // intentionally broken cleanup in a negative control is reaped.
        assert!(
            old_survived_grace,
            "old directory must survive the close grace period"
        );
    };
    // Wall-time failure bound only; virtual time never advances to the TERM
    // or KILL grace deadlines, so file gates determine the whole ordering.
    let watchdog = async {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        () = scenario => {}
        () = watchdog => panic!("failed-handshake retry exceeded its wall-time bound"),
    }
}

/// Dispose must join retired cleanup before the executor can be shut down.
/// Cancel the first close caller while the failed child is gated, then let
/// the real session registry own the final stop.
#[tokio::test(start_paused = true)]
async fn registry_dispose_joins_retired_directory_after_cancelled_close() {
    use pilotdeck_gateway::runtime::mcp::{
        registry::SessionMcpRuntimeRegistry,
        runtime::{McpRuntime, McpRuntimeOptions},
    };
    use std::sync::Arc;
    struct ClockHold(tokio::task::JoinHandle<()>);
    impl Drop for ClockHold {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _clock = ClockHold(tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    }));
    let scenario = async {
        let scratch = Scratch::new("mcp-dispose-retired");
        let dir = std::fs::canonicalize(scratch.path()).unwrap();
        let bad_init = INIT_REPLY.replace("2025-06-18", "unsupported");
        let template = race_client_with_session(
            &dir,
            &[
                format!(
                    r#"{{"initialize":[{bad_init}],"stdin-end":[{{"wait_file":"old-exit"}}]}}"#
                ),
                format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
            ],
            None,
            true,
        );
        let runtime = Arc::new(McpRuntime::new(
            vec![template.spec().clone()],
            McpRuntimeOptions::default(),
        ));
        let client = runtime.client("mock").unwrap();
        assert!(client.start().await.is_err());
        while !logged(&dir, 0, "stdin-end") {
            tokio::task::yield_now().await;
        }
        let old = logged_session_dir(&dir, 0);
        client.start().await.unwrap();
        let new = logged_session_dir(&dir, 1);
        let mut close = Box::pin(client.close());
        let returned_early = loop {
            tokio::select! {
                biased;
                () = &mut close => break true,
                () = tokio::task::yield_now() => {
                    if !new.exists() && logged(&dir, 1, "exit") { break false; }
                }
            }
        };
        drop(close);
        let registry = SessionMcpRuntimeRegistry::new();
        let _registration = registry.register("session", runtime.clone()).unwrap();
        let mut dispose = Box::pin(registry.dispose());
        // Both close and registry disposal use owned completion tickets. A
        // cancelled close must not hide the old gated directory from dispose.
        let mut disposed_early = false;
        for _ in 0..64 {
            tokio::select! {
                biased;
                result = &mut dispose => {
                    result.unwrap();
                    disposed_early = true;
                    break;
                }
                () = tokio::task::yield_now() => {}
            }
        }
        let old_still_live = old.exists() && !logged(&dir, 0, "exit");
        open_gate(&dir, "old-exit");
        if !disposed_early {
            dispose.await.unwrap();
        }
        let deleted_on_return = !old.exists() && !new.exists();
        // Reap even an intentionally broken detached implementation before
        // asserting its observation, so a negative control leaves no child.
        while !logged(&dir, 0, "exit") || old.exists() {
            tokio::task::yield_now().await;
        }
        assert!(!returned_early, "close returned before retired cleanup");
        assert!(
            !disposed_early,
            "dispose lost the cancelled close's retired cleanup"
        );
        assert!(old_still_live);
        assert!(
            deleted_on_return,
            "dispose must finish deletion before returning"
        );
    };
    let watchdog = async {
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < end {
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        () = scenario => {}
        () = watchdog => panic!("dispose cleanup scenario exceeded wall-time bound"),
    }
}

/// Two real request timers on W1 expire on opposite sides of W2 publishing
/// its handshake. Virtual time moves only at the explicit advance calls;
/// file events establish that each process/request reached the right point.
#[tokio::test(start_paused = true)]
async fn second_old_timeout_does_not_discard_the_new_handshake() {
    bounded_double_timeout(false).await;
}

#[tokio::test(start_paused = true)]
async fn second_old_timeout_does_not_recycle_the_new_ready_client() {
    bounded_double_timeout(true).await;
}

async fn bounded_double_timeout(after_ready: bool) {
    // The virtual clock is deliberately held. Use wall time only as a failure
    // bound, including for awaits on start/close that a regression could hang.
    let watchdog = async {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        () = double_timeout(after_ready) => {}
        () = watchdog => panic!("double-timeout scenario exceeded its wall-time bound"),
    }
}

async fn double_timeout(after_ready: bool) {
    // Node is an external process. Keep virtual time from automatically
    // jumping while the runtime waits for its I/O.
    struct ClockHold(tokio::task::JoinHandle<()>);
    impl Drop for ClockHold {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _clock = ClockHold(tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    }));
    async fn until(mut check: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !check() {
            assert!(
                std::time::Instant::now() < deadline,
                "mock event did not arrive"
            );
            tokio::task::yield_now().await;
        }
    }
    let scratch = Scratch::new("mcp-client-double-timeout");
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
    let client = race_client(
        &dir,
        &[
            format!(r#"{{"initialize":[{INIT_REPLY}],"stdin-end":[{{"wait_file":"old-exit"}}]}}"#),
            format!(r#"{{"initialize":[{{"wait_file":"new-init"}},{INIT_REPLY}]}}"#),
            format!(r#"{{"initialize":[{INIT_REPLY}]}}"#),
        ],
        None,
    );
    client.start().await.unwrap();
    let call = |name: &'static str, timeout_ms| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .call_tool(
                    &name.into(),
                    &JsValue::Null,
                    McpCallOptions {
                        timeout_ms: Some(timeout_ms),
                        ..McpCallOptions::default()
                    },
                )
                .await
        })
    };
    let first = call("first", 10.0);
    let second = call("second", 100.0);
    until(|| {
        read_logs(&dir).first().is_some_and(|log| {
            log.iter()
                .filter(|entry| {
                    field(entry, "line").as_string().is_some_and(|line| {
                        line.to_utf8()
                            .unwrap()
                            .contains("\"method\":\"tools/call\"")
                    })
                })
                .count()
                == 2
        })
    })
    .await;
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(
        first
            .await
            .unwrap()
            .unwrap_err()
            .client_code()
            .unwrap()
            .as_str(),
        "mcp_call_timeout"
    );
    until(|| logged(&dir, 0, "stdin-end")).await;
    let mut start = Some(tokio::spawn({
        let client = client.clone();
        async move { client.start().await }
    }));
    until(|| logged(&dir, 1, "frame")).await;
    if after_ready {
        open_gate(&dir, "new-init");
        start.take().unwrap().await.unwrap().unwrap();
    }
    tokio::time::advance(Duration::from_millis(90)).await;
    assert_eq!(
        second
            .await
            .unwrap()
            .unwrap_err()
            .client_code()
            .unwrap()
            .as_str(),
        "mcp_call_timeout"
    );
    // A third caller must join W2. Poll it before opening W2's gate; the
    // broken implementation instead publishes W3, superseding W2.
    let (joined, started) = tokio::join!(client.start(), async {
        open_gate(&dir, "new-init");
        open_gate(&dir, "old-exit");
        match start {
            Some(start) => start.await.unwrap(),
            None => Ok(()),
        }
    });
    client.close().await;
    until(|| {
        read_logs(&dir).iter().all(|log| {
            log.iter().any(|e| {
                field(e, "event")
                    .as_string()
                    .is_some_and(|e| e.equals_str("exit"))
            })
        })
    })
    .await;
    joined.expect("third caller joins W2");
    started.expect("W2 must not be superseded by a stale timeout");
    assert_eq!(read_logs(&dir).len(), 2, "only W1 and W2 may exist");
}

/// #681（A6.5-MCP-CONNECT-HOSTNAME-001）：主机名 URL 的拒连文案显示解析出
/// 的地址，而不是 URL 主机名。TS 金样 2026-10-25 实测（证据
/// `_tmp/20261025-issue-681/ts-probe.out`）：legacy SSE 连
/// `http://localhost:<closed>/sse` 报
/// `MCP handshake failed: SSE error: TypeError: fetch failed: connect
/// ECONNREFUSED ::1:60169, connect ECONNREFUSED 127.0.0.1:60169`。
/// `localhost` 的解析结果与顺序由本机 getaddrinfo 决定，所以这里钉住两条
/// 不变量：文案不含 URL 主机名，且以解析地址逐条列举。
#[tokio::test]
async fn hostname_refusal_reports_resolved_addresses_not_the_hostname() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind probe port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let resolved = tokio::net::lookup_host(("localhost", port))
        .await
        .expect("localhost resolves on the test host");
    let attempts: Vec<String> = resolved
        .map(|address| format!("connect ECONNREFUSED {}:{}", address.ip(), port))
        .collect();
    assert!(!attempts.is_empty(), "localhost must resolve to loopback");
    let client = McpClient::new(
        McpServerSpec {
            id: "hostname-refused".into(),
            transport: McpTransportSpec::Sse {
                url: format!("http://localhost:{port}/sse"),
                headers: None,
            },
            call_timeout_ms: None,
        },
        McpClientOptions::default(),
    );
    let message = client
        .start()
        .await
        .expect_err("a closed port must refuse")
        .message()
        .to_owned();
    client.close().await;
    assert!(
        !message.contains("localhost"),
        "the URL hostname must not appear: {message}"
    );
    for attempt in &attempts {
        assert!(
            message.contains(attempt),
            "the message must list {attempt}: {message}"
        );
    }
}
