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
const SCENARIOS: usize = 17;
const STEPS: usize = 77;

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
fn fill(value: &JsValue, dir: &str) -> JsValue {
    let mock = mock_path();
    map_strings(value, &|text| {
        text.replace("<dir>", dir).replace("<mock>", &mock)
    })
}
fn scrub(value: &JsValue, dir: &str) -> JsValue {
    let temp = regex::Regex::new(r"(pilotdeck-mcp-[^/]*-)[A-Za-z0-9]{6}").unwrap();
    // Only strings that are valid UTF-16 survive the lossy round trip; the
    // corpus keeps lone surrogates out of scrubbed strings.
    let scrubbed = |text: &str| {
        temp.replace_all(&text.replace(dir, "<dir>"), "${1}XXXXXX")
            .into_owned()
    };
    match value {
        JsValue::String(text) if text.to_utf8().is_err() => value.clone(),
        JsValue::String(_) => map_strings(value, &scrubbed),
        JsValue::Array(items) => JsValue::Array(items.iter().map(|v| scrub(v, dir)).collect()),
        JsValue::Object(entries) => {
            let mut out = JsObject::new();
            for (key, item) in entries.own_entries() {
                out.insert(key.clone(), scrub(item, dir));
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
    assert_eq!(string(field(value, "transport"), "transport"), "stdio");
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
                    .call_tool(&name, field(&op, "args"), McpCallOptions { timeout_ms })
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
        let client = McpClient::new(
            spec(&fill(field(scenario, "spec"), &dir_text)),
            options(field(scenario, "options")),
        );
        let expected_steps = array(field(scenario, "steps"), "steps");
        for (index, expected) in expected_steps.iter().enumerate() {
            let op = field(expected, "step").clone();
            let mut fields = vec![("step", op.clone())];
            fields.extend(run_op(client.clone(), op, dir.clone()).await);
            fields.push(("status", JsValue::String(client.status().as_str().into())));
            let actual = scrub(&object(fields), &dir_text);
            assert_eq!(
                text(&actual),
                text(expected),
                "scenario {id} step {index} differs"
            );
            steps_seen += 1;
        }
        settle(&dir, field(scenario, "orphan").as_bool().unwrap_or(false)).await;
        let processes: Vec<JsValue> = read_logs(&dir)
            .into_iter()
            .map(|log| JsValue::Array(log.iter().map(|entry| scrub(entry, &dir_text)).collect()))
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
        let output = scrub(&result.to_value(), &dir.to_string_lossy());
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
