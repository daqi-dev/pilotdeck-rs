//! End-to-end leaf-copy/rename failures, and explicit native-only race boundaries.
use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0.join("blocked"), fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn check(section: &str, name: &str, operation: impl FnOnce(&Path) -> FileMemoryError) {
    let raw = include_str!("copy-io-oracle.json");
    crate::test_baseline::oracle(raw);
    let oracle: serde_json::Value = serde_json::from_str(raw).unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-pr537-round7-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let scratch = Scratch(fs::canonicalize(root).unwrap());
    let error = operation(&scratch.0);
    let actual = serde_json::json!({
        "error": error.io_code(),
        "message": error.to_string().replace(scratch.0.to_str().unwrap(), "$ROOT"),
    });
    assert_eq!(actual, oracle[section][name], "round7 {section}/{name}");
}
fn readonly_destination(root: &Path) -> PathBuf {
    let blocked = root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
    // Independent host probe: never let the implementation decide that chmod
    // cannot be tested. The probe has its own pathname, outside the golden.
    assert_eq!(
        fs::write(blocked.join("probe"), "probe")
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
    );
    blocked.join("destination")
}
fn fallback() -> SnapshotIoHook {
    Arc::new(|op, _, _| {
        if op == SnapshotIoOperation::Rename {
            Err(io::Error::from_raw_os_error(libc::EACCES))
        } else {
            Ok(())
        }
    })
}
#[test]
fn round7_lstat_matches_real_ts_fallback() {
    check("cases", "lstat", |root| {
        rename(
            &fallback(),
            &root.join("source"),
            &root.join("blocked/destination"),
        )
        .unwrap_err()
    });
}
#[test]
fn round7_copyfile_matches_real_ts_readonly_destination() {
    check("cases", "copyfile", |root| {
        let src = root.join("source");
        fs::write(&src, "payload").unwrap();
        let dst = readonly_destination(root);
        let hook: SnapshotIoHook = Arc::new(|_, _, _| Ok(()));
        rename(&hook, &src, &dst).unwrap_err()
    });
}
#[test]
fn round7_symlink_matches_real_ts_readonly_destination() {
    check("cases", "symlink", |root| {
        let target = root.join("target");
        fs::write(&target, "target").unwrap();
        let src = root.join("source");
        symlink(&target, &src).unwrap();
        let dst = readonly_destination(root);
        let hook: SnapshotIoHook = Arc::new(|_, _, _| Ok(()));
        rename(&hook, &src, &dst).unwrap_err()
    });
}
#[test]
fn round7_rename_matches_real_ts_non_permission_error() {
    check("cases", "rename", |root| {
        fs::create_dir(root.join("blocked")).unwrap();
        let hook: SnapshotIoHook = Arc::new(|_, _, _| Ok(()));
        rename(
            &hook,
            &root.join("source"),
            &root.join("blocked/destination"),
        )
        .unwrap_err()
    });
}
// Invoke the same wrappers used by copy_tree with naturally missing paths.
// These check error propagation, not the nondeterministic inter-syscall race.
#[test]
fn round7_readlink_matches_native_boundary() {
    check("nativeBoundaries", "readlink", |root| {
        copy_read_link(&root.join("missing")).unwrap_err().into()
    });
}
#[test]
fn round7_realpath_matches_native_boundary() {
    check("nativeBoundaries", "realpath", |root| {
        copy_realpath(&root.join("missing")).unwrap_err().into()
    });
}
#[test]
fn round7_chmod_matches_native_boundary() {
    check("nativeBoundaries", "chmod", |root| {
        copy_permissions(&root.join("missing"), fs::Permissions::from_mode(0o700))
            .unwrap_err()
            .into()
    });
}
