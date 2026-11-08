use super::*;
use crate::runtime::tools::builtin::bash::runner::{
    CommandOptions, CommandRunner, SandboxedShellCommandRunner,
};
use std::{path::PathBuf, time::Duration};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn options(cwd: &Path) -> CommandOptions {
    CommandOptions {
        cwd: cwd.to_string_lossy().as_ref().into(),
        env: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
        timeout_ms: 5000.0,
        signal: None,
        on_stdout: None,
        on_stderr: None,
        max_output_units: Some(10000),
    }
}
#[test]
fn profile_escapes_paths_and_filesystem_fence_follows_dangling_links() {
    assert_eq!(quote_profile("a\\b\"c").unwrap(), "\"a\\\\b\\\"c\"");
    assert!(quote_profile("a\0b").is_err());
    let scratch = Scratch(
        std::env::temp_dir().join(format!("pdrs-native-sandbox-fs-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(scratch.0.join("workspace")).unwrap();
    let sandbox = NativeSandbox::new(SandboxMode::ReadOnly, std::env::temp_dir());
    assert_eq!(
        sandbox
            .check_write(&scratch.0, &scratch.0.join("new.txt"))
            .unwrap_err()
            .code,
        "permission_denied"
    );
    let sandbox = NativeSandbox::new(SandboxMode::WorkspaceWrite, std::env::temp_dir());
    let profile = sandbox.profile(&scratch.0.join("workspace")).unwrap();
    assert!(profile.contains("(deny file-write*)"));
    assert!(profile.contains("(literal \"/dev/null\")"));
    assert!(!profile.contains("deny network"));
    // No target is written. Use a nonexistent absolute location outside all
    // allowed roots to test resolution of a dangling escape before mutation.
    std::os::unix::fs::symlink(
        "/pdrs-never-create-outside",
        scratch.0.join("workspace/link"),
    )
    .unwrap();
    assert!(
        sandbox
            .check_write(
                &scratch.0.join("workspace"),
                &scratch.0.join("workspace/link/new.txt")
            )
            .is_err()
    );
}
#[tokio::test]
async fn full_access_preparation_does_not_probe_or_rewrite_argv() {
    let sandbox = NativeSandbox::new(SandboxMode::DangerFullAccess, std::env::temp_dir());
    let executable: JsString = "/unavailable/exact-executable".into();
    let args = vec![JsString::from("a; b"), JsString::from("quoted'\"value")];
    let prepared = sandbox
        .prepare(executable.clone(), args.clone(), &"/".into(), None)
        .await
        .unwrap();
    assert_eq!(prepared, (executable, args));
    assert!(sandbox.available.get().is_none());
}
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn unavailable_platform_never_returns_a_bare_confined_command() {
    let sandbox = NativeSandbox::new(SandboxMode::ReadOnly, std::env::temp_dir());
    assert_eq!(
        sandbox
            .prepare(
                "/bin/sh".into(),
                vec!["-c".into(), "true".into()],
                &"/tmp".into(),
                None
            )
            .await
            .unwrap_err()
            .code,
        "sandbox_unavailable"
    );
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn real_seatbelt_foreground_and_detached_tasks_enforce_file_effects() {
    use crate::runtime::background::{BackgroundTaskRuntime, StartTaskSpec, TaskStatus};
    let scratch = Scratch(
        std::env::temp_dir().join(format!("pdrs-native-seatbelt-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&scratch.0).unwrap();
    std::fs::write(
        scratch.0.join("owned.sh"),
        "set -e\nprintf DENIED > denied.txt\n",
    )
    .unwrap();
    let sandbox = Arc::new(NativeSandbox::new(
        SandboxMode::ReadOnly,
        std::env::temp_dir(),
    ));
    let runner = SandboxedShellCommandRunner(sandbox.clone());
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        runner.run("sh owned.sh".into(), options(&scratch.0)),
    )
    .await
    .unwrap();
    assert!(
        *sandbox
            .available
            .get()
            .expect("real Seatbelt probe must run"),
        "mbp2 has no working Seatbelt provider; no execution PASS may be claimed"
    );
    let result = result.unwrap();
    assert_ne!(result.exit_code, Some(0));
    assert!(!scratch.0.join("denied.txt").exists());
    let tasks = BackgroundTaskRuntime::default().with_sandbox(sandbox);
    let task = tasks
        .start(StartTaskSpec {
            command: "sh owned.sh".into(),
            cwd: scratch.0.to_string_lossy().as_ref().into(),
            env: Some(vec![("PATH".into(), "/usr/bin:/bin".into())]),
            session_id: "owner".into(),
            agent_id: pilotdeck_protocol::JsValue::Undefined,
            kind: pilotdeck_protocol::JsValue::Undefined,
        })
        .await;
    let waited = if let Ok(task) = &task {
        tasks
            .wait(
                &task.task_id,
                &pilotdeck_protocol::JsValue::Number(3000.0),
                None,
                None,
            )
            .await
    } else {
        Err(crate::runtime::background::BackgroundTaskError {
            name: "Error",
            message: "start failed".into(),
        })
    };
    let disposed = tasks.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(waited.unwrap().unwrap().task.status, TaskStatus::Failed);
    assert!(!scratch.0.join("denied.txt").exists());
    let write_sandbox = Arc::new(NativeSandbox::new(
        SandboxMode::WorkspaceWrite,
        std::env::temp_dir(),
    ));
    let result = SandboxedShellCommandRunner(write_sandbox)
        .run("sh owned.sh".into(), options(&scratch.0))
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("denied.txt")).unwrap(),
        "DENIED"
    );
    // macOS permits writes throughout /tmp for workspace-write, so a genuine
    // outside-root negative must live in an independently owned directory
    // outside platform tmp. Only our UUID directory under this test checkout's
    // existing _tmp is touched; no user file is a test target.
    let outside = Scratch(
        std::env::current_dir()
            .unwrap()
            .join("_tmp")
            .join(format!("native-seatbelt-outside-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&outside.0).unwrap();
    std::fs::write(
        scratch.0.join("outside.sh"),
        "set -e\nprintf OUTSIDE > \"$1\"\n",
    )
    .unwrap();
    let target = outside.0.join("must-not-exist");
    let quoted = format!("'{}'", target.to_string_lossy().replace('\'', "'\\''"));
    let sandbox = Arc::new(NativeSandbox::new(
        SandboxMode::WorkspaceWrite,
        std::env::temp_dir(),
    ));
    let result = SandboxedShellCommandRunner(sandbox)
        .run(
            format!("sh outside.sh {quoted}").into(),
            options(&scratch.0),
        )
        .await
        .unwrap();
    assert_ne!(result.exit_code, Some(0));
    assert!(!target.exists());
}
