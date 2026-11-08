use super::*;
use crate::runtime::{project::{ProjectRuntimeRegistry, ResourceBundle}, tools::{ToolFuture, builtin::bash::runner::NativeOwnedShellCommandRunner}};
use std::time::Duration;
struct Scratch(PathBuf);
impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
struct Fixture { root: Scratch, paths: AlwaysOnPaths, registry: Arc<ProjectRuntimeRegistry> }
impl Fixture {
    fn new() -> Self {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-always-provider-{}", uuid::Uuid::new_v4())));
        let project = root.0.join("project"); std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(root.0.join("home")).unwrap();
        let dir = root.0.join("home/always-on/projects/fixture");
        let paths = AlwaysOnPaths { project_key: project.to_string_lossy().into_owned(), state_file: dir.join("state.json"), plans_dir: dir.join("plans"), plan_index_file: dir.join("plans/index.json"), cycle_index_file: dir.join("cycles/index.json"), reports_dir: dir.join("reports"), discovery_lock_file: dir.join("locks/discovery.lock") };
        let registry = Arc::new(ProjectRuntimeRegistry::new(project, Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(()))));
        Self { root, paths, registry }
    }
    fn env(&self) -> Vec<(JsString, JsString)> {
        vec![("PATH".into(), "/usr/bin:/bin".into()), ("HOME".into(), self.root.0.join("home").to_string_lossy().as_ref().into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()), ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_AUTHOR_NAME".into(), "Fixture".into()), ("GIT_AUTHOR_EMAIL".into(), "fixture@example.invalid".into()),
            ("GIT_COMMITTER_NAME".into(), "Fixture".into()), ("GIT_COMMITTER_EMAIL".into(), "fixture@example.invalid".into()),
            ("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into())]
    }
    fn provider(&self, runner: Arc<dyn CommandRunner>) -> NativeWorkspace {
        NativeWorkspace::new(self.paths.clone(), Arc::new(self.registry.acquire(None).unwrap()), runner, Some(self.env())).unwrap()
    }
    async fn git(&self, cwd: &Path, args: &[&str]) -> CommandResult {
        let command = std::iter::once("git").chain(args.iter().copied()).map(quote).collect::<Vec<_>>().join(" ");
        NativeOwnedShellCommandRunner.run(command.as_str().into(), CommandOptions { cwd: cwd.to_string_lossy().as_ref().into(), env: Some(self.env()), timeout_ms: 10_000.0, signal: None, on_stdout: None, on_stderr: None, max_output_units: Some(1024 * 1024) }).await.unwrap()
    }
    async fn initialize_git(&self, cwd: &Path) {
        assert_eq!(self.git(cwd, &["init", "-q"]).await.exit_code, Some(0));
        std::fs::write(cwd.join("tracked.txt"), "INITIAL\n").unwrap();
        assert_eq!(self.git(cwd, &["add", "-A"]).await.exit_code, Some(0));
        assert_eq!(self.git(cwd, &["commit", "-qm", "fixture initial"]).await.exit_code, Some(0));
    }
}

#[tokio::test]
async fn snapshot_provider_persists_and_re_admits_a_real_old_generation_cycle() {
    let fixture = Fixture::new(); let source = Path::new(&fixture.paths.project_key);
    std::fs::write(source.join("input.txt"), "ORIGINAL\n").unwrap();
    let first = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let handle = first.prepare("snapshot-copy", "snapshot-run".into(), "fixture".into(), None).await.unwrap();
    let handle_copy = handle.clone(); let paths = fixture.paths.clone();
    storage::run(move || { super::super::cycles::WorkCycleStore::new(paths).create(&handle_copy.cycle_workspace(), handle_copy.run_id, "cycle".into(), 0.0).map(|_| ()) }).await.unwrap();
    first.close().await.unwrap(); fixture.registry.invalidate(None);
    let second = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let cycle = super::super::cycles::WorkCycleStore::new(fixture.paths.clone()).read_apply("cycle".into()).await.unwrap().unwrap().0;
    let restored = second.authorize_cycle(cycle.clone()).await.unwrap();
    let target = native_path(&restored.cwd).unwrap(); std::fs::write(target.join("input.txt"), "WORKSPACE ONLY\n").unwrap();
    let diff = second.diff("snapshot-copy", restored.cwd.clone(), None).await;
    let mut foreign = cycle; foreign.workspace.as_mut().unwrap().cwd = source.to_string_lossy().as_ref().into();
    let refused = second.authorize_cycle(foreign).await;
    let disposed = second.dispose(restored, false).await;
    let closed = second.close().await; let drained = fixture.registry.dispose().await;
    assert!(closed.is_ok()); assert!(drained.is_ok()); assert!(disposed.is_ok()); assert!(refused.is_err());
    let diff = diff.unwrap(); let text = diff.text.to_utf8().unwrap();
    assert!(text.contains("WORKSPACE ONLY") && text.contains("ORIGINAL")); assert_eq!(diff.file_count, 1);
    assert_eq!(std::fs::read(source.join("input.txt")).unwrap(), b"ORIGINAL\n"); assert!(!target.exists());
}

#[tokio::test]
async fn real_git_checkpoint_worktree_diff_restart_and_foreign_repository_refusal() {
    let fixture = Fixture::new(); let source = Path::new(&fixture.paths.project_key); fixture.initialize_git(source).await;
    let initial = fixture.git(source, &["rev-parse", "HEAD"]).await.stdout;
    std::fs::write(source.join("tracked.txt"), "CHECKPOINT\n").unwrap();
    let first = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let handle = first.prepare("auto", "git-run".into(), "  fixture\n checkpoint  ".into(), None).await.unwrap();
    assert_eq!(handle.strategy, "git-worktree");
    let after = fixture.git(source, &["rev-parse", "HEAD"]).await.stdout;
    let message = fixture.git(source, &["log", "-1", "--format=%s"]).await.stdout;
    let status = fixture.git(source, &["status", "--porcelain"]).await.stdout;
    let paths = fixture.paths.clone(); let handle_copy = handle.clone();
    storage::run(move || { super::super::cycles::WorkCycleStore::new(paths).create(&handle_copy.cycle_workspace(), handle_copy.run_id, "cycle".into(), 0.0).map(|_| ()) }).await.unwrap();
    first.close().await.unwrap(); fixture.registry.invalidate(None);
    let second = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let cycle = super::super::cycles::WorkCycleStore::new(fixture.paths.clone()).read_apply("cycle".into()).await.unwrap().unwrap().0;
    let restored = second.authorize_cycle(cycle).await.unwrap(); let target = native_path(&restored.cwd).unwrap();
    std::fs::write(target.join("tracked.txt"), "CHILD CHANGE\n").unwrap(); std::fs::write(target.join("created.txt"), "NEW FILE\n").unwrap();
    let diff = second.diff("git-worktree", restored.cwd.clone(), None).await;
    // Being under this project's base is insufficient: an independently
    // initialized repository there must never be staged by the apply helper.
    let foreign = fixture.paths.worktrees_dir().join("foreign"); std::fs::create_dir_all(&foreign).unwrap(); fixture.initialize_git(&foreign).await;
    std::fs::write(foreign.join("do-not-stage"), "FOREIGN\n").unwrap();
    let refused = second.diff("git-worktree", foreign.to_string_lossy().as_ref().into(), None).await;
    let foreign_status = fixture.git(&foreign, &["status", "--porcelain"]).await.stdout;
    let disposed = second.dispose(restored, false).await; let closed = second.close().await; let drained = fixture.registry.dispose().await;
    assert!(closed.is_ok()); assert!(drained.is_ok()); assert!(disposed.is_ok()); assert!(refused.is_err());
    assert_ne!(initial, after); assert!(status.is_empty());
    assert_eq!(message.trim(), "chore(always-on): checkpoint before executing fixture checkpoint");
    let diff = diff.unwrap(); let text = diff.text.to_utf8().unwrap(); assert!(text.contains("CHILD CHANGE") && text.contains("created.txt")); assert_eq!(diff.file_count, 2);
    assert!(foreign_status.lines().any(|line| line == "?? do-not-stage")); assert!(!target.exists());
    assert_eq!(std::fs::read(source.join("tracked.txt")).unwrap(), b"CHECKPOINT\n");
}

struct DelayedRunner { common: PathBuf, entered: Arc<tokio::sync::Notify>, release: Arc<tokio::sync::Notify> }
impl CommandRunner for DelayedRunner {
    fn run(&self, command: JsString, _: CommandOptions) -> ToolFuture<CommandResult> {
        let common = self.common.to_string_lossy().into_owned(); let entered = self.entered.clone(); let release = self.release.clone();
        Box::pin(async move {
            let command = command.to_utf8().unwrap();
            if command.contains("'add'") { entered.notify_one(); release.notified().await; }
            let stdout = if command.contains("--git-common-dir") { common } else { String::new() };
            Ok(CommandResult { exit_code: Some(0), stdout, stderr: String::new(), timed_out: false, duration_ms: 0.0, pid: None })
        })
    }
}
#[tokio::test]
async fn caller_cancellation_does_not_release_the_generation_or_skip_close_drain() {
    let fixture = Fixture::new(); let common = Path::new(&fixture.paths.project_key).join(".git"); std::fs::create_dir_all(&common).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new()); let release = Arc::new(tokio::sync::Notify::new());
    let provider = fixture.provider(Arc::new(DelayedRunner { common, entered: entered.clone(), release: release.clone() }));
    let target = fixture.paths.worktrees_dir().join("owned"); std::fs::create_dir_all(&target).unwrap();
    let worker = provider.clone(); let caller = tokio::spawn(async move { worker.diff("git-worktree", target.to_string_lossy().as_ref().into(), None).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await.unwrap();
    caller.abort(); let _ = caller.await;
    let runtime = fixture.registry.resolve(None).unwrap();
    let closing = provider.clone(); let close = tokio::spawn(async move { closing.close().await });
    tokio::task::yield_now().await; let prematurely_closed = close.is_finished(); let held = runtime.lease_count();
    release.notify_one(); let closed = tokio::time::timeout(Duration::from_secs(5), close).await.unwrap().unwrap();
    let drained = fixture.registry.dispose().await;
    assert!(!prematurely_closed, "close must wait for an accepted operation after its caller vanished"); assert!(held > 0); assert!(closed.is_ok()); assert!(drained.is_ok()); assert_eq!(runtime.lease_count(), 0);
}
