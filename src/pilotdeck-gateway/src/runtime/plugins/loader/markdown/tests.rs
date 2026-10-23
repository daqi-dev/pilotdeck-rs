//! #564 / ROOT-001: checked Markdown read with deterministic races.
use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};

#[test]
fn leaf_replaced_with_symlink_after_check_is_skipped() {
    run_race("symlink", |_scratch, _safe, leaf, outside| {
        fs::remove_file(leaf).unwrap();
        std::os::unix::fs::symlink(outside, leaf).unwrap();
    });
}

#[test]
fn ancestor_replaced_with_symlink_after_check_is_skipped() {
    run_race("ancestor", |scratch, _safe, leaf, _outside| {
        let sub = leaf.parent().unwrap();
        let saved = scratch.join("saved-sub");
        fs::rename(sub, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, sub).unwrap();
    });
}

fn run_race<F>(label: &str, swap: F)
where
    F: FnOnce(&PathBuf, &PathBuf, &PathBuf, &PathBuf) + Send + 'static,
{
    let scratch = std::env::temp_dir().join(format!("pdrs-564-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&scratch).unwrap();
    let base = scratch.join("p/commands");
    let sub = base.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir(scratch.join("outside")).unwrap();
    let safe = base.join("a.md");
    let leaf = sub.join("b.md");
    let outside = scratch.join("outside/secret.md");
    fs::write(&safe, "safe").unwrap();
    fs::write(&leaf, "inside").unwrap();
    fs::write(&outside, "SECRET").unwrap();
    let swapped = Arc::new(AtomicBool::new(false));
    let swap_flag = Arc::clone(&swapped);
    let scratch_clone = scratch.clone();
    BEFORE_OPEN.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |_canonical| {
            swap_flag.store(true, Ordering::SeqCst);
            swap(&scratch_clone, &safe, &leaf, &outside);
        }));
    });
    let result = load_plugin_commands("p", &base).unwrap();
    fs::remove_dir_all(&scratch).unwrap();
    assert!(swapped.load(Ordering::SeqCst), "{label}: hook did not fire");
    assert_eq!(result.len(), 1, "{label}: expected only safe sibling");
    assert_eq!(result[0].name, "p:a");
    assert_eq!(result[0].content, "safe");
    assert!(
        !result.iter().any(|c| c.content == "SECRET"),
        "{label}: outside content leaked"
    );
}

#[test]
fn fifo_markdown_is_skipped_without_blocking() {
    let scratch = std::env::temp_dir().join(format!("pdrs-564-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&scratch).unwrap();
    let base = scratch.join("p/commands");
    fs::create_dir_all(&base).unwrap();
    let fifo = base.join("pipe.md");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let result = load_plugin_commands("p", &base).unwrap();
    fs::remove_dir_all(&scratch).unwrap();
    assert!(
        result.is_empty(),
        "FIFO must be skipped, not block the traversal"
    );
}
