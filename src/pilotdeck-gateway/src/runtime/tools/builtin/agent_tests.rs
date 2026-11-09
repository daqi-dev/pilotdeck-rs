//! Contract checks run through the production module host and native child loop.
use crate::{
    host::{
        HostHandler,
        modules::{
            ModuleRouter,
            tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::{
        network::Cancellation,
        session::{
            module_host::{self, EventBuffer},
            routes::live::LiveSessionState,
        },
        tools::{
            ExecutableTool, ToolRunContext,
            builtin::web::{
                SecondaryModel, SecondaryModelEvent, SecondaryNext, SecondaryStream,
                WebFetchModelPort,
            },
        },
    },
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

pub(super) const REPORT: &str = "Scope: inspect fixture\nResult: native child proof\nKey files: none\nFiles changed: none\nIssues: none";

fn expected_agent_description(nested: bool) -> String {
    let general = if nested {
        "- general-purpose: General-purpose subagent for complex research/synthesis tasks. Has broad parent-tool access; nested delegation follows the configured depth cap. Tools: all parent tools; nested delegation is available within the configured depth cap."
    } else {
        "- general-purpose: General-purpose subagent for complex research/synthesis tasks. Has broad parent-tool access except nested subagent launch. Tools: all parent tools except nested agent launch."
    };
    [
        "Launch a new subagent to handle a focused multi-step task.",
        "",
        "Use this tool when a bounded piece of work would benefit from an autonomous helper instead of keeping every intermediate step in the parent agent's context.",
        "",
        "Provide:",
        "- `description`: a short 3-5 word label for the task.",
        "- `prompt`: the full directive for the subagent. Write it like a complete briefing: include the goal, relevant context, constraints, and what good output looks like.",
        "- `subagent_type` (optional): choose a built-in preset. If omitted, `general-purpose` is used.",
        "",
        "Available built-in subagent types:",
        general,
        "- explore: Read-only exploration subagent. Inspects files, runs grep/glob, and may run safe shell commands. Cannot edit files. Tools: read_file, grep, glob, bash.",
        "- plan: Read-only planning subagent. Inspects code via read/grep/glob and produces a step-by-step plan. Tools: read_file, grep, glob.",
        "",
        "The subagent returns one structured report with these sections: `Scope`, `Result`, `Key files`, `Files changed`, and `Issues`.",
        "",
        "Runtime behavior:",
        "- Multiple independent agent calls in one assistant message may run concurrently; batch sibling investigations when their scopes do not depend on each other.",
        "- Inside the AgentLoop, this runs a real forked subagent with its own scoped tool loop.",
        "- In stand-alone runtimes and some tests, it falls back to a single model call that preserves the same high-level subagent intent.",
    ]
    .join("\n")
}

pub(super) fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}

fn encoded(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("expected array: {value:?}"),
    }
}

fn string(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}

pub(super) fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("pdrs-native-agent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub(super) fn input(root: &Path, agent_options: Value) -> JsValue {
    let mut agent = json!({
        "provider": "proof",
        "model": "agent-proof",
        "cwd": root,
        "permissionMode": "default",
        "permissionContext": { "canPrompt": false },
        "maxSubagentDepth": 1,
        "maxOutputTokens": 1024,
        "maxContextTokens": 32768,
        "systemPrompt": "Parent fixture instructions.",
    });
    agent
        .as_object_mut()
        .unwrap()
        .extend(agent_options.as_object().unwrap().clone());
    js(json!({
        "sessionId": "agent-parent",
        "turnId": "agent-parent-turn",
        "canPrompt": false,
        "agent": agent,
    }))
}

pub(super) fn snapshot(url: &str) -> JsValue {
    js(json!({ "config": {
        "agent": { "model": "proof/agent-proof" },
        "model": { "providers": { "proof": {
            "id": "proof",
            "protocol": "openai",
            "url": format!("{url}/v1"),
            "apiKey": "public-agent-test-fixture",
            "retry": { "streamMaxRetries": 0 },
            "models": { "agent-proof": {
                "id": "agent-proof",
                "multimodal": { "input": ["text"] },
                "capabilities": {
                    "supportsStreaming": true,
                    "supportsToolUse": true,
                    "supportsSystemPrompt": true,
                    "maxContextTokens": 32768,
                    "maxOutputTokens": 4096,
                },
            } },
        } } },
    } }))
}

pub(super) fn router(
    snapshot: JsValue,
    input: &JsValue,
    events: Arc<EventBuffer>,
    cancel: Cancellation,
) -> ModuleRouter {
    let registry = test_subagent_provider_registry();
    module_host::with_subagent_providers(Some(registry), || {
        module_host::build(snapshot, input, events, cancel).unwrap()
    })
}

fn test_subagent_provider_registry(
) -> Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>
{
    use crate::runtime::session::subagent_continuation::{
        NamedContinuationProviderRegistry, NativeContinuationProvider,
    };
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<
        Arc<NamedContinuationProviderRegistry>,
    > = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let registry = Arc::new(NamedContinuationProviderRegistry::new());
            registry
                .register("pilotdeck-native", Arc::new(NativeContinuationProvider))
                .expect("test provider registry");
            registry
        })
        .clone()
}

pub(super) async fn execute(router: &ModuleRouter, name: &str, arguments: Value) -> JsObject {
    timeout(
        Duration::from_secs(10),
        router.module_call(js(json!({
            "module": "capability",
            "payload": {
                "operation": "execute",
                "toolCallId": "parent-agent-call",
                "name": name,
                "arguments": arguments,
            },
        }))),
    )
    .await
    .expect("native agent call must settle")
    .expect("capability callback must return a tool result")
}

fn assert_error(result: &JsObject, code: &str) -> JsValue {
    assert_eq!(result.get("type"), Some(&"error".into()), "{result:?}");
    let error = result.get("error").unwrap();
    assert_eq!(error.get("code"), Some(&code.into()), "{result:?}");
    error.clone()
}

pub(super) fn assert_success(result: &JsObject) -> &JsValue {
    assert_eq!(result.get("type"), Some(&"success".into()), "{result:?}");
    result.get("data").expect("agent result must carry data")
}

pub(super) enum Reply {
    Text(String),
    Tool { name: String, arguments: Value },
    ProviderError,
    Stall,
}

fn sse(reply: Reply) -> String {
    let (delta, finish) = match reply {
        Reply::Text(text) => (json!({ "role": "assistant", "content": text }), "stop"),
        Reply::Tool { name, arguments } => (
            json!({ "role": "assistant", "tool_calls": [{
                "index": 0,
                "id": "native-child-tool",
                "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() },
            }] }),
            "tool_calls",
        ),
        Reply::ProviderError => {
            return "data: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"agent fixture provider failed\"}}\n\n".into();
        }
        Reply::Stall => unreachable!("stalled connections have no terminal body"),
    };
    let chunk = json!({
        "id": "agent-fixture",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": null }],
    });
    let final_chunk = json!({
        "id": "agent-fixture",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 },
    });
    format!("data: {chunk}\n\ndata: {final_chunk}\n\ndata: [DONE]\n\n")
}

async fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let body_start = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "model request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions "));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .expect("model request must have content-length");
    while bytes.len() < body_start + length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "model request ended before its body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap()
}

pub(super) struct ModelFixture {
    pub(super) url: String,
    pub(super) requests: Arc<Mutex<Vec<Value>>>,
    received: mpsc::UnboundedReceiver<Value>,
    disconnected: mpsc::UnboundedReceiver<()>,
    task: JoinHandle<()>,
}

impl ModelFixture {
    pub(super) async fn start(replies: Vec<Reply>) -> Self {
        let mut listener = None;
        for port in 18600..=18689 {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(bound) => {
                    listener = Some(bound);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(error) => panic!("cannot bind agent model fixture: {error}"),
            }
        }
        let listener = listener.expect("an authorized agent test port must be available");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let (received_tx, received) = mpsc::unbounded_channel();
        let (disconnected_tx, disconnected) = mpsc::unbounded_channel();
        let mut replies = VecDeque::from(replies);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                recorded.lock().unwrap().push(request.clone());
                let _ = received_tx.send(request);
                let reply = replies.pop_front().unwrap_or(Reply::ProviderError);
                if matches!(reply, Reply::Stall) {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                    let mut buffer = [0; 1];
                    let closed = stream.read(&mut buffer).await;
                    assert!(
                        matches!(closed, Ok(0) | Err(_)),
                        "unexpected model client bytes"
                    );
                    let _ = disconnected_tx.send(());
                } else {
                    let body = sse(reply);
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(headers.as_bytes()).await.unwrap();
                    stream.write_all(body.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            }
        });
        Self {
            url,
            requests,
            received,
            disconnected,
            task,
        }
    }

    async fn request(&mut self) -> Value {
        timeout(Duration::from_secs(5), self.received.recv())
            .await
            .expect("native child must reach the real HTTP provider")
            .expect("model fixture must stay alive")
    }

    async fn assert_disconnected(&mut self) {
        timeout(Duration::from_secs(5), self.disconnected.recv())
            .await
            .expect("cancelled child must close its HTTP connection")
            .expect("model fixture must observe disconnect");
    }
}

impl Drop for ModelFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct McpFixture {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl McpFixture {
    async fn start(proof: String) -> Self {
        let mut listener = None;
        for port in 18600..=18689 {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(bound) => {
                    listener = Some(bound);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(error) => panic!("cannot bind agent MCP fixture: {error}"),
            }
        }
        let listener = listener.expect("an authorized agent MCP port must be available");
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let body_start = loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "MCP client must send HTTP headers");
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
                let is_post = headers.starts_with("POST /mcp ");
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < body_start + length {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "MCP request body must complete");
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let (status, body) = if is_post {
                    let request: Value =
                        serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
                    recorded.lock().unwrap().push(request.clone());
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => Some(json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "agent-proof", "version": "1" },
                        })),
                        "tools/list" => Some(json!({ "tools": [{
                            "name": "proof",
                            "description": "Return the MCP proof",
                            "inputSchema": { "type": "object", "properties": {} },
                            "annotations": { "readOnlyHint": true },
                        }] })),
                        "tools/call" => Some(
                            json!({ "content": [{ "type": "text", "text": proof }], "isError": false }),
                        ),
                        "notifications/initialized" => None,
                        method => panic!("unexpected agent MCP request: {method}"),
                    };
                    match result {
                        Some(result) => (
                            "200 OK",
                            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })
                                .to_string(),
                        ),
                        None => ("202 Accepted", String::new()),
                    }
                } else {
                    ("405 Method Not Allowed", String::new())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for McpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn tool_names(request: &Value) -> Vec<&str> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn sdk_agent_definitions_and_subagent_settings_project_the_public_contract() {
    use crate::runtime::session::subagent::apply_sdk_config;

    let mut agent = js(json!({
        "provider": "proof", "model": "parent-proof",
        "subagentDefinitions": { "stale": {} },
    }))
    .as_object()
    .unwrap()
    .clone();
    let sdk = js(json!({
        "agents": {
            "inspector": {
                "description": "Inspect a bounded task",
                "prompt": "Keep the SDK prompt exact.",
                "tools": ["read_file"],
                "disallowedTools": ["bash"],
                "model": "proof/models/inspector",
                "maxTurns": 2,
                "effort": "high",
                "permissionMode": "plan",
                "mcpServers": { "fixture": { "command": "fixture-command" } },
                "skills": ["review"],
                "memory": "disabled",
                "initialPrompt": "SDK initial context",
                "background": true,
                "observer": "auditor",
                "observerMessage": " \tObserve carefully.\n ",
                "criticalSystemReminder_EXPERIMENTAL": "SDK critical reminder",
            },
            "auditor": { "description": "Audit", "prompt": "Report findings." },
        },
        "settings": { "agent": { "subagents": { "timeoutMs": 800, "default": null } } },
    }));
    let baseline = js(json!({ "subagents": {
        "timeoutMs": 900,
        "maxDepth": 3,
        "default": { "provider": "proof", "model": "baseline-child" },
    } }));
    apply_sdk_config(&mut agent, Some(&sdk), Some(&baseline));
    let actual: Value =
        serde_json::from_str(&encoded(agent.get("subagentDefinitions").unwrap())).unwrap();
    assert_eq!(
        actual,
        json!({
            "inspector": {
                "id": "inspector",
                "description": "Inspect a bounded task",
                "systemPromptSuffix": "Keep the SDK prompt exact.",
                "allowedTools": ["read_file"],
                "disallowedTools": ["bash"],
                "modelOverride": { "provider": "proof", "model": "models/inspector" },
                "maxTurns": 2,
                "effort": "high",
                "permissionMode": "plan",
                "mcpServers": { "fixture": { "command": "fixture-command" } },
                "skills": ["review"],
                "memory": "disabled",
                "initialPrompt": "SDK initial context",
                "background": true,
                "observer": "auditor",
                "observerMessage": "Observe carefully.",
                "criticalSystemReminder": "SDK critical reminder",
                "omitProjectInstructions": false,
                "omitGitStatus": false,
                "isReadOnly": true,
            },
            "auditor": {
                "id": "auditor",
                "description": "Audit",
                "systemPromptSuffix": "Report findings.",
                "allowedTools": ["*"],
                "omitProjectInstructions": false,
                "omitGitStatus": false,
                "isReadOnly": false,
            },
        })
    );
    assert_eq!(
        agent.get("subagentTimeoutMs"),
        Some(&JsValue::Number(800.0))
    );
    assert_eq!(agent.get("maxSubagentDepth"), Some(&JsValue::Number(3.0)));
    assert!(agent.get("subagentModel").unwrap().is_undefined());

    apply_sdk_config(&mut agent, None, Some(&baseline));
    assert_eq!(
        agent.get("subagentTimeoutMs"),
        Some(&JsValue::Number(900.0))
    );
    assert_eq!(
        agent.get("subagentModel"),
        Some(&js(
            json!({ "provider": "proof", "model": "baseline-child" })
        ))
    );
    apply_sdk_config(
        &mut agent,
        Some(&js(json!({ "settings": { "agent": { "subagents": {
            "default": "invalid-without-provider",
        } } } }))),
        Some(&baseline),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&encoded(agent.get("subagentModel").unwrap())).unwrap(),
        json!({ "provider": "proof", "model": "parent-proof" }),
        "an invalid SDK model must resolve to the current parent model"
    );
    apply_sdk_config(&mut agent, Some(&js(json!({ "agents": {} }))), None);
    assert_eq!(
        agent
            .get("subagentDefinitions")
            .unwrap()
            .as_object()
            .unwrap()
            .entries()
            .len(),
        0,
        "an empty SDK catalog must replace previous definitions"
    );
}

#[tokio::test]
async fn sdk_registry_validates_and_caps_agent_skills_before_storing_configuration() {
    use crate::{
        config::Env,
        runtime::{
            plugins::{
                LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSource,
                loader::LoadedPluginCommand,
            },
            sdk_session::{
                ResolvedSdkRuntime, SdkRuntimeResolver, SdkSessionError, SdkSessionOptions,
                SdkSessionRegistry,
            },
        },
    };

    struct Resolver {
        root: PathBuf,
        runtime: Arc<PluginRuntime>,
    }
    impl SdkRuntimeResolver for Resolver {
        fn resolve(
            &self,
            _: Option<&pilotdeck_protocol::JsString>,
        ) -> Result<ResolvedSdkRuntime, SdkSessionError> {
            Ok(ResolvedSdkRuntime {
                project_root: self.root.clone(),
                snapshot: snapshot("http://127.0.0.1:18600"),
                plugin_runtime: self.runtime.clone(),
            })
        }
    }

    let root = root();
    let skill = |name: &str| LoadedPluginCommand {
        name: name.into(),
        path: root.join(format!("{}.md", name.replace(':', "-"))),
        content: "Skill contribution proof".into(),
        frontmatter: Default::default(),
        is_skill: true,
    };
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: root.clone(),
        pilot_home: root.join("sdk-home"),
        builtin_skills_root: None,
        builtin_plugins: vec![Arc::new(LoadedPlugin {
            name: "fixture".into(),
            path: root.join("fixture-plugin"),
            source: PluginSource::Builtin,
            commands: vec![],
            skills: vec![skill("ns:visible"), skill("ns:other"), skill("alt:visible")],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            router_contributions: Vec::new(),
            dispose: None,
        })],
        builtin_plugins_enabled: Default::default(),
    }));
    let mut env = Env::new();
    env.insert("HOME".into(), root.to_string_lossy().into_owned());
    env.insert(
        "PILOT_HOME".into(),
        root.join("sdk-home").to_string_lossy().into_owned(),
    );
    let mut registry = SdkSessionRegistry::new(
        SdkSessionOptions::new(
            root.join("fallback"),
            &root,
            env,
            Arc::new(Resolver {
                root: root.clone(),
                runtime: runtime.clone(),
            }),
        )
        .with_homedir(&root),
    );
    let session = "agent-skill-session".into();
    for (config, expected) in [
        (
            json!({ "skills": ["ns:visible"], "agents": { "worker": { "skills": "all" } } }),
            json!(["ns:visible"]),
        ),
        (
            json!({ "skills": ["ns:visible"], "agents": { "worker": {} } }),
            json!(["ns:visible"]),
        ),
        (
            json!({ "skills": ["ns:visible"], "agents": { "worker": { "skills": ["ns:visible", "ns:visible"] } } }),
            json!(["ns:visible"]),
        ),
        (
            json!({ "skills": [], "agents": { "worker": { "skills": "all" } } }),
            json!([]),
        ),
        (
            json!({ "skills": [], "agents": { "worker": {} } }),
            json!([]),
        ),
        (
            json!({ "agents": { "worker": { "skills": "all" } } }),
            json!("all"),
        ),
        (json!({ "agents": { "worker": {} } }), Value::Null),
    ] {
        assert!(
            registry
                .set_session_config(&session, &js(config), None, None)
                .await
                .unwrap()
        );
        let actual = registry
            .session_config(&session)
            .unwrap()
            .get("agents")
            .unwrap()
            .get("worker")
            .unwrap()
            .get("skills");
        if expected.is_null() {
            assert!(actual.is_none_or(JsValue::is_undefined));
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(&encoded(actual.unwrap())).unwrap(),
                expected
            );
        }
    }
    let previous = registry.session_config(&session).unwrap().clone();
    for (config, code, message) in [
        (
            json!({ "agents": { "worker": { "skills": ["missing"] } } }),
            "SDK_SKILL_NOT_FOUND",
            "SDK session skill is not available in this project: missing",
        ),
        (
            json!({ "skills": [], "agents": { "worker": { "skills": ["missing"] } } }),
            "SDK_SKILL_NOT_FOUND",
            "SDK session skill is not available in this project: missing",
        ),
        (
            json!({ "skills": ["ns:visible"], "agents": { "worker": { "skills": ["ns:other"] } } }),
            "SDK_AGENT_SKILL_OUT_OF_SCOPE",
            "Agent worker skill is not visible to the parent SDK session: ns:other",
        ),
        (
            json!({ "agents": { "worker": { "skills": ["visible"] } } }),
            "SDK_SKILL_AMBIGUOUS",
            "SDK session skill is ambiguous; use its fully-qualified name: visible",
        ),
    ] {
        let error = registry
            .set_session_config(&session, &js(config), None, None)
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, message);
        assert_eq!(
            registry.session_config(&session),
            Some(&previous),
            "rejected child scope must not commit SDK configuration"
        );
    }
    runtime.dispose().await.unwrap();
}

struct FallbackStream(VecDeque<SecondaryModelEvent>);

impl SecondaryStream for FallbackStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        Box::pin(async move { self.0.pop_front() })
    }
}

#[derive(Default)]
struct FallbackModel {
    events: Mutex<VecDeque<Vec<SecondaryModelEvent>>>,
    requests: Mutex<Vec<JsValue>>,
}

impl SecondaryModel for FallbackModel {
    fn stream(&self, request: JsValue, signal: Option<ToolRunContext>) -> Box<dyn SecondaryStream> {
        assert!(signal.is_some(), "fallback must pass the tool signal");
        self.requests.lock().unwrap().push(request);
        Box::new(FallbackStream(
            self.events
                .lock()
                .unwrap()
                .pop_front()
                .expect("fallback model stream must be configured")
                .into(),
        ))
    }
}

fn fallback_context(model: Option<Arc<dyn SecondaryModel>>) -> ToolRunContext {
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: Some(Arc::new(Cancellation::new())),
        model: model.map(|model| Arc::new(WebFetchModelPort(model)) as _),
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

#[tokio::test]
async fn standalone_fallback_without_a_model_preserves_the_precise_error() {
    let error = super::agent::AgentTool::new()
        .execute(
            js(json!({ "description": "Fallback proof", "prompt": "directive" })),
            fallback_context(None),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported_tool");
    assert_eq!(error.message, "agent tool requires a model client. Configure dependencies.model on AgentRuntimeDependencies, pass createAgentTool({ model }), or wire context.subagent for full-fork mode.".into());
}

#[tokio::test]
async fn standalone_fallback_streams_model_text_usage_and_failures() {
    let model = Arc::new(FallbackModel {
        events: Mutex::new(VecDeque::from([
            vec![
                SecondaryModelEvent::Event(js(
                    json!({ "type": "text_delta", "text": " \tFallback " }),
                )),
                SecondaryModelEvent::Event(js(
                    json!({ "type": "text_delta", "text": "report\n " }),
                )),
                SecondaryModelEvent::Event(js(
                    json!({ "type": "usage", "usage": { "totalTokens": 23 } }),
                )),
            ],
            vec![SecondaryModelEvent::Event(js(
                json!({ "type": "error", "error": { "message": "fallback failure proof", "code": "fixture_error" } }),
            ))],
            vec![SecondaryModelEvent::Throw {
                is_error: true,
                value: js(json!({ "message": "fallback iterator proof" })),
            }],
            vec![],
        ])),
        requests: Mutex::default(),
    });
    let tool = super::agent::AgentTool::new();
    let arguments = js(
        json!({ "description": "Fallback proof", "prompt": "  exact directive  ", "subagent_type": "plan" }),
    );
    let output = tool
        .execute(arguments.clone(), fallback_context(Some(model.clone())))
        .await
        .unwrap();
    let data = output.data.unwrap();
    assert_eq!(data.get("text"), Some(&"Fallback report".into()));
    assert_eq!(
        data.get("usage").unwrap().get("totalTokens"),
        Some(&JsValue::Number(23.0))
    );
    assert_eq!(data.get("subagentType"), Some(&"plan".into()));
    let metadata = output.metadata.unwrap();
    assert_eq!(metadata.get("forkMode"), Some(&"fallback".into()));
    assert_eq!(metadata.get("provider"), Some(&"pilotdeck".into()));
    assert_eq!(metadata.get("model"), Some(&"moonshotai/kimi-k2.6".into()));
    let error = tool
        .execute(arguments.clone(), fallback_context(Some(model.clone())))
        .await
        .unwrap_err();
    assert_eq!(error.code, "tool_execution_failed");
    assert_eq!(
        error.message,
        "agent subagent model error: fallback failure proof".into()
    );
    assert_eq!(
        error.details.unwrap().get("errorCode"),
        Some(&"fixture_error".into())
    );
    let error = tool
        .execute(arguments.clone(), fallback_context(Some(model.clone())))
        .await
        .unwrap_err();
    assert_eq!(error.code, "tool_execution_failed");
    assert_eq!(error.message, "fallback iterator proof".into());
    let output = tool
        .execute(arguments, fallback_context(Some(model.clone())))
        .await
        .unwrap();
    assert_eq!(
        output.data.unwrap().get("text"),
        Some(&"(empty subagent response)".into())
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].get("provider"), Some(&"pilotdeck".into()));
    assert_eq!(
        requests[0].get("model"),
        Some(&"moonshotai/kimi-k2.6".into())
    );
    assert_eq!(
        requests[0].get("maxOutputTokens"),
        Some(&JsValue::Number(65_536.0))
    );
    assert_eq!(requests[0].get("stream"), Some(&JsValue::Bool(true)));
    assert!(string(requests[0].get("systemPrompt").unwrap()).contains("planning subagent"));
    let content = array(
        array(requests[0].get("messages").unwrap())[0]
            .get("content")
            .unwrap(),
    );
    assert_eq!(content.len(), 1);
    assert_eq!(content[0].get("type"), Some(&"text".into()));
    assert_eq!(content[0].get("text"), Some(&"  exact directive  ".into()));
}

#[test]
fn production_registry_exposes_agent_aliases_schema_and_capability() {
    let registry = module_host::build_registry(&JsValue::Null, None).unwrap();
    let agent = registry
        .get(&"agent".into())
        .expect("production agent registration");
    for alias in ["Agent", "Task"] {
        assert!(Arc::ptr_eq(&agent, &registry.get(&alias.into()).unwrap()));
    }
    let fields = agent.fields();
    assert_eq!(fields.get("aliases"), Some(&js(json!(["Agent", "Task"]))));
    assert_eq!(
        fields.get("description"),
        Some(&expected_agent_description(false).as_str().into())
    );
    assert_eq!(fields.get("kind"), Some(&"agent".into()));
    assert_eq!(fields.get("isOpenWorld"), Some(&JsValue::Bool(true)));
    assert_eq!(
        fields.get("maxResultBytes"),
        Some(&JsValue::Number(200_000.0))
    );
    assert_eq!(
        agent.required_runtime_capabilities(),
        Some(vec!["subagent_fork".into()])
    );
    assert_eq!(
        agent.is_read_only(&JsValue::Null).unwrap(),
        JsValue::Bool(false)
    );
    assert_eq!(
        agent.is_concurrency_safe(&JsValue::Null).unwrap(),
        JsValue::Bool(true)
    );
    let schema = fields.get("inputSchema").unwrap();
    assert_eq!(
        encoded(schema),
        r#"{"type":"object","required":["description","prompt"],"additionalProperties":false,"properties":{"description":{"type":"string","description":"Short 3-5 word task summary used to label the subagent run."},"prompt":{"type":"string","description":"Detailed directive for the subagent. Include the goal, relevant context, constraints, and desired output; do not assume the subagent already knows why the task matters."},"subagent_type":{"type":"string","description":"Optional subagent preset. Public built-ins: 'general-purpose' (full tool access), 'explore' (read-only investigation with read_file/grep/glob/bash), or 'plan' (read-only planning with read_file/grep/glob). Some runtimes may also expose additional presets such as 'verify'. Defaults to 'general-purpose' when omitted. Legacy 'general_purpose' is still accepted for compatibility."},"subagentType":{"type":"string","description":"Deprecated legacy alias for subagent_type. Prefer subagent_type."}}}"#
    );
    assert_eq!(schema.get("type"), Some(&"object".into()));
    assert_eq!(
        schema.get("additionalProperties"),
        Some(&JsValue::Bool(false))
    );
    assert_eq!(
        schema.get("required"),
        Some(&js(json!(["description", "prompt"])))
    );
    for property in ["description", "prompt", "subagent_type", "subagentType"] {
        let property = schema.get("properties").unwrap().get(property).unwrap();
        assert_eq!(property.get("type"), Some(&"string".into()));
        assert!(
            property.get("minLength").is_none(),
            "empty strings remain valid"
        );
    }
    assert_eq!(
        schema.get("properties").unwrap().get("subagent_type").unwrap().get("description"),
        Some(&"Optional subagent preset. Public built-ins: 'general-purpose' (full tool access), 'explore' (read-only investigation with read_file/grep/glob/bash), or 'plan' (read-only planning with read_file/grep/glob). Some runtimes may also expose additional presets such as 'verify'. Defaults to 'general-purpose' when omitted. Legacy 'general_purpose' is still accepted for compatibility.".into())
    );
    assert!(
        module_host::descriptors(&registry)
            .unwrap()
            .iter()
            .any(|tool| {
                tool.get("name") == Some(&"agent".into())
                    && tool.get("requiredRuntimeCapabilities")
                        == Some(&js(json!(["subagent_fork"])))
            })
    );
}

#[tokio::test]
async fn production_schema_rejects_invalid_arguments_before_child_model() {
    let root = root();
    let router = router(
        JsValue::Null,
        &input(&root, json!({})),
        Arc::default(),
        Cancellation::new(),
    );
    for arguments in [
        json!({}),
        json!({ "description": "label" }),
        json!({ "description": 1, "prompt": "directive" }),
        json!({ "description": "label", "prompt": [] }),
        json!({ "description": "label", "prompt": "directive", "subagent_type": 1 }),
        json!({ "description": "label", "prompt": "directive", "subagentType": null }),
        json!({ "description": "label", "prompt": "directive", "unexpected": true }),
    ] {
        let result = execute(&router, "agent", arguments).await;
        let error = assert_error(&result, "invalid_tool_input");
        assert!(!array(error.get("details").unwrap().get("issues").unwrap()).is_empty());
    }
}

#[tokio::test]
async fn unknown_preset_and_depth_errors_match_the_fork_contract() {
    let root = root();
    let events = Arc::new(EventBuffer::default());
    let router = router(
        JsValue::Null,
        &input(&root, json!({ "subagentDepth": 1 })),
        events.clone(),
        Cancellation::new(),
    );
    let unknown = execute(&router, "agent", json!({ "description": "label", "prompt": "directive", "subagent_type": "  Custom-Type  " })).await;
    let error = assert_error(&unknown, "invalid_tool_input");
    assert_eq!(error.get("message"), Some(&"Unknown subagent_type \"Custom-Type\". Available: general-purpose, explore, plan, verify.".into()));
    let depth = execute(
        &router,
        "Task",
        json!({ "description": "label", "prompt": "directive" }),
    )
    .await;
    let error = assert_error(&depth, "tool_execution_failed");
    assert_eq!(
        error.get("message"),
        Some(&"subagent_depth_exceeded (depth=1, max=1); nested fork rejected.".into())
    );
    assert_eq!(
        error.get("details").unwrap().get("errorCode"),
        Some(&"subagent_depth_exceeded".into())
    );
    assert!(
        events.drain().is_empty(),
        "rejected forks must not start children"
    );
}

#[tokio::test]
async fn agent_permission_allow_preserves_explicit_parent_deny() {
    let root = root();
    let input = input(
        &root,
        json!({ "permissionContext": {
        "canPrompt": false,
        "rules": { "deny": [{ "source": "session", "behavior": "deny", "toolName": "agent" }] },
    } }),
    );
    let events = Arc::new(EventBuffer::default());
    let router = router(JsValue::Null, &input, events.clone(), Cancellation::new());
    let result = execute(
        &router,
        "Agent",
        json!({ "description": "label", "prompt": "directive" }),
    )
    .await;
    assert_error(&result, "permission_denied");
    assert!(events.drain().is_empty());
}

#[tokio::test]
async fn production_agent_normalizes_presets_and_remaps_ask_and_plan() {
    let root = root();
    let cases = [
        (json!({}), json!({}), "general-purpose"),
        (
            json!({ "subagent_type": " GeNeRaL_PuRpOsE " }),
            json!({}),
            "general-purpose",
        ),
        (
            json!({ "subagent_type": "general purpose" }),
            json!({}),
            "general-purpose",
        ),
        (
            json!({ "subagentType": " ExPlOrEr " }),
            json!({}),
            "explore",
        ),
        (json!({ "subagent_type": " PlAn " }), json!({}), "plan"),
        (json!({ "subagent_type": " VERIFY " }), json!({}), "verify"),
        (
            json!({ "subagent_type": "\u{feff}\t ", "subagentType": "plan" }),
            json!({}),
            "general-purpose",
        ),
        (
            json!({ "subagent_type": "general-purpose" }),
            json!({ "runMode": "ask" }),
            "explore",
        ),
        (json!({}), json!({ "permissionMode": "plan" }), "explore"),
    ];
    let server =
        ModelFixture::start(cases.iter().map(|_| Reply::Text(REPORT.into())).collect()).await;
    for (extra, config, expected) in cases {
        let events = Arc::new(EventBuffer::default());
        let router = router(
            snapshot(&server.url),
            &input(&root, config),
            events.clone(),
            Cancellation::new(),
        );
        let mut arguments = json!({ "description": "", "prompt": "  exact directive\n" });
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let result = execute(&router, "agent", arguments).await;
        let data = assert_success(&result);
        assert_eq!(data.get("subagentType"), Some(&expected.into()));
        assert_eq!(
            data.get("description"),
            Some(&"".into()),
            "empty description is valid"
        );
        assert_eq!(data.get("text"), Some(&REPORT.into()));
        let content = array(result.get("content").unwrap());
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].get("type"), Some(&"text".into()));
        assert_eq!(
            content[0].get("text"),
            Some(&format!("[{expected}] \n\n{REPORT}").as_str().into())
        );
        assert_eq!(content[1].get("type"), Some(&"json".into()));
        assert_eq!(content[1].get("value"), Some(data));
        assert_eq!(
            result.get("metadata").unwrap().get("forkMode"),
            Some(&"full".into())
        );
        assert!(
            !events
                .drain()
                .iter()
                .any(|event| event.get("type") == Some(&"permission_request".into()))
        );
        let requests = server.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert!(
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["role"] == "user" && message["content"] == "  exact directive\n"
                }),
            "directive must survive without trimming: {request}"
        );
        let names = tool_names(request);
        assert!(
            !names.contains(&"agent"),
            "default depth must remove nested delegation"
        );
        if expected == "plan" {
            assert!(!names.contains(&"bash"));
        }
        if expected != "general-purpose" {
            assert!(!names.contains(&"write_file"));
            assert!(!names.contains(&"edit_file"));
        }
    }
    assert_eq!(server.requests.lock().unwrap().len(), 9);
}

#[tokio::test]
async fn real_child_loop_reads_a_file_then_returns_structured_report_and_events() {
    let root = root();
    let proof = format!("native-child-read-{}", uuid::Uuid::new_v4());
    let path = root.join("proof.txt");
    std::fs::write(&path, &proof).unwrap();
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "read_file".into(),
            arguments: json!({ "file_path": path }),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let events = Arc::new(EventBuffer::default());
    let router = router(
        snapshot(&server.url),
        &input(&root, json!({})),
        events.clone(),
        Cancellation::new(),
    );
    let listed = router
        .module_call(js(
            json!({ "module": "capability", "payload": { "operation": "list_tools" } }),
        ))
        .await
        .unwrap();
    assert!(
        array(listed.get("tools").unwrap())
            .iter()
            .any(|tool| tool.get("name") == Some(&"agent".into()))
    );
    let result = execute(&router, "Task", json!({ "description": "Read a proof file", "prompt": "Use read_file to inspect the provided proof file, then return the structured report." })).await;
    let data = assert_success(&result);
    assert_eq!(data.get("subagentType"), Some(&"general-purpose".into()));
    assert_eq!(data.get("text"), Some(&REPORT.into()));
    assert_eq!(data.get("turns"), Some(&JsValue::Number(2.0)));
    assert_eq!(
        data.get("parsed").unwrap().get("Result"),
        Some(&"native child proof".into())
    );
    assert_eq!(
        data.get("usage").unwrap().get("totalTokens"),
        Some(&JsValue::Number(36.0))
    );
    assert!(
        data.get("durationMs")
            .and_then(JsValue::as_number)
            .is_some_and(|ms| ms >= 0.0)
    );
    let content = array(result.get("content").unwrap());
    assert_eq!(content.len(), 2);
    assert_eq!(
        content[0].get("text"),
        Some(
            &format!("[general-purpose] Read a proof file\n\n{REPORT}")
                .as_str()
                .into()
        )
    );
    assert_eq!(content[1].get("type"), Some(&"json".into()));
    assert_eq!(content[1].get("value"), Some(data));
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "a real child must continue after its tool result"
    );
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains(&proof))
            }),
        "second model invocation must contain actual filesystem proof: {}",
        requests[1]
    );
    let emitted = events.drain();
    let started: Vec<_> = emitted
        .iter()
        .filter(|event| event.get("type") == Some(&"subagent_started".into()))
        .collect();
    let completed: Vec<_> = emitted
        .iter()
        .filter(|event| event.get("type") == Some(&"subagent_completed".into()))
        .collect();
    assert_eq!(started.len(), 1, "{emitted:?}");
    assert_eq!(completed.len(), 1, "{emitted:?}");
    let metadata = result.get("metadata").unwrap();
    assert_eq!(started[0].get("subagentId"), metadata.get("subagentId"));
    assert_eq!(completed[0].get("subagentId"), metadata.get("subagentId"));
    assert_eq!(
        started[0].get("toolCallId"),
        Some(&"parent-agent-call".into())
    );
    assert_eq!(started[0].get("sessionId"), Some(&"agent-parent".into()));
    assert_eq!(completed[0].get("success"), Some(&JsValue::Bool(true)));
    assert!(
        !emitted
            .iter()
            .any(|event| event.get("type") == Some(&"permission_request".into()))
    );
}

#[tokio::test]
async fn child_registry_cannot_expand_explicit_parent_allowlist() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let mut input = input(&root, json!({})).as_object().unwrap().clone();
    input.insert("allowedTools".into(), js(json!(["agent", "read_file"])));
    let router = router(
        snapshot(&server.url),
        &JsValue::Object(input),
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Bound child tools", "prompt": "Return a structured report." }),
    )
    .await;
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(tool_names(&requests[0]), vec!["read_file"]);
}

#[tokio::test]
async fn native_nested_delegation_obeys_the_effective_depth_cap() {
    let root = root();
    let server = ModelFixture::start(vec![
        Reply::Text(REPORT.into()),
        Reply::Tool {
            name: "agent".into(),
            arguments: json!({ "description": "Nested proof", "prompt": "Return a structured report." }),
        },
        Reply::Text(REPORT.into()),
        Reply::Text(REPORT.into()),
    ])
    .await;
    let cap_two = router(
        snapshot(&server.url),
        &input(&root, json!({ "maxSubagentDepth": 2 })),
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(
        &cap_two,
        "agent",
        json!({ "description": "Depth two", "prompt": "Return a structured report." }),
    )
    .await;
    assert_success(&result);
    assert!(
        !tool_names(&server.requests.lock().unwrap()[0]).contains(&"agent"),
        "the TS native fork increments depth in both adapter and composition"
    );
    let events = Arc::new(EventBuffer::default());
    let cap_three = router(
        snapshot(&server.url),
        &input(&root, json!({ "maxSubagentDepth": 3 })),
        events.clone(),
        Cancellation::new(),
    );
    let result = execute(
        &cap_three,
        "agent",
        json!({ "description": "Depth three", "prompt": "Delegate one bounded nested task and return a structured report." }),
    )
    .await;
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        4,
        "nested agent must run its own model turn"
    );
    assert!(tool_names(&requests[1]).contains(&"agent"));
    assert!(!tool_names(&requests[2]).contains(&"agent"));
    assert!(
        requests[3]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("native child proof"))
            })
    );
    let emitted = events.drain();
    for kind in ["subagent_started", "subagent_completed"] {
        assert_eq!(
            emitted
                .iter()
                .filter(|event| event.get("type") == Some(&kind.into()))
                .count(),
            1,
            "TS forwards child activity but not nested lifecycle events: {emitted:?}"
        );
    }
}

#[tokio::test]
async fn child_agent_description_shadows_parent_depth_only_in_its_local_registry() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let router = router(
        snapshot(&server.url),
        &input(&root, json!({ "maxSubagentDepth": 3 })),
        Arc::default(),
        Cancellation::new(),
    );
    let describe_parent = || async {
        let listed = router
            .module_call(js(
                json!({ "module": "capability", "payload": { "operation": "list_tools" } }),
            ))
            .await
            .unwrap();
        array(listed.get("tools").unwrap())
            .iter()
            .find(|tool| tool.get("name") == Some(&"agent".into()))
            .unwrap()
            .get("description")
            .unwrap()
            .clone()
    };
    assert_eq!(
        describe_parent().await,
        expected_agent_description(true).as_str().into()
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Child depth prose", "prompt": "Return a structured report." }),
    )
    .await;
    assert_success(&result);
    assert_eq!(
        describe_parent().await,
        expected_agent_description(true).as_str().into()
    );
    let requests = server.requests.lock().unwrap();
    let child_agent = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "agent")
        .unwrap();
    assert_eq!(
        child_agent["function"]["description"],
        expected_agent_description(false)
    );
}

#[tokio::test]
async fn direct_definition_uses_catalog_keys_for_dispatch_but_ids_for_list_and_events() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let events = Arc::new(EventBuffer::default());
    let router = router(
        snapshot(&server.url),
        &input(
            &root,
            json!({ "subagentDefinitions": {
            "dispatch-route": { "id": "visible-agent", "allowedTools": ["read_file"] },
        } }),
        ),
        events.clone(),
        Cancellation::new(),
    );
    let invalid = execute(&router, "agent", json!({ "description": "Visible id is not key", "prompt": "directive", "subagent_type": "visible-agent" })).await;
    assert_eq!(
        assert_error(&invalid, "invalid_tool_input").get("message"),
        Some(&"Unknown subagent_type \"visible-agent\". Available: visible-agent.".into())
    );
    assert!(server.requests.lock().unwrap().is_empty());
    let result = execute(&router, "agent", json!({ "description": "Dispatch by catalog key", "prompt": "Return a structured report.", "subagent_type": "dispatch-route" })).await;
    assert_eq!(
        assert_success(&result).get("subagentType"),
        Some(&"dispatch-route".into())
    );
    assert_eq!(
        result.get("metadata").unwrap().get("subagent"),
        Some(&"dispatch-route".into())
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    let emitted = events.drain();
    for kind in ["subagent_started", "subagent_completed"] {
        let events: Vec<_> = emitted
            .iter()
            .filter(|event| event.get("type") == Some(&kind.into()))
            .collect();
        assert_eq!(events.len(), 1, "{emitted:?}");
        assert_eq!(events[0].get("subagentType"), Some(&"visible-agent".into()));
    }
}

#[tokio::test]
async fn direct_definition_skills_project_an_empty_host_without_rejecting_the_fork() {
    let root = root();
    let server =
        ModelFixture::start(vec![Reply::Text(REPORT.into()), Reply::Text(REPORT.into())]).await;
    for skills in [json!("all"), json!(["missing-skill"])] {
        let router = router(
            snapshot(&server.url),
            &input(
                &root,
                json!({ "subagentDefinitions": {
                "skill-proof": { "allowedTools": ["read_file"], "skills": skills },
            } }),
            ),
            Arc::default(),
            Cancellation::new(),
        );
        let result = execute(&router, "agent", json!({ "description": "Empty skill contribution", "prompt": "Return a structured report.", "subagent_type": "skill-proof" })).await;
        assert_eq!(assert_success(&result).get("text"), Some(&REPORT.into()));
    }
    assert_eq!(
        server.requests.lock().unwrap().len(),
        2,
        "both skill selections must reach the real model on an empty host"
    );
}

#[tokio::test]
async fn dynamic_definition_replaces_presets_and_applies_tools_model_and_initial_prompt() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let mut snapshot = snapshot(&server.url).as_object().unwrap().clone();
    let mut config = snapshot.get("config").unwrap().as_object().unwrap().clone();
    let mut model = config.get("model").unwrap().as_object().unwrap().clone();
    let mut providers = model.get("providers").unwrap().as_object().unwrap().clone();
    let mut provider = providers.get("proof").unwrap().as_object().unwrap().clone();
    provider.insert("models".into(), js(json!({
        "agent-proof": { "id": "agent-proof", "multimodal": { "input": ["text"] }, "capabilities": { "supportsStreaming": true, "supportsToolUse": true, "supportsSystemPrompt": true, "maxContextTokens": 32768, "maxOutputTokens": 4096 } },
        "child-proof": { "id": "child-proof", "multimodal": { "input": ["text"] }, "capabilities": { "supportsStreaming": true, "supportsToolUse": true, "supportsSystemPrompt": true, "maxContextTokens": 32768, "maxOutputTokens": 512 } },
    })));
    providers.insert("proof".into(), JsValue::Object(provider));
    model.insert("providers".into(), JsValue::Object(providers));
    config.insert("model".into(), JsValue::Object(model));
    snapshot.insert("config".into(), JsValue::Object(config));
    let input = input(
        &root,
        json!({
            "subagentDefinitions": { "special": {
                "id": "special",
                "description": "Dynamic fixture definition",
                "allowedTools": ["read_file", "bash"],
                "disallowedTools": ["bash"],
                "isReadOnly": true,
                "omitProjectInstructions": true,
                "omitGitStatus": true,
                "systemPromptSuffix": "DYNAMIC_CHILD_POLICY_PROOF",
                "initialPrompt": "  dynamic initial context  ",
                "maxTurns": 3,
                "modelOverride": { "provider": "proof", "model": "child-proof" },
            } },
        }),
    );
    let router = router(
        JsValue::Object(snapshot),
        &input,
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(&router, "agent", json!({ "description": "Dynamic child", "prompt": "  exact dynamic directive  ", "subagent_type": "special" })).await;
    let data = assert_success(&result);
    assert_eq!(data.get("subagentType"), Some(&"special".into()));
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["model"], "child-proof");
    assert_eq!(tool_names(request), vec!["read_file"]);
    assert!(
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "system"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("DYNAMIC_CHILD_POLICY_PROOF"))
            })
    );
    let users: Vec<_> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert_eq!(
        users,
        ["dynamic initial context", "  exact dynamic directive  "]
    );
    let unknown = execute(
        &router,
        "agent",
        json!({ "description": "Default removed", "prompt": "directive" }),
    )
    .await;
    assert_eq!(
        assert_error(&unknown, "invalid_tool_input").get("message"),
        Some(&"Unknown subagent_type \"general-purpose\". Available: special.".into())
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dynamic_definition_max_turns_limits_the_real_child_loop() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::Tool {
        name: "get_current_time".into(),
        arguments: json!({}),
    }])
    .await;
    let input = input(
        &root,
        json!({ "subagentDefinitions": { "bounded": {
        "id": "bounded",
        "allowedTools": ["get_current_time"],
        "systemPromptSuffix": "Return a structured report.",
        "maxTurns": 1,
    } } }),
    );
    let router = router(
        snapshot(&server.url),
        &input,
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(&router, "agent", json!({ "description": "Bound loop turns", "prompt": "Return a structured report.", "subagent_type": "bounded" })).await;
    let data = assert_success(&result);
    assert_eq!(data.get("turns"), Some(&JsValue::Number(2.0)));
    assert_eq!(data.get("text"), Some(&"".into()));
    assert!(data.get("parsed").is_none_or(JsValue::is_undefined));
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "definition maxTurns must stop further model calls"
    );
}

#[tokio::test]
async fn definition_mcp_tool_executes_through_http_and_reaches_the_real_child_model() {
    let root = root();
    let proof = format!("native-mcp-proof-{}", uuid::Uuid::new_v4());
    let mcp = McpFixture::start(proof.clone()).await;
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "mcp__fixture__proof".into(),
            arguments: json!({}),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let input = input(
        &root,
        json!({ "subagentDefinitions": { "mcp-proof": {
            "allowedTools": ["mcp__fixture__proof"],
            "mcpServers": { "fixture": { "type": "http", "url": mcp.url, "timeout": 2000 } },
        } } }),
    );
    let router = router(
        snapshot(&server.url),
        &input,
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "MCP child proof", "prompt": "Call the proof MCP tool and return a structured report.", "subagent_type": "mcp-proof" }),
    )
    .await;
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_names(&requests[0]), vec!["mcp__fixture__proof"]);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains(&proof))
            }),
        "a real MCP response must reach the child continuation: {}",
        requests[1]
    );
    let requests = mcp.requests.lock().unwrap();
    for method in [
        "initialize",
        "notifications/initialized",
        "tools/list",
        "tools/call",
    ] {
        assert!(
            requests.iter().any(|request| request["method"] == method),
            "missing MCP {method}: {requests:?}"
        );
    }
    assert_eq!(
        requests
            .iter()
            .find(|request| request["method"] == "tools/call")
            .unwrap()["params"],
        json!({ "name": "proof", "arguments": {} })
    );
}

#[tokio::test]
async fn definition_mcp_honors_allowed_and_disallowed_tool_lists() {
    let root = root();
    let mcp = McpFixture::start("disallowed-proof".into()).await;
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let input = input(
        &root,
        json!({ "subagentDefinitions": { "mcp-filter": {
            "allowedTools": ["mcp__fixture__proof"],
            "disallowedTools": ["mcp__fixture__proof"],
            "mcpServers": { "fixture": { "type": "http", "url": mcp.url, "timeout": 2000 } },
        } } }),
    );
    let router = router(
        snapshot(&server.url),
        &input,
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({
            "description": "MCP filter proof",
            "prompt": "Try the proof MCP tool and return a structured report.",
            "subagent_type": "mcp-filter",
        }),
    )
    .await;
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "disallowed MCP tools must not reach the child model (SubAgentSession.ts:302-306)"
    );
    assert!(
        !requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "mcp__fixture__proof"),
        "definition MCP attach must filter denied tools before registry registration"
    );
}

#[tokio::test]
async fn native_subagent_fork_pairs_subagent_start_and_stop_lifecycle_hooks() {
    use crate::hooks::config::parse_hooks_config;
    use crate::runtime::hooks::{
        HookRuntime, LifecycleRuntime, runtime::HookExecutionEventHandler,
    };
    use crate::runtime::session::module_host::with_lifecycle;
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let lifecycle_events = Arc::new(Mutex::new(Vec::<String>::new()));
    struct Recorder(Arc<Mutex<Vec<String>>>);
    impl HookExecutionEventHandler for Recorder {
        fn handle(&self, event: &JsObject) {
            let Some(event_name) = event.get("hookEvent").and_then(JsValue::as_string) else {
                return;
            };
            let name = String::from_utf16_lossy(event_name.units());
            if name == "SubagentStart" || name == "SubagentStop" {
                self.0.lock().unwrap().push(name);
            }
        }
    }
    let settings = parse_hooks_config(&js(json!({
        "SubagentStart": [{ "hooks": [{ "type": "command", "command": "/bin/true" }] }],
        "SubagentStop": [{ "hooks": [{ "type": "command", "command": "/bin/true" }] }],
    })))
    .settings;
    let hook_runtime = HookRuntime::new(settings);
    let _subscription = hook_runtime
        .subscribe_execution_events(Arc::new(Recorder(lifecycle_events.clone())))
        .unwrap();
    let lifecycle = LifecycleRuntime::new(hook_runtime);
    let buffer = Arc::new(EventBuffer::default());
    let router = with_lifecycle(lifecycle, || {
        module_host::with_subagent_providers(Some(test_subagent_provider_registry()), || {
            module_host::build(
                snapshot(&server.url),
                &input(&root, json!({})),
                buffer,
                Cancellation::new(),
            )
            .unwrap()
        })
    });
    assert_success(
        &execute(
            &router,
            "Task",
            json!({
                "description": "Lifecycle pairing proof",
                "prompt": "Return a structured report.",
            }),
        )
        .await,
    );
    let names = lifecycle_events.lock().unwrap();
    let start = names.iter().position(|name| name == "SubagentStart");
    let stop = names.iter().position(|name| name == "SubagentStop");
    assert!(start.is_some(), "SubagentStart lifecycle dispatch missing: {names:?}");
    assert!(stop.is_some(), "SubagentStop lifecycle dispatch missing: {names:?}");
    assert!(
        start.unwrap() < stop.unwrap(),
        "SubagentStop must follow SubagentStart: {names:?}"
    );
}

fn background_input(root: &Path) -> JsValue {
    input(
        root,
        json!({ "subagentDefinitions": { "background-proof": {
        "id": "background-proof",
        "allowedTools": ["read_file"],
        "systemPromptSuffix": "Return a structured report.",
        "background": true,
    } } }),
    )
}

#[tokio::test]
async fn background_definition_requires_a_real_gateway_launcher() {
    let root = root();
    let router = router(
        JsValue::Null,
        &background_input(&root),
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(&router, "agent", json!({ "description": "Missing launcher", "prompt": "Return a structured report.", "subagent_type": "background-proof" })).await;
    let error = assert_error(&result, "unsupported_tool");
    assert_eq!(error.get("message"), Some(&"Background subagent background-proof requires a Gateway background-subagent launcher.".into()));
}

#[tokio::test]
async fn background_launch_returns_task_id_and_owner_drains_the_real_child_on_stop() {
    use crate::runtime::{
        background::{BackgroundResources, TaskStatus},
        steer::{SteerMailbox, TurnSteers},
    };
    let root = root();
    let mut server = ModelFixture::start(vec![Reply::Stall]).await;
    let resources = BackgroundResources::default();
    let services = Arc::new(
        crate::cron::native::NativeSessionServices::open(
            &root.join("background-home"),
            &root,
            &root.to_string_lossy(),
            &"agent-parent".into(),
            None,
            resources.clone(),
        )
        .unwrap(),
    );
    let input = input(
        &root,
        json!({ "subagentDefinitions": { "background-route": {
            "id": "background-proof",
            "allowedTools": ["read_file"],
            "background": true,
        } } }),
    );
    let mailbox = SteerMailbox::new(None);
    mailbox.start("agent-parent-turn".into());
    let events = Arc::new(EventBuffer::default());
    let cancel = Cancellation::new();
    let router = module_host::build_with_services_runtime(
        snapshot(&server.url),
        &input,
        events.clone(),
        cancel.clone(),
        TurnSteers::new(mailbox, "agent-parent-turn".into(), vec![]),
        None,
        Some(services),
    )
    .unwrap();
    let result = execute(&router, "agent", json!({ "description": "Background child", "prompt": "Return a structured report.", "subagent_type": "background-route" })).await;
    let data = assert_success(&result);
    assert_eq!(data.get("subagentType"), Some(&"background-route".into()));
    let task_id = data
        .get("backgroundTaskId")
        .unwrap()
        .as_string()
        .unwrap()
        .clone();
    assert_eq!(
        result.get("metadata").unwrap().get("forkMode"),
        Some(&"background".into())
    );
    assert_eq!(
        result.get("metadata").unwrap().get("backgroundTaskId"),
        Some(&JsValue::String(task_id.clone()))
    );
    assert_eq!(
        data.get("text"),
        Some(
            &format!(
                "Background subagent started as task {}.",
                task_id.to_utf8().unwrap()
            )
            .as_str()
            .into()
        )
    );
    assert!(data.get("turns").is_none());
    server.request().await;
    let task = resources
        .subagents
        .get(&task_id)
        .expect("gateway background owner must retain launched task");
    assert_eq!(task.status, TaskStatus::Running);
    assert_eq!(task.session_id, "agent-parent".into());
    assert_eq!(task.subagent_type, "background-proof".into());
    assert_eq!(
        task.subagent_id,
        *result
            .get("metadata")
            .unwrap()
            .get("subagentId")
            .unwrap()
            .as_string()
            .unwrap()
    );
    cancel.abort();
    assert_eq!(
        resources.subagents.get(&task_id).unwrap().status,
        TaskStatus::Running,
        "background lifetime belongs to gateway owner, not parent turn cancellation"
    );
    assert!(
        timeout(
            Duration::from_secs(5),
            resources.subagents.stop_and_wait(&task_id)
        )
        .await
        .unwrap()
    );
    server.assert_disconnected().await;
    assert_eq!(
        resources.subagents.get(&task_id).unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    let drained = events.drain();
    assert!(
        drained
            .iter()
            .any(|event| event.get("type") == Some(&"subagent_started".into())),
        "background fork body must run the real child loop (OneShotSubagentPort.ts:88-89)"
    );
    assert!(drained.iter().any(|event| event.get("type")
        == Some(&"subagent_completed".into())
        && event.get("success") == Some(&JsValue::Bool(false))));
}

#[tokio::test]
async fn observer_uses_the_gateway_owner_without_tools_and_emits_its_real_report() {
    use crate::runtime::{
        background::{BackgroundResources, SubagentKind, SubagentListFilter, TaskStatus},
        steer::{SteerMailbox, TurnSteers},
    };
    let root = root();
    let forbidden = root.join("observer-must-not-write.txt");
    let server = ModelFixture::start(vec![
        Reply::Text(REPORT.into()),
        Reply::Tool {
            name: "write_file".into(),
            arguments: json!({ "file_path": forbidden, "content": "must not be written" }),
        },
        Reply::Text("Observer report proof".into()),
    ])
    .await;
    let resources = BackgroundResources::default();
    let services = Arc::new(
        crate::cron::native::NativeSessionServices::open(
            &root.join("observer-home"),
            &root,
            &root.to_string_lossy(),
            &"agent-parent".into(),
            None,
            resources.clone(),
        )
        .unwrap(),
    );
    let input = input(
        &root,
        json!({ "subagentDefinitions": {
            "observed-route": {
                "id": "observed",
                "allowedTools": ["read_file"],
                "observer": "auditor-route",
                "observerMessage": "Inspect the observer-proof marker.",
            },
            "auditor-route": {
                "id": "auditor",
                "allowedTools": ["*"],
                "permissionMode": "bypassPermissions",
                "initialPrompt": "Observer initial context must be removed.",
                "background": true,
                "observer": "auditor-route",
            },
        } }),
    );
    let mailbox = SteerMailbox::new(None);
    mailbox.start("agent-parent-turn".into());
    let events = Arc::new(EventBuffer::default());
    let router = module_host::build_with_services_runtime(
        snapshot(&server.url),
        &input,
        events.clone(),
        Cancellation::new(),
        TurnSteers::new(mailbox, "agent-parent-turn".into(), vec![]),
        None,
        Some(services),
    )
    .unwrap();
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Observe bounded work", "prompt": "Return the observer-proof marker.", "subagent_type": "observed-route" }),
    )
    .await;
    assert_success(&result);
    let observed_id = result
        .get("metadata")
        .unwrap()
        .get("subagentId")
        .unwrap()
        .clone();
    let mut emitted = Vec::new();
    let (report, tasks) = timeout(Duration::from_secs(5), async {
        loop {
            emitted.extend(events.drain());
            let tasks = resources.subagents.list(&SubagentListFilter {
                session_id: Some("agent-parent".into()),
                kind: Some(SubagentKind::Observer),
                ..Default::default()
            });
            if let Some(report) = emitted
                .iter()
                .find(|event| event.get("type") == Some(&"observer_report".into()))
                .filter(|_| tasks.len() == 1 && tasks[0].status == TaskStatus::Completed)
            {
                break (report.clone(), tasks);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gateway observer must finish and emit its real model report");
    assert_eq!(report.get("success"), Some(&JsValue::Bool(true)));
    assert_eq!(report.get("report"), Some(&"Observer report proof".into()));
    assert_eq!(report.get("observedSubagentId"), Some(&observed_id));
    assert_eq!(report.get("observedSubagentType"), Some(&"observed".into()));
    assert_eq!(report.get("observerSubagentType"), Some(&"auditor".into()));
    assert_eq!(
        report.get("observerSubagentId"),
        Some(&JsValue::String(tasks[0].subagent_id.clone()))
    );
    assert_eq!(tasks[0].parent_turn_id, "agent-parent-turn".into());
    assert_eq!(
        tasks[0].observed_subagent_id.as_ref(),
        observed_id.as_string()
    );
    assert!(!forbidden.exists(), "observer must not execute writes");
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(tool_names(&requests[1]).is_empty());
    assert!(tool_names(&requests[2]).is_empty());
    let digest = requests[1]["messages"].to_string();
    for marker in [
        "You are an observer. Do not execute the observed task",
        "Return the observer-proof marker.",
        "native child proof",
        "Inspect the observer-proof marker.",
    ] {
        assert!(digest.contains(marker), "missing {marker}: {digest}");
    }
    assert!(!digest.contains("Observer initial context must be removed."));
    assert!(requests[2]["messages"].to_string().contains("TOOL_ERROR"));
    assert_eq!(
        emitted
            .iter()
            .filter(|event| event.get("type") == Some(&"observer_report".into()))
            .count(),
        1,
        "observer definitions must not recursively launch observers"
    );
}

#[tokio::test]
async fn child_inherits_parent_deny_rules_and_cannot_write_the_file() {
    let root = root();
    let path = root.join("denied.txt");
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "write_file".into(),
            arguments: json!({ "file_path": path, "content": "must not be written" }),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let input = input(
        &root,
        json!({ "permissionContext": {
        "canPrompt": false,
        "rules": { "deny": [{ "source": "session", "behavior": "deny", "toolName": "write_file" }] },
    } }),
    );
    let router = router(
        snapshot(&server.url),
        &input,
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(&router, "agent", json!({ "description": "Check inherited permissions", "prompt": "Return a structured report." })).await;
    assert_success(&result);
    assert!(
        !path.exists(),
        "a child must never bypass a parent deny rule"
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"].as_str().is_some_and(|text| {
                        text.contains("permission_denied") || text.contains("denied")
                    })
            }),
        "child must receive the denied tool result: {}",
        requests[1]
    );
}

#[tokio::test]
async fn read_only_child_cannot_use_allowed_bash_to_mutate_the_workspace() {
    let root = root();
    let forbidden = root.join("must-not-exist");
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "bash".into(),
            arguments: json!({ "command": "mkdir must-not-exist" }),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let router = router(
        snapshot(&server.url),
        &input(&root, json!({})),
        Arc::default(),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Check read only shell", "prompt": "Return a structured report.", "subagent_type": "explore" }),
    )
    .await;
    assert_success(&result);
    assert!(
        !forbidden.exists(),
        "read-only scope must reject destructive bash"
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(tool_names(&requests[0]).contains(&"bash"));
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("TOOL_ERROR"))
            }),
        "blocked shell must return an error to the child: {}",
        requests[1]
    );
}

#[tokio::test]
async fn child_provider_failure_is_reported_as_a_failed_fork() {
    let root = root();
    let server = ModelFixture::start(vec![Reply::ProviderError]).await;
    let events = Arc::new(EventBuffer::default());
    let router = router(
        snapshot(&server.url),
        &input(&root, json!({})),
        events.clone(),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Provider failure", "prompt": "Return a structured report." }),
    )
    .await;
    let error = assert_error(&result, "tool_execution_failed");
    assert!(string(error.get("message").unwrap()).starts_with("agent subagent failed: "));
    assert_eq!(
        error.get("details").unwrap().get("errorCode"),
        Some(&"subagent_execution_failed".into())
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    let emitted = events.drain();
    let completed: Vec<_> = emitted
        .iter()
        .filter(|event| event.get("type") == Some(&"subagent_completed".into()))
        .collect();
    assert_eq!(completed.len(), 1, "{emitted:?}");
    assert_eq!(completed[0].get("success"), Some(&JsValue::Bool(false)));
}

#[tokio::test]
async fn child_permission_request_reaches_parent_stream_and_accepts_shared_coordinator_answer() {
    permission_roundtrip(false).await;
}

#[tokio::test]
async fn nested_child_permission_request_retains_the_root_coordinator_owner() {
    permission_roundtrip(true).await;
}

async fn permission_roundtrip(nested: bool) {
    use crate::runtime::{
        interaction::{GatewayInteractionCoordinator, PermissionDecision},
        steer::{SteerMailbox, TurnSteers},
    };
    let root = root();
    let mut replies = Vec::new();
    if nested {
        replies.push(Reply::Tool { name: "agent".into(), arguments: json!({ "description": "Nested approval", "prompt": "Run the bounded command and report." }) });
    }
    replies.extend([
        Reply::Tool {
            name: "bash".into(),
            arguments: json!({ "command": "printf child-permission-proof" }),
        },
        Reply::Text(REPORT.into()),
    ]);
    if nested {
        replies.push(Reply::Text(REPORT.into()));
    }
    let server = ModelFixture::start(replies).await;
    let mut input = input(
        &root,
        json!({ "maxSubagentDepth": 5, "permissionContext": {
        "canPrompt": true,
        "rules": { "ask": [{ "source": "session", "behavior": "ask", "toolName": "bash" }] },
    } }),
    )
    .as_object()
    .unwrap()
    .clone();
    input.insert("canPrompt".into(), JsValue::Bool(true));
    let interactions = Arc::new(GatewayInteractionCoordinator::default());
    let mailbox = SteerMailbox::new(None);
    mailbox.start("agent-parent-turn".into());
    let events = Arc::new(EventBuffer::default());
    let router = Arc::new(
        module_host::build_with_services_interactions(
            snapshot(&server.url),
            &JsValue::Object(input),
            events.clone(),
            Cancellation::new(),
            TurnSteers::new(mailbox, "agent-parent-turn".into(), vec![]),
            None,
            None,
            Some(interactions.clone()),
        )
        .unwrap(),
    );
    let task = tokio::spawn(async move {
        execute(&router, "agent", json!({ "description": "Approve child command", "prompt": "Return a structured report." })).await
    });
    let request = timeout(Duration::from_secs(5), async {
        loop {
            for event in events.drain() {
                if event.get("type") == Some(&"permission_request".into()) {
                    return event;
                }
            }
            assert!(
                !task.is_finished(),
                "child must wait for the parent's visible permission answer"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child permission request must reach the parent event stream");
    assert_eq!(request.get("sessionId"), Some(&"agent-parent".into()));
    assert_eq!(request.get("turnId"), Some(&"agent-parent-turn".into()));
    assert_eq!(request.get("toolName"), Some(&"bash".into()));
    let request_id = request
        .get("requestId")
        .unwrap()
        .as_string()
        .unwrap()
        .clone();
    assert!(interactions.decide_permission(
        &"agent-parent".into(),
        PermissionDecision {
            request_id,
            decision: "allow_once".into(),
            remember: None,
            reason: None,
        },
        None
    ));
    let result = timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), if nested { 4 } else { 2 });
    let continued = &requests[if nested { 2 } else { 1 }];
    assert!(
        continued["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("child-permission-proof"))
            }),
        "approved bash output must reach the real child loop: {}",
        continued
    );
}

#[tokio::test]
async fn full_fork_accepts_unstructured_reports_and_retries_empty_model_output() {
    let root = root();
    let server = ModelFixture::start(vec![
        Reply::Text("Unstructured child report".into()),
        Reply::Text(String::new()),
        Reply::Text("Recovered child report".into()),
    ])
    .await;
    for expected in ["Unstructured child report", "Recovered child report"] {
        let router = router(
            snapshot(&server.url),
            &input(&root, json!({})),
            Arc::default(),
            Cancellation::new(),
        );
        let result = execute(&router, "agent", json!({ "description": "", "prompt": "" })).await;
        let data = assert_success(&result);
        assert_eq!(data.get("text"), Some(&expected.into()));
        assert!(data.get("parsed").is_none_or(JsValue::is_undefined));
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "empty output must trigger a real retry");
    assert!(
        requests[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "user"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("Your previous response was empty"))
            }),
        "empty output recovery prompt must reach the model: {}",
        requests[2]
    );
}

#[tokio::test]
async fn full_fork_report_fields_use_js_whitespace_for_bom_and_nel() {
    let root = root();
    let bom_report = REPORT.replace(
        "Result: native child proof",
        "Result: \u{feff}trimmed proof\u{feff}",
    );
    let nel_report = REPORT.replace(
        "Result: native child proof",
        "Result: \u{85}preserved proof\u{85}",
    );
    let server = ModelFixture::start(vec![Reply::Text(bom_report), Reply::Text(nel_report)]).await;
    for expected in ["trimmed proof", "\u{85}preserved proof\u{85}"] {
        let router = router(
            snapshot(&server.url),
            &input(&root, json!({})),
            Arc::default(),
            Cancellation::new(),
        );
        let result = execute(
            &router,
            "agent",
            json!({ "description": "Whitespace report", "prompt": "Return a structured report." }),
        )
        .await;
        assert_eq!(
            assert_success(&result).get("parsed").unwrap().get("Result"),
            Some(&expected.into())
        );
    }
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn child_cannot_elicit_even_when_the_parent_has_a_real_question_channel() {
    use crate::runtime::{
        interaction::GatewayInteractionCoordinator,
        steer::{SteerMailbox, TurnSteers},
    };
    let root = root();
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "ask_user_question".into(),
            arguments: json!({ "questions": [{
                "question": "Continue the child?",
                "header": "Child",
                "options": [
                    { "label": "Continue", "description": "Continue the task" },
                    { "label": "Stop", "description": "Stop the task" },
                ],
            }] }),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let mut parent_input = input(
        &root,
        json!({ "permissionMode": "bypassPermissions", "permissionContext": { "canPrompt": true } }),
    )
    .as_object()
    .unwrap()
    .clone();
    parent_input.insert("canPrompt".into(), JsValue::Bool(true));
    parent_input.insert("canElicit".into(), JsValue::Bool(true));
    let mailbox = SteerMailbox::new(None);
    mailbox.start("agent-parent-turn".into());
    let events = Arc::new(EventBuffer::default());
    let router = module_host::build_with_services_interactions(
        snapshot(&server.url),
        &JsValue::Object(parent_input),
        events.clone(),
        Cancellation::new(),
        TurnSteers::new(mailbox, "agent-parent-turn".into(), vec![]),
        None,
        None,
        Some(Arc::new(GatewayInteractionCoordinator::default())),
    )
    .unwrap();
    let result = timeout(
        Duration::from_secs(5),
        execute(
            &router,
            "agent",
            json!({ "description": "No child questions", "prompt": "Return a structured report." }),
        ),
    )
    .await
    .expect("a forbidden child question must fail without waiting for a parent answer");
    assert_success(&result);
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(tool_names(&requests[0]).contains(&"ask_user_question"));
    let tool_reply = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let error = tool_reply["content"].as_str().unwrap();
    assert!(error.contains("unsupported_tool"), "{tool_reply}");
    assert!(
        error.contains("ask_user_question requires a host elicitation channel (none registered)."),
        "{tool_reply}"
    );
    assert!(
        events
            .drain()
            .iter()
            .all(|event| { event.get("type") != Some(&"elicitation_request".into()) })
    );
}

#[tokio::test]
async fn parent_cancellation_stops_child_http_and_emits_failed_completion_once() {
    let root = root();
    let mut server = ModelFixture::start(vec![Reply::Stall]).await;
    let cancel = Cancellation::new();
    let events = Arc::new(EventBuffer::default());
    let router = Arc::new(router(
        snapshot(&server.url),
        &input(&root, json!({})),
        events.clone(),
        cancel.clone(),
    ));
    let pending = tokio::spawn(async move {
        execute(&router, "agent", json!({ "description": "Cancel child request", "prompt": "Return a structured report." })).await
    });
    server.request().await;
    cancel.abort();
    let result = timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    let error = assert_error(&result, "tool_aborted");
    assert_eq!(
        error.get("message"),
        Some(&"agent subagent aborted before completion.".into())
    );
    server.assert_disconnected().await;
    let emitted = events.drain();
    let completed: Vec<_> = emitted
        .iter()
        .filter(|event| event.get("type") == Some(&"subagent_completed".into()))
        .collect();
    assert_eq!(completed.len(), 1, "{emitted:?}");
    assert_eq!(completed[0].get("success"), Some(&JsValue::Bool(false)));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn subagent_timeout_stops_child_http_without_aborting_parent() {
    let root = root();
    let mut server = ModelFixture::start(vec![Reply::Stall, Reply::Stall]).await;
    // Prime the same composed-cancel → HTTP teardown path as parent_cancellation_*.
    let prime = Cancellation::new();
    let prime_router = router(
        snapshot(&server.url),
        &input(&root, json!({})),
        Arc::new(EventBuffer::default()),
        prime.clone(),
    );
    let pending = tokio::spawn(async move {
        execute(
            &prime_router,
            "agent",
            json!({ "description": "Prime cancel path", "prompt": "Return a structured report." }),
        )
        .await
    });
    server.request().await;
    prime.abort();
    let _ = timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    server.assert_disconnected().await;

    let cancel = Cancellation::new();
    let events = Arc::new(EventBuffer::default());
    let router = router(
        snapshot(&server.url),
        &input(&root, json!({ "subagentTimeoutMs": 500 })),
        events.clone(),
        cancel.clone(),
    );
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Timeout child request", "prompt": "Return a structured report." }),
    )
    .await;
    let error = assert_error(&result, "tool_execution_failed");
    assert_eq!(
        error.get("message"),
        Some(&"agent subagent failed: Subagent timed out after 500ms.".into())
    );
    assert_eq!(
        error.get("details").unwrap().get("errorCode"),
        Some(&"subagent_execution_failed".into())
    );
    assert!(
        !cancel.is_aborted(),
        "a child deadline must not cancel its parent"
    );
    server.assert_disconnected().await;
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let emitted = events.drain();
    let completed: Vec<_> = emitted
        .iter()
        .filter(|event| event.get("type") == Some(&"subagent_completed".into()))
        .collect();
    assert_eq!(completed.len(), 1, "{emitted:?}");
    assert_eq!(completed[0].get("success"), Some(&JsValue::Bool(false)));
}

#[tokio::test]
async fn full_fork_persistence_references_resolve_to_the_real_child_transcript() {
    let root = root();
    let proof_path = root.join("durable-proof.txt");
    std::fs::write(&proof_path, "durable native child proof").unwrap();
    let adapter = AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(|key| key.pilot_home.join("chats")),
    );
    let storage = adapter.create_session_storage(SessionStorageInput {
        project_root: root.clone(),
        pilot_home: root.join("isolated-home"),
        session_id: "agent-parent".into(),
        now: None,
    });
    let chat_dir = storage.chat_dir.clone();
    let parent_path = storage.transcript_path.clone();
    let live = LiveSessionState::new(
        "agent-parent".into(),
        root.to_string_lossy().as_ref().into(),
        storage,
    )
    .unwrap();
    live.recorder
        .start_turn(&"agent-parent".into(), &"agent-parent-turn".into())
        .unwrap();
    live.recorder
        .record_model_request(
            &"agent-parent".into(),
            &"agent-parent-turn".into(),
            js(json!({ "provider": "proof", "model": "agent-proof", "messages": [] })),
        )
        .unwrap();
    assert!(
        live.checkpoint
            .tool_context_state()
            .read_file_state
            .lock()
            .unwrap()
            .is_empty()
    );
    let server = ModelFixture::start(vec![
        Reply::Tool {
            name: "read_file".into(),
            arguments: json!({ "file_path": proof_path }),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let events = Arc::new(EventBuffer::default());
    let input = input(&root, json!({}));
    let router = module_host::with_live_state(live.clone(), || {
        router(
            snapshot(&server.url),
            &input,
            events.clone(),
            Cancellation::new(),
        )
    });
    let result = execute(
        &router,
        "agent",
        json!({ "description": "Persist child report", "prompt": "Return a structured report." }),
    )
    .await;
    let data = assert_success(&result);
    let metadata = result.get("metadata").unwrap();
    let child_id = string(metadata.get("subagentId").unwrap());
    assert_eq!(
        data.get("subagentSessionId"),
        Some(
            &format!("{}::sub::{child_id}", root.display())
                .as_str()
                .into()
        )
    );
    assert_eq!(
        data.get("subagentSessionId"),
        metadata.get("subagentSessionId")
    );
    assert_eq!(
        data.get("transcriptRelativePath"),
        metadata.get("transcriptRelativePath")
    );
    let relative = string(data.get("transcriptRelativePath").unwrap());
    assert!(!Path::new(&relative).is_absolute());
    assert!(
        !Path::new(&relative)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    );
    let child_path = chat_dir.join(&relative);
    assert!(
        child_path.is_file(),
        "advertised transcript must exist: {}",
        child_path.display()
    );
    let child = crate::storage::read_transcript(&child_path).unwrap();
    assert!(
        !child
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error"),
        "{:?}",
        child.diagnostics
    );
    for kind in [
        "turn_started",
        "model_request",
        "model_stream_event",
        "tool_call",
        "tool_result",
        "turn_result",
    ] {
        assert!(
            child
                .entries
                .iter()
                .any(|entry| entry.get("type") == Some(&kind.into())),
            "missing {kind}: {:?}",
            child.entries
        );
    }
    assert_eq!(
        child
            .entries
            .iter()
            .filter(|entry| entry.get("type") == Some(&"turn_result".into()))
            .count(),
        1
    );
    assert!(encoded(&JsValue::Array(child.entries)).contains("native child proof"));
    assert!(
        live.checkpoint
            .tool_context_state()
            .read_file_state
            .lock()
            .unwrap()
            .is_empty(),
        "child reads must update a detached checkpoint, never its parent's cache"
    );
    let parent = crate::storage::read_transcript(&parent_path).unwrap();
    for kind in ["subagent_started", "subagent_completed", "tool_result"] {
        assert!(
            parent
                .entries
                .iter()
                .any(|entry| entry.get("type") == Some(&kind.into())),
            "missing parent {kind}: {:?}",
            parent.entries
        );
    }
}

#[tokio::test]
async fn production_one_shot_holds_provider_run_lease_during_agent_fork() {
    use crate::runtime::session::subagent_continuation::{
        NamedContinuationProviderRegistry, NativeContinuationProvider,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let registry = Arc::new(NamedContinuationProviderRegistry::new());
    let registration = registry
        .register("pilotdeck-native", Arc::new(NativeContinuationProvider))
        .unwrap();
    let saw_lease = Arc::new(AtomicBool::new(false));
    let saw = saw_lease.clone();
    let reg = registration.clone();
    let watcher = tokio::spawn(async move {
        while !saw.load(Ordering::Acquire) {
            if reg.in_flight() > 0 {
                saw.store(true, Ordering::Release);
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    let root = root();
    let server = ModelFixture::start(vec![Reply::Text(REPORT.into())]).await;
    let input = input(&root, json!({}));
    let router = module_host::with_subagent_providers(Some(registry), || {
        module_host::build(
            snapshot(&server.url),
            &input,
            Arc::new(EventBuffer::default()),
            Cancellation::new(),
        )
        .unwrap()
    });
    let result = execute(
        &router,
        "agent",
        json!({
            "description": "provider lease proof",
            "prompt": "Return the structured report.",
            "subagent_type": "general-purpose",
        }),
    )
    .await;
    assert_success(&result);
    watcher.abort();
    assert!(
        saw_lease.load(Ordering::Acquire),
        "one-shot run_inner must hold pilotdeck-native run lease for the whole fork"
    );
    assert_eq!(registration.in_flight(), 0);
}
