//! Host commands reuse the installed native CLI and the owned command runner.
use crate::{
    channel::protocol::{
        BoxFuture, ChannelError, ChannelStatusPort, ChannelStatusSnapshot, ChannelUpdatePort,
        ChannelUpdateResult,
    },
    config::Env,
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::{
        network::Cancellation,
        tools::{
            ToolRunContext,
            builtin::bash::{
                AbortSignal, CommandOptions, CommandRunner, runner::NativeOwnedShellCommandRunner,
            },
        },
    },
};
use pilotdeck_protocol::JsObject;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::oneshot;

#[derive(Clone)]
pub(super) struct NativeChannelSystem(Arc<Inner>);
struct Inner {
    cwd: PathBuf,
    cli: PathBuf,
    token_file: PathBuf,
    env: Env,
    started: Instant,
    cancelled: Cancellation,
    jobs: Mutex<Jobs>,
    close: tokio::sync::Mutex<()>,
}
#[derive(Default)]
struct Jobs {
    closing: bool,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
struct CancelOnDrop(Cancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl NativeChannelSystem {
    pub(super) fn new(cwd: PathBuf, cli: PathBuf, token_file: PathBuf, env: Env) -> Self {
        Self(Arc::new(Inner {
            cwd,
            cli,
            token_file,
            env,
            started: Instant::now(),
            cancelled: Cancellation::new(),
            jobs: Mutex::new(Jobs::default()),
            close: tokio::sync::Mutex::new(()),
        }))
    }
    fn command(
        &self,
        command: String,
        timeout_ms: f64,
        needs_auth: bool,
    ) -> BoxFuture<Result<ChannelUpdateResult, ChannelError>> {
        let owner = self.0.clone();
        let cancel = Cancellation::new();
        let request_guard = CancelOnDrop(cancel.clone());
        let (send, receive) = oneshot::channel();
        let mut jobs = lock(&owner.jobs);
        if jobs.closing {
            return Box::pin(async { Err(ChannelError::error("Gateway is stopping")) });
        }
        jobs.tasks.retain(|job| !job.is_finished());
        let worker = owner.clone();
        jobs.tasks.push(tokio::spawn(async move {
            let result = execute(worker, command, timeout_ms, needs_auth, cancel).await;
            let _ = send.send(result);
        }));
        drop(jobs);
        Box::pin(async move {
            let _guard = request_guard;
            receive
                .await
                .map_err(|e| ChannelError::error(e.to_string()))?
        })
    }
    /// Cancellation starts before joining; every tracked child completes its
    /// native cleanup before the channel assembly can report closed.
    pub(super) async fn stop(&self) -> Result<(), ChannelError> {
        let _close = self.0.close.lock().await;
        let tasks = {
            let mut jobs = lock(&self.0.jobs);
            jobs.closing = true;
            self.0.cancelled.cancel();
            std::mem::take(&mut jobs.tasks)
        };
        let mut errors = Vec::new();
        for task in tasks {
            if let Err(error) = task.await {
                errors.push(ChannelError::error(error.to_string()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ChannelError::Aggregate {
                message: "Channel command shutdown failed".into(),
                errors,
            })
        }
    }
}
fn abort_signal(cancel: Cancellation) -> Result<AbortSignal, ChannelError> {
    let checkpoint =
        HostToolCheckpoint::new(None, &[]).map_err(|e| ChannelError::error(e.to_string()))?;
    let context = ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: checkpoint.tool_context_state(),
        abort_signal: Some(Arc::new(cancel)),
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
    }));
    AbortSignal::from_context(&context)
        .ok_or_else(|| ChannelError::error("Command cancellation unavailable"))
}
async fn execute(
    owner: Arc<Inner>,
    command: String,
    timeout_ms: f64,
    needs_auth: bool,
    cancel: Cancellation,
) -> Result<ChannelUpdateResult, ChannelError> {
    let output = Arc::new(Mutex::new(String::new()));
    let capture: crate::runtime::tools::builtin::bash::OutputCallback = {
        let output = output.clone();
        Arc::new(move |chunk| lock(&output).push_str(chunk))
    };
    let mut env = owner.env.clone();
    if needs_auth {
        let token = tokio::fs::read_to_string(&owner.token_file)
            .await
            .map_err(|e| ChannelError::error(e.to_string()))?;
        env.insert("PILOTDECK_GATEWAY_TOKEN".into(), token.trim().to_owned());
    }
    let future = NativeOwnedShellCommandRunner.run(
        command.as_str().into(),
        CommandOptions {
            cwd: owner.cwd.to_string_lossy().as_ref().into(),
            env: Some(
                env.iter()
                    .map(|(k, v)| (k.as_str().into(), v.as_str().into()))
                    .collect(),
            ),
            timeout_ms,
            signal: Some(abort_signal(cancel.clone())?),
            on_stdout: Some(capture.clone()),
            on_stderr: Some(capture),
            max_output_units: None,
        },
    );
    tokio::pin!(future);
    let result = tokio::select! {
        result = &mut future => result,
        _ = owner.cancelled.cancelled() => { cancel.cancel(); future.await },
    }
    .map_err(|e| ChannelError::error(String::from_utf16_lossy(e.message.units())))?;
    if result.timed_out {
        return Err(ChannelError::error("Channel host command timed out"));
    }
    Ok(ChannelUpdateResult {
        code: result.exit_code.unwrap_or(1),
        output: lock(&output).clone(),
    })
}
impl ChannelUpdatePort for NativeChannelSystem {
    fn run(&self, check: bool) -> BoxFuture<Result<ChannelUpdateResult, ChannelError>> {
        let command = format!(
            "exec {} update {}",
            quote(&self.0.cli.to_string_lossy()),
            if check { "--check" } else { "--restart" }
        );
        // Source compilation is intentionally done on the installation target.
        self.command(command, 3_600_000.0, !check)
    }
}
impl ChannelStatusPort for NativeChannelSystem {
    fn read(&self) -> BoxFuture<Result<ChannelStatusSnapshot, ChannelError>> {
        let owner = self.clone();
        Box::pin(async move {
            let branch = owner
                .command("git rev-parse --abbrev-ref HEAD".into(), 10_000.0, false)
                .await?;
            if branch.code != 0 {
                return Err(ChannelError::error(branch.output));
            }
            let commit = owner
                .command("git log -1 --format='%h %s'".into(), 10_000.0, false)
                .await?;
            if commit.code != 0 {
                return Err(ChannelError::error(commit.output));
            }
            Ok(ChannelStatusSnapshot {
                branch: branch.output.trim().into(),
                commit: commit.output.trim().into(),
                uptime: owner.0.started.elapsed().as_secs_f64(),
                platform: if cfg!(target_os = "macos") {
                    "darwin"
                } else {
                    std::env::consts::OS
                }
                .into(),
                runtime_version: format!("pilotdeck-rs {}", env!("CARGO_PKG_VERSION")),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        system: NativeChannelSystem,
    }
    impl Fixture {
        fn new(script: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("pdrs-channel-cmd-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let cli = root.join("pilotdeck quoted ' $name");
            std::fs::write(&cli, script).unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
            let token = root.join("token");
            std::fs::write(&token, "fixture-token").unwrap();
            let env = Env::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), root.to_string_lossy().into_owned()),
                ("PILOT_HOME".into(), root.to_string_lossy().into_owned()),
                ("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into()),
            ]);
            let system = NativeChannelSystem::new(root.clone(), cli, token, env);
            Self { root, system }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[tokio::test]
    async fn update_uses_exact_installed_cli_arguments_and_exit_result() {
        let f = Fixture::new(
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\nif [ \"$2\" = --check ]; then exit 2; fi\n[ \"$PILOTDECK_GATEWAY_TOKEN\" = fixture-token ] || exit 9\nprintf 'restart accepted\\n'\n",
        );
        let check = f.system.run(true).await.unwrap();
        assert_eq!(check.code, 2);
        assert_eq!(check.output, "update\n--check\n");
        let apply = f.system.run(false).await.unwrap();
        assert_eq!(apply.code, 0);
        assert_eq!(apply.output, "update\n--restart\nrestart accepted\n");
        f.system.stop().await.unwrap();
        assert!(f.system.run(true).await.is_err());
    }
    #[tokio::test]
    async fn dropped_update_is_cancelled_and_stop_joins_process_cleanup() {
        let f = Fixture::new(
            "#!/bin/sh\necho $$ > leader.pid\ntrap 'exit 3' TERM\nwhile :; do sleep 0.1; done\n",
        );
        let request = f.system.run(true);
        let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(f.root.join("leader.pid"))
                    && text.ends_with('\n')
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(request);
        tokio::time::timeout(std::time::Duration::from_secs(10), f.system.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(!crate::process_group::group_exists(
            rustix::process::Pid::from_raw(pid).unwrap()
        ));
    }
}
