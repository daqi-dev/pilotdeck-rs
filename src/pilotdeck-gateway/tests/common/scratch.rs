//! Test scratch helpers.
//!
//! `Scratch` is a temporary directory owned by exactly one test run. The name
//! carries a fresh UUID (never a pid, which macOS reuses after a killed run),
//! creation refuses an existing path, and an owner token is written inside.
//! Drop deletes the tree only while that token is still the one this value
//! wrote, so a path that was replaced underneath the test is left alone rather
//! than removed.
//!
//! `remove_owned*` is the shared delete-ownership guard for fixture/test
//! scratch trees: a per-run temporary directory may remove itself, but no
//! lexical path outside that tree. The `remove` hook stays injectable so a
//! negative control can prove a refused delete never reaches the filesystem.
#![allow(dead_code)]

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const OWNER_FILE: &str = ".pdrs-scratch-owner";

pub(crate) struct Scratch {
    path: PathBuf,
    token: String,
}

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

/// Publish an executable file the way #1125 requires: never by writing the
/// path a test will exec.
///
/// A direct `fs::write` on the final path leaves this thread holding the file
/// open for writing between `open` and `close`. Under libtest's thread pool a
/// sibling test that forks inside that window hands the write descriptor to
/// its child (O_CLOEXEC only closes at the child's own exec), and exec'ing
/// the script before that child execs fails with ETXTBSY on Linux. Writing a
/// unique temporary name in the same directory, fsyncing it, chmod'ing it,
/// closing it, and only then renaming it into place publishes an inode no
/// process has ever held open for writing, so `Command::new(script)` can
/// never observe ETXTBSY. `mode` is applied before the rename, so the target
/// name never exists in a non-executable state.
pub(crate) fn write_executable(path: &Path, body: &str, mode: u32) -> io::Result<()> {
    let name = path
        .file_name()
        .expect("executable file name")
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4()));
    let mut file = fs::File::create(&temporary)?;
    io::Write::write_all(&mut file, body.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    drop(file);
    let published = fs::rename(&temporary, path);
    if published.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    published
}

/// Lexical ownership check: every delete may only target the per-run tree that
/// owns it. This runs before the filesystem is touched, independent of where a
/// sentinel lives.
pub(crate) fn outside_run_error(root: &Path, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "remove refused: {} is outside the run directory {}",
            path.display(),
            root.display()
        ),
    )
}

pub(crate) fn owned_path<'a>(root: &Path, path: &'a Path) -> io::Result<&'a Path> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| outside_run_error(root, path))?;
    let mut depth = 0usize;
    for component in relative.components() {
        match component {
            Component::ParentDir => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| outside_run_error(root, path))?;
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(outside_run_error(root, path));
            }
        }
    }
    Ok(path)
}

pub(crate) fn remove_owned_with<F>(
    root: &Path,
    path: &Path,
    recursive: bool,
    remove: F,
) -> io::Result<()>
where
    F: FnOnce(&Path, bool) -> io::Result<()>,
{
    owned_path(root, path)?;
    remove(path, recursive)
}

pub(crate) fn remove_owned(root: &Path, path: &Path, recursive: bool) -> io::Result<()> {
    remove_owned_with(root, path, recursive, |path, recursive| {
        if recursive {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    })
}
