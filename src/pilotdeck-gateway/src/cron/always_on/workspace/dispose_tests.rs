//! No real cleanup is run: every Git and filesystem effect uses this fake.
use super::*;
use crate::cron::always_on::{AlwaysOnHost, HostFuture, TurnRequest};
use std::{collections::VecDeque, sync::Mutex};

struct Fake {
    events: Mutex<Vec<String>>,
    git_results: Mutex<VecDeque<Result<bool, AlwaysOnError>>>,
    rm_failure: bool,
}
impl Fake {
    fn new(results: Vec<Result<bool, AlwaysOnError>>, rm_failure: bool) -> Self {
        Self {
            events: Mutex::default(),
            git_results: Mutex::new(results.into()),
            rm_failure,
        }
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}
impl GitWorktreeCleanup for Fake {
    fn git<'a>(&'a self, root: &'a Path, args: &'a [&'a str]) -> HostFuture<'a, bool> {
        Box::pin(async move {
            self.events.lock().unwrap().push(format!(
                "git -C {} {}",
                root.display(),
                args.join(" ")
            ));
            self.git_results.lock().unwrap().pop_front().unwrap()
        })
    }
    fn remove_tree<'a>(&'a self, path: &'a Path) -> HostFuture<'a, ()> {
        Box::pin(async move {
            self.events
                .lock()
                .unwrap()
                .push(format!("rm {}", path.display()));
            if self.rm_failure {
                Err(AlwaysOnError::new("storage_failed", "fixture rm failure"))
            } else {
                Ok(())
            }
        })
    }
}
impl AlwaysOnHost for Fake {
    fn turn(&self, _: TurnRequest) -> HostFuture<'_, Vec<Value>> {
        panic!("dispose must not start a turn")
    }
    fn abort<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> HostFuture<'a, ()> {
        panic!("dispose must not abort a turn")
    }
    fn close<'a>(&'a self, _: &'a str) -> HostFuture<'a, ()> {
        panic!("dispose must not close sessions")
    }
    fn on_turn_event(&self, _: &str, _: &str, _: &Value) {
        panic!("dispose has no turn event")
    }
    fn on_worktree_removed(&self, cwd: &Path) {
        self.events
            .lock()
            .unwrap()
            .push(format!("removed {}", cwd.display()));
    }
}

fn workspace() -> Value {
    json!({"strategy":"git-worktree","cwd":"/fake/worktree","metadata":{"repoRoot":"/fake/repo","branchName":"always-on/run"}})
}

#[tokio::test]
async fn keep_returns_before_callbacks_validation_and_cleanup() {
    let fake = Fake::new(vec![], false);
    dispose_git_worktree_with(&Value::Null, true, Some(&fake), &fake)
        .await
        .unwrap();
    assert!(fake.events().is_empty());
}

#[tokio::test]
async fn removal_notification_precedes_git_and_success_skips_fallback() {
    let fake = Fake::new(vec![Ok(true), Ok(false)], false);
    dispose_git_worktree_with(&workspace(), false, Some(&fake), &fake)
        .await
        .unwrap();
    assert_eq!(
        fake.events(),
        [
            "removed /fake/worktree",
            "git -C /fake/repo worktree remove --force /fake/worktree",
            "git -C /fake/repo branch -D always-on/run",
        ]
    );
}

#[tokio::test]
async fn failed_or_unspawnable_git_removes_then_prunes_then_deletes_branch() {
    for first in [
        Ok(false),
        Err(AlwaysOnError::new("spawn", "fixture spawn failure")),
    ] {
        let fake = Fake::new(
            vec![
                first,
                Err(AlwaysOnError::new("prune", "ignored")),
                Ok(false),
            ],
            false,
        );
        dispose_git_worktree_with(&workspace(), false, Some(&fake), &fake)
            .await
            .unwrap();
        assert_eq!(
            fake.events(),
            [
                "removed /fake/worktree",
                "git -C /fake/repo worktree remove --force /fake/worktree",
                "rm /fake/worktree",
                "git -C /fake/repo worktree prune",
                "git -C /fake/repo branch -D always-on/run",
            ]
        );
    }
}

#[tokio::test]
async fn filesystem_failure_stops_before_prune_and_does_not_repeat_notification() {
    let fake = Fake::new(vec![Ok(false)], true);
    let error = dispose_git_worktree_with(&workspace(), false, Some(&fake), &fake)
        .await
        .unwrap_err();
    assert_eq!(error.message, "fixture rm failure");
    assert_eq!(
        fake.events(),
        [
            "removed /fake/worktree",
            "git -C /fake/repo worktree remove --force /fake/worktree",
            "rm /fake/worktree",
        ]
    );
}

#[tokio::test]
async fn missing_repo_uses_cwd_and_empty_branch_has_no_delete_attempt() {
    for branch in [Value::Null, json!("")] {
        let fake = Fake::new(vec![Ok(true)], false);
        let workspace =
            json!({"cwd":"/fake/worktree","metadata":{"repoRoot":null,"branchName":branch}});
        dispose_git_worktree_with(&workspace, false, Some(&fake), &fake)
            .await
            .unwrap();
        assert_eq!(
            fake.events(),
            [
                "removed /fake/worktree",
                "git -C /fake/worktree worktree remove --force /fake/worktree",
            ]
        );
    }
}
