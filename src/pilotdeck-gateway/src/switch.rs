//! #1353 A8.5-pr2 — the switch state machine.
//!
//! The switch order is the matrix §二 row
//! (docs/research/012_phase-a-switch-matrix/matrix.md, the authority this
//! round implements against): steady service → stop the automation face
//! (S1, cron first) → stop accepting and drain every entry E1–E4 (S2) →
//! flush durable channel state (S3) → stop the old owner, background
//! writers included (S4) → only then release the new owner (S5).
//!
//! The machine owns exactly that order and its failure semantics; every step
//! is an injectable face so the assertions stay deterministic:
//!
//! * a face rejecting (A2) or hanging past its bound (A3) halts the switch —
//!   no later face runs, the new-owner gate stays closed, and the error is
//!   reported in the shutdown-bundle form (one failure thrown directly,
//!   several as one aggregate; probe P-A3-STOPREJ,
//!   `_tmp/20261108-issue-1353-r1/raw/probe_stop_reject.log`);
//! * flush is unreachable while entries are still in flight (A10);
//! * the new-owner gate opens only after stop-owner and the recovery face
//!   completed (A8/A9) — never while the old owner or a background writer is
//!   still running.
//!
//! The production faces over the real assembly live in
//! `native_process/switch_wiring.rs`; this module stays independent of the
//! assembly so the machine can be driven by injected faces.

use crate::runtime::cron::{CronAgentGateway, CronError, CronManager, CronTurnSubmit};
use pilotdeck_protocol::JsString;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

/// The shared bound cap, in milliseconds — the same family as the
/// `--drain-ms` gate (1..300000, native_process options parsing).
pub const TIMEOUT_CAP_MS: u64 = 300_000;

/// The slack a machine backstop adds to a face's own budget (A3): long
/// enough for the face's own internal deadline to fire and be reported
/// first, short enough to still bound a hung face.
pub const BACKSTOP_SLACK_MS: u64 = 10_000;

/// The switch phases, matrix §二. `Halted` and `Switched` are terminal: a
/// halted switch never runs a later face, and only a switched one opened the
/// new-owner gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Steady service — the machine has not started.
    Steady,
    /// S1: stop the automation providers (cron first, then always-on wait
    /// semantics).
    StopAutomation,
    /// S2: atomically stop admission and drain E1–E4.
    StopAccepting,
    /// S3: flush durable channel state.
    Flush,
    /// S4: stop the old owner, background writers included.
    StopOwner,
    /// S5: the recovery face before the new-owner gate opens.
    NewHandshake,
    /// The whole sequence completed — the gate is open.
    Switched,
    /// A face rejected (A2) or exceeded its bound (A3).
    Halted,
}

/// DC-1's bounded timeout: positive, finite, at most [`TIMEOUT_CAP_MS`]
/// milliseconds. "Positive and finite" without the cap would also pass
/// values that are unbounded in practice (`u64::MAX` ms, `1e300`), so the
/// cap is part of the same predicate the automation, entry, flush, owner and
/// handshake faces all share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundedTimeout(Duration);

impl BoundedTimeout {
    /// Milliseconds, the unit every face shares. The `u64` spelling cannot
    /// express the non-finite or fractional rejections, so both spellings
    /// route through the same [`Self::from_millis_f64`] predicate —
    /// converting first cannot dodge it.
    pub fn from_millis(ms: u64) -> Result<Self, String> {
        // `u64 as f64` loses precision above 2^53; those values exceed the
        // cap by orders of magnitude, so the loss cannot pass a predicate
        // they must fail.
        Self::from_millis_f64(ms as f64)
    }

    /// The shared DC-1 predicate itself, in the floating spelling the
    /// overflowed and non-finite values arrive in.
    pub fn from_millis_f64(ms: f64) -> Result<Self, String> {
        if !ms.is_finite() {
            return Err(format!("switch timeout must be finite, got {ms}"));
        }
        if ms <= 0.0 {
            return Err(format!("switch timeout must be positive, got {ms}"));
        }
        // `switch-unbounded-timeout` (negative control, A3/DC-1): the cap
        // clause deleted, so the actually-unbounded spellings pass as
        // bounded.
        if ms > TIMEOUT_CAP_MS as f64 && !crate::mutants::on("switch-unbounded-timeout") {
            return Err(format!(
                "switch timeout must be at most {TIMEOUT_CAP_MS} ms, got {ms}"
            ));
        }
        Ok(Self(Duration::from_secs_f64(ms / 1000.0)))
    }

    pub fn duration(self) -> Duration {
        self.0
    }

    /// A machine backstop's bound (A3): a face's own budget plus
    /// [`BACKSTOP_SLACK_MS`]. The shared cap governs operator-supplied
    /// budgets; a backstop is derived from one, so it may exceed the cap by
    /// at most the slack. Positive and finite are checked here too — the
    /// same spelling rules as [`Self::from_millis_f64`], one ceiling
    /// higher.
    pub fn backstop_from_millis(ms: f64) -> Result<Self, String> {
        if !ms.is_finite() {
            return Err(format!("switch backstop must be finite, got {ms}"));
        }
        if ms <= 0.0 {
            return Err(format!("switch backstop must be positive, got {ms}"));
        }
        // `switch-backstop-unbounded-timeout` (negative control, #2363): the
        // backstop's own copy of the cap clause deleted, so an over-cap value
        // passes as a bounded backstop here.
        if ms > (TIMEOUT_CAP_MS + BACKSTOP_SLACK_MS) as f64
            && !crate::mutants::on("switch-backstop-unbounded-timeout")
        {
            return Err(format!(
                "switch backstop must be at most {} ms, got {ms}",
                TIMEOUT_CAP_MS + BACKSTOP_SLACK_MS
            ));
        }
        Ok(Self(Duration::from_secs_f64(ms / 1000.0)))
    }
}

/// The failure form the shutdown bundle produces (probe P-A3-STOPREJ): a
/// single failing stop chain throws its error directly; several failures
/// surface as one aggregate that keeps every message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepFailure {
    Step(String),
    Aggregate {
        message: String,
        errors: Vec<String>,
    },
}

impl StepFailure {
    pub fn step(message: impl Into<String>) -> Self {
        Self::Step(message.into())
    }

    /// The aggregate form `stopGeneration` produces when more than one
    /// provider cleanup fails (probe scenario C: "Project automation
    /// provider cleanup failed." over both causes).
    pub fn aggregate(message: impl Into<String>, errors: Vec<String>) -> Self {
        Self::Aggregate {
            message: message.into(),
            errors,
        }
    }

    pub fn messages(&self) -> Vec<String> {
        match self {
            Self::Step(message) => vec![message.clone()],
            Self::Aggregate { errors, .. } => errors.clone(),
        }
    }
}

impl std::fmt::Display for StepFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step(message) => write!(f, "{message}"),
            Self::Aggregate { message, errors } => {
                write!(f, "AggregateError: {message} ({})", errors.join("; "))
            }
        }
    }
}

/// Why a switch halted. The old owner keeps whatever the completed faces
/// left it — existing connections keep finishing, nothing flushes and no
/// owner stop runs without the machine saying so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchError {
    /// A face rejected (A2).
    Drain(StepFailure),
    /// A face exceeded its bound (A3).
    Timeout { phase: Phase, bound: Duration },
}

impl std::fmt::Display for SwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Drain(failure) => write!(f, "switch halted: {failure}"),
            Self::Timeout { phase, bound } => write!(
                f,
                "switch halted: {phase:?} face exceeded its {:?} bound",
                bound
            ),
        }
    }
}

pub type FaceFuture = Pin<Box<dyn Future<Output = Result<(), StepFailure>> + Send>>;

/// The six machine steps, injectable so every assertion has a deterministic
/// driver. Production implements these over the real assembly
/// (`native_process/switch_wiring.rs`).
pub trait SwitchFaces: Send + Sync {
    /// S1: stop the automation providers. A rejection is the third §三 r3
    /// branch — the stop chain itself failing — and classifies as A2.
    fn stop_automation(&self) -> FaceFuture;
    /// S2: stop admission atomically (channel closing gate and listener at
    /// one point, A1) and drain every E1–E4 entry within the shared bound
    /// (A4/A12). Resolving `Ok` means the drain completed.
    fn stop_accepting(&self) -> FaceFuture;
    /// S3: flush durable state. The machine keeps this face unreachable
    /// until stop-accepting drained (A10).
    fn flush(&self) -> FaceFuture;
    /// S4: stop the old owner — survivors get explicit close frames (A5),
    /// sessions/project and the background runtime dispose (A11).
    fn stop_owner(&self) -> FaceFuture;
    /// S5: the recovery face — whatever the new owner needs before the gate
    /// opens (A9).
    fn recover(&self) -> FaceFuture;
}

/// Every face's bound, all through the same [`BoundedTimeout`] predicate.
#[derive(Clone, Copy, Debug)]
pub struct SwitchTimeouts {
    pub automation: BoundedTimeout,
    pub entries: BoundedTimeout,
    pub flush: BoundedTimeout,
    pub owner: BoundedTimeout,
    pub handshake: BoundedTimeout,
}

impl SwitchTimeouts {
    /// One bound for every face.
    pub fn uniform(bound: BoundedTimeout) -> Self {
        Self {
            automation: bound,
            entries: bound,
            flush: bound,
            owner: bound,
            handshake: bound,
        }
    }

    /// The production shape: the entry and owner faces already carry their
    /// own internal deadlines (the WS drain budget; the host shutdown
    /// budget), so each machine bound is the internal budget plus slack —
    /// the machine's timeout is the backstop for a *hung* face (A3), not a
    /// second copy of the face's own deadline. The budget part is capped at
    /// the shared cap and the slack rides on top: at the maximum
    /// `--drain-ms` the backstop still sits strictly above the face's own
    /// deadline instead of racing it (a tie would make the A3 halt fire at
    /// the same instant and lose the face's own diagnosis).
    pub fn production(drain_budget: Duration) -> Self {
        let base = drain_budget.as_millis() as f64;
        let budget = BoundedTimeout::from_millis_f64(base).expect("drain budget is 1..300000 ms");
        let backstop = |extra_ms: f64| {
            BoundedTimeout::backstop_from_millis(base.min(TIMEOUT_CAP_MS as f64) + extra_ms)
                .expect("backstop is inside (0, cap + slack] ms")
        };
        Self {
            automation: budget,
            entries: backstop(BACKSTOP_SLACK_MS as f64),
            flush: budget,
            owner: backstop(BACKSTOP_SLACK_MS as f64),
            handshake: budget,
        }
    }
}

/// The old owner's release valve (A8/A9). Constructed closed; only a machine
/// that completed stop-owner and recovery opens it, so a new owner can never
/// observe the gate open while the old one — background writers included —
/// is still running.
#[derive(Clone)]
pub struct NewOwnerGate(watch::Sender<bool>);

impl NewOwnerGate {
    fn closed() -> Self {
        let (sender, _) = watch::channel(false);
        Self(sender)
    }

    pub fn is_open(&self) -> bool {
        *self.0.borrow()
    }

    /// Only the machine's completed sequence calls this. `send_modify`, not
    /// `send`: a gate nobody subscribed to yet must still record the open —
    /// `send` drops the value when no receiver exists.
    fn open(&self) {
        self.0.send_modify(|open| *open = true);
    }

    /// Resolves once the gate opened (A9's new-handshake precondition). If
    /// every holder of the gate was dropped without completing — a halted or
    /// abandoned switch — this returns with `is_open()` still false instead
    /// of hanging.
    pub async fn wait_open(&self) {
        let mut receiver = self.0.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The switch machine: runs the faces in the matrix §二 order with the
/// matrix §四 failure semantics.
pub struct SwitchMachine<F> {
    faces: F,
    timeouts: SwitchTimeouts,
    phase: watch::Sender<Phase>,
    gate: NewOwnerGate,
}

impl<F: SwitchFaces> SwitchMachine<F> {
    pub fn new(faces: F, timeouts: SwitchTimeouts) -> Self {
        let (phase, _) = watch::channel(Phase::Steady);
        Self {
            faces,
            timeouts,
            phase,
            gate: NewOwnerGate::closed(),
        }
    }

    /// A receiver that tracks the phase for tests and observers.
    pub fn phase(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    /// The new-owner gate (A8/A9) — closed until the whole sequence,
    /// including the old owner's exit, has completed.
    pub fn gate(&self) -> NewOwnerGate {
        self.gate.clone()
    }

    /// Run the sequence. `Ok` means every face completed and the gate is
    /// open; `Err` means the switch halted — no later face ran and the gate
    /// stays closed.
    pub async fn run(self) -> Result<(), SwitchError> {
        self.enter(Phase::StopAutomation);
        self.bounded(self.timeouts.automation, self.faces.stop_automation())
            .await?;
        self.enter(Phase::StopAccepting);
        self.bounded(self.timeouts.entries, self.faces.stop_accepting())
            .await?;
        // `switch-gate-early-open` (negative control, A8): the release pulled
        // ahead of the owner stop — the gate opens while flush and stop-owner
        // still have to run.
        if crate::mutants::on("switch-gate-early-open") {
            self.gate.open();
        }
        self.enter(Phase::Flush);
        self.bounded(self.timeouts.flush, self.faces.flush())
            .await?;
        self.enter(Phase::StopOwner);
        self.bounded(self.timeouts.owner, self.faces.stop_owner())
            .await?;
        self.enter(Phase::NewHandshake);
        self.bounded(self.timeouts.handshake, self.faces.recover())
            .await?;
        // A8/A9: only a fully completed sequence opens the gate — a halted
        // switch never reaches this line.
        self.gate.open();
        self.enter(Phase::Switched);
        Ok(())
    }

    /// One face under its bound. A rejection (A2) or an overrun (A3) halts
    /// the machine; later faces never run.
    async fn bounded(&self, bound: BoundedTimeout, face: FaceFuture) -> Result<(), SwitchError> {
        let phase = *self.phase.borrow();
        match tokio::time::timeout(bound.duration(), face).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(failure)) => {
                // `switch-halt-continues` (negative control, A2): the halt
                // branch deleted — the rejection is swallowed and the
                // machine walks on to the later faces.
                if crate::mutants::on("switch-halt-continues") {
                    return Ok(());
                }
                self.enter(Phase::Halted);
                Err(SwitchError::Drain(failure))
            }
            Err(_) => {
                self.enter(Phase::Halted);
                Err(SwitchError::Timeout {
                    phase,
                    bound: bound.duration(),
                })
            }
        }
    }

    fn enter(&self, phase: Phase) {
        self.phase.send_if_modified(|current| {
            if *current != phase {
                *current = phase;
                true
            } else {
                false
            }
        });
    }
}

/// A6 (DC-5): the managers whose management face a switch closed. Keyed by
/// manager identity rather than a bare process flag: the native process has
/// exactly one `CronManager`, so in production this is process-global in
/// effect (the RPC routing seam, `native_process/methods/cron.rs`, has no
/// per-request state to thread through), while parallel test assemblies —
/// each with its own manager — cannot observe one another's switches. The
/// entries hold their manager alive, so a freed and reallocated address can
/// never alias a closed one.
static CRON_MANAGEMENT_CLOSED: Mutex<Vec<Arc<CronManager>>> = Mutex::new(Vec::new());

/// Whether this manager's management methods must reject (A6). `None` (no
/// `cron:` section) is never closed — the `requireCron` gate owns that shape.
pub fn cron_management_closed(manager: Option<&Arc<CronManager>>) -> bool {
    match manager {
        None => false,
        Some(manager) => CRON_MANAGEMENT_CLOSED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|closed| Arc::ptr_eq(closed, manager)),
    }
}

/// Close the manager's management face. The S1 cron face does this before the
/// stop, so a management call racing the stop observes the gate, not a
/// half-stopped manager. A switch is one-way — production never reopens it.
pub fn close_cron_management(manager: &Arc<CronManager>) {
    let mut closed = CRON_MANAGEMENT_CLOSED
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !closed.iter().any(|entry| Arc::ptr_eq(entry, manager)) {
        closed.push(manager.clone());
    }
}

/// #2363 item 6: the management face's admission gate — the read-write half
/// the closed registry alone could not provide. Every management dispatch
/// holds a read guard across its whole body (dispatches stay concurrent with
/// one another; the TS baseline gets that serialization for free from the
/// single-threaded event loop, which the blocking-pool port lost), and the
/// S1 stop face takes the write guard before stopping the manager: acquiring
/// it waits for every dispatch already admitted to finish, so a dispatch
/// either completes before the stop proceeds or — once the write guard has
/// landed — answers the closed rejection. The pre-item-6 seam checked the
/// closed registry before the `spawn_blocking`, so a dispatch that had
/// passed the check could still run its whole body on the stopped manager
/// after S1 had already reported Ok.
pub(crate) struct CronManagementGate {
    admission: tokio::sync::RwLock<()>,
}

/// The managers' management-admission gates, keyed by manager identity like
/// [`CRON_MANAGEMENT_CLOSED`] except the key is weak: parallel test
/// assemblies cannot observe one another's switches, and a stale entry is
/// swept on the next lookup (a dead key can never match a live manager), so
/// a freed and reallocated address can never alias another manager's gate
/// while the registry itself never keeps a manager — and with it the
/// manager's scheduler cleanup, `CronRuntime::drop` — alive past the host's
/// own references. Every management dispatch registers here, so a strong
/// key would retire the cleanup of an ordinary host that never closes the
/// face to process exit (#2363 codex concurrency P1).
static CRON_MANAGEMENT_GATES: Mutex<Vec<(Weak<CronManager>, Arc<CronManagementGate>)>> =
    Mutex::new(Vec::new());

/// The manager's management-admission gate, created on first use.
pub(crate) fn cron_management_gate(manager: &Arc<CronManager>) -> Arc<CronManagementGate> {
    let mut gates = CRON_MANAGEMENT_GATES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Sweep the dead keys first — that is what keeps the registry from
    // owning the managers' lifetimes: an entry only survives while its
    // manager is alive somewhere else, and a dead entry lingers at most
    // until the next lookup reclaims it (per dead manager at most one
    // entry — its weak key and the gate it holds).
    gates.retain(|(held, _)| held.strong_count() > 0);
    // Compare by pointer, never by upgrading the entry's key: an upgrade
    // would lend the entry a fleeting strong reference for the length of
    // the comparison, so a concurrent test asserting the manager's death
    // (after dropping its own references) could observe it alive — the
    // same window a lookup could race a concurrent drop through.
    let key = Arc::downgrade(manager);
    // `switch-cron-gate-registry-lookup` (negative control, #2363): the
    // find's match is discarded, so every lookup mints and registers a
    // fresh gate — the reuse criterion's red shape (a repeat lookup of
    // the same live manager must answer the same gate).
    let matched = gates
        .iter()
        .find(|(held, _)| held.ptr_eq(&key))
        .filter(|_| !crate::mutants::on("switch-cron-gate-registry-lookup"));
    if let Some((_, gate)) = matched {
        return gate.clone();
    }
    let gate = Arc::new(CronManagementGate {
        admission: tokio::sync::RwLock::new(()),
    });
    gates.push((key, gate.clone()));
    gate
}

impl CronManagementGate {
    /// The dispatch half: the read guard a management dispatch holds across
    /// its whole body. Blocking — the dispatch closure runs on the blocking
    /// pool (#1509 P2-3b), never from async code. The only wait is behind
    /// the stop face's write guard, which it acquires after the closed mark
    /// and holds for an instant alone.
    pub(crate) fn enter_dispatch(&self) -> tokio::sync::RwLockReadGuard<'_, ()> {
        self.admission.blocking_read()
    }

    /// The stop half: the write guard whose acquisition drains every
    /// in-flight dispatch's read guard (#2363 item 6). Async — the stop face
    /// runs on the runtime, and an unyielding wait would defeat the
    /// machine's own A3 bound; dropping the future cancels the queued
    /// acquisition (the lock is cancel-safe), which is the cancelled-face
    /// shape: the closed mark stays, the admitted dispatches finish on
    /// their own, the manager keeps whatever the completed faces left it.
    async fn drain_dispatches(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.admission.write().await
    }
}

/// Where the recording cron gateway accumulates abort rejections.
pub type AbortRecord = Arc<Mutex<Vec<String>>>;

fn cron_message(error: &CronError) -> String {
    String::from_utf16_lossy(error.message().units())
}

/// #1326's one `CronManager` shared between the switch machine (S1) and the
/// host teardown. The flag makes the stop idempotent: the machine stops it
/// first; the teardown then observes the earlier stop instead of repeating
/// it.
pub struct CronStopHandle {
    manager: Arc<CronManager>,
    stopped: AtomicBool,
    abort_failures: AbortRecord,
}

impl CronStopHandle {
    pub fn new(manager: Arc<CronManager>, abort_failures: AbortRecord) -> Self {
        Self {
            manager,
            stopped: AtomicBool::new(false),
            abort_failures,
        }
    }

    /// Stop once. A second call (the host teardown after a machine-driven
    /// stop) observes the earlier stop instead of repeating it.
    pub fn stop_once(&self) -> Result<(), CronError> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.manager.stop()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// A7: the abort rejections `CronManager::stop` swallowed while aborting
    /// in-flight runs.
    pub fn abort_failures(&self) -> Vec<String> {
        self.abort_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The machine's S1 face over this handle: close the management face
    /// (A6), drain the dispatches already admitted (#2363 item 6), stop
    /// the manager exactly once, and report the abort rejections its stop
    /// swallowed (A7).
    pub fn stop_face(self: &Arc<Self>) -> FaceFuture {
        let handle = self.clone();
        Box::pin(async move {
            close_cron_management(&handle.manager);
            // #2363 item 6: the admission gate's write half — every dispatch
            // already past the closed check holds the read half across its
            // whole body, so acquiring the write half waits for those
            // bodies to finish before anything stops. The wait is bounded
            // by the machine's own automation bound (a dispatch whose
            // predecessor never completes is the same hung-face A3 the
            // machine already polices); a dispatch arriving after the
            // write half releases sees the closed mark and rejects.
            // `switch-cron-stop-no-drain` (negative control, #2363): the
            // write half skipped — the face reports Ok while an admitted
            // dispatch is still running, the pre-item-6 check-then-act
            // window.
            if !crate::mutants::on("switch-cron-stop-no-drain") {
                let gate = cron_management_gate(&handle.manager);
                drop(gate.drain_dispatches().await);
            }
            handle.stop_once().map_err(|error| {
                StepFailure::step(format!("cron stop failed: {}", cron_message(&error)))
            })?;
            for failure in handle.abort_failures() {
                // #2558: a best-effort abort-rejected report (std panics when
                // a raw console write fails); a panic here would kill the
                // switch face mid-stop, the halt line's shape.
                crate::native_process::channels::console_log_line(
                    &mut std::io::stderr(),
                    &format!("native cron stop: in-flight run abort rejected: {failure}"),
                );
            }
            Ok(())
        })
    }
}

/// A7's observable abort surface: `CronManager::stop` aborts in-flight runs
/// through the bound gateway and swallows every rejection (the `let _ =
/// gateway.abort_turn(...)` in manager.rs `stop`), so the rejection has to be
/// recorded where the S1 face can report it. Delegation is
/// behavior-preserving.
pub struct RecordingCronGateway {
    inner: Arc<dyn CronAgentGateway>,
    failures: AbortRecord,
}

impl RecordingCronGateway {
    pub fn wrap(inner: Arc<dyn CronAgentGateway>) -> Self {
        Self {
            inner,
            failures: AbortRecord::default(),
        }
    }

    pub fn failures(&self) -> AbortRecord {
        self.failures.clone()
    }
}

impl CronAgentGateway for RecordingCronGateway {
    fn submit_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.inner.submit_turn(input)
    }

    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.inner
            .abort_turn(session_key, run_id, reason)
            .inspect_err(|error| {
                let run = run_id
                    .map(|run| String::from_utf16_lossy(run.units()))
                    .unwrap_or_else(|| "-".into());
                self.failures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("abortTurn {run} rejected: {}", cron_message(error)));
            })
    }

    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.inner.close_session(session_key, reason)
    }
}
