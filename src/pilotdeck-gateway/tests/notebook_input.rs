#![allow(clippy::result_large_err)]
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::tools::{
        ExecutableTool, ToolRunContext,
        builtin::{edit_notebook::EditNotebookTool, read_file::ReadFileTool},
    },
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{fs, path::Path, sync::Arc};

fn object(values: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in values {
        result.insert(key.into(), value);
    }
    result
}

fn context(root: &Path) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::default();
    let fields = object([
        ("cwd", JsValue::from(root.to_string_lossy().as_ref())),
        ("permissionMode", JsValue::from("default")),
        (
            "env",
            JsValue::Object(object([
                (
                    "PILOT_HOME",
                    JsValue::from(root.join("home").to_string_lossy().as_ref()),
                ),
                ("HOME", JsValue::from(root.to_string_lossy().as_ref())),
            ])),
        ),
    ]);
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields,
        files: checkpoint.tool_context_state(),
        abort_signal: None,
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

fn read_input() -> JsValue {
    JsValue::Object(object([("file_path", JsValue::from("book.ipynb"))]))
}

fn edit_input() -> JsValue {
    JsValue::Object(object([
        ("notebook_path", JsValue::from("book.ipynb")),
        ("cell_id", JsValue::from("cell-0")),
        (
            "new_source",
            JsValue::from(
                "after
",
            ),
        ),
    ]))
}

const NOTEBOOK: &str = r#"{"cells":[{"cell_type":"code","source":["before
"],"outputs":[]}],"nbformat":4,"nbformat_minor":0}"#;

#[tokio::test]
async fn ordinary_notebook_reads_and_edits_through_the_public_tools() {
    let scratch = scratch::Scratch::new("normal-notebook-read-edit");
    let path = scratch.path().join("book.ipynb");
    fs::write(&path, NOTEBOOK).unwrap();
    let context = context(scratch.path());
    let read = ReadFileTool::new()
        .execute(read_input(), context.clone())
        .await
        .unwrap();
    assert_eq!(
        read.data.as_ref().unwrap().get("kind"),
        Some(&JsValue::from("notebook"))
    );
    let tool = EditNotebookTool::new();
    assert!(
        tool.validate_input(edit_input(), context.clone())
            .await
            .unwrap()
            .is_none()
    );
    tool.execute(edit_input(), context).await.unwrap();
    let changed = fs::read_to_string(path).unwrap();
    let notebook = parse_json(&changed).unwrap();
    let serialized = stringify_json(&notebook).unwrap().unwrap();
    assert!(serialized.contains("after"));
    assert!(!serialized.contains("before"));
}

#[tokio::test]
async fn both_notebook_snapshot_paths_reject_oversized_input_before_reading_it() {
    let scratch = scratch::Scratch::new("normal-notebook-size-bound");
    let path = scratch.path().join("book.ipynb");
    fs::write(&path, NOTEBOOK).unwrap();
    let context = context(scratch.path());
    ReadFileTool::new()
        .execute(read_input(), context.clone())
        .await
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let tool = EditNotebookTool::new();
    let validation = tool
        .validate_input(edit_input(), context.clone())
        .await
        .unwrap_err();
    assert_eq!(validation.code, "result_too_large");
    let execution = tool.execute(edit_input(), context).await.unwrap_err();
    assert_eq!(execution.code, "result_too_large");
    let read = ReadFileTool::new()
        .execute(read_input(), self::context(scratch.path()))
        .await
        .unwrap_err();
    assert_eq!(read.code, "result_too_large");
}

#[tokio::test]
async fn unread_notebook_and_invalid_json_keep_their_existing_errors() {
    let scratch = scratch::Scratch::new("normal-notebook-error-order");
    fs::write(scratch.path().join("book.ipynb"), b"not JSON").unwrap();
    let context = context(scratch.path());
    let rejected = EditNotebookTool::new()
        .validate_input(edit_input(), context.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(rejected.iter().any(|issue| {
        issue
            .get("message")
            .and_then(JsValue::as_string)
            .is_some_and(|message| {
                message
                    .equals_str("File has not been read yet. Read it first before writing to it.")
            })
    }));
    let error = ReadFileTool::new()
        .execute(read_input(), context)
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_tool_input");
    assert!(error.message.to_utf8().unwrap().contains("not valid JSON"));
}
