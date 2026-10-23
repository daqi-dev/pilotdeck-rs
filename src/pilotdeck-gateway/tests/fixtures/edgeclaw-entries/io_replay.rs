use crate::runtime::memory::edgeclaw::{EdgeClawFileStores, FileMemoryError};
use crate::test_baseline as baseline;
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        fn restore(path: &Path) {
            if path.is_symlink() {
                return;
            }
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    restore(&entry.path());
                }
            }
        }
        restore(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn deny(path: &Path, directory: bool) {
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o100 } else { 0 }),
    )
    .unwrap();
    // Fail explicitly on hosts where chmod cannot exercise this boundary.
    let error = if directory {
        fs::read_dir(path).err()
    } else {
        fs::read(path).err()
    };
    assert_eq!(
        error.expect("native permission probe must fail").kind(),
        std::io::ErrorKind::PermissionDenied
    );
}
pub fn check(name: &str, operation: impl FnOnce(&EdgeClawFileStores, &Path) -> FileMemoryError) {
    let oracle = include_str!("io-oracle.json");
    // Same source identity check as the integration corpora, including the full closure.
    baseline::oracle(oracle);
    let expected: serde_json::Value = serde_json::from_str(oracle).unwrap();
    let root = std::env::temp_dir().join(format!("pdrs-entries-io-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let scratch = Scratch(fs::canonicalize(root).unwrap());
    let root = &scratch.0;
    let repo = EdgeClawFileStores::with_workspace_clock(
        root.join("control.sqlite"),
        root.join("current/memory"),
        root.join("global"),
        root.join("workspace"),
        root.join("workspace"),
        Arc::new(|| "2026-10-23T00:00:00.000Z".into()),
    )
    .unwrap();
    let error = operation(&repo, root);
    let actual = serde_json::json!({"error": error.io_code(), "message": error.to_string().replace(root.to_str().unwrap(), "$ROOT")});
    assert_eq!(actual, expected["cases"][name], "real TS IO case {name}");
}
