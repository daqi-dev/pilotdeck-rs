//! Replay the real-TS `rewindFilesForSdk` oracle: the transcript written by real
//! write tools drives `FileHistoryStore` replay, conflict detection, diff stats,
//! dry run, and the per-file restore.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    runtime::rewind::{RewindFilesInput, rewind_files},
    storage::{
        AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput,
        file_history::FileHistoryClock,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};

const ORACLE: &str = include_str!("fixtures/rewind/oracle.json");
const FIXED_MS: i64 = 1_767_225_600_000;
const SESSION: &str = "session-1";

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("fixture string")
        .to_utf8()
        .expect("fixture UTF-8 text")
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

/// Replace the run-specific project root in strings and object keys.
fn normalize(value: &JsValue, root: &Path) -> JsValue {
    normalize_text(value, &root.to_string_lossy())
}
fn normalize_text(value: &JsValue, root: &str) -> JsValue {
    match value {
        JsValue::String(value) => {
            JsValue::String(value.to_utf8().unwrap().replace(root, "<root>").into())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| normalize_text(value, root))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                let key = key.to_utf8().unwrap().replace(root, "<root>");
                result.insert(JsString::from(key.as_str()), normalize_text(value, root));
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

/// Substitute `<root>` in strings and object keys with the run project root.
fn substitute(value: &JsValue, root: &Path) -> JsValue {
    substitute_text(value, &root.to_string_lossy())
}
fn substitute_text(value: &JsValue, root: &str) -> JsValue {
    match value {
        JsValue::String(value) => {
            JsValue::String(value.to_utf8().unwrap().replace("<root>", root).into())
        }
        JsValue::Array(values) => JsValue::Array(
            values
                .iter()
                .map(|value| substitute_text(value, root))
                .collect(),
        ),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, value) in object.entries() {
                let key = key.to_utf8().unwrap().replace("<root>", root);
                result.insert(JsString::from(key.as_str()), substitute_text(value, root));
            }
            JsValue::Object(result)
        }
        other => other.clone(),
    }
}

fn write_file(path: &Path, base64: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, BASE64.decode(base64).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).unwrap();
}

fn materialize(root: &Path, rows: &[JsValue]) {
    if root.exists() {
        fs::remove_dir_all(root).unwrap();
    }
    fs::create_dir_all(root).unwrap();
    for row in rows {
        write_file(
            &root.join(text(field(row, "name"))),
            &text(field(row, "base64")),
            field(row, "mode").as_number().unwrap_or(0o644 as f64) as u32,
        );
    }
}

fn apply_step(project_root: &Path, step: &JsValue) {
    let relative = text(field(step, "path"));
    let target = project_root.join(&relative);
    match text(field(step, "op")).as_str() {
        "file" => write_file(
            &target,
            &text(field(step, "base64")),
            field(step, "mode").as_number().unwrap_or(0o644 as f64) as u32,
        ),
        "remove" => {
            if target.is_dir() {
                fs::remove_dir_all(&target)
                    .unwrap_or_else(|error| panic!("remove dir {relative}: {error}"));
            } else {
                match fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("remove {relative}: {error}"),
                }
            }
        }
        other => panic!("unknown setup step {other}"),
    }
}

fn walk(root: &Path, prefix: &str, out: &mut Vec<JsValue>) {
    let mut entries: Vec<_> = match fs::read_dir(root) {
        Ok(entries) => entries.map(|entry| entry.unwrap()).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("read {}: {error}", root.display()),
    };
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = if prefix.is_empty() {
            entry.file_name().to_string_lossy().into_owned()
        } else {
            format!("{prefix}/{}", entry.file_name().to_string_lossy())
        };
        let path = entry.path();
        if path.is_dir() {
            walk(&path, &name, out);
        } else {
            let bytes = fs::read(&path).unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            out.push(JsValue::Object(object([
                ("name", JsValue::from(name.as_str())),
                ("base64", JsValue::from(BASE64.encode(bytes).as_str())),
                ("mode", JsValue::Number(mode as f64)),
            ])));
        }
    }
}

fn tree(root: &Path) -> JsValue {
    let mut rows = Vec::new();
    walk(root, "", &mut rows);
    JsValue::Array(rows)
}

fn adapter(pilot_home: &Path) -> AsyncTranscriptAdapter {
    let chat = pilot_home.join("chats");
    AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(move |_key| chat.clone()),
    )
}

fn run_scenario(scenario: &JsValue) {
    let name = text(field(scenario, "name"));
    let scratch = scratch::Scratch::new("rewind");
    let project_root = scratch.path().join("project");
    let pilot_home = scratch.path().join("pilot");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&pilot_home).unwrap();

    let adapter = adapter(&pilot_home);
    let storage = adapter.create_session_storage(SessionStorageInput {
        project_root: project_root.clone(),
        pilot_home: pilot_home.clone(),
        session_id: JsString::from(SESSION),
        now: None,
    });

    let transcript: String = array(field(scenario, "transcript"))
        .iter()
        .map(|entry| format!("{}\n", wire(&substitute(entry, &project_root))))
        .collect();
    fs::create_dir_all(storage.transcript_path.parent().unwrap()).unwrap();
    fs::write(&storage.transcript_path, transcript).unwrap();

    for backup in array(field(scenario, "backups")) {
        write_file(
            &storage.file_history_dir.join(text(field(backup, "name"))),
            &text(field(backup, "base64")),
            field(backup, "mode").as_number().unwrap_or(0o644 as f64) as u32,
        );
    }
    for missing in array(field(scenario, "missingBackups")) {
        match fs::remove_file(storage.file_history_dir.join(text(missing))) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove missing backup: {error}"),
        }
    }

    for file in array(field(scenario, "initialFiles")) {
        write_file(
            &project_root.join(text(field(file, "path"))),
            &text(field(file, "base64")),
            field(file, "mode").as_number().unwrap_or(0o644 as f64) as u32,
        );
    }
    for step in array(field(scenario, "mutations")) {
        apply_step(&project_root, step);
    }
    // The generator captures the exact worktree before the first call, so it
    // is authoritative for edits made by the write tools as well.
    materialize(&project_root, array(field(scenario, "treeBefore")));

    assert_eq!(
        wire(&tree(&project_root)),
        wire(field(scenario, "treeBefore")),
        "{name}: setup tree"
    );
    let backups_before = wire(&tree(&storage.file_history_dir));

    let clock: FileHistoryClock = Arc::new(|| FIXED_MS);
    let session = JsString::from(SESSION);
    for call in array(field(scenario, "calls")) {
        let user_message_id = text(field(call, "userMessageId"));
        let dry_run = field(call, "dryRun").as_bool().unwrap_or(false);
        let result = rewind_files(RewindFilesInput {
            adapter: &adapter,
            project_root: &project_root,
            pilot_home: &pilot_home,
            session_key: &session,
            user_message_id: &user_message_id,
            dry_run,
            now: Some(clock.clone()),
        });
        let normalized = normalize(&result, &project_root);
        assert_eq!(
            wire(&normalized),
            text(field(call, "resultJson")),
            "{name}: {user_message_id} dryRun={dry_run} result"
        );
        assert_eq!(
            wire(&tree(&project_root)),
            wire(field(call, "treeAfter")),
            "{name}: {user_message_id} dryRun={dry_run} tree"
        );
        assert_eq!(
            wire(&tree(&storage.file_history_dir)),
            backups_before,
            "{name}: {user_message_id} dryRun={dry_run} backup tree"
        );
    }
}

#[test]
fn rewind_files_replays_the_ts_oracle() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 14, "fixture scenario count");
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| array(field(scenario, "calls")).len())
            .sum::<usize>(),
        21,
        "fixture call count"
    );
    for scenario in scenarios {
        run_scenario(scenario);
    }
}

#[test]
fn fixture_paths_are_project_relative_or_tokens() {
    // A guard against leaking the generator's scratch path into the oracle.
    let oracle = baseline::oracle(ORACLE);
    let oracle_text = wire(&oracle);
    assert!(
        !oracle_text.contains("pdrs-rewind-"),
        "oracle contains the generator scratch prefix"
    );
    assert_no_absolute_path(&oracle, "oracle");
    let scenarios = array(field(&oracle, "scenarios"));
    for scenario in scenarios {
        let transcript = wire(field(scenario, "transcript"));
        // Malformed entries (missing trackedFileBackups / messageId) carry no
        // paths at all; only a real backup record must use the tokens.
        if transcript.contains(r#""backupFileName":"<backup:"#) {
            assert!(
                transcript.contains("<root>/"),
                "transcript paths and backup names must be tokens"
            );
        }
        let mut trees = vec![field(scenario, "treeBefore")];
        for call in array(field(scenario, "calls")) {
            trees.push(field(call, "treeAfter"));
        }
        for tree in trees {
            for row in array(tree) {
                let name = text(field(row, "name"));
                assert!(!name.starts_with('/'), "tree name must be relative: {name}");
                assert!(!name.split('/').any(|part| part == ".."));
            }
        }
    }
}

/// The scratch root is the only external absolute path the generator can
/// touch, and it is always replaced by `<root>`. Any other absolute path token
/// (a `/` at a string boundary) means normalization leaked a run-specific path.
/// This is platform-independent, unlike matching `/tmp` or `/var/folders`.
fn assert_no_absolute_path(value: &JsValue, location: &str) {
    match value {
        JsValue::String(text) => check_no_absolute_path(&text.to_utf8().unwrap(), location),
        JsValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                assert_no_absolute_path(value, &format!("{location}[{index}]"));
            }
        }
        JsValue::Object(object) => {
            for (key, value) in object.entries() {
                check_no_absolute_path(&key.to_utf8().unwrap(), location);
                assert_no_absolute_path(value, location);
            }
        }
        _ => {}
    }
}

fn check_no_absolute_path(text: &str, location: &str) {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'/' {
            continue;
        }
        let boundary = index == 0
            || matches!(
                bytes[index - 1],
                b' ' | b'\t' | b'\n' | b'\'' | b'"' | b'(' | b'[' | b'{' | b',' | b':'
            );
        // `<root>/...` and `<backup:...>` are the sanctioned tokens.
        if boundary {
            panic!("{location}: absolute path outside a token: {text}");
        }
    }
}

#[test]
fn transcript_directory_reports_node_eisdir() {
    let scratch = scratch::Scratch::new("rewind-transcript-dir");
    let project_root = scratch.path().join("project");
    let pilot_home = scratch.path().join("pilot");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&pilot_home).unwrap();
    let adapter = adapter(&pilot_home);
    let storage = adapter.create_session_storage(SessionStorageInput {
        project_root: project_root.clone(),
        pilot_home: pilot_home.clone(),
        session_id: JsString::from(SESSION),
        now: None,
    });
    fs::create_dir_all(&storage.transcript_path).unwrap();
    let session = JsString::from(SESSION);
    let result = rewind_files(RewindFilesInput {
        adapter: &adapter,
        project_root: &project_root,
        pilot_home: &pilot_home,
        session_key: &session,
        user_message_id: "msg-1",
        dry_run: false,
        now: None,
    });
    // TS readAgentProjectSessionTranscript uses fs.readFile, so a directory at
    // the transcript path surfaces EISDIR and never the Rust "(os error 21)".
    assert_eq!(
        wire(&result),
        r#"{"canRewind":false,"error":"EISDIR: illegal operation on a directory, read"}"#
    );
}

#[test]
fn missing_session_reports_no_checkpoint() {
    let scratch = scratch::Scratch::new("rewind-empty");
    let project_root = scratch.path().join("project");
    let pilot_home = scratch.path().join("pilot");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&pilot_home).unwrap();
    let adapter = adapter(&pilot_home);
    let session = JsString::from(SESSION);
    let result = rewind_files(RewindFilesInput {
        adapter: &adapter,
        project_root: &project_root,
        pilot_home: &pilot_home,
        session_key: &session,
        user_message_id: "msg-1",
        dry_run: false,
        now: None,
    });
    assert_eq!(
        wire(&result),
        r#"{"canRewind":false,"error":"No persisted file checkpoint is available for this session."}"#
    );
}
