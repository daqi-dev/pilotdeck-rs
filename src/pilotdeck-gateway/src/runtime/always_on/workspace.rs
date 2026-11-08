//! Project-generation-owned native workspace providers. Command ownership,
//! process groups, timeout and output buffering reuse CommandRunner.
use std::{fs::File, future::Future, io, os::fd::AsRawFd, path::{Path, PathBuf}, sync::{Arc, Mutex}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::runtime::{cleanup_task, project::RuntimeLease, tools::builtin::bash::{AbortSignal, CommandOptions, CommandResult, CommandRunner}};
use tokio::sync::{oneshot, watch};
use super::{AlwaysOnPaths, storage, workspace_copy};

const MAX_JOBS: usize = 4;
const COMMAND_UNITS: usize = 8 * 1024 * 1024;
const DIFF_UNITS: usize = 80_000;
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceError { pub code: String, pub message: JsString, missing: bool }
impl WorkspaceError {
    pub(crate) fn new(code: &str, message: impl Into<JsString>) -> Self { Self { code: code.into(), message: message.into(), missing: false } }
    pub(crate) fn is_missing(&self) -> bool { self.missing }
}
impl From<io::Error> for WorkspaceError {
    fn from(error: io::Error) -> Self {
        let missing = error.kind() == io::ErrorKind::NotFound;
        Self { code: "workspace_prepare_failed".into(), message: error.to_string().into(), missing }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceHandle {
    pub run_id: JsString, pub project_key: JsString, pub strategy: String, pub cwd: JsString, pub metadata: JsObject,
}
impl WorkspaceHandle {
    pub(crate) fn cycle_workspace(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert("strategy".into(), self.strategy.as_str().into());
        object.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        object.insert("metadata".into(), JsValue::Object(self.metadata.clone())); object
    }
}
#[derive(Clone, Debug)]
pub(crate) struct NativeWorkspaceDiff { pub text: JsString, pub file_count: usize, pub truncated: bool }
type Done = watch::Receiver<Option<Result<(), WorkspaceError>>>;
struct Jobs { closed: bool, running: Vec<Done> }
struct Inner {
    paths: AlwaysOnPaths, project: PathBuf, generation: Mutex<Option<Arc<RuntimeLease>>>,
    runner: Arc<dyn CommandRunner>, env: Option<Vec<(JsString, JsString)>>,
    jobs: Mutex<Jobs>, serial: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub(crate) struct NativeWorkspace(Arc<Inner>);
impl NativeWorkspace {
    pub(crate) fn new(paths: AlwaysOnPaths, generation: Arc<RuntimeLease>, runner: Arc<dyn CommandRunner>, env: Option<Vec<(JsString, JsString)>>) -> io::Result<Self> {
        let project = generation.runtime().project_root().to_owned();
        if project != Path::new(&paths.project_key) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Always-On workspace project does not match its admitted generation."));
        }
        Ok(Self(Arc::new(Inner { paths, project, generation: Mutex::new(Some(generation)), runner, env,
            jobs: Mutex::new(Jobs { closed: false, running: Vec::new() }), serial: tokio::sync::Mutex::new(()) })))
    }
    async fn owned<T: Send + 'static>(&self, operation: impl FnOnce(Self) -> std::pin::Pin<Box<dyn Future<Output = Result<T, WorkspaceError>> + Send>> + Send + 'static) -> Result<T, WorkspaceError> {
        let receiver = {
        let mut jobs = self.0.jobs.lock().unwrap_or_else(|error| error.into_inner());
        if jobs.closed { return Err(WorkspaceError::new("workspace_unavailable", "Always-On workspace owner is closed.")); }
        // Completed successes need not be retained. A dropped/panicked job's
        // failure stays observable by close instead of claiming quiescence.
        jobs.running.retain(|job| !matches!(&*job.borrow(), Some(Ok(()))));
        if jobs.running.len() >= MAX_JOBS { return Err(WorkspaceError::new("workspace_unavailable", "Always-On workspace operation capacity is full.")); }
        let generation = self.0.generation.lock().unwrap_or_else(|error| error.into_inner()).clone()
            .ok_or_else(|| WorkspaceError::new("workspace_unavailable", "Always-On workspace generation was released."))?;
        let (complete, done) = watch::channel(None); jobs.running.push(done);
        let (sender, receiver) = oneshot::channel(); let workspace = self.clone();
        let guard = cleanup_task::Completion::new(complete, WorkspaceError::new("workspace_cleanup_failed", "Always-On workspace operation was dropped before quiescence."));
        cleanup_task::spawn_completion(&tokio::runtime::Handle::current(), guard, async move {
            let _generation = generation;
            let _serial = workspace.0.serial.lock().await;
            let result = operation(workspace.clone()).await;
            let completion = result.as_ref().err().filter(|error| error.code == "workspace_cleanup_failed").cloned().map_or(Ok(()), Err);
            let _ = sender.send(result);
            completion
        });
        receiver
        };
        receiver.await.map_err(|_| WorkspaceError::new("workspace_cleanup_failed", "Always-On workspace operation ended without a result."))?
    }
    pub(crate) async fn close(&self) -> Result<(), WorkspaceError> {
        let jobs = { let mut state = self.0.jobs.lock().unwrap_or_else(|error| error.into_inner()); state.closed = true; state.running.clone() };
        let mut first = None;
        for mut job in jobs {
            loop {
                let result = job.borrow_and_update().clone();
                if let Some(result) = result { if let Err(error) = result { first.get_or_insert(error); } break; }
                if job.changed().await.is_err() { first.get_or_insert_with(|| WorkspaceError::new("workspace_cleanup_failed", "Always-On workspace completion channel closed.")); break; }
            }
        }
        // Every accepted job retained the exact selected generation until its
        // copy/owned child finished, even when its original caller vanished.
        self.0.generation.lock().unwrap_or_else(|error| error.into_inner()).take();
        first.map_or(Ok(()), Err)
    }
    pub(crate) fn project_root(&self) -> &Path { &self.0.project }
    async fn io<T: Send + 'static>(&self, operation: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
        let generation = self.0.generation.lock().unwrap_or_else(|error| error.into_inner()).clone()
            .ok_or_else(|| io::Error::other("Always-On workspace generation was released."))?;
        storage::run(move || { let _generation = generation; operation() }).await
    }


    pub(crate) async fn prepare(&self, strategy: &str, run_id: JsString, title: JsString, signal: Option<AbortSignal>) -> Result<WorkspaceHandle, WorkspaceError> {
        let strategy = strategy.to_owned();
        self.owned(move |owner| Box::pin(async move { owner.prepare_inner(&strategy, run_id, title, signal).await })).await
    }
    async fn prepare_inner(&self, strategy: &str, run_id: JsString, title: JsString, signal: Option<AbortSignal>) -> Result<WorkspaceHandle, WorkspaceError> {
        let id = run_id.to_utf8().map_err(|_| WorkspaceError::new("workspace_prepare_failed", "Always-On run id is not a native path segment."))?;
        if id.is_empty() || id.len() > 256 || id == "." || id == ".." || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) {
            return Err(WorkspaceError::new("workspace_prepare_failed", "Always-On run id is not a bounded native path segment."));
        }
        let root = self.0.project.clone();
        let project = self.io(move || workspace_copy::pin_project(&root)).await?;
        let top = if strategy == "auto" || strategy == "git-worktree" {
            let top = self.command(&project, &["git", "rev-parse", "--show-toplevel"], signal.clone()).await?;
            let head = self.command(&project, &["git", "rev-parse", "HEAD"], signal.clone()).await?;
            (top.exit_code == Some(0) && head.exit_code == Some(0)).then_some(crate::metadata::js_trim(&top.stdout).to_owned())
        } else { None };
        let selected = if strategy == "auto" { if top.is_some() { "git-worktree" } else { "snapshot-copy" } } else { strategy };
        let (cwd, metadata) = match selected {
            "git-worktree" => {
                let top = top.ok_or_else(|| WorkspaceError::new("workspace_prepare_failed", "git rev-parse failed for the selected project."))?;
                self.prepare_git(&project, &top, &id, &title, signal).await?
            }
            "snapshot-copy" => {
                let paths = self.0.paths.clone(); let selected = project.clone();
                self.io(move || workspace_copy::snapshot(&paths, &selected, &id, workspace_copy::CopyLimits::default())).await?
            }
            _ => return Err(WorkspaceError::new("workspace_unavailable", format!("workspace strategy \"{selected}\" is not available."))),
        };
        Ok(WorkspaceHandle { run_id, project_key: self.0.paths.project_key.as_str().into(), strategy: selected.into(), cwd: cwd.to_string_lossy().as_ref().into(), metadata })
    }
    async fn prepare_git(&self, project: &Arc<File>, top: &str, id: &str, title: &JsString, signal: Option<AbortSignal>) -> Result<(PathBuf, JsObject), WorkspaceError> {
        let project_root = self.0.project.clone(); let top_path = PathBuf::from(top);
        let repository = self.io(move || {
            let canonical = top_path.canonicalize()?;
            let selected = project_root.canonicalize()?;
            if !selected.starts_with(&canonical) { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Always-On git root is outside the admitted project ancestry.")); }
            workspace_copy::pin_project(&canonical)
        }).await?;
        let status = self.command(&repository, &["git", "status", "--porcelain"], signal.clone()).await?;
        expect_ok(&status, "git status --porcelain")?;
        if !crate::metadata::js_trim(&status.stdout).is_empty() {
            expect_ok(&self.command(&repository, &["git", "add", "-A"], signal.clone()).await?, "git add -A")?;
            let title = String::from_utf16_lossy(title.units()).split(crate::metadata::js_space).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
            let message = format!("chore(always-on): checkpoint before executing {}", if title.is_empty() { "untitled plan" } else { &title });
            expect_ok(&self.command(&repository, &["git", "commit", "-m", &message], signal.clone()).await?, "git commit")?;
            let clean = self.command(&repository, &["git", "status", "--porcelain"], signal.clone()).await?;
            expect_ok(&clean, "git status --porcelain")?;
            if !crate::metadata::js_trim(&clean.stdout).is_empty() { return Err(WorkspaceError::new("workspace_prepare_failed", "git worktree remained dirty after the Always-On checkpoint commit.")); }
        }
        let branch = self.command(&repository, &["git", "rev-parse", "--abbrev-ref", "HEAD"], signal.clone()).await?;
        expect_ok(&branch, "git rev-parse --abbrev-ref HEAD")?;
        let commit = self.command(&repository, &["git", "rev-parse", "HEAD"], signal.clone()).await?;
        expect_ok(&commit, "git rev-parse HEAD")?;
        let paths = self.0.paths.clone(); let run = id.to_owned();
        let (target, _pinned_target) = self.io(move || workspace_copy::new_target(&paths, "git-worktree", &run)).await?;
        let branch_name = format!("always-on/{id}");
        let target_text = target.to_str().ok_or_else(|| WorkspaceError::new("workspace_prepare_failed", "Always-On worktree path is not representable."))?;
        let result = self.command(&repository, &["git", "worktree", "add", "-b", &branch_name, target_text, crate::metadata::js_trim(&commit.stdout)], signal).await?;
        expect_ok(&result, "git worktree add")?;
        // Revalidate the result using this generation's expected base. It is
        // also the admission used by persisted cycles after a host restart.
        let paths = self.0.paths.clone(); let target_copy = target.clone();
        self.io(move || workspace_copy::owned_workspace(&paths, "git-worktree", &target_copy)).await?;
        let mut metadata = JsObject::new();
        for (key, value) in [("repoRoot", top), ("baseBranch", crate::metadata::js_trim(&branch.stdout)), ("baseCommit", crate::metadata::js_trim(&commit.stdout)), ("branchName", branch_name.as_str())] { metadata.insert(key.into(), value.into()); }
        let _ = project; // project fd remains held through the entire prepare.
        Ok((target, metadata))
    }
    pub(crate) async fn authorize_cycle(&self, cycle: super::WorkCycleRecord) -> Result<WorkspaceHandle, WorkspaceError> {
        self.owned(move |owner| Box::pin(async move {
            let selected_project = native_path(&cycle.project_key)?;
            let expected = owner.0.project.clone();
            owner.io(move || {
                if selected_project.canonicalize()? != expected.canonicalize()? {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Persisted work cycle belongs to another project."));
                }
                Ok(())
            }).await?;
            let workspace = cycle.workspace.ok_or_else(|| WorkspaceError::new("missing_workspace", "Cycle has no associated workspace to apply"))?;
            owner.authorize_handle(WorkspaceHandle { run_id: cycle.created_by_run_id, project_key: cycle.project_key, strategy: workspace.strategy, cwd: workspace.cwd, metadata: workspace.metadata }).await
        })).await
    }

    /// Distinct ensureActiveWorkCycle currentWorkspace branch: validate the
    /// existing reference but do not migrate plans or create/write a cycle.
    pub(crate) async fn authorize_legacy(&self, workspace: JsObject) -> Result<WorkspaceHandle, WorkspaceError> {
        self.owned(move |owner| Box::pin(async move {
            let required = |field: &str| workspace.get(field).and_then(JsValue::as_string).cloned()
                .ok_or_else(|| WorkspaceError::new("invalid_workspace", format!("Legacy workspace {field} must be a string.")));
            let strategy = required("strategy")?.to_utf8().map_err(|_| WorkspaceError::new("invalid_workspace", "Legacy workspace strategy is not representable."))?;
            let handle = WorkspaceHandle { run_id: required("runId")?, project_key: owner.0.paths.project_key.as_str().into(), strategy,
                cwd: required("cwd")?, metadata: workspace.get("metadata").and_then(JsValue::as_object).cloned().unwrap_or_default() };
            owner.authorize_handle(handle).await
        })).await
    }
    async fn authorize_handle(&self, handle: WorkspaceHandle) -> Result<WorkspaceHandle, WorkspaceError> {
        let path = native_path(&handle.cwd)?; let paths = self.0.paths.clone(); let strategy = handle.strategy.clone();
        let directory = self.io(move || workspace_copy::owned_workspace(&paths, &strategy, &path)).await?;
        if handle.strategy == "git-worktree" { self.verify_git_project(&directory, None).await?; }
        Ok(handle)
    }

    pub(crate) async fn dispose(&self, handle: WorkspaceHandle, keep: bool) -> Result<(), WorkspaceError> {
        if keep { return Ok(()); }
        self.owned(move |owner| Box::pin(async move {
            let cwd = native_path(&handle.cwd)?; let paths = owner.0.paths.clone(); let strategy = handle.strategy.clone(); let copy = cwd.clone();
            let directory = owner.io(move || workspace_copy::owned_workspace(&paths, &strategy, &copy)).await?;
            if handle.strategy == "git-worktree" {
                owner.verify_git_project(&directory, None).await?;
                let root = owner.0.project.clone(); let repository = owner.io(move || workspace_copy::pin_project(&root)).await?;
                let cwd_text = cwd.to_string_lossy().into_owned();
                let removed = owner.command(&repository, &["git", "worktree", "remove", "--force", &cwd_text], None).await?;
                if removed.exit_code != Some(0) {
                    let paths = owner.0.paths.clone(); let copy = cwd.clone(); owner.io(move || workspace_copy::remove_owned(&paths, "git-worktree", &copy)).await?;
                    let _ = owner.command(&repository, &["git", "worktree", "prune"], None).await?;
                }
                if let Some(branch) = handle.metadata.get("branchName").and_then(JsValue::as_string) {
                    let branch = branch.to_utf8().map_err(|_| WorkspaceError::new("invalid_workspace", "Workspace branch is not representable."))?;
                    // Only the branch corresponding to this owned run may be
                    // removed, never arbitrary persisted metadata commands.
                    let run = cwd.file_name().and_then(|name| name.to_str()).unwrap_or("");
                    if branch == format!("always-on/{run}") { let _ = owner.command(&repository, &["git", "branch", "-D", &branch], None).await?; }
                }
            } else {
                let paths = owner.0.paths.clone(); owner.io(move || workspace_copy::remove_owned(&paths, "snapshot-copy", &cwd)).await?;
            }
            Ok(())
        })).await
    }

    async fn verify_git_project(&self, workspace: &Arc<File>, signal: Option<AbortSignal>) -> Result<(), WorkspaceError> {
        let root = self.0.project.clone(); let project = self.io(move || workspace_copy::pin_project(&root)).await?;
        let selected = self.command(workspace, &["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], signal.clone()).await?;
        let expected = self.command(&project, &["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], signal).await?;
        expect_ok(&selected, "git rev-parse --git-common-dir")?; expect_ok(&expected, "git rev-parse --git-common-dir")?;
        let selected = PathBuf::from(crate::metadata::js_trim(&selected.stdout)); let expected = PathBuf::from(crate::metadata::js_trim(&expected.stdout));
        self.io(move || {
            let selected = std::fs::metadata(selected.canonicalize()?)?; let expected = std::fs::metadata(expected.canonicalize()?)?;
            use std::os::unix::fs::MetadataExt;
            if selected.dev() != expected.dev() || selected.ino() != expected.ino() {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Workspace Git repository does not belong to the admitted project."));
            }
            Ok(())
        }).await?;
        Ok(())
    }

    pub(crate) async fn diff(&self, strategy: &str, cwd: JsString, signal: Option<AbortSignal>) -> Result<NativeWorkspaceDiff, WorkspaceError> {
        let strategy = strategy.to_owned();
        self.owned(move |owner| Box::pin(async move { owner.diff_inner(&strategy, cwd, signal).await })).await
    }
    async fn diff_inner(&self, strategy: &str, cwd: JsString, signal: Option<AbortSignal>) -> Result<NativeWorkspaceDiff, WorkspaceError> {
        let cwd = native_path(&cwd)?; let cwd_text = cwd.to_string_lossy().into_owned(); let paths = self.0.paths.clone(); let selected = strategy.to_owned();
        let workspace = self.io(move || workspace_copy::owned_workspace(&paths, &selected, &cwd)).await?;
        let (text, count) = if strategy == "git-worktree" {
            self.verify_git_project(&workspace, signal.clone()).await?;
            let add = self.command(&workspace, &["git", "add", "-A"], signal.clone()).await?;
            if add.exit_code != Some(0) { return Ok(empty_diff()); }
            let stat = self.command(&workspace, &["git", "diff", "--cached", "HEAD", "--stat"], signal.clone()).await?;
            let count = if stat.exit_code == Some(0) { stat.stdout.bytes().filter(|byte| *byte == b'\n').count().saturating_sub(1) } else { 0 };
            let diff = self.command(&workspace, &["git", "diff", "--cached", "HEAD"], signal).await?;
            (if diff.exit_code == Some(0) { diff.stdout } else { String::new() }, count)
        } else {
            let root = self.0.project.to_str().ok_or_else(|| WorkspaceError::new("invalid_workspace", "Project root is not representable."))?;
            // The process is already chdir'd to the pinned selected workspace.
            let result = self.command(&workspace, &["diff", "-ruN", "--exclude=.git", "--exclude=node_modules", "--exclude=dist", "--exclude=.pilotdeck", "--exclude=.pilotdeck-always-on", root, &cwd_text], signal).await?;
            if result.exit_code.is_some_and(|code| code > 1) { return Ok(empty_diff()); }
            let count = result.stdout.lines().filter(|line| line.starts_with("diff ")).count();
            (result.stdout, count)
        };
        if crate::metadata::js_trim(&text).is_empty() { return Ok(NativeWorkspaceDiff { text: "".into(), file_count: count, truncated: false }); }
        let units: Vec<_> = text.encode_utf16().collect(); let truncated = units.len() > DIFF_UNITS;
        Ok(NativeWorkspaceDiff { text: JsString::from_utf16(units.into_iter().take(DIFF_UNITS).collect()), file_count: count, truncated })
    }
    async fn command(&self, directory: &Arc<File>, args: &[&str], signal: Option<AbortSignal>) -> Result<CommandResult, WorkspaceError> {
        let command = args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
        let mut env = self.0.env.clone().unwrap_or_else(|| std::env::vars_os().map(|(key, value)| (key.to_string_lossy().as_ref().into(), value.to_string_lossy().as_ref().into())).collect());
        // Internal argv commands must not execute a user's login-shell rc
        // that changes cwd after the descriptor-based child chdir.
        env.retain(|(key, _)| !key.equals_str("PILOTDECK_SHELL_PATH"));
        env.push(("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into()));
        #[cfg(target_os = "linux")]
        let cwd = format!("/proc/self/fd/{}", directory.as_raw_fd());
        #[cfg(not(target_os = "linux"))]
        let cwd = format!("/dev/fd/{}", directory.as_raw_fd());
        let result = self.0.runner.run(command.as_str().into(), CommandOptions { cwd: cwd.as_str().into(), env: Some(env), timeout_ms: 60_000.0, signal,
            on_stdout: None, on_stderr: None, max_output_units: Some(COMMAND_UNITS + 64 * 1024 + 8) }).await
            .map_err(|error| WorkspaceError::new(if error.code == "owned_command_not_quiescent" { "workspace_cleanup_failed" } else { "workspace_prepare_failed" }, error.message))?;
        // CommandRunner retains an owned 3s kill/reap timer after TERM. Do
        // not release our generation/job admission while that group lives.
        if let Some(group) = result.pid.and_then(|pid| rustix::process::Pid::from_raw(pid as i32)) {
            crate::process_group::wait_for_group_empty(Some(group), std::time::Duration::from_secs(4)).await;
            if crate::process_group::group_exists(group) { return Err(WorkspaceError::new("workspace_cleanup_failed", "Always-On command group did not quiesce.")); }
        }
        if result.timed_out { return Err(WorkspaceError::new("workspace_prepare_failed", "Always-On workspace command timed out.")); }
        if result.stdout.encode_utf16().count() >= COMMAND_UNITS || result.stderr.encode_utf16().count() >= COMMAND_UNITS {
            return Err(WorkspaceError::new("workspace_resource_limit", "Always-On workspace command output exceeded its native resource limit."));
        }
        Ok(result)
    }
}
fn native_path(value: &JsString) -> Result<PathBuf, WorkspaceError> {
    value.to_utf8().map(PathBuf::from).map_err(|_| WorkspaceError::new("invalid_workspace", "Workspace path is not representable by the native filesystem."))
}
fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }
fn expect_ok(result: &CommandResult, label: &str) -> Result<(), WorkspaceError> {
    if result.exit_code == Some(0) { Ok(()) } else { Err(WorkspaceError::new("workspace_prepare_failed", format!("{label} failed: {}", if result.stderr.is_empty() { &result.stdout } else { &result.stderr }))) }
}
fn empty_diff() -> NativeWorkspaceDiff { NativeWorkspaceDiff { text: "".into(), file_count: 0, truncated: false } }

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
