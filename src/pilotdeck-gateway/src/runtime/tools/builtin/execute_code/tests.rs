use super::*;
use crate::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::network::Cancellation,
};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
const ORACLE: &str = include_str!("../../../../../tests/fixtures/execute-code/oracle.json");
fn js(value: &Value) -> JsValue {
    pilotdeck_protocol::parse_json(&value.to_string()).unwrap()
}
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(&pilotdeck_protocol::stringify_json(value).unwrap().unwrap()).unwrap()
}
pub(super) fn make_context(root: &std::path::Path, cancel: Cancellation) -> ToolRunContext {
    let checkpoint = HostToolCheckpoint::new(None, &[]).unwrap();
    let path = root.to_string_lossy().into_owned();
    ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: object([
            ("cwd", string(path.clone())),
            (
                "env",
                JsValue::Object(object([
                    ("PATH", string("/usr/bin:/bin")),
                    ("HOME", string(path.clone())),
                    ("PILOT_HOME", string(path.clone())),
                    ("TMPDIR", string(path)),
                ])),
            ),
        ]),
        files: checkpoint.tool_context_state(),
        abort_signal: Some(Arc::new(cancel)),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: Some(Arc::new(|| JsValue::Number(100_000.))),
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    }))
}
fn approved(context: &mut ToolRunContext) {
    context.fields.insert(
        "currentPermissionDecision".into(),
        JsValue::Object(object([("type", string("allow"))])),
    );
}
fn hook(
    context: &mut ToolRunContext,
    calls: Arc<std::sync::Mutex<Vec<crate::host::modules::capability::ToolCall>>>,
) {
    context.execute_tool = Some(Arc::new(move |call| {
        calls.lock().unwrap().push(call);
        Box::pin(async {
            Ok(object([
                ("type", string("success")),
                (
                    "content",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("type", string("text")),
                        ("text", string("fixture content")),
                    ]))]),
                ),
                (
                    "data",
                    JsValue::Object(object([("kept", JsValue::Bool(true))])),
                ),
            ]))
        })
    }));
}
fn data(output: &ToolOutput) -> &JsValue {
    output.data.as_ref().unwrap()
}
async fn execute(tool: &ExecuteCodeTool, context: ToolRunContext, input: Value) -> ToolOutput {
    tokio::time::timeout(Duration::from_secs(12), tool.execute(js(&input), context))
        .await
        .unwrap()
        .unwrap()
}
#[test]
fn fixed_ts_classification_validation_and_result_projection() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let root = Workspace::create().unwrap();
    let mut context = make_context(&root.path, Cancellation::new());
    context
        .fields
        .insert("cwd".into(), string("/fixture/project"));
    let tool = ExecuteCodeTool::new();
    for case in oracle["readOnly"].as_array().unwrap() {
        let input = js(&json!({"code":case["code"]}));
        assert_eq!(
            tool.is_read_only(&input).unwrap(),
            js(&case["readOnly"]),
            "{case}"
        );
        assert_eq!(
            tool.is_concurrency_safe(&input).unwrap(),
            JsValue::Bool(false)
        );
    }
    for case in oracle["validation"].as_array().unwrap() {
        let actual = validation(&js(&case["input"]));
        if case["output"]["ok"] == true {
            assert!(actual.is_empty())
        } else {
            assert_eq!(
                json_value(&JsValue::Array(
                    actual.into_iter().map(JsValue::Object).collect()
                )),
                case["output"]["issues"]
            );
        }
    }
    for case in oracle["rows"].as_array().unwrap() {
        let (status,output,error,duration)=match case["id"].as_str().unwrap() {
            "missing-hook"=>("unsupported".into(),"".into(),Some("execute_code requires a ToolRuntime recursion hook, but this host did not provide one.".into()),0.),
            "missing-python"=>("unsupported".into(),"".into(),Some("execute_code requires python3 on PATH.".into()),0.),
            "runtime-throw"=>("error".into(),"".into(),Some("fixture runtime failed".into()),1.25),
            _=>{
                let supplied=&case["options"]["result"];
                let result=process::ResultData{
                    exit_code:if supplied.get("exitCode").is_some(){supplied["exitCode"].as_i64().map(|n|n as i32)}else{Some(0)},
                    stdout:supplied["stdout"].as_str().unwrap_or("hello\n").into(),
                    stderr:supplied["stderr"].as_str().unwrap_or("").into(),
                    timed_out:supplied["timedOut"].as_bool().unwrap_or(false),
                    cancelled:supplied["cancelled"].as_bool().unwrap_or(false),
                };
                let (status,output,error)=finish_execution(result,2.);
                (status,output,error,1.25)
            }
        };
        let output = output_value(
            &context,
            &status,
            &output,
            error.as_deref(),
            0,
            duration,
            vec![],
        );
        assert_eq!(
            json_value(data(&output)),
            case["output"]["data"],
            "{}",
            case["id"]
        );
        assert_eq!(
            json_value(&JsValue::Array(output.content)),
            case["output"]["content"]
        );
        assert_eq!(
            json_value(&JsValue::Object(output.metadata.unwrap())),
            case["output"]["metadata"]
        );
    }
}
#[tokio::test]
async fn fixed_ts_rpc_responses_and_nested_call_ids() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let root = Workspace::create().unwrap();
    for case in oracle["rpc"].as_array().unwrap() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let result = js(&case["result"]).as_object().cloned().unwrap_or_default();
        let mut allowed = HELPERS.to_vec();
        if case["webSearch"] == false {
            allowed.retain(|n| *n != "web_search");
        }
        let state = rpc::State::new(
            Arc::new(move |call| {
                seen.lock().unwrap().push(call);
                let result = result.clone();
                Box::pin(async move { Ok(result) })
            }),
            case["expectedToken"].as_str().map(str::to_owned),
            allowed,
            "execute_code".into(),
            50,
            make_context(&root.path, Cancellation::new()),
        );
        let response = state.handle(&case["request"].to_string()).await.unwrap();
        assert_eq!(json_value(&response), case["output"], "{}", case["id"]);
        let actual:Vec<_>=calls.lock().unwrap().iter().map(|call|json!({"call":{"id":text(&call.id),"name":text(&call.name),"input":json_value(&call.input)},"overrides":{"currentToolCallId":text(&call.id)}})).collect();
        assert_eq!(actual, case["calls"].as_array().unwrap().clone());
    }
}
#[tokio::test]
async fn actual_python_helpers_keep_results_local_and_enforce_call_limit() {
    let root = Workspace::create().unwrap();
    let mut context = make_context(&root.path, Cancellation::new());
    approved(&mut context);
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    hook(&mut context, calls.clone());
    context
        .fields
        .insert("currentToolCallId".into(), string("outer"));
    let output=execute(&ExecuteCodeTool::new(),context.clone(),json!({"code":"from pilotdeck_tools import read_file\nr=read_file('x',offset=2,limit=3)\nprint(r['content'])","timeout_seconds":3})).await;
    assert_eq!(data(&output).get("status"), Some(&string("success")));
    assert_eq!(
        data(&output).get("output"),
        Some(&string("fixture content\n"))
    );
    assert_eq!(
        data(&output).get("tool_calls_made"),
        Some(&JsValue::Number(1.))
    );
    let calls = calls.lock().unwrap();
    assert_eq!(text(&calls[0].id), "outer:code:1");
    assert_eq!(text(&calls[0].name), "read_file");
    assert_eq!(
        json_value(&calls[0].input),
        json!({"file_path":"x","offset":2,"limit":3})
    );
    drop(calls);
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    context.execute_tool = Some(Arc::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("zero limit must reject before dispatch") })
    }));
    let output=execute(&ExecuteCodeTool::new(),context,json!({"code":"from pilotdeck_tools import read_file\ntry:\n read_file('x')\nexcept RuntimeError as e:\n print(e)","max_tool_calls":0,"timeout_seconds":3})).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        data(&output).get("output"),
        Some(&string(
            "Tool call limit reached (0). No more tool calls allowed in this execution.\n"
        ))
    );
    assert_eq!(
        data(&output).get("tool_calls_made"),
        Some(&JsValue::Number(0.))
    );
}
#[tokio::test]
async fn actual_python_syntax_timeout_cancel_and_byte_output_bounds() {
    let root = Workspace::create().unwrap();
    let signal = Cancellation::new();
    let mut context = make_context(&root.path, signal.clone());
    approved(&mut context);
    hook(&mut context, Arc::new(std::sync::Mutex::new(Vec::new())));
    let tool = ExecuteCodeTool::new();
    let issues = tool
        .validate_input(js(&json!({"code":"if ("})), context.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        text(issues[0].get("message").unwrap().as_string().unwrap())
            .starts_with("Python syntax error at L1:")
    );
    let output = execute(
        &tool,
        context.clone(),
        json!({"code":"import time\ntime.sleep(30)","timeout_seconds":1}),
    )
    .await;
    assert_eq!(data(&output).get("status"), Some(&string("timeout")));
    let future = tool.execute(
        js(&json!({"code":"import time\ntime.sleep(30)","timeout_seconds":10})),
        context.clone(),
    );
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal.cancel();
    };
    let (output, ()) = tokio::join!(tokio::time::timeout(Duration::from_secs(5), future), cancel);
    assert_eq!(
        data(&output.unwrap().unwrap()).get("status"),
        Some(&string("cancelled"))
    );
    let fresh = Cancellation::new();
    let mut context = make_context(&root.path, fresh);
    approved(&mut context);
    hook(&mut context, Arc::new(std::sync::Mutex::new(Vec::new())));
    let output=execute(&tool,context,json!({"code":"import sys\nsys.stdout.write('a'*100000)\nsys.stderr.write('b'*20000)","timeout_seconds":3})).await;
    let text = text(data(&output).get("output").unwrap().as_string().unwrap());
    assert_eq!(
        text,
        format!(
            "{}\n\n... [OUTPUT TRUNCATED - 50,000 bytes omitted out of 100,000 total] ...\n\n{}",
            "a".repeat(20000),
            "a".repeat(30000)
        )
    );
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn auto_readonly_including_accept_edits_actually_blocks_direct_python_writes() {
    let root = Workspace::create().unwrap();
    for mode in ["default", "acceptEdits", "plan"] {
        let mut context = make_context(&root.path, Cancellation::new());
        hook(&mut context, Arc::new(std::sync::Mutex::new(Vec::new())));
        context.fields.insert("currentPermissionDecision".into(),js(&json!({"type":"allow","reason":{"type":"mode","mode":mode,"message":format!("Mode {mode} allows read-only tool execute_code.")}})));
        let output=execute(&ExecuteCodeTool::new(),context,json!({"code":"from pathlib import Path\nPath('must-not-exist').write_text('bad')","timeout_seconds":3})).await;
        assert_eq!(
            data(&output).get("status"),
            Some(&string("error")),
            "{mode}"
        );
        assert!(!root.path.join("must-not-exist").exists());
    }
    let mut context = make_context(&root.path, Cancellation::new());
    approved(&mut context);
    hook(&mut context, Arc::new(std::sync::Mutex::new(Vec::new())));
    let output=execute(&ExecuteCodeTool::new(),context,json!({"code":"from pathlib import Path\nPath('approved').write_text('allowed')","timeout_seconds":3})).await;
    assert_eq!(data(&output).get("status"), Some(&string("success")));
    assert_eq!(
        fs::read_to_string(root.path.join("approved")).unwrap(),
        "allowed"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn explicit_permission_preserves_workspace_sandbox_and_nested_write_dispatch() {
    let root = Workspace::create().unwrap();
    let outside = Workspace::create().unwrap();
    let mut context = make_context(&root.path, Cancellation::new());
    approved(&mut context);
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    hook(&mut context, calls.clone());
    let tool = ExecuteCodeTool::with_sandbox(Some(Arc::new(NativeSandbox::new(
        SandboxMode::WorkspaceWrite,
        root.path.clone(),
    ))));
    let output = execute(&tool, context.clone(), json!({
        "code":"from pathlib import Path\nfrom pilotdeck_tools import write_file\nPath('direct').write_text('ok')\nwrite_file('nested','keeps full native permission gates')\nprint('done')",
        "timeout_seconds":3,
    })).await;
    assert_eq!(data(&output).get("status"), Some(&string("success")));
    assert_eq!(fs::read_to_string(root.path.join("direct")).unwrap(), "ok");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(text(&calls.lock().unwrap()[0].name), "write_file");
    let outside_path = outside.path.join("forbidden");
    let code = format!(
        "from pathlib import Path\nPath({}).write_text('bad')",
        serde_json::to_string(&outside_path.to_string_lossy()).unwrap()
    );
    let output = execute(&tool, context, json!({"code":code,"timeout_seconds":3})).await;
    assert_eq!(data(&output).get("status"), Some(&string("error")));
    assert!(!outside_path.exists());
}
