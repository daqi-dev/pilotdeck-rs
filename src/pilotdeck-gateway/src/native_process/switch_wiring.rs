//! #1353 A8.5-pr2 — the production switch faces over the real assembly.
//!
//! [`crate::switch`] owns the order and the failure semantics with injectable
//! faces; this module is the production implementation of those faces:
//!
//! * S1 (`stop_automation`) stops the one `CronManager` through
//!   [`crate::switch::CronStopHandle`] — the handle closes the A6 management
//!   face and reports the abort rejections A7 records;
//! * S2 (`stop_accepting`) seals admission at one point (A1) and drains both
//!   entry surfaces — the native WS entries through `PhasedRun::drained()`
//!   and the channel entries through the close join;
//! * S3 (`flush`) flushes the durable channel state, re-asserting the A10
//!   precondition (entries drained first);
//! * S4 (`stop_owner`) is `PhasedRun::finish()` — the A5 close notice, the
//!   bounded abort, and the host shutdown that runs the assembly teardown;
//! * S5 (`recover`) is empty here: the new handshake belongs to the next
//!   owner's process, and the gate opening after this face is the whole
//!   observable recovery in this embedding.
//!
//! The embedding is a signal-triggered process exit, so a halted machine
//! still has to terminate the process: [`ProductionFaces::finish_after_halt`]
//! walks the remaining way out (a registered difference,
//! specs/gateway-parity/diffs/).

use crate::native_ws::PhasedRun;
use crate::switch::{CronStopHandle, FaceFuture, StepFailure, SwitchFaces};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

use super::channels::NativeChannels;

/// The handles `start_with_webhook` hands the embedding that drives the
/// machine: the cron stop handle (absent when no `cron:` section was
/// configured) and the channel assembly.
pub(crate) struct AssemblySwitchHandles {
    pub(crate) cron: Option<Arc<CronStopHandle>>,
    pub(crate) channels: Arc<NativeChannels>,
}

/// The out-of-band slots that carry those handles out of `start_assembly`:
/// the channel assembly is constructed before the host starts, the cron stop
/// handle only on the gateway thread inside the builder, so both travel
/// through `OnceLock`s the embedding reads once the builder has run.
#[derive(Clone, Default)]
pub(crate) struct AssemblySwitchSlots {
    pub(crate) cron: Arc<std::sync::OnceLock<Arc<CronStopHandle>>>,
    pub(crate) channels: Arc<std::sync::OnceLock<Arc<NativeChannels>>>,
}

/// A1's single admission flip: the channel server's close and the native WS
/// listener stop linearize at one point. `ChannelServer::close` runs its
/// synchronous prefix — the `closing` flip and the running-handle claim — at
/// call time and returns the joining future, so constructing it and then
/// firing the trigger leaves no await between the two flips: after `seal()`
/// returns, no new entry is accepted through either surface.
#[derive(Clone)]
pub(crate) struct AdmissionGate {
    channels: Arc<NativeChannels>,
    trigger: watch::Sender<bool>,
}

impl AdmissionGate {
    pub(crate) fn new(channels: Arc<NativeChannels>, trigger: watch::Sender<bool>) -> Self {
        Self { channels, trigger }
    }

    pub(crate) fn channels(&self) -> &Arc<NativeChannels> {
        &self.channels
    }

    /// The synchronous seal. The returned future joins the channel stops;
    /// the admission effects have already happened when this returns.
    pub(crate) fn seal(
        &self,
    ) -> crate::channel::protocol::BoxFuture<Result<(), crate::channel::protocol::ChannelError>>
    {
        let close = self.channels.server().close();
        // `send_modify`, not `send`: a watch `send` drops the value when no
        // receiver is subscribed yet — the trigger must persist regardless.
        self.trigger.send_modify(|fired| *fired = true);
        close
    }
}

/// The production faces. Clone shares every handle — the machine runs one
/// clone and the embedding keeps another for the post-halt walk.
#[derive(Clone)]
pub(crate) struct ProductionFaces {
    cron: Option<Arc<CronStopHandle>>,
    admission: AdmissionGate,
    phased: Arc<PhasedRun>,
    /// A10's precondition latch: set by the S2 face to the WS drain's own
    /// verdict, checked by the flush face.
    entries_drained: Arc<AtomicBool>,
}

impl ProductionFaces {
    pub(crate) fn new(
        cron: Option<Arc<CronStopHandle>>,
        admission: AdmissionGate,
        phased: Arc<PhasedRun>,
    ) -> Self {
        Self {
            cron,
            admission,
            phased,
            entries_drained: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The death march after a halted machine. This embedding's switch is
    /// the process's own shutdown — SIGTERM must still terminate the process
    /// (the matrix A2/A3 halt semantics describe a surviving owner, which a
    /// process exit cannot be) — so the remaining faces still run: seal the
    /// admission, drain what can be drained, run the S4 half. The machine's
    /// own error stays the one the exit status reports; this returns the
    /// teardown's.
    ///
    /// The channel close join is bounded here: a hung channel stop must not
    /// keep a halted process alive. Entries-scale, like the machine's own
    /// S2 backstop — the drain budget plus its slack — so a close that
    /// honors the budget is still joined to completion.
    ///
    /// #2363: the S4 half (`PhasedRun::finish`) spends the same owner-scale
    /// deadline — one `Option<Instant>` walks both halves (the close join and
    /// the finish), the waiver band included. A hung finish — a host teardown
    /// that never answers, an un-abortable connection parked in the abort
    /// join — must not keep a halted process alive either: the timeout is
    /// reported as the walk's own failure and the walk returns, leaving the
    /// exit to the embedding.
    pub(crate) async fn finish_after_halt(&self) -> Result<(), crate::ProxyError> {
        let close = self.admission.seal();
        let drained = self.phased.drained().await;
        // `saturating_add`, not `+`: a `Duration::MAX` drain budget must reach
        // the waiver branch below (saturated to `MAX`, which `checked_add`
        // then overflows on) rather than panicking here on the way in.
        let close_bound = self
            .phased
            .drain_budget()
            .saturating_add(Duration::from_millis(crate::switch::BACKSTOP_SLACK_MS));
        // #2363: one owner-scale deadline for the whole walk — the close join
        // and the S4 half spend the same bound, and the S4 half does not
        // re-arm a fresh one. A halted process must not be kept alive longer
        // than a single owner scale, so a hung close followed by a hung
        // finish still exits within the one bound (and an S4 that honors its
        // own internal deadlines after a close that already spent the bound
        // is cut short by the walk's deadline, not waited out on top of it).
        // A deadline that does not fit the clock is awaited without one —
        // the #1949 waiver spelling the host drain itself uses
        // (`gateway.rs`); the filter's headroom is that same
        // tick-conversion slack, the shared constant.
        let deadline = tokio::time::Instant::now()
            .checked_add(close_bound)
            .filter(|deadline| {
                deadline
                    .checked_add(crate::runtime::turn::gateway::TICK_CONVERSION_HEADROOM)
                    .is_some()
            });
        let close_result = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, close).await,
            // The waiver band: a drain budget too large for the clock keeps
            // both halves' pre-#2363 unbounded await rather than panicking at
            // timer registration.
            None => Ok(close.await),
        };
        let finish_result = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, self.phased.finish()).await,
            None => Ok(self.phased.finish().await),
        };
        let mut failures = Vec::new();
        match close_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(format!("channel close: {}", error.message())),
            Err(_) => {
                // `switch-halt-close-swallowed` (negative control, #2363):
                // this branch's failure report deleted, so the hung channel
                // stop is no longer surfaced by the halt walk.
                if !crate::mutants::on("switch-halt-close-swallowed") {
                    failures.push("channel close: exceeded the halt close bound".to_owned());
                }
            }
        }
        match finish_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(error.to_string()),
            Err(_) => {
                // #2557: the final-cancellation cell. The shared deadline
                // dropped the first `finish()` mid-body, the walk returns on
                // this timeout and the process exits, so no revisit is
                // coming to re-issue the teardown request — the cancelled
                // half may have died inside the close-notice window, before
                // its own request below `finish()`'s window ever fired,
                // which would leave the teardown to the unawaited `StopHost`
                // drop request. Synchronous and idempotent, so a no-op when
                // the cancelled run (or the drain) already sent it; the
                // normal completion path is untouched — the window still
                // closes before the request, the shutdown is not moved up.
                // `switch-halt-owner-skip-reissue` (negative control,
                // #2557): the re-issue deleted, so the cell regresses to the
                // unawaited backstop alone.
                if !crate::mutants::on("switch-halt-owner-skip-reissue") {
                    self.phased.request_host_shutdown();
                }
                // `switch-halt-owner-swallowed` (negative control, #2363):
                // this branch's failure report deleted, so the hung owner
                // finish is no longer surfaced by the halt walk.
                if !crate::mutants::on("switch-halt-owner-swallowed") {
                    failures.push("owner finish: exceeded the halt owner bound".to_owned());
                }
            }
        }
        // `drained() == false` already surfaces through `finish()`'s own
        // reporting; nothing else reads it here.
        let _ = drained;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(crate::ProxyError::from(failures.join("; ")))
        }
    }
}

impl SwitchFaces for ProductionFaces {
    fn stop_automation(&self) -> FaceFuture {
        match &self.cron {
            // S1: close the management face (A6) before the stop, stop the
            // manager exactly once, report the swallowed abort rejections
            // (A7). Without a `cron:` section there is no automation to stop.
            Some(handle) => handle.stop_face(),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn stop_accepting(&self) -> FaceFuture {
        let admission = self.admission.clone();
        let phased = self.phased.clone();
        let entries_drained = self.entries_drained.clone();
        Box::pin(async move {
            // A1: the seal happens synchronously at the first poll of this
            // face — both admission flips, no await between them.
            let close = admission.seal();
            // The channel stops (spawned inside the seal) and the WS entry
            // drain proceed concurrently — both are S2's entry surfaces.
            let drained = phased.drained().await;
            entries_drained.store(drained, Ordering::SeqCst);
            close.await.map_err(|error| {
                StepFailure::step(format!("channel close failed: {}", error.message()))
            })?;
            // A4/A12: an expired WS drain budget is a drain failure, never
            // a successful quiescence — the switch halts before flush.
            if !drained {
                return Err(StepFailure::step(
                    "native WS drain deadline exceeded: entries still in flight",
                ));
            }
            Ok(())
        })
    }

    fn flush(&self) -> FaceFuture {
        let channels = self.admission.channels().clone();
        let entries_drained = self.entries_drained.clone();
        Box::pin(async move {
            // A10: flush is unreachable while entries are still in flight.
            // The machine enforces the order; the face re-asserts it so a
            // wiring bug cannot write half-drained state durably.
            // `switch-flush-before-drain` (negative control): the
            // precondition gate deleted, so the half-drained flush proceeds.
            if !entries_drained.load(Ordering::SeqCst)
                && !crate::mutants::on("switch-flush-before-drain")
            {
                return Err(StepFailure::step(
                    "channel flush before the entry drain completed",
                ));
            }
            channels.flush().await.map_err(StepFailure::step)
        })
    }

    fn stop_owner(&self) -> FaceFuture {
        let phased = self.phased.clone();
        Box::pin(async move {
            // S4's WS half: A5 close notice, bounded abort, host shutdown —
            // the last one runs the assembly teardown (cron stop observes the
            // machine's own, channels close idempotently, sessions and
            // project dispose).
            phased
                .finish()
                .await
                .map_err(|error| StepFailure::step(error.to_string()))
        })
    }

    fn recover(&self) -> FaceFuture {
        // A9 in this embedding: the new handshake belongs to the next
        // owner's process; the gate opening after this face is the whole
        // observable recovery here.
        Box::pin(async { Ok(()) })
    }
}

/// The stop future the phased run waits on: the admission trigger the S2
/// face (or the post-halt walk) fires. Kept as a function so `run()` and
/// the tests build the identical pairing of sender and receiver.
pub(crate) fn trigger_stop(
    mut trigger: watch::Receiver<bool>,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        // A dropped sender resolves `wait_for` with an error — a stop, not a
        // hang: nothing is left to wait for.
        let _ = trigger.wait_for(|fired| *fired).await;
    })
}
