#[path = "common/oracle_replay.rs"]
mod oracle_replay;
use oracle_replay::*;

#[path = "common/baseline.rs"]
mod baseline;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::tools::{
        ExecutableTool, ToolRunContext,
        builtin::{
            glob::GlobTool, read_file::ReadFileTool, register_read_only_tools,
            time::GetCurrentTimeTool,
        },
        registry::ToolRegistry,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

const ORACLE: &str = include_str!("fixtures/builtin-tools/oracle.json");
const FIXED_NOW: f64 = 1_767_225_600_000.0;

struct ReplayContext {
    context: ToolRunContext,
    now: Arc<Mutex<f64>>,
}
impl ReplayContext {
    fn new(scratch: &Scratch, cwd: &Path) -> Self {
        let now = Arc::new(Mutex::new(FIXED_NOW));
        let clock = {
            let now = now.clone();
            Arc::new(move || JsValue::Number(*now.lock().unwrap()))
        };
        let checkpoint = HostToolCheckpoint::default();
        let permission = object([
            ("mode", JsValue::from("default")),
            ("canPrompt", JsValue::Bool(true)),
            ("additionalWorkingDirectories", JsValue::Array(Vec::new())),
        ]);
        let fields = object([
            ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
            ("permissionMode", JsValue::from("default")),
            ("permissionContext", JsValue::Object(permission)),
            (
                "env",
                JsValue::Object(object([
                    (
                        "PILOT_HOME",
                        JsValue::from(scratch.root.join("pilot-home").to_string_lossy().as_ref()),
                    ),
                    ("HOME", JsValue::from(scratch.text(&scratch.root).as_str())),
                ])),
            ),
        ]);
        let host = ToolRuntimeContext {
            fields,
            files: checkpoint.tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: Some(clock),
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        };
        Self {
            context: ToolRunContext::new(Arc::new(host)),
            now,
        }
    }
    fn set_now(&self, value: f64) {
        *self.now.lock().unwrap() = value;
    }
}

fn validate_value(
    tool_name: &str,
    tool: &dyn ExecutableTool,
    input: &JsValue,
    context: ToolRunContext,
) -> JsValue {
    if tool_name == "read_file" {
        match futures_block(tool.validate_input(input.clone(), context)) {
            Ok(None) => JsValue::Object(object([("ok", JsValue::Bool(true))])),
            Ok(Some(issues)) => JsValue::Object(object([
                ("ok", JsValue::Bool(false)),
                (
                    "issues",
                    JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
                ),
            ])),
            Err(error) => error_value(&error),
        }
    } else {
        JsValue::Null
    }
}

fn tool_for(name: &str) -> Box<dyn ExecutableTool> {
    match name {
        "read_file" => Box::new(ReadFileTool::new()),
        "glob" => Box::new(GlobTool::new()),
        "get_current_time" => Box::new(GetCurrentTimeTool::new()),
        other => panic!("unknown fixture tool {other}"),
    }
}

#[test]
fn builtin_tool_outputs_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 55, "scenario count");
    let mut calls = 0;
    let mut compared = 0;
    for scenario in scenarios {
        let name = text(field(scenario, "name"));
        let scratch = Scratch::new();
        for outside in optional_array(field(scenario, "outside")) {
            let path = text(field(outside, "path"));
            let absolute = scratch.outside.join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&absolute, text(field(outside, "content"))).unwrap();
        }
        let cwd = match field(scenario, "cwd").as_string() {
            Some(cwd) => scratch.root.join(cwd.to_utf8().unwrap()),
            None => scratch.root.clone(),
        };
        let mut replay = ReplayContext::new(&scratch, &cwd);
        let calls_value = array(field(scenario, "calls"));
        for step in array(field(scenario, "steps")) {
            if text(field(step, "op")) != "call" {
                apply_step(step, &scratch).unwrap_or_else(|e| panic!("{name}: setup: {e}"));
                continue;
            }
            let index = number(field(step, "index")).expect("call index") as usize;
            let call = &calls_value[index];
            if let Some(context) = field(call, "context").as_object() {
                for (key, value) in context.entries() {
                    replay.context.fields.insert(key.clone(), value.clone());
                }
            }
            calls += 1;
            let tool_name = text(field(call, "tool"));
            let input = substitute(field(call, "input"), &scratch);
            let tool = tool_for(&tool_name);
            replay.set_now(number(field(call, "now")).unwrap_or(FIXED_NOW));
            let actual = validate_value(&tool_name, tool.as_ref(), &input, replay.context.clone());
            assert_eq!(
                normalize(&actual, &scratch),
                wire(field(call, "validate")),
                "{name}: validate"
            );
            let Some(expected) = expected_execute(call) else {
                continue;
            };
            compared += 1;
            let actual = match futures_block(tool.execute(input, replay.context.clone())) {
                Ok(output) => output_value(output),
                Err(error) => error_value(&error),
            };
            assert_eq!(
                normalize(&actual, &scratch),
                wire(expected),
                "{name}: execute"
            );
        }
    }
    assert_eq!(calls, 63, "call count");
    assert_eq!(compared, 60, "compared call count");
}

fn multimodal(input: &[&str], image_detail: Option<&str>) -> JsValue {
    let mut value = object([(
        "input",
        JsValue::Array(
            input
                .iter()
                .map(|kind| JsValue::from(*kind))
                .collect::<Vec<_>>(),
        ),
    )]);
    if let Some(detail) = image_detail {
        value.insert("imageDetail".into(), JsValue::from(detail));
    }
    JsValue::Object(value)
}

#[test]
fn multimodal_success_branches_emit_model_visible_payloads() {
    let scratch = Scratch::new();
    let mut replay = ReplayContext::new(&scratch, &scratch.root);

    fs::write(scratch.root.join("doc.pdf"), b"not a pdf").unwrap();
    replay
        .context
        .fields
        .insert("modelMultimodal".into(), multimodal(&["pdf"], None));
    let output = futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("doc.pdf"))])),
        replay.context.clone(),
    ))
    .unwrap();
    let supplemental = output.supplemental_messages.expect("PDF supplemental");
    assert_eq!(supplemental.len(), 1);
    let message = &supplemental[0];
    assert_eq!(text(field(message, "role")), "user");
    assert_eq!(field(message, "isMeta"), &JsValue::Bool(true));
    let content = &array(field(message, "content"))[0];
    assert_eq!(text(field(content, "type")), "pdf");
    assert_eq!(text(field(content, "mimeType")), "application/pdf");
    assert_eq!(text(field(content, "data")), BASE64.encode(b"not a pdf"));
    assert_eq!(field(content, "bytes"), &JsValue::Number(9.0));

    let jpeg = [vec![b'A'; 1024], vec![0xff, 0xd9]].concat();
    fs::write(scratch.root.join("pic.jpg"), &jpeg).unwrap();
    replay.context.fields.insert(
        "modelMultimodal".into(),
        multimodal(&["image"], Some("low")),
    );
    let output = futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("pic.jpg"))])),
        replay.context.clone(),
    ))
    .unwrap();
    let content = &output.content[0];
    assert_eq!(text(field(content, "type")), "image");
    assert_eq!(text(field(content, "mimeType")), "image/jpeg");
    assert_eq!(text(field(content, "data")), BASE64.encode(&jpeg));
    assert_eq!(field(content, "bytes"), &JsValue::Number(jpeg.len() as f64));
    assert_eq!(text(field(content, "detail")), "low");
    assert!(output.supplemental_messages.is_none());
}

#[test]
fn builtin_family_registers_as_read_only_catalog_entries() {
    let registry = ToolRegistry::default();
    let registrations = register_read_only_tools(&registry).expect("register builtins");
    assert_eq!(registrations.len(), 4);
    let names: Vec<String> = registry
        .list()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| text(field(&JsValue::Object(tool.fields().clone()), "name")))
                .collect()
        })
        .expect("list builtins");
    let mut names = names;
    names.sort();
    assert_eq!(
        names,
        vec!["get_current_time", "glob", "grep", "read_file"],
        "registered names"
    );
    assert!(registry.get(&JsString::from("Read")).is_some(), "alias");

    let tools = pilotdeck_gateway::runtime::tools::builtin::read_only_tools();
    for tool in tools {
        let empty = JsValue::Object(JsObject::new());
        let name = text(field(&JsValue::Object(tool.fields().clone()), "name"));
        assert_eq!(
            tool.is_read_only(&empty).unwrap(),
            JsValue::Bool(true),
            "{name} read-only"
        );
        let expected_safe = name != "read_file";
        assert_eq!(
            tool.is_concurrency_safe(&empty).unwrap(),
            JsValue::Bool(expected_safe),
            "{name} concurrency"
        );
    }
}
