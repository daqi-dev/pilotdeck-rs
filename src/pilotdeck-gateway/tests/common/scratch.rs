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

/// How far past the shared expiry the writer may lag before it is declared
/// stuck (#1125 fix-r3 review P2-2); a correct writer gives up at the
/// expiry, within one [`HELPER_POLL`] sleep.
const WRITER_GRACE: Duration = Duration::from_millis(500);

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
/// `/bin/sh -c 'exec cat >"$tmp"'` helper child: the write descriptor exists
/// only inside that helper, no fork of this process can inherit it, and the
/// bounded wait below reaps the helper before `chmod` + `rename`
/// publish the file, so when this returns no process anywhere still holds
/// it open for writing (cb8 probe: 0 ETXTBSY in 300 pressured iterations).
///
/// `mode` is applied to the temporary name before the rename, so the target
/// name never exists in a non-executable state.
///
/// Every wait is bounded by [`HELPER_DEADLINE`] (#1125 fix-r2 review P2),
/// and the bound holds even when the pipe cannot drain — a helper that
/// never reads, or a stranger still holding the read end after the helper
/// is killed — because the writer writes through a non-blocking write end
/// against the same expiry (#1125 fix-r3 review P2-2): it gives up, reports
/// `ErrorKind::TimedOut`, and drops its copy of the write end before this
/// returns. A helper that overruns is killed and reaped by the
/// [`ReapGuard`] on every exit, and every error exit removes the temporary
/// name again (#1125 fix-r1 review P2).
pub(crate) fn write_executable(path: &Path, body: &str, mode: u32) -> io::Result<()> {
    write_executable_bounded(
        path,
        body,
        mode,
        HELPER_DEADLINE,
        |temporary| {
            let mut helper = process::Command::new("/bin/sh");
            helper
                .arg("-c")
                .arg("exec cat >\"$1\"")
                .arg("write-executable")
                .arg(temporary)
                .stdin(process::Stdio::piped());
            helper
        },
        // The production helper is spawned without a group of its own and
        // nothing must clean up its descendants, so no test consumes its id.
        |_| {},
        // The production helper is killed exactly when it overruns; only
        // the publication tests consume a kill report.
        |_| {},
        // The production helper needs no return-boundary proof; only the
        // publication tests observe the pipe.
        |_| Ok(()),
    )
}

/// The deadline-parameterized core of [`write_executable`]: `build_helper`
/// receives the temporary name it must pipe the body into, so a test can
/// substitute a helper that stalls deterministically (#1125 fix-r2 review
/// P2 asked for exactly that coverage). `spawned` receives the helper's id
/// the moment the child exists — for a helper built with `process_group(0)`
/// that id is also the process-group id its descendants land in, so a test
/// that must clean those up owns the group before the helper (or anything it
/// forks) can run a single instruction, instead of relying on the child
/// publishing its group id afterwards (#1141 fix-r2 review P1-1). `reaped`
/// receives the kill + wait outcome whenever the guard had to end the helper
/// (#1125 fix-r3 review P2): it is the only liveness evidence the
/// publication tests use, so no pid is ever read back from a file and
/// signalled. `returning` receives the pipe's inode and runs once
/// `published` is decided but before the guard disarms — the return
/// boundary — so a test can prove no write end of that pipe survives the
/// return (#1155 r3 review P2-1); its error takes precedence over a
/// `TimedOut` publication, because a leaked descriptor must not hide
/// behind a deadline error the test already expects.
///
/// The write deadline starts only after `spawned` returns (#1155 r4 review
/// P2): the handover may legitimately wait — for a stranger that confirms it
/// holds the pipe's read end before the writer starts — and that wait must
/// not consume the write budget the deadline is supposed to bound.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_executable_bounded(
    path: &Path,
    body: &str,
    mode: u32,
    deadline: Duration,
    build_helper: impl FnOnce(&Path) -> process::Command,
    spawned: impl FnOnce(u32),
    reaped: impl FnOnce(io::Result<process::ExitStatus>),
    returning: impl FnOnce(u64) -> io::Result<()>,
) -> io::Result<()> {
    let name = path
        .file_name()
        .expect("#1125 executable file name")
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4()));
    let helper = build_helper(&temporary).spawn()?;
    // #1125 fix-r3 review P2-1: the guard owns the helper from the spawn on,
    // so every later exit — an error return, a deadline overrun, or a panic
    // unwinding through this function — kills and reaps it and removes the
    // temporary name. Killing an already-reaped child is a no-op in std, so
    // a pid recycled after the wait loop reaped the helper is never
    // signalled.
    let mut guard = ReapGuard {
        helper: Some(helper),
        temporary: temporary.clone(),
        reaped: Some(reaped),
    };
    // #1141 fix-r2 review P1-1: the handover happens while the guard already
    // owns the helper but before any later step — taking the stdin, starting
    // the writer, the write itself, the waits — can fail or panic, so a panic
    // raised by the hook itself still unwinds through the guard's cleanup.
    let helper_id = guard
        .helper
        .as_ref()
        .expect("#1125 reap guard holds the helper")
        .id();
    spawned(helper_id);
    // #1155 r4 review P2: the expiry starts here, after the handover, so a
    // `spawned` hook that waits for the helper's descendants to settle (the
    // stranger confirming the read end) does not eat into the write budget.
    let expiry = Instant::now() + deadline;
    let mut stdin = guard
        .helper
        .as_mut()
        .expect("#1125 reap guard holds the helper")
        .stdin
        .take()
        .expect("#1125 helper stdin is piped");
    // #1155 r3 review P2-1: the pipe's inode identifies every fd of the
    // pipe in the return-boundary scan — both ends share it — and it must
    // be taken before the writer owns the write end.
    let pipe = pipe_inode(&stdin)?;
    set_nonblocking(&mut stdin)?;
    let (sent, done) = mpsc::channel();
    let payload = body.as_bytes().to_vec();
    // #1125 fix-r3 review P2-1: a writer that cannot start is an ordinary
    // error return, not a panic unwinding past the cleanup: the dropped
    // closure closes the pipe's write end, so the helper still sees EOF,
    // and the guard reaps it and removes the temporary name.
    let writer = thread::Builder::new()
        .name("#1125 scratch writer".into())
        .spawn(move || {
            let written = write_payload_by_deadline(&mut stdin, &payload, expiry, deadline);
            let _ = sent.send(written);
            // `stdin` drops with the closure: closing the write end is what
            // lets the helper see EOF, and a broken pipe was reported by
            // `written`, never by hanging on the helper.
        });
    let written = match writer {
        Err(error) => Err(error),
        Ok(writer) => {
            let reported = done.recv_timeout(expiry.saturating_duration_since(Instant::now()));
            // #1125 fix-r3 review P2-2: the writer shares the expiry, so it
            // — and with it the last copy of the pipe's write end — must be
            // gone shortly after the expiry at the latest, whatever
            // happened to the helper or to anyone else holding the read
            // end. The grace absorbs one poll interval of lag.
            let writer_gone = expiry + WRITER_GRACE;
            while !writer.is_finished() && Instant::now() < writer_gone {
                thread::sleep(HELPER_POLL);
            }
            assert!(
                writer.is_finished(),
                "#1125 helper writer did not exit by its deadline"
            );
            match reported {
                Ok(written) => written,
                Err(mpsc::RecvTimeoutError::Timeout) => Err(helper_deadline_error(deadline)),
                // The writer can only end without reporting by panicking,
                // which still drops the pipe's write end; success is never
                // assumed.
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
                    "#1125 helper writer ended without reporting",
                )),
            }
        }
    };
    let waited = if matches!(&written, Err(error) if error.kind() == io::ErrorKind::TimedOut) {
        // The write phase itself overran: the helper cannot be waited out,
        // the guard below ends it instead.
        Err(helper_deadline_error(deadline))
    } else {
        let helper = guard
            .helper
            .as_mut()
            .expect("#1125 reap guard holds the helper");
        loop {
            match helper.try_wait() {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(status)) => {
                    break Err(io::Error::other(format!(
                        "#1125 helper for {} exited with {status}",
                        temporary.display()
                    )));
                }
                Ok(None) if Instant::now() < expiry => thread::sleep(HELPER_POLL),
                Ok(None) => break Err(helper_deadline_error(deadline)),
                // A wait error leaves the helper's state unknown; the guard
                // still kills and reaps it before this returns (#1125
                // fix-r3 review P3: every non-disarmed exit reaps).
                Err(error) => break Err(error),
            }
        }
    };
    let published = written.and(waited).and_then(|()| {
        #[cfg(unix)]
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        #[cfg(not(unix))]
        let _ = mode;
        fs::rename(&temporary, path)
    });
    // #1155 r3 review P2-1: the return boundary. `returning` proves — while
    // this function still owns the guard, so an error can still clean up —
    // that no write end of the pipe survives the return; its error takes
    // precedence over `published` so a leak surfaces even on a deadline
    // failure the test already expects.
    let boundary = returning(pipe);
    if boundary.is_ok() && published.is_ok() {
        // The wait loop reaped the helper and the rename consumed the
        // temporary name; the guard has nothing left to do.
        guard.disarm();
    }
    boundary.and(published)
}

/// Owns the helper between its spawn and the end of
/// [`write_executable_bounded`]. Drop is the single cleanup point (#1125
/// fix-r3 review P2-1): on every non-disarmed exit the helper is killed
/// and reaped — never left running, never signalled through a pid read back
/// from a file — the kill + wait outcome is handed to `reaped`, and the
/// temporary name is removed (#1125 fix-r1 review P2).
struct ReapGuard<R: FnOnce(io::Result<process::ExitStatus>)> {
    helper: Option<process::Child>,
    temporary: PathBuf,
    reaped: Option<R>,
}

impl<R: FnOnce(io::Result<process::ExitStatus>)> ReapGuard<R> {
    /// The publication succeeded and the wait loop already reaped the
    /// helper: there is nothing left to kill, report, or remove.
    fn disarm(mut self) {
        self.helper = None;
        self.reaped = None;
    }
}

impl<R: FnOnce(io::Result<process::ExitStatus>)> Drop for ReapGuard<R> {
    fn drop(&mut self) {
        if let Some(mut helper) = self.helper.take() {
            let waited = match helper.kill() {
                Ok(()) => helper.wait(),
                Err(error) => Err(error),
            };
            if let Some(report) = self.reaped.take() {
                report(waited);
            }
        }
        let _ = fs::remove_file(&self.temporary);
    }
}

/// Write `payload` into the helper's pipe, giving up at `expiry`: the write
/// end is non-blocking, so a helper (or anyone else) that never drains the
/// pipe surfaces as `WouldBlock` here instead of an unbounded write (#1125
/// fix-r3 review P2-2).
fn write_payload_by_deadline(
    stdin: &mut process::ChildStdin,
    payload: &[u8],
    expiry: Instant,
    deadline: Duration,
) -> io::Result<()> {
    let mut written = 0;
    while written < payload.len() {
        match io::Write::write(stdin, &payload[written..]) {
            // A pipe write with a nonzero request never reports a short
            // success, so `Ok(0)` cannot happen; treat it as an error.
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "#1125 helper pipe",
                ));
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // The read end is not keeping up: the deadline — not the
                // helper — ends the write.
                if Instant::now() >= expiry {
                    return Err(helper_deadline_error(deadline));
                }
                thread::sleep(HELPER_POLL);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The write end of the helper's pipe is switched to non-blocking on unix
/// so the writer above can honor the expiry; other targets keep the
/// blocking default, and the unix-only publication tests are the only
/// tests that rely on the write deadline (#1125 fix-r3 review P2-2).
#[cfg(unix)]
fn set_nonblocking(stdin: &mut process::ChildStdin) -> io::Result<()> {
    use std::os::fd::AsFd;
    rustix::fs::fcntl_setfl(stdin.as_fd(), rustix::fs::OFlags::NONBLOCK).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn set_nonblocking(_stdin: &mut process::ChildStdin) -> io::Result<()> {
    Ok(())
}

/// The helper pipe's inode, shared by both of its ends and unique to it for
/// as long as any fd of the pipe exists (#1155 r3 review P2-1): the
/// return-boundary scan identifies a write end of this pipe — as opposed to
/// any other pipe in the process tree — by that inode. Other targets keep
/// the `returning` hook but have no scan to feed.
#[cfg(unix)]
fn pipe_inode(stdin: &process::ChildStdin) -> io::Result<u64> {
    use std::os::fd::AsFd;
    Ok(rustix::fs::fstat(stdin.as_fd())?.st_ino)
}

#[cfg(not(unix))]
fn pipe_inode(_stdin: &process::ChildStdin) -> io::Result<u64> {
    Ok(0)
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
