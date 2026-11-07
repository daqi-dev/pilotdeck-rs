//! #1468/#1494: the resolved path of an already-open file handle.
//!
//! The path spelling a caller used to open a file says nothing about the
//! object the handle references once a swapped symlink has been followed:
//! `File::open` re-walks the path, so a leaf or ancestor replaced between a
//! canonicalize check and the open changes which file is read while every
//! path-based check still passes. This helper asks the platform for the
//! handle's own location instead — the object the descriptor references,
//! not the spelling the caller used to reach it. It is a name, not an
//! identity: a rename after the open changes the answer, so callers pair it
//! with an open-time `(dev, inode)` comparison (see `services.rs` step 0/3)
//! rather than treating the containment check as sufficient on its own.
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

    /// The kernel's raw name bytes → a `PathBuf`, rejecting a name that is
    /// not UTF-8: a lossy spelling names an object that was never opened, so
    /// it must not reach a caller's containment check (U+FFFD replacements
    /// are not the vnode's name).
    ///
    /// #1990 fix-r1: split out of `opened_path` so the rejection is reachable
    /// from a unit test with synthetic bytes. No macOS filesystem can host a
    /// non-UTF-8 file name — APFS/HFS+ answer `EILSEQ` (os error 92) for
    /// `open(O_CREAT)`, `mkdir` and `create_dir_all` alike, measured on
    /// darwin 25.6 and 26.6 across two hosts (`runtime::cron::tests::
    /// non_utf8_names_writable`, `tests/workspace_snapshot.rs`) — so the
    /// end-to-end fixture this branch used to build cannot exist on the only
    /// platform the module compiles on, and it panicked before its assertion
    /// ran (mbp2 macOS 26.6, 2026-11-07).
    pub(super) fn utf8_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
        String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the handle path is not UTF-8"))
    }

    pub(super) fn opened_path(file: &File) -> io::Result<PathBuf> {
        let resolved = rustix::fs::getpath(file)?;
        utf8_path(resolved.to_bytes().to_vec())
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

    /// #1672 fix-r2 (r2 reviews — three roles, same site): the fd triage for
    /// this file's two probe calls (`fs::write` and `fs::File::open` — the
    /// open eats the same process fd table as the write), as a tested unit —
    /// the same contract as the three `triage_probe` twins. `cfg(unix)`, not
    /// `cfg(macos)`, so the synthetic-errno unit test below compiles and runs
    /// on every unix host while its caller stays macOS-only.
    #[cfg(unix)]
    fn probe_hit_the_fd_limit(error: &std::io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(libc::EMFILE) | Some(libc::ENFILE)
        )
    }

    /// #1672 fix-r2: exactly one bounded retry for an fd-limit failure — the
    /// one errno family whose cause (the process's or the system's fd table)
    /// lives outside the probed file. Any other error is attempted exactly
    /// once; the retried attempt's outcome wins.
    #[cfg(unix)]
    fn attempt_with_one_fd_retry<T>(
        mut attempt: impl FnMut() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let mut outcome = attempt();
        if let Err(error) = &outcome
            && probe_hit_the_fd_limit(error)
        {
            outcome = attempt();
        }
        outcome
    }

    /// #1672 fix-r2: the executable evidence for the two helpers above —
    /// synthetic errnos, no filesystem, runs on every unix host (the caller
    /// is macOS-only, where no CI and no unix sibling can execute it).
    #[cfg(unix)]
    #[test]
    fn fd_limit_attempts_retry_exactly_once_and_terminal_outcome_wins() {
        let error = |errno: i32| std::io::Error::from_raw_os_error(errno);
        // A non-fd error is attempted exactly once: the retry condition must
        // not be loosened into "any error retries".
        let mut calls = 0;
        let outcome: std::io::Result<()> = attempt_with_one_fd_retry(|| {
            calls += 1;
            Err(error(libc::EILSEQ))
        });
        assert!(
            outcome.is_err() && calls == 1,
            "#1990 a non-fd error is not retried: the fd retry must stay on EMFILE/ENFILE"
        );
        // Both fd-limit errnos get exactly one retry, and the retried
        // attempt's outcome wins — a later-freed fd lets the second attempt
        // through.
        for errno in [libc::EMFILE, libc::ENFILE] {
            let mut calls = 0;
            let outcome: std::io::Result<()> = attempt_with_one_fd_retry(|| {
                calls += 1;
                if calls == 1 {
                    Err(error(errno))
                } else {
                    Ok(())
                }
            });
            assert!(
                outcome.is_ok() && calls == 2,
                "#1990 errno {errno} gets exactly one retry: a later-freed fd must let the second attempt through"
            );
        }
        // A persistent limit is retried once and then stops — never a loop.
        let mut calls = 0;
        let outcome: std::io::Result<()> = attempt_with_one_fd_retry(|| {
            calls += 1;
            Err(error(libc::EMFILE))
        });
        assert!(
            matches!(&outcome, Err(error) if probe_hit_the_fd_limit(error)) && calls == 2,
            "#1990 the fd retry is bounded: exactly one retry, then the error stands, never a loop"
        );
        // #1672 fix-r3 (DS r3 boundary P3): the retried attempt's own
        // outcome wins even when it fails with a different, non-fd error —
        // EMFILE then EACCES must surface EACCES, not the first EMFILE.
        let mut calls = 0;
        let outcome: std::io::Result<()> = attempt_with_one_fd_retry(|| {
            calls += 1;
            Err(error(if calls == 1 { libc::EMFILE } else { libc::EACCES }))
        });
        assert!(
            matches!(&outcome, Err(err) if err.raw_os_error() == Some(libc::EACCES)) && calls == 2,
            "#1990 the second attempt's terminal outcome wins: EMFILE then EACCES surfaces EACCES, not EMFILE"
        );
    }

    // #1990: the two tests below are macOS-only on purpose. `mod macos` is
    // compiled out everywhere else, so these are the only assertions that can
    // pin which step of `macos::opened_path` produced a rejection: the
    // `is_err()` shape above cannot tell the kernel `F_GETPATH` error apart
    // from the UTF-8 rejection, and no CI job compiles this module at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn handle_path_non_filesystem_error_comes_from_getpath() {
        // A pipe's rejection must carry the kernel's own errno: the raw
        // F_GETPATH failure is an OS error, while the helper's only
        // manufactured error (the UTF-8 InvalidData) has none behind it.
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("#1990 spawn the pipe fixture for the getpath error step");
        let stdin = child
            .stdin
            .take()
            .expect("#1990 take the pipe write end for the getpath error step");
        let file = fs::File::from(std::os::fd::OwnedFd::from(stdin));
        let resolved = opened_path(&file);
        negative_assert!(
            matches!(&resolved, Err(error) if error.raw_os_error().is_some()),
            "#1990 macOS must surface the kernel F_GETPATH error itself, not a later-stage failure"
        );
        drop(file);
        let _ = child.wait();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn handle_path_rejects_a_non_utf8_name() {
        // #1990 fix-r1: this used to build a real `bad\xff\xfe.bin` file and
        // assert the rejection end to end. That fixture cannot exist here:
        // macOS rejects the bytes at `open(O_CREAT)` with EILSEQ (os error
        // 92), so `fs::write` panicked before the assertion ever ran (mbp2,
        // macOS 26.6, 2026-11-07: `write the non-UTF-8 named scratch file:
        // Os { code: 92, kind: Uncategorized, message: "Illegal byte
        // sequence" }`). The rejection is pinned on the bytes→path step
        // itself with the same synthetic bytes; the end-to-end half below
        // probes with the real name where the filesystem can host it and
        // announces the skip, with its evidence, where it cannot. Every
        // host that compiles this test is in the second camp: the write
        // arm returns at the EILSEQ skip, so the open arm below is
        // structurally unreachable here (#1672 fix-r3, DS r3 boundary
        // P2); that arm's fd protection is executed by the helper's unit
        // test, not by this end-to-end test.
        let rejected = super::macos::utf8_path(b"bad\xff\xfe.bin".to_vec());
        negative_assert!(
            matches!(&rejected, Err(error) if error.kind() == std::io::ErrorKind::InvalidData),
            "#1990 macOS must reject a non-UTF-8 handle path as invalid data"
        );
        // Positive control on the same step: a UTF-8 name must survive it
        // byte for byte, so "reject everything" cannot satisfy the
        // assertion above.
        let accepted = super::macos::utf8_path(b"/private/tmp/ok-\xe4\xb8\xad.bin".to_vec());
        negative_assert_eq!(
            accepted.expect("#1990 macOS must accept a UTF-8 handle path"),
            PathBuf::from("/private/tmp/ok-\u{4e2d}.bin"),
            "#1990 the UTF-8 handle path must survive the bytes to path step unchanged"
        );
        use std::os::unix::ffi::OsStrExt;
        let scratch = Scratch::new("non-utf8");
        let file_path = scratch
            .0
            .join(std::ffi::OsStr::from_bytes(b"bad\xff\xfe.bin"));
        // #1672 fix-r1→r3 (DS boundary P1 + r2 sibling/boundary/concurrency
        // P2 + r3 boundary P2): the fd limit (EMFILE/ENFILE) lives in the
        // process or the system, not in this name. Both probe calls — the
        // write and the open — go through the tested
        // `attempt_with_one_fd_retry` helper above, and a persistent limit
        // fails closed with its own marker instead of blaming the file
        // name for a process-level condition. On hosts where the write
        // arm returns at the EILSEQ skip, the open call is never reached
        // end to end; the helper's unit test carries that arm's contract.
        let write_outcome =
            attempt_with_one_fd_retry(|| fs::write(&file_path, b"non-utf8-bytes"));
        match write_outcome {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => {
                // The filesystem refuses the name, so no handle for it can
                // ever be opened: nothing end-to-end is left to measure.
                // Printed through the raw stderr handle because libtest
                // captures `eprintln!` for a passing test.
                crate::test_skip_line::stderr_skip_line(
                    "handle_path_rejects_a_non_utf8_name: end-to-end half skipped, \
                     this filesystem rejects non-UTF-8 file names with EILSEQ; \
                     the bytes-to-path rejection above already ran\n",
                );
                return;
            }
            Err(error) if probe_hit_the_fd_limit(&error) => {
                panic!(
                    "#1990 the non-UTF-8 named scratch write hit the process fd \
                     limit, not the file name: {error}"
                );
            }
            Err(error) => panic!(
                "#1990 write the non-UTF-8 named scratch file failed for a reason \
                 other than the file name: {error}"
            ),
        }
        let file = match attempt_with_one_fd_retry(|| fs::File::open(&file_path)) {
            Ok(file) => file,
            Err(error) if probe_hit_the_fd_limit(&error) => panic!(
                "#1990 opening the non-UTF-8 named scratch file hit the process \
                 fd limit, not the file name: {error}"
            ),
            Err(error) => panic!(
                "#1990 open the non-UTF-8 named scratch file failed for a reason \
                 other than the file name: {error}"
            ),
        };
        let resolved = opened_path(&file);
        negative_assert!(
            matches!(&resolved, Err(error) if error.kind() == std::io::ErrorKind::InvalidData),
            "#1990 macOS must reject a non-UTF-8 handle path as invalid data"
        );
    }
}
