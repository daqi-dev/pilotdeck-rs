//! #3152 items 13 + 9: the shared parent half of the failed-stderr
//! re-exec probes. Four copies of this shape had drifted —
//! `upstream.rs`, `http_proxy.rs`, `runtime/memory/maintenance/tests.rs`
//! and `native_ws/tests.rs` (#2434), the last even waiting
//! `DEADLINE * 6` instead of the converged 30 s budget the others
//! stated in their own message. One module now owns the mechanics:
//! the `run:<token>` probe-token check, the panic hook the child
//! installs, and the spawn-drain-receipt sequence with its
//! termination conditions.
//!
//! Item 9's rule lives in [`dead_stderr`]: the old shape spawned the
//! child with a piped stderr and dropped the read end after the
//! spawn, trusting the child's first write to land on a peer-less
//! pipe. The parent now proves the sink dead before the child
//! exists — a socketpair whose peer is dropped and probed with a
//! write that must fail `BrokenPipe` — so EPIPE is a settled fact,
//! not a race between the parent's `drop` and the child's first
//! `eprintln!`. fix-r2 (review sibling#3) moved the sink itself to
//! `tests/common/dead_stderr.rs`, one implementation for this tree
//! and the integration tree's `Gateway::dead_stderr`
//! (`tests/console_panic_safety.rs`, item 4) — the twin can no
//! longer drift.
//!
//! Only unix: the socketpair sink is a unix-domain construct, and
//! every host this crate tests on (CI's ubuntu runner, the dev
//! macmini) is unix — `tests/common/write_stage.rs` sets the
//! precedent for bare unix gating in this tree.

use crate::dead_stderr::dead_stderr;
use tokio::io::AsyncReadExt as _;

/// The re-exec probe's token: `run:<token>` in `probe_env`, and the
/// token also required as an argv positional, so a stray env value
/// alone can never branch a normal test run into the child path
/// (the #2301 probe-token shape).
pub(crate) fn probe_token(probe_env: &str) -> Option<String> {
    let value = std::env::var_os(probe_env)?.to_str()?.to_owned();
    let (_, token) = value.split_once(':')?;
    if token.is_empty() {
        return None;
    }
    std::env::args()
        .any(|argument| argument == token)
        .then(|| token.to_owned())
}

/// The child's panic hook: marker file with the panic info, then exit
/// 33 — the parent's evidence, where the default hook would print to
/// the failed stderr and lose it.
pub(crate) fn install_panic_hook(marker_env: &str, tag: &str, token: &str) {
    let marker = std::env::var(marker_env)
        .unwrap_or_else(|_| panic!("{tag} {token} is missing the marker env"));
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::write(&marker, format!("{info}"));
        std::process::exit(33);
    }));
}

/// One site's probe description — everything [`FailedStderrProbe::drive`]
/// cannot know on its own: the env names, the `--exact` receipt path,
/// and the message fragments the site's control words its evidence in.
pub(crate) struct FailedStderrProbe {
    /// The re-exec probe's env (checked by [`probe_token`]).
    pub(crate) probe_env: &'static str,
    /// Where the child's panic hook leaves its evidence.
    pub(crate) marker_env: &'static str,
    /// This parent test's full path — the `--exact` filter and the
    /// receipt name.
    pub(crate) test_path: &'static str,
    /// The site's `#NNNN` tag.
    pub(crate) tag: &'static str,
    /// The token prefix (`pdrs-2558-` / `pdrs-2434-`).
    pub(crate) token_prefix: &'static str,
    /// The success line: what the child's clean exit is claimed to prove.
    pub(crate) success_line: &'static str,
    /// The no-marker line (the marker's path and content follow it).
    pub(crate) marker_line: &'static str,
    /// One row per reached marker: (suffix, note). An empty suffix
    /// names `{token}.reached`; any other names `{token}.reached.{suffix}`.
    pub(crate) reached: &'static [(&'static str, &'static str)],
}

impl FailedStderrProbe {
    /// The parent half: spawn the re-exec'd child with the proven-dead
    /// stderr sink, drain its stdout concurrently with the wait, and
    /// hold every exit to its termination condition — 30 seconds on
    /// the child and the same budget on the stdout drain, each ended
    /// with kill/abort + reap before the named failure.
    pub(crate) async fn drive(&self) {
        let token = format!("{}{}", self.token_prefix, uuid::Uuid::new_v4().simple());
        let marker = std::env::temp_dir().join(format!("{token}.marker"));
        let mut child = tokio::process::Command::new(
            std::env::current_exe()
                .unwrap_or_else(|error| panic!("{} the probe binary path: {error:?}", self.tag)),
        )
        .args([
            "--exact",
            self.test_path,
            "--nocapture",
            "--test-threads=1",
            &token,
        ])
        .env(self.probe_env, format!("run:{token}"))
        .env(self.marker_env, &marker)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(dead_stderr(self.tag))
        .spawn()
        .unwrap_or_else(|error| {
            panic!(
                "{} the failed-stderr probe child spawns: {error:?}",
                self.tag
            )
        });
        // The stdout pipe is drained concurrently with the wait, never after
        // it (the #2301 probe's drain-before-wait shape): a full pipe — a
        // failing child prints a whole backtrace — would block the child
        // before it could exit.
        let stdout = child.stdout.take();
        // `mut` because the drain below takes `&mut console_task` (the
        // timeout wrapper needs the handle to stay borrowable after the
        // await so the Err arm can still abort it).
        let mut console_task = tokio::spawn(async move {
            let mut out = Vec::new();
            if let Some(mut pipe) = stdout {
                let _ = pipe.read_to_end(&mut out).await;
            }
            String::from_utf8_lossy(&out).into_owned()
        });
        // Every wait states its termination condition: 30 seconds on the
        // child, then kill + reap before the named failure.
        let status =
            match tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await {
                Ok(status) => status.unwrap_or_else(|error| {
                    panic!(
                        "{} the failed-stderr probe child is waitable: {error:?}",
                        self.tag
                    )
                }),
                Err(_) => {
                    child.kill().await.unwrap_or_else(|error| {
                        panic!(
                            "{} the timed-out probe child is killed: {error:?}",
                            self.tag
                        )
                    });
                    let _ = child.wait().await;
                    crate::negative_assert!(
                        false,
                        "{} the failed-stderr probe child did not finish within 30s",
                        self.tag
                    );
                    unreachable!("negative_assert! panics");
                }
            };
        // fix-r2 (review concurrency#1): this await once had no termination
        // condition — a descendant holding the pipe's write end leaves
        // `read_to_end` without an EOF even after the child itself is
        // reaped, and the bare await would hang the whole test binary. The
        // same 30 s budget bounds it: on timeout the drain task is aborted
        // and reaped, then the named failure reports it.
        // The await is bound before the match: a `&mut console_task` in the
        // match scrutinee would extend the borrow over the whole statement
        // (E0596), leaving the timeout arm unable to abort the same task it
        // is still borrowing.
        let drain =
            tokio::time::timeout(std::time::Duration::from_secs(30), &mut console_task).await;
        let console = match drain {
            Ok(joined) => joined.unwrap_or_else(|error| {
                panic!("{} the probe's stdout drain finishes: {error:?}", self.tag)
            }),
            Err(_) => {
                console_task.abort();
                let _ = console_task.await;
                crate::negative_assert!(
                    false,
                    "{} the probe's stdout drain did not reach EOF within 30s \
                     (a descendant still holds the pipe)",
                    self.tag
                );
                unreachable!("negative_assert! panics");
            }
        };
        crate::negative_assert!(
            status.success(),
            "{} {} (exit {status})\n{console}",
            self.tag,
            self.success_line
        );
        // Positive receipt — success alone is not evidence the probe case
        // ran: a renamed test makes the `--exact` filter match nothing and
        // libtest still exits 0 (the #273 / fix-r1 lesson).
        crate::negative_assert!(
            crate::test_exact_receipt::single_pass(&status, self.test_path, &console),
            "{} the child's libtest receipt proves the probe case ran\n{console}",
            self.tag
        );
        // The child leaves one reached marker per scenario, each written
        // only after that site's error-line code path has provably run.
        for (suffix, note) in self.reached {
            let reached = std::env::temp_dir().join(if suffix.is_empty() {
                format!("{token}.reached")
            } else {
                format!("{token}.reached.{suffix}")
            });
            crate::negative_assert!(reached.exists(), "{} {note}", self.tag);
            let _ = std::fs::remove_file(&reached);
        }
        crate::negative_assert!(
            !marker.exists(),
            "{} {} ({}): {:?}",
            self.tag,
            self.marker_line,
            marker.display(),
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }
}
