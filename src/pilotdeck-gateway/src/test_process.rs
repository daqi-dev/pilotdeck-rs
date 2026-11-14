//! Test subprocesses are workers, never alternate entry paths of a public test.
//! A successful exit only counts with one libtest result and one case receipt.
use std::{
    collections::BTreeSet,
    io::{self, Read},
    os::fd::AsFd,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
const LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

/// Deadline for an owned rerun of a whole test case (#1632). The audit cases
/// build a real fixture before they can fail, so this is generous but finite:
/// the old `Command::output()` had no bound at all, and a descendant holding
/// the stream write ends could keep the parent test blocked forever.
pub(crate) const RERUN_LIMIT: Duration = Duration::from_secs(300);
/// Upper bound on the *reap* that follows the kill, kept separate from
/// `RERUN_LIMIT`: `Child::wait` blocks until the leader is really reaped, and a
/// leader stuck in uninterruptible sleep (D state — an NFS or disk write that
/// never returns) ignores `SIGKILL` until it wakes. An unbounded wait there
/// would hang the whole test host with no diagnostic, which is the exact
/// failure mode this module exists to remove (r13 MiniMax concurrency P2).
const RERUN_REAP_LIMIT: Duration = Duration::from_secs(30);
const RERUN_POLL: Duration = Duration::from_millis(10);
/// Per-file cap for the rerun's captured streams. A descendant that keeps
/// writing is exactly the regression the audit hunts, so the capture must not
/// be able to fill the host: the wait below stops the group once either file
/// passes the cap, and the read truncates.
///
/// This is a sampled threshold, not a hard ceiling: the size is checked every
/// `RERUN_POLL`, so a writer can overshoot it by one poll interval of its own
/// write rate. Without `RLIMIT_FSIZE` (`unsafe` is forbidden in this
/// workspace) that residual is the price of bounding it in the parent at all.
#[cfg(unix)]
const RERUN_OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;

/// Size of a captured stream, or 0 while it does not exist yet.
///
/// A missing file is "not written yet"; any other error is *not* a zero. The
/// old `unwrap_or(0)` would read a failing `metadata` as an empty stream on
/// every poll, so a runaway writer could grow the file without ever tripping
/// the cap — the cap would be silently off.
#[cfg(unix)]
fn stream_len(path: &std::path::Path) -> io::Result<u64> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Read a rerun stream. A read failure is reported, never swallowed into an
/// empty string: an audit that fails must still show its evidence.
///
/// The read is capped *while* reading (`take`), not after: `fs::read` would
/// allocate the whole file first and then truncate, so a capture that outgrew
/// the cap between two polls could still cost unbounded memory.
#[cfg(unix)]
fn read_capped(path: &std::path::Path) -> String {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return format!("<rerun output unreadable: {error}>"),
    };
    let mut bytes = Vec::new();
    if let Err(error) = file.take(RERUN_OUTPUT_LIMIT + 1).read_to_end(&mut bytes) {
        return format!("<rerun output unreadable: {error}>");
    }
    let truncated = bytes.len() as u64 > RERUN_OUTPUT_LIMIT;
    let end = bytes.len().min(RERUN_OUTPUT_LIMIT as usize);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        // Derived from the constant: a hardcoded size would keep claiming
        // 64 MiB after the cap moved.
        text.push_str(&format!(
            "\n<rerun output truncated at {} MiB>",
            RERUN_OUTPUT_LIMIT / (1024 * 1024)
        ));
    }
    text
}

/// Result of [`rerun_exact`]. `stdout`/`stderr` are returned even on timeout so
/// a stuck rerun still reports what it managed to print.
pub(crate) struct RerunOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
    /// The deadline expired and the whole process group was killed.
    pub timed_out: bool,
    /// A captured stream passed [`RERUN_OUTPUT_LIMIT`]; the group was stopped
    /// so a runaway writer cannot fill the host.
    pub overflow: bool,
}

/// Run an audit's owned rerun and fail the caller unless the rerun finished on
/// its own and matched exactly one passing test.
///
/// Both `/proc` audits spelled these two assertions out side by side; a new
/// stop reason on [`RerunOutput`] would then have to be added to both copies,
/// and the copy that was missed would read a stopped rerun as a pass (r4
/// simplify/concurrency, both families).
#[cfg(target_os = "linux")]
pub(crate) fn rerun_audit_case(test: &str, envs: &[(&str, &str)], issue: &str) {
    let rerun = rerun_exact(test, envs, RERUN_LIMIT);
    assert!(
        !rerun.timed_out && !rerun.overflow,
        "{issue} the audit rerun was stopped (timeout={} overflow={}):\n{}\n{}",
        rerun.timed_out,
        rerun.overflow,
        rerun.stdout,
        rerun.stderr
    );
    assert!(
        crate::test_exact_receipt::single_pass(&rerun.status, test, &rerun.stdout),
        "{issue} the audit rerun matched no test:\n{}\n{}",
        rerun.stdout,
        rerun.stderr
    );
}

/// Run one test case in an owned rerun of this binary, bounded in both
/// directions that used to hang the parent:
///
/// * `Command::output()` blocked until every write end of its pipes closed, so
///   a grandchild that inherited stdout/stderr (the regression the audit is
///   meant to catch) kept the parent blocked forever. Here both streams go to
///   files, which never block on a lingering writer.
/// * The rerun gets its own process group (`process_group(0)`); when the
///   deadline expires the whole group is killed, not just the leader, and the
///   leader is reaped so the captured bytes survive the failure.
///
/// This mirrors `tests/fixtures/negative_process.py` `run_owned` (start a new
/// session, bounded wait, group kill, always keep the output).
///
/// Scope: for reruns whose subject can leave descendants behind (the `/proc`
/// audits) or that can hang. A short-lived worker rerun that provably spawns
/// nothing keeps using `Command::output()` — the pipe is not a hazard there;
/// the remaining `.output()` rerun sites are tracked in #1704 rather than
/// pulled in here.
#[cfg(unix)]
pub(crate) fn rerun_exact(test: &str, envs: &[(&str, &str)], limit: Duration) -> RerunOutput {
    use std::os::unix::process::CommandExt;
    let scratch = crate::test_scratch::Scratch::new("rerun");
    let stdout_path = scratch.path().join("stdout");
    let stderr_path = scratch.path().join("stderr");
    let mut command = Command::new(std::env::current_exe().expect("rerun test binary path"));
    command
        // `--include-ignored` keeps the control worker cases selectable under
        // `--exact`; the audits themselves are not ignored.
        .args([
            "--exact",
            test,
            "--include-ignored",
            "--nocapture",
            "--test-threads=2",
        ])
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            std::fs::File::create(&stdout_path).expect("rerun stdout file"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(&stderr_path).expect("rerun stderr file"),
        ))
        // The rerun owns its group so the deadline can kill the descendants
        // the case spawned, not only the leader. That also detaches it from
        // the test host's group: a ^C aimed at the host does not reach the
        // rerun, and if the host dies first only the parent's own wait ends it
        // (the workspace forbids the `unsafe` a `PR_SET_PDEATHSIG` would
        // need). The parent stays the only reaper by design — a cancellation
        // path that outlives the parent is tracked in #1708.
        .process_group(0);
    let mut child = command.spawn().expect("spawn the owned rerun");
    let group =
        rustix::process::Pid::from_raw(child.id() as i32).expect("the rerun leader has a pid");
    // Kills the group on every way out of this function, panic included: a
    // leader reaped by `Child::wait` does not take its descendants with it,
    // and the capture files disappear with the scratch dir.
    let mut guard = crate::process_group::GroupGuard(Some(group));
    // `true` once either capture passes the cap. An unmeasurable stream counts
    // as over the cap: it cannot be bounded, and quietly polling a zero would
    // switch the cap off.
    let oversized = || {
        [&stdout_path, &stderr_path]
            .iter()
            .any(|path| match stream_len(path) {
                Ok(len) => len > RERUN_OUTPUT_LIMIT,
                Err(_) => true,
            })
    };
    let started = Instant::now();
    let mut timed_out = false;
    let mut overflow = false;
    // `Some` once the leader was reaped inside the loop: the stop path below,
    // or a platform whose only way to observe the exit also reaps.
    let mut reaped: Option<ExitStatus> = None;
    loop {
        // Observe the leader *without* reaping it: the zombie keeps the pgid
        // reserved until the final group signal. Reaping first frees the pgid,
        // and an unrelated `process_group(0)` leader could claim it between
        // the reap and `kill_process_group` — the final kill would then hit a
        // stranger's whole group (#1632 r3, both reviewers).
        #[cfg(not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )))]
        match crate::process_group::leader_exited_unreaped(group) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) => panic!("observe the owned rerun leader: {error:?}"),
        }
        // Those targets cannot wait without reaping; `try_wait` reaps, so the
        // final group signal is skipped there rather than aimed at a pgid that
        // is free again.
        #[cfg(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))]
        if let Some(status) = child.try_wait().expect("poll the owned rerun") {
            reaped = Some(status);
            break;
        }
        // The workspace forbids `unsafe`, so the child cannot be given
        // `RLIMIT_FSIZE` through `pre_exec`; the cap is enforced here instead:
        // a runaway writer is stopped as soon as either file passes it.
        let expired = started.elapsed() > limit;
        let too_big = oversized();
        if expired || too_big {
            timed_out = expired;
            overflow = too_big;
            // The leader is still alive, so the group id is still ours.
            guard.signal(rustix::process::Signal::KILL);
            let direct = child.kill();
            let reap = reap_bounded(&mut child, test);
            guard.disarm();
            eprintln!(
                "PDRS_RERUN_STOPPED {test}: timeout={timed_out} overflow={overflow} group kill sent, direct kill {direct:?}, reap {reap:?}"
            );
            reaped = Some(reap);
            break;
        }
        std::thread::sleep(RERUN_POLL);
    }
    let status = match reaped {
        Some(status) => {
            // Reaped already: nothing to reap, and no group signal — its pgid
            // may have been handed to an unrelated group.
            guard.disarm();
            status
        }
        None => {
            // The leader exited on its own and is still a zombie, so the group
            // id is still ours: kill the descendants it left behind, then reap.
            guard.signal(rustix::process::Signal::KILL);
            let status = reap_bounded(&mut child, test);
            guard.disarm();
            status
        }
    };
    // A rerun that ran away and then exited is still an overflow: the loop
    // breaks on `try_wait` before it looks at the files, so without this last
    // look a writer that filled the capture and exited would report success
    // with a silently truncated stdout.
    overflow |= oversized();
    // The deadline can fire in the same poll window in which the leader
    // finished on its own. Nothing was stopped in that case, and reporting a
    // timeout would fail a clean rerun.
    if status.success() && !overflow {
        timed_out = false;
    }
    RerunOutput {
        status,
        stdout: read_capped(&stdout_path),
        stderr: read_capped(&stderr_path),
        timed_out,
        overflow,
    }
}

/// Reap the rerun leader, but never wait on it forever: `Child::wait` has no
/// deadline of its own, and a leader that `SIGKILL` cannot reach (uninterruptible
/// sleep) would block the whole test host. On expiry the caller's `GroupGuard`
/// still owns the group, and the panic carries the pid so the leaked process is
/// identifiable in the failure evidence.
fn reap_bounded(child: &mut Child, test: &str) -> ExitStatus {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(error) => panic!("{test}: poll the owned rerun while reaping: {error:?}"),
        }
        if started.elapsed() > RERUN_REAP_LIMIT {
            panic!(
                "{test}: the owned rerun leader (pid {}) was still unreapable {} s after the kill; its process group cannot be reclaimed from here",
                child.id(),
                RERUN_REAP_LIMIT.as_secs()
            );
        }
        std::thread::sleep(RERUN_POLL);
    }
}

pub(crate) fn json_env(value: &serde_json::Value) -> Vec<(String, String)> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
        .collect()
}
pub(crate) fn receipt(actual_case: &str) {
    println!("\nPDRS_CASE_OK {actual_case}");
}
pub(crate) fn run_cases(test: &str, cases: Vec<(String, Vec<(String, String)>)>) {
    let expected: BTreeSet<_> = cases.iter().map(|(id, _)| id.clone()).collect();
    assert!(!cases.is_empty());
    assert_eq!(expected.len(), cases.len(), "duplicate case id");
    let receipts: BTreeSet<_> = cases
        .into_iter()
        .map(|(id, env)| {
            run_case(test, &id, env, DEADLINE, LIMIT)
                .unwrap_or_else(|e| panic!("{test}/{id}: {e}"));
            println!("PDRS_VERIFIED_CHILD {id}");
            id
        })
        .collect();
    assert_eq!(receipts, expected);
}
/// Owns a child process so a panic cannot leave it running or unreaped.
pub(crate) struct Worker(pub(crate) Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblocking(fd: &impl AsFd) -> Result<(), String> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(|e| e.to_string())?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK).map_err(|e| e.to_string())
}
fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>) -> Result<bool, String> {
    let mut chunk = [0; 4096];
    match reader.read(&mut chunk) {
        Ok(0) => Ok(true),
        Ok(size) => {
            bytes.extend_from_slice(&chunk[..size]);
            Ok(false)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}
fn run_case(
    test: &str,
    id: &str,
    env: Vec<(String, String)>,
    deadline: Duration,
    limit: usize,
) -> Result<(), String> {
    let mut env = env;
    crate::test_negative_receipt::forward_to_worker(&mut env);
    let mut child = Worker(
        Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .args([
                "--exact",
                test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let mut out = child.0.stdout.take().unwrap();
    let mut err = child.0.stderr.take().unwrap();
    nonblocking(&out)?;
    nonblocking(&err)?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let start = Instant::now();
    let status = loop {
        if start.elapsed() >= deadline {
            return Err("timeout".into());
        }
        let out_eof = drain(&mut out, &mut stdout)?;
        let err_eof = drain(&mut err, &mut stderr)?;
        if stdout.len() + stderr.len() > limit {
            return Err("output limit".into());
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())?
            && out_eof
            && err_eof
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = String::from_utf8(stdout).map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&stderr);
    let receipt = format!("PDRS_CASE_OK {id}");
    let summaries = stdout
        .lines()
        .filter(|line| line.starts_with("test result:"))
        .collect::<Vec<_>>();
    if !status.success()
        || summaries.len() != 1
        || !summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;")
        || stdout
            .lines()
            .filter(|line| *line == "running 1 test")
            .count()
            != 1
        || stdout
            .lines()
            .filter(|line| line.starts_with("PDRS_CASE_OK "))
            .collect::<Vec<_>>()
            != [receipt.as_str()]
    {
        return Err(format!(
            "expected one passed test and receipt {id}: {status}\n{stdout}\n{stderr}"
        ));
    }
    Ok(())
}
#[test]
#[ignore = "only launched by bounded child controls"]
fn control_worker() {
    let control = std::env::var("PDRS_PROCESS_CONTROL").unwrap();
    match control.as_str() {
        "timeout" => std::thread::sleep(Duration::from_secs(60)),
        "output" => {
            use std::io::Write;
            loop {
                if std::io::stdout().write_all(&[b'x'; 4096]).is_err() {
                    break;
                }
            }
        }
        "missing" => {}
        "wrong" => receipt("different-case"),
        "duplicate" => {
            receipt(&control);
            receipt(&control);
        }
        "failure" => panic!("controlled failure"),
        // #1632: a descendant that outlives this worker while inheriting its
        // stdout/stderr is what blocks a pipe-based rerun after the worker
        // exits. `sh` forks the sleep into the background and exits at once,
        // so the write end is held by the grandchild, not by this worker.
        "grandchild" => {
            // The grandchild publishes a marker as its very last act, so a
            // caller can tell "returned while it was still alive" from
            // "waited for it to finish" without comparing clocks.
            let marker = std::env::var_os("PDRS_CONTROL_GRANDCHILD_MARKER")
                .expect("the control grandchild publishes an exit marker");
            // The caller owns the sleep: the pipe-based half has to outlast it
            // (that is what blocks it), while the owned half only needs the
            // grandchild to still be alive when it returns — so it asks for a
            // sleep far longer than its own runtime instead of turning the
            // sleep into a deadline for the implementation (r4: 6 s was the
            // owned half's entire budget and would fail a correct rerun on a
            // stalled host).
            let seconds =
                std::env::var("PDRS_CONTROL_GRANDCHILD_SECONDS").unwrap_or_else(|_| "6".to_owned());
            let mut sh = Command::new("sh")
                .arg("-c")
                .arg(format!("sleep {seconds} && : > \"$1\" &"))
                .arg("sh")
                .arg(&marker)
                .spawn()
                .expect("control grandchild");
            // `sh` exits at once and is reaped here; the `sleep` it
            // backgrounded keeps this worker's stdout/stderr open and outlives
            // it, which is the lingering writer under test.
            sh.wait().expect("reap the control grandchild shell");
            receipt(&control);
        }
        // #1632: a worker that never exits and leaves a descendant behind, so
        // the deadline has to kill the whole process group, not just the
        // leader.
        "tree" => {
            // The published pid is the *grandchild* (`sleep`), not `sh`'s own:
            // `sh` is a direct child, so a rerun that killed only the leader
            // and its direct children would still look correct. What has to be
            // proven dead is the process that inherited the streams.
            // `echo $!` writes once and `mv` renames atomically, so the reader
            // never sees a half-written pid file.
            let marker = std::env::var_os("PDRS_CONTROL_TREE_PID")
                .expect("the control tree publishes its descendant pid");
            let mut sh = Command::new("sh")
                .arg("-c")
                .arg("sleep 60 & echo $! > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; wait")
                .arg("sh")
                .arg(&marker)
                .spawn()
                .expect("control tree descendant");
            // Neither this worker nor its descendant ends on its own: only the
            // deadline's group kill stops them.
            let _ = sh.wait();
            std::thread::sleep(Duration::from_secs(60));
        }
        "ok" => receipt(&control),
        other => panic!("unknown control {other}"),
    }
}

/// #1632: a rerun whose descendant inherits stdout/stderr must not block the
/// parent test. The pre-fix shape (`Command::output()`, pipes) only returns
/// once every write end closes, i.e. after the descendant's whole lifetime —
/// here bounded by the 6 s control grandchild, in the audit regression
/// unbounded. The file-backed, group-owned rerun returns with the owned case.
///
/// Both halves are judged by the grandchild's own exit marker, not by elapsed
/// time: the pipe rerun cannot come back before the marker exists, and the
/// owned rerun must come back before it does. The two halves ask for different
/// grandchild lifetimes — the pipe half's sleep *is* its runtime, the owned
/// half gets a sleep far beyond anything the rerun can take — so no assertion
/// is a clock budget that a slow host can exhaust (r3/r4 concurrency).
#[cfg(unix)]
#[test]
fn owned_rerun_is_not_blocked_by_a_grandchild_holding_the_streams() {
    let scratch = crate::test_scratch::Scratch::new("1632-grandchild");
    let legacy_marker = scratch.path().join("legacy-grandchild-exited");
    let owned_marker = scratch.path().join("owned-grandchild-exited");
    let control = "test_process::control_worker";
    let envs = [
        ("PDRS_PROCESS_CONTROL", "grandchild"),
        // The pipe half waits the sleep out, so it is the test's duration.
        ("PDRS_CONTROL_GRANDCHILD_SECONDS", "6"),
        (
            "PDRS_CONTROL_GRANDCHILD_MARKER",
            legacy_marker
                .to_str()
                .expect("the legacy marker path is utf-8"),
        ),
    ];
    let started = Instant::now();
    let legacy = Command::new(std::env::current_exe().expect("legacy rerun binary"))
        .args([
            "--exact",
            control,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(envs)
        .output()
        .expect("legacy pipe rerun");
    let blocked = started.elapsed();
    assert!(
        legacy.status.success(),
        "legacy rerun failed:\n{}\n{}",
        String::from_utf8_lossy(&legacy.stdout),
        String::from_utf8_lossy(&legacy.stderr)
    );
    // The grandchild writes its marker only after its sleep, i.e. just before
    // it exits and closes the write ends the pipe rerun is waiting on.
    assert!(
        legacy_marker.is_file(),
        "the pipe-based rerun returned while the grandchild was still alive ({blocked:?})"
    );
    let envs = [
        ("PDRS_PROCESS_CONTROL", "grandchild"),
        // Far longer than the owned half can take, so the assertion below is
        // "the rerun returned while the grandchild lived", not "the rerun was
        // faster than 6 s" (r4 boundary/concurrency, both families).
        ("PDRS_CONTROL_GRANDCHILD_SECONDS", "60"),
        (
            "PDRS_CONTROL_GRANDCHILD_MARKER",
            owned_marker
                .to_str()
                .expect("the owned marker path is utf-8"),
        ),
    ];
    let started = Instant::now();
    let rerun = rerun_exact(control, &envs, RERUN_LIMIT);
    let bounded = started.elapsed();
    println!("PDRS_RERUN_TIMING legacy_pipe={blocked:?} owned_files={bounded:?}");
    assert!(
        !rerun.timed_out && !rerun.overflow,
        "the owned rerun was stopped (timeout={} overflow={}):\n{}\n{}",
        rerun.timed_out,
        rerun.overflow,
        rerun.stdout,
        rerun.stderr
    );
    assert!(
        rerun.status.success(),
        "the owned rerun failed:\n{}\n{}",
        rerun.stdout,
        rerun.stderr
    );
    assert!(
        rerun.stdout.contains("PDRS_CASE_OK grandchild"),
        "the owned rerun lost its receipt:\n{}\n{}",
        rerun.stdout,
        rerun.stderr
    );
    assert!(
        !owned_marker.is_file(),
        "the owned rerun waited for the grandchild (it published {owned_marker:?} after {bounded:?})"
    );
}

/// #1632: at the deadline the rerun's whole process group dies, not just the
/// leader — otherwise a stuck audit would leave its descendants running and
/// the failure evidence could still be held open.
///
/// The worker publishes its descendant's pid before the deadline, so this
/// cannot pass by killing a group that never had a descendant (a short deadline
/// that fires before the worker spawns would only exercise the direct kill).
/// The probe is the recorded pid, not `kill(-pgid, 0)`: a zombie keeps the
/// group alive in environments whose init does not reap promptly, and a
/// SIGKILLed process that still shows as `Z` has already been reached.
#[cfg(target_os = "linux")]
#[test]
fn owned_rerun_kills_the_whole_group_at_the_deadline() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = crate::test_scratch::Scratch::new("1632-tree");
    let marker = scratch.path().join("descendant.pid");
    let rerun = rerun_exact(
        "test_process::control_worker",
        &[
            ("PDRS_PROCESS_CONTROL", "tree"),
            (
                "PDRS_CONTROL_TREE_PID",
                marker
                    .to_str()
                    .expect("the descendant marker path is utf-8"),
            ),
        ],
        // This budget only has to outlast rerun startup + libtest init + the
        // worker's marker publication, which normally takes well under a
        // second; the worker itself lives ~120 s, so a loaded host has room
        // here without eating into that (r3 / r7 concurrency). The marker is
        // read with its own bounded wait below, so a slow host reports "the
        // worker never published" instead of "the group kill failed".
        Duration::from_secs(30),
    );
    assert!(
        rerun.timed_out,
        "the owned rerun did not hit its deadline:\n{}\n{}",
        rerun.stdout, rerun.stderr
    );
    assert_eq!(
        rerun.status.signal(),
        Some(rustix::process::Signal::KILL.as_raw()),
        "the leader was not SIGKILLed at the deadline"
    );
    // The marker is the worker's own word that it spawned the descendant. Wait
    // for it separately: if it never appears, the group kill above still
    // happened and this failure must say "never published" rather than blame
    // the kill (r7 concurrency).
    let marker_deadline = Instant::now() + Duration::from_secs(15);
    let descendant: i32 = loop {
        match std::fs::read_to_string(&marker) {
            Ok(text) => break text.trim().parse().expect("the published descendant pid"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("read the descendant marker: {error}"),
        }
        assert!(
            Instant::now() < marker_deadline,
            "the control worker never published its descendant pid (marker {}); the group kill \
             itself is not implicated",
            marker.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
            // Gone: reaped by whoever adopted it after the group kill. Only
            // "no such process" proves that — any other read error would
            // silently turn "I could not look" into "it is dead" and pass the
            // very kill this test exists to prove (r4 boundary).
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => panic!("read the descendant's /proc entry: {error}"),
            Ok(stat) => {
                let state = stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .unwrap_or_default()
                    .to_owned();
                // A zombie has already been killed; only its reaping is
                // pending, and that is not this test's subject.
                if state == "Z" {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "descendant {descendant} survived the group kill (state {state})"
                );
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "PDRS_RERUN_GROUP_KILL descendant={descendant} status={:?}",
        rerun.status
    );
}
#[test]
fn bounded_children_reject_zero_match_timeout_output_and_invalid_receipts() {
    let test = "test_process::control_worker";
    for control in [
        "ok",
        "missing",
        "wrong",
        "duplicate",
        "failure",
        "timeout",
        "output",
    ] {
        let result = run_case(
            test,
            control,
            vec![("PDRS_PROCESS_CONTROL".into(), control.into())],
            if control == "timeout" {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(3)
            },
            8192,
        );
        if control == "ok" {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            if matches!(control, "timeout" | "output") {
                assert!(error.contains(control), "{error}");
            }
            println!("PDRS_REJECTED_CHILD {control}");
        }
    }
    assert!(
        run_case(
            "test_process::does_not_exist",
            "zero",
            vec![],
            DEADLINE,
            LIMIT
        )
        .is_err()
    );
    println!("PDRS_REJECTED_CHILD zero-match");
}

// ---------------------------------------------------------------------------
// `/proc` audit toolkit shared by the process audits (#1316, #1349, #1632).
//
// Two snapshots (before/after) cannot see a child that lives and dies between
// them, and a "before" snapshot taken after the window opened can miss a child
// that exits in the gap. Both audits therefore open the window with the
// sampler's first sweep and judge the accumulated sets.
//
// #2138: both audits prove a *negative* — "this window spawned no child" and
// "no other process holds the data dir open" — so a `/proc` read the audit
// could not perform is an unknown, never evidence of absence. Three rules,
// mirroring the design settled for #1409's return-boundary scan (issue #1409,
// whose implementation lands with PR #2166 in `tests/scratch_publication.rs`;
// that scan proves a descriptor was released, this module proves that no child
// and no holder exists):
//
//   1. a failed read is classified by source ([`BlindSource`]) before it may
//      be skipped; a source that could be what the audit hunts fails it closed;
//   2. every sweep first proves, through the same primitives, that it can see
//      this process's own stat / descriptors — a host that hides `/proc`
//      fails the audit instead of passing it on nothing;
//   3. a sweep that read no process at all fails: "saw nothing" is not
//      evidence of "nothing there".
//
// The integration-test crate cannot share crate-private code with this module,
// and the two audits hunt different objects, so #1409's rules are mirrored by
// concept rather than by symbol. A pipe can only be inherited, so another
// user's process can never hold it; a data dir can be opened by anyone with
// the permissions, which is why the holder audit narrows the skip set further
// ([`BlindSources::of_holder`]). Move the two issues together when the rule
// itself changes.
// ---------------------------------------------------------------------------

/// #2138: the state and parent fields of `/proc/<pid>/stat`. Everything is
/// read after the *last* `)` because the comm field between the parentheses
/// may contain spaces and parentheses of its own.
#[cfg(target_os = "linux")]
fn stat_fields(stat: &str) -> Option<(char, &str)> {
    let (_, tail) = stat.rsplit_once(')')?;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?;
    Some((state, parent))
}

/// #2138: whether `pid` is this process or one of its descendants — the only
/// processes that can be a child the child audit failed to see. An unreadable
/// chain is an unknown and counts as related, so it fails the audit closed
/// rather than passing it. Known limits, both needing pid reuse to be reached
/// at all: an ancestor reaped and its pid reused mid-walk would send the chain
/// into an unrelated tree, and a `SCM_RIGHTS` handoff is an inheritance the
/// parent chain does not show (irrelevant for pids: a pid is a child by the
/// kernel's own parent link, not by descriptor inheritance).
#[cfg(target_os = "linux")]
fn descended_from_this_process(pid: &str) -> bool {
    let mine = std::process::id().to_string();
    let mut current = pid.to_owned();
    for _ in 0..64 {
        if current == mine {
            return true;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{current}/stat")) else {
            return true;
        };
        let Some((_, parent)) = stat_fields(&stat) else {
            return true;
        };
        if parent == "0" {
            return false;
        }
        current = parent.to_owned();
    }
    true
}

/// #2138: whether a `Z`/`X` state means the whole *thread group* is dead, and
/// with it the group's descriptor table. A thread-group leader can be a zombie
/// while its group lives on (`pthread_exit` in the leader): the surviving
/// threads share the fd table, so a descriptor is still open, and
/// `/proc/<tgid>/fd` answers EACCES in exactly that state (#1409 measured it
/// on cb8 6.17, `_tmp/20261107-issue-1409/zombie-leader-probe3.log`). A zombie
/// leader alone therefore proves nothing; only a group whose every task is
/// `Z`/`X` has closed its descriptors. Anything unreadable is *not* dead, so
/// the caller keeps treating the source as live.
#[cfg(target_os = "linux")]
fn thread_group_is_dead(pid: &str) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    let mut seen = 0usize;
    for task in tasks {
        let Ok(task) = task else { return false };
        let Ok(stat) = std::fs::read_to_string(task.path().join("stat")) else {
            return false;
        };
        if !matches!(stat_fields(&stat), Some(('Z' | 'X', _))) {
            return false;
        }
        seen += 1;
    }
    seen != 0
}

/// #2138 (#2142 fix-r1, boundary r1 #4; fix-r2, sibling r2 #2): the one
/// definition of "this state means the source is terminal" — `Z`/`X` alone
/// proves nothing (a zombie leader with a live thread group still shares its
/// fd table, see [`thread_group_is_dead`]), only a dead thread group behind
/// the state does. [`blind_source`] and [`settles_into_a_terminal_source`]
/// both end here, so the first check and the settle re-check cannot disagree
/// on the same pid — the criterion used to be written out twice at those two
/// call sites. The publication tests keep their own copy
/// (`tests/scratch_publication.rs`, same name): change the two together —
/// and since fix-r2 "together" is machine-checked on both sides (each copy's
/// body is a negative-control anchor, and each crate's unit test pins the
/// Terminal verdict of both entries).
#[cfg(target_os = "linux")]
fn terminal_state(pid: &str, state: char) -> bool {
    matches!(state, 'Z' | 'X') && thread_group_is_dead(pid)
}

/// #2138: how long a related source may still be finishing its exit before its
/// unreadable entries count as hidden while alive. `exit_files()` runs before
/// `exit_notify()` marks the task a zombie, and in that window the descriptor
/// directory already fails while `/proc/<pid>/stat` still says `R` (#1409
/// measured the window on cb8 6.17: p99 48µs, max 2.7ms). Failing on that
/// first read would turn a healthy run red; failing only after the source
/// settles keeps a live descendant with hidden descriptors a hard failure.
#[cfg(target_os = "linux")]
const BLIND_SOURCE_SETTLE: Duration = Duration::from_millis(50);

/// #2138: whether the process behind a failed `/proc` read could be what the
/// audit is looking for. A child of this process is a descendant by
/// definition, so a source that is gone, already dead, or provably unrelated
/// cannot be a child the sweep failed to see; everything else is an unknown,
/// and on a host that hides `/proc` treating an unreadable related process as
/// "nothing there" is exactly how the audit would report a clean result it
/// never verified.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlindSource {
    /// Gone between the `/proc` listing and the read: it holds nothing.
    Vanished,
    /// A zombie or a dead task: the kernel already closed its descriptors.
    Terminal,
    /// Not this process and not one of its descendants.
    Unrelated,
    /// This process, a descendant, or a task whose relation cannot be read.
    Related,
}

#[cfg(target_os = "linux")]
impl BlindSource {
    /// #2138: whether a source can be what the *child* audit hunts at all. A
    /// child is a descendant by definition, so a source that is gone, already
    /// dead, or provably unrelated cannot be the child a sweep failed to see;
    /// this is the one place that skip set is written, so a new variant or a
    /// narrowed set cannot silently apply on one side only. The holder audit
    /// hunts non-descendants and therefore cannot reuse the set unchanged —
    /// see [`BlindSources::of_holder`].
    fn holds_nothing(self) -> bool {
        matches!(self, Self::Vanished | Self::Terminal | Self::Unrelated)
    }
}

/// #2138: one classification per holder per sweep. [`blind_source`] may spend
/// the whole settle window on a single holder, and a sweep meets the same
/// holder once per descriptor, so without the memo one hidden holder would
/// cost N × `BLIND_SOURCE_SETTLE` inside a sampler cycle that runs every
/// millisecond. A memo cannot grow stale in a way that loosens the sweep:
/// `Terminal`/`Vanished` are final, `Unrelated` cannot become a descendant,
/// and a cached `Related` is the fail-closed verdict.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct BlindSources {
    seen: std::collections::HashMap<String, BlindSource>,
}

#[cfg(target_os = "linux")]
impl BlindSources {
    /// The classification of `pid`, decided once per sweep.
    fn of(&mut self, pid: &str) -> BlindSource {
        *self
            .seen
            .entry(pid.to_owned())
            .or_insert_with(|| blind_source(pid))
    }

    /// #2138: the holder audit's classification, used by *both* of its error
    /// arms. That audit hunts processes that are *not* descendants by
    /// definition, so "unrelated" is no reason to skip one: a process of this
    /// very user, leaked out of an earlier run and reparented to init, is
    /// unrelated and can still hold the data dir — exactly the regression the
    /// audit exists to catch.
    ///
    /// The only skippable source is one the kernel hides from us *as a whole*,
    /// which [`descriptors_are_policy_hidden`] decides. That is a strictly
    /// smaller set than "not this user", and it is the set a healthy host
    /// actually produces: on cb8 every same-uid process the audit cannot read
    /// also refuses `/proc/<pid>/exe` (measured: session `sshd`, `gpg-agent`,
    /// `(sd-pam)`, and `systemd --user` — the last one lists its fd directory,
    /// 143 entries, but denies every link *and* its own `exe`). A source whose
    /// `exe` we can read is a process we are allowed to inspect, so a denied
    /// descriptor there is not policy but an anomaly: it stays `Related` and
    /// fails the audit closed.
    ///
    /// Before failing it, though, an unrelated source of this user gets the
    /// same settle window a related one gets: `exit_files()` runs before
    /// `exit_notify()`, so a process of this user — another tree's `cargo`, a
    /// shell command — can lose its descriptor table while `stat` still reports
    /// it alive. Such a source is exiting, not hiding, and the window is short
    /// (#1409 measured p99 48 µs, max 2.7 ms), so waiting it out is both
    /// cheaper and truer than a false red on a healthy host.
    fn of_holder(&mut self, pid: &str) -> BlindSource {
        let source = self.of(pid);
        if source != BlindSource::Unrelated || descriptors_are_policy_hidden(pid) {
            return source;
        }
        let settled = settles_into_a_terminal_source(pid);
        if settled.holds_nothing() {
            return settled;
        }
        BlindSource::Related
    }
}

/// #2138: whether the kernel denies us this process *entirely*, so that its
/// descriptors are unreadable for a reason that has nothing to do with what
/// the audit is looking for.
///
/// The test is `/proc/<pid>/exe`: it is subject to the same ptrace policy as
/// `/proc/<pid>/fd`, but a process whose `exe` is readable is one the kernel
/// lets us inspect, so a denied descriptor there is an anomaly and must not be
/// excused. Measured on cb8 (uid 1000) across every same-uid process: the
/// readable ones (`cargo`, `rustc`, `bash`, `pipewire`, …) answer both reads;
/// the denied ones (`sshd`, `gpg-agent`, `(sd-pam)`, `systemd --user`) answer
/// neither. No process was seen with a readable `exe` and a denied fd link,
/// which is the shape an LSM that distinguishes the two would produce — that
/// shape fails closed here, by design.
///
/// A missing `exe` is *not* by itself proof of anything. A zombie thread-group
/// leader has no `exe` either, yet its live threads still share the descriptor
/// table — this file's own #1409 `probe3` note says so — so it can hold the
/// data dir the whole time the audit would be skipping it. `NotFound` therefore
/// only excuses a process provably of another user (a kernel thread has no
/// `exe` and is not ours); a missing `exe` on a source of this user fails the
/// audit closed. Every other error means the kernel gave us an answer we
/// cannot interpret, so the source stays `Related` (fail-closed) as well.
#[cfg(target_os = "linux")]
fn descriptors_are_policy_hidden(pid: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(_) => false,
        // The kernel refuses us this process as a whole, which is what a
        // non-dumpable process of any user looks like (measured on cb8:
        // `systemd --user` denies `exe` while listing its fd directory).
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => true,
        // No `exe` at all: only another user's process (or a kernel thread,
        // which is root's) may be passed over. `false` when ownership cannot
        // be read — that is not proof of someone else.
        Err(error) if error.kind() == io::ErrorKind::NotFound => matches!(
            (
                std::fs::metadata(format!("/proc/{pid}")).map(|entry| entry.uid()),
                std::fs::metadata("/proc/self").map(|entry| entry.uid()),
            ),
            (Ok(theirs), Ok(ours)) if theirs != ours
        ),
        Err(_) => false,
    }
}

/// #2138: classify a source whose `/proc` reads failed, for the sweep's
/// "may this be skipped" decision.
#[cfg(target_os = "linux")]
fn blind_source(pid: &str) -> BlindSource {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        // No stat at all: the task is gone between the listing and the read.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return BlindSource::Vanished,
        // Any other failure leaves the relation unknown: never skipped.
        Err(_) => return BlindSource::Related,
    };
    let Some((state, _)) = stat_fields(&stat) else {
        return BlindSource::Related;
    };
    // A zombie leader with a live group has not released its descriptors, so
    // "terminal" is the whole group being dead, not one task reporting `Z`.
    if terminal_state(pid, state) {
        return BlindSource::Terminal;
    }
    if descended_from_this_process(pid) {
        settles_into_a_terminal_source(pid)
    } else {
        BlindSource::Unrelated
    }
}

/// #2138: re-read a related source's state until it either proves terminal — a
/// dead thread group, a dead task, or gone — or the settle window runs out, in
/// which case it is still an unknown and the sweep fails closed on it.
#[cfg(target_os = "linux")]
fn settles_into_a_terminal_source(pid: &str) -> BlindSource {
    let deadline = Instant::now() + BLIND_SOURCE_SETTLE;
    loop {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => match stat_fields(&stat) {
                Some((state, _)) if terminal_state(pid, state) => {
                    return BlindSource::Terminal;
                }
                Some(_) => {}
                None => return BlindSource::Related,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return BlindSource::Vanished;
            }
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return BlindSource::Related;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// #2138: the child audit's own eyes. Before a sweep's child set is trusted it
/// must read the stat of the process running it, through the same primitive it
/// uses on everyone else. A host that hides `/proc` (hidepid, gVisor, a
/// restricted container, an LSM) then fails the audit instead of returning an
/// empty child list that proves nothing.
#[cfg(target_os = "linux")]
fn this_process_stat_is_visible(proc_root: &std::path::Path, mine: &str) -> io::Result<()> {
    let stat = std::fs::read_to_string(proc_root.join(mine).join("stat")).map_err(|error| {
        io::Error::other(format!(
            "#2138 the child-process audit cannot read its own {}/{mine}/stat ({error}): \
             a clean child list would prove nothing",
            proc_root.display()
        ))
    })?;
    if stat_fields(&stat).is_none() {
        return Err(io::Error::other(format!(
            "#2138 the child-process audit cannot parse its own {}/{mine}/stat ({stat:?}): \
             a clean child list would prove nothing",
            proc_root.display()
        )));
    }
    Ok(())
}

/// #2138: the child audit behind [`child_processes`], with the procfs root
/// injectable so a negative control can simulate a host that hides this
/// process's own stat (production always passes `/proc`). The only injected
/// read is the listing and the per-pid `stat`/`exe`; the classification reads
/// the real `/proc`, because it decides what a real pid is.
#[cfg(target_os = "linux")]
fn child_processes_in(
    proc_root: &std::path::Path,
) -> io::Result<Vec<(i32, Option<std::path::PathBuf>)>> {
    let mine = std::process::id().to_string();
    // #2138: the positive self-check comes first — a sweep that cannot see its
    // own process must fail, never pass on an empty result.
    this_process_stat_is_visible(proc_root, &mine)?;
    let mut children = Vec::new();
    // #2138: the sweep counts what it could read and fails when that count is
    // zero; "saw nothing" is not evidence of "nothing there".
    let mut readable = 0usize;
    let mut skipped = 0usize;
    let mut sources = BlindSources::default();
    let entries = std::fs::read_dir(proc_root).map_err(|error| {
        io::Error::other(format!(
            "#1316 the audit reads {}: {error}",
            proc_root.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            io::Error::other(format!(
                "#1316 the audit reads an entry of {}: {error}",
                proc_root.display()
            ))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // A numeric name outside `i32` cannot be a pid this audit can meet
        // (Linux pids are bounded far below that); it is not a blind read.
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        let stat = match std::fs::read_to_string(proc_root.join(&name).join("stat")) {
            Ok(stat) => {
                readable += 1;
                stat
            }
            // #2138: a failed read is never silently skipped. A source that
            // cannot be a child of this process may be passed over; anything
            // else is an unknown the audit fails closed on, because a hidden
            // stat is exactly where a child would hide.
            Err(error) => {
                if sources.of(&name).holds_nothing() {
                    skipped += 1;
                    continue;
                }
                return Err(io::Error::other(format!(
                    "#2138 reading the stat of {name}: {error}"
                )));
            }
        };
        // `comm)` is followed by state and ppid. A readable stat whose fields
        // do not parse is an unknown too: it cannot prove the process is not
        // our child, so it fails the audit instead of being skipped.
        let Some((_, ppid)) = stat_fields(&stat) else {
            return Err(io::Error::other(format!(
                "#2138 the stat of {name} has no parsable state/parent ({stat:?})"
            )));
        };
        if ppid == mine.as_str() {
            let exe = std::fs::read_link(proc_root.join(&name).join("exe")).ok();
            children.push((pid, exe));
        }
    }
    if readable == 0 {
        return Err(io::Error::other(format!(
            "#2138 the child-process audit could not read the stat of any process \
             ({skipped} skipped): a clean result would prove nothing"
        )));
    }
    children.sort();
    Ok(children)
}

/// #2138: every child of this process, sorted. Fails (panics) when the sweep
/// cannot prove it saw what it claims — see [`child_processes_in`].
#[cfg(target_os = "linux")]
pub(crate) fn child_processes() -> Vec<(i32, Option<std::path::PathBuf>)> {
    child_processes_in(std::path::Path::new("/proc")).unwrap_or_else(|error| panic!("{error}"))
}

/// #2138: the data-dir audit's own eyes. Before a sweep's holder list is
/// trusted it must resolve a descriptor this process provably holds through
/// the very primitives it uses on everyone else — `/proc/<pid>/fd` and
/// `read_link`. A host that hides descriptors from `/proc` then fails the
/// audit instead of reporting that no one holds the data dir.
#[cfg(target_os = "linux")]
fn this_process_descriptors_are_visible(proc_root: &std::path::Path, mine: &str) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // A pipe this process holds for the whole check: the kernel always resolves
    // its `/proc/<pid>/fd/<n>` link, while the data dir's own files may
    // legitimately be closed at any point in the audit.
    let (_reader, writer) = io::pipe().map_err(|error| {
        io::Error::other(format!(
            "#2138 creating the data-dir audit's self-check pipe: {error}"
        ))
    })?;
    let directory = proc_root.join(mine).join("fd");
    let entries = std::fs::read_dir(&directory).map_err(|error| {
        io::Error::other(format!(
            "#2138 the data-dir audit cannot list {} ({error}): \
             a clean holder list would prove nothing",
            directory.display()
        ))
    })?;
    let wanted = writer.as_raw_fd().to_string();
    let mut seen = false;
    for entry in entries {
        let entry = entry.map_err(|error| {
            io::Error::other(format!(
                "#2138 the data-dir audit cannot enumerate {}: {error}",
                directory.display()
            ))
        })?;
        if entry.file_name().to_string_lossy() != wanted {
            continue;
        }
        seen = true;
        let target = std::fs::read_link(entry.path()).map_err(|error| {
            io::Error::other(format!(
                "#2138 the data-dir audit cannot resolve its own descriptor {}: {error}",
                entry.path().display()
            ))
        })?;
        if !target.to_string_lossy().starts_with("pipe:[") {
            return Err(io::Error::other(format!(
                "#2138 the data-dir audit resolved its own descriptor {} to {} instead of \
                 the pipe it holds",
                entry.path().display(),
                target.display()
            )));
        }
    }
    if !seen {
        return Err(io::Error::other(format!(
            "#2138 the data-dir audit cannot find its own descriptor in {}: \
             a clean holder list would prove nothing",
            directory.display()
        )));
    }
    Ok(())
}

/// #2138: the data-dir audit behind [`TransientAudit`], with the procfs root
/// injectable for the same negative-control reason as [`child_processes_in`].
#[cfg(target_os = "linux")]
fn foreign_data_dir_holders_in(
    proc_root: &std::path::Path,
    root: &std::path::Path,
) -> io::Result<Vec<i32>> {
    let me = std::process::id();
    let mine = me.to_string();
    // #2138: the positive self-check comes first — a sweep that cannot see its
    // own descriptors must fail, never pass on an empty holder list.
    this_process_descriptors_are_visible(proc_root, &mine)?;
    let mut holders = Vec::new();
    let mut readable = 0usize;
    let mut skipped = 0usize;
    let mut sources = BlindSources::default();
    let entries = std::fs::read_dir(proc_root).map_err(|error| {
        io::Error::other(format!(
            "#1316 the audit reads {}: {error}",
            proc_root.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            io::Error::other(format!(
                "#1316 the audit reads an entry of {}: {error}",
                proc_root.display()
            ))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // See [`child_processes_in`]: a numeric name outside `i32` is not a
        // pid this audit can meet, and not a blind read.
        let Ok(pid) = name.parse::<i32>() else {
            continue;
        };
        // This process is the audit itself, not an external holder.
        if name == mine {
            continue;
        }
        let descriptors = match std::fs::read_dir(proc_root.join(&name).join("fd")) {
            Ok(descriptors) => {
                readable += 1;
                descriptors
            }
            // #2138: a failed read is never silently skipped. Only a source
            // that provably holds nothing may be passed over; anything else
            // is an unknown the audit fails closed on, because a hidden
            // descriptor of a live process is exactly where a foreign holder
            // would hide — and a process of this user is not exempt from
            // being that holder just because it is not a descendant.
            Err(error) => {
                if sources.of_holder(&name).holds_nothing() {
                    skipped += 1;
                    continue;
                }
                return Err(io::Error::other(format!(
                    "#2138 reading the descriptors of {name}: {error}"
                )));
            }
        };
        for descriptor in descriptors {
            let descriptor = descriptor.map_err(|error| {
                io::Error::other(format!(
                    "#2138 enumerating the descriptors of {name}: {error}"
                ))
            })?;
            let target = match std::fs::read_link(descriptor.path()) {
                Ok(target) => target,
                // #2138: only a link that vanished between the enumeration and
                // the read is a release. Any other errno (EACCES, EIO) leaves
                // a descriptor the audit cannot see, so the source decides —
                // with the same [`BlindSources::of_holder`] rule as the
                // directory arm above. That rule is what keeps this arm off a
                // healthy host: a non-dumpable process of this very user lists
                // its descriptor directory and then denies every link *and*
                // its own `exe` (measured on cb8: `systemd --user`, 143
                // entries listed, `fd/0` EACCES, `exe` EACCES), so it is
                // skipped as policy-hidden. A process whose `exe` is readable
                // is one we may inspect, and its denied link fails the audit.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    if sources.of_holder(&name).holds_nothing() {
                        continue;
                    }
                    return Err(io::Error::other(format!(
                        "#2138 reading the descriptor {} of {name}: {error}",
                        descriptor.path().display()
                    )));
                }
            };
            if target.starts_with(root) {
                holders.push(pid);
                break;
            }
        }
    }
    if readable == 0 {
        return Err(io::Error::other(format!(
            "#2138 the data-dir audit could not read the descriptors of any other process \
             ({skipped} skipped): a clean result would prove nothing"
        )));
    }
    holders.sort();
    Ok(holders)
}

/// Everything the sampler observed inside the write window. A named alias keeps
/// the `JoinHandle` field and the `finish` return type under
/// `clippy::type_complexity` (CI runs clippy with `-D warnings`).
#[cfg(target_os = "linux")]
pub(crate) type TransientObservations = (TransientChildren, std::collections::BTreeSet<i32>);

/// The child set of one `/proc` sweep, keyed by pid.
#[cfg(target_os = "linux")]
pub(crate) type TransientChildren = std::collections::BTreeMap<i32, Option<std::path::PathBuf>>;

/// Children `seen` reports that the window did not open with, as
/// `pid:exe` strings ready for a failure message.
///
/// This is the audit predicate, shared so the two audits cannot drift: an
/// **exit** is not a spawn, so the end snapshot is judged as a subset of the
/// before-set, not by equality. Applying the same function to the sampler's
/// accumulated set also covers a child that lived and died between two
/// snapshots.
#[cfg(target_os = "linux")]
pub(crate) fn unknown_children(
    seen: &TransientChildren,
    before: &TransientChildren,
) -> Vec<String> {
    seen.iter()
        // Compared as (pid, executable), not by pid alone: a pid the window
        // opened with but whose executable changed is a *different* process
        // that reused the number, and a pid-only subset would hide exactly the
        // spawn this audit exists to catch (r4 boundary). An unreadable
        // executable never matches a readable one, so the stricter direction
        // is also the fail-closed one — the one legitimate source of that
        // mismatch, a child turning into an unreadable zombie, is kept out of
        // the map by the sampler (it keeps the last readable exe, r5 boundary).
        .filter(|(pid, exe)| before.get(*pid) != Some(*exe))
        .map(|(pid, exe)| format!("{pid}:{exe:?}"))
        .collect()
}

/// The window's closing child set.
///
/// A bare sweep reads `None` for a child that has exited but not been reaped —
/// a zombie's `/proc/<pid>/exe` no longer resolves — while the window's
/// before-set holds the exe it had. Compared as `(pid, exe)`, that turns the
/// *exit* into a mismatch and [`unknown_children`] reports it as a spawn
/// (#1632 r6 boundary). Folding in the sampler's per-pid last readable exe
/// keeps the exit an exit; only pids the sampler already saw are folded, so a
/// pid the window did not open with is still judged from its own sweep.
#[cfg(target_os = "linux")]
pub(crate) fn end_snapshot(seen: &TransientChildren) -> TransientChildren {
    let mut after: TransientChildren = child_processes().into_iter().collect();
    for (pid, exe) in after.iter_mut() {
        if exe.is_none()
            && let Some(Some(known)) = seen.get(pid)
        {
            *exe = Some(known.clone());
        }
    }
    after
}

/// The one verdict both `/proc` audits share: the audited window spawned no
/// child at all.
///
/// Three predicates, in this order: the end snapshot adds nothing the window
/// opened with, nothing appeared even transiently, and no child runs a node
/// executable. Returns the failure text (which keeps the offending set) or
/// `None` when the window is clean.
///
/// Sharing the *combination* — not just the primitives — is the point: #1316
/// and #1349 used to spell these three out separately, and a future predicate
/// added to one copy would silently miss the other (#1632 r7 simplify). The
/// caller picks the assertion macro (`negative_assert!` for the audit that also
/// has to fail a negative-control run), so only the predicate lives here.
#[cfg(target_os = "linux")]
pub(crate) fn child_spawn_failure(
    issue: &str,
    what: &str,
    seen: &TransientChildren,
    before: &TransientChildren,
) -> Option<String> {
    let after = end_snapshot(seen);
    let unknown_after = unknown_children(&after, before);
    if !unknown_after.is_empty() {
        return Some(format!(
            "{issue} {what} spawn no child processes ({unknown_after:?})"
        ));
    }
    let transient = unknown_children(seen, before);
    if !transient.is_empty() {
        return Some(format!(
            "{issue} {what} spawn no child processes, even transiently ({transient:?})"
        ));
    }
    let node = node_children(seen);
    if !node.is_empty() {
        return Some(format!(
            "{issue} no child process runs a node executable ({node:?})"
        ));
    }
    None
}

/// Every child the window observed that runs a node executable, as
/// `pid:exe` strings.
///
/// Deliberately *not* filtered to the added set, unlike [`unknown_children`]:
/// the audit claims no TS process participates at any point in the window, and
/// a node child that already existed when the window opened can still serve
/// the writes. Narrowing this to "spawned inside the window" turns a pre-window
/// node helper into a false PASS. The conservative direction is a failure when
/// an unrelated node child happens to exist, not a silent green.
#[cfg(target_os = "linux")]
pub(crate) fn node_children(seen: &TransientChildren) -> Vec<String> {
    seen.iter()
        .filter(|(_, exe)| {
            exe.as_ref()
                .is_some_and(|path| path.to_string_lossy().to_lowercase().contains("node"))
        })
        .map(|(pid, exe)| format!("{pid}:{exe:?}"))
        .collect()
}

/// Poll `/proc` for the whole audited write window.
///
/// Two snapshots (before/after) say nothing about a child or fd holder whose
/// lifetime is shorter than the gap between them, so a transient writer would
/// slip through both and leave the audit green. This sampler keeps running
/// until `finish`, accumulating every child pid and every foreign fd holder it
/// ever observes; the assertions then judge the accumulated sets.
///
/// Coverage is bracketed on both ends, not assumed: `start` returns only after
/// the first full sweep has completed, together with that sweep's child set
/// (the window's "before" snapshot, see [`TransientAudit::start`]), and
/// `finish` sets the stop flag *before*
/// the worker takes its last sweep (`done` is read at the top of the cycle, so a
/// stop request during a sweep still buys one more), which means the worker
/// always sweeps once after the last write. The residual blind spot is the gap
/// between two consecutive sweeps — one `/proc` pass pair plus the 1 ms sleep,
/// i.e. proportional to the host's process and fd count, not 1 ms. A writer
/// whose entire lifetime fits inside one such gap is invisible to this audit.
#[cfg(target_os = "linux")]
pub(crate) struct TransientAudit {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// #2138: the first audit read the sampler could not perform. A sweep that
    /// cannot see must fail the run, not hand back an empty observation set —
    /// "saw nothing" is not evidence of "nothing there".
    failure: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    worker: Option<std::thread::JoinHandle<TransientObservations>>,
}

#[cfg(target_os = "linux")]
impl TransientAudit {
    /// Open the sampling window and return the first sweep's child set.
    ///
    /// The "before" snapshot and the opening of the window must be the same
    /// observation. Taking `child_processes()` *after* `start` returns leaves a
    /// gap in which a child that already existed when the window opened can
    /// exit; the sampler has it in `children_seen` but the late snapshot does
    /// not, so the audit reports it as a transient writer and fails a run in
    /// which nothing regressed (#1632). Returning the first sweep removes that
    /// gap: the returned set is exactly what the sampler saw when the window
    /// opened.
    pub(crate) fn start(root: std::path::PathBuf) -> (Self, TransientChildren) {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        // #2138: an audit read that fails inside the sampler must reach the
        // test as a failure carrying its reason. The worker stores the first
        // failure here and `finish` re-raises it; the opening sweep also sends
        // it, so `start` fails with the reason instead of waiting on a sweep
        // that never completes.
        let failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let worker_failure = failure.clone();
        // `start` must not hand control back before the first sweep has run:
        // otherwise the write could begin during the thread's scheduling delay
        // and the opening of the window would be unsampled.
        let (swept, first_sweep) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut children = TransientChildren::new();
            let mut holders = std::collections::BTreeSet::new();
            let mut announced = false;
            loop {
                // Read the flag before sweeping: a stop request that lands while
                // this sweep runs still leaves one more sweep after the writes.
                let done = flag.load(std::sync::atomic::Ordering::SeqCst);
                let sweep = child_processes_in(std::path::Path::new("/proc")).and_then(|seen| {
                    Ok((
                        seen,
                        foreign_data_dir_holders_in(std::path::Path::new("/proc"), &root)?,
                    ))
                });
                let (spawned, foreign) = match sweep {
                    Ok(observation) => observation,
                    // #2138: never sample on from a blind read — stop and let
                    // the failure surface at `start`/`finish`.
                    Err(error) => {
                        let text = format!("{error}");
                        if !announced {
                            let _ = swept.send(Err(text.clone()));
                        }
                        *worker_failure
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(text);
                        return (children, holders);
                    }
                };
                for (pid, exe) in spawned {
                    // A child that exits is a zombie until it is reaped, and a
                    // zombie's `/proc/<pid>/exe` no longer resolves. Overwriting
                    // the exe already read for that pid with `None` would turn
                    // the *exit* into a different `(pid, exe)` pair, and
                    // `unknown_children` would report a spawn for a child that
                    // merely died — the false FAIL this audit exists to remove
                    // (#1632 r5 boundary). Keep the last readable exe: an
                    // unreadable *first* observation still records `None`, and a
                    // readable change still overwrites (pid reuse or an exec).
                    if exe.is_none() && matches!(children.get(&pid), Some(Some(_))) {
                        continue;
                    }
                    children.insert(pid, exe);
                }
                for pid in foreign {
                    holders.insert(pid);
                }
                if !announced {
                    announced = true;
                    // The first sweep is the window's opening snapshot. A
                    // closed receiver only means `start` was abandoned; keep
                    // sampling, `Drop` still stops this thread.
                    let _ = swept.send(Ok(children.clone()));
                }
                if done {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            (children, holders)
        });
        let children_before = match first_sweep.recv() {
            Ok(Ok(children)) => children,
            Ok(Err(error)) => panic!("{error}"),
            Err(_) => panic!("#1316 the audit sampler completes its first sweep"),
        };
        (
            Self {
                stop,
                failure,
                worker: Some(worker),
            },
            children_before,
        )
    }

    pub(crate) fn finish(mut self) -> TransientObservations {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let joined = self
            .worker
            .take()
            .expect("#1316 the audit sampler is running")
            .join();
        // A panic in the sweep keeps its own payload (it is not this module's
        // failure path); an audit read that could not be performed is re-raised
        // here with the reason the sweep recorded (#2138).
        let observations = joined.unwrap_or_else(|payload| std::panic::resume_unwind(payload));
        if let Some(failure) = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            panic!("{failure}");
        }
        observations
    }
}

/// A panic before `finish` must not leave a spinning sampler in the test
/// process; stopping it here also keeps the owned rerun process bounded.
///
/// #2138: [`TransientAudit::finish`] is the reporting path for a sweep that
/// could not read `/proc`. `Drop` cannot report it the same way — panicking
/// while the test is already unwinding aborts the process — but it must not
/// swallow the reason either: a test that fails in the window would otherwise
/// blame its own assertion and hide that the audit itself went blind. The
/// reason is written to stderr, without panicking, so it survives in the
/// captured test output.
#[cfg(target_os = "linux")]
impl Drop for TransientAudit {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = failure.as_deref() {
            use std::io::Write as _;
            let _ = writeln!(io::stderr(), "{reason}");
        }
    }
}

/// #2138: the stat parse must survive a comm field with spaces and parentheses
/// — every field is read after the *last* `)`, because comm may contain both.
#[cfg(target_os = "linux")]
#[test]
fn the_stat_fields_are_read_after_the_last_parenthesis() {
    let (state, parent) = stat_fields("42 (a ) b (c) Z 7 42 42 0 -1 4194304")
        .expect("#2138 the stat fields of a comm with parentheses must parse");
    assert_eq!(
        (state, parent),
        ('Z', "7"),
        "#2138 the state and parent follow the last ')'"
    );
    assert!(
        stat_fields("no parentheses here").is_none(),
        "#2138 a stat without its comm parentheses must not be guessed at"
    );
}

/// #2138: the classification that decides whether a failed `/proc` read may be
/// passed over. This process is always related (its own children are what the
/// audit reads first), init cannot descend from a test process, and a reaped
/// child is gone. #2142 fix-r2 (sibling r2 #2 / boundary r2 #2 / simplify r2
/// #1, five reviewers): this test also walks the settle entry —
/// `blind_source` of this very process passes the enumeration's first check
/// as non-terminal and descends into [`settles_into_a_terminal_source`] —
/// so it is where the Terminal side of this crate's [`terminal_state`]
/// criterion is pinned, through both entries on one dead pid, mirroring the
/// publication tests' tuple assert: a killed, un-reaped group with no live
/// task must read `Terminal` from the enumeration's first check and from
/// the settle re-check alike. One assert holds both verdicts, so a criterion
/// that drifts — or is hardcoded away — shows both halves flipping in the
/// failure payload; the publication copy of the criterion has its own
/// mutant, and this one is the negative control bound here
/// (`terminal-state-production-hardcoded-false`).
#[cfg(target_os = "linux")]
#[test]
fn a_blind_source_is_skipped_only_when_it_cannot_be_a_child() {
    let mine = std::process::id().to_string();
    assert_eq!(
        blind_source(&mine),
        BlindSource::Related,
        "#2138 this process is the audit's own source and must never be skipped"
    );
    // Init cannot descend from this process — unless the test binary *is* PID 1
    // (a container entrypoint), in which case the self check below is the same
    // assertion and this one must not contradict it; and on a host that hides
    // other users' processes (`hidepid`) `/proc/1/stat` is not readable, which
    // is the blind case this audit exists to fail on, not a classification
    // result to assert.
    if mine != "1" && std::path::Path::new("/proc/1/stat").exists() {
        assert_eq!(
            blind_source("1"),
            BlindSource::Unrelated,
            "#2138 init cannot descend from this process"
        );
    }
    let mut child = Command::new("/bin/true")
        .spawn()
        .expect("#2138 spawn the vanished-source probe");
    let pid = child.id().to_string();
    let _ = child.wait().expect("#2138 reap the vanished-source probe");
    assert_eq!(
        blind_source(&pid),
        BlindSource::Vanished,
        "#2138 a reaped process is gone and cannot be a child"
    );
    // #2142: the Terminal verdict of this crate's criterion, through both
    // entries, on one pid — the production twin of the publication tests'
    // dead-group probe. The probe is killed but not reaped, so it is still
    // in `/proc` as an all-`Z` group: terminal for the enumeration's first
    // check (inside `blind_source`) and for the settle re-check (called
    // directly, the entry a related-but-alive source reaches when the first
    // check does not fire). Both calls are one expression, so a criterion
    // that is hardcoded away fails with both verdicts in its payload.
    let mut dead = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("#2142 spawn the dead-group probe");
    let dead_pid = dead.id().to_string();
    dead.kill().expect("#2142 kill the dead-group probe");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{dead_pid}/stat")).unwrap_or_default();
        if matches!(stat_fields(&stat), Some(('Z', _))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "#2142 the dead-group probe must become a zombie, not stay {stat:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        (
            blind_source(&dead_pid),
            settles_into_a_terminal_source(&dead_pid)
        ),
        (BlindSource::Terminal, BlindSource::Terminal),
        "#2142 a dead thread group must be terminal through both the enumeration and the settle"
    );
    let _ = dead.wait().expect("#2142 reap the dead-group probe");
}

/// #2138: the child audit's own eyes — on a Linux host it must see the stat of
/// the process running it, and the sweep must not count this process as its
/// own child.
#[cfg(target_os = "linux")]
#[test]
fn this_process_stat_is_visible_to_the_child_audit() {
    let mine = std::process::id().to_string();
    this_process_stat_is_visible(std::path::Path::new("/proc"), &mine)
        .expect("#2138 the child audit can read its own /proc stat");
    let children = child_processes_in(std::path::Path::new("/proc"))
        .expect("#2138 the child audit reads /proc on this host");
    assert!(
        !children.iter().any(|(pid, _)| pid.to_string() == mine),
        "#2138 this process is nobody's child"
    );
}

/// #2138 negative control: a host that hides this process's own stat must fail
/// the child audit instead of returning an empty (clean) list. The injected
/// proc root lists this process but carries no `stat` for it — the shape of a
/// hidden `/proc`.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_self_stat_fails_the_child_audit() {
    let scratch = crate::test_scratch::Scratch::new("2138-blind-child");
    let mine = std::process::id().to_string();
    std::fs::create_dir(scratch.path().join(&mine)).expect("#2138 create the blind proc entry");
    let error = child_processes_in(scratch.path())
        .expect_err("#2138 a sweep that cannot read its own stat must fail");
    let text = error.to_string();
    assert!(
        text.contains("#2138") && text.contains("prove nothing"),
        "#2138 the failure must name the blind self-read: {text}"
    );
}

/// #2138 negative control: a live child whose `stat` the sweep cannot read must
/// fail the child audit. The injected proc root carries a valid stat for this
/// process — so the self-check passes — and a mode-000 `stat` for a real,
/// living child, which the source classification (reading the real `/proc`)
/// rightly calls related. Skipping it is exactly the fail-open #2138 removes.
#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_stat_of_a_possible_child_fails_the_child_audit() {
    use std::os::unix::fs::PermissionsExt;
    let mine = std::process::id().to_string();
    let scratch = crate::test_scratch::Scratch::new("2138-blind-child-source");
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("#2138 spawn the blind-source child");
    let child_pid = child.id().to_string();
    let self_entry = scratch.path().join(&mine);
    std::fs::create_dir(&self_entry).expect("#2138 create the self proc entry");
    std::fs::write(self_entry.join("stat"), format!("{mine} (self) S 1 0 0 0"))
        .expect("#2138 write the self stat the self-check parses");
    let child_entry = scratch.path().join(&child_pid);
    std::fs::create_dir(&child_entry).expect("#2138 create the child proc entry");
    std::fs::write(
        child_entry.join("stat"),
        format!("{child_pid} (child) S {mine} 0 0"),
    )
    .expect("#2138 write the child stat");
    std::fs::set_permissions(
        child_entry.join("stat"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("#2138 make the child stat unreadable");
    // #1461 r6 (DS r5 concurrency P2): probe the behaviour, not the uid —
    // the native read probe alone already covers every permission-blind
    // runner (root, CAP_DAC_OVERRIDE, permission-blind mounts), and an
    // euid shortcut would false-fail a mode-enforced root host (NFS
    // root_squash) under PDRS_REQUIRE_PERMISSION_FAULTS=1. Probe-only
    // shape, as tests/write_tools.rs.
    if std::fs::read_to_string(child_entry.join("stat")).is_ok() {
        let _ = child.kill();
        let _ = child.wait();
        crate::test_permission_faults::skip_or_require(
            "#2138 skip: an_unreadable_stat_of_a_possible_child_fails_the_child_audit: \
             running where mode 000 is readable (root or CAP_DAC_OVERRIDE)\n",
            "#2138 PDRS_REQUIRE_PERMISSION_FAULTS is set but mode 000 did not stop the stat \
             read (root or a permission-blind runner); this test needs a host that enforces mode \
             bits (需要强制 mode 位的宿主)",
        );
        return;
    }
    let outcome = child_processes_in(scratch.path());
    let _ = child.kill();
    let _ = child.wait();
    let error = outcome.expect_err("#2138 an unreadable child stat must fail the audit");
    let text = error.to_string();
    assert!(
        text.contains("#2138") && text.contains(&child_pid) && text.contains("reading the stat"),
        "#2138 the failure must name the unreadable child: {text}"
    );
}

/// #2138: the data-dir audit's own eyes — on a Linux host it must resolve a
/// descriptor this process holds, and a scratch directory nobody holds comes
/// back as no holders (a readable, sound clean result).
#[cfg(target_os = "linux")]
#[test]
fn this_process_descriptors_are_visible_to_the_holder_audit() {
    let mine = std::process::id().to_string();
    this_process_descriptors_are_visible(std::path::Path::new("/proc"), &mine)
        .expect("#2138 the data-dir audit can resolve its own descriptor");
    let scratch = crate::test_scratch::Scratch::new("2138-holder-audit");
    let holders = foreign_data_dir_holders_in(std::path::Path::new("/proc"), scratch.path())
        .expect("#2138 the data-dir audit reads /proc on this host");
    assert!(
        !holders.contains(&(std::process::id() as i32)),
        "#2138 this process is not an external holder of its own scratch ({holders:?})"
    );
}

/// #2138 negative control: a host that hides this process's own descriptors
/// must fail the data-dir audit instead of reporting that no one holds the
/// data dir. The injected proc root lists this process but carries no `fd`
/// directory for it — the shape of a hidden `/proc`.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_self_descriptor_dir_fails_the_holder_audit() {
    let scratch = crate::test_scratch::Scratch::new("2138-blind-holders");
    let mine = std::process::id().to_string();
    std::fs::create_dir(scratch.path().join(&mine)).expect("#2138 create the blind proc entry");
    let error = foreign_data_dir_holders_in(scratch.path(), scratch.path())
        .expect_err("#2138 a sweep that cannot read its own descriptors must fail");
    let text = error.to_string();
    assert!(
        text.contains("#2138") && text.contains("prove nothing"),
        "#2138 the failure must name the blind self-read: {text}"
    );
}

/// #2138: the holder audit's skip set is not "another user" but "a source the
/// kernel hides entirely". "Not a descendant" is what the holder audit is
/// *looking for*, so it never licenses a skip on its own: a source of this
/// user we are allowed to inspect stays an unknown even when it is unrelated
/// and its descriptor directory is unreadable.
#[cfg(target_os = "linux")]
#[test]
fn the_holder_audit_skips_only_sources_the_kernel_hides_entirely() {
    let mine = std::process::id().to_string();
    let mut sources = BlindSources::default();
    assert_eq!(
        sources.of_holder(&mine),
        BlindSource::Related,
        "#2138 this process can hold its own data dir"
    );
    // The parent is alive, of this user, never a descendant of this binary, and
    // the kernel lets us inspect it — exactly the shape that must not be
    // mistaken for somebody else.
    let parent = std::os::unix::process::parent_id().to_string();
    assert!(
        !descriptors_are_policy_hidden(&parent),
        "#2138 the test runner must be inspectable for this case to mean anything"
    );
    sources.seen.insert(parent.clone(), BlindSource::Unrelated);
    assert_eq!(
        sources.of_holder(&parent),
        BlindSource::Related,
        "#2138 an inspectable source of this user is never skippable"
    );
    // A pid that no longer exists is handled by its classification — gone, so
    // nothing can be held — and never reaches the ownership question at all.
    assert_eq!(
        blind_source("4000000"),
        BlindSource::Vanished,
        "#2138 a missing pid is classified, not skipped on ownership"
    );
    // A missing `exe` is not a licence to skip: with no `/proc` entry we cannot
    // prove the source belongs to someone else, so it stays an unknown. This is
    // the zombie-leader shape — no `exe`, live threads still sharing the fd
    // table — that must fail closed.
    assert!(
        !descriptors_are_policy_hidden("4000000"),
        "#2138 an unreadable ownership is not proof that the kernel hides it"
    );
    // Only a source the kernel denies wholesale may be passed over.
    if std::path::Path::new("/proc/1").exists() && descriptors_are_policy_hidden("1") {
        sources.seen.insert("1".to_owned(), BlindSource::Unrelated);
        assert_eq!(
            sources.of_holder("1"),
            BlindSource::Unrelated,
            "#2138 a source denied as a whole is skippable"
        );
    }
}

/// #2138 negative control: an unreadable descriptor directory of a *same-user*
/// process must fail the holder audit. The injected proc root gives the parent
/// process — alive, this user, never a descendant of this binary, and
/// inspectable by the kernel — an `fd` entry that cannot be listed, and adds
/// one readable source so the sweep does see something. Skipping the
/// unreadable same-user source (what a "not a descendant" or an ownership-based
/// rule would do) would hand back a clean holder list.
#[cfg(target_os = "linux")]
#[test]
fn an_unreadable_holder_of_this_user_fails_the_data_dir_audit() {
    use std::os::unix::fs::PermissionsExt;
    let mine = std::process::id().to_string();
    let parent = std::os::unix::process::parent_id().to_string();
    // The fixture can only prove the audit's behaviour if the real parent is
    // the shape the audit has to judge: alive, classified unrelated, and
    // inspectable. A degraded host (reparented to init, a session the kernel
    // hides) would fail here for a reason that has nothing to do with the
    // code under test, so say so rather than report a false regression.
    assert_eq!(
        blind_source(&parent),
        BlindSource::Unrelated,
        "#2138 this case needs a live, unrelated parent process"
    );
    assert!(
        !descriptors_are_policy_hidden(&parent),
        "#2138 this case needs a parent the kernel lets us inspect"
    );
    let scratch = crate::test_scratch::Scratch::new("2138-same-user-holder");
    let proc_root = scratch.path().join("proc");
    // The audit's own eyes must pass through the injected root: this process
    // needs an fd directory holding a descriptor that resolves to a pipe. The
    // self-check creates its own pipe, so the fixture cannot know which number
    // it will get — every plausible number resolves to the same pipe target.
    // The range covers what a test binary with many open descriptors can reach.
    let self_fd = proc_root.join(&mine).join("fd");
    std::fs::create_dir_all(&self_fd).expect("#2138 create the self fd fixture");
    for fd in 3..=1024 {
        std::os::unix::fs::symlink("pipe:[1]", self_fd.join(fd.to_string()))
            .expect("#2138 link the self pipe fixture");
    }
    // The source that must fail the audit: alive, this user, not a descendant,
    // descriptor directory unlistable. A mode-000 directory is how the kernel
    // actually refuses the listing (EACCES); a non-directory would fail with
    // ENOTDIR, which no healthy host produces.
    let blind = proc_root.join(&parent);
    std::fs::create_dir(&blind).expect("#2138 create the blind holder");
    std::fs::create_dir(blind.join("fd")).expect("#2138 create the unlistable fd entry");
    std::fs::set_permissions(blind.join("fd"), std::fs::Permissions::from_mode(0o000))
        .expect("#2138 make the blind holder's fd unlistable");
    // #1461 r6 (DS r5 concurrency P2): probe the behaviour, not the uid —
    // the native listing probe alone covers every permission-blind runner,
    // and an euid shortcut would false-fail a mode-enforced root host under
    // PDRS_REQUIRE_PERMISSION_FAULTS=1 (probe-only shape, as
    // tests/write_tools.rs).
    if std::fs::read_dir(blind.join("fd")).is_ok() {
        crate::test_permission_faults::skip_or_require(
            "#2138 skip: an_unreadable_holder_of_this_user_fails_the_data_dir_audit: \
             running where mode 000 is readable (root or CAP_DAC_OVERRIDE)\n",
            "#2138 PDRS_REQUIRE_PERMISSION_FAULTS is set but mode 000 did not stop the listing \
             (root or a permission-blind runner); this test needs a host that enforces mode bits \
             (需要强制 mode 位的宿主)",
        );
        return;
    }
    // One readable source, so the sweep has something it could see and the
    // failure can only come from the blind same-user holder.
    let readable = proc_root.join("1000000").join("fd");
    std::fs::create_dir_all(&readable).expect("#2138 create the readable source");
    std::os::unix::fs::symlink("/dev/null", readable.join("0"))
        .expect("#2138 link the readable source's descriptor");
    let error = foreign_data_dir_holders_in(&proc_root, scratch.path())
        .expect_err("#2138 an unreadable same-user source must fail the audit");
    let text = error.to_string();
    assert!(
        text.contains(&parent) && text.contains("reading the descriptors"),
        "#2138 the failure must name the unreadable same-user holder: {text}"
    );
}
