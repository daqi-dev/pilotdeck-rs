use super::{
    AlwaysOnError,
    storage::{Store, safe_id},
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};
use tokio::process::Command;

const IGNORES: [&str; 5] = [
    ".git",
    "node_modules",
    "dist",
    ".pilotdeck",
    ".pilotdeck-always-on",
];
const SNAPSHOT_CAP: u64 = 1024 * 1024 * 1024;

async fn command(bin: &str, args: &[&str]) -> Result<Output, AlwaysOnError> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(Into::into)
}

async fn git(root: &Path, args: &[&str]) -> Result<String, AlwaysOnError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(AlwaysOnError::new(
            "workspace_prepare_failed",
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().into())
}

/// Effects used by the explicit GitWorktreeProvider dispose operation. Hosts
/// may inject a runner without executing filesystem or Git cleanup commands.
pub trait GitWorktreeCleanup: Send + Sync {
    fn git<'a>(&'a self, root: &'a Path, args: &'a [&'a str]) -> super::HostFuture<'a, bool>;
    fn remove_tree<'a>(&'a self, cwd: &'a Path) -> super::HostFuture<'a, ()>;
}

pub struct NativeGitWorktreeCleanup;
impl GitWorktreeCleanup for NativeGitWorktreeCleanup {
    fn git<'a>(&'a self, root: &'a Path, args: &'a [&'a str]) -> super::HostFuture<'a, bool> {
        Box::pin(async move {
            Ok(Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?
                .status
                .success())
        })
    }
    fn remove_tree<'a>(&'a self, cwd: &'a Path) -> super::HostFuture<'a, ()> {
        Box::pin(async move {
            let metadata = match fs::symlink_metadata(cwd) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let removed = if metadata.is_dir() {
                fs::remove_dir_all(cwd)
            } else {
                fs::remove_file(cwd)
            };
            match removed {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        })
    }
}

/// Explicit provider disposal, matching `GitWorktreeProvider.ts:86-113`.
/// This API is not called automatically by an Always-On run or UI cleanup.
pub async fn dispose_git_worktree(
    workspace: &Value,
    keep: bool,
    host: Option<&dyn super::AlwaysOnHost>,
) -> Result<(), AlwaysOnError> {
    dispose_git_worktree_with(workspace, keep, host, &NativeGitWorktreeCleanup).await
}

pub async fn dispose_git_worktree_with(
    workspace: &Value,
    keep: bool,
    host: Option<&dyn super::AlwaysOnHost>,
    cleanup: &dyn GitWorktreeCleanup,
) -> Result<(), AlwaysOnError> {
    if keep {
        return Ok(());
    }
    let cwd = workspace["cwd"].as_str().ok_or_else(|| {
        AlwaysOnError::new(
            "missing_workspace",
            "Workspace cwd is required for disposal",
        )
    })?;
    let path = Path::new(cwd);
    // The TS callback precedes all cleanup attempts, including failed ones.
    if let Some(host) = host {
        host.on_worktree_removed(path);
    }
    let repo = Path::new(workspace["metadata"]["repoRoot"].as_str().unwrap_or(cwd));
    let removed = cleanup
        .git(repo, &["worktree", "remove", "--force", cwd])
        .await
        .unwrap_or(false);
    if !removed {
        cleanup.remove_tree(path).await?;
        let _ = cleanup.git(repo, &["worktree", "prune"]).await;
    }
    if let Some(branch) = workspace["metadata"]["branchName"]
        .as_str()
        .filter(|name| !name.is_empty())
    {
        let _ = cleanup.git(repo, &["branch", "-D", branch]).await;
    }
    Ok(())
}

pub fn validate(store: &Store, workspace: &Value) -> Result<(), AlwaysOnError> {
    let cwd = workspace["cwd"].as_str().ok_or_else(|| {
        AlwaysOnError::new(
            "missing_workspace",
            "Cycle has no associated workspace to apply",
        )
    })?;
    let cwd = fs::canonicalize(cwd)
        .map_err(|e| AlwaysOnError::new("workspace_unavailable", e.to_string()))?;
    let project = fs::canonicalize(&store.project)?;
    let base = match workspace["strategy"].as_str() {
        Some("git-worktree") => &store.worktrees,
        Some("snapshot-copy") => &store.snapshots,
        _ => {
            return Err(AlwaysOnError::new(
                "workspace_unavailable",
                "Unknown workspace strategy",
            ));
        }
    };
    let base = fs::canonicalize(base)?;
    if cwd == project || cwd == base || !cwd.starts_with(base) || !cwd.is_dir() {
        return Err(AlwaysOnError::new(
            "workspace_unavailable",
            "Workspace must be an isolated directory inside its configured base",
        ));
    }
    Ok(())
}

pub async fn prepare(
    store: &Store,
    run: &str,
    title: &str,
    strategy: &str,
) -> Result<Value, AlwaysOnError> {
    prepare_notified(store, run, title, strategy, None).await
}

pub(super) async fn prepare_notified(
    store: &Store,
    run: &str,
    title: &str,
    strategy: &str,
    host: Option<&dyn super::AlwaysOnHost>,
) -> Result<Value, AlwaysOnError> {
    prepare_inner(store, run, title, strategy, host)
        .await
        .map_err(|mut error| {
            if error.code == "storage_failed" {
                error.code = "workspace_prepare_failed".into();
            }
            error
        })
}

async fn prepare_inner(
    store: &Store,
    run: &str,
    title: &str,
    strategy: &str,
    host: Option<&dyn super::AlwaysOnHost>,
) -> Result<Value, AlwaysOnError> {
    let id = safe_id(run);
    if matches!(id.as_str(), "." | "..") {
        return Err(AlwaysOnError::new(
            "workspace_prepare_failed",
            "Invalid workspace run id",
        ));
    }
    let repo = git(&store.project, &["rev-parse", "--show-toplevel"])
        .await
        .ok();
    let has_head = repo.is_some() && git(&store.project, &["rev-parse", "HEAD"]).await.is_ok();
    let use_git = match strategy {
        "git-worktree" => {
            if !has_head {
                return Err(AlwaysOnError::new(
                    "workspace_unavailable",
                    "Git repository with HEAD required",
                ));
            }
            true
        }
        "snapshot-copy" => false,
        "auto" => has_head,
        _ => {
            return Err(AlwaysOnError::new(
                "workspace_unavailable",
                "Unknown workspace strategy",
            ));
        }
    };
    if use_git {
        let root =
            repo.ok_or_else(|| AlwaysOnError::new("workspace_unavailable", "Git root missing"))?;
        let root = Path::new(&root);
        if !git(root, &["status", "--porcelain"])
            .await?
            .trim()
            .is_empty()
        {
            git(root, &["add", "-A"]).await?;
            let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
            let message = format!(
                "chore(always-on): checkpoint before executing {}",
                if title.is_empty() {
                    "untitled plan"
                } else {
                    &title
                }
            );
            git(root, &["commit", "-m", &message]).await?;
            if !git(root, &["status", "--porcelain"])
                .await?
                .trim()
                .is_empty()
            {
                return Err(AlwaysOnError::new(
                    "workspace_prepare_failed",
                    "Git worktree remained dirty after checkpoint",
                ));
            }
        }
        let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        let commit = git(root, &["rev-parse", "HEAD"]).await?;
        fs::create_dir_all(&store.worktrees)?;
        let cwd = store.worktrees.join(id);
        let branch_name = format!("always-on/{run}");
        git(
            root,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &cwd.to_string_lossy(),
                &commit,
            ],
        )
        .await?;
        // GitWorktreeProvider.ts:65 fires immediately after successful add,
        // including when the owning phase is cancelled before it returns.
        if let Some(host) = host {
            host.on_worktree_created(run, &cwd);
        }
        let workspace = json!({"strategy":"git-worktree", "cwd":cwd, "metadata":{"repoRoot":root,"baseBranch":branch,"baseCommit":commit,"branchName":branch_name}});
        validate(store, &workspace)?;
        Ok(workspace)
    } else {
        if !store.project.is_dir() {
            return Err(AlwaysOnError::new(
                "workspace_unavailable",
                "Project directory missing",
            ));
        }
        fs::create_dir_all(&store.snapshots)?;
        let target = store.snapshots.join(id);
        let source = fs::canonicalize(&store.project)?;
        let parent = fs::canonicalize(&store.snapshots)?;
        if parent.starts_with(&source) {
            return Err(AlwaysOnError::new(
                "workspace_prepare_failed",
                "Snapshot target must not be inside source",
            ));
        }
        if target.try_exists()? {
            return Err(AlwaysOnError::new(
                "workspace_prepare_failed",
                "Snapshot target already exists",
            ));
        }
        let estimated = command("du", &["-sk", &source.to_string_lossy()])
            .await
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .unwrap_or(0)
            .saturating_mul(1024);
        if estimated > SNAPSHOT_CAP {
            return Err(AlwaysOnError::new(
                "workspace_prepare_failed",
                format!("snapshot source size {estimated} exceeds maxBytes {SNAPSHOT_CAP}."),
            ));
        }
        let copy_strategy = if cfg!(target_os = "macos") {
            "clonefile"
        } else {
            "reflink"
        };
        let args = if cfg!(target_os = "macos") {
            vec!["-c", "-R"]
        } else {
            vec!["--reflink=auto", "-R"]
        };
        let mut args = args;
        let source_text = source.to_string_lossy();
        let target_text = target.to_string_lossy();
        args.push(&source_text);
        args.push(&target_text);
        let cloned = command("cp", &args)
            .await
            .is_ok_and(|output| output.status.success())
            && target.exists();
        let copy_strategy = if cloned {
            for ignored in IGNORES {
                let path = target.join(ignored);
                if let Ok(metadata) = path.symlink_metadata() {
                    if metadata.is_dir() {
                        let _ = fs::remove_dir_all(path);
                    } else {
                        let _ = fs::remove_file(path);
                    }
                }
            }
            copy_strategy
        } else {
            let from = source.clone();
            let to = target.clone();
            tokio::task::spawn_blocking(move || copy_tree(&from, &to, true))
                .await
                .map_err(|e| AlwaysOnError::new("workspace_prepare_failed", e.to_string()))??;
            "fs.cp"
        };
        let workspace = json!({"strategy":"snapshot-copy","cwd":target,"metadata":{"copyStrategy":copy_strategy,"baseSize":estimated.to_string()}});
        validate(store, &workspace)?;
        Ok(workspace)
    }
}

fn copy_tree(source: &Path, target: &Path, root: bool) -> Result<(), AlwaysOnError> {
    fs::create_dir_all(target)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if root && IGNORES.contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let metadata = entry.path().symlink_metadata()?;
        let dest = target.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree(&entry.path(), &dest, false)?;
        } else if metadata.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, dest)?;
            #[cfg(not(unix))]
            return Err(AlwaysOnError::new(
                "workspace_prepare_failed",
                "Symlink copy unsupported on this platform",
            ));
        } else if metadata.is_file() {
            fs::copy(entry.path(), dest)?;
        } else {
            return Err(AlwaysOnError::new(
                "workspace_prepare_failed",
                "Unsupported special file in snapshot",
            ));
        }
    }
    fs::set_permissions(target, source.metadata()?.permissions())?;
    Ok(())
}

pub async fn diff(store: &Store, workspace: &Value) -> Result<Value, AlwaysOnError> {
    validate(store, workspace)?;
    let cwd = workspace["cwd"].as_str().expect("workspace validated");
    let (text, count) = if workspace["strategy"] == "git-worktree" {
        git(Path::new(cwd), &["add", "-A"]).await?;
        let stat = git(Path::new(cwd), &["diff", "--cached", "HEAD", "--stat"]).await?;
        (
            git(Path::new(cwd), &["diff", "--cached", "HEAD"]).await?,
            stat.lines().count().saturating_sub(1),
        )
    } else {
        let root = store.project.to_string_lossy();
        let output = command(
            "diff",
            &[
                "-ruN",
                "--exclude=.git",
                "--exclude=node_modules",
                "--exclude=dist",
                "--exclude=.pilotdeck",
                "--exclude=.pilotdeck-always-on",
                &root,
                cwd,
            ],
        )
        .await?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(AlwaysOnError::new(
                "workspace_diff_failed",
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let count = text
            .lines()
            .filter(|line| line.starts_with("diff "))
            .count();
        (text, count)
    };
    let truncated = text.encode_utf16().count() > 80_000;
    let mut units = 0;
    let text = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= 80_000
        })
        .collect::<String>();
    Ok(json!({"diff":text,"fileCount":count,"truncated":truncated}))
}

#[cfg(test)]
#[path = "workspace/dispose_tests.rs"]
mod dispose_tests;
