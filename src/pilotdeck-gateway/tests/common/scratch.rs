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

/// Publish an executable file the way #1125 requires: no descriptor this
/// process ever holds open for writing may be attached to a path a test (or
/// the product under test) will exec.
///
/// Any write in this process leaves a window where this thread holds the
/// file open for writing. Under libtest's thread pool a sibling fork inside
/// that window hands the write descriptor to its child (O_CLOEXEC only
/// closes at that child's own exec) — and it keeps it even across a rename,
/// because rename keeps the inode: the fix-r1 temp-name + rename scheme
/// narrowed the window but never closed it (cb8 probe: 28 ETXTBSY in 300
/// pressured iterations). Instead the body is piped to a
/// `/bin/sh -c 'cat >"$tmp"'` helper child: the write descriptor exists
/// only inside that helper, no fork of this process can inherit it, and
/// the `wait` below reaps the helper before `chmod` + `rename` publish
/// the file, so when this returns no process anywhere still holds it open
/// for writing (cb8 probe: 0 ETXTBSY in 300 pressured iterations).
///
/// `mode` is applied to the temporary name before the rename, so the target
/// name never exists in a non-executable state, and every error exit after
/// the helper created the file removes the temporary name again.
pub(crate) fn write_executable(path: &Path, body: &str, mode: u32) -> io::Result<()> {
    let name = path
        .file_name()
        .expect("#1125 executable file name")
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4()));
    let mut helper = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("cat >\"$1\"")
        .arg("write-executable")
        .arg(&temporary)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = helper.stdin.take().expect("#1125 helper stdin is piped");
    let written = io::Write::write_all(&mut stdin, body.as_bytes());
    // Close the pipe so `cat` sees EOF and exits; a broken pipe is reported
    // by `written`, never by hanging on the helper.
    drop(stdin);
    let waited = helper.wait().and_then(|status| {
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "#1125 helper sh exited with {status}"
            )))
        }
    });
    let published = written.and(waited).and_then(|()| {
        #[cfg(unix)]
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        #[cfg(not(unix))]
        let _ = mode;
        fs::rename(&temporary, path)
    });
    if published.is_err() {
        // Every error exit after the helper may have created the file
        // removes the temporary name again (#1125 fix-r1 review P2).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #1125 fix-r1 review P2: a publication that fails after the helper
    /// created the temporary name must remove it again, so a failed
    /// `write_executable` leaves no `.tmp-` residue behind.
    #[test]
    fn failed_publication_removes_the_temporary_name() {
        let scratch = Scratch::new("write-executable");
        // Renaming a file onto an existing directory fails, so the helper's
        // write succeeds while the publication does not.
        let target = scratch.path().join("occupied");
        fs::create_dir(&target).expect("#1125 create occupied dir");
        let error = write_executable(&target, "#!/bin/sh\n", 0o755)
            .expect_err("#1125 publication must fail onto a directory");
        assert_ne!(
            error.kind(),
            io::ErrorKind::NotFound,
            "#1125 failure must come from the rename, not a missing path: {error}"
        );
        let mut names: Vec<_> = fs::read_dir(scratch.path())
            .expect("#1125 read scratch directory")
            .map(|entry| entry.expect("#1125 scratch entry").file_name())
            .collect();
        names.sort();
        // Only the owner token and the occupied directory may remain.
        let expected: Vec<std::ffi::OsString> = vec![OWNER_FILE.into(), "occupied".into()];
        assert_eq!(
            names, expected,
            "#1125 temp residue after failed publication"
        );
    }
}
