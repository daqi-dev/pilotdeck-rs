//! A temporary directory owned by exactly one test run.
//!
//! The name carries a fresh UUID (never a pid, which macOS reuses after a
//! killed run), creation refuses an existing path, and an owner token is
//! written inside. Drop deletes the tree only while that token is still the
//! one this value wrote, so a path that was replaced underneath the test is
//! left alone rather than removed.
use std::path::{Path, PathBuf};

const OWNER_FILE: &str = ".pdrs-scratch-owner";

pub(crate) struct Scratch {
    path: PathBuf,
    token: String,
}

#[allow(dead_code)]
impl Scratch {
    pub(crate) fn new(prefix: &str) -> Self {
        let token = uuid::Uuid::new_v4().to_string();
        let path = std::env::temp_dir().join(format!("pdrs-{prefix}-{token}"));
        std::fs::create_dir(&path).expect("scratch directory must be new");
        std::fs::write(path.join(OWNER_FILE), &token).expect("write scratch owner token");
        Self { path, token }
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    /// Whether `path` is this scratch's owner token file (to hide it from
    /// directory listings that a replay compares).
    pub(crate) fn is_owner_file(name: &str) -> bool {
        name == OWNER_FILE
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let owned = std::fs::read_to_string(self.path.join(OWNER_FILE))
            .is_ok_and(|token| token == self.token);
        if owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
