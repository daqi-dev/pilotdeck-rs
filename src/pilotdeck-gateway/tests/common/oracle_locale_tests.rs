//! Deterministic clock/pipe probes for the Rust-only subprocess supervisor.
use super::*;
use std::{
    future,
    os::unix::process::ExitStatusExt,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::AsyncWriteExt;

fn poll_once<T>(future: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

async fn progress(stderr_progress: bool) {
    let (mut out_writer, stdout) = tokio::io::duplex(16);
    let (mut err_writer, stderr) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        async { exit.await.expect("#962 progress: exit sender") },
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 progress: starts pending"
    );
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(29)).await;
        if stderr_progress {
            err_writer
                .write_all(b".")
                .await
                .expect("#962 progress: stderr write");
        } else {
            out_writer
                .write_all(b".")
                .await
                .expect("#962 progress: stdout write");
        }
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#962 progress: bytes renew idle deadline"
        );
    }
    drop(out_writer);
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#962 progress: send exit");
    let output = collect
        .await
        .expect("#962 progress: completes after 87 seconds");
    assert!(output.status.success(), "#962 progress: preserves exit");
    assert_eq!(
        output.stdout,
        if stderr_progress {
            &b""[..]
        } else {
            &b"..."[..]
        },
        "#962 progress: stdout retained"
    );
    assert_eq!(
        output.stderr,
        if stderr_progress {
            &b"..."[..]
        } else {
            &b""[..]
        },
        "#962 progress: stderr retained"
    );
}

#[tokio::test(start_paused = true)]
async fn stdout_bytes_renew_idle_deadline() {
    progress(false).await;
}

#[tokio::test(start_paused = true)]
async fn stderr_bytes_renew_idle_deadline() {
    progress(true).await;
}

#[tokio::test(start_paused = true)]
async fn silent_child_expires_at_idle_deadline() {
    let (_out_writer, stdout) = tokio::io::duplex(16);
    let (_err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 silent: starts pending"
    );
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 silent: not before deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#962 silent: must expire at idle deadline");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 silent: timeout kind"
    );
}

#[test]
fn adaptive_idle_scales_with_host_load() {
    assert_eq!(
        idle_limit_for(32.0, 8),
        Duration::from_secs(150),
        "#978 adaptive idle must scale with host load"
    );
    for (load, cpus) in [(0.0, 8), (f64::NAN, 8), (f64::INFINITY, 8), (4.0, 0)] {
        assert_eq!(
            idle_limit_for(load, cpus),
            IDLE_LIMIT,
            "#978 adaptive idle must fall back to the base budget for load={load:?} cpus={cpus}"
        );
    }
}

#[test]
fn adaptive_idle_caps_at_startup_budget() {
    for load in [1000.0, f64::MAX] {
        assert_eq!(
            idle_limit_for(load, 1),
            STARTUP_LIMIT,
            "#978 adaptive idle must stay within the startup cap for load={load:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn silent_child_expires_at_adaptive_idle_deadline() {
    // #978: the adaptive budget is still a finite no-output deadline. A load of
    // 32 on 8 CPUs scales 30 s to 150 s; after the first byte, a child that
    // stops writing must be killed at the scaled deadline, not wait for a PID
    // or an unbounded extension.
    let idle = idle_limit_for(32.0, 8);
    let (mut out_writer, stdout) = tokio::io::duplex(16);
    let (_err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        STARTUP_LIMIT,
        idle,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#978 adaptive idle: starts pending"
    );
    out_writer
        .write_all(b".")
        .await
        .expect("#978 adaptive idle: first byte");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#978 adaptive idle: first byte switches to the idle phase"
    );
    // Keep the clock advance fixed at the non-mutant 150 s budget: a mutant
    // that makes the budget unbounded must not also move the assertion.
    tokio::time::advance(Duration::from_secs(149)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#978 adaptive idle must not expire before the scaled deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#978 adaptive idle must still expire for a silent child");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#978 adaptive idle must still report a timeout"
    );
}

#[tokio::test(start_paused = true)]
async fn eof_does_not_renew_deadline_or_complete_a_live_child() {
    let (out_writer, stdout) = tokio::io::duplex(16);
    let (err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 EOF: starts pending"
    );
    tokio::time::advance(Duration::from_secs(29)).await;
    drop(out_writer);
    drop(err_writer);
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 EOF: still waits for exit"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(poll_once(collect.as_mut()), Poll::Ready(Err(ref e)) if e.kind() == io::ErrorKind::TimedOut),
        "#962 EOF: EOF must not renew deadline"
    );
}

// advance() moves the clock but need not flush the timer driver. Register a
// later fence before advancing, then await it without polling the collector.
// Its completion proves the driver has processed the collector's old deadline.
async fn expire_registered_deadline() {
    let mut fence = Box::pin(tokio::time::sleep(IDLE_LIMIT + Duration::from_secs(1)));
    assert!(
        poll_once(fence.as_mut()).is_pending(),
        "#962 ready fence: registered before advancing"
    );
    tokio::time::advance(IDLE_LIMIT + Duration::from_secs(1)).await;
    fence.await;
}

#[tokio::test(start_paused = true)]
async fn ready_output_and_exit_win_over_overdue_timer() {
    let (mut out_writer, stdout) = tokio::io::duplex(16);
    let (mut err_writer, stderr) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        async { exit.await.expect("#962 ready: exit sender") },
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 ready: starts pending"
    );
    // Pipes and the exit oneshot model a child held behind explicit gates.
    // First readiness is delayed beyond half the idle limit, then progress
    // continues beyond the original total limit without wall-clock sleeps.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#962 ready: held gate remains within idle limit"
        );
        out_writer
            .write_all(b".")
            .await
            .expect("#962 ready: release stdout gate");
        err_writer
            .write_all(b"!")
            .await
            .expect("#962 ready: release stderr gate");
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#962 ready: gated progress renews deadline"
        );
    }
    // Deliberately stop polling the parent. Both the old timer and buffered
    // output must actually be ready, not merely past the timer's deadline.
    expire_registered_deadline().await;
    // EOF does not renew the timer. It finishes the already-pending select
    // and forces the next iteration to evaluate its guards at the late time.
    // Keeping both streams readable here would renew before either mutant's
    // loop-top check / select guard could observe the elapsed deadline.
    drop(out_writer);
    err_writer
        .write_all(b"err")
        .await
        .expect("#962 ready: stderr write");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 ready: queued output precedes deadline"
    );
    // Keep stderr open until after the next expiry. Its EOF forces another
    // guard evaluation without renewing the timer before the ready exit.
    expire_registered_deadline().await;
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(101 << 8)))
        .expect("#962 ready: send exit");
    let output = collect
        .await
        .expect("#962 ready: queued exit precedes deadline");
    assert_eq!(
        output.status.code(),
        Some(101),
        "#962 ready: failure status retained"
    );
    assert_eq!(output.stdout, b"...", "#962 ready: drain stdout");
    assert_eq!(output.stderr, b"!!!err", "#962 ready: drain stderr");
}

#[tokio::test(start_paused = true)]
async fn ready_exit_without_output_wins_over_overdue_timer() {
    let (out_writer, stdout) = tokio::io::duplex(16);
    let (err_writer, stderr) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        async { exit.await.expect("#962 exit: exit sender") },
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 exit: starts pending"
    );
    expire_registered_deadline().await;
    // Release EOF only after expiry so the next select evaluates its wait
    // guard at the overdue time, even if the previous select was suspended.
    drop(out_writer);
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#962 exit: send exit");
    assert!(
        collect
            .await
            .expect("#962 exit: queued exit precedes deadline")
            .status
            .success(),
        "#962 exit: success preserved"
    );
}

#[tokio::test(start_paused = true)]
async fn exited_child_still_drains_both_pipes_with_idle_bound() {
    let (mut out_writer, stdout) = tokio::io::duplex(16);
    let (mut err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::ready(Ok(ExitStatus::from_raw(0))),
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 drain: exit alone is not complete"
    );
    out_writer
        .write_all(b"tail-out")
        .await
        .expect("#962 drain: stdout write");
    err_writer
        .write_all(b"tail-err")
        .await
        .expect("#962 drain: stderr write");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 drain: wait for EOF"
    );
    // An inherited writer that never closes cannot hang the contract forever.
    tokio::time::advance(IDLE_LIMIT).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#962 drain: inherited pipes must remain bounded");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 drain: timeout kind"
    );
    assert!(
        error.to_string().contains("tail-out") && error.to_string().contains("tail-err"),
        "#962 drain: partial logs retained"
    );
}

#[tokio::test(start_paused = true)]
async fn read_and_wait_errors_are_not_success() {
    struct Broken;
    impl AsyncRead for Broken {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("#962 injected read error")))
        }
    }
    let error = collect_output(
        Broken,
        tokio::io::empty(),
        future::pending(),
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    )
    .await
    .expect_err("#962 error: read must fail");
    assert!(
        error.to_string().contains("#962 injected read error"),
        "#962 error: read reason retained"
    );
    let error = collect_output(
        tokio::io::empty(),
        tokio::io::empty(),
        future::ready(Err(io::Error::other("#962 injected wait error"))),
        IDLE_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    )
    .await
    .expect_err("#962 error: wait must fail");
    assert!(
        error.to_string().contains("#962 injected wait error"),
        "#962 error: wait reason retained"
    );
}

// Real children use real time. cat blocks on held-open stdin without consuming
// CPU. The test owns that stdin until after the assertion, so EOF cannot make a
// disabled kill look successful; unwinding still releases a surviving mutant.
fn blocked_child(
    kill_on_drop: bool,
) -> (
    tokio::process::Child,
    tokio::process::ChildStdin,
    rustix::process::Pid,
) {
    let mut command = tokio::process::Command::new("cat");
    command.stdin(Stdio::piped());
    let mut child = if kill_on_drop {
        spawn_child(&mut command)
    } else {
        // Isolate explicit timeout cleanup from Drop's fallback; otherwise
        // Tokio can synchronously reap on Drop and hide a missing kill().await.
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()
    }
    .expect("#962 cleanup: spawn cat");
    let pid = rustix::process::Pid::from_raw(child.id().expect("#962 cleanup: child id") as i32)
        .expect("#962 cleanup: valid pid");
    let stdin = child.stdin.take().expect("#962 cleanup: piped stdin");
    (child, stdin, pid)
}

async fn assert_gone(pid: rustix::process::Pid) {
    tokio::time::timeout(IDLE_LIMIT, async {
        loop {
            match rustix::process::test_kill_process(pid) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("#962 cleanup: child liveness probe: {error}"),
            }
        }
    })
    .await
    .expect("#962 cleanup: child must be killed and reaped");
}

#[tokio::test]
async fn idle_timeout_kills_and_reaps_child() {
    let (child, _stdin, pid) = blocked_child(false);
    let error = child_output(child, Duration::ZERO, Duration::ZERO)
        .await
        .expect_err("#962 cleanup: silent child must time out");
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 cleanup: timeout reported"
    );
    assert!(
        matches!(
            rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
            Err(rustix::io::Errno::CHILD)
        ),
        "#962 cleanup: timeout reaps before returning"
    );
    assert_gone(pid).await;
}

#[tokio::test]
async fn cancelling_supervision_kills_child() {
    let (child, _stdin, pid) = blocked_child(true);
    let mut output = Box::pin(child_output(child, STARTUP_LIMIT, IDLE_LIMIT));
    assert!(
        poll_once(output.as_mut()).is_pending(),
        "#962 cleanup: live child initially pending"
    );
    drop(output);
    assert_gone(pid).await;
}

async fn startup_then_idle(stderr_first: bool) {
    let (mut out_writer, stdout) = tokio::io::duplex(16);
    let (mut err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        STARTUP_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(Some(962)),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup: initially pending"
    );
    tokio::time::advance(Duration::from_secs(40)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup: permits first output after 30 seconds"
    );
    if stderr_first {
        err_writer
            .write_all(b"!")
            .await
            .expect("#962 startup: write stderr gate");
    } else {
        out_writer
            .write_all(b".")
            .await
            .expect("#962 startup: write stdout gate");
    }
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup: first byte accepted"
    );
    tokio::time::advance(IDLE_LIMIT - Duration::from_secs(1)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup: first byte renews full idle budget"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#962 startup: first byte switches to short idle deadline");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 startup: idle timeout kind"
    );
    assert!(
        error.to_string().contains("output/drain deadline")
            && error.to_string().contains("pid=Some(962)"),
        "#962 startup: phase and PID diagnostics retained"
    );
    assert!(
        error
            .to_string()
            .contains(if stderr_first { "!" } else { "." }),
        "#962 startup: partial byte retained"
    );
}

#[tokio::test(start_paused = true)]
async fn startup_stdout_switches_to_idle() {
    startup_then_idle(false).await;
}

#[tokio::test(start_paused = true)]
async fn startup_stderr_switches_to_idle() {
    startup_then_idle(true).await;
}

#[tokio::test(start_paused = true)]
async fn silent_startup_is_bounded() {
    let (_out_writer, stdout) = tokio::io::duplex(16);
    let (_err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        STARTUP_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 silent startup: initially pending"
    );
    tokio::time::advance(STARTUP_LIMIT - Duration::from_secs(1)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 silent startup: full startup budget available"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#962 silent startup: must expire at startup deadline");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 silent startup: timeout kind"
    );
    assert!(
        error.to_string().contains("startup deadline"),
        "#962 silent startup: startup phase diagnostic"
    );
}

#[tokio::test(start_paused = true)]
async fn startup_eof_does_not_extend_budget() {
    let (out_writer, stdout) = tokio::io::duplex(16);
    let (err_writer, stderr) = tokio::io::duplex(16);
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        future::pending(),
        STARTUP_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup EOF: initially pending"
    );
    tokio::time::advance(STARTUP_LIMIT - Duration::from_secs(1)).await;
    drop(out_writer);
    drop(err_writer);
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 startup EOF: still waiting for process"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(poll_once(collect.as_mut()), Poll::Ready(Err(ref e)) if e.kind() == io::ErrorKind::TimedOut),
        "#962 startup EOF: must not renew startup budget"
    );
}

#[tokio::test(start_paused = true)]
async fn output_can_outlive_startup_budget() {
    let (mut writer, stdout) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        tokio::io::empty(),
        async { exit.await.expect("#962 long output: exit sender") },
        STARTUP_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 long output: initially pending"
    );
    for _ in 0..8 {
        tokio::time::advance(Duration::from_secs(25)).await;
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#962 long output: progress is not a total deadline"
        );
        writer
            .write_all(b".")
            .await
            .expect("#962 long output: write gate");
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#962 long output: progress renews idle deadline"
        );
    }
    drop(writer);
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#962 long output: send exit");
    let Poll::Ready(Ok(output)) = poll_once(collect.as_mut()) else {
        panic!("#962 long output: ready output and exit complete");
    };
    assert!(
        output.status.success(),
        "#962 long output: preserves status"
    );
    assert_eq!(
        output.stdout, b"........",
        "#962 long output: retains every byte"
    );
}

#[tokio::test(start_paused = true)]
async fn exit_before_output_bounds_drain() {
    let (_out_writer, stdout) = tokio::io::duplex(16);
    let (_err_writer, stderr) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        async { exit.await.expect("#962 exit drain: exit sender") },
        STARTUP_LIMIT,
        IDLE_LIMIT,
        ChildTrace::new(None),
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 exit drain: initially pending"
    );
    tokio::time::advance(Duration::from_secs(40)).await;
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#962 exit drain: send exit");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 exit drain: inherited pipes not yet closed"
    );
    tokio::time::advance(IDLE_LIMIT - Duration::from_secs(1)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#962 exit drain: full drain budget available"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#962 exit drain: exit switches to short drain deadline");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#962 exit drain: timeout kind"
    );
    assert!(
        error.to_string().contains("output/drain deadline"),
        "#962 exit drain: drain phase diagnostic"
    );
}
