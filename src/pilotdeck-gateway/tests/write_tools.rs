//! Replay the real-TS write-tool / FileHistoryStore oracle.
#![allow(clippy::result_large_err)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    host::{
        HostFuture,
        modules::{
            capability::{
                RuntimeContext, ToolCall, ToolDefinition, ToolExecution, ToolInvocationContext,
            },
            permission::PermissionDecision,
            tool_context::{ExecutionContext, HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::tools::{
        ExecutableTool, SchedulerMode, ToolOutput, ToolRunContext, ToolRuntime,
        builtin::{
            edit_file::EditFileTool, edit_notebook::EditNotebookTool, read_file::ReadFileTool,
            write_file::WriteFileTool,
        },
    },
    storage::file_history::{
        BackupFileName, ExpectedFileState, FileHistoryOptions, FileHistoryStore, RecordedBackup,
        RewindOutcome, SnapshotRecordedEntry, get_backup_file_name, resolve_node_path,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

const ORACLE: &str = include_str!("fixtures/write-tools/oracle.json");
const FIXED_MS: i64 = 1_767_225_600_000;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn js_text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("fixture string")
        .to_utf8()
        .expect("fixture UTF-8 text")
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
        "write_file" => Box::new(WriteFileTool::new()),
        "edit_file" => Box::new(EditFileTool::new()),
        "edit_notebook" => Box::new(EditNotebookTool::new()),
        "read_file" => Box::new(ReadFileTool::new()),
        other => panic!("unknown fixture tool {other}"),
    }
}

fn replace_path(value: &str, root: &Path, outside: &Path) -> String {
    let outside_name = outside
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    value
        .replace(&root.to_string_lossy().into_owned(), "<root>")
        .replace(&outside.to_string_lossy().into_owned(), "<outside>")
        .replace(&outside_name, "<outside>")
}

fn substitute(value: &JsValue, root: &Path, outside: &Path) -> JsValue {
    match value {
        JsValue::String(value) => {
            let text = value
                .to_utf8()
                .unwrap()
                .replace("<root>", &root.to_string_lossy())
                .replace("<outside>", &outside.to_string_lossy());
            JsValue::from(text.as_str())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| substitute(value, root, outside))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                let key = key
                    .to_utf8()
                    .unwrap()
                    .replace("<root>", &root.to_string_lossy())
                    .replace("<outside>", &outside.to_string_lossy());
                result.insert(
                    JsString::from(key.as_str()),
                    substitute(value, root, outside),
                );
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

fn normalize_value(value: &JsValue, root: &Path, outside: &Path) -> JsValue {
    match value {
        JsValue::String(value) => {
            JsValue::from(replace_path(&value.to_utf8().unwrap(), root, outside).as_str())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| normalize_value(value, root, outside))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                let key = replace_path(&key.to_utf8().unwrap(), root, outside);
                result.insert(
                    JsString::from(key.as_str()),
                    normalize_value(value, root, outside),
                );
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

fn normalize_backup_name(name: &str) -> String {
    let Some((hash, version)) = name.split_once("@v") else {
        panic!("unexpected backup name {name}");
    };
    assert_eq!(hash.len(), 16, "backup hash prefix");
    assert!(
        hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "backup hash hex"
    );
    format!("<backup>@v{version}")
}

fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn normalize_sha_for_ids(path: &Path, generated: &[String]) -> String {
    let mut content = fs::read_to_string(path).expect("read ipynb for normalized sha");
    for id in generated {
        content = content.replace(id, "<cellid>");
    }
    hash_text(&content)
}

fn normalize_record(
    record: &JsValue,
    root: &Path,
    outside: &Path,
    generated: &[String],
) -> JsValue {
    let mut record = record.clone();
    if let JsValue::Object(object) = &mut record
        && let Some(JsValue::Object(entry)) = object.get("entry")
    {
        let mut entry = entry.clone();
        if let Some(JsValue::Object(backups)) = entry.get("trackedFileBackups") {
            let mut next = JsObject::new();
            for (path, backup) in backups.entries() {
                let mut backup = backup.clone();
                if let JsValue::Object(backup_object) = &mut backup
                    && let Some(JsValue::String(name)) = backup_object.get("backupFileName")
                {
                    let name = normalize_backup_name(&name.to_utf8().unwrap());
                    backup_object.insert("backupFileName".into(), JsValue::from(name.as_str()));
                }
                next.insert(path.clone(), backup);
            }
            entry.insert("trackedFileBackups".into(), JsValue::Object(next));
        }
        if let Some(JsValue::Object(expected)) = entry.get("expectedFileStates")
            && !generated.is_empty()
        {
            let mut next = JsObject::new();
            for (path, state) in expected.entries() {
                let mut state = state.clone();
                let file = path.to_utf8().unwrap();
                if file.ends_with(".ipynb")
                    && let JsValue::Object(state_object) = &mut state
                    && state_object.get("exists") == Some(&JsValue::Bool(true))
                {
                    let sha = normalize_sha_for_ids(Path::new(&file), generated);
                    state_object.insert("sha256".into(), JsValue::from(sha.as_str()));
                }
                next.insert(path.clone(), state);
            }
            entry.insert("expectedFileStates".into(), JsValue::Object(next));
        }
        object.insert("entry".into(), JsValue::Object(entry));
    }
    normalize_value(&record, root, outside)
}

struct Replay {
    context: ToolRunContext,
    store: Arc<FileHistoryStore>,
    records: Arc<Mutex<Vec<JsValue>>>,
    warnings: Arc<Mutex<Vec<String>>>,
    ops: Vec<JsValue>,
    generated_ids: Vec<String>,
}

impl Replay {
    fn new(root: &Path, scenario: &JsValue) -> Self {
        Self::new_with(root, root, &[], scenario)
    }

    /// Variant for guards that depend on how the caller spells `cwd` (case /
    /// Unicode normalization) or on additional allowed roots.
    fn new_with(root: &Path, cwd: &Path, additional: &[&Path], scenario: &JsValue) -> Self {
        let records = Arc::new(Mutex::new(Vec::new()));
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let backup_dir = match field(scenario, "backupDir").as_string() {
            Some(value) => root.join(value.to_utf8().unwrap()),
            None => root.join(".file-history"),
        };
        let store = Arc::new(FileHistoryStore::new(FileHistoryOptions {
            backup_dir,
            max_file_bytes: number(field(scenario, "maxFileBytes")).map(|value| value as u64),
            max_snapshots: number(field(scenario, "maxSnapshots")).map(|value| value as usize),
            now: Some(Arc::new(|| FIXED_MS)),
            on_snapshot_recorded: Some({
                let records = records.clone();
                Arc::new(move |entry: JsObject, kind: &str| {
                    records.lock().unwrap().push(JsValue::Object(object([
                        ("kind", JsValue::from(kind)),
                        ("entry", JsValue::Object(entry)),
                    ])));
                    Ok(())
                })
            }),
            warn: Some({
                let warnings = warnings.clone();
                Arc::new(move |message: &str| warnings.lock().unwrap().push(message.to_string()))
            }),
        }));
        let checkpoint = HostToolCheckpoint::default();
        let permission = object([
            ("mode", JsValue::from("default")),
            ("canPrompt", JsValue::Bool(true)),
            (
                "additionalWorkingDirectories",
                JsValue::Array(
                    additional
                        .iter()
                        .map(|path| JsValue::from(path.to_string_lossy().as_ref()))
                        .collect(),
                ),
            ),
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
                        JsValue::from(root.join("pilot-home").to_string_lossy().as_ref()),
                    ),
                    ("HOME", JsValue::from(root.to_string_lossy().as_ref())),
                ])),
            ),
            ("messageId", JsValue::from("msg-1")),
        ]);
        let host = ToolRuntimeContext {
            fields: fields.clone(),
            files: checkpoint.tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: Some(store.clone()),
            file_update_notifier: None,
            now: Some(Arc::new(|| JsValue::Number(FIXED_MS as f64))),
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        };
        Self {
            context: ToolRunContext::new(Arc::new(host)),
            store,
            records,
            warnings,
            ops: Vec::new(),
            generated_ids: Vec::new(),
        }
    }

    fn apply_context(&mut self, context: &JsValue) {
        if let JsValue::Object(object) = context {
            for (key, value) in object.entries() {
                self.context.fields.insert(key.clone(), value.clone());
            }
        }
    }

    fn run_call(&mut self, call: &JsValue, root: &Path, outside: &Path) -> (JsValue, JsValue) {
        let tool_name = text(field(call, "tool"));
        let input = substitute(field(call, "input"), root, outside);
        self.apply_context(field(call, "context"));
        let tool = tool_for(&tool_name);
        let validation =
            match futures_block(tool.validate_input(input.clone(), self.context.clone())) {
                Ok(None) => JsValue::Object(object([("ok", JsValue::Bool(true))])),
                Ok(Some(issues)) => JsValue::Object(object([
                    ("ok", JsValue::Bool(false)),
                    (
                        "issues",
                        JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
                    ),
                ])),
                Err(error) => normalize_value(&error_value(&error), root, outside),
            };
        let validation_failed = field(&validation, "ok") != &JsValue::Bool(true);
        if validation_failed {
            return (normalize_value(&validation, root, outside), JsValue::Null);
        }
        let mut output = match futures_block(tool.execute(input.clone(), self.context.clone())) {
            Ok(output) => output_value(output),
            Err(error) => {
                return (
                    normalize_value(&validation, root, outside),
                    normalize_value(&error_value(&error), root, outside),
                );
            }
        };
        self.normalize_notebook_output(&mut output, &input);
        if let JsValue::Object(object) = &mut output
            && let Some(JsValue::Object(metadata)) = object.get("metadata")
        {
            let mut metadata = metadata.clone();
            if metadata.get("mtimeMs").is_some() {
                metadata.insert("mtimeMs".into(), JsValue::from("<mtime>"));
            }
            object.insert("metadata".into(), JsValue::Object(metadata));
        }
        (
            normalize_value(&validation, root, outside),
            normalize_value(&output, root, outside),
        )
    }

    fn normalize_notebook_output(&mut self, output: &mut JsValue, input: &JsValue) {
        let JsValue::Object(root) = output else {
            return;
        };
        let Some(JsValue::Object(data)) = root.get("data") else {
            return;
        };
        let mut data = data.clone();
        if data
            .get("edit_mode")
            .and_then(JsValue::as_string)
            .map(js_text)
            .as_deref()
            != Some("insert")
        {
            return;
        }
        let Some(cell_id) = data
            .get("cell_id")
            .and_then(JsValue::as_string)
            .map(js_text)
        else {
            return;
        };
        let Some(original) = data.get("original_file").and_then(JsValue::as_string) else {
            return;
        };
        let notebook = match parse_json(&original.to_utf8().unwrap()) {
            Ok(JsValue::Object(ref notebook)) => notebook.clone(),
            _ => return,
        };
        let format = notebook
            .get("nbformat")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let minor = notebook
            .get("nbformat_minor")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let supports = format > 4.0 || (format == 4.0 && minor >= 5.0);
        let input_id = field(input, "cell_id").as_string().map(js_text);
        if !supports || input_id.as_deref() == Some(cell_id.as_str()) {
            return;
        }
        let Some(updated) = data
            .get("updated_file")
            .and_then(JsValue::as_string)
            .map(js_text)
        else {
            return;
        };
        let updated = updated.replace(&cell_id, "<cellid>");
        data.insert("cell_id".into(), JsValue::from("<cellid>"));
        data.insert("updated_file".into(), JsValue::from(updated.as_str()));
        root.insert("data".into(), JsValue::Object(data));
        if let Some(JsValue::Array(content)) = root.get("content") {
            let content = content
                .clone()
                .into_iter()
                .map(|mut item| {
                    if let JsValue::Object(object) = &mut item
                        && let Some(JsValue::String(value)) = object.get("text")
                    {
                        let replaced = value.to_utf8().unwrap().replace(&cell_id, "<cellid>");
                        object.insert("text".into(), JsValue::from(replaced.as_str()));
                    }
                    item
                })
                .collect();
            root.insert("content".into(), JsValue::Array(content));
        }
        if let Some(JsValue::Object(metadata)) = root.get("metadata") {
            let mut metadata = metadata.clone();
            metadata.insert("bytesWritten".into(), JsValue::Number(updated.len() as f64));
            root.insert("metadata".into(), JsValue::Object(metadata));
        }
        self.generated_ids.push(cell_id);
    }

    fn make_snapshot(&self, message_id: &str) {
        self.store.make_snapshot(message_id).expect("makeSnapshot");
    }

    fn rewind(&mut self, message_id: &str) {
        let result: RewindOutcome = self.store.rewind(message_id).expect("rewind");
        self.ops.push(JsValue::Object(object([
            ("op", JsValue::from("rewind")),
            ("messageId", JsValue::from(message_id)),
            (
                "result",
                JsValue::Object(object([
                    (
                        "filesChanged",
                        JsValue::Array(
                            result
                                .files_changed
                                .iter()
                                .map(|path| JsValue::from(path.as_str()))
                                .collect(),
                        ),
                    ),
                    (
                        "missing",
                        JsValue::Array(
                            result
                                .missing
                                .iter()
                                .map(|path| JsValue::from(path.as_str()))
                                .collect(),
                        ),
                    ),
                    (
                        "conflicts",
                        JsValue::Array(
                            result
                                .conflicts
                                .iter()
                                .map(|path| JsValue::from(path.as_str()))
                                .collect(),
                        ),
                    ),
                ])),
            ),
        ])));
    }

    fn diff_stats(&mut self, message_id: &str) {
        let stats = self.store.get_diff_stats(message_id).expect("diffStats");
        self.ops.push(JsValue::Object(object([
            ("op", JsValue::from("diff_stats")),
            ("messageId", JsValue::from(message_id)),
            (
                "result",
                JsValue::Object(object([
                    ("filesChanged", JsValue::Number(stats.files_changed as f64)),
                    ("insertions", JsValue::Number(stats.insertions as f64)),
                    ("deletions", JsValue::Number(stats.deletions as f64)),
                ])),
            ),
        ])));
    }
}

fn apply_step(step: &JsValue, root: &Path, outside: &Path) -> Result<(), String> {
    let op = text(field(step, "op"));
    let path = field(step, "path")
        .as_string()
        .map(js_text)
        .unwrap_or_default();
    let absolute = root.join(&path);
    match op.as_str() {
        "file" => {
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let bytes = if let Some(encoded) = field(step, "base64").as_string() {
                BASE64
                    .decode(encoded.to_utf8().unwrap())
                    .map_err(|e| e.to_string())?
            } else {
                text(field(step, "content")).into_bytes()
            };
            fs::write(&absolute, bytes).map_err(|e| e.to_string())?;
            if let Some(mtime) = number(field(step, "mtime")) {
                let taken = UNIX_EPOCH + Duration::from_millis(mtime as u64);
                fs::OpenOptions::new()
                    .write(true)
                    .open(&absolute)
                    .and_then(|file| file.set_modified(taken))
                    .map_err(|e| e.to_string())?;
            }
            let mode = number(field(step, "mode")).map_or(0o644, |mode| mode as u32);
            fs::set_permissions(&absolute, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        "dir" => fs::create_dir_all(&absolute).map_err(|e| e.to_string())?,
        "symlink" => {
            let target = text(field(step, "target"));
            let target = target.replace("<outside>", &outside.to_string_lossy());
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::os::unix::fs::symlink(target, &absolute).map_err(|e| e.to_string())?;
        }
        "hardlink" => {
            let target = root.join(text(field(step, "target")));
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::hard_link(target, &absolute).map_err(|e| e.to_string())?;
        }
        "chmod" => {
            let mode = number(field(step, "mode")).ok_or("chmod mode")? as u32;
            fs::set_permissions(&absolute, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        "remove" => {
            if absolute.is_dir() {
                fs::remove_dir_all(&absolute).map_err(|e| e.to_string())?;
            } else if absolute.exists() {
                fs::remove_file(&absolute).map_err(|e| e.to_string())?;
            }
        }
        other => return Err(format!("unknown setup step {other}")),
    }
    Ok(())
}

fn walk(dir: &Path, prefix: &str, rows: &mut Vec<(String, String, u32)>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut paths: Vec<PathBuf> = entries.map(|entry| entry.unwrap().path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let name = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            walk(&path, &name, rows)?;
        } else {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            rows.push((
                normalize_backup_name(&name),
                BASE64.encode(fs::read(&path).unwrap()),
                mode,
            ));
        }
    }
    Ok(())
}

fn capture_history(replay: &Replay, root: &Path, outside: &Path) -> JsValue {
    let records = replay.records.lock().unwrap().clone();
    let warnings = replay.warnings.lock().unwrap().clone();
    let mut tree = Vec::new();
    walk(replay.store.backup_dir(), "", &mut tree).expect("walk backups");
    tree.sort();
    let mut snapshots = Vec::new();
    for snapshot in replay.store.state().snapshots {
        let tracked = snapshot
            .tracked_file_backups
            .iter()
            .map(|(path, backup)| {
                let mut value = object([
                    ("path", JsValue::from(path.as_str())),
                    (
                        "backupFileName",
                        backup
                            .backup_file_name
                            .as_name()
                            .map(|name| JsValue::from(normalize_backup_name(name).as_str()))
                            .unwrap_or(JsValue::Null),
                    ),
                    ("version", JsValue::Number(backup.version as f64)),
                    (
                        "backupTime",
                        JsValue::from(
                            pilotdeck_transport::system_clock::format_epoch_iso(
                                backup.backup_time_ms,
                            )
                            .as_str(),
                        ),
                    ),
                ]);
                if let Some(mode) = backup.mode {
                    value.insert("mode".into(), JsValue::Number(mode as f64));
                }
                JsValue::Object(value)
            })
            .collect::<Vec<_>>();
        let expected = snapshot
            .expected_file_states
            .iter()
            .map(|(path, state)| {
                let mut value = object([("path", JsValue::from(path.as_str()))]);
                match state {
                    pilotdeck_gateway::storage::file_history::ExpectedFileState::Missing => {
                        value.insert("exists".into(), JsValue::Bool(false));
                    }
                    pilotdeck_gateway::storage::file_history::ExpectedFileState::Present {
                        sha256,
                        mode,
                    } => {
                        value.insert("exists".into(), JsValue::Bool(true));
                        let sha256 = if path.ends_with(".ipynb") && !replay.generated_ids.is_empty()
                        {
                            normalize_sha_for_ids(Path::new(path), &replay.generated_ids)
                        } else {
                            sha256.clone()
                        };
                        value.insert("sha256".into(), JsValue::from(sha256.as_str()));
                        if let Some(mode) = mode {
                            value.insert("mode".into(), JsValue::Number(*mode as f64));
                        }
                    }
                }
                JsValue::Object(value)
            })
            .collect::<Vec<_>>();
        snapshots.push(JsValue::Object(object([
            (
                "messageId",
                JsValue::from(snapshot.message_id.as_deref().unwrap_or_default()),
            ),
            (
                "timestamp",
                JsValue::from(
                    pilotdeck_transport::system_clock::format_epoch_iso(snapshot.timestamp_ms)
                        .as_str(),
                ),
            ),
            ("tracked", JsValue::Array(tracked)),
            ("expected", JsValue::Array(expected)),
        ])));
    }
    normalize_value(
        &JsValue::Object(object([
            (
                "records",
                JsValue::Array(
                    records
                        .iter()
                        .map(|record| {
                            normalize_record(record, root, outside, &replay.generated_ids)
                        })
                        .collect(),
                ),
            ),
            (
                "warnings",
                JsValue::Array(
                    warnings
                        .iter()
                        .map(|message| JsValue::from(replace_path(message, root, outside).as_str()))
                        .collect(),
                ),
            ),
            (
                "ops",
                JsValue::Array(
                    replay
                        .ops
                        .iter()
                        .map(|op| normalize_value(op, root, outside))
                        .collect(),
                ),
            ),
            (
                "tree",
                JsValue::Array(
                    tree.into_iter()
                        .map(|(name, base64, mode)| {
                            JsValue::Object(object([
                                ("name", JsValue::from(name.as_str())),
                                ("base64", JsValue::from(base64.as_str())),
                                ("mode", JsValue::Number(mode as f64)),
                            ]))
                        })
                        .collect(),
                ),
            ),
            ("snapshots", JsValue::Array(snapshots)),
        ])),
        root,
        outside,
    )
}

fn capture_files(scenario: &JsValue, replay: &Replay, root: &Path, outside: &Path) -> JsValue {
    let mut files = Vec::new();
    for entry in array(field(scenario, "files")) {
        let path = text(field(entry, "path"));
        let absolute = if let Some(rest) = path.strip_prefix("<outside>/") {
            outside.join(rest)
        } else {
            root.join(&path)
        };
        match fs::read(&absolute) {
            Ok(mut bytes) => {
                if path.ends_with(".ipynb") && !replay.generated_ids.is_empty() {
                    let mut content = String::from_utf8_lossy(&bytes).into_owned();
                    for id in &replay.generated_ids {
                        content = content.replace(id, "<cellid>");
                    }
                    bytes = content.into_bytes();
                }
                files.push(JsValue::Object(object([
                    ("path", JsValue::from(path.as_str())),
                    ("base64", JsValue::from(BASE64.encode(bytes).as_str())),
                ])));
            }
            Err(_) => files.push(JsValue::Object(object([
                ("path", JsValue::from(path.as_str())),
                ("missing", JsValue::Bool(true)),
            ]))),
        }
    }
    JsValue::Array(files)
}

#[test]
fn write_tools_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 45, "scenario count");
    let mut calls = 0;
    for scenario in scenarios {
        let name = text(field(scenario, "name"));
        let root = scratch::Scratch::new("write-tools");
        let outside = scratch::Scratch::new("write-tools-outside");
        let case = root.path().join("case");
        fs::create_dir(&case).unwrap();
        let mut replay = Replay::new(&case, scenario);
        let calls_value = array(field(scenario, "calls"));
        for step in array(field(scenario, "steps")) {
            match text(field(step, "op")).as_str() {
                "call" => {
                    let index = number(field(step, "index")).expect("call index") as usize;
                    let call = &calls_value[index];
                    calls += 1;
                    let (validate, execute) = replay.run_call(call, &case, outside.path());
                    assert_eq!(
                        wire(&validate),
                        wire(field(call, "validate")),
                        "{name}: validate"
                    );
                    if field(call, "execute").get("error").is_some() {
                        assert_eq!(
                            wire(&execute),
                            wire(field(call, "execute")),
                            "{name}: execute error"
                        );
                    } else if field(call, "execute") == &JsValue::Null {
                        assert_eq!(execute, JsValue::Null, "{name}: unexpected execute");
                    } else {
                        assert_eq!(
                            wire(&execute),
                            wire(field(call, "execute")),
                            "{name}: execute"
                        );
                    }
                }
                "make_snapshot" => replay.make_snapshot(&text(field(step, "messageId"))),
                "rewind" => replay.rewind(&text(field(step, "messageId"))),
                "diff_stats" => replay.diff_stats(&text(field(step, "messageId"))),
                _ => apply_step(step, &case, outside.path())
                    .unwrap_or_else(|e| panic!("{name}: setup: {e}")),
            }
        }
        let expected_files = capture_files(scenario, &replay, &case, outside.path());
        assert_eq!(
            wire(&expected_files),
            wire(field(scenario, "files")),
            "{name}: final files"
        );
        let history = capture_history(&replay, &case, outside.path());
        assert_eq!(
            wire(&history),
            wire(field(scenario, "history")),
            "{name}: file history"
        );
    }
    assert_eq!(calls, 86, "call count");
}

#[test]
fn backup_names_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let names = field(&oracle, "backupFileNames");
    assert_eq!(
        get_backup_file_name("/fixed/project/a.txt", 1),
        text(field(names, "a"))
    );
    assert_eq!(
        get_backup_file_name("/fixed/project/中文 文件.txt", 2),
        text(field(names, "unicode"))
    );
}

#[test]
fn write_tools_register_in_catalog() {
    let registry = pilotdeck_gateway::runtime::tools::registry::ToolRegistry::default();
    let registrations = pilotdeck_gateway::runtime::tools::builtin::register_write_tools(&registry)
        .expect("register write tools");
    assert_eq!(registrations.len(), 3);
    let mut names: Vec<String> = registry
        .list()
        .unwrap()
        .iter()
        .map(|tool| text(field(&JsValue::Object(tool.fields().clone()), "name")))
        .collect();
    names.sort();
    assert_eq!(names, vec!["edit_file", "edit_notebook", "write_file"]);
}

struct AllowAll;
impl PermissionDecision for AllowAll {
    fn decide(
        &self,
        _: Arc<dyn ToolDefinition>,
        _: JsValue,
        _: RuntimeContext,
        _: JsString,
    ) -> HostFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Object(object([("type", JsValue::from("allow"))]))) })
    }
}

/// Drive the write tools through `ToolRuntime` exactly as production does: the
/// scheduler asks the permission port, injects `currentPermissionDecision`,
/// then validates and executes. A direct `execute` call would miss S1.
fn scheduled_execute(replay: &Replay, calls: Vec<(&str, JsValue)>) -> Vec<JsValue> {
    let registry = pilotdeck_gateway::runtime::tools::registry::ToolRegistry::default();
    pilotdeck_gateway::runtime::tools::builtin::register_write_tools(&registry)
        .expect("register write tools");
    let runtime = ToolRuntime::new(registry, Arc::new(AllowAll), SchedulerMode::Sequential);
    let calls = calls
        .into_iter()
        .enumerate()
        .map(|(index, (name, input))| ToolCall {
            id: JsString::from(format!("call-{index}").as_str()),
            name: JsString::from(name),
            input,
        })
        .collect();
    let results = futures_block(runtime.execute_all(
        calls,
        ToolInvocationContext {
            runtime: replay.context.host.clone(),
            execution: Arc::new(ExecutionContext {
                fields: JsObject::new(),
                abort_signal: None,
            }),
        },
    ))
    .expect("scheduler batch");
    results.into_iter().map(JsValue::Object).collect()
}

fn scheduled_error_code(result: &JsValue) -> Option<String> {
    result
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(JsValue::as_string)
        .map(js_text)
}

fn scheduled_error_message(result: &JsValue) -> Option<String> {
    result
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(JsValue::as_string)
        .map(js_text)
}

fn assert_outside_workspace_rejection(result: &JsValue, label: &str) {
    // Scheduler validation turns the resolver's path_not_allowed into a
    // schema issue, so the wire code is invalid_tool_input; the containment
    // message is what distinguishes this from the unread-file error.
    assert_eq!(
        scheduled_error_code(result).as_deref(),
        Some("invalid_tool_input"),
        "{label}: {result:?}"
    );
    assert!(
        scheduled_error_message(result)
            .is_some_and(|message| message.contains("resolves outside the PilotDeck workspace")),
        "{label}: {result:?}"
    );
}

#[test]
fn scheduled_allow_decision_still_contains_symlink_escape() {
    let root = scratch::Scratch::new("write-tools-scheduled-security");
    let outside = scratch::Scratch::new("write-tools-scheduled-security-outside");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    // Two-hop dangling link: hop1 -> hop2 -> <outside>/two-hop.txt.
    std::os::unix::fs::symlink(outside.path().join("two-hop.txt"), case.join("hop2")).unwrap();
    std::os::unix::fs::symlink("hop2", case.join("hop1")).unwrap();
    // Dangling link through a directory symlink that points outside the root.
    std::os::unix::fs::symlink(outside.path(), case.join("outdir")).unwrap();
    std::os::unix::fs::symlink("outdir/via-dir.txt", case.join("via-dir")).unwrap();

    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    let results = scheduled_execute(
        &replay,
        vec![
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("hop1")),
                    ("content", JsValue::from("escaped\n")),
                ])),
            ),
            (
                "edit_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("hop1")),
                    ("old_string", JsValue::from("")),
                    ("new_string", JsValue::from("escaped-edit\n")),
                ])),
            ),
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("via-dir")),
                    ("content", JsValue::from("escaped-dir\n")),
                ])),
            ),
        ],
    );
    for (index, result) in results.iter().enumerate() {
        assert_outside_workspace_rejection(
            result,
            &format!("call {index} must be rejected through the scheduler allow path"),
        );
    }
    assert!(!outside.path().join("two-hop.txt").exists());
    assert!(!outside.path().join("via-dir.txt").exists());
}

#[test]
fn symlink_boundaries_still_reject_escapes() {
    let root = scratch::Scratch::new("write-tools-symlink-boundaries");
    let outside = scratch::Scratch::new("write-tools-symlink-boundaries-outside");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    // cwd itself is a symlink to the real root.
    std::os::unix::fs::symlink(&case, root.path().join("rootlink")).unwrap();
    let replay = Replay::new_with(
        &root.path().join("rootlink"),
        &root.path().join("rootlink"),
        &[],
        &JsValue::Object(JsObject::new()),
    );
    // A dangling link at the edge, a `..` in a link target that walks out
    // through a directory link, and a chain longer than the guard limit.
    std::os::unix::fs::symlink(outside.path().join("dangling.txt"), case.join("dangling")).unwrap();
    fs::create_dir(outside.path().join("deep")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("deep"), case.join("deeplink")).unwrap();
    std::os::unix::fs::symlink("deeplink/../dotdot-escape.txt", case.join("dotdot")).unwrap();
    let chain = case.join("chain41");
    fs::create_dir(&chain).unwrap();
    for index in 0..41 {
        let target = if index + 1 == 41 {
            outside
                .path()
                .join("chain-escape.txt")
                .to_string_lossy()
                .into_owned()
        } else {
            format!("h{}", index + 1)
        };
        std::os::unix::fs::symlink(&target, chain.join(format!("h{index}"))).unwrap();
    }
    let results = scheduled_execute(
        &replay,
        vec![
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("plain.txt")),
                    ("content", JsValue::from("inside\n")),
                ])),
            ),
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("dangling")),
                    ("content", JsValue::from("escaped\n")),
                ])),
            ),
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("dotdot")),
                    ("content", JsValue::from("escaped\n")),
                ])),
            ),
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("chain41/h0")),
                    ("content", JsValue::from("escaped\n")),
                ])),
            ),
        ],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "root symlink must still be writable: {:?}",
        results[0]
    );
    assert_eq!(
        fs::read_to_string(case.join("plain.txt")).unwrap(),
        "inside\n"
    );
    for (index, label) in [(1, "dangling"), (2, "dotdot"), (3, "chain41")] {
        assert!(
            scheduled_error_code(&results[index]).is_some(),
            "{label} must be rejected: {:?}",
            results[index]
        );
        assert!(
            !scheduled_error_message(&results[index])
                .unwrap_or_default()
                .is_empty(),
            "{label}"
        );
    }
    assert!(!outside.path().join("dangling.txt").exists());
    assert!(!outside.path().join("dotdot-escape.txt").exists());
    assert!(!outside.path().join("chain-escape.txt").exists());
}

#[test]
fn scheduled_allow_decision_still_permits_outside_paths() {
    let root = scratch::Scratch::new("write-tools-scheduled-outside");
    let outside = scratch::Scratch::new("write-tools-scheduled-outside-target");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    let target = outside.path().join("allowed.txt");
    let results = scheduled_execute(
        &replay,
        vec![(
            "write_file",
            JsValue::Object(object([
                (
                    "file_path",
                    JsValue::from(target.to_string_lossy().as_ref()),
                ),
                ("content", JsValue::from("allowed\n")),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "allowed\n");
}

#[test]
fn symlink_negative_reads_before_swap_and_needs_the_guard() {
    let root = scratch::Scratch::new("write-tools-symlink-swap");
    let outside = scratch::Scratch::new("write-tools-symlink-swap-outside");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    let target = case.join("swap.txt");
    let secret = outside.path().join("secret.txt");
    fs::write(&target, "secret\n").unwrap();
    fs::write(&secret, "secret\n").unwrap();
    let modified = fs::metadata(&target).unwrap().modified().unwrap();
    fs::File::open(&secret)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("swap.txt"))])),
        replay.context.clone(),
    ))
    .expect("read the file before it becomes a symlink");
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(&secret, &target).unwrap();

    // The read snapshot is fresh (same hash and mtime), so only the containment
    // guard can stop this write. Removing the guard makes this assert fail
    // because the outside file is overwritten.
    let results = scheduled_execute(
        &replay,
        vec![(
            "write_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("swap.txt")),
                ("content", JsValue::from("hijacked\n")),
            ])),
        )],
    );
    assert_outside_workspace_rejection(
        &results[0],
        "symlink escape must be rejected after a real read",
    );
    assert_eq!(fs::read_to_string(&secret).unwrap(), "secret\n");
}

#[test]
fn hardlink_edits_match_ts_allowance() {
    let root = scratch::Scratch::new("write-tools-hardlink");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    fs::write(case.join("doc.txt"), "inside\n").unwrap();
    fs::hard_link(case.join("doc.txt"), case.join("alias.txt")).unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("alias.txt"))])),
        replay.context.clone(),
    ))
    .expect("read hard-linked alias");
    let results = scheduled_execute(
        &replay,
        vec![(
            "edit_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("alias.txt")),
                ("old_string", JsValue::from("inside")),
                ("new_string", JsValue::from("edited")),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(
        fs::read_to_string(case.join("doc.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        fs::read_to_string(case.join("alias.txt")).unwrap(),
        "edited\n"
    );
}

#[test]
fn hardlinked_notebook_is_editable() {
    let root = scratch::Scratch::new("write-tools-hardlink-notebook");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    let notebook = r#"{"nbformat":4,"nbformat_minor":4,"metadata":{},"cells":[{"cell_type":"code","source":"print(1)","metadata":{},"outputs":[],"execution_count":null}]}"#;
    fs::write(case.join("book.ipynb"), notebook).unwrap();
    fs::hard_link(case.join("book.ipynb"), case.join("alias.ipynb")).unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("alias.ipynb"))])),
        replay.context.clone(),
    ))
    .expect("read hard-linked notebook");
    let results = scheduled_execute(
        &replay,
        vec![(
            "edit_notebook",
            JsValue::Object(object([
                ("notebook_path", JsValue::from("alias.ipynb")),
                ("cell_id", JsValue::from("cell-0")),
                ("new_source", JsValue::from("print(2)")),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    let updated = fs::read_to_string(case.join("book.ipynb")).unwrap();
    assert!(updated.contains("print(2)"), "{updated}");
    assert_eq!(
        fs::read_to_string(case.join("alias.ipynb")).unwrap(),
        updated
    );
}

#[test]
fn edit_file_expands_dollar_replacement_patterns() {
    let root = scratch::Scratch::new("write-tools-dollar");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    fs::write(case.join("doc.txt"), "A echo PID B\n").unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("doc.txt"))])),
        replay.context.clone(),
    ))
    .expect("read replacement target");
    let results = scheduled_execute(
        &replay,
        vec![(
            "edit_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("doc.txt")),
                ("old_string", JsValue::from("echo PID")),
                ("new_string", JsValue::from("$$|$&|$`|$'|$1|$<n>")),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(
        fs::read_to_string(case.join("doc.txt")).unwrap(),
        "A $|echo PID|A | B\n|$1|$<n> B\n"
    );

    fs::write(case.join("literal.txt"), "x x\n").unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("literal.txt"))])),
        replay.context.clone(),
    ))
    .expect("read replace_all target");
    let results = scheduled_execute(
        &replay,
        vec![(
            "edit_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("literal.txt")),
                ("old_string", JsValue::from("x")),
                ("new_string", JsValue::from("$& $$")),
                ("replace_all", JsValue::Bool(true)),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(
        fs::read_to_string(case.join("literal.txt")).unwrap(),
        "$& $$ $& $$\n"
    );
}

/// B1: the kernel resolves `caseroot` to `CaseRoot`, so containment must use
/// the same real spelling on both sides. The write succeeds in TS; the first
/// implementation rejected every in-root write when cwd spelling differed.
fn alias_cwd_write_and_edit(disk: &Path, alias: &Path, label: &str) {
    fs::write(disk.join("old.txt"), "old\n").unwrap();
    let replay = Replay::new_with(disk, alias, &[], &JsValue::Object(JsObject::new()));
    futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("old.txt"))])),
        replay.context.clone(),
    ))
    .unwrap_or_else(|error| panic!("{label}: read must succeed: {error:?}"));
    let results = scheduled_execute(
        &replay,
        vec![
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("new.txt")),
                    ("content", JsValue::from("new\n")),
                ])),
            ),
            (
                "edit_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("old.txt")),
                    ("old_string", JsValue::from("old")),
                    ("new_string", JsValue::from("edited")),
                ])),
            ),
        ],
    );
    for (index, result) in results.iter().enumerate() {
        assert!(
            scheduled_error_code(result).is_none(),
            "{label}: call {index} must succeed: {result:?}"
        );
    }
    assert_eq!(fs::read_to_string(disk.join("new.txt")).unwrap(), "new\n");
    assert_eq!(
        fs::read_to_string(disk.join("old.txt")).unwrap(),
        "edited\n"
    );
}

#[test]
fn case_insensitive_cwd_spelling_writes_and_edits() {
    let root = scratch::Scratch::new("write-tools-case-cwd");
    let disk = root.path().join("CaseRoot");
    fs::create_dir(&disk).unwrap();
    let alias = root.path().join("caseroot");
    if fs::canonicalize(&alias).is_err() {
        eprintln!("skip: volume is case-sensitive");
        return;
    }
    alias_cwd_write_and_edit(&disk, &alias, "case-insensitive cwd");
}

#[test]
fn nfd_directory_with_nfc_cwd_writes_and_edits() {
    let root = scratch::Scratch::new("write-tools-nfd-cwd");
    let disk = root.path().join("Cafe\u{301}");
    fs::create_dir(&disk).unwrap();
    let alias = root.path().join("Caf\u{e9}");
    if fs::canonicalize(&alias).is_err() {
        eprintln!("skip: volume distinguishes NFC/NFD");
        return;
    }
    alias_cwd_write_and_edit(&disk, &alias, "NFC cwd over NFD directory");
}

#[test]
fn ordinary_io_errors_are_not_reported_as_outside() {
    let root = scratch::Scratch::new("write-tools-io-errors");
    let case = root.path().join("case");
    fs::create_dir(&case).unwrap();
    fs::write(case.join("afile"), "not a directory\n").unwrap();
    std::os::unix::fs::symlink("loopb", case.join("loopa")).unwrap();
    std::os::unix::fs::symlink("loopa", case.join("loopb")).unwrap();
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    let results = scheduled_execute(
        &replay,
        vec![
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("afile/x")),
                    ("content", JsValue::from("x\n")),
                ])),
            ),
            (
                "write_file",
                JsValue::Object(object([
                    ("file_path", JsValue::from("loopa")),
                    ("content", JsValue::from("x\n")),
                ])),
            ),
        ],
    );
    assert_eq!(
        scheduled_error_code(&results[0]).as_deref(),
        Some("tool_execution_failed"),
        "{:?}",
        results[0]
    );
    let notdir = scheduled_error_message(&results[0]).unwrap_or_default();
    assert!(
        notdir.starts_with("ENOTDIR: not a directory, stat '"),
        "{notdir}"
    );
    assert!(!notdir.contains("resolves outside"), "{notdir}");
    assert_eq!(
        scheduled_error_code(&results[1]).as_deref(),
        Some("tool_execution_failed"),
        "{:?}",
        results[1]
    );
    let loop_message = scheduled_error_message(&results[1]).unwrap_or_default();
    assert!(
        loop_message.starts_with("ELOOP: too many symbolic links encountered, stat '"),
        "{loop_message}"
    );
    assert!(!loop_message.contains("resolves outside"), "{loop_message}");
}

#[test]
fn permission_denied_is_not_reported_as_outside() {
    let root = scratch::Scratch::new("write-tools-eacces");
    let case = root.path().join("case");
    let locked = case.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Root bypasses the permission bits; skip rather than assert a different OS
    // behavior than the one Node would report for the same user.
    let denied = fs::symlink_metadata(locked.join("x"))
        .err()
        .is_some_and(|error| error.kind() == io::ErrorKind::PermissionDenied);
    if !denied {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skip: user bypasses mode 000 (running as root)");
        return;
    }
    let replay = Replay::new(&case, &JsValue::Object(JsObject::new()));
    let results = scheduled_execute(
        &replay,
        vec![(
            "write_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("locked/x")),
                ("content", JsValue::from("x\n")),
            ])),
        )],
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        scheduled_error_code(&results[0]).as_deref(),
        Some("tool_execution_failed"),
        "{:?}",
        results[0]
    );
    let message = scheduled_error_message(&results[0]).unwrap_or_default();
    assert!(
        message.starts_with("EACCES: permission denied, stat '"),
        "{message}"
    );
    assert!(!message.contains("resolves outside"), "{message}");
}

#[test]
fn in_root_link_to_another_allowed_root_is_writable() {
    let root = scratch::Scratch::new("write-tools-cross-root");
    let work = root.path().join("work");
    let lib2 = root.path().join("lib2");
    fs::create_dir(&work).unwrap();
    fs::create_dir(&lib2).unwrap();
    std::os::unix::fs::symlink(&lib2, work.join("shared")).unwrap();
    let replay = Replay::new_with(
        &work,
        &work,
        &[lib2.as_path()],
        &JsValue::Object(JsObject::new()),
    );
    let results = scheduled_execute(
        &replay,
        vec![(
            "write_file",
            JsValue::Object(object([
                ("file_path", JsValue::from("shared/x.ts")),
                ("content", JsValue::from("export const x = 1;\n")),
            ])),
        )],
    );
    assert!(
        scheduled_error_code(&results[0]).is_none(),
        "{:?}",
        results[0]
    );
    assert_eq!(
        fs::read_to_string(lib2.join("x.ts")).unwrap(),
        "export const x = 1;\n"
    );
}

#[test]
fn file_history_backup_refuses_preplanted_links() {
    for symlink in [false, true] {
        let root = scratch::Scratch::new("write-tools-backup-link");
        let backups = root.path().join("backups");
        fs::create_dir(&backups).unwrap();
        let victim = root.path().join("victim.txt");
        let file = root.path().join("doc.txt");
        fs::write(&victim, "victim\n").unwrap();
        fs::write(&file, "original\n").unwrap();
        let name = get_backup_file_name(
            &resolve_node_path(file.to_str().unwrap()).expect("#879 resolve absolute backup path"),
            1,
        );
        let backup = backups.join(&name);
        if symlink {
            std::os::unix::fs::symlink(&victim, &backup).unwrap();
        } else {
            fs::hard_link(&victim, &backup).unwrap();
        }
        let store = FileHistoryStore::new(FileHistoryOptions {
            backup_dir: backups,
            max_file_bytes: None,
            max_snapshots: None,
            now: Some(Arc::new(|| FIXED_MS)),
            on_snapshot_recorded: None,
            warn: None,
        });
        assert!(
            store.track_edit(file.to_str().unwrap(), "m1").is_err(),
            "pre-planted {} backup target must be refused",
            if symlink { "symlink" } else { "hard link" }
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "victim\n");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "victim\n");
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
    }
}

#[test]
fn file_history_replay_and_conflict_round_trip() {
    let root = scratch::Scratch::new("write-tools-history-unit");
    let file = root.path().join("a.txt");
    fs::write(&file, "v1\n").unwrap();
    let store = FileHistoryStore::new(FileHistoryOptions {
        backup_dir: root.path().join("backups"),
        max_file_bytes: None,
        max_snapshots: None,
        now: Some(Arc::new(|| FIXED_MS)),
        on_snapshot_recorded: None,
        warn: None,
    });
    store.track_edit(file.to_str().unwrap(), "m1").unwrap();
    let tracked = store.state();
    assert_eq!(tracked.snapshots.len(), 1);
    assert_eq!(tracked.snapshots[0].tracked_file_backups.len(), 1);
    fs::write(&file, "v2\n").unwrap();
    store
        .mark_edit_committed(file.to_str().unwrap(), "m1")
        .unwrap();
    fs::write(&file, "v3\n").unwrap();
    assert_eq!(
        store.get_conflict_paths("m1").unwrap(),
        vec![file.to_string_lossy().into_owned()]
    );

    let replay = FileHistoryStore::new(FileHistoryOptions {
        backup_dir: root.path().join("replay-backups"),
        max_file_bytes: None,
        max_snapshots: None,
        now: Some(Arc::new(|| FIXED_MS)),
        on_snapshot_recorded: None,
        warn: None,
    });
    replay.replay_from_transcript(&[SnapshotRecordedEntry {
        message_id: Some("m2".into()),
        tracked_file_backups: vec![(
            file.to_string_lossy().into_owned(),
            RecordedBackup {
                backup_file_name: BackupFileName::Named("0123456789abcdef@v1".into()),
                version: 1,
                backup_time_ms: FIXED_MS,
                mode: Some(0o100644),
            },
        )],
        expected_file_states: vec![(
            file.to_string_lossy().into_owned(),
            ExpectedFileState::Present {
                sha256: "abc".into(),
                mode: Some(0o644),
            },
        )],
        timestamp_ms: Some(FIXED_MS),
    }]);
    let state = replay.state();
    assert_eq!(state.snapshots.len(), 1);
    assert_eq!(state.snapshots[0].message_id.as_deref(), Some("m2"));
    assert_eq!(
        state.tracked_files,
        vec![file.to_string_lossy().into_owned()]
    );
    assert_eq!(
        state.snapshots[0].expected_file_states[0].1,
        ExpectedFileState::Present {
            sha256: "abc".into(),
            mode: Some(0o644)
        }
    );
}

#[test]
fn resolve_node_path_normalizes_dotdot() {
    assert_eq!(
        resolve_node_path("/tmp/a/../b/./c").expect("#879 resolve dot segments"),
        "/tmp/b/c",
        "#879 normalize dot segments"
    );
    assert_eq!(
        resolve_node_path("/tmp/a/b/../../c").expect("#879 resolve parent segments"),
        "/tmp/c",
        "#879 normalize parent segments"
    );
    assert_eq!(
        resolve_node_path("/").expect("#879 resolve root"),
        "/",
        "#879 retain root"
    );
}
