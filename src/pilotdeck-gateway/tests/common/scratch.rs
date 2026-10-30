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
    process,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) const OWNER_FILE: &str = ".pdrs-scratch-owner";

/// How long the helper below may take to drain the body and exit (#1125
/// fix-r2 review P2): the write end of its stdin pipe can be inherited by a
/// sibling fork that stalls before its own exec, so EOF may never arrive.
const HELPER_DEADLINE: Duration = Duration::from_secs(30);

/// Reap the helper at this granularity once its deadline is running.
const HELPER_POLL: Duration = Duration::from_millis(5);

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
/// the bounded wait below reaps the helper before `chmod` + `rename`
/// publish the file, so when this returns no process anywhere still holds
/// it open for writing (cb8 probe: 0 ETXTBSY in 300 pressured iterations).
///
/// `mode` is applied to the temporary name before the rename, so the target
/// name never exists in a non-executable state, and every error exit after
/// the helper created the file removes the temporary name again.
///
/// Every wait on the helper is bounded by `HELPER_DEADLINE` (#1125 fix-r2
/// review P2): the write end of its stdin pipe can be inherited by a
/// sibling fork that stalls before its own exec, so `cat` may never see
/// EOF. Past the deadline the helper is killed and reaped, the temporary
/// name is removed, and the failure is returned as `ErrorKind::TimedOut`.
pub(crate) fn write_executable(path: &Path, body: &str, mode: u32) -> io::Result<()> {
    write_executable_bounded(path, body, mode, HELPER_DEADLINE, |temporary| {
        let mut helper = process::Command::new("/bin/sh");
        helper
            .arg("-c")
            .arg("cat >\"$1\"")
            .arg("write-executable")
            .arg(temporary)
            .stdin(process::Stdio::piped());
        helper
    })
}

/// The deadline-parameterized core of [`write_executable`]: `build_helper`
/// receives the temporary name it must pipe the body into, so a test can
/// substitute a helper that stalls deterministically (#1125 fix-r2 review
/// P2 asked for exactly that coverage).
pub(crate) fn write_executable_bounded(
    path: &Path,
    body: &str,
    mode: u32,
    deadline: Duration,
    build_helper: impl FnOnce(&Path) -> process::Command,
) -> io::Result<()> {
    let name = path
        .file_name()
        .expect("#1125 executable file name")
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4()));
    let mut helper = build_helper(&temporary).spawn()?;
    // The body is written from a short-lived thread that owns the pipe's
    // write end: the parent's copy is closed — and reported — before the
    // bounded wait below starts, and a helper that never reads cannot
    // wedge this thread in an unbounded in-process write.
    let mut stdin = helper.stdin.take().expect("#1125 helper stdin is piped");
    let (sent, done) = mpsc::channel();
    let payload = body.as_bytes().to_vec();
    thread::spawn(move || {
        // Close the pipe so `cat` sees EOF and exits; a broken pipe is
        // reported by `written`, never by hanging on the helper.
        let written = io::Write::write_all(&mut stdin, &payload);
        drop(stdin);
        let _ = sent.send(written);
    });
    let expiry = Instant::now() + deadline;
    let mut exceeded = false;
    let written = match done.recv_timeout(expiry.saturating_duration_since(Instant::now())) {
        Ok(written) => written,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            exceeded = true;
            Err(helper_deadline_error(deadline))
        }
        // The writer thread can only end without reporting by panicking,
        // which still drops the pipe's write end; success is never assumed.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "#1125 helper writer ended without reporting",
        )),
    };
    let waited = if exceeded {
        Err(helper_deadline_error(deadline))
    } else {
        loop {
            match helper.try_wait() {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(status)) => {
                    break Err(io::Error::other(format!(
                        "#1125 helper sh for {} exited with {status}",
                        temporary.display()
                    )));
                }
                Ok(None) if Instant::now() < expiry => thread::sleep(HELPER_POLL),
                Ok(None) => {
                    exceeded = true;
                    break Err(helper_deadline_error(deadline));
                }
                Err(error) => break Err(error),
            }
        }
    };
    if exceeded {
        // The helper cannot be waited out: terminate and reap it so no
        // process keeps running in this test's name, then fall through to
        // the temporary-name cleanup below.
        let _ = helper.kill();
        let _ = helper.wait();
    }
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

fn helper_deadline_error(deadline: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("#1125 helper did not finish within its {deadline:?} deadline"),
    )
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
