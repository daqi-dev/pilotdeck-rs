//! Shared entry guard for en-US snapshots. Each crate root supplies the same
//! production parser, without exposing a public test API.
use crate::node_locale;
use std::{
    future::Future, io, process::ExitStatus, process::Output, process::Stdio, time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const IDLE_LIMIT: Duration = Duration::from_secs(30);
// A spawned libtest process can spend longer than IDLE_LIMIT loading before
// its first byte. Bound that phase separately; a live PID is not progress and
// must never grant unbounded extensions. This is a wall-clock startup budget.
const STARTUP_LIMIT: Duration = Duration::from_secs(180);

/// #978: the no-output budget after the first byte. A loaded host can starve
/// this child for longer than 30 s even though it is runnable, so the old
/// fixed budget reported a deadlock. Scale the budget by the one-minute load
/// per CPU, capped by the startup budget; invalid load data keeps the base.
fn idle_limit_for(load: f64, cpus: usize) -> Duration {
    if pilotdeck_gateway::mutants::on("locale-idle-nonadaptive") {
        return IDLE_LIMIT;
    }
    if pilotdeck_gateway::mutants::on("locale-idle-unbounded") {
        return Duration::from_secs(24 * 60 * 60);
    }
    if cpus == 0 || !load.is_finite() || load <= 0.0 {
        return IDLE_LIMIT;
    }
    let pressure = load / cpus as f64;
    Duration::try_from_secs_f64(IDLE_LIMIT.as_secs_f64() * (1.0 + pressure))
        .map(|limit| limit.clamp(IDLE_LIMIT, STARTUP_LIMIT))
        .unwrap_or(STARTUP_LIMIT)
}

/// #978: one bounded load-sample execution. The sample runs before the locale
/// child and its 180 s supervisor exist, so a sampler that never exits must be
/// stopped here rather than holding the spawn: the wait is capped by `limit`,
/// and an expired child is killed and reaped before the caller falls back to
/// the base budget.
#[cfg(unix)]
enum Sampled {
    Finished(Output),
    TimedOut { pid: u32 },
    Failed,
}

/// #978: wait bound for one macOS `sysctl` load sample. Two candidate paths are
/// tried, so the sampling phase costs at most two of these bounds.
#[cfg(target_os = "macos")]
const SAMPLE_LIMIT: Duration = Duration::from_secs(2);

#[cfg(unix)]
fn sample_bounded(mut command: std::process::Command, limit: Duration) -> Sampled {
    // Negative control only: a mutant that removes the wait bound must be
    // observable, so it widens the deadline instead of rewriting the source.
    let limit = if pilotdeck_gateway::mutants::on("locale-sampler-unbounded") {
        Duration::from_secs(24 * 60 * 60)
    } else {
        limit
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return Sampled::Failed,
    };
    let pid = child.id();
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                // Bounded wait expired: kill and reap here, so no sampler
                // survives the fallback decision and no PID is left to Drop.
                let _ = child.kill();
                let _ = child.wait();
                return Sampled::TimedOut { pid };
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Sampled::Failed;
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) => Sampled::Finished(output),
        Err(_) => Sampled::Failed,
    }
}

/// #978: the single fallback rule for every unusable sample: no load keeps the
/// base budget (`idle_limit_for` treats 0.0 as the non-adaptive base).
fn sampled_budget(load: Option<f64>, cpus: usize) -> Duration {
    idle_limit_for(load.unwrap_or(0.0), cpus)
}

/// One-minute load average, read without `unsafe` (the workspace forbids it).
/// Any failure is reported as `None`, which falls back to the base budget.
#[cfg(target_os = "linux")]
fn load_average() -> Option<f64> {
    let raw = std::fs::read_to_string("/proc/loadavg").ok()?;
    raw.split_whitespace().next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn parse_load(output: &Output) -> Option<f64> {
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    text.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn load_average() -> Option<f64> {
    for program in ["/usr/sbin/sysctl", "sysctl"] {
        let mut command = std::process::Command::new(program);
        command.args(["-n", "vm.loadavg"]);
        // A stuck sampler times out, is reaped, and the next path is tried.
        if let Sampled::Finished(output) = sample_bounded(command, SAMPLE_LIMIT)
            && let Some(load) = parse_load(&output)
        {
            return Some(load);
        }
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn load_average() -> Option<f64> {
    None
}

/// #978: compute the adaptive idle budget before every locale child spawn and
/// log the inputs, so a host failure shows whether the load was observed.
fn idle_limit() -> Duration {
    let cpus = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(0);
    let load = load_average();
    let limit = sampled_budget(load, cpus);
    eprintln!("#978 locale child: adaptive idle={limit:?} load={load:?} cpus={cpus}");
    limit
}

struct ChildTrace {
    started: tokio::time::Instant,
    pid: Option<u32>,
    first_output: bool,
}

impl ChildTrace {
    fn new(pid: Option<u32>) -> Self {
        Self {
            started: tokio::time::Instant::now(),
            pid,
            first_output: false,
        }
    }

    fn event(&self, event: &str) {
        eprintln!(
            "#962 locale child: pid={:?} elapsed_ms={} unix_ms={} event={event}",
            self.pid,
            self.started.elapsed().as_millis(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        );
    }

    fn output(&mut self, stream: &str) {
        if !self.first_output {
            self.first_output = true;
            self.event(stream);
        }
    }
}

// A libtest child can make progress for longer than IDLE_LIMIT on a busy host.
// Read bytes (including partial lines) from both pipes; EOF is not progress.
// Poll ready output and exit before the timer when the parent was descheduled.
async fn collect_output(
    mut stdout: impl AsyncRead + Unpin,
    mut stderr: impl AsyncRead + Unpin,
    wait: impl Future<Output = io::Result<ExitStatus>>,
    startup_limit: Duration,
    idle_limit: Duration,
    mut trace: ChildTrace,
) -> io::Result<Output> {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_open = true;
    let mut err_open = true;
    let mut status = None;
    let mut out_chunk = [0; 4096];
    let mut err_chunk = [0; 4096];
    tokio::pin!(wait);
    let deadline = tokio::time::sleep(startup_limit);
    tokio::pin!(deadline);
    let result = async {
        while status.is_none() || out_open || err_open {
            tokio::select! {
                biased;
                read = stdout.read(&mut out_chunk), if out_open => {
                    let count = read?;
                    out_open = count != 0;
                    if count != 0 {
                        trace.output("first-stdout");
                        out.extend_from_slice(&out_chunk[..count]);
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);
                    }
                }
                read = stderr.read(&mut err_chunk), if err_open => {
                    let count = read?;
                    err_open = count != 0;
                    if count != 0 {
                        trace.output("first-stderr");
                        err.extend_from_slice(&err_chunk[..count]);
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);
                    }
                }
                exited = &mut wait, if status.is_none() => {
                    status = Some(exited?);
                    trace.event("exit");
                    // An exited child is no longer starting, even if a
                    // descendant still holds its pipes without writing.
                    if !trace.first_output {
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_limit);
                    }
                }
                () = &mut deadline => {
                    let phase = if trace.first_output || status.is_some() {
                        "output/drain"
                    } else {
                        "startup"
                    };
                    trace.event(phase);
                    return Err(io::Error::new(io::ErrorKind::TimedOut,
                        format!("#962 locale child: {phase} deadline; pid={:?} elapsed={:?} startup={startup_limit:?} idle={idle_limit:?}",
                            trace.pid, trace.started.elapsed())));
                }
            }
        }
        Ok(status.expect("#962 locale child: exit status collected"))
    }
    .await;
    match result {
        Ok(status) => Ok(Output {
            status,
            stdout: out,
            stderr: err,
        }),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!(
                "{error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            ),
        )),
    }
}

async fn child_output(
    mut child: tokio::process::Child,
    startup_limit: Duration,
    idle_limit: Duration,
) -> io::Result<Output> {
    let trace = ChildTrace::new(child.id());
    trace.event("spawn-returned");
    let stdout = child
        .stdout
        .take()
        .expect("#962 locale child: piped stdout");
    let stderr = child
        .stderr
        .take()
        .expect("#962 locale child: piped stderr");
    let result = collect_output(
        stdout,
        stderr,
        child.wait(),
        startup_limit,
        idle_limit,
        trace,
    )
    .await;
    if result.is_err() {
        // kill() also reaps. kill_on_drop remains the fallback on cancellation.
        child.kill().await.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("#962 locale child cleanup: {error}; {result:?}"),
            )
        })?;
    }
    result
}

fn spawn_child(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    let started = std::time::Instant::now();
    eprintln!(
        "#962 locale child: event=spawn-start unix_ms={}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let result = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    eprintln!(
        "#962 locale child: event=spawn-finished elapsed_ms={} pid={:?}",
        started.elapsed().as_millis(),
        result.as_ref().ok().and_then(|child| child.id())
    );
    result
}

pub(crate) const MESSAGE: &str =
    "#887 oracle requires en-US; set LANG=en_US.UTF-8 and unset LC_ALL/LC_MESSAGES";

pub(crate) fn require_en_us() {
    assert_eq!(
        node_locale::node_locale(|key| std::env::var(key).ok()).to_string(),
        "en-US",
        "{MESSAGE}"
    );
}

pub(crate) async fn replay_locale_contract(child_test: &str, suite: &str) {
    const MSG: &str = "#887 replay locale contract";
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/node-locale/oracle.json")).expect(MSG);
    // #978: recompute the adaptive budget for every child spawn, so a load
    // spike during a long contract is observed. The deadline still ends on a
    // finite no-output interval; load only scales that interval.
    // These empty-directory replays have no locale-sensitive names. A missing
    // guard must fail the contract even when all their data still match.
    for case in fixture["localeProbe"].as_array().expect(MSG) {
        let idle_budget = idle_limit();
        let id = case["id"].as_str().expect(MSG);
        let success = case["locale"] == "en-US";
        let mut child = tokio::process::Command::new(std::env::current_exe().expect(MSG));
        child
            .args([child_test, "--exact", "--nocapture", "--format", "pretty"])
            .stdin(Stdio::null())
            .env_remove("LANG")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES");
        for (key, value) in case["env"].as_object().expect(MSG) {
            child.env(key, value.as_str().expect(MSG));
        }
        eprintln!(
            "#962 locale contract: {suite}/{id} startup={STARTUP_LIMIT:?} idle={idle_budget:?}"
        );
        let child = spawn_child(&mut child)
            .unwrap_or_else(|e| panic!("#962 locale child spawn: {suite}/{id}: {e}"));
        let output = child_output(child, STARTUP_LIMIT, idle_budget)
            .await
            .unwrap_or_else(|e| panic!("#962 replay locale contract: {suite}/{id}: {e}"));
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(log.contains("running 1 test"), "{MSG}: {suite}/{id}: {log}");
        if success {
            assert!(output.status.success(), "{MSG}: {suite}/{id}: {log}");
            assert!(
                log.contains("1 passed; 0 failed") && log.contains(&format!("{child_test} ... ok")),
                "{MSG}: {suite}/{id}: {log}"
            );
        } else {
            if output.status.success() {
                assert!(
                    log.contains("1 passed; 0 failed")
                        && log.contains(&format!("{child_test} ... ok")),
                    "{MSG}: malformed child success: {suite}/{id}: {log}"
                );
                panic!("#887 {suite} locale guard bypassed: {id}");
            }
            assert_eq!(
                output.status.code(),
                Some(101),
                "{MSG}: {suite}/{id}: {log}"
            );
            assert!(
                log.contains(MESSAGE) && log.contains(&format!("{child_test} ... FAILED")),
                "{MSG}: {suite}/{id}: {log}"
            );
        }
        println!(
            "#887 {suite}/{id}: locale={} success={success}",
            case["locale"]
        );
    }
}

#[cfg(unix)]
#[path = "oracle_locale_tests.rs"]
mod tests;
