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

struct ReplacingCwdRunner;
impl CommandRunner for ReplacingCwdRunner {
    fn run(&self, _: JsString, options: CommandOptions) -> ToolFuture<CommandResult> {
        Box::pin(async move {
            let cwd = PathBuf::from(options.cwd.to_utf8().unwrap());
            std::fs::rename(&cwd, cwd.with_extension("original")).unwrap();
            std::fs::create_dir(&cwd).unwrap();
            Ok(CommandResult { exit_code: Some(0), stdout: "must not succeed".into(), stderr: String::new(), timed_out: false, duration_ms: 0.0, pid: None })
        })
    }
}
#[tokio::test]
async fn command_cwd_is_identity_checked_before_and_after_spawn() {
    let fixture = Fixture::new();
    let cwd = fixture.root.0.join("selected space 'quoted'"); std::fs::create_dir(&cwd).unwrap();
    let selected = workspace_copy::pin_project(&cwd).unwrap();
    let other = fixture.root.0.join("foreign"); std::fs::create_dir(&other).unwrap();
    let provider = fixture.provider(Arc::new(ReplacingCwdRunner));
    // Mismatch must reject before invoking a runner which mutates its cwd.
    let before = provider.command(&selected, &other, &["pwd"], None).await;
    assert!(before.is_err()); assert!(!other.with_extension("original").exists());
    let after = provider.command(&selected, &cwd, &["pwd"], None).await;
    assert!(after.is_err(), "a successful child result cannot hide cwd replacement");
    assert!(cwd.with_extension("original").exists());
    assert!(provider.close().await.is_ok()); assert!(fixture.registry.dispose().await.is_ok());
}
#[tokio::test]
async fn command_uses_a_real_canonical_cwd_with_spaces_and_aliases() {
    let fixture = Fixture::new();
    let cwd = fixture.root.0.join("selected space 'quoted'"); std::fs::create_dir(&cwd).unwrap();
    let alias = fixture.root.0.join("alias"); std::os::unix::fs::symlink(&cwd, &alias).unwrap();
    let selected = workspace_copy::pin_project(&alias).unwrap();
    let provider = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let result = provider.command(&selected, &alias, &["pwd", "-P"], None).await;
    let closed = provider.close().await; let drained = fixture.registry.dispose().await;
    assert!(closed.is_ok()); assert!(drained.is_ok());
    let result = result.unwrap(); assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), cwd.canonicalize().unwrap().to_str().unwrap());
}

#[tokio::test]
async fn failed_git_post_checkout_rolls_back_only_its_created_tree_and_branch() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(); let root = Path::new(&fixture.paths.project_key); fixture.initialize_git(root).await;
    let hook = root.join(".git/hooks/post-checkout"); std::fs::write(&hook, "#!/bin/sh\nexit 19\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let provider = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    let refused = provider.prepare("git-worktree", "hook-failure".into(), "fixture".into(), None).await;
    assert!(refused.is_err()); assert!(!fixture.paths.worktrees_dir().join("hook-failure").exists());
    assert_ne!(fixture.git(root, &["show-ref", "--verify", "--quiet", "refs/heads/always-on/hook-failure"]).await.exit_code, Some(0));
    let listed = fixture.git(root, &["worktree", "list", "--porcelain"]).await.stdout;
    assert!(!listed.contains("hook-failure"), "failed prepare must leave no owned registration");
    std::fs::remove_file(hook).unwrap();
    let handle = provider.prepare("git-worktree", "hook-failure".into(), "retry".into(), None).await.unwrap();
    provider.dispose(handle, false).await.unwrap(); provider.close().await.unwrap(); fixture.registry.dispose().await.unwrap();
}
#[tokio::test]
async fn failed_prepare_preserves_existing_branch_and_replaced_owned_directory() {
    let fixture = Fixture::new(); let root = Path::new(&fixture.paths.project_key); fixture.initialize_git(root).await;
    assert_eq!(fixture.git(root, &["branch", "always-on/foreign"]).await.exit_code, Some(0));
    let foreign = fixture.paths.worktrees_dir().join("foreign"); std::fs::create_dir_all(&foreign).unwrap(); std::fs::write(foreign.join("sentinel"), "FOREIGN").unwrap();
    let provider = fixture.provider(Arc::new(NativeOwnedShellCommandRunner));
    assert!(provider.prepare("git-worktree", "foreign".into(), "fixture".into(), None).await.is_err());
    assert_eq!(fixture.git(root, &["show-ref", "--verify", "--quiet", "refs/heads/always-on/foreign"]).await.exit_code, Some(0));
    assert_eq!(std::fs::read(foreign.join("sentinel")).unwrap(), b"FOREIGN");
    let handle = provider.prepare("snapshot-copy", "replace".into(), "fixture".into(), None).await.unwrap();
    let original = native_path(&handle.cwd).unwrap(); let retained = original.with_extension("kept"); std::fs::rename(&original, &retained).unwrap();
    std::fs::create_dir(&original).unwrap(); std::fs::write(original.join("sentinel"), "REPLACEMENT").unwrap();
    assert!(provider.dispose(handle, false).await.is_err(), "fresh handle must reject a replaced directory");
    assert_eq!(std::fs::read(original.join("sentinel")).unwrap(), b"REPLACEMENT"); assert!(retained.exists());
    provider.close().await.unwrap(); fixture.registry.dispose().await.unwrap();
}

struct RemoveFailureRunner {commands:Arc<Mutex<Vec<String>>>}
impl CommandRunner for RemoveFailureRunner {
    fn run(&self,command:JsString,options:CommandOptions)->ToolFuture<CommandResult>{let commands=self.commands.clone();Box::pin(async move{let text=command.to_utf8().unwrap();commands.lock().unwrap().push(text.clone());if text.contains("'worktree' 'remove'"){return Err(crate::runtime::tools::ToolError::new("injected_remove_failure","owned remove failed"));}NativeOwnedShellCommandRunner.run(command,options).await})}
}
#[tokio::test]
async fn rollback_attempts_the_real_branch_cas_even_after_owned_directory_cleanup_fails(){
    use std::os::unix::fs::PermissionsExt;
    let fixture=Fixture::new();let project=Path::new(&fixture.paths.project_key);fixture.initialize_git(project).await;let hook=project.join(".git/hooks/post-checkout");std::fs::write(&hook,"#!/bin/sh\nexit 19\n").unwrap();std::fs::set_permissions(&hook,std::fs::Permissions::from_mode(0o700)).unwrap();let commands=Arc::new(Mutex::new(Vec::new()));let provider=fixture.provider(Arc::new(RemoveFailureRunner{commands:commands.clone()}));let refused=provider.prepare("git-worktree","remove-failure".into(),"fixture".into(),None).await;let closed=provider.close().await;let drained=fixture.registry.dispose().await;
    assert!(refused.is_err());assert!(closed.is_err());assert!(drained.is_ok());assert_ne!(fixture.git(project,&["show-ref","--verify","--quiet","refs/heads/always-on/remove-failure"]).await.exit_code,Some(0));let commands=commands.lock().unwrap();let remove=commands.iter().position(|c|c.contains("'worktree' 'remove'")).unwrap();let cas=commands.iter().position(|c|c.contains("'update-ref' '-d'")).unwrap();assert!(cas>remove,"independent cleanup must not short-circuit: {commands:?}");
}
struct OriginalReplacementRunner {original:PathBuf,entered:Arc<tokio::sync::Notify>,release:Arc<tokio::sync::Notify>}
impl CommandRunner for OriginalReplacementRunner {
    fn run(&self,command:JsString,options:CommandOptions)->ToolFuture<CommandResult>{let original=self.original.clone();let entered=self.entered.clone();let release=self.release.clone();Box::pin(async move{if command.to_utf8().unwrap().contains("'worktree' 'repair'"){assert!(!original.exists());entered.notify_one();release.notified().await;}NativeOwnedShellCommandRunner.run(command,options).await})}
}
#[tokio::test]
async fn git_cleanup_barrier_preserves_a_replacement_at_the_original_path(){
    let fixture=Fixture::new();let project=Path::new(&fixture.paths.project_key);fixture.initialize_git(project).await;let setup=fixture.provider(Arc::new(NativeOwnedShellCommandRunner));let handle=setup.prepare("git-worktree","rename-window".into(),"fixture".into(),None).await.unwrap();setup.close().await.unwrap();let original=native_path(&handle.cwd).unwrap();let entered=Arc::new(tokio::sync::Notify::new());let release=Arc::new(tokio::sync::Notify::new());let provider=fixture.provider(Arc::new(OriginalReplacementRunner{original:original.clone(),entered:entered.clone(),release:release.clone()}));let worker=provider.clone();let removal=tokio::spawn(async move{worker.dispose(handle,false).await});
    let waiting=tokio::time::timeout(Duration::from_secs(10),entered.notified()).await;let replacement=if waiting.is_ok(){std::fs::create_dir(&original).and_then(|_|std::fs::write(original.join("foreign"),"FOREIGN"))}else{Err(io::Error::other("repair barrier was not reached"))};release.notify_one();let removed=tokio::time::timeout(Duration::from_secs(15),removal).await;let closed=provider.close().await;let drained=fixture.registry.dispose().await;
    assert!(waiting.is_ok());replacement.unwrap();removed.unwrap().unwrap().unwrap();assert!(closed.is_ok());assert!(drained.is_ok());assert_eq!(std::fs::read(original.join("foreign")).unwrap(),b"FOREIGN");assert_ne!(fixture.git(project,&["show-ref","--verify","--quiet","refs/heads/always-on/rename-window"]).await.exit_code,Some(0));
}
