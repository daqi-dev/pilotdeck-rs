use super::*;
use crate::host::ModuleError;
use crate::runtime::{
    model::router::{AbortFlag, RouterFuture, RouterModelInvocation},
    project::FamilyResource,
    turn::runner::tail::TitleInput,
};
use crate::{negative_assert, negative_assert_eq};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
};

fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/native-process/oracle.json"
    ))
    .expect("#1071 oracle JSON")
}
fn js(value: &Value) -> JsValue {
    pilotdeck_protocol::parse_json(&value.to_string()).expect("#1071 test JSON")
}
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(
        &pilotdeck_protocol::stringify_json(value)
            .expect("#1071 stringify")
            .expect("#1071 serializable value"),
    )
    .expect("#1071 serialized JSON")
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1071-{}", uuid::Uuid::new_v4()));
        for name in ["home", "project"] {
            std::fs::create_dir_all(root.join(name)).expect("#1071 scratch dirs");
        }
        for name in ["token", "sidecar"] {
            std::fs::write(root.join(name), "fixture").expect("#1071 scratch file");
        }
        Self(root.canonicalize().expect("#1071 scratch canonical path"))
    }
    fn args(&self) -> Vec<OsString> {
        [
            "--home",
            self.0.join("home").to_str().expect("#1071 home UTF8"),
            "--project",
            self.0.join("project").to_str().expect("#1071 project UTF8"),
            "--listen",
            "127.0.0.1:0",
            "--token-file",
            self.0.join("token").to_str().expect("#1071 token UTF8"),
            "--sidecar",
            self.0.join("sidecar").to_str().expect("#1071 sidecar UTF8"),
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_options_and_environment() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let captured = std::panic::catch_unwind(|| {
            capture_environment([(OsString::from("NON_UTF8"), OsString::from_vec(vec![255]))])
        });
        negative_assert!(
            captured.is_ok(),
            "#1071 non UTF-8 environment does not panic"
        );
        negative_assert_eq!(
            captured
                .expect("#1071 captured environment was checked")
                .get("NON_UTF8"),
            Some(&"\u{fffd}".to_owned()),
            "#1071 non UTF-8 environment replacement preserved"
        );
    }
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 valid isolated options");
    negative_assert!(
        native_requested(Some(std::ffi::OsStr::new("--native"))),
        "#1071 explicit native switch"
    );
    for first in [
        None,
        Some(std::ffi::OsStr::new("native")),
        Some(std::ffi::OsStr::new("--native=true")),
        Some(std::ffi::OsStr::new("--help")),
    ] {
        negative_assert!(!native_requested(first), "#1071 A4 remains default");
    }
    for (key, bad) in [
        ("--home", "relative"),
        ("--listen", "0.0.0.0:0"),
        ("--listen", "127.0.0.1:80"),
        ("--drain-ms", "0"),
        ("--drain-ms", "300001"),
        ("--unknown", "x"),
    ] {
        let mut args = scratch.args();
        if let Some(index) = args.iter().position(|v| v == key) {
            args[index + 1] = bad.into();
        } else {
            args.extend([key.into(), bad.into()]);
        }
        negative_assert!(
            Options::parse(args).is_err(),
            "#1071 invalid configuration rejected"
        );
    }
    let mut same = scratch.args();
    same[3] = same[1].clone();
    negative_assert!(Options::parse(same).is_err(), "#1071 separated roots");
    let mut duplicate = scratch.args();
    duplicate.extend(["--home".into(), scratch.0.join("home").into_os_string()]);
    negative_assert!(
        Options::parse(duplicate).is_err(),
        "#1071 duplicate option rejected"
    );
    negative_assert!(
        Options::parse(Vec::<OsString>::new()).is_err(),
        "#1071 required authority"
    );
    let inherited = Env::from_iter([
        ("HOME".into(), "outside".into()),
        ("PILOT_HOME".into(), "outside".into()),
        ("PILOTDECK_CONFIG_PATH".into(), "outside.yaml".into()),
        ("PILOTDECK_PROXY_UPSTREAM".into(), "outside".into()),
        ("PILOTDECK_AGENT_LOOP_TCP_HOST".into(), "outside".into()),
        ("PILOTDECK_AGENT_LOOP_TCP_PORT".into(), "99".into()),
    ]);
    let env = options.environment(inherited);
    negative_assert_eq!(
        env.get("HOME"),
        options.home.to_str().map(|v| v.to_owned()).as_ref(),
        "#1071 isolated HOME"
    );
    negative_assert_eq!(
        env.get("PILOT_HOME"),
        env.get("HOME"),
        "#1071 isolated PILOT_HOME"
    );
    for key in [
        "PILOTDECK_CONFIG_PATH",
        "PILOTDECK_PROXY_UPSTREAM",
        "PILOTDECK_AGENT_LOOP_TCP_HOST",
        "PILOTDECK_AGENT_LOOP_TCP_PORT",
    ] {
        negative_assert!(!env.contains_key(key), "#1071 inherited authority removed");
    }
    for key in [
        "PILOTDECK_MANAGED_CONFIG",
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
    ] {
        negative_assert!(
            validate_environment(&Env::from_iter([(key.into(), "outside".into())])).is_err(),
            "#1071 ambient state refused"
        );
    }
    negative_assert!(
        validate_environment(&Env::new()).is_ok(),
        "#1071 clean environment accepted"
    );
}

#[tokio::test]
async fn default_lsp_matches_ts() {
    let service = families::DefaultLsp::default();
    let bound = service.bind().expect("#1071 LSP bind");
    for (name, service, workspace, path, line) in [
        ("empty", &service, "/isolated/project", "file.ts", 0.0),
        ("bound", &bound, "/isolated/project", "file.ts", 0.0),
        ("workspace", &service, "", "file.ts", 0.0),
        ("position", &service, "/isolated/project", "file.ts", -1.0),
        ("escape", &service, "/isolated/project", "../file.ts", 0.0),
        ("dot", &service, "/isolated/project", "dir.ts/.", 0.0),
        ("parent", &service, "/isolated/project", "dir.ts/..", 0.0),
    ] {
        let error = crate::test_negative_receipt::expect_err(
            service.query(workspace, path, line, 0.0),
            "#1071 LSP query refuses unavailable",
        );
        let detail = error.js_error().expect("#1071 LSP typed error");
        let actual = json!({"name":detail.name,"code":detail.code,"message":String::from_utf16_lossy(detail.message.units())});
        let fixture = oracle();
        let expected = fixture["observations"]["lsp"]
            .as_array()
            .expect("#1071 LSP rows")
            .iter()
            .find(|r| r["name"] == name)
            .expect("#1071 LSP case");
        negative_assert_eq!(actual, expected["error"], "#1071 LSP oracle");
    }
    service.dispose().await.expect("#1071 LSP dispose");
    service.dispose().await.expect("#1071 LSP dispose twice");
    negative_assert!(service.bind().is_err(), "#1071 disposed LSP rejects bind");
    negative_assert!(
        bound
            .query("/isolated/project", "file.ts", 0.0, 0.0)
            .expect_err("#1071 bound empty LSP")
            .js_error()
            .expect("#1071 bound typed error")
            .code
            .as_ref()
            .is_some_and(|v| v == "LSP_UNAVAILABLE"),
        "#1071 bound LSP independent lifetime"
    );
}

struct Model {
    response: JsValue,
    calls: Arc<Mutex<Vec<JsValue>>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    entered: Arc<tokio::sync::Notify>,
    abort: Arc<Mutex<Option<AbortFlag>>>,
    reject: bool,
}
impl Default for Model {
    fn default() -> Self {
        Self {
            response: JsValue::Undefined,
            calls: Arc::default(),
            gate: None,
            entered: Arc::default(),
            abort: Arc::default(),
            reject: false,
        }
    }
}
impl RouterModelInvocation for Model {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn complete(&self, request: JsValue, abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        self.calls
            .lock()
            .expect("#1071 model calls lock")
            .push(request);
        *self.abort.lock().expect("#1071 model abort lock") = abort;
        let gate = self.gate.clone();
        let entered = self.entered.clone();
        let response = self.response.clone();
        let reject = self.reject;
        Box::pin(async move {
            entered.notify_one();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if reject {
                Err(crate::runtime::model::router::RouterInvokeError::other(
                    "cancelled",
                ))
            } else {
                Ok(response)
            }
        })
    }
}
fn title_input(text: JsString) -> TitleInput {
    TitleInput {
        text,
        session_id: "session".into(),
        turn_id: "turn".into(),
        message_sequences: vec![],
        signal: Default::default(),
        model: None,
    }
}
#[tokio::test]
async fn title_matches_ts_requests_and_results() {
    let fixture = oracle();
    for (name, text, content, override_model) in [
        ("empty", " \n\t ".to_owned(), json!([]), None),
        (
            "normal",
            "  中文\n请求  ".to_owned(),
            json!([{"type":"text","text":"```json\n{\"title\":\"  标题  内容  \"}\n```"}]),
            None,
        ),
        (
            "override",
            "a".repeat(1201),
            json!([{"type":"text","text":json!({"title":"b".repeat(81)}).to_string()}]),
            Some(js(&json!({"provider":"other","model":"chosen"}))),
        ),
        (
            "invalid",
            "request".to_owned(),
            json!([{"type":"text","text":"invalid json"}]),
            None,
        ),
    ] {
        let model = Arc::new(Model {
            response: js(&json!({"content":content})),
            ..Default::default()
        });
        let provider = families::title_provider(
            model.clone(),
            js(&json!({"provider":"fixture","model":"model"})),
            "zh-CN".into(),
            Duration::from_secs(30),
        );
        let mut input = title_input(text.into());
        input.model = override_model;
        let result = (provider.generate)(input)
            .await
            .expect("#1071 title generate");
        let mut calls: Vec<Value> = model
            .calls
            .lock()
            .expect("#1071 calls observed")
            .iter()
            .map(json_value)
            .collect();
        for call in &mut calls {
            let prompt = call
                .as_object_mut()
                .expect("#1071 request object")
                .remove("systemPrompt")
                .expect("#1071 title prompt");
            call["systemPromptSha256"] = json!(format!(
                "{:x}",
                Sha256::digest(prompt.as_str().expect("#1071 prompt string").as_bytes())
            ));
        }
        let expected = fixture["observations"]["title"]
            .as_array()
            .expect("#1071 title rows")
            .iter()
            .find(|r| r["name"] == name)
            .expect("#1071 title row");
        negative_assert_eq!(
            json!(result.map(|s| String::from_utf16_lossy(s.units()))),
            expected["result"],
            "#1071 title result oracle"
        );
        negative_assert_eq!(
            json!(calls),
            expected["calls"],
            "#1071 title request oracle"
        );
    }
}
#[tokio::test(start_paused = true)]
async fn title_waits_for_provider_after_abort() {
    for mode in [
        "cancel-release",
        "timeout-release",
        "cancel-reject",
        "cancel-never",
    ] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let model = Arc::new(Model {
            response: js(&json!({"content":[{"type":"text","text":"{\"title\":\"late title\"}"}]})),
            gate: Some(gate.clone()),
            reject: mode == "cancel-reject",
            ..Default::default()
        });
        let provider = families::title_provider(
            model.clone(),
            JsValue::Undefined,
            "en".into(),
            Duration::from_millis(1),
        );
        let input = title_input("hello".into());
        let cancel = input.signal.clone();
        let task = tokio::spawn((provider.generate)(input));
        tokio::time::timeout(Duration::from_secs(1), model.entered.notified())
            .await
            .expect("#1071 model entered deadline");
        if mode == "timeout-release" {
            tokio::time::advance(Duration::from_millis(2)).await;
        } else {
            cancel.cancel(None);
        }
        tokio::task::yield_now().await;
        let abort = model
            .abort
            .lock()
            .expect("#1071 abort observed")
            .clone()
            .expect("#1071 abort flag");
        negative_assert!(abort.is_aborted(), "#1071 title provider sees abort");
        negative_assert!(!task.is_finished(), "#1071 title waits after abort");
        if mode == "cancel-never" {
            task.abort();
            negative_assert!(
                task.await
                    .expect_err("#1071 never title cancellation")
                    .is_cancelled(),
                "#1071 never provider bounded by owner"
            );
            continue;
        }
        gate.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("#1071 title settle deadline")
            .expect("#1071 title task join")
            .expect("#1071 title provider result");
        let fixture = oracle();
        let expected = fixture["observations"]["titleWait"]
            .as_array()
            .expect("#1071 title wait rows")
            .iter()
            .find(|r| r["kind"] == mode)
            .expect("#1071 title wait row");
        negative_assert_eq!(
            json!(result.map(|s| String::from_utf16_lossy(s.units()))),
            expected["result"],
            "#1071 title wait oracle"
        );
    }
}
#[test]
#[ignore = "requires fixed read-only TS baseline and Node v22.22.0"]
fn regenerates_identically() {
    crate::test_baseline::regenerates_identically("native-process");
}

#[test]
fn fixture_provenance() {
    crate::test_baseline::oracle(include_str!(
        "../../tests/fixtures/native-process/oracle.json"
    ));
}

const CONFIG: &str = "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:0/v1\n      apiKey: mock-only\n      models: {mock: {}}\n";
#[tokio::test]
async fn production_profile_and_registration() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 profile options");
    let env = options.environment(Env::new());
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG).expect("#1071 profile config");
    assembly::validate_profile(&options, &env)
        .await
        .expect("#1071 empty integrations accepted");
    assembly::register_project(&options).expect("#1071 project registration");
    assembly::register_project(&options).expect("#1071 repeated project registration");
    let metadata = crate::metadata::Metadata {
        home: options.home.clone(),
        cwd: options.project.clone(),
        files: crate::metadata::NativeFiles,
    };
    negative_assert!(
        metadata.has_registered_project(&options.project),
        "#1071 project visible to metadata"
    );
    let selection = crate::session_model::execute(
        &metadata,
        &js(&json!({"projectKey":options.project,"sessionKey":"fresh"})),
        &env,
    )
    .expect("#1071 registered session model read");
    negative_assert_eq!(
        selection.get("effective").and_then(|v| v.get("model")),
        Some(&"mock".into()),
        "#1071 isolated model selection"
    );
    for (base, body) in [
        (
            &options.home,
            "{\"mcpServers\":{\"server\":{\"command\":\"must-not-run\"}}}",
        ),
        (&options.project.join(".pilotdeck"), "invalid json"),
    ] {
        std::fs::create_dir_all(base).expect("#1071 MCP parent");
        let file = base.join("mcp.json");
        std::fs::write(&file, body).expect("#1071 MCP configuration");
        negative_assert!(
            assembly::validate_profile(&options, &env).await.is_err(),
            "#1071 unsupported MCP refused"
        );
        std::fs::remove_file(file).expect("#1071 remove MCP fixture");
    }
    let skill = options.project.join(".pilotdeck/skills/fixture");
    std::fs::create_dir_all(&skill).expect("#1071 skill fixture");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported contributions refused"
    );
    std::fs::remove_dir_all(options.project.join(".pilotdeck/skills"))
        .expect("#1071 remove skill fixture");
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        CONFIG.replace(
            "agent: {model: fixture/mock}",
            "agent: {model: fixture/mock, subagents: {}}",
        ),
    )
    .expect("#1071 subagent fixture");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported subagents refused"
    );
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        format!("{CONFIG}modules:\n  skills: {{enabled: false}}\n"),
    )
    .expect("#1071 module fixture");
    use crate::runtime::project::ConfigSource;
    crate::runtime::project::NativeConfigSource::new(
        options.project.clone(),
        env.clone(),
        Some(options.home.clone()),
        None,
    )
    .load(&options.project)
    .expect("#1071 module fixture parses successfully");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported module bindings refused"
    );
}
#[test]
fn tool_policy_and_turn_environment() {
    let lsp = Arc::new(families::DefaultLsp::default());
    let all = tools::native_tools(None, None, lsp.clone())
        .list()
        .expect("#1071 native tool catalog");
    negative_assert!(
        all.iter()
            .any(|tool| tool.fields().get("name") == Some(&"read_file".into())),
        "#1071 native read tool installed"
    );
    negative_assert!(
        tools::native_tools(Some(vec![]), None, lsp.clone())
            .list()
            .expect("#1071 restricted catalog")
            .is_empty(),
        "#1071 empty tool allowlist enforced"
    );
    let denied = tools::native_tools(
        Some(vec!["read_file".into()]),
        Some(vec!["read_file".into()]),
        lsp,
    )
    .list()
    .expect("#1071 denied catalog");
    negative_assert!(denied.is_empty(), "#1071 tool denylist enforced");
    let environment = NativeTurnEnvironment(assembly::value([("CAPTURED", "present".into())]));
    let env = environment
        .build(None, &"/project".into(), &"session".into(), &"turn".into())
        .expect("#1071 turn environment");
    negative_assert_eq!(
        env.get("CAPTURED"),
        Some(&"present".into()),
        "#1071 captured turn environment"
    );
    negative_assert_eq!(
        env.get("PILOTDECK_SESSION_ID"),
        Some(&"session".into()),
        "#1071 turn session binding"
    );
    negative_assert!(
        environment
            .build(
                Some(&JsValue::Bool(true)),
                &"/project".into(),
                &"session".into(),
                &"turn".into()
            )
            .is_err(),
        "#1071 invalid turn base refused"
    );
}
