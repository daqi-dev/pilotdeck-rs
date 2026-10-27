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
        async { exit.await.expect("#926 progress: exit sender") },
        IDLE_LIMIT,
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 progress: starts pending"
    );
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(29)).await;
        if stderr_progress {
            err_writer
                .write_all(b".")
                .await
                .expect("#926 progress: stderr write");
        } else {
            out_writer
                .write_all(b".")
                .await
                .expect("#926 progress: stdout write");
        }
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#926 progress: bytes renew idle deadline"
        );
    }
    drop(out_writer);
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#926 progress: send exit");
    let output = collect
        .await
        .expect("#926 progress: completes after 87 seconds");
    assert!(output.status.success(), "#926 progress: preserves exit");
    assert_eq!(
        output.stdout,
        if stderr_progress {
            &b""[..]
        } else {
            &b"..."[..]
        },
        "#926 progress: stdout retained"
    );
    assert_eq!(
        output.stderr,
        if stderr_progress {
            &b"..."[..]
        } else {
            &b""[..]
        },
        "#926 progress: stderr retained"
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
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 silent: starts pending"
    );
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 silent: not before deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#926 silent: must expire at idle deadline");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#926 silent: timeout kind"
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
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 EOF: starts pending"
    );
    tokio::time::advance(Duration::from_secs(29)).await;
    drop(out_writer);
    drop(err_writer);
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 EOF: still waits for exit"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(poll_once(collect.as_mut()), Poll::Ready(Err(ref e)) if e.kind() == io::ErrorKind::TimedOut),
        "#926 EOF: EOF must not renew deadline"
    );
}

// advance() moves the clock but need not flush the timer driver. Register a
// later fence before advancing, then await it without polling the collector.
// Its completion proves the driver has processed the collector's old deadline.
async fn expire_registered_deadline() {
    let mut fence = Box::pin(tokio::time::sleep(IDLE_LIMIT + Duration::from_secs(1)));
    assert!(
        poll_once(fence.as_mut()).is_pending(),
        "#926 ready fence: registered before advancing"
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
        async { exit.await.expect("#926 ready: exit sender") },
        IDLE_LIMIT,
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 ready: starts pending"
    );
    // Pipes and the exit oneshot model a child held behind explicit gates.
    // First readiness is delayed beyond half the idle limit, then progress
    // continues beyond the original total limit without wall-clock sleeps.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#926 ready: held gate remains within idle limit"
        );
        out_writer
            .write_all(b".")
            .await
            .expect("#926 ready: release stdout gate");
        err_writer
            .write_all(b"!")
            .await
            .expect("#926 ready: release stderr gate");
        assert!(
            poll_once(collect.as_mut()).is_pending(),
            "#926 ready: gated progress renews deadline"
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
        .expect("#926 ready: stderr write");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 ready: queued output precedes deadline"
    );
    // Keep stderr open until after the next expiry. Its EOF forces another
    // guard evaluation without renewing the timer before the ready exit.
    expire_registered_deadline().await;
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(101 << 8)))
        .expect("#926 ready: send exit");
    let output = collect
        .await
        .expect("#926 ready: queued exit precedes deadline");
    assert_eq!(
        output.status.code(),
        Some(101),
        "#926 ready: failure status retained"
    );
    assert_eq!(output.stdout, b"...", "#926 ready: drain stdout");
    assert_eq!(output.stderr, b"!!!err", "#926 ready: drain stderr");
}

#[tokio::test(start_paused = true)]
async fn ready_exit_without_output_wins_over_overdue_timer() {
    let (out_writer, stdout) = tokio::io::duplex(16);
    let (err_writer, stderr) = tokio::io::duplex(16);
    let (done, exit) = tokio::sync::oneshot::channel();
    let mut collect = Box::pin(collect_output(
        stdout,
        stderr,
        async { exit.await.expect("#926 exit: exit sender") },
        IDLE_LIMIT,
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 exit: starts pending"
    );
    expire_registered_deadline().await;
    // Release EOF only after expiry so the next select evaluates its wait
    // guard at the overdue time, even if the previous select was suspended.
    drop(out_writer);
    drop(err_writer);
    done.send(Ok(ExitStatus::from_raw(0)))
        .expect("#926 exit: send exit");
    assert!(
        collect
            .await
            .expect("#926 exit: queued exit precedes deadline")
            .status
            .success(),
        "#926 exit: success preserved"
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
    ));
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 drain: exit alone is not complete"
    );
    out_writer
        .write_all(b"tail-out")
        .await
        .expect("#926 drain: stdout write");
    err_writer
        .write_all(b"tail-err")
        .await
        .expect("#926 drain: stderr write");
    assert!(
        poll_once(collect.as_mut()).is_pending(),
        "#926 drain: wait for EOF"
    );
    // An inherited writer that never closes cannot hang the contract forever.
    tokio::time::advance(IDLE_LIMIT).await;
    let Poll::Ready(Err(error)) = poll_once(collect.as_mut()) else {
        panic!("#926 drain: inherited pipes must remain bounded");
    };
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#926 drain: timeout kind"
    );
    assert!(
        error.to_string().contains("tail-out") && error.to_string().contains("tail-err"),
        "#926 drain: partial logs retained"
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
            Poll::Ready(Err(io::Error::other("#926 injected read error")))
        }
    }
    let error = collect_output(Broken, tokio::io::empty(), future::pending(), IDLE_LIMIT)
        .await
        .expect_err("#926 error: read must fail");
    assert!(
        error.to_string().contains("#926 injected read error"),
        "#926 error: read reason retained"
    );
    let error = collect_output(
        tokio::io::empty(),
        tokio::io::empty(),
        future::ready(Err(io::Error::other("#926 injected wait error"))),
        IDLE_LIMIT,
    )
    .await
    .expect_err("#926 error: wait must fail");
    assert!(
        error.to_string().contains("#926 injected wait error"),
        "#926 error: wait reason retained"
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
    .expect("#926 cleanup: spawn cat");
    let pid = rustix::process::Pid::from_raw(child.id().expect("#926 cleanup: child id") as i32)
        .expect("#926 cleanup: valid pid");
    let stdin = child.stdin.take().expect("#926 cleanup: piped stdin");
    (child, stdin, pid)
}

async fn assert_gone(pid: rustix::process::Pid) {
    tokio::time::timeout(IDLE_LIMIT, async {
        loop {
            match rustix::process::test_kill_process(pid) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("#926 cleanup: child liveness probe: {error}"),
            }
        }
    })
    .await
    .expect("#926 cleanup: child must be killed and reaped");
}

#[tokio::test]
async fn idle_timeout_kills_and_reaps_child() {
    let (child, _stdin, pid) = blocked_child(false);
    let error = child_output(child, Duration::ZERO)
        .await
        .expect_err("#926 cleanup: silent child must time out");
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "#926 cleanup: timeout reported"
    );
    assert!(
        matches!(
            rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
            Err(rustix::io::Errno::CHILD)
        ),
        "#926 cleanup: timeout reaps before returning"
    );
    assert_gone(pid).await;
}

#[tokio::test]
async fn cancelling_supervision_kills_child() {
    let (child, _stdin, pid) = blocked_child(true);
    let mut output = Box::pin(child_output(child, IDLE_LIMIT));
    assert!(
        poll_once(output.as_mut()).is_pending(),
        "#926 cleanup: live child initially pending"
    );
    drop(output);
    assert_gone(pid).await;
}
