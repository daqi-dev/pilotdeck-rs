//! #1433 S3: the four faces of the session MCP tool composition, driven over
//! the production path — a real `SdkMcpConfig` (`set_mcp_servers` /
//! `toggle_mcp_server` commits), a real `ProjectMcpRuntimeProvider` with the
//! native runtime factory (real stdio children: the shared
//! `tests/fixtures/mcp-client/mock_server.mjs`), and the production entry
//! `native_process::session_mcp_tools`. The issue's own four faces:
//! 工具发现 (discovery), 工具调用 (invocation), 禁用 (disable), 会话隔离
//! (isolation).
// Module name matters: `common/negative_receipt.rs`'s own tests reference
// `crate::test_scratch::Scratch`, so the sibling `common/scratch.rs` must be
// declared under that name here (the `native_registry_switch.rs` layout).
#[allow(dead_code)]
#[path = "../src/process_group.rs"]
mod process_group;
#[allow(dead_code)]
#[path = "common/exact_receipt.rs"]
mod test_exact_receipt;
#[allow(dead_code)]
#[path = "common/negative_receipt.rs"]
mod test_negative_receipt;
#[allow(dead_code)]
#[path = "common/permission_faults.rs"]
mod test_permission_faults;
#[allow(dead_code)]
// `common/negative_receipt.rs`'s own tests reach `crate::test_process`;
// same module-name rule as `native_registry_switch.rs`.
#[path = "../src/test_process.rs"]
mod test_process;
#[path = "common/scratch.rs"]
mod test_scratch;

use pilotdeck_gateway::{
    config::permission::GatewaySessionPermissionRuleSetRegistry,
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    native_process::{
        SessionMcpRelease, SessionMcpTools, SessionMcpToolsOptions, session_mcp_tools,
    },
    runtime::{
        mcp::{
            bridge::McpBridgeTool,
            registry::{
                BuiltinMcpPaths, ProjectMcpRuntimeProvider, ProjectMcpRuntimeProviderOptions,
                SessionMcpRuntimeRegistry,
            },
            sdk::{
                SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError, SdkMcpFuture, SdkMcpProject,
                SdkMcpProjectResolver, SdkMcpSessionPlugins, SystemEnvironment,
            },
        },
        plugins::LoadedPlugin,
        sdk_session::SdkSessionProjects,
        tools::{ExecutableTool, ToolCancellation, ToolRegistry, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use test_scratch::Scratch;

/// One scripted stdio server: initialize, advertise `echo`, answer calls with
/// `echo: ok`. The TS oracle's `basic` scenario shape (the mock server and
/// the client handshake are the production code under test — only the remote
/// end is scripted).
const SCRIPT: &str = r#"{"procs":[{"initialize":[{"send":{"jsonrpc":"2.0","id":"$id","result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"1"}}}}],"tools/list":[{"send":{"jsonrpc":"2.0","id":"$id","result":{"tools":[{"name":"echo","description":"Echo the input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"additionalProperties":false}}]}}}],"tools/call:echo":[{"send":{"jsonrpc":"2.0","id":"$id","result":{"content":[{"type":"text","text":"echo: ok"}]}}}]}]}"#;

fn mock_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-client/mock_server.mjs")
        .to_string_lossy()
        .into_owned()
}

/// `MOCK_DIR` for one server: `script.json` in, `log-<i>.jsonl` out.
fn mock_dir(scratch: &Scratch, name: &str) -> PathBuf {
    let dir = scratch.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("script.json"), SCRIPT).unwrap();
    dir
}

/// The session server config shape `set_mcp_servers` stores (TS
/// `SdkMcpServerConfig`): stdio command line for the mock server, with or
/// without `perSession: true`.
fn stdio_mock(dir: &Path, per_session: bool) -> JsValue {
    let mut env = JsObject::new();
    env.insert(
        "MOCK_DIR".into(),
        JsValue::String(dir.to_string_lossy().into_owned().into()),
    );
    let mut config = JsObject::new();
    config.insert("type".into(), JsValue::String("stdio".into()));
    config.insert("command".into(), JsValue::String("node".into()));
    config.insert(
        "args".into(),
        JsValue::Array(vec![JsValue::String(mock_path().into())]),
    );
    config.insert("env".into(), JsValue::Object(env));
    if per_session {
        config.insert("perSession".into(), JsValue::Bool(true));
    }
    JsValue::Object(config)
}

fn set_input(session_key: &str, servers: &[(&str, JsValue)]) -> JsValue {
    let mut map = JsObject::new();
    for (name, value) in servers {
        map.insert((*name).into(), value.clone());
    }
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(session_key.into()));
    input.insert("servers".into(), JsValue::Object(map));
    JsValue::Object(input)
}

fn toggle_input(session_key: &str, server_name: &str, enabled: bool) -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String(session_key.into()));
    input.insert("serverName".into(), JsValue::String(server_name.into()));
    input.insert("enabled".into(), JsValue::Bool(enabled));
    JsValue::Object(input)
}

// ── the sdk_mcp.rs harness shapes (FakeProject / FakeResolver), pointed at
// the scratch project root the provider also uses.

struct FakeProject {
    root: JsString,
}
impl SdkMcpProject for FakeProject {
    fn project_root(&self) -> JsString {
        self.root.clone()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        Box::pin(async { Ok(()) })
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
        Box::pin(async { Vec::new() })
    }
}

struct FakeResolver {
    project: Arc<FakeProject>,
}
impl SdkMcpProjectResolver for FakeResolver {
    fn resolve<'a>(
        &'a self,
        _project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let project: Arc<dyn SdkMcpProject> = self.project.clone();
        Box::pin(async move { Ok(project) })
    }
}

struct NoPlugins;
impl SdkMcpSessionPlugins for NoPlugins {
    fn plugins(&self, _session_key: &JsValue) -> Vec<Arc<LoadedPlugin>> {
        Vec::new()
    }
}

/// Real config + real provider + the factory's registry, all over one
/// scratch project with no configured MCP files (the only servers are the
/// SDK session commits the tests make).
fn harness(
    scratch: &Scratch,
) -> (
    SdkMcpConfig,
    ProjectMcpRuntimeProvider,
    SessionMcpRuntimeRegistry,
) {
    let project = scratch.path().join("project");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let config = SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(FakeResolver {
            project: Arc::new(FakeProject {
                root: project.to_string_lossy().into_owned().into(),
            }),
        }),
        close_session: Arc::new(|_session_key, _reason| Box::pin(async { Ok(()) })),
        session_plugins: Arc::new(NoPlugins),
        permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
        environment: Arc::new(SystemEnvironment),
        projects: SdkSessionProjects::shared(),
    });
    let provider = ProjectMcpRuntimeProvider::new(ProjectMcpRuntimeProviderOptions {
        project_root: project.to_string_lossy().into_owned(),
        pilot_home: home.to_string_lossy().into_owned(),
        // None → the native runtime factory: the composition spawns real
        // stdio children, exactly like the assembly does in production.
        create_runtime: None,
        proxy: None,
        on_diagnostic: None,
        environment: Arc::new(SystemEnvironment),
        builtin_paths: BuiltinMcpPaths::default(),
    });
    (config, provider, SessionMcpRuntimeRegistry::new())
}

/// The production composition entry, as the assembly's prepare closure
/// calls it.
async fn compose(
    config: &SdkMcpConfig,
    provider: &ProjectMcpRuntimeProvider,
    per_session_runtimes: &SessionMcpRuntimeRegistry,
    session_key: &str,
) -> (SessionMcpTools, SessionMcpRelease) {
    session_mcp_tools(SessionMcpToolsOptions {
        mcp_config: config,
        provider,
        per_session_runtimes,
        session_key: &session_key.into(),
        max_per_session_instances: 5.0,
        on_diagnostic: None,
    })
    .await
    .expect("#1433 session composition")
}

/// The sibling tests' node guard (mcp_client.rs:368-371, mcp_runtime.rs and
/// mcp_proxy.rs the same): the mock server is plain Node ESM, so fail loudly
/// instead of surfacing spawn errors as behaviour diffs.
fn require_node() {
    let node = std::process::Command::new("node").arg("--version").output();
    assert!(
        node.is_ok_and(|output| output.status.success()),
        "the session MCP tool tests need `node` (>= 18) on PATH to run mock_server.mjs"
    );
}

fn wire_names(tools: &[Arc<McpBridgeTool>]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| String::from_utf16_lossy(tool.spec().wire_name.units()))
        .collect()
}

fn run_context() -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: checkpoint.tool_context_state(),
        abort_signal: Some(Arc::new(ToolCancellation::default())),
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

/// The mcp_runtime.rs settle pattern: every spawned mock process has logged
/// its `exit` (bounded, so a leaked child fails the test instead of hanging).
async fn settle(dirs: &[PathBuf]) {
    for _ in 0..400 {
        let done = dirs.iter().all(|dir| {
            for index in 0.. {
                let Ok(content) = std::fs::read_to_string(dir.join(format!("log-{index}.jsonl")))
                else {
                    return true;
                };
                if !content.split('\n').any(|line| line.contains("\"exit\"")) {
                    return false;
                }
            }
            unreachable!()
        });
        if done {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("#1433 mock processes did not settle");
}

async fn invoke(tool: &McpBridgeTool, input: &str) -> String {
    let output = tool
        .execute(parse_json(input).expect("#1433 call input"), run_context())
        .await
        .expect("#1433 tool call");
    let content = JsValue::Array(output.content);
    stringify_json(&content)
        .expect("#1433 call output is serializable")
        .expect("#1433 call output is not undefined")
}

/// 面 1 · 工具发现: both the shared lease and the per-session runtime
/// surface their server's tools in the composition, under the
/// `mcp__<server>__<tool>` wire names, and the turn registry sees them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_composes_shared_and_per_session_tools() {
    require_node();
    let scratch = Scratch::new("sdk-mcp-session-tools-discovery");
    let shared_dir = mock_dir(&scratch, "shared-mock");
    let isola_dir = mock_dir(&scratch, "isola");
    let (config, provider, registry) = harness(&scratch);
    config
        .set_mcp_servers(&set_input(
            "a",
            &[
                ("shared-mock", stdio_mock(&shared_dir, false)),
                ("isola", stdio_mock(&isola_dir, true)),
            ],
        ))
        .await
        .expect("#1433 set_mcp_servers");
    let (tools, release) = compose(&config, &provider, &registry, "a").await;
    negative_assert_eq!(
        wire_names(tools.shared()),
        vec!["mcp__shared-mock__echo"],
        "#1433 the shared lease tool is discovered"
    );
    negative_assert_eq!(
        wire_names(tools.per_session()),
        vec!["mcp__isola__echo"],
        "#1433 the per-session tool is discovered"
    );
    let turn = ToolRegistry::default();
    tools.register(&turn);
    negative_assert!(
        turn.has(&"mcp__shared-mock__echo".into()),
        "#1433 the turn registry carries the shared session tool"
    );
    negative_assert!(
        turn.has(&"mcp__isola__echo".into()),
        "#1433 the turn registry carries the per-session session tool"
    );
    release.release().await.expect("#1433 release");
    settle(&[shared_dir, isola_dir]).await;
}

/// 面 2 · 工具调用: executing the composed bridge tool routes the
/// `tools/call` over the live stdio child and maps the text block back —
/// for the shared lease and for the session-owned runtime alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invocation_routes_tool_calls_through_the_children() {
    require_node();
    let scratch = Scratch::new("sdk-mcp-session-tools-invoke");
    let shared_dir = mock_dir(&scratch, "shared-mock");
    let isola_dir = mock_dir(&scratch, "isola");
    let (config, provider, registry) = harness(&scratch);
    config
        .set_mcp_servers(&set_input(
            "a",
            &[
                ("shared-mock", stdio_mock(&shared_dir, false)),
                ("isola", stdio_mock(&isola_dir, true)),
            ],
        ))
        .await
        .expect("#1433 set_mcp_servers");
    let (tools, release) = compose(&config, &provider, &registry, "a").await;
    negative_assert_eq!(
        invoke(&tools.shared()[0], r#"{"text":"hi"}"#).await,
        r#"[{"type":"text","text":"echo: ok"}]"#,
        "#1433 the shared-lease tool call answers"
    );
    negative_assert_eq!(
        invoke(&tools.per_session()[0], r#"{"text":"hi"}"#).await,
        r#"[{"type":"text","text":"echo: ok"}]"#,
        "#1433 the per-session tool call answers"
    );
    release.release().await.expect("#1433 release");
    settle(&[shared_dir, isola_dir]).await;
}

/// 面 3 · 禁用: `toggle_mcp_server(enabled: false)` removes the server from
/// the next composition (and spawns nothing for it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disable_removes_the_server_from_the_next_composition() {
    require_node();
    let scratch = Scratch::new("sdk-mcp-session-tools-disable");
    let mock = mock_dir(&scratch, "mock");
    let (config, provider, registry) = harness(&scratch);
    config
        .set_mcp_servers(&set_input("a", &[("mock", stdio_mock(&mock, false))]))
        .await
        .expect("#1433 set_mcp_servers");
    let (tools, release) = compose(&config, &provider, &registry, "a").await;
    let turn = ToolRegistry::default();
    tools.register(&turn);
    negative_assert!(
        turn.has(&"mcp__mock__echo".into()),
        "#1433 the tool is present before the disable"
    );
    release.release().await.expect("#1433 release");
    settle(std::slice::from_ref(&mock)).await;
    config
        .toggle_mcp_server(&toggle_input("a", "mock", false))
        .await
        .expect("#1433 toggle_mcp_server");
    let (tools, release) = compose(&config, &provider, &registry, "a").await;
    negative_assert!(
        tools.is_empty(),
        "#1433 the disabled server leaves the composition"
    );
    let turn = ToolRegistry::default();
    tools.register(&turn);
    negative_assert!(
        !turn.has(&"mcp__mock__echo".into()),
        "#1433 the turn registry no longer carries the disabled tool"
    );
    release.release().await.expect("#1433 release");
    // The disabled composition must not have spawned a second process.
    negative_assert!(
        !mock.join("log-1.jsonl").exists(),
        "#1433 the disabled server spawns nothing"
    );
    settle(&[mock]).await;
}

/// 面 4 · 会话隔离: each session composes from its own table only — session
/// a's servers never reach session b's composition, and releasing a leaves
/// b's session-owned runtime serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolation_composes_each_session_from_its_own_table() {
    require_node();
    let scratch = Scratch::new("sdk-mcp-session-tools-isolation");
    let a_dir = mock_dir(&scratch, "a-isola");
    let shared_dir = mock_dir(&scratch, "shared-mock");
    let b_dir = mock_dir(&scratch, "b-other");
    let (config, provider, registry) = harness(&scratch);
    config
        .set_mcp_servers(&set_input(
            "a",
            &[
                ("shared-mock", stdio_mock(&shared_dir, false)),
                ("isola", stdio_mock(&a_dir, true)),
            ],
        ))
        .await
        .expect("#1433 set_mcp_servers for a");
    config
        .set_mcp_servers(&set_input("b", &[("other", stdio_mock(&b_dir, true))]))
        .await
        .expect("#1433 set_mcp_servers for b");
    let (a_tools, a_release) = compose(&config, &provider, &registry, "a").await;
    let (b_tools, b_release) = compose(&config, &provider, &registry, "b").await;
    negative_assert_eq!(
        wire_names(a_tools.shared()),
        vec!["mcp__shared-mock__echo"],
        "#1433 session a composes its shared server"
    );
    negative_assert_eq!(
        wire_names(a_tools.per_session()),
        vec!["mcp__isola__echo"],
        "#1433 session a composes its per-session server"
    );
    negative_assert!(
        b_tools.shared().is_empty(),
        "#1433 session a's shared server does not reach session b"
    );
    negative_assert_eq!(
        wire_names(b_tools.per_session()),
        vec!["mcp__other__echo"],
        "#1433 session b composes only its own server"
    );
    negative_assert_eq!(
        registry.size(),
        2,
        "#1433 each session owns its per-session runtime"
    );
    // Releasing a stops a's children only; b's session-owned runtime keeps
    // answering.
    a_release.release().await.expect("#1433 release a");
    settle(&[shared_dir, a_dir]).await;
    negative_assert_eq!(
        invoke(&b_tools.per_session()[0], r#"{"text":"hi"}"#).await,
        r#"[{"type":"text","text":"echo: ok"}]"#,
        "#1433 session b's tool still answers after a's release"
    );
    b_release.release().await.expect("#1433 release b");
    settle(&[b_dir]).await;
}
