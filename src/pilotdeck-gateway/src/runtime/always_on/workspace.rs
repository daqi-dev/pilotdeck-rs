//! Project-generation-owned native workspace providers. Command ownership,
//! process groups, timeout and output buffering reuse CommandRunner.
use super::{AlwaysOnPaths, storage, workspace_copy};
use crate::runtime::{
    cleanup_task,
    project::RuntimeLease,
    tools::builtin::bash::{AbortSignal, CommandOptions, CommandResult, CommandRunner},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs::File,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{oneshot, watch};

const MAX_JOBS: usize = 4;
const COMMAND_UNITS: usize = 8 * 1024 * 1024;
const DIFF_UNITS: usize = 80_000;
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceError {
    pub code: String,
    pub message: JsString,
    missing: bool,
}
impl WorkspaceError {
    pub(crate) fn new(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            missing: false,
        }
    }
    pub(crate) fn is_missing(&self) -> bool {
        self.missing
    }
}
impl From<io::Error> for WorkspaceError {
    fn from(error: io::Error) -> Self {
        let missing = error.kind() == io::ErrorKind::NotFound;
        Self {
            code: "workspace_prepare_failed".into(),
            message: error.to_string().into(),
            missing,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceHandle {
    pub run_id: JsString,
    pub project_key: JsString,
    pub strategy: String,
    pub cwd: JsString,
    pub metadata: JsObject,
    pub(super) created_identity: Option<Arc<File>>,
    /// Exact original common Git directory, for a retry after workspace removal.
    pub(super) created_repository: Option<Arc<File>>,
}
impl WorkspaceHandle {
    pub(crate) fn cycle_workspace(&self) -> JsObject {
        let mut object = JsObject::new();
        object.insert("strategy".into(), self.strategy.as_str().into());
        object.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        object.insert("metadata".into(), JsValue::Object(self.metadata.clone()));
        object
    }
}
#[derive(Clone, Debug)]
pub(crate) struct NativeWorkspaceDiff {
    pub text: JsString,
    pub file_count: usize,
    pub truncated: bool,
}
type PreparedWorkspace = (PathBuf, JsObject, Arc<File>, Option<Arc<File>>);
type Done = watch::Receiver<Option<Result<(), WorkspaceError>>>;
struct Jobs {
    closed: bool,
    running: Vec<Done>,
}
struct Inner {
    paths: AlwaysOnPaths,
    project: PathBuf,
    generation: Mutex<Option<Arc<RuntimeLease>>>,
    runner: Arc<dyn CommandRunner>,
    env: Option<Vec<(JsString, JsString)>>,
    jobs: Mutex<Jobs>,
    serial: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub(crate) struct NativeWorkspace(Arc<Inner>);
impl NativeWorkspace {
    pub(crate) fn new(
        paths: AlwaysOnPaths,
        generation: Arc<RuntimeLease>,
        runner: Arc<dyn CommandRunner>,
        env: Option<Vec<(JsString, JsString)>>,
    ) -> io::Result<Self> {
        let project = generation.runtime().project_root().to_owned();
        if project != Path::new(&paths.project_key) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Always-On workspace project does not match its admitted generation.",
            ));
        }
        Ok(Self(Arc::new(Inner {
            paths,
            project,
            generation: Mutex::new(Some(generation)),
            runner,
            env,
            jobs: Mutex::new(Jobs {
                closed: false,
                running: Vec::new(),
            }),
            serial: tokio::sync::Mutex::new(()),
        })))
    }
    async fn owned<T: Send + 'static>(
        &self,
        operation: impl FnOnce(
            Self,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, WorkspaceError>> + Send>,
        > + Send
        + 'static,
    ) -> Result<T, WorkspaceError> {
        let receiver = {
            let mut jobs = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if jobs.closed {
                return Err(WorkspaceError::new(
                    "workspace_unavailable",
                    "Always-On workspace owner is closed.",
                ));
            }
            // Completed successes need not be retained. A dropped/panicked job's
            // failure stays observable by close instead of claiming quiescence.
            jobs.running
                .retain(|job| !matches!(&*job.borrow(), Some(Ok(()))));
            if jobs.running.len() >= MAX_JOBS {
                return Err(WorkspaceError::new(
                    "workspace_unavailable",
                    "Always-On workspace operation capacity is full.",
                ));
            }
            let generation = self
                .0
                .generation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or_else(|| {
                    WorkspaceError::new(
                        "workspace_unavailable",
                        "Always-On workspace generation was released.",
                    )
                })?;
            let (complete, done) = watch::channel(None);
            jobs.running.push(done);
            let (sender, receiver) = oneshot::channel();
            let workspace = self.clone();
            let guard = cleanup_task::Completion::new(
                complete,
                WorkspaceError::new(
                    "workspace_cleanup_failed",
                    "Always-On workspace operation was dropped before quiescence.",
                ),
            );
            cleanup_task::spawn_completion(&tokio::runtime::Handle::current(), guard, async move {
                let _generation = generation;
                let _serial = workspace.0.serial.lock().await;
                let result = operation(workspace.clone()).await;
                let completion = result
                    .as_ref()
                    .err()
                    .filter(|error| error.code == "workspace_cleanup_failed")
                    .cloned()
                    .map_or(Ok(()), Err);
                let _ = sender.send(result);
                completion
            });
            receiver
        };
        receiver.await.map_err(|_| {
            WorkspaceError::new(
                "workspace_cleanup_failed",
                "Always-On workspace operation ended without a result.",
            )
        })?
    }
    pub(crate) async fn close(&self) -> Result<(), WorkspaceError> {
        let jobs = {
            let mut state = self
                .0
                .jobs
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.closed = true;
            state.running.clone()
        };
        let mut first = None;
        for mut job in jobs {
            loop {
                let result = job.borrow_and_update().clone();
                if let Some(result) = result {
                    if let Err(error) = result {
                        first.get_or_insert(error);
                    }
                    break;
                }
                if job.changed().await.is_err() {
                    first.get_or_insert_with(|| {
                        WorkspaceError::new(
                            "workspace_cleanup_failed",
                            "Always-On workspace completion channel closed.",
                        )
                    });
                    break;
                }
            }
        }
        // Every accepted job retained the exact selected generation until its
        // copy/owned child finished, even when its original caller vanished.
        self.0
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        first.map_or(Ok(()), Err)
    }
    pub(crate) fn project_root(&self) -> &Path {
        &self.0.project
    }
    async fn io<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let generation = self
            .0
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| io::Error::other("Always-On workspace generation was released."))?;
        storage::run(move || {
            let _generation = generation;
            operation()
        })
        .await
    }

    pub(crate) async fn prepare(
        &self,
        strategy: &str,
        run_id: JsString,
        title: JsString,
        signal: Option<AbortSignal>,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        let strategy = strategy.to_owned();
        self.owned(move |owner| {
            Box::pin(async move { owner.prepare_inner(&strategy, run_id, title, signal).await })
        })
        .await
    }
    async fn prepare_inner(
        &self,
        strategy: &str,
        run_id: JsString,
        title: JsString,
        signal: Option<AbortSignal>,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        let id = run_id.to_utf8().map_err(|_| {
            WorkspaceError::new(
                "workspace_prepare_failed",
                "Always-On run id is not a native path segment.",
            )
        })?;
        if id.is_empty()
            || id.len() > 256
            || id == "."
            || id == ".."
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(WorkspaceError::new(
                "workspace_prepare_failed",
                "Always-On run id is not a bounded native path segment.",
            ));
        }
        let root = self.0.project.clone();
        let project = self.io(move || workspace_copy::pin_project(&root)).await?;
        let top = if strategy == "auto" || strategy == "git-worktree" {
            let top = self
                .command(
                    &project,
                    &self.0.project,
                    &["git", "rev-parse", "--show-toplevel"],
                    signal.clone(),
                )
                .await?;
            let head = self
                .command(
                    &project,
                    &self.0.project,
                    &["git", "rev-parse", "HEAD"],
                    signal.clone(),
                )
                .await?;
            (top.exit_code == Some(0) && head.exit_code == Some(0))
                .then_some(crate::metadata::js_trim(&top.stdout).to_owned())
        } else {
            None
        };
        let selected = if strategy == "auto" {
            if top.is_some() {
                "git-worktree"
            } else {
                "snapshot-copy"
            }
        } else {
            strategy
        };
        let (cwd, metadata, created_identity, created_repository) = match selected {
            "git-worktree" => {
                let top = top.ok_or_else(|| {
                    WorkspaceError::new(
                        "workspace_prepare_failed",
                        "git rev-parse failed for the selected project.",
                    )
                })?;
                self.prepare_git(&project, &top, &id, &title, signal)
                    .await?
            }
            "snapshot-copy" => {
                let paths = self.0.paths.clone();
                let selected = project.clone();
                let (cwd, metadata, identity) = self
                    .io(move || {
                        workspace_copy::snapshot(
                            &paths,
                            &selected,
                            &id,
                            workspace_copy::CopyLimits::default(),
                        )
                    })
                    .await?;
                (cwd, metadata, identity, None)
            }
            _ => {
                return Err(WorkspaceError::new(
                    "workspace_unavailable",
                    format!("workspace strategy \"{selected}\" is not available."),
                ));
            }
        };
        Ok(WorkspaceHandle {
            run_id,
            project_key: self.0.paths.project_key.as_str().into(),
            strategy: selected.into(),
            cwd: cwd.to_string_lossy().as_ref().into(),
            metadata,
            created_identity: Some(created_identity),
            created_repository,
        })
    }
    async fn prepare_git(
        &self,
        project: &Arc<File>,
        top: &str,
        id: &str,
        title: &JsString,
        signal: Option<AbortSignal>,
    ) -> Result<PreparedWorkspace, WorkspaceError> {
        let project_root = self.0.project.clone();
        let top_path = PathBuf::from(top);
        let (repository, repository_path) = self
            .io(move || {
                let canonical = top_path.canonicalize()?;
                let selected = project_root.canonicalize()?;
                if !selected.starts_with(&canonical) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Always-On git root is outside the admitted project ancestry.",
                    ));
                }
                workspace_copy::pin_project(&canonical).map(|directory| (directory, canonical))
            })
            .await?;
        let common = self
            .command(
                &repository,
                &repository_path,
                &[
                    "git",
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ],
                signal.clone(),
            )
            .await?;
        expect_ok(&common, "git rev-parse original workspace repository")?;
        let common_path = PathBuf::from(crate::metadata::js_trim(&common.stdout));
        let common_copy = common_path.clone();
        let original_repository = self
            .io(move || workspace_copy::pin_project(&common_copy))
            .await?;
        let status = self
            .command(
                &repository,
                &repository_path,
                &["git", "status", "--porcelain"],
                signal.clone(),
            )
            .await?;
        expect_ok(&status, "git status --porcelain")?;
        if !crate::metadata::js_trim(&status.stdout).is_empty() {
            expect_ok(
                &self
                    .command(
                        &repository,
                        &repository_path,
                        &["git", "add", "-A"],
                        signal.clone(),
                    )
                    .await?,
                "git add -A",
            )?;
            let title = String::from_utf16_lossy(title.units())
                .split(crate::metadata::js_space)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let message = format!(
                "chore(always-on): checkpoint before executing {}",
                if title.is_empty() {
                    "untitled plan"
                } else {
                    &title
                }
            );
            expect_ok(
                &self
                    .command(
                        &repository,
                        &repository_path,
                        &["git", "commit", "-m", &message],
                        signal.clone(),
                    )
                    .await?,
                "git commit",
            )?;
            let clean = self
                .command(
                    &repository,
                    &repository_path,
                    &["git", "status", "--porcelain"],
                    signal.clone(),
                )
                .await?;
            expect_ok(&clean, "git status --porcelain")?;
            if !crate::metadata::js_trim(&clean.stdout).is_empty() {
                return Err(WorkspaceError::new(
                    "workspace_prepare_failed",
                    "git worktree remained dirty after the Always-On checkpoint commit.",
                ));
            }
        }
        let branch = self
            .command(
                &repository,
                &repository_path,
                &["git", "rev-parse", "--abbrev-ref", "HEAD"],
                signal.clone(),
            )
            .await?;
        expect_ok(&branch, "git rev-parse --abbrev-ref HEAD")?;
        let commit = self
            .command(
                &repository,
                &repository_path,
                &["git", "rev-parse", "HEAD"],
                signal.clone(),
            )
            .await?;
        expect_ok(&commit, "git rev-parse HEAD")?;
        let branch_name = format!("always-on/{id}");
        let branch_ref = format!("refs/heads/{branch_name}");
        let base_commit = crate::metadata::js_trim(&commit.stdout).to_owned();
        // A zero old value atomically reserves a previously absent ref. A
        // pre-existing/user-created branch is never claimed for rollback.
        let zero = "0".repeat(base_commit.len());
        let claimed = self
            .command(
                &repository,
                &repository_path,
                &["git", "update-ref", &branch_ref, &base_commit, &zero],
                signal.clone(),
            )
            .await?;
        expect_ok(&claimed, "git update-ref create workspace branch")?;
        let mut created = None;
        let prepared = async {
            let paths = self.0.paths.clone();
            let run = id.to_owned();
            let (target, pinned_target) = self
                .io(move || workspace_copy::new_target(&paths, "git-worktree", &run))
                .await?;
            created = Some((target.clone(), pinned_target.clone()));
            let target_text = target.to_str().ok_or_else(|| {
                WorkspaceError::new(
                    "workspace_prepare_failed",
                    "Always-On worktree path is not representable.",
                )
            })?;
            let result = self
                .command(
                    &repository,
                    &repository_path,
                    &["git", "worktree", "add", target_text, &branch_name],
                    signal,
                )
                .await?;
            expect_ok(&result, "git worktree add")?;
            let paths = self.0.paths.clone();
            let target_copy = target.clone();
            let selected = pinned_target.clone();
            self.io(move || {
                let admitted =
                    workspace_copy::owned_workspace(&paths, "git-worktree", &target_copy)?;
                checked_directory_identity(&selected, &admitted)
            })
            .await?;
            let mut metadata = JsObject::new();
            for (key, value) in [
                ("repoRoot", top),
                ("baseBranch", crate::metadata::js_trim(&branch.stdout)),
                ("baseCommit", base_commit.as_str()),
                ("branchName", branch_name.as_str()),
            ] {
                metadata.insert(key.into(), value.into());
            }
            let selected = original_repository.clone();
            let path = common_path.clone();
            self.io(move || checked_command_path(&selected, &path).map(|_| ()))
                .await?;
            Ok((
                target,
                metadata,
                pinned_target,
                Some(original_repository.clone()),
            ))
        }
        .await;
        let _ = project;
        match prepared {
            Ok(value) => Ok(value),
            Err(error) => match self
                .rollback_git_prepare(
                    &repository,
                    &repository_path,
                    created,
                    &branch_ref,
                    &base_commit,
                )
                .await
            {
                Ok(()) => Err(error),
                Err(cleanup) => Err(WorkspaceError::new(
                    "workspace_cleanup_failed",
                    format!(
                        "{}; owned workspace rollback failed: {}",
                        String::from_utf16_lossy(error.message.units()),
                        String::from_utf16_lossy(cleanup.message.units())
                    ),
                )),
            },
        }
    }
    async fn rollback_git_prepare(
        &self,
        repository: &Arc<File>,
        repository_path: &Path,
        created: Option<(PathBuf, Arc<File>)>,
        branch_ref: &str,
        base_commit: &str,
    ) -> Result<(), WorkspaceError> {
        let directory = async {
            let current = self
                .command(
                    repository,
                    repository_path,
                    &["git", "rev-parse", "--verify", branch_ref],
                    None,
                )
                .await?;
            if current.exit_code != Some(0)
                || crate::metadata::js_trim(&current.stdout) != base_commit
            {
                return Err(WorkspaceError::new(
                    "workspace_cleanup_failed",
                    "Workspace branch changed after creation; preserving its directory.",
                ));
            }
            if let Some((target, pinned)) = created {
                self.remove_git_selected(repository, repository_path, target, pinned)
                    .await?;
            }
            Ok(())
        }
        .await;
        // A failed removal leaves a possibly checked-out workspace. Ref CAS
        // does not enforce Git's checked-out-branch protection.
        directory?;
        self.require_unregistered_branch(repository, repository_path, branch_ref)
            .await?;
        match self
            .command(
                repository,
                repository_path,
                &["git", "update-ref", "-d", branch_ref, base_commit],
                None,
            )
            .await
        {
            Ok(result) => expect_ok(&result, "git update-ref rollback workspace branch"),
            Err(error) => Err(error),
        }
    }
    async fn require_unregistered_branch(
        &self,
        repository: &Arc<File>,
        repository_path: &Path,
        branch_ref: &str,
    ) -> Result<(), WorkspaceError> {
        let listed = self
            .command(
                repository,
                repository_path,
                &["git", "worktree", "list", "--porcelain", "-z"],
                None,
            )
            .await?;
        expect_ok(&listed, "git worktree list before owned branch deletion")?;
        let expected = format!("branch {branch_ref}");
        if listed.stdout.split('\0').any(|field| field == expected) {
            return Err(WorkspaceError::new(
                "workspace_cleanup_failed",
                "Workspace branch remains registered in a worktree; preserving its ref.",
            ));
        }
        // This observation is not a lock on other Git writers. An external
        // registration after this check is outside the native owner's fence.
        Ok(())
    }
    async fn registered_worktree(
        &self,
        repository: &Arc<File>,
        repository_path: &Path,
        target: &Path,
    ) -> Result<Option<Option<String>>, WorkspaceError> {
        let listed = self
            .command(
                repository,
                repository_path,
                &["git", "worktree", "list", "--porcelain", "-z"],
                None,
            )
            .await?;
        expect_ok(&listed, "git worktree list")?;
        let spelling = target.to_owned();
        let target = self
            .io(move || {
                let parent = spelling
                    .parent()
                    .ok_or(io::ErrorKind::InvalidInput)?
                    .canonicalize()?;
                Ok(parent.join(spelling.file_name().ok_or(io::ErrorKind::InvalidInput)?))
            })
            .await?;
        let target = target.to_str().ok_or_else(|| {
            WorkspaceError::new(
                "workspace_cleanup_failed",
                "Workspace cleanup path is not representable.",
            )
        })?;
        let mut selected = false;
        let mut result = None;
        for field in listed.stdout.split('\0') {
            if let Some(path) = field.strip_prefix("worktree ") {
                if selected {
                    break;
                }
                selected = path == target;
                if selected {
                    result = Some(None);
                }
            } else if selected && let Some(branch) = field.strip_prefix("branch ") {
                result = Some(Some(branch.to_owned()));
            }
        }
        Ok(result)
    }
    async fn remove_git_selected(
        &self,
        repository: &Arc<File>,
        repository_path: &Path,
        target: PathBuf,
        pinned: Arc<File>,
    ) -> Result<(), WorkspaceError> {
        let registration = self
            .registered_worktree(repository, repository_path, &target)
            .await?;
        if registration.is_none() {
            let paths = self.0.paths.clone();
            return self
                .io(move || {
                    workspace_copy::remove_created(&paths, "git-worktree", &target, &pinned)
                })
                .await
                .map_err(Into::into);
        }
        let paths = self.0.paths.clone();
        let captured = self
            .io(move || workspace_copy::capture_selected(&paths, "git-worktree", &target, &pinned))
            .await?;
        let recovery = captured.path.clone();
        let removed: Result<(), WorkspaceError> = async {
            // Never ask git to recursively remove the replaceable original run
            // path. Its exact existing registration moves into our private capture.
            let directory = captured.directory.clone();
            let selected_path = captured.path.clone();
            let canonical = self
                .io(move || checked_command_path(&directory, &selected_path))
                .await?;
            self.verify_git_project(&captured.directory, &canonical, None)
                .await?;
            let text = canonical.to_str().ok_or_else(|| {
                WorkspaceError::new(
                    "workspace_cleanup_failed",
                    "Captured worktree path is not representable.",
                )
            })?;
            let repaired = self
                .command(
                    repository,
                    repository_path,
                    &["git", "worktree", "repair", text],
                    None,
                )
                .await?;
            expect_ok(&repaired, "git worktree repair owned capture")?;
            if self
                .registered_worktree(repository, repository_path, &canonical)
                .await?
                != registration
            {
                return Err(WorkspaceError::new(
                    "workspace_cleanup_failed",
                    format!(
                        "Captured worktree registration is uncertain; preserving {}",
                        canonical.display()
                    ),
                ));
            }
            let removed = self
                .command(
                    repository,
                    repository_path,
                    &["git", "worktree", "remove", "--force", text],
                    None,
                )
                .await?;
            expect_ok(&removed, "git worktree remove owned capture")?;
            self.io(move || captured.finish_git_remove()).await?;
            Ok(())
        }
        .await;
        removed.map_err(|error| {
            WorkspaceError::new(
                "workspace_cleanup_failed",
                format!(
                    "{}; captured workspace cleanup is uncertain at {}",
                    String::from_utf16_lossy(error.message.units()),
                    recovery.display()
                ),
            )
        })
    }
    pub(crate) async fn authorize_cycle(
        &self,
        cycle: super::WorkCycleRecord,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        self.owned(move |owner| {
            Box::pin(async move {
                let selected_project = native_path(&cycle.project_key)?;
                let expected = owner.0.project.clone();
                owner
                    .io(move || {
                        if selected_project.canonicalize()? != expected.canonicalize()? {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "Persisted work cycle belongs to another project.",
                            ));
                        }
                        Ok(())
                    })
                    .await?;
                let workspace = cycle.workspace.ok_or_else(|| {
                    WorkspaceError::new(
                        "missing_workspace",
                        "Cycle has no associated workspace to apply",
                    )
                })?;
                owner
                    .authorize_handle(WorkspaceHandle {
                        run_id: cycle.created_by_run_id,
                        project_key: cycle.project_key,
                        strategy: workspace.strategy,
                        cwd: workspace.cwd,
                        metadata: workspace.metadata,
                        created_identity: None,
                        created_repository: None,
                    })
                    .await
            })
        })
        .await
    }

    /// Distinct ensureActiveWorkCycle currentWorkspace branch: validate the
    /// existing reference but do not migrate plans or create/write a cycle.
    pub(crate) async fn authorize_legacy(
        &self,
        workspace: JsObject,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        self.owned(move |owner| {
            Box::pin(async move {
                let required = |field: &str| {
                    workspace
                        .get(field)
                        .and_then(JsValue::as_string)
                        .cloned()
                        .ok_or_else(|| {
                            WorkspaceError::new(
                                "invalid_workspace",
                                format!("Legacy workspace {field} must be a string."),
                            )
                        })
                };
                let strategy = required("strategy")?.to_utf8().map_err(|_| {
                    WorkspaceError::new(
                        "invalid_workspace",
                        "Legacy workspace strategy is not representable.",
                    )
                })?;
                let handle = WorkspaceHandle {
                    run_id: required("runId")?,
                    project_key: owner.0.paths.project_key.as_str().into(),
                    strategy,
                    cwd: required("cwd")?,
                    created_identity: None,
                    created_repository: None,
                    metadata: workspace
                        .get("metadata")
                        .and_then(JsValue::as_object)
                        .cloned()
                        .unwrap_or_default(),
                };
                owner.authorize_handle(handle).await
            })
        })
        .await
    }
    async fn authorize_handle(
        &self,
        handle: WorkspaceHandle,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
        let path = native_path(&handle.cwd)?;
        let selected_path = path.clone();
        let paths = self.0.paths.clone();
        let strategy = handle.strategy.clone();
        let directory = self
            .io(move || workspace_copy::owned_workspace(&paths, &strategy, &path))
            .await?;
        if handle.strategy == "git-worktree" {
            self.verify_git_project(&directory, &selected_path, None)
                .await?;
        }
        Ok(handle)
    }

    pub(crate) async fn dispose(
        &self,
        handle: WorkspaceHandle,
        keep: bool,
    ) -> Result<(), WorkspaceError> {
        if keep {
            return Ok(());
        }
        self.owned(move |owner| {
            Box::pin(async move {
                let result: Result<(), WorkspaceError> = async {
                    let cwd = native_path(&handle.cwd)?;
                    let paths = owner.0.paths.clone();
                    let strategy = handle.strategy.clone();
                    let copy = cwd.clone();
                    let selected = handle.created_identity.clone();
                    let checked = owner
                        .io(move || {
                            let current =
                                workspace_copy::owned_workspace(&paths, &strategy, &copy)?;
                            if let Some(selected) = selected {
                                checked_directory_identity(&selected, &current)?;
                            }
                            Ok(current)
                        })
                        .await;
                    let directory = match checked {
                        Ok(directory) => directory,
                        Err(error)
                            if error.kind() == io::ErrorKind::NotFound
                                && handle.strategy == "git-worktree" =>
                        {
                            return owner.dispose_removed_git(&handle, &cwd).await;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if handle.strategy == "git-worktree" {
                        owner.verify_git_project(&directory, &cwd, None).await?;
                        let root = owner.0.project.clone();
                        let repository =
                            owner.io(move || workspace_copy::pin_project(&root)).await?;
                        let branch = handle
                            .metadata
                            .get("branchName")
                            .and_then(JsValue::as_string)
                            .map(|v| {
                                v.to_utf8().map_err(|_| {
                                    WorkspaceError::new(
                                        "invalid_workspace",
                                        "Workspace branch is not representable.",
                                    )
                                })
                            })
                            .transpose()?;
                        let run = cwd.file_name().and_then(|name| name.to_str()).unwrap_or("");
                        // A previous partial cleanup may already have removed the ref.
                        // Removing the directory/registration must succeed before CAS.
                        let owned_branch: Result<Option<(String, String)>, WorkspaceError> =
                            async {
                                let Some(branch) =
                                    branch.filter(|b| b == &format!("always-on/{run}"))
                                else {
                                    return Ok(None);
                                };
                                let branch_ref = format!("refs/heads/{branch}");
                                let exists = owner
                                    .command(
                                        &repository,
                                        &owner.0.project,
                                        &["git", "show-ref", "--verify", "--quiet", &branch_ref],
                                        None,
                                    )
                                    .await?;
                                if exists.exit_code == Some(1) {
                                    return Ok(None);
                                }
                                expect_ok(&exists, "git show-ref owned workspace branch")?;
                                let current = owner
                                    .command(
                                        &repository,
                                        &owner.0.project,
                                        &["git", "rev-parse", "--verify", &branch_ref],
                                        None,
                                    )
                                    .await?;
                                expect_ok(&current, "git rev-parse owned workspace branch")?;
                                Ok(Some((
                                    branch_ref,
                                    crate::metadata::js_trim(&current.stdout).to_owned(),
                                )))
                            }
                            .await;
                        let directory = owner
                            .remove_git_selected(&repository, &owner.0.project, cwd, directory)
                            .await;
                        directory?;
                        match owned_branch {
                            Ok(Some((branch_ref, expected))) => {
                                owner
                                    .require_unregistered_branch(
                                        &repository,
                                        &owner.0.project,
                                        &branch_ref,
                                    )
                                    .await?;
                                match owner
                                    .command(
                                        &repository,
                                        &owner.0.project,
                                        &["git", "update-ref", "-d", &branch_ref, &expected],
                                        None,
                                    )
                                    .await
                                {
                                    Ok(result) => expect_ok(
                                        &result,
                                        "git update-ref dispose workspace branch",
                                    ),
                                    Err(error) => Err(error),
                                }
                            }
                            Ok(None) => Ok(()),
                            Err(error) => Err(error),
                        }
                    } else {
                        let paths = owner.0.paths.clone();
                        let captured = owner
                            .io(move || {
                                workspace_copy::capture_selected(
                                    &paths,
                                    "snapshot-copy",
                                    &cwd,
                                    &directory,
                                )
                            })
                            .await?;
                        owner.io(move || captured.remove()).await?;
                        Ok(())
                    }
                }
                .await;
                // Every owned dispose failure must survive the job drain,
                // including Git command/CAS and IO errors. Preserve its
                // original message and missing flag; prepare uses its own domain.
                result.map_err(|mut error| {
                    error.code = "workspace_cleanup_failed".into();
                    error
                })
            })
        })
        .await
    }

    /// Retry only a previously owned, now removed object. No pathname removal
    /// or global worktree prune is authorized by this branch.
    async fn dispose_removed_git(
        &self,
        handle: &WorkspaceHandle,
        cwd: &Path,
    ) -> Result<(), WorkspaceError> {
        let denied = |message: &str| WorkspaceError::new("workspace_cleanup_failed", message);
        let selected = handle.created_identity.clone().ok_or_else(|| {
            denied("Missing workspace has no retained created identity; preserving its branch.")
        })?;
        let paths = self.0.paths.clone();
        let path = cwd.to_owned();
        self.io(move || {
            workspace_copy::removed_owned_workspace(&paths, "git-worktree", &path, &selected)
        })
        .await
        .map_err(|error| {
            denied(&format!(
                "Missing workspace identity is uncertain; preserving its branch: {error}"
            ))
        })?;
        let run = cwd
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| denied("Missing workspace run identity is invalid."))?;
        if !handle.run_id.equals_str(run)
            || !handle.project_key.equals_str(&self.0.paths.project_key)
        {
            return Err(denied(
                "Missing workspace does not match its original run/project identity.",
            ));
        }
        let branch = handle
            .metadata
            .get("branchName")
            .and_then(JsValue::as_string)
            .and_then(|v| v.to_utf8().ok())
            .filter(|v| v == &format!("always-on/{run}"))
            .ok_or_else(|| denied("Missing workspace branch metadata is invalid."))?;
        let base = handle
            .metadata
            .get("baseCommit")
            .and_then(JsValue::as_string)
            .and_then(|v| v.to_utf8().ok())
            .filter(|v| matches!(v.len(), 40 | 64) && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| denied("Missing workspace base commit metadata is invalid."))?;
        let root = self.0.project.clone();
        let repository = self.io(move || workspace_copy::pin_project(&root)).await?;
        let top = self
            .command(
                &repository,
                &self.0.project,
                &["git", "rev-parse", "--show-toplevel"],
                None,
            )
            .await?;
        expect_ok(&top, "git rev-parse cleanup repository")?;
        let top = PathBuf::from(crate::metadata::js_trim(&top.stdout));
        let original = handle
            .metadata
            .get("repoRoot")
            .and_then(JsValue::as_string)
            .and_then(|v| v.to_utf8().ok())
            .map(PathBuf::from)
            .ok_or_else(|| denied("Missing workspace repository metadata is invalid."))?;
        self.io(move || {
            if top.canonicalize()? != original.canonicalize()? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Workspace repository changed.",
                ));
            }
            Ok(())
        })
        .await?;
        let original = handle.created_repository.clone().ok_or_else(|| {
            denied(
                "Missing workspace has no retained Git repository identity; preserving its branch.",
            )
        })?;
        let common = self
            .command(
                &repository,
                &self.0.project,
                &[
                    "git",
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ],
                None,
            )
            .await?;
        expect_ok(&common, "git rev-parse missing workspace repository")?;
        let path = PathBuf::from(crate::metadata::js_trim(&common.stdout));
        self.io(move || {
            let current = workspace_copy::pin_project(&path)?;
            checked_directory_identity(&original, &current)
        })
        .await
        .map_err(|error| {
            denied(&format!(
                "Missing workspace Git repository identity changed; preserving its branch: {error}"
            ))
        })?;
        let reference = format!("refs/heads/{branch}");
        // A surviving registration could name an externally recreated directory
        // or checked-out branch. Never ask Git to remove that mutable pathname.
        let listed = self
            .command(
                &repository,
                &self.0.project,
                &["git", "worktree", "list", "--porcelain", "-z"],
                None,
            )
            .await?;
        expect_ok(&listed, "git worktree list missing workspace")?;
        if listed
            .stdout
            .split('\0')
            .any(|field| field == format!("branch {reference}"))
            || self
                .registered_worktree(&repository, &self.0.project, cwd)
                .await?
                .is_some()
        {
            return Err(denied(
                "Missing workspace still has a Git registration; preserving its branch for explicit recovery.",
            ));
        }
        let current = self
            .command(
                &repository,
                &self.0.project,
                &["git", "show-ref", "--verify", "--quiet", &reference],
                None,
            )
            .await?;
        if current.exit_code == Some(1) {
            return Ok(());
        }
        expect_ok(&current, "git show-ref missing workspace branch")?;
        let current = self
            .command(
                &repository,
                &self.0.project,
                &["git", "rev-parse", "--verify", &reference],
                None,
            )
            .await?;
        expect_ok(&current, "git rev-parse missing workspace branch")?;
        if crate::metadata::js_trim(&current.stdout) != base {
            return Err(denied(
                "Missing workspace branch changed from its original base commit; preserving it.",
            ));
        }
        let deleted = self
            .command(
                &repository,
                &self.0.project,
                &["git", "update-ref", "-d", &reference, &base],
                None,
            )
            .await?;
        expect_ok(&deleted, "git update-ref retry removed workspace branch")
    }

    async fn verify_git_project(
        &self,
        workspace: &Arc<File>,
        workspace_path: &Path,
        signal: Option<AbortSignal>,
    ) -> Result<(), WorkspaceError> {
        let root = self.0.project.clone();
        let project = self.io(move || workspace_copy::pin_project(&root)).await?;
        let selected = self
            .command(
                workspace,
                workspace_path,
                &[
                    "git",
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ],
                signal.clone(),
            )
            .await?;
        let expected = self
            .command(
                &project,
                &self.0.project,
                &[
                    "git",
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                ],
                signal,
            )
            .await?;
        expect_ok(&selected, "git rev-parse --git-common-dir")?;
        expect_ok(&expected, "git rev-parse --git-common-dir")?;
        let selected = PathBuf::from(crate::metadata::js_trim(&selected.stdout));
        let expected = PathBuf::from(crate::metadata::js_trim(&expected.stdout));
        self.io(move || {
            let selected = std::fs::metadata(selected.canonicalize()?)?;
            let expected = std::fs::metadata(expected.canonicalize()?)?;
            use std::os::unix::fs::MetadataExt;
            if selected.dev() != expected.dev() || selected.ino() != expected.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Workspace Git repository does not belong to the admitted project.",
                ));
            }
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn diff(
        &self,
        strategy: &str,
        cwd: JsString,
        signal: Option<AbortSignal>,
    ) -> Result<NativeWorkspaceDiff, WorkspaceError> {
        let strategy = strategy.to_owned();
        self.owned(move |owner| {
            Box::pin(async move { owner.diff_inner(&strategy, cwd, signal).await })
        })
        .await
    }
    async fn diff_inner(
        &self,
        strategy: &str,
        cwd: JsString,
        signal: Option<AbortSignal>,
    ) -> Result<NativeWorkspaceDiff, WorkspaceError> {
        let cwd = native_path(&cwd)?;
        let workspace_path = cwd.clone();
        let cwd_text = cwd.to_string_lossy().into_owned();
        let paths = self.0.paths.clone();
        let selected = strategy.to_owned();
        let workspace = self
            .io(move || workspace_copy::owned_workspace(&paths, &selected, &cwd))
            .await?;
        let (text, count) = if strategy == "git-worktree" {
            self.verify_git_project(&workspace, &workspace_path, signal.clone())
                .await?;
            let add = self
                .command(
                    &workspace,
                    &workspace_path,
                    &["git", "add", "-A"],
                    signal.clone(),
                )
                .await?;
            if add.exit_code != Some(0) {
                return Ok(empty_diff());
            }
            let stat = self
                .command(
                    &workspace,
                    &workspace_path,
                    &["git", "diff", "--cached", "HEAD", "--stat"],
                    signal.clone(),
                )
                .await?;
            let count = if stat.exit_code == Some(0) {
                stat.stdout
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    .saturating_sub(1)
            } else {
                0
            };
            let diff = self
                .command(
                    &workspace,
                    &workspace_path,
                    &["git", "diff", "--cached", "HEAD"],
                    signal,
                )
                .await?;
            (
                if diff.exit_code == Some(0) {
                    diff.stdout
                } else {
                    String::new()
                },
                count,
            )
        } else {
            let root = self.0.project.to_str().ok_or_else(|| {
                WorkspaceError::new("invalid_workspace", "Project root is not representable.")
            })?;
            // The command uses the identity-checked selected workspace path.
            let result = self
                .command(
                    &workspace,
                    &workspace_path,
                    &[
                        "diff",
                        "-ruN",
                        "--exclude=.git",
                        "--exclude=node_modules",
                        "--exclude=dist",
                        "--exclude=.pilotdeck",
                        "--exclude=.pilotdeck-always-on",
                        root,
                        &cwd_text,
                    ],
                    signal,
                )
                .await?;
            if result.exit_code.is_some_and(|code| code > 1) {
                return Ok(empty_diff());
            }
            let count = result
                .stdout
                .lines()
                .filter(|line| line.starts_with("diff "))
                .count();
            (result.stdout, count)
        };
        if crate::metadata::js_trim(&text).is_empty() {
            return Ok(NativeWorkspaceDiff {
                text: "".into(),
                file_count: count,
                truncated: false,
            });
        }
        let units: Vec<_> = text.encode_utf16().collect();
        let truncated = units.len() > DIFF_UNITS;
        Ok(NativeWorkspaceDiff {
            text: JsString::from_utf16(units.into_iter().take(DIFF_UNITS).collect()),
            file_count: count,
            truncated,
        })
    }
    async fn command(
        &self,
        directory: &Arc<File>,
        directory_path: &Path,
        args: &[&str],
        signal: Option<AbortSignal>,
    ) -> Result<CommandResult, WorkspaceError> {
        let command = args
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let mut env = self.0.env.clone().unwrap_or_else(|| {
            std::env::vars_os()
                .map(|(key, value)| {
                    (
                        key.to_string_lossy().as_ref().into(),
                        value.to_string_lossy().as_ref().into(),
                    )
                })
                .collect()
        });
        // Internal argv commands must not execute a user's login-shell rc
        // that changes cwd after the identity-checked child chdir.
        env.retain(|(key, _)| !key.equals_str("PILOTDECK_SHELL_PATH"));
        env.push(("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into()));
        // Darwin /dev/fd directory aliases cannot be used by chdir. Follow the
        // TS path-based spawn contract, checking against the selected directory
        // descriptor before and after the owned command. This detects a changed
        // path, but does not eliminate a same-user rename-and-restore spawn race.
        let selected = directory.clone();
        let path = directory_path.to_owned();
        let cwd = self
            .io(move || checked_command_path(&selected, &path))
            .await?;
        let cwd_text = cwd.to_str().ok_or_else(|| {
            WorkspaceError::new("invalid_workspace", "Command cwd is not representable.")
        })?;
        let result = self
            .0
            .runner
            .run(
                command.as_str().into(),
                CommandOptions {
                    cwd: cwd_text.into(),
                    env: Some(env),
                    timeout_ms: 60_000.0,
                    signal,
                    on_stdout: None,
                    on_stderr: None,
                    max_output_units: Some(COMMAND_UNITS + 64 * 1024 + 8),
                },
            )
            .await
            .map_err(|error| {
                WorkspaceError::new(
                    if error.code == "owned_command_not_quiescent" {
                        "workspace_cleanup_failed"
                    } else {
                        "workspace_prepare_failed"
                    },
                    error.message,
                )
            })?;
        // CommandRunner retains an owned 3s kill/reap timer after TERM. Do
        // not release our generation/job admission while that group lives.
        if let Some(group) = result
            .pid
            .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
        {
            crate::process_group::wait_for_group_empty(
                Some(group),
                std::time::Duration::from_secs(4),
            )
            .await;
            if crate::process_group::group_exists(group) {
                return Err(WorkspaceError::new(
                    "workspace_cleanup_failed",
                    "Always-On command group did not quiesce.",
                ));
            }
        }
        let selected = directory.clone();
        self.io(move || checked_command_path(&selected, &cwd).map(|_| ()))
            .await?;
        if result.timed_out {
            return Err(WorkspaceError::new(
                "workspace_prepare_failed",
                "Always-On workspace command timed out.",
            ));
        }
        if result.stdout.encode_utf16().count() >= COMMAND_UNITS
            || result.stderr.encode_utf16().count() >= COMMAND_UNITS
        {
            return Err(WorkspaceError::new(
                "workspace_resource_limit",
                "Always-On workspace command output exceeded its native resource limit.",
            ));
        }
        Ok(result)
    }
}
fn checked_directory_identity(selected: &File, current: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let selected = selected.metadata()?;
    let current = current.metadata()?;
    if selected.dev() != current.dev() || selected.ino() != current.ino() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Created workspace was replaced before cleanup.",
        ));
    }
    Ok(())
}
fn checked_command_path(directory: &File, path: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let canonical = path.canonicalize()?;
    let selected = directory.metadata()?;
    let current = std::fs::metadata(&canonical)?;
    if !current.is_dir() || current.dev() != selected.dev() || current.ino() != selected.ino() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Always-On command cwd no longer matches the admitted directory.",
        ));
    }
    Ok(canonical)
}
fn native_path(value: &JsString) -> Result<PathBuf, WorkspaceError> {
    value.to_utf8().map(PathBuf::from).map_err(|_| {
        WorkspaceError::new(
            "invalid_workspace",
            "Workspace path is not representable by the native filesystem.",
        )
    })
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn expect_ok(result: &CommandResult, label: &str) -> Result<(), WorkspaceError> {
    if result.exit_code == Some(0) {
        Ok(())
    } else {
        Err(WorkspaceError::new(
            "workspace_prepare_failed",
            format!(
                "{label} failed: {}",
                if result.stderr.is_empty() {
                    &result.stdout
                } else {
                    &result.stderr
                }
            ),
        ))
    }
}
fn empty_diff() -> NativeWorkspaceDiff {
    NativeWorkspaceDiff {
        text: "".into(),
        file_count: 0,
        truncated: false,
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
