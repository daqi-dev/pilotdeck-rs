// Replay of the real TS ToolResultBudget + DefaultContextRuntime.applyToolResults
// corpus. Fixture: tests/fixtures/tool-budget/. Every step compares the whole
// JSON result and the spill tree byte-for-byte (no key sorting, no tolerance).
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::host::modules::context::{ContextInput, ContextRuntime};
use pilotdeck_gateway::runtime::context::tool_budget::{ToolResultBudget, ToolResultBudgetOptions};
use pilotdeck_gateway::runtime::context::{DefaultContextRuntime, Options};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use scratch::Scratch;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
const ORACLE: &str = include_str!("fixtures/tool-budget/oracle.json");
const CASES: usize = 21;
const STEPS: usize = 24;
static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("array {value:?}")
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().expect("string").to_utf8().expect("utf8")
}
fn number(value: &JsValue) -> f64 {
    value.as_number().expect("number")
}
fn decode(value: &JsValue, root: &str) -> JsValue {
    match value {
        JsValue::String(value) => {
            let text = value.to_utf8().expect("utf8 fixture string");
            JsValue::String(text.replace("<fixture>", root).into())
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| decode(item, root)).collect())
        }
        JsValue::Object(object) => {
            let mut decoded = JsObject::new();
            for (key, value) in object.entries() {
                decoded.insert(key.clone(), decode(value, root));
            }
            JsValue::Object(decoded)
        }
        value => value.clone(),
    }
}

/// Restore a denied directory before the scratch owner runs its drop.
struct RestoreDirectory(PathBuf);
impl Drop for RestoreDirectory {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn snapshot(root: &Path) -> JsValue {
    let mut entries = Vec::new();
    walk(root, "", &mut entries);
    JsValue::Array(entries)
}

fn walk(root: &Path, relative: &str, entries: &mut Vec<JsValue>) {
    let directory = root.join(relative);
    let mut names: Vec<String> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let absolute = root.join(&path);
        let metadata = fs::symlink_metadata(&absolute).unwrap();
        if metadata.file_type().is_symlink() {
            entries.push(obj(vec![
                ("path", JsValue::String(path.clone().into())),
                ("kind", "symlink".into()),
                (
                    "target",
                    JsValue::String(
                        fs::read_link(&absolute)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                            .into(),
                    ),
                ),
            ]));
        } else if metadata.is_dir() {
            entries.push(obj(vec![
                ("path", JsValue::String(path.clone().into())),
                ("kind", "dir".into()),
            ]));
            walk(root, &path, entries);
        } else {
            let bytes = fs::read(&absolute).unwrap();
            let mut entry = JsObject::new();
            entry.insert("path".into(), JsValue::String(path.clone().into()));
            entry.insert("kind".into(), "file".into());
            entry.insert("size".into(), JsValue::Number(bytes.len() as f64));
            entry.insert(
                "sha256".into(),
                format!("{:x}", Sha256::digest(&bytes)).as_str().into(),
            );
            if let Ok(text) = std::str::from_utf8(&bytes)
                && bytes.len() <= 4096
            {
                entry.insert("text".into(), JsValue::String(text.to_owned().into()));
            }
            entries.push(JsValue::Object(entry));
        }
    }
}

fn obj(entries: Vec<(&'static str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

fn optional_usize(value: Option<&JsValue>) -> Option<usize> {
    value
        .filter(|value| !value.is_undefined())
        .and_then(JsValue::as_number)
        .map(|value| value as usize)
}

#[tokio::test]
async fn real_ts_tool_budget_and_apply_tool_results_replay() {
    let oracle = baseline::oracle(ORACLE);
    assert_eq!(text(field(&oracle, "locale")), "en-US");
    let expected_counts = field(&oracle, "counts");
    assert_eq!(number(field(expected_counts, "cases")) as usize, CASES);
    assert_eq!(number(field(expected_counts, "steps")) as usize, STEPS);
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), CASES, "case count");

    let scratch = Scratch::new("tool-budget");
    let root = scratch.path().to_path_buf();
    let root_string = root.to_string_lossy().into_owned();
    let mut unprivileged = true;
    let mut executed = 0usize;
    let mut skipped = 0usize;

    for case in cases {
        let name = text(field(case, "name"));
        let case_root = root.join(&name);
        fs::create_dir_all(&case_root).unwrap();
        let tool_results_dir =
            PathBuf::from(text(field(case, "toolResultsDir")).replace("<fixture>", &root_string));
        if field(case, "blockingFile").is_undefined() {
            fs::create_dir_all(tool_results_dir.parent().unwrap()).unwrap();
        } else {
            fs::write(
                case_root.join(text(field(case, "blockingFile"))),
                b"not a directory\n",
            )
            .unwrap();
        }
        if let Some(files) = case.get("files") {
            for file in array(files) {
                let path = case_root.join(text(field(file, "path")));
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text(field(file, "text"))).unwrap();
            }
        }
        let denied = case
            .get("deniedDir")
            .and_then(JsValue::as_string)
            .map(|dir| case_root.join(String::from_utf16_lossy(dir.units())));
        let mut probe = None;
        let _restore = denied.clone().map(|path| {
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
            let candidate = path.join(".pdrs-write-probe");
            probe = Some(fs::write(&candidate, b"").is_ok());
            let _ = fs::remove_file(&candidate);
            RestoreDirectory(path)
        });
        if probe == Some(true) {
            unprivileged = false;
        }
        if case.get("requiresUnprivileged").and_then(JsValue::as_bool) == Some(true)
            && !unprivileged
        {
            skipped += array(field(case, "steps")).len();
            continue;
        }

        let budget_options = field(case, "budget");
        let budget = Arc::new(ToolResultBudget::new(ToolResultBudgetOptions {
            tool_results_dir,
            max_result_size_chars: optional_usize(budget_options.get("maxResultSizeChars")),
            max_result_size_tokens: optional_usize(budget_options.get("maxResultSizeTokens")),
            preview_bytes: optional_usize(budget_options.get("previewBytes")),
        }));
        let runtime = DefaultContextRuntime::new(Options {
            tool_result_budget: Some(budget),
            locale: Some("en-US".into()),
            ..Options::default()
        });

        for (index, step) in array(field(case, "steps")).iter().enumerate() {
            let fields = decode(field(step, "input"), &root_string)
                .as_object()
                .expect("input object")
                .clone();
            let result = runtime
                .apply_tool_results(ContextInput {
                    fields,
                    abort_signal: None,
                    budget_evaluator: None,
                })
                .await
                .unwrap_or_else(|error| panic!("case {name} step {index}: {}", error.message));
            let actual = stringify_json(&result)
                .unwrap()
                .unwrap()
                .replace(&root_string, "<fixture>");
            let expected = text(field(step, "outputJson"));
            assert_eq!(actual, expected, "case {name} step {index} output");
            let actual_files = stringify_json(&snapshot(&case_root)).unwrap().unwrap();
            let expected_files = text(field(step, "filesJson"));
            assert_eq!(
                actual_files, expected_files,
                "case {name} step {index} spill tree"
            );
            executed += 1;
        }
    }
    assert_eq!(executed + skipped, STEPS, "executed + skipped steps");
    assert_eq!(executed, STEPS - skipped);
    assert_eq!(skipped, if unprivileged { 0 } else { 1 });
}

#[tokio::test]
async fn spill_rejects_traversal_and_symlink_escapes() {
    let scratch = Scratch::new("tool-budget-security");
    let outside = Scratch::new("tool-budget-outside");
    let dir = scratch.path().join("tool-results/session");
    let budget = ToolResultBudget::new(ToolResultBudgetOptions {
        tool_results_dir: dir.clone(),
        max_result_size_chars: Some(1),
        max_result_size_tokens: Some(100),
        preview_bytes: None,
    });
    // A traversal-shaped id is sanitised by the TS regex; no path escapes.
    let message = obj(vec![
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(vec![obj(vec![
                ("type", "tool_result".into()),
                ("toolCallId", "../../escape".into()),
                (
                    "content",
                    JsValue::Array(vec![obj(vec![
                        ("type", "text".into()),
                        ("text", "payload".into()),
                    ])]),
                ),
            ])]),
        ),
    ]);
    let turn = pilotdeck_protocol::JsString::from("../turn");
    let rewritten = budget
        .apply_to_message(&message, Some(&turn))
        .await
        .unwrap();
    let content = match rewritten.get("content") {
        Some(JsValue::Array(items)) => items,
        _ => panic!("rewritten content is not an array"),
    };
    let path = text(field(&content[0], "path"));
    assert!(
        Path::new(&path).starts_with(&dir),
        "sanitised spill path escaped: {path}"
    );
    assert_eq!(
        foreign_entries(outside.path()),
        0,
        "traversal wrote outside the tool-results directory"
    );

    // The caller-provided directory itself is a symlink: refuse before writing.
    let linked = scratch.path().join("linked");
    std::os::unix::fs::symlink(outside.path(), &linked).unwrap();
    let linked_budget = ToolResultBudget::new(ToolResultBudgetOptions {
        tool_results_dir: linked,
        max_result_size_chars: Some(1),
        max_result_size_tokens: Some(100),
        preview_bytes: None,
    });
    let error = linked_budget
        .apply_to_message(&message, None)
        .await
        .unwrap_err();
    assert!(error.message.contains("EACCES"), "{}", error.message);
    assert_eq!(foreign_entries(outside.path()), 0);

    // A symlinked intermediate directory below the root is refused as well.
    let inner = scratch.path().join("inner");
    fs::create_dir_all(&inner).unwrap();
    std::os::unix::fs::symlink(outside.path(), inner.join("refs")).unwrap();
    let inner_budget = ToolResultBudget::new(ToolResultBudgetOptions {
        tool_results_dir: inner.clone(),
        max_result_size_chars: Some(1),
        max_result_size_tokens: Some(100),
        preview_bytes: None,
    });
    let error = inner_budget
        .apply_to_message(&message, None)
        .await
        .unwrap_err();
    assert!(error.message.contains("EACCES"), "{}", error.message);
    assert_eq!(foreign_entries(outside.path()), 0);
}

fn foreign_entries(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .filter(|entry| {
            !Scratch::is_owner_file(&entry.as_ref().unwrap().file_name().to_string_lossy())
        })
        .count()
}

#[tokio::test]
async fn runtime_without_budget_passes_messages_through() {
    let runtime = DefaultContextRuntime::new(Options {
        locale: Some("en-US".into()),
        ..Options::default()
    });
    let message = obj(vec![
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(vec![obj(vec![
                ("type", "tool_result".into()),
                ("toolCallId", "call-plain".into()),
                (
                    "content",
                    JsValue::Array(vec![obj(vec![
                        ("type", "text".into()),
                        ("text", "kept".into()),
                    ])]),
                ),
            ])]),
        ),
    ]);
    let mut fields = JsObject::new();
    fields.insert("turnId".into(), "turn-1".into());
    fields.insert("toolResultMessage".into(), message.clone());
    fields.insert(
        "messages".into(),
        JsValue::Array(vec![obj(vec![
            ("role", "assistant".into()),
            (
                "content",
                JsValue::Array(vec![obj(vec![
                    ("type", "text".into()),
                    ("text", "prior".into()),
                ])]),
            ),
        ])]),
    );
    let result = runtime
        .apply_tool_results(ContextInput {
            fields,
            abort_signal: None,
            budget_evaluator: None,
        })
        .await
        .unwrap();
    assert_eq!(
        stringify_json(&result).unwrap().unwrap(),
        format!(
            "{{\"messages\":[{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"prior\"}}]}},{}],\"appendedMessages\":[{}],\"diagnostics\":[]}}",
            stringify_json(&message).unwrap().unwrap(),
            stringify_json(&message).unwrap().unwrap()
        )
    );
}
