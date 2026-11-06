//! #1468/#1494: the resolved path of an already-open file handle.
//!
//! The path spelling a caller used to open a file says nothing about the
//! object the handle references once a swapped symlink has been followed:
//! `File::open` re-walks the path, so a leaf or ancestor replaced between a
//! canonicalize check and the open changes which file is read while every
//! path-based check still passes. This helper asks the platform for the
//! handle's own location instead — the object the descriptor references,
//! not the spelling used to reach it:
//!
//! - macOS: `fcntl(F_GETPATH)` through the already-locked `rustix::getpath`
//!   wrapper (the workspace forbids `unsafe`, so the raw `libc::fcntl` call
//!   the issue prescribes is made through this safe wrapper of the same
//!   syscall). The result is the vnode's resolved physical spelling
//!   (`/tmp` opens report `/private/tmp`, matching `fs::canonicalize`), and
//!   a hard-linked vnode may answer with **either** of its names — callers
//!   that must accept a hard link (the attachment lease files are hard
//!   links of the upload-store files) have to accept every legitimate home
//!   of the object, not just the spelling they opened (probe:
//!   `_tmp/20261106-issue-1468-1494/probe/`).
//! - Linux: `readlink /proc/self/fd/<N>` gives the spelling the open used;
//!   an unlinked object keeps its last path with a ` (deleted)` suffix,
//!   which callers' containment checks treat like any other spelling.
//!   Non-filesystem descriptors answer with a pseudo path such as
//!   `pipe:[…]` — not an absolute path — and are reported as unsupported
//!   rather than handed to a prefix check.
//! - Other platforms: explicitly unsupported.
//!
//! Every caller must treat an error as "not approved" (fail closed): #1468
//! turns it into the not-found diagnostic branch, #1494 into
//! `PROJECT_PATH_FORBIDDEN`. Silently approving an unresolvable handle is
//! the one forbidden outcome.

use std::fs::File;
use std::io;
use std::path::PathBuf;

/// The resolved filesystem path of an already-open handle, per the module
/// docs. Never a path that was merely checked before the open.
pub(crate) fn opened_path(file: &File) -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        linux::opened_path(file)
    }
    #[cfg(target_os = "macos")]
    {
        macos::opened_path(file)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        other::opened_path(file)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io;
    use std::os::unix::io::AsRawFd;
    use std::path::PathBuf;

    pub(super) fn opened_path(file: &File) -> io::Result<PathBuf> {
        let resolved = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        // procfs answers non-filesystem descriptors with pseudo spellings
        // (`pipe:[…]`, `socket:[…]`, `anon_inode:[…]`) that are not absolute
        // paths; a containment check would silently pass them to `starts_with`
        // on the next `/`, so they fail closed here instead.
        if resolved.is_absolute() {
            Ok(resolved)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the handle has no filesystem path",
            ))
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::File;
    use std::io;
    use std::path::PathBuf;

    pub(super) fn opened_path(file: &File) -> io::Result<PathBuf> {
        let resolved = rustix::fs::getpath(file)?;
        let bytes = resolved.to_bytes().to_vec();
        String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the handle path is not UTF-8"))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod other {
    use std::fs::File;
    use std::io;
    use std::path::PathBuf;

    pub(super) fn opened_path(_file: &File) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "opened_path has no implementation on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::opened_path;
    use crate::{negative_assert, negative_assert_eq};
    use std::fs;
    use std::path::PathBuf;

    /// A scratch file outside the worktree, like the services tests.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let root = fs::canonicalize(std::env::temp_dir())
                .expect("#1468 canonical temp dir for the handle-path scratch")
                .join(format!("pdrs-handle-path-{label}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).expect("#1468 handle-path scratch dir");
            Self(root)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn handle_path_names_the_opened_object() {
        let scratch = Scratch::new("object");
        let file_path = scratch.path("target.bin");
        fs::write(&file_path, b"handle-path-bytes").expect("#1468 scratch file");
        let file = fs::File::open(&file_path).expect("#1468 open the scratch file");
        let resolved = opened_path(&file).expect("#1468 resolve the opened handle's path");
        negative_assert_eq!(
            fs::canonicalize(&resolved).expect("#1468 canonicalize the handle path"),
            fs::canonicalize(&file_path).expect("#1468 canonicalize the scratch path"),
            "#1468 the handle path must name the opened object"
        );
    }

    #[cfg(unix)]
    #[test]
    fn handle_path_resolves_a_symlink_to_its_target() {
        let scratch = Scratch::new("symlink");
        let target = scratch.path("real-target.bin");
        let link = scratch.path("leaf-link.bin");
        fs::write(&target, b"target-bytes").expect("#1468 symlink target fixture");
        std::os::unix::fs::symlink(&target, &link).expect("#1468 symlink fixture");
        let file = fs::File::open(&link).expect("#1468 open through the symlink");
        let resolved = opened_path(&file).expect("#1468 resolve the handle opened via a symlink");
        negative_assert_eq!(
            fs::canonicalize(&resolved).expect("#1468 canonicalize the handle path"),
            fs::canonicalize(&target).expect("#1468 canonicalize the target"),
            "#1468 a handle opened through a symlink must report the target, not the link"
        );
        // Lexical spelling, deliberately not canonicalize(&link) — that
        // would follow the link to the target and compare target vs target.
        negative_assert!(
            resolved != link,
            "#1468 the handle path must never be the symlink's own spelling"
        );
    }

    #[cfg(unix)]
    #[test]
    fn handle_path_does_not_panic_on_an_unlinked_file() {
        let scratch = Scratch::new("unlinked");
        let file_path = scratch.path("gone.bin");
        fs::write(&file_path, b"gone-bytes").expect("#1468 unlinked fixture");
        let file = fs::File::open(&file_path).expect("#1468 open before the unlink");
        fs::remove_file(&file_path).expect("#1468 unlink while the handle is open");
        // A deleted object keeps its last known spelling (Linux appends
        // " (deleted)"): the call must succeed without panicking so a caller's
        // containment check can decide, and an outside spelling — deleted or
        // not — still fails it.
        let resolved = opened_path(&file).expect("#1468 an unlinked handle still resolves");
        let spelling = resolved.to_string_lossy().into_owned();
        #[cfg(target_os = "linux")]
        negative_assert!(
            spelling.ends_with(" (deleted)"),
            "#1468 Linux must mark an unlinked handle's path deleted, got: {spelling}"
        );
        #[cfg(target_os = "macos")]
        negative_assert!(
            resolved == file_path,
            "#1468 macOS keeps the unlinked handle's last path, got: {spelling}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn handle_path_fails_closed_for_non_filesystem_handles() {
        // A pipe has no filesystem path: macOS `F_GETPATH` fails outright
        // (EBADF, probe `_tmp/20261106-issue-1468-1494/probe/`), Linux
        // answers `pipe:[…]`, which the linux module rejects as not an
        // absolute path. Either way the helper must report an error — the
        // error branch the #1468 fail-closed decision consumes.
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("#1468 spawn the pipe fixture");
        let stdin = child.stdin.take().expect("#1468 pipe write end");
        let file = fs::File::from(std::os::fd::OwnedFd::from(stdin));
        let resolved = opened_path(&file);
        negative_assert!(
            resolved.is_err(),
            "#1468 a non-filesystem handle must fail closed with an error"
        );
        drop(file);
        let _ = child.wait();
    }
}
