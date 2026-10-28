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
    // These empty-directory replays have no locale-sensitive names. A missing
    // guard must fail the contract even when all their data still match.
    for case in fixture["localeProbe"].as_array().expect(MSG) {
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
            "#962 locale contract: {suite}/{id} startup={STARTUP_LIMIT:?} idle={IDLE_LIMIT:?}"
        );
        let child = spawn_child(&mut child)
            .unwrap_or_else(|e| panic!("#962 locale child spawn: {suite}/{id}: {e}"));
        let output = child_output(child, STARTUP_LIMIT, IDLE_LIMIT)
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
