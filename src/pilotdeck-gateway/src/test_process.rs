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
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(crate) fn child_processes() -> Vec<(i32, Option<std::path::PathBuf>)> {
    let me = std::process::id() as i32;
    let mut children = Vec::new();
    let entries = std::fs::read_dir("/proc").expect("#1316 the audit reads /proc");
    for entry in entries {
        let entry = entry.expect("#1316 the audit reads a /proc entry");
        let Some(pid) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        // After `comm)` come state and ppid.
        let Some(ppid) = rest
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<i32>().ok())
        else {
            continue;
        };
        if ppid == me {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            children.push((pid, exe));
        }
    }
    children.sort();
    children
}

#[cfg(target_os = "linux")]
fn foreign_data_dir_holders(root: &std::path::Path) -> Vec<i32> {
    let me = std::process::id() as i32;
    let mut holders = Vec::new();
    let entries = std::fs::read_dir("/proc").expect("#1316 the audit reads /proc");
    for entry in entries {
        let entry = entry.expect("#1316 the audit reads a /proc entry");
        let Some(pid) = entry.file_name().to_string_lossy().parse::<i32>().ok() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds {
            let Ok(fd) = fd else {
                continue;
            };
            if let Ok(target) = std::fs::read_link(fd.path())
                && target.starts_with(root)
            {
                holders.push(pid);
                break;
            }
        }
    }
    holders.sort();
    holders
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
                for (pid, exe) in child_processes() {
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
                for pid in foreign_data_dir_holders(&root) {
                    holders.insert(pid);
                }
                if !announced {
                    announced = true;
                    // The first sweep is the window's opening snapshot. A
                    // closed receiver only means `start` was abandoned; keep
                    // sampling, `Drop` still stops this thread.
                    let _ = swept.send(children.clone());
                }
                if done {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            (children, holders)
        });
        let children_before = first_sweep
            .recv()
            .expect("#1316 the audit sampler completes its first sweep");
        (
            Self {
                stop,
                worker: Some(worker),
            },
            children_before,
        )
    }

    pub(crate) fn finish(mut self) -> TransientObservations {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.worker
            .take()
            .expect("#1316 the audit sampler is running")
            .join()
            .expect("#1316 the audit sampler joins")
    }
}

/// A panic before `finish` must not leave a spinning sampler in the test
/// process; stopping it here also keeps the owned rerun process bounded.
#[cfg(target_os = "linux")]
impl Drop for TransientAudit {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
