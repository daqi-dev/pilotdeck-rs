//! Replay of the real-TS MCP runtime oracle
//! (`tests/fixtures/mcp-runtime/generate.mts`): the native `McpRuntime` and
//! tool bridge against pr10a's scripted stdio server, the bridge projection
//! over a scripted port, `parsePluginMcpServers`, `patchProjectScopedMcpSpec`,
//! `SessionMcpRuntimeRegistry`, `ProjectMcpRuntimeProvider` (scripted runtime
//! factory) and the `mcpTool` / `mcpResources` builtins. Every step result is
//! compared as `JSON.stringify` text, in order.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    host::modules::{
        capability::ToolDefinition,
        tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    },
    runtime::{
        mcp::{
            bridge::{CreateToolDefinitionsOptions, create_mcp_tool_definitions_from_runtime},
            client::{McpCallOptions, McpClientError, McpErrorCode},
            protocol::{McpEnvironment, McpServerSpec, McpStatus, McpToolSpec, McpTransportSpec},
            registry::{
                BuiltinMcpPaths, ProjectMcpRuntimeLease, ProjectMcpRuntimeProvider,
                ProjectMcpRuntimeProviderOptions, SessionMcpRuntimeRegistration,
                SessionMcpRuntimeRegistry, parse_plugin_mcp_servers, patch_project_scoped_mcp_spec,
                server_spec_value,
            },
            runtime::{
                McpClientStatusEntry, McpFuture, McpRuntime, McpRuntimeFactory, McpRuntimeOptions,
                McpRuntimePort, McpRuntimeServerInfo, McpToolCallResult, SessionMcpRuntime,
            },
        },
        project::RuntimeError,
        tools::{
            ExecutableTool, ToolCancellation, ToolError, ToolOutput, ToolRunContext,
            builtin::{
                mcp_resources::{
                    McpResourceAdapter, McpResourceFuture, create_list_mcp_resources_tool,
                    create_read_mcp_resource_tool,
                },
                mcp_tool::{
                    CreateMcpToolOptions, McpAdapterFuture, McpToolAdapter, create_mcp_tool,
                },
            },
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use scratch::Scratch;
use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    rc::Rc,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/mcp-runtime/oracle.json");

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

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
fn s(value: &str) -> JsValue {
    JsValue::String(value.into())
}
fn map_strings(value: &JsValue, map: &dyn Fn(&str) -> String) -> JsValue {
    match value {
        JsValue::String(text) if text.to_utf8().is_ok() => s(&map(&text.to_utf8().unwrap())),
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
/// JSON with object keys sorted (for spec objects whose TS key order is an
/// artefact of object spread, see `patch`).
fn sorted(value: &JsValue) -> JsValue {
    match value {
        JsValue::Object(object) => {
            let mut pairs = object.own_entries();
            pairs.sort_by(|a, b| a.0.units().cmp(b.0.units()));
            let mut out = JsObject::new();
            for (key, item) in pairs {
                out.insert(key.clone(), sorted(item));
            }
            JsValue::Object(out)
        }
        JsValue::Array(items) => JsValue::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}
fn mock_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-client/mock_server.mjs")
        .to_string_lossy()
        .into_owned()
}

type Fields = Vec<(&'static str, JsValue)>;
type LocalFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// `Promise.all` over futures started in order: each is first polled in
/// index order, so every synchronous prefix runs before the next one, like
/// JS async functions called one after another.
async fn join_in_order<'a, T>(futures: Vec<LocalFuture<'a, T>>) -> Vec<T> {
    let mut slots: Vec<Option<LocalFuture<'a, T>>> = futures.into_iter().map(Some).collect();
    let mut results: Vec<Option<T>> = slots.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        for (index, slot) in slots.iter_mut().enumerate() {
            if let Some(future) = slot
                && let Poll::Ready(value) = future.as_mut().poll(cx)
            {
                results[index] = Some(value);
                *slot = None;
            }
        }
        if slots.iter().all(Option::is_none) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    results.into_iter().map(|r| r.expect("settled")).collect()
}

fn tool_error_value(error: &ToolError) -> JsValue {
    let mut fields = vec![
        ("name", s("PilotDeckToolRuntimeError")),
        ("code", s(&error.code)),
        ("message", JsValue::String(error.message.clone())),
    ];
    if let Some(details) = &error.details {
        fields.push(("details", JsValue::Object(details.clone())));
    }
    object(fields)
}
fn client_error_value(error: &McpClientError) -> JsValue {
    let code = match error.code() {
        None => JsValue::Null,
        Some(McpErrorCode::Client(code)) => s(code.as_str()),
        Some(McpErrorCode::Number(code)) => JsValue::Number(*code),
        Some(McpErrorCode::Errno(code) | McpErrorCode::Text(code)) => s(code),
    };
    object(vec![
        ("name", s(error.name())),
        ("code", code),
        ("message", s(error.message())),
    ])
}
fn runtime_error_value(error: &RuntimeError) -> JsValue {
    let mut fields = vec![
        (
            "name",
            s(if error.causes.is_empty() {
                "Error"
            } else {
                "AggregateError"
            }),
        ),
        ("code", JsValue::Null),
        ("message", s(&error.message)),
    ];
    if !error.causes.is_empty() {
        fields.push((
            "causes",
            JsValue::Array(error.causes.iter().map(runtime_error_value).collect()),
        ));
    }
    object(fields)
}
fn output_value(output: &ToolOutput) -> JsValue {
    let mut fields = vec![("content", JsValue::Array(output.content.clone()))];
    if let Some(data) = &output.data {
        fields.push(("data", data.clone()));
    }
    if let Some(metadata) = &output.metadata {
        fields.push(("metadata", JsValue::Object(metadata.clone())));
    }
    object(fields)
}
fn descriptor(tool: &dyn ToolDefinition, destructive: JsValue, open_world: JsValue) -> JsValue {
    let f = tool.fields();
    let get = |key: &str| f.get(key).cloned().unwrap_or(JsValue::Null);
    let empty = JsValue::Object(JsObject::new());
    object(vec![
        ("name", get("name")),
        ("description", get("description")),
        ("kind", get("kind")),
        ("inputSchema", get("inputSchema")),
        ("maxResultBytes", get("maxResultBytes")),
        ("aliases", get("aliases")),
        ("readOnly", tool.is_read_only(&empty).unwrap()),
        ("concurrencySafe", tool.is_concurrency_safe(&empty).unwrap()),
        ("destructive", destructive),
        ("openWorld", open_world),
    ])
}
fn run_context(cancel: &ToolCancellation) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: checkpoint.tool_context_state(),
        abort_signal: Some(Arc::new(cancel.clone())),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}
async fn execute(tool: &dyn ExecutableTool, input: &JsValue, abort: &JsValue) -> Fields {
    let cancel = ToolCancellation::default();
    if field(abort, "pre").as_bool() == Some(true) {
        cancel.cancel();
    }
    if let Some(after) = field(abort, "afterMs").as_number() {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(after as u64)).await;
            cancel.cancel();
        });
    }
    match tool.execute(input.clone(), run_context(&cancel)).await {
        Ok(output) => vec![("ok", output_value(&output))],
        Err(error) => vec![("error", tool_error_value(&error))],
    }
}

fn pairs(value: &JsValue) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    Some(
        object
            .own_entries()
            .into_iter()
            .map(|(k, v)| (k.to_utf8().unwrap(), string(v, "record value")))
            .collect(),
    )
}
fn spec_from(value: &JsValue) -> McpServerSpec {
    let id = string(field(value, "id"), "id");
    let call_timeout_ms = field(value, "callTimeoutMs").as_number();
    let opt = |key: &str| field(value, key).as_string().map(|v| v.to_utf8().unwrap());
    let transport = match string(field(value, "transport"), "transport").as_str() {
        "stdio" => McpTransportSpec::Stdio {
            command: opt("command").unwrap(),
            args: match field(value, "args") {
                JsValue::Array(items) => Some(items.iter().map(|a| string(a, "arg")).collect()),
                _ => None,
            },
            env: pairs(field(value, "env")),
            cwd: opt("cwd"),
            per_session: field(value, "perSession").as_bool() == Some(true),
        },
        "sse" => McpTransportSpec::Sse {
            url: opt("url").unwrap(),
            headers: pairs(field(value, "headers")),
        },
        _ => McpTransportSpec::StreamableHttp {
            url: opt("url").unwrap(),
            headers: pairs(field(value, "headers")),
        },
    };
    McpServerSpec {
        id,
        transport,
        call_timeout_ms,
    }
}

struct FixedEnvironment;
impl McpEnvironment for FixedEnvironment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        if name.equals_str("PDRS_MCP_SET") {
            Some("value".into())
        } else if name.equals_str("PDRS_MCP_NESTED") {
            Some("${env:PDRS_MCP_SET}".into())
        } else {
            None
        }
    }
    fn home(&self) -> JsString {
        "/pdrs-home".into()
    }
}

fn builtin_paths() -> BuiltinMcpPaths {
    BuiltinMcpPaths {
        node_executable: "<node>".into(),
        playwright_mcp_entrypoint: "<playwright>".into(),
        funasr_mcp_entrypoint: "<funasr>".into(),
        install_command: "<install>".into(),
    }
}

// ---------------------------------------------------------------------------
// A. live runtime
// ---------------------------------------------------------------------------

struct Live {
    runtime: Arc<McpRuntime>,
    tools: RefCell<Vec<Arc<pilotdeck_gateway::runtime::mcp::bridge::McpBridgeTool>>>,
    call_timeout_ms: Option<f64>,
}

fn status_list(statuses: &[McpClientStatusEntry]) -> JsValue {
    JsValue::Array(
        statuses
            .iter()
            .map(McpClientStatusEntry::to_value)
            .collect(),
    )
}

fn live_op<'a>(state: &'a Live, op: JsValue) -> LocalFuture<'a, Fields> {
    Box::pin(async move {
        if let Some(delay) = field(&op, "delayMs").as_number() {
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        }
        match string(field(&op, "op"), "op").as_str() {
            "start" => {
                let mut statuses = state.runtime.start().await;
                if field(&op, "sorted").as_bool() == Some(true) {
                    statuses.sort_by(|a, b| a.server_id.cmp(&b.server_id));
                }
                vec![("ok", status_list(&statuses))]
            }
            "statuses" => vec![("ok", status_list(&state.runtime.statuses()))],
            "serverInfo" => {
                let info = state.runtime.server_info(&string(field(&op, "id"), "id"));
                vec![(
                    "ok",
                    match info {
                        None => JsValue::Null,
                        Some(McpRuntimeServerInfo { transport, cwd }) => {
                            let mut fields = vec![("transport", s(transport))];
                            if let Some(cwd) = cwd {
                                fields.push(("cwd", s(&cwd)));
                            }
                            object(fields)
                        }
                    },
                )]
            }
            "definitions" => {
                let port: Arc<dyn McpRuntimePort> = state.runtime.clone();
                let tools = create_mcp_tool_definitions_from_runtime(
                    port,
                    CreateToolDefinitionsOptions {
                        call_timeout_ms: state.call_timeout_ms,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
                let list = tools
                    .iter()
                    .map(|t| {
                        descriptor(
                            t.as_ref(),
                            JsValue::Bool(t.is_destructive()),
                            JsValue::Bool(t.is_open_world()),
                        )
                    })
                    .collect();
                *state.tools.borrow_mut() = tools;
                vec![("ok", JsValue::Array(list))]
            }
            "execute" => {
                let name = string(field(&op, "tool"), "tool");
                let tool = state
                    .tools
                    .borrow()
                    .iter()
                    .find(|t| t.spec().wire_name.equals_str(&name))
                    .cloned()
                    .expect("defined tool");
                execute(tool.as_ref(), field(&op, "input"), field(&op, "abort")).await
            }
            "callTool" => {
                let result = state
                    .runtime
                    .call_tool(
                        &string(field(&op, "server"), "server"),
                        field(&op, "name").as_string().unwrap(),
                        field(&op, "input"),
                        McpCallOptions::default(),
                    )
                    .await;
                match result {
                    Ok(result) => vec![("ok", result.to_value())],
                    Err(error) => vec![("error", client_error_value(&error))],
                }
            }
            "stop" => {
                state.runtime.stop().await;
                vec![("ok", JsValue::Null)]
            }
            "parallel" => {
                let futures = array(field(&op, "ops"), "ops")
                    .iter()
                    .map(|inner| live_op(state, inner.clone()))
                    .collect();
                let results = join_in_order(futures).await;
                vec![(
                    "ok",
                    JsValue::Array(results.into_iter().map(object).collect()),
                )]
            }
            other => panic!("unknown op {other}"),
        }
    })
}

fn read_logs(dir: &Path, root: &str) -> Vec<JsValue> {
    let mut logs = Vec::new();
    for index in 0.. {
        let Ok(content) = std::fs::read_to_string(dir.join(format!("log-{index}.jsonl"))) else {
            break;
        };
        logs.push(JsValue::Array(
            content
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| map_strings(&parse_json(line).unwrap(), &|t| t.replace(root, "<dir>")))
                .collect(),
        ));
    }
    logs
}

async fn settle(dirs: &[PathBuf], root: &str) {
    for _ in 0..400 {
        let done = dirs.iter().all(|dir| {
            read_logs(dir, root).iter().all(|log| {
                array(log, "log").iter().any(|entry| {
                    field(entry, "event")
                        .as_string()
                        .is_some_and(|e| e.equals_str("exit"))
                })
            })
        });
        if done {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("mock processes did not settle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replays_ts_runtime_and_bridge_against_the_scripted_server() {
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|o| o.status.success()),
        "the MCP runtime replay needs `node` on PATH to run mock_server.mjs"
    );
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "live"), "live");
    assert_eq!(scenarios.len(), 7);
    let scratch = Scratch::new("mcp-runtime");
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let mut steps_seen = 0;
    for scenario in scenarios {
        let id = string(field(scenario, "id"), "id");
        let dir = root.join("live").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_text = dir.to_string_lossy().into_owned();
        let mock = mock_path();
        let fill = |value: &JsValue| {
            map_strings(value, &|t| {
                t.replace("<dir>", &dir_text).replace("<mock>", &mock)
            })
        };
        if let JsValue::Object(files) = field(scenario, "files") {
            use base64::Engine;
            for (path, data) in files.own_entries() {
                let target = dir.join(path.to_utf8().unwrap());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(string(data, "file"))
                    .unwrap();
                std::fs::write(target, bytes).unwrap();
            }
        }
        let mut specs = Vec::new();
        let mut server_dirs = Vec::new();
        for server in array(field(scenario, "servers"), "servers") {
            let server_id = string(field(server, "id"), "server id");
            let server_dir = dir.join(&server_id);
            std::fs::create_dir_all(&server_dir).unwrap();
            let script = object(vec![("procs", field(server, "proc").clone())]);
            std::fs::write(server_dir.join("script.json"), text(&script)).unwrap();
            let mut spec = object(vec![
                ("id", s(&server_id)),
                ("transport", s("stdio")),
                ("command", s("node")),
                ("args", JsValue::Array(vec![s("<mock>")])),
                (
                    "env",
                    object(vec![("MOCK_DIR", s(&format!("<dir>/{server_id}")))]),
                ),
            ]);
            if let (JsValue::Object(base), JsValue::Object(extra)) =
                (&mut spec, field(server, "spec"))
            {
                for (key, value) in extra.own_entries() {
                    base.insert(key.clone(), value.clone());
                }
            }
            specs.push(spec_from(&fill(&spec)));
            server_dirs.push(server_dir);
        }
        let options = field(scenario, "options");
        let state = Live {
            runtime: Arc::new(McpRuntime::new(
                specs,
                McpRuntimeOptions {
                    connect_concurrency: field(options, "connectConcurrency")
                        .as_number()
                        .map(|n| n as usize),
                    ..McpRuntimeOptions::default()
                },
            )),
            tools: RefCell::new(Vec::new()),
            call_timeout_ms: field(options, "callTimeoutMs").as_number(),
        };
        for (index, expected) in array(field(scenario, "steps"), "steps").iter().enumerate() {
            let op = field(expected, "step").clone();
            let mut fields = vec![("step", op.clone())];
            fields.extend(live_op(&state, op).await);
            let actual = map_strings(&object(fields), &|t| t.replace(&dir_text, "<dir>"));
            assert_eq!(text(&actual), text(expected), "live {id} step {index}");
            steps_seen += 1;
        }
        settle(&server_dirs, &dir_text).await;
        let expected = field(scenario, "processes");
        for (server_dir, server) in server_dirs
            .iter()
            .zip(array(field(scenario, "servers"), "servers"))
        {
            let server_id = string(field(server, "id"), "id");
            let logs = JsValue::Array(read_logs(server_dir, &dir_text));
            assert_eq!(
                text(&logs),
                text(field(expected, &server_id)),
                "live {id} server {server_id} processes"
            );
        }
    }
    assert_eq!(steps_seen, 47);

    // #361: close during a handshake. The TS oracle records the leak this
    // port fixes on purpose: the late connection still turns `ready` and its
    // process is not stopped.
    let ts = field(&oracle, "handshakeClose");
    assert_eq!(
        text(field(ts, "statuses")),
        r#"[{"serverId":"a","status":"ready"}]"#
    );
    assert_eq!(field(ts, "exitedBeforeSecondStop").as_bool(), Some(false));
    let dir = root.join("live").join("close-during-handshake");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("script.json"),
        r#"{"procs":[{"initialize":[{"sleep":200},{"send":{"jsonrpc":"2.0","id":"$id","result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"1"}}}}]}]}"#,
    )
    .unwrap();
    let runtime = McpRuntime::new(
        vec![McpServerSpec {
            id: "a".into(),
            transport: McpTransportSpec::Stdio {
                command: "node".into(),
                args: Some(vec![mock_path()]),
                env: Some(vec![(
                    "MOCK_DIR".into(),
                    dir.to_string_lossy().into_owned(),
                )]),
                cwd: None,
                per_session: false,
            },
            call_timeout_ms: None,
        }],
        McpRuntimeOptions::default(),
    );
    let started = runtime.start();
    tokio::pin!(started);
    let statuses = tokio::select! {
        statuses = &mut started => panic!("handshake finished early: {statuses:?}"),
        () = tokio::time::sleep(Duration::from_millis(50)) => {
            runtime.stop().await;
            assert_eq!(runtime.statuses()[0].status, McpStatus::Idle);
            started.await
        }
    };
    assert_eq!(statuses[0].status, McpStatus::Error);
    assert_eq!(
        statuses[0].error.as_deref(),
        Some("MCP handshake aborted: client closed during connect (server=a)")
    );
    assert_eq!(runtime.statuses()[0].status, McpStatus::Idle);
    // The late connection is closed: its process exits without another stop.
    let dir_text = dir.to_string_lossy().into_owned();
    settle(&[dir], &dir_text).await;
}

// ---------------------------------------------------------------------------
// B. bridge over a scripted port
// ---------------------------------------------------------------------------

struct FakePort {
    specs: Vec<McpToolSpec>,
    cwd: String,
    next: Mutex<JsValue>,
    calls: Mutex<Vec<JsValue>>,
}
impl SessionMcpRuntime for FakePort {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async { Ok(()) })
    }
}
impl McpRuntimePort for FakePort {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        Box::pin(async { Ok(self.specs.clone()) })
    }
    fn server_info(&self, server_id: &str) -> Option<McpRuntimeServerInfo> {
        match server_id {
            "s" => Some(McpRuntimeServerInfo {
                transport: "stdio",
                cwd: Some(self.cwd.clone()),
            }),
            "h" => Some(McpRuntimeServerInfo {
                transport: "streamable_http",
                cwd: None,
            }),
            _ => None,
        }
    }
    fn call_tool<'a>(
        &'a self,
        server_id: &'a str,
        tool_name: &'a JsString,
        input: &'a JsValue,
        options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(object(vec![
                ("serverId", s(server_id)),
                ("toolName", JsValue::String(tool_name.clone())),
                ("input", input.clone()),
                (
                    "timeoutMs",
                    options.timeout_ms.map_or(JsValue::Null, JsValue::Number),
                ),
                ("signal", JsValue::Bool(options.signal.is_some())),
            ]));
            let current = self.next.lock().unwrap().clone();
            if let Some(throws) = current.get("throws") {
                let code = match field(throws, "code") {
                    JsValue::String(code) => Some(McpErrorCode::Text(Box::leak(
                        code.to_utf8().unwrap().into_boxed_str(),
                    ))),
                    JsValue::Number(code) => Some(McpErrorCode::Number(*code)),
                    _ => None,
                };
                return Err(McpClientError::new(
                    "Error",
                    code,
                    string(field(throws, "message"), "message"),
                ));
            }
            let result = field(&current, "result");
            Ok(McpToolCallResult {
                content: field(result, "content").clone(),
                is_error: field(result, "isError").as_bool(),
            })
        })
    }
}

fn tool_spec(value: &JsValue) -> McpToolSpec {
    let js = |key: &str| field(value, key).as_string().cloned().unwrap();
    McpToolSpec {
        server_id: js("serverId"),
        tool_name: js("toolName"),
        wire_name: js("wireName"),
        description: js("description"),
        input_schema: field(value, "inputSchema").clone(),
        annotations: value.get("annotations").cloned(),
        meta: None,
    }
}

#[tokio::test]
async fn replays_ts_bridge_projection() {
    let oracle = baseline::oracle(ORACLE);
    let bridge = field(&oracle, "bridge");
    let scratch = Scratch::new("mcp-bridge");
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    std::fs::write(cwd.join("a.webp"), png).unwrap();
    std::fs::write(cwd.join("b.gif"), png).unwrap();
    let port = Arc::new(FakePort {
        specs: array(field(bridge, "specs"), "specs")
            .iter()
            .map(tool_spec)
            .collect(),
        cwd: cwd.to_string_lossy().into_owned(),
        next: Mutex::new(JsValue::Null),
        calls: Mutex::new(Vec::new()),
    });
    let tools = create_mcp_tool_definitions_from_runtime(
        port.clone(),
        CreateToolDefinitionsOptions {
            call_timeout_ms: Some(1234.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let descriptors = JsValue::Array(
        tools
            .iter()
            .map(|t| {
                descriptor(
                    t.as_ref(),
                    JsValue::Bool(t.is_destructive()),
                    JsValue::Bool(t.is_open_world()),
                )
            })
            .collect(),
    );
    assert_eq!(text(&descriptors), text(field(bridge, "descriptors")));
    let root_text = root.to_string_lossy().into_owned();
    let cases = array(field(bridge, "cases"), "cases");
    assert_eq!(cases.len(), 17);
    for (index, expected) in cases.iter().enumerate() {
        let case = field(expected, "case");
        *port.next.lock().unwrap() = case.clone();
        let name = string(field(case, "tool"), "tool");
        let tool = tools
            .iter()
            .find(|t| t.spec().wire_name.equals_str(&name))
            .unwrap();
        let before = port.calls.lock().unwrap().len();
        let mut fields = vec![("case", case.clone())];
        fields.extend(
            execute(
                tool.as_ref(),
                &object(vec![("k", JsValue::Number(1.0))]),
                &JsValue::Undefined,
            )
            .await,
        );
        fields.push((
            "calls",
            JsValue::Array(port.calls.lock().unwrap()[before..].to_vec()),
        ));
        let actual = map_strings(&object(fields), &|t| t.replace(&root_text, "<dir>"));
        assert_eq!(text(&actual), text(expected), "bridge case {index}");
    }
}

// ---------------------------------------------------------------------------
// C/D. parse + patch
// ---------------------------------------------------------------------------

#[test]
fn replays_ts_parse_and_patch() {
    let oracle = baseline::oracle(ORACLE);
    let cases = array(field(&oracle, "parse"), "parse");
    assert_eq!(cases.len(), 3);
    for (index, case) in cases.iter().enumerate() {
        let result = parse_plugin_mcp_servers(field(case, "input").as_object(), &FixedEnvironment);
        let actual = object(vec![
            (
                "servers",
                JsValue::Array(result.servers.iter().map(server_spec_value).collect()),
            ),
            (
                "diagnostics",
                JsValue::Array(
                    result
                        .diagnostics
                        .iter()
                        .map(|d| object(vec![("id", s(&d.id)), ("message", s(&d.message))]))
                        .collect(),
                ),
            ),
        ]);
        assert_eq!(
            text(&actual),
            text(field(case, "output")),
            "parse case {index}"
        );
    }
    let cases = array(field(&oracle, "patch"), "patch");
    assert_eq!(cases.len(), 6);
    for (index, case) in cases.iter().enumerate() {
        let patched = patch_project_scoped_mcp_spec(
            spec_from(field(case, "spec")),
            &string(field(case, "projectRoot"), "projectRoot"),
            &string(field(case, "pilotHome"), "pilotHome"),
            &builtin_paths(),
        );
        // Key order of the TS object comes from `{ ...spec, command, cwd, args }`
        // and carries no meaning; the values are compared exactly.
        assert_eq!(
            text(&sorted(&server_spec_value(&patched))),
            text(&sorted(field(case, "output"))),
            "patch case {index}"
        );
    }
}

// ---------------------------------------------------------------------------
// E. SessionMcpRuntimeRegistry
// ---------------------------------------------------------------------------

struct CountingRuntime {
    label: String,
    stops: Arc<Mutex<Vec<(String, u32)>>>,
    fail: Option<String>,
    delay_ms: Option<u64>,
}
impl SessionMcpRuntime for CountingRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            if let Some((_, count)) = self
                .stops
                .lock()
                .unwrap()
                .iter_mut()
                .find(|(l, _)| *l == self.label)
            {
                *count += 1;
            }
            if let Some(delay) = self.delay_ms {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            match &self.fail {
                Some(message) => Err(RuntimeError::new(message.clone())),
                None => Ok(()),
            }
        })
    }
}

struct RegistryRun {
    subject: SessionMcpRuntimeRegistry,
    runtimes: HashMap<String, Arc<CountingRuntime>>,
    registrations: RefCell<HashMap<String, Rc<SessionMcpRuntimeRegistration>>>,
}

fn registry_op<'a>(run: &'a RegistryRun, op: JsValue) -> LocalFuture<'a, Fields> {
    Box::pin(async move {
        let unit = |result: Result<(), RuntimeError>| match result {
            Ok(()) => vec![("ok", JsValue::Null)],
            Err(error) => vec![("error", runtime_error_value(&error))],
        };
        match string(field(&op, "op"), "op").as_str() {
            "register" => {
                let runtime = run.runtimes[&string(field(&op, "runtime"), "runtime")].clone();
                match run
                    .subject
                    .register(&string(field(&op, "key"), "key"), runtime)
                {
                    Ok(registration) => {
                        run.registrations
                            .borrow_mut()
                            .insert(string(field(&op, "as"), "as"), Rc::new(registration));
                        vec![("ok", JsValue::Null)]
                    }
                    Err(error) => vec![("error", runtime_error_value(&error))],
                }
            }
            "release" => {
                let registration =
                    run.registrations.borrow()[&string(field(&op, "reg"), "reg")].clone();
                unit(registration.dispose().await)
            }
            "size" => vec![("ok", JsValue::Number(run.subject.size() as f64))],
            "state" => vec![("ok", s(run.subject.state().as_str()))],
            "dispose" => unit(run.subject.dispose().await),
            "parallel" => {
                let futures = array(field(&op, "ops"), "ops")
                    .iter()
                    .map(|inner| registry_op(run, inner.clone()))
                    .collect();
                let results = join_in_order(futures).await;
                vec![(
                    "ok",
                    JsValue::Array(results.into_iter().map(object).collect()),
                )]
            }
            other => panic!("unknown op {other}"),
        }
    })
}

#[tokio::test]
async fn replays_ts_session_registry() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "registry"), "registry");
    assert_eq!(scenarios.len(), 4);
    let mut steps_seen = 0;
    for scenario in scenarios {
        let id = string(field(scenario, "id"), "id");
        let stops = Arc::new(Mutex::new(Vec::new()));
        let mut runtimes = HashMap::new();
        for (label, behaviour) in field(scenario, "runtimes")
            .as_object()
            .unwrap()
            .own_entries()
        {
            let label = label.to_utf8().unwrap();
            stops.lock().unwrap().push((label.clone(), 0));
            runtimes.insert(
                label.clone(),
                Arc::new(CountingRuntime {
                    label,
                    stops: stops.clone(),
                    fail: field(behaviour, "fail")
                        .as_string()
                        .map(|f| f.to_utf8().unwrap()),
                    delay_ms: field(behaviour, "delayMs").as_number().map(|d| d as u64),
                }),
            );
        }
        let run = RegistryRun {
            subject: SessionMcpRuntimeRegistry::new(),
            runtimes,
            registrations: RefCell::new(HashMap::new()),
        };
        for (index, expected) in array(field(scenario, "steps"), "steps").iter().enumerate() {
            let op = field(expected, "step").clone();
            let mut fields = vec![("step", op.clone())];
            fields.extend(registry_op(&run, op).await);
            let counts = stops
                .lock()
                .unwrap()
                .iter()
                .map(|(label, count)| (label.clone(), JsValue::Number(f64::from(*count))))
                .collect::<Vec<_>>();
            let mut counts_object = JsObject::new();
            for (label, count) in counts {
                counts_object.insert(label.as_str().into(), count);
            }
            fields.push(("stops", JsValue::Object(counts_object)));
            assert_eq!(
                text(&object(fields)),
                text(expected),
                "registry {id} step {index}"
            );
            steps_seen += 1;
        }
    }
    assert_eq!(steps_seen, 30);
}

// ---------------------------------------------------------------------------
// F. ProjectMcpRuntimeProvider
// ---------------------------------------------------------------------------

struct ScriptedRuntime {
    label: String,
    servers: Vec<McpServerSpec>,
    behaviour: JsValue,
    events: Arc<Mutex<Vec<JsValue>>>,
}
impl SessionMcpRuntime for ScriptedRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(object(vec![
                ("event", s("stop")),
                ("runtime", s(&self.label)),
            ]));
            match field(&self.behaviour, "stopError").as_string() {
                Some(message) => Err(RuntimeError::new(message.to_utf8().unwrap())),
                None => Ok(()),
            }
        })
    }
}
impl McpRuntimePort for ScriptedRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(object(vec![
                ("event", s("start")),
                ("runtime", s(&self.label)),
            ]));
            if let Some(delay) = field(&self.behaviour, "startDelayMs").as_number() {
                tokio::time::sleep(Duration::from_millis(delay as u64)).await;
            }
            if let Some(message) = field(&self.behaviour, "startError").as_string() {
                return Err(RuntimeError::new(message.to_utf8().unwrap()));
            }
            let statuses = field(&self.behaviour, "statuses");
            Ok(self
                .servers
                .iter()
                .map(|server| match field(statuses, &server.id).as_string() {
                    Some(error) => McpClientStatusEntry {
                        server_id: server.id.clone(),
                        status: McpStatus::Error,
                        error: Some(error.to_utf8().unwrap()),
                    },
                    None => McpClientStatusEntry {
                        server_id: server.id.clone(),
                        status: McpStatus::Ready,
                        error: None,
                    },
                })
                .collect())
        })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        Box::pin(async move {
            Ok(self
                .servers
                .iter()
                .map(|server| McpToolSpec {
                    server_id: server.id.as_str().into(),
                    tool_name: "t".into(),
                    wire_name: format!("mcp__{}__t", server.id).into(),
                    description: JsString::default(),
                    input_schema: parse_json(r#"{"type":"object"}"#).unwrap(),
                    annotations: None,
                    meta: None,
                })
                .collect())
        })
    }
    fn server_info(&self, _server_id: &str) -> Option<McpRuntimeServerInfo> {
        None
    }
    fn call_tool<'a>(
        &'a self,
        _server_id: &'a str,
        _tool_name: &'a JsString,
        _input: &'a JsValue,
        _options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        Box::pin(async {
            Ok(McpToolCallResult {
                content: JsValue::Array(Vec::new()),
                is_error: None,
            })
        })
    }
}

struct ProviderRun {
    subject: ProjectMcpRuntimeProvider,
    leases: RefCell<HashMap<String, Rc<ProjectMcpRuntimeLease>>>,
}

fn provider_op<'a>(run: &'a ProviderRun, op: JsValue) -> LocalFuture<'a, Fields> {
    Box::pin(async move {
        let unit = |result: Result<(), RuntimeError>| match result {
            Ok(()) => vec![("ok", JsValue::Null)],
            Err(error) => vec![("error", runtime_error_value(&error))],
        };
        let plugins = || {
            field(&op, "plugins")
                .as_object()
                .cloned()
                .unwrap_or_default()
        };
        match string(field(&op, "op"), "op").as_str() {
            "acquire" => match run.subject.acquire_shared_runtime(&plugins()).await {
                Ok(lease) => {
                    let names = lease
                        .tools()
                        .iter()
                        .map(|t| JsValue::String(t.spec().wire_name.clone()))
                        .collect();
                    run.leases
                        .borrow_mut()
                        .insert(string(field(&op, "as"), "as"), Rc::new(lease));
                    vec![("ok", JsValue::Array(names))]
                }
                Err(error) => vec![("error", runtime_error_value(&error))],
            },
            "release" => {
                let lease = run.leases.borrow()[&string(field(&op, "lease"), "lease")].clone();
                unit(lease.release().await)
            }
            "perSession" => {
                let specs = run.subject.per_session_server_specs(
                    &plugins(),
                    field(&op, "force").as_bool() == Some(true),
                );
                vec![(
                    "ok",
                    specs.map_or(JsValue::Null, |specs| {
                        JsValue::Array(specs.iter().map(server_spec_value).collect())
                    }),
                )]
            }
            "generations" => vec![(
                "ok",
                JsValue::Number(run.subject.shared_generation_count() as f64),
            )],
            "state" => vec![("ok", s(run.subject.state().as_str()))],
            "dispose" => {
                // The fixed TS oracle force-stops live leases. The requested
                // repair drains them instead: initiate dispose, return this
                // harness's leases, then compare the original stop outcomes.
                // Drain ordering itself is covered by deterministic tests below.
                let mut dispose = Box::pin(run.subject.dispose());
                let first = std::future::poll_fn(|cx| Poll::Ready(dispose.as_mut().poll(cx))).await;
                match first {
                    Poll::Ready(result) => unit(result),
                    Poll::Pending => {
                        let leases: Vec<_> = run.leases.borrow().values().cloned().collect();
                        for lease in leases {
                            // Any stop errors remain in dispose's aggregate.
                            let _ = lease.release().await;
                        }
                        unit(dispose.await)
                    }
                }
            }
            "parallel" => {
                let futures = array(field(&op, "ops"), "ops")
                    .iter()
                    .map(|inner| provider_op(run, inner.clone()))
                    .collect();
                let results = join_in_order(futures).await;
                vec![(
                    "ok",
                    JsValue::Array(results.into_iter().map(object).collect()),
                )]
            }
            other => panic!("unknown op {other}"),
        }
    })
}

#[tokio::test]
async fn replays_ts_project_provider() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "provider"), "provider");
    assert_eq!(scenarios.len(), 6);
    let scratch = Scratch::new("mcp-provider");
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let mut steps_seen = 0;
    for scenario in scenarios {
        let id = string(field(scenario, "id"), "id");
        let dir = root.join(&id);
        let project = dir.join("project");
        let home = dir.join("home");
        std::fs::create_dir_all(project.join(".pilotdeck")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let write = |path: PathBuf, value: &JsValue| match value {
            JsValue::Undefined => {}
            JsValue::String(raw) => std::fs::write(path, raw.to_utf8().unwrap()).unwrap(),
            other => std::fs::write(path, text(other)).unwrap(),
        };
        let config = field(scenario, "config");
        write(home.join("mcp.json"), field(config, "global"));
        write(
            project.join(".pilotdeck/mcp.json"),
            field(config, "project"),
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let behaviours: Vec<JsValue> = array(field(scenario, "runtimes"), "runtimes").to_vec();
        let created = Arc::new(Mutex::new(0usize));
        let factory: McpRuntimeFactory = {
            let events = events.clone();
            Arc::new(move |servers: Vec<McpServerSpec>| {
                let mut count = created.lock().unwrap();
                let label = format!("R{count}");
                let behaviour = behaviours
                    .get(*count)
                    .cloned()
                    .unwrap_or(JsValue::Undefined);
                *count += 1;
                events.lock().unwrap().push(object(vec![
                    ("event", s("create")),
                    ("runtime", s(&label)),
                    (
                        "servers",
                        JsValue::Array(servers.iter().map(server_spec_value).collect()),
                    ),
                ]));
                Arc::new(ScriptedRuntime {
                    label,
                    servers,
                    behaviour,
                    events: events.clone(),
                }) as Arc<dyn McpRuntimePort>
            })
        };
        let sink = diagnostics.clone();
        let run = ProviderRun {
            subject: ProjectMcpRuntimeProvider::new(ProjectMcpRuntimeProviderOptions {
                project_root: project.to_string_lossy().into_owned(),
                pilot_home: home.to_string_lossy().into_owned(),
                create_runtime: Some(factory),
                on_diagnostic: Some(Arc::new(move |message, error| {
                    let mut fields = vec![("message", s(&message))];
                    if let Some(error) = error {
                        fields.push(("error", runtime_error_value(&error)));
                    }
                    sink.lock().unwrap().push(object(fields));
                })),
                environment: Arc::new(FixedEnvironment),
                builtin_paths: builtin_paths(),
            }),
            leases: RefCell::new(HashMap::new()),
        };
        let dir_text = dir.to_string_lossy().into_owned();
        for (index, expected) in array(field(scenario, "steps"), "steps").iter().enumerate() {
            events.lock().unwrap().clear();
            diagnostics.lock().unwrap().clear();
            let op = field(expected, "step").clone();
            let mut fields = vec![("step", op.clone())];
            fields.extend(provider_op(&run, op).await);
            let recorded = std::mem::take(&mut *events.lock().unwrap());
            let (stops, mut others): (Vec<JsValue>, Vec<JsValue>) =
                recorded.into_iter().partition(|e| {
                    field(e, "event")
                        .as_string()
                        .is_some_and(|k| k.equals_str("stop"))
                });
            let mut stops = stops;
            stops.sort_by_key(|e| string(field(e, "runtime"), "runtime"));
            others.extend(stops);
            fields.push(("events", JsValue::Array(others)));
            fields.push((
                "diagnostics",
                JsValue::Array(std::mem::take(&mut *diagnostics.lock().unwrap())),
            ));
            let actual = map_strings(&object(fields), &|t| t.replace(&dir_text, "<dir>"));
            assert_eq!(text(&actual), text(expected), "provider {id} step {index}");
            steps_seen += 1;
        }
    }
    assert_eq!(steps_seen, 45);
}

// P1-1 regression: the startup task owns a strong handle to the tools slot,
// so an immediately-ready runtime cannot finish before the entry exists and
// leave the generation with an empty tool list. The old
// `Arc::new_cyclic`/`Weak` write could not upgrade while the closure was
// still building the entry. Run on a multi-thread runtime: the previous
// `#[tokio::test]` (current_thread) hid exactly this race.
struct ImmediateRuntime {
    servers: Vec<McpServerSpec>,
}
impl SessionMcpRuntime for ImmediateRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async { Ok(()) })
    }
}
impl McpRuntimePort for ImmediateRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async move {
            Ok(self
                .servers
                .iter()
                .map(|server| McpClientStatusEntry {
                    server_id: server.id.clone(),
                    status: McpStatus::Ready,
                    error: None,
                })
                .collect())
        })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        Box::pin(async move {
            Ok(self
                .servers
                .iter()
                .map(|server| McpToolSpec {
                    server_id: server.id.as_str().into(),
                    tool_name: "t".into(),
                    wire_name: format!("mcp__{}__t", server.id).into(),
                    description: JsString::default(),
                    input_schema: parse_json(r#"{"type":"object"}"#).unwrap(),
                    annotations: None,
                    meta: None,
                })
                .collect())
        })
    }
    fn server_info(&self, _server_id: &str) -> Option<McpRuntimeServerInfo> {
        None
    }
    fn call_tool<'a>(
        &'a self,
        _server_id: &'a str,
        _tool_name: &'a JsString,
        _input: &'a JsValue,
        _options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        Box::pin(async {
            Ok(McpToolCallResult {
                content: JsValue::Array(Vec::new()),
                is_error: None,
            })
        })
    }
}

fn provider_plugins(name: &str) -> JsObject {
    let mut plugins = JsObject::new();
    let mut spec = JsObject::new();
    spec.insert("command".into(), s("node"));
    spec.insert("args".into(), JsValue::Array(vec![s(name)]));
    plugins.insert("a".into(), JsValue::Object(spec));
    plugins
}

fn race_provider(
    scratch: &Scratch,
    factory: McpRuntimeFactory,
) -> (ProjectMcpRuntimeProvider, PathBuf) {
    let dir = scratch.path().join("race");
    let project = dir.join("project");
    let home = dir.join("home");
    std::fs::create_dir_all(project.join(".pilotdeck")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let provider = ProjectMcpRuntimeProvider::new(ProjectMcpRuntimeProviderOptions {
        project_root: project.to_string_lossy().into_owned(),
        pilot_home: home.to_string_lossy().into_owned(),
        create_runtime: Some(factory),
        on_diagnostic: None,
        environment: Arc::new(FixedEnvironment),
        builtin_paths: builtin_paths(),
    });
    (provider, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immediate_start_tools_are_visible() {
    let scratch = Scratch::new("mcp-provider-race");
    let factory: McpRuntimeFactory =
        Arc::new(|servers| Arc::new(ImmediateRuntime { servers }) as Arc<dyn McpRuntimePort>);
    let (provider, _dir) = race_provider(&scratch, factory);
    for iteration in 0..20000 {
        let name = format!("s{iteration}");
        let plugins = provider_plugins(&name);
        let lease = provider
            .acquire_shared_runtime(&plugins)
            .await
            .expect("acquire");
        assert_eq!(lease.tools().len(), 1, "iteration {iteration}");
        lease.release().await.expect("release");
        assert_eq!(
            provider.shared_generation_count(),
            0,
            "iteration {iteration}"
        );
    }
}

// DeepSeek P2-2: dropping an `acquire` future while it waits for `ready`
// must not leave a lease behind. `startEntry` gated on a semaphore keeps the
// startup pending; after the waiter is aborted the next acquire/release pair
// must stop the generation (old code left leases=1 and never stopped it).
struct GatedRuntime {
    servers: Vec<McpServerSpec>,
    started: Arc<tokio::sync::Semaphore>,
    gate: Arc<tokio::sync::Semaphore>,
    finished: Arc<tokio::sync::Semaphore>,
    stops: Arc<std::sync::atomic::AtomicUsize>,
}
impl SessionMcpRuntime for GatedRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}
impl McpRuntimePort for GatedRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async move {
            self.started.add_permits(1);
            let _ = self.gate.acquire().await;
            self.finished.add_permits(1);
            Ok(self
                .servers
                .iter()
                .map(|server| McpClientStatusEntry {
                    server_id: server.id.clone(),
                    status: McpStatus::Ready,
                    error: None,
                })
                .collect())
        })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        Box::pin(async move {
            Ok(self
                .servers
                .iter()
                .map(|server| McpToolSpec {
                    server_id: server.id.as_str().into(),
                    tool_name: "t".into(),
                    wire_name: format!("mcp__{}__t", server.id).into(),
                    description: JsString::default(),
                    input_schema: parse_json(r#"{"type":"object"}"#).unwrap(),
                    annotations: None,
                    meta: None,
                })
                .collect())
        })
    }
    fn server_info(&self, _server_id: &str) -> Option<McpRuntimeServerInfo> {
        None
    }
    fn call_tool<'a>(
        &'a self,
        _server_id: &'a str,
        _tool_name: &'a JsString,
        _input: &'a JsValue,
        _options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        Box::pin(async {
            Ok(McpToolCallResult {
                content: JsValue::Array(Vec::new()),
                is_error: None,
            })
        })
    }
}

// Independent gates establish each interleaving; no sleep determines order.
struct LifecycleGate {
    start: tokio::sync::Semaphore,
    stop_entered: tokio::sync::Semaphore,
    stop: tokio::sync::Semaphore,
    stops: std::sync::atomic::AtomicUsize,
}
impl LifecycleGate {
    fn new() -> Self {
        Self {
            start: tokio::sync::Semaphore::new(0),
            stop_entered: tokio::sync::Semaphore::new(0),
            stop: tokio::sync::Semaphore::new(0),
            stops: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}
struct LifecycleRuntime {
    inner: ImmediateRuntime,
    gate: Arc<LifecycleGate>,
}
impl SessionMcpRuntime for LifecycleRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            self.gate
                .stops
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.gate.stop_entered.add_permits(1);
            self.gate.stop.acquire().await.unwrap().forget();
            Ok(())
        })
    }
}
impl McpRuntimePort for LifecycleRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async move {
            self.gate.start.acquire().await.unwrap().forget();
            self.inner.start().await
        })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        self.inner.list_all_tools()
    }
    fn server_info(&self, id: &str) -> Option<McpRuntimeServerInfo> {
        self.inner.server_info(id)
    }
    fn call_tool<'a>(
        &'a self,
        id: &'a str,
        name: &'a JsString,
        input: &'a JsValue,
        options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        self.inner.call_tool(id, name, input, options)
    }
}
type LifecycleGates = Arc<Mutex<Vec<Arc<LifecycleGate>>>>;
fn lifecycle_provider(scratch: &Scratch) -> (ProjectMcpRuntimeProvider, LifecycleGates) {
    let gates = Arc::new(Mutex::new(Vec::new()));
    let captured = gates.clone();
    let factory: McpRuntimeFactory = Arc::new(move |servers| {
        let gate = Arc::new(LifecycleGate::new());
        captured.lock().unwrap().push(gate.clone());
        Arc::new(LifecycleRuntime {
            inner: ImmediateRuntime { servers },
            gate,
        }) as Arc<dyn McpRuntimePort>
    });
    (race_provider(scratch, factory).0, gates)
}
async fn poll_pending<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "future unexpectedly settled"
        );
        Poll::Ready(())
    })
    .await
}
async fn check_stopping_generation(queued_before_ready: bool) {
    let scratch = Scratch::new("mcp-provider-stop-race");
    let (provider, gates) = lifecycle_provider(&scratch);
    let plugins = provider_plugins("same");
    let mut first = Box::pin(provider.acquire_shared_runtime(&plugins));
    poll_pending(first.as_mut()).await;
    let mut next = Box::pin(provider.acquire_shared_runtime(&plugins));
    if queued_before_ready {
        poll_pending(next.as_mut()).await;
    }
    let old = gates.lock().unwrap()[0].clone();
    old.start.add_permits(1);
    let first = first.await.unwrap();
    let mut releasing = Box::pin(first.release());
    poll_pending(releasing.as_mut()).await;
    old.stop_entered.acquire().await.unwrap().forget();
    poll_pending(next.as_mut()).await;
    assert_eq!(
        gates.lock().unwrap().len(),
        2,
        "stopping generation cannot be leased"
    );
    let new = gates.lock().unwrap()[1].clone();
    new.start.add_permits(1);
    let next = next.await.unwrap();
    assert_eq!(next.tools().len(), 1);
    old.stop.add_permits(1);
    releasing.await.unwrap();
    assert_eq!(provider.shared_generation_count(), 1);
    new.stop.add_permits(1);
    next.release().await.unwrap();
    assert_eq!(provider.shared_generation_count(), 0);
    assert_eq!(old.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(new.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    provider.dispose().await.unwrap();
}
#[tokio::test]
async fn acquire_during_stop_uses_new_generation() {
    tokio::time::timeout(Duration::from_secs(10), check_stopping_generation(false))
        .await
        .unwrap();
}
#[tokio::test]
async fn ready_waiter_cannot_retain_stopping_generation() {
    tokio::time::timeout(Duration::from_secs(10), check_stopping_generation(true))
        .await
        .unwrap();
}

// A paused clock makes the pending check deterministic: once advancing to the
// timeout, Tokio has polled all runnable tasks, including the stop task. An
// incorrect early stop would have signalled stop_entered by then.
#[tokio::test(start_paused = true)]
async fn dispose_drains_all_existing_leases_before_stopping() {
    let scratch = Scratch::new("mcp-provider-dispose-drain");
    let (provider, gates) = lifecycle_provider(&scratch);
    let plugins = provider_plugins("same");
    let mut first = Box::pin(provider.acquire_shared_runtime(&plugins));
    poll_pending(first.as_mut()).await;
    let gate = gates.lock().unwrap()[0].clone();
    gate.start.add_permits(1);
    let first = first.await.unwrap();
    let second = provider.acquire_shared_runtime(&plugins).await.unwrap();
    let dispose = tokio::spawn({
        let provider = provider.clone();
        async move { provider.dispose().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(1), gate.stop_entered.acquire())
            .await
            .is_err()
    );
    assert_eq!(provider.state().as_str(), "draining");
    assert!(!dispose.is_finished());
    assert!(provider.acquire_shared_runtime(&plugins).await.is_err());
    first.release().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), gate.stop_entered.acquire())
            .await
            .is_err()
    );
    assert_eq!(gate.stops.load(std::sync::atomic::Ordering::SeqCst), 0);
    let mut release = Box::pin(second.release());
    poll_pending(release.as_mut()).await;
    gate.stop_entered.acquire().await.unwrap().forget();
    assert!(
        !dispose.is_finished(),
        "dispose must wait for the actual stop"
    );
    gate.stop.add_permits(1);
    release.await.unwrap();
    dispose.await.unwrap().unwrap();
    assert_eq!(provider.state().as_str(), "disposed");
    assert_eq!(provider.shared_generation_count(), 0);
    assert_eq!(gate.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

async fn check_dropped_lease(abort_holder: bool) {
    let scratch = Scratch::new("mcp-provider-dropped-lease");
    let (provider, gates) = lifecycle_provider(&scratch);
    let plugins = provider_plugins("same");
    let mut first = Box::pin(provider.acquire_shared_runtime(&plugins));
    poll_pending(first.as_mut()).await;
    let gate = gates.lock().unwrap()[0].clone();
    gate.start.add_permits(1);
    let first = first.await.unwrap();
    let second = provider.acquire_shared_runtime(&plugins).await.unwrap();
    let mut dispose = Box::pin(provider.dispose());
    poll_pending(dispose.as_mut()).await;
    first.release().await.unwrap();
    drop(first); // Drop after explicit release must not decrement twice.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), gate.stop_entered.acquire())
            .await
            .is_err()
    );
    if abort_holder {
        let (entered, held) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _lease = second;
            entered.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        held.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    } else {
        std::thread::spawn(move || drop(second))
            .join()
            .expect("Drop needs no current Tokio context");
    }
    tokio::time::timeout(Duration::from_secs(10), gate.stop_entered.acquire())
        .await
        .expect("last lease Drop must let disposal drain")
        .unwrap()
        .forget();
    gate.stop.add_permits(1);
    dispose.await.unwrap();
    assert_eq!(provider.state().as_str(), "disposed");
    assert_eq!(provider.shared_generation_count(), 0);
    assert_eq!(gate.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn lease_dropped_on_plain_thread_unblocks_dispose_once() {
    check_dropped_lease(false).await;
}

#[tokio::test(start_paused = true)]
async fn cancelled_lease_holder_unblocks_dispose_once() {
    check_dropped_lease(true).await;
}

#[tokio::test(start_paused = true)]
async fn last_lease_drop_on_plain_thread_starts_stop() {
    let scratch = Scratch::new("mcp-provider-drop-start-stop");
    let (provider, gates) = lifecycle_provider(&scratch);
    let plugins = provider_plugins("same");
    let mut acquire = Box::pin(provider.acquire_shared_runtime(&plugins));
    poll_pending(acquire.as_mut()).await;
    let gate = gates.lock().unwrap()[0].clone();
    gate.start.add_permits(1);
    let lease = acquire.await.unwrap();
    // No dispose/stop has been initiated: this Drop must create the stop
    // task from a plain thread, using the entry's originating executor.
    std::thread::spawn(move || drop(lease))
        .join()
        .expect("Drop starts stop outside Tokio");
    tokio::time::timeout(Duration::from_secs(10), gate.stop_entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let mut dispose = Box::pin(provider.dispose());
    poll_pending(dispose.as_mut()).await;
    gate.stop.add_permits(1);
    dispose.await.unwrap();
    assert_eq!(provider.shared_generation_count(), 0);
    assert_eq!(gate.stops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_acquire_does_not_leak_a_lease() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let scratch = Scratch::new("mcp-provider-cancel");
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let finished = Arc::new(tokio::sync::Semaphore::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let factory: McpRuntimeFactory = {
        let started = started.clone();
        let gate = gate.clone();
        let finished = finished.clone();
        let stops = stops.clone();
        Arc::new(move |servers| {
            Arc::new(GatedRuntime {
                servers,
                started: started.clone(),
                gate: gate.clone(),
                finished: finished.clone(),
                stops: stops.clone(),
            }) as Arc<dyn McpRuntimePort>
        })
    };
    let (provider, _dir) = race_provider(&scratch, factory);
    let plugins = provider_plugins("gated");
    let acquire = tokio::spawn({
        let provider = provider.clone();
        let plugins = plugins.clone();
        async move { provider.acquire_shared_runtime(&plugins).await }
    });
    started.acquire().await.unwrap().forget();
    // Let the acquire future reach `settled(ready).await` (and, under the
    // old code, take its lease) before dropping it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    acquire.abort();
    let _ = acquire.await;
    gate.add_permits(1);
    finished.acquire().await.unwrap().forget();
    let lease = provider
        .acquire_shared_runtime(&plugins)
        .await
        .expect("reacquire");
    assert_eq!(lease.tools().len(), 1);
    lease.release().await.expect("release");
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "a cancelled acquire must not pin the generation"
    );
}

// ---------------------------------------------------------------------------
// G. builtins
// ---------------------------------------------------------------------------

struct Adapters {
    calls: Mutex<Vec<JsValue>>,
}
impl McpToolAdapter for Adapters {
    fn call_tool<'a>(
        &'a self,
        server_id: &'a str,
        tool_name: &'a str,
        input: JsValue,
    ) -> McpAdapterFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(object(vec![
                ("op", s("callTool")),
                ("serverId", s(server_id)),
                ("toolName", s(tool_name)),
                ("input", input.clone()),
            ]));
            if field(&input, "fail").as_bool() == Some(true) {
                return Err(ToolError::failed("adapter failed"));
            }
            Ok(object(vec![("echoed", input)]))
        })
    }
}
impl McpResourceAdapter for Adapters {
    fn list_resources(&self, server_id: JsValue) -> McpResourceFuture<'_> {
        Box::pin(async move {
            let server = if server_id.is_undefined() {
                JsValue::Null
            } else {
                server_id
            };
            self.calls.lock().unwrap().push(object(vec![
                ("op", s("listResources")),
                ("serverId", server.clone()),
            ]));
            Ok(JsValue::Array(vec![object(vec![
                ("uri", s("file:///a")),
                ("serverId", server),
            ])]))
        })
    }
    fn read_resource(&self, server_id: JsValue, uri: JsValue) -> McpResourceFuture<'_> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(object(vec![
                ("op", s("readResource")),
                ("serverId", server_id),
                ("uri", uri.clone()),
            ]));
            if uri.as_string().is_some_and(|u| u.equals_str("missing")) {
                return Err(ToolError::failed("no such resource"));
            }
            Ok(object(vec![(
                "contents",
                JsValue::Array(vec![object(vec![("uri", uri), ("text", s("body"))])]),
            )]))
        })
    }
}

/// `normalizeToolError`: a plain `Error` thrown by an adapter is reported as
/// `tool_execution_failed` by the TS tool runtime.
fn normalized(error: &JsValue) -> JsValue {
    let name = field(error, "name").as_string().cloned();
    let code = if name
        .as_ref()
        .is_some_and(|n| n.equals_str("PilotDeckToolRuntimeError"))
    {
        field(error, "code").clone()
    } else {
        s("tool_execution_failed")
    };
    object(vec![
        ("code", code),
        ("message", field(error, "message").clone()),
    ])
}

#[tokio::test]
async fn replays_ts_mcp_builtins() {
    let oracle = baseline::oracle(ORACLE);
    let builtins = field(&oracle, "builtins");
    for (index, case) in array(field(builtins, "names"), "names").iter().enumerate() {
        let created = create_mcp_tool(CreateMcpToolOptions {
            server_id: string(field(case, "serverId"), "serverId"),
            tool_name: string(field(case, "toolName"), "toolName"),
            ..CreateMcpToolOptions::default()
        });
        match created {
            Ok(tool) => assert_eq!(
                text(&descriptor(
                    &tool,
                    JsValue::Null,
                    JsValue::Bool(tool.is_open_world())
                )),
                text(field(case, "ok")),
                "name case {index}"
            ),
            Err(error) => assert_eq!(
                text(&tool_error_value(&error)),
                text(&{
                    let e = field(case, "error");
                    object(vec![
                        ("name", field(e, "name").clone()),
                        ("code", field(e, "code").clone()),
                        ("message", field(e, "message").clone()),
                    ])
                }),
                "name case {index}"
            ),
        }
    }
    let adapters = Arc::new(Adapters {
        calls: Mutex::new(Vec::new()),
    });
    let custom = create_mcp_tool(CreateMcpToolOptions {
        server_id: "s".into(),
        tool_name: "t".into(),
        description: Some("custom".into()),
        input_schema: Some(parse_json(r#"{"type":"object","required":["x"]}"#).unwrap()),
        adapter: Some(adapters.clone()),
    })
    .unwrap();
    assert_eq!(
        text(&descriptor(&custom, JsValue::Null, JsValue::Bool(true))),
        text(field(builtins, "custom"))
    );
    let list = create_list_mcp_resources_tool(None);
    let read = create_read_mcp_resource_tool(None);
    assert_eq!(
        text(&descriptor(
            &list,
            JsValue::Null,
            JsValue::Bool(list.is_open_world())
        )),
        text(field(builtins, "list"))
    );
    assert_eq!(
        text(&descriptor(
            &read,
            JsValue::Null,
            JsValue::Bool(read.is_open_world())
        )),
        text(field(builtins, "read"))
    );
    let runs = array(field(builtins, "runs"), "runs");
    assert_eq!(runs.len(), 10);
    for run in runs {
        let label = string(field(run, "label"), "label");
        let tool: Box<dyn ExecutableTool> = match label.as_str() {
            "mcp-tool" | "mcp-tool-fail" => Box::new(
                create_mcp_tool(CreateMcpToolOptions {
                    server_id: "s".into(),
                    tool_name: "t".into(),
                    adapter: Some(adapters.clone()),
                    ..CreateMcpToolOptions::default()
                })
                .unwrap(),
            ),
            "mcp-tool-no-adapter" => Box::new(
                create_mcp_tool(CreateMcpToolOptions {
                    server_id: "s".into(),
                    tool_name: "t".into(),
                    ..CreateMcpToolOptions::default()
                })
                .unwrap(),
            ),
            "list" | "list-server" | "list-number" => {
                Box::new(create_list_mcp_resources_tool(Some(adapters.clone())))
            }
            "list-no-adapter" => Box::new(create_list_mcp_resources_tool(None)),
            "read" | "read-missing" => {
                Box::new(create_read_mcp_resource_tool(Some(adapters.clone())))
            }
            _ => Box::new(create_read_mcp_resource_tool(None)),
        };
        let before = adapters.calls.lock().unwrap().len();
        let result = execute(tool.as_ref(), field(run, "input"), &JsValue::Undefined).await;
        let calls = JsValue::Array(adapters.calls.lock().unwrap()[before..].to_vec());
        assert_eq!(
            text(&calls),
            text(field(run, "calls")),
            "builtin {label} calls"
        );
        match &result[0] {
            ("ok", value) => assert_eq!(text(value), text(field(run, "ok")), "builtin {label}"),
            (_, error) => assert_eq!(
                text(&normalized(error)),
                text(&normalized(field(run, "error"))),
                "builtin {label}"
            ),
        }
    }
}

/// Two generator runs into fresh directories reproduce the committed oracle.
#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn generator_reproduces_the_committed_oracle_twice() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-runtime/generate.mts");
    for run in 0..2 {
        let scratch = Scratch::new("mcp-runtime-generate");
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
/// The outputSchema Ajv oracle is also generated from the real SDK; two
/// fresh runs must reproduce the committed bytes.
#[test]
#[ignore = "requires fixed TS baseline and Node; run with PDRS_BASELINE"]
fn output_schema_generator_reproduces_the_committed_oracle_twice() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/output-schema/generate.mjs");
    let committed = include_str!("fixtures/output-schema/ajv-oracle.json");
    for run in 0..2 {
        let scratch = Scratch::new("output-schema-generate");
        let status = std::process::Command::new("node")
            .env("PDRS_BASELINE", &baseline)
            .env("LANG", "en_US.UTF-8")
            .env_remove("LC_ALL")
            .arg(&generator)
            .arg(scratch.path())
            .status()
            .expect("run node");
        assert!(status.success(), "generator run {run} failed");
        let generated = std::fs::read_to_string(scratch.path().join("ajv-oracle.json")).unwrap();
        assert!(
            generated == committed,
            "generator run {run} differs from the committed oracle"
        );
    }
}

// #394: Tokio accepts spawn on a shut-down Handle and cancels the task.
// A dropped lease may arrive on a plain thread after its original executor dies.
#[test]
fn lease_dropped_after_origin_runtime_shutdown_is_safe() {
    let scratch = Scratch::new("mcp-provider-shutdown-drop");
    let factory: McpRuntimeFactory = Arc::new(|servers| Arc::new(ImmediateRuntime { servers }));
    let (provider, _dir) = race_provider(&scratch, factory);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(provider.acquire_shared_runtime(&provider_plugins("shutdown")))
        .unwrap();
    assert_eq!(lease.tools().len(), 1);
    drop(runtime);
    std::thread::spawn(move || drop(lease))
        .join()
        .expect("Drop after runtime shutdown must not panic");
    // A fresh executor can await the cancelled task: it must settle as an error,
    // never report that an unexecuted stop succeeded, and never hang.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(1), provider.dispose())
            .await
            .unwrap()
            .unwrap_err()
    });
    assert!(
        error
            .message
            .contains("MCP lifecycle task ended unexpectedly"),
        "{error:?}"
    );
}

// Harmless scratch files verify the previously reported parent/symlink escape.
// #508: human-approved security divergence preserves text/data/metadata.
#[tokio::test]
async fn bridge_file_projection_rejects_outside_cwd_preserving_ts_text() {
    let oracle = baseline::oracle(include_str!("fixtures/mcp-worker-boundary/oracle.json"));
    let scratch = Scratch::new("mcp-file-boundary");
    let cwd = scratch.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(scratch.path().join("outside.png"), b"fixture image").unwrap();
    std::os::unix::fs::symlink("../outside.png", cwd.join("link.png")).unwrap();
    for row in array(field(&oracle, "cases"), "cases") {
        if field(row, "id") == &s("fifo") {
            continue;
        }
        let port = Arc::new(FakePort {
            specs: Vec::new(),
            cwd: cwd.to_string_lossy().into_owned(),
            next: Mutex::new(object(vec![(
                "result",
                object(vec![("content", field(row, "raw").clone())]),
            )])),
            calls: Mutex::new(Vec::new()),
        });
        let spec = McpToolSpec {
            server_id: "s".into(),
            tool_name: "t".into(),
            wire_name: "mcp__s__t".into(),
            description: "".into(),
            input_schema: object(vec![("type", s("object"))]),
            annotations: None,
            meta: None,
        };
        let tool = pilotdeck_gateway::runtime::mcp::bridge::McpBridgeTool::new(
            spec,
            port,
            CreateToolDefinitionsOptions::default(),
        );
        let actual = object(execute(&tool, &object(vec![]), &JsValue::Undefined).await);
        assert_eq!(
            text(field(&actual, "ok")),
            text(&object(vec![
                ("content", field(row, "raw").clone()),
                ("data", field(row, "raw").clone()),
                (
                    "metadata",
                    field(field(row, "observed"), "metadata").clone()
                ),
            ])),
            "{}",
            text(field(row, "id"))
        );
    }
}

// #508: a FIFO is skipped without opening it; no writer is necessary.
#[tokio::test]
async fn bridge_fifo_is_rejected_without_waiting_for_a_writer() {
    let oracle = baseline::oracle(include_str!("fixtures/mcp-worker-boundary/oracle.json"));
    let row = array(field(&oracle, "cases"), "cases")
        .iter()
        .find(|r| field(r, "id") == &s("fifo"))
        .unwrap();
    let scratch = Scratch::new("mcp-fifo-worker");
    let fifo = scratch.path().join("fifo.png");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let port = Arc::new(FakePort {
        specs: Vec::new(),
        cwd: scratch.path().to_string_lossy().into_owned(),
        next: Mutex::new(object(vec![(
            "result",
            object(vec![("content", field(row, "raw").clone())]),
        )])),
        calls: Mutex::new(Vec::new()),
    });
    let spec = McpToolSpec {
        server_id: "s".into(),
        tool_name: "t".into(),
        wire_name: "mcp__s__t".into(),
        description: "".into(),
        input_schema: object(vec![("type", s("object"))]),
        annotations: None,
        meta: None,
    };
    let tool = pilotdeck_gateway::runtime::mcp::bridge::McpBridgeTool::new(
        spec,
        port,
        CreateToolDefinitionsOptions::default(),
    );
    let input = object(vec![]);
    let actual = object(
        tokio::time::timeout(Duration::from_secs(2), execute(&tool, &input, &UNDEFINED))
            .await
            .expect("FIFO must be skipped without a writer"),
    );
    assert_eq!(
        text(field(&actual, "ok")),
        text(&object(vec![
            ("content", field(row, "raw").clone()),
            ("data", field(row, "raw").clone()),
            (
                "metadata",
                field(field(row, "observed"), "metadata").clone()
            ),
        ])),
    );
}

#[tokio::test]
async fn bridge_safe_images_match_real_ts_bytes_and_host_whitelist_is_applied() {
    let oracle = baseline::oracle(include_str!("fixtures/mcp-image-sandbox/oracle.json"));
    assert_eq!(
        field(&oracle, "maxImageBytes"),
        &JsValue::Number(pilotdeck_gateway::runtime::mcp::bridge::MCP_FILE_IMAGE_MAX_BYTES as f64),
        "use the real TS attachment image cap",
    );
    let scratch = Scratch::new("mcp-image-sandbox");
    let cwd = scratch.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    for file in array(field(&oracle, "files"), "files") {
        let hex = string(field(file, "hex"), "hex");
        let bytes: Vec<u8> = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        std::fs::write(cwd.join(string(field(file, "path"), "path")), bytes).unwrap();
    }
    std::os::unix::fs::symlink("pixel.png", cwd.join("link.png")).unwrap();
    let port = Arc::new(FakePort {
        specs: Vec::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        next: Mutex::new(JsValue::Null),
        calls: Mutex::new(Vec::new()),
    });
    let spec = McpToolSpec {
        server_id: "s".into(),
        tool_name: "t".into(),
        wire_name: "mcp__s__t".into(),
        description: "".into(),
        input_schema: object(vec![("type", s("object"))]),
        annotations: None,
        meta: None,
    };
    let tool = pilotdeck_gateway::runtime::mcp::bridge::McpBridgeTool::new(
        spec.clone(),
        port.clone(),
        CreateToolDefinitionsOptions::default(),
    );
    for row in array(field(&oracle, "cases"), "cases") {
        *port.next.lock().unwrap() = object(vec![(
            "result",
            object(vec![("content", field(row, "raw").clone())]),
        )]);
        let actual = object(execute(&tool, &object(vec![]), &UNDEFINED).await);
        assert_eq!(
            text(field(&actual, "ok")),
            text(field(row, "observed")),
            "{}",
            text(field(row, "id"))
        );
    }
    // Move the same golden image outside cwd and authorize it via host options.
    let allowed = scratch.path().join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    std::fs::rename(cwd.join("pixel.png"), allowed.join("pixel.png")).unwrap();
    std::fs::remove_file(cwd.join("link.png")).unwrap();
    std::os::unix::fs::symlink("../allowed/pixel.png", cwd.join("link.png")).unwrap();
    let row = &array(field(&oracle, "cases"), "cases")[1];
    *port.next.lock().unwrap() = object(vec![(
        "result",
        object(vec![("content", field(row, "raw").clone())]),
    )]);
    let denied = object(execute(&tool, &object(vec![]), &UNDEFINED).await);
    assert_eq!(field(field(&denied, "ok"), "content"), field(row, "raw"));
    let tool = pilotdeck_gateway::runtime::mcp::bridge::McpBridgeTool::new(
        spec,
        port,
        CreateToolDefinitionsOptions {
            image_read_allowed_roots: vec![allowed],
            ..Default::default()
        },
    );
    let actual = object(execute(&tool, &object(vec![]), &UNDEFINED).await);
    assert_eq!(text(field(&actual, "ok")), text(field(row, "observed")));
}

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn image_sandbox_generator_reproduces_committed_oracle_twice() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-image-sandbox/generate.mts");
    let committed = include_bytes!("fixtures/mcp-image-sandbox/oracle.json");
    for run in 0..2 {
        let scratch = Scratch::new("mcp-image-generate");
        let status = std::process::Command::new("node")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(&generator)
            .arg(scratch.path())
            .status()
            .expect("run node");
        assert!(status.success(), "generator run {run}");
        assert_eq!(
            std::fs::read(scratch.path().join("oracle.json")).unwrap(),
            committed
        );
    }
}
