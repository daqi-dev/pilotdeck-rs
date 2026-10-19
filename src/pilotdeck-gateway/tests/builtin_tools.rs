#[path = "common/baseline.rs"]
mod baseline;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::tools::{
        ExecutableTool, ToolOutput, ToolRunContext,
        builtin::{
            glob::GlobTool, read_file::ReadFileTool, register_read_only_tools,
            time::GetCurrentTimeTool,
        },
        registry::ToolRegistry,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

const ORACLE: &str = include_str!("fixtures/builtin-tools/oracle.json");
const FIXED_NOW: f64 = 1_767_225_600_000.0;

static SCRATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    root: PathBuf,
    outside: PathBuf,
}
impl Scratch {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        let unique = format!(
            "pdrs-builtin-tools-{}-{}",
            std::process::id(),
            SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let root = base.join(&unique);
        let outside = base.join(format!("{unique}-outside"));
        for path in [&root, &outside] {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    panic!("scratch path already exists: {}", path.display())
                }
                Err(error) => panic!("create {}: {error}", path.display()),
            }
        }
        Self { root, outside }
    }
    fn text(&self, path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let base = fs::canonicalize(std::env::temp_dir()).expect("canonical temp");
        for path in [&self.outside, &self.root] {
            let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            assert!(
                canonical.starts_with(&base),
                "refusing to remove {} outside {}",
                canonical.display(),
                base.display()
            );
            let _ = fs::remove_dir_all(canonical);
        }
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue) -> &JsString {
    value.as_string().expect("fixture string")
}
fn text(value: &JsValue) -> String {
    string(value).to_utf8().expect("fixture UTF-8 text")
}
fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("fixture array"),
    }
}
fn optional_array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => &[],
    }
}
fn object(entries: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}
fn wire(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}

/// Wire inputs carry `<root>` / `<outside>` placeholders so the oracle is
/// independent of the run's mkdtemp name.
fn substitute(value: &JsValue, scratch: &Scratch) -> JsValue {
    match value {
        JsValue::String(value) => {
            let value = value
                .to_utf8()
                .unwrap()
                .replace("<outside>", &scratch.text(&scratch.outside))
                .replace("<root>", &scratch.text(&scratch.root));
            JsValue::from(value.as_str())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| substitute(value, scratch))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                result.insert(key.clone(), substitute(value, scratch));
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

fn apply_step(step: &JsValue, scratch: &Scratch) -> Result<(), String> {
    let op = text(field(step, "op"));
    let path = field(step, "path")
        .as_string()
        .map(|value| value.to_utf8().unwrap())
        .unwrap_or_default();
    let absolute = scratch.root.join(&path);
    match op.as_str() {
        "file" => {
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let content = if let Some(encoded) = field(step, "base64").as_string() {
                BASE64
                    .decode(encoded.to_utf8().unwrap())
                    .map_err(|e| e.to_string())?
            } else if let Some(repeat) = field(step, "repeat").as_object() {
                let unit = field(&JsValue::Object(repeat.clone()), "unit")
                    .as_string()
                    .ok_or("repeat.unit")?
                    .to_utf8()
                    .unwrap();
                let count = field(&JsValue::Object(repeat.clone()), "count")
                    .as_number()
                    .ok_or("repeat.count")? as usize;
                unit.repeat(count).into_bytes()
            } else {
                text(field(step, "content")).into_bytes()
            };
            fs::write(&absolute, content).map_err(|e| e.to_string())?;
            if let Some(mtime) = number(field(step, "mtime")) {
                let taken = UNIX_EPOCH + Duration::from_millis(mtime as u64);
                fs::OpenOptions::new()
                    .write(true)
                    .open(&absolute)
                    .and_then(|file| file.set_modified(taken))
                    .map_err(|e| e.to_string())?;
            }
        }
        "dir" => fs::create_dir_all(&absolute).map_err(|e| e.to_string())?,
        "symlink" => {
            let target = text(field(step, "target"));
            let target = target.replace("<outside>", &scratch.text(&scratch.outside));
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            symlink(target, &absolute).map_err(|e| e.to_string())?;
        }
        "chmod" => {
            let mode = number(field(step, "mode")).ok_or("chmod mode")? as u32;
            fs::set_permissions(&absolute, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        "sleep" => std::thread::sleep(Duration::from_millis(
            number(field(step, "ms")).ok_or("sleep ms")? as u64,
        )),
        other => return Err(format!("unknown setup step {other}")),
    }
    Ok(())
}

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

fn normalize(value: &JsValue, scratch: &Scratch) -> String {
    wire(value)
        .replace(&scratch.text(&scratch.outside), "<outside>")
        .replace(&scratch.text(&scratch.root), "<root>")
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

fn futures_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn error_value(error: &pilotdeck_gateway::runtime::tools::ToolError) -> JsValue {
    JsValue::Object(object([(
        "error",
        JsValue::Object(object([
            ("code", JsValue::from(error.code.as_str())),
            ("message", JsValue::String(error.message.clone())),
        ])),
    )]))
}

fn output_value(output: ToolOutput) -> JsValue {
    let mut value = object([("content", JsValue::Array(output.content))]);
    if let Some(data) = output.data {
        value.insert("data".into(), data);
    }
    if let Some(metadata) = output.metadata {
        value.insert("metadata".into(), JsValue::Object(metadata));
    }
    JsValue::Object(value)
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
            let expected = field(call, "execute");
            if matches!(expected, JsValue::Null) {
                continue;
            }
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
    assert_eq!(registrations.len(), 3);
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
        vec!["get_current_time", "glob", "read_file"],
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
