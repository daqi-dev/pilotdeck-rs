//! Additional native resource/lifecycle regressions; no external provider.
use super::*;
use crate::{host::modules::capability::ToolCall, runtime::network::Cancellation};
#[cfg(target_os = "macos")]
use serde_json::Value;
use serde_json::json;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn context(root: &std::path::Path, signal: Cancellation) -> ToolRunContext {
    let mut context = super::tests::make_context(root, signal);
    context.fields.insert(
        "currentPermissionDecision".into(),
        JsValue::Object(object([("type", string("allow"))])),
    );
    context.execute_tool = Some(Arc::new(|_| {
        Box::pin(async {
            Ok(object([
                ("type", string("success")),
                ("content", JsValue::Array(vec![])),
            ]))
        })
    }));
    context
}
fn input(code: &str, seconds: u64) -> JsValue {
    pilotdeck_protocol::parse_json(&json!({"code":code,"timeout_seconds":seconds}).to_string())
        .unwrap()
}
fn status(output: &ToolOutput) -> String {
    text(
        output
            .data
            .as_ref()
            .unwrap()
            .get("status")
            .unwrap()
            .as_string()
            .unwrap(),
    )
}
#[tokio::test]
async fn concurrent_helper_requests_reserve_exactly_fifty_distinct_calls() {
    let root = Workspace::create().unwrap();
    let calls = Arc::new(Mutex::new(Vec::<ToolCall>::new()));
    let seen = calls.clone();
    let dispatched = Arc::new(AtomicUsize::new(0));
    let counter = dispatched.clone();
    let state = Arc::new(rpc::State::new(
        Arc::new(move |call| {
            counter.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(call);
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(object([
                    ("type", string("success")),
                    ("content", JsValue::Array(vec![])),
                ]))
            })
        }),
        Some("fixture-token".into()),
        vec!["read_file"],
        "outer".into(),
        50,
        context(&root.path, Cancellation::new()),
    ));
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..60 {
        let state = state.clone();
        requests.spawn(async move {
            state
                .handle(r#"{"token":"fixture-token","tool":"read_file","args":{"file_path":"x"}}"#)
                .await
                .unwrap()
        });
    }
    let mut allowed = 0;
    let mut denied = 0;
    while let Some(response) = requests.join_next().await {
        let response = response.unwrap();
        if response.get("code") == Some(&string("tool_call_limit_reached")) {
            denied += 1;
        } else {
            assert_eq!(response.get("content"), Some(&string("")));
            allowed += 1;
        }
    }
    assert_eq!((allowed, denied), (50, 10));
    assert_eq!(dispatched.load(Ordering::SeqCst), 50);
    assert_eq!(state.made.load(Ordering::SeqCst), 50);
    assert_eq!(state.log.lock().unwrap().len(), 50);
    let ids: std::collections::HashSet<_> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| text(&call.id))
        .collect();
    assert_eq!(ids, (1..=50).map(|n| format!("outer:code:{n}")).collect());
}
#[test]
fn private_workspace_permissions_and_replacement_refusal_are_real_filesystem_checks() {
    use std::os::unix::fs::PermissionsExt;
    let root = Workspace::create().unwrap();
    assert_eq!(
        fs::metadata(&root.path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    root.write("script.py", b"pass\n").unwrap();
    assert_eq!(
        fs::metadata(root.path.join("script.py"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let fixture = Workspace::create().unwrap();
    let moved = fixture.path.join("moved");
    let victim = fixture.path.join("victim");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("KEEP"), b"original").unwrap();
    fs::rename(&root.path, &moved).unwrap();
    std::os::unix::fs::symlink(&victim, &root.path).unwrap();
    assert_eq!(root.cleanup().unwrap_err().code, "execution_cleanup_failed");
    assert_eq!(fs::read(victim.join("KEEP")).unwrap(), b"original");
    assert_eq!(fs::read(moved.join("script.py")).unwrap(), b"pass\n");
    fs::remove_file(&root.path).unwrap();
    // A missing original pathname is not proof of deletion: its retained fd
    // still reports links, so a renamed private root remains an explicit error.
    assert_eq!(root.cleanup().unwrap_err().code, "execution_cleanup_failed");
    fs::rename(&moved, &root.path).unwrap();
    root.cleanup().unwrap();
    assert!(
        !root.path.exists(),
        "confirmed cleanup must remove the original inode pathname"
    );
}
#[tokio::test]
async fn script_replacing_private_root_cannot_report_successful_cleanup_or_delete_victim() {
    let fixture = Workspace::create().unwrap();
    let victim = fixture.path.join("victim");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("KEEP"), b"original").unwrap();
    let victim = fs::canonicalize(&victim).unwrap();
    let code = "import os\nfrom pathlib import Path\noriginal=os.environ['PILOTDECK_EXECUTE_CODE_TEMP_ROOT']\nos.rename(original,'moved-execution')\nos.symlink(str(Path('victim').resolve()),original)\nPath('replacement-link').write_text(original)\nprint('script completed but cleanup must fail')\n";
    let output = tokio::time::timeout(
        Duration::from_secs(8),
        ExecuteCodeTool::new().execute(input(code, 3), context(&fixture.path, Cancellation::new())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status(&output), "error");
    assert!(
        text(
            output
                .data
                .as_ref()
                .unwrap()
                .get("error")
                .unwrap()
                .as_string()
                .unwrap()
        )
        .contains("workspace identity changed")
    );
    assert_eq!(fs::read(victim.join("KEEP")).unwrap(), b"original");
    let link = PathBuf::from(fs::read_to_string(fixture.path.join("replacement-link")).unwrap());
    assert_eq!(fs::read_link(&link).unwrap(), victim);
    fs::remove_file(link).unwrap(); // only the exact symlink made by this script
}

#[cfg(target_os = "macos")]
mod owned_groups {
    use super::*;
    use rustix::process::{Pid, Signal};
    fn identity(pid: i32) -> Option<String> {
        let output = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_string())
            .filter(|s| !s.is_empty())
    }
    struct Pids(Vec<(Pid, String)>);
    impl Pids {
        fn capture(value: &Value) -> Self {
            let leader = Pid::from_raw(value["leader"].as_i64().unwrap() as i32).unwrap();
            Self(
                ["leader", "child"]
                    .iter()
                    .map(|key| {
                        let pid = Pid::from_raw(value[*key].as_i64().unwrap() as i32).unwrap();
                        assert_eq!(rustix::process::getpgid(Some(pid)).unwrap(), leader);
                        (
                            pid,
                            identity(pid.as_raw_pid())
                                .expect("owned process must still exist before cancellation"),
                        )
                    })
                    .collect(),
            )
        }
        async fn wait_reaped(&self) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if self
                        .0
                        .iter()
                        .all(|(pid, born)| identity(pid.as_raw_pid()).as_ref() != Some(born))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("owned Python leader or group descendant survived");
        }
    }
    impl Drop for Pids {
        fn drop(&mut self) {
            // Failure cleanup is restricted to the observed identities of
            // this test's live reserved process group. Never signal by name.
            for (pid, born) in &self.0 {
                if identity(pid.as_raw_pid()).as_ref() == Some(born) {
                    let _ = rustix::process::kill_process(*pid, Signal::KILL);
                }
            }
        }
    }
    async fn proof(root: &std::path::Path) -> Pids {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(bytes) = fs::read(root.join("pids.json"))
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                {
                    return Pids::capture(&value);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Python did not reach owned descendant fixture")
    }
    const BASE: &str = "import os,sys,json,time,subprocess\nfrom pathlib import Path\nchild=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])\nPath('pids.json').write_text(json.dumps({'leader':os.getpid(),'child':child.pid}))\n";
    #[tokio::test]
    async fn timeout_cancel_drop_and_rpc_failure_reap_real_python_group_children() {
        for mode in ["timeout", "cancel", "drop", "rpc_failure"] {
            let root = Workspace::create().unwrap();
            let signal = Cancellation::new();
            let code = if mode == "rpc_failure" {
                format!(
                    "{BASE}while not Path('release').exists(): time.sleep(.01)\nimport socket\ns=socket.create_connection(('127.0.0.1',int(os.environ['PILOTDECK_RPC_PORT'])))\ns.sendall(b'x'*(1024*1024+1)+b'\\n')\ntime.sleep(30)\n"
                )
            } else {
                format!("{BASE}time.sleep(30)\n")
            };
            let seconds = if mode == "timeout" { 3 } else { 10 };
            let mut future = ExecuteCodeTool::new()
                .execute(input(&code, seconds), context(&root.path, signal.clone()));
            let pids = tokio::select! {
                pids=proof(&root.path)=>pids,
                result=&mut future=>panic!("{mode} ended before PID receipt: {result:?}"),
            };
            match mode {
                "drop" => drop(future),
                "cancel" => {
                    signal.cancel();
                    let output = tokio::time::timeout(Duration::from_secs(6), future)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(status(&output), "cancelled");
                }
                "rpc_failure" => {
                    fs::write(root.path.join("release"), b"go").unwrap();
                    let output = tokio::time::timeout(Duration::from_secs(6), future)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(status(&output), "error");
                    assert!(
                        text(
                            output
                                .data
                                .as_ref()
                                .unwrap()
                                .get("error")
                                .unwrap()
                                .as_string()
                                .unwrap()
                        )
                        .contains("1MiB")
                    );
                }
                _ => {
                    let output = tokio::time::timeout(Duration::from_secs(6), future)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(status(&output), "timeout");
                }
            }
            pids.wait_reaped().await;
        }
    }
}
