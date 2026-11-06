//! Live configuration store, ported from the fixed TypeScript baseline's
//! `src/pilot/config/PilotConfigStore.ts` (`DefaultPilotConfigStore`).
//!
//! * `reload` calls made before an in-flight reload has started loading share
//!   it: they all resolve to the same snapshot (or the same error) and only
//!   one event is published. A listener that calls `reload` during
//!   publication also joins the in-flight reload, as in the baseline. A call
//!   from anywhere else once loading has started (another task or thread,
//!   including the window after publication before the flight is cleared)
//!   gets a fresh reload that starts after the current one: in the baseline
//!   loading, publication and clearing `reloading` all happen within one
//!   macrotask's microtasks, so an I/O-, timer- or RPC-originated call can
//!   only ever see `reloading` unset and re-reads the files (probe:
//!   `_tmp/20261019-a63-pr5/probes/store_window.mts`).
//! * A successful reload takes the version reserved by the previous success
//!   (`nextVersion`), diffs/classifies with [`super::changes`], clears the
//!   retained reload diagnostics and publishes to every listener. A failed
//!   reload consumes no version; only a `PilotConfigError` replaces the
//!   retained diagnostics, any other error keeps the previous ones.
//! * Listeners are called in subscription order with JavaScript `Set`
//!   iteration semantics (one entry per listener `Arc`; entries added during
//!   publication are visited, removed ones are skipped). A panicking listener
//!   neither stops the others nor changes the store, mirroring the
//!   baseline's `try { listener(event) } catch {}`.
//! * `start_watching` debounces change notifications (250 ms by default) into
//!   one `reload("watch")` whose failure is swallowed (diagnostics stay on the
//!   store). The baseline uses `fs.watch`; this crate has no native watcher
//!   dependency, so watched paths are polled for changes (see
//!   [`WatchOptions::poll_interval`], 20 ms by default). The decision of what
//!   to watch matches the baseline: an existing path is watched itself, a
//!   missing one through its parent directory, and a path whose parent is
//!   missing is not watched; relative config paths are anchored at
//!   [`StoreOptions::cwd`] like the loader. Polling differs from `fs.watch` in
//!   three observable ways: (1) latency — the debounce starts when a change
//!   is *sampled*, up to one poll interval after it happened; (2) sampling
//!   merge — two changes whose sampled times fall within one debounce window
//!   yield one reload although their real spacing was slightly over 250 ms
//!   (with 20 ms polling, spacings in roughly 250–270 ms depending on tick
//!   phase); (3) net-zero bursts — changes that leave content *and* metadata
//!   (including the inode change time, which every write and `utimes` bump)
//!   identical between two samples trigger no reload, where `fs.watch` would
//!   produce an identity reload (version+1, empty `changedPaths`). With
//!   change-time sampling this needs every write in the burst to leave ctime
//!   on the tick the sample already recorded. Ledger: specs/gateway-parity/accepted-diffs.md
//!   A6.3-CONFIG-POLL-001 (unapproved).
//!
//! Each sample compares metadata first (#1739): when the whole metadata
//! fingerprint — length, mtime, ctime, inode, device, plus, for a parent
//! directory watch, every other entry the fingerprint covers — is unchanged,
//! the sample skips the content read and treats the content as unchanged.
//! That inference is sound except in one cell. ctime carries only a tick
//! number, so a write that leaves ctime on the tick the fingerprint already
//! recorded is invisible *to that field*; if it also leaves every other
//! hashed field unchanged — same length, mtime restored, same inode and
//! device, and for a parent directory watch every other covered entry
//! unchanged — then the whole fingerprint is identical and the content change
//! is missed. That window runs from the sample which recorded the tick to the
//! end of the tick — at most one granularity wide (ns on APFS/ext4, 1 s on
//! HFS+), independent of the sampling interval, and empty when the tick has
//! already ended at sample time (the usual case on APFS/ext4) — the same
//! caveat as (3) above. For the one entry the sample reads — the watched
//! config file, and only when it is a regular file of at most
//! [`CONTENT_SAMPLE_LIMIT`] bytes — a rewrite meeting all of those conditions
//! and landing in the window is *not* seen, where the pre-#1739 per-tick
//! content comparison did see it. Larger files and the other entries of a
//! watched directory were metadata-only before #1739 too, so this cell adds
//! nothing for them. Measured on HFS+ (1 s ctime granularity) in
//! `_tmp/20261106-issue-1736/r2/evidence/`; ledger:
//! specs/gateway-parity/diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.md
//! (unapproved). Only when
//! metadata changed is up to [`CONTENT_SAMPLE_LIMIT`] bytes of the watched
//! config file read (the file itself, or the missing config file's name
//! inside a watched parent directory); larger files and other directory
//! entries are sampled by metadata only.
use super::{
    Env, Error,
    changes::{ChangeClass, classify_config_changes, diff_config_snapshots},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::hash_map::DefaultHasher,
    future::Future,
    hash::{Hash, Hasher},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Notify, mpsc},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

/// One immutable configuration snapshot (the `loadPilotConfig` value).
pub type Snapshot = Arc<JsValue>;
/// Shared outcome of one reload; coalesced callers receive the same `Arc`s.
pub type ReloadResult = Result<Snapshot, Arc<Error>>;
/// A store subscriber. Identity is the `Arc` allocation, like a JS function.
pub type Listener = Arc<dyn Fn(&ReloadEvent) + Send + Sync>;
/// Produces the `loadedAt` ISO timestamp for each load.
pub type LoadedAtClock = Arc<dyn Fn() -> String + Send + Sync>;

/// `PilotConfigReloadEvent`.
#[derive(Clone, Debug)]
pub struct ReloadEvent {
    pub previous_snapshot: Snapshot,
    pub next_snapshot: Snapshot,
    pub changed_paths: Vec<JsString>,
    pub change_classes: Vec<ChangeClass>,
}

/// `PilotConfigLoadOptions` for an explicit PilotHome and process cwd.
#[derive(Clone)]
pub struct StoreOptions {
    pub pilot_home: PathBuf,
    /// Stands in for `process.cwd()`; relative values are anchored like
    /// `path.resolve`.
    pub cwd: PathBuf,
    pub env: Env,
    /// `projectRoot`, resolved against `cwd`.
    pub project: Option<String>,
    /// Version of the initial snapshot (`options.version ?? 1`).
    pub version: Option<f64>,
    pub loaded_at: LoadedAtClock,
}

impl StoreOptions {
    pub fn new(pilot_home: impl Into<PathBuf>, cwd: impl Into<PathBuf>, env: Env) -> Self {
        Self {
            pilot_home: pilot_home.into(),
            cwd: cwd.into(),
            env,
            project: None,
            version: None,
            loaded_at: Arc::new(system_loaded_at),
        }
    }
}

fn system_loaded_at() -> String {
    let ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    };
    pilotdeck_transport::system_clock::format_epoch_iso(ms)
}

/// `startWatching` options.
#[derive(Clone, Copy, Debug)]
pub struct WatchOptions {
    /// Quiet period after the last change before reloading (baseline 250 ms).
    /// Over-ceiling values are clamped to a one-hour ceiling in the watcher
    /// (#1736).
    pub debounce: Duration,
    /// How often watched paths are checked; stands in for `fs.watch` events.
    /// Kept well below `debounce` so the sampling merge window stays small.
    /// Sub-floor values are clamped to a 10 ms floor in the watcher (#1650);
    /// over-ceiling values to a 60 s ceiling (#1736).
    pub poll_interval: Duration,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(250),
            poll_interval: Duration::from_millis(20),
        }
    }
}

/// The floor [`watch_loop`] clamps `poll_interval` to (#1650).
///
/// `tokio::time::interval` panics on a zero period, and that panic happens
/// inside the detached watcher task: `WatchHandle::stop` swallows the
/// `JoinError` and `Drop` only aborts, so a zero `poll_interval` would stop
/// config hot reload without a trace.
///
/// Half the 20 ms default rather than the 1 ms floors elsewhere in the tree:
/// every tick re-reads and hashes the watched paths (`fingerprint`, up to 2
/// MiB), so the floor is also the worst-case sampling bill a typo can buy.
/// `upload`'s `MIN_CLEANUP_INTERVAL` is a full second for the same reason,
/// while `user_dialog::channel`'s 1 ms is an in-memory map lookup that costs
/// nothing — neither precedent transfers to a file-hashing poll loop. At this
/// floor a mistyped period costs at most twice the default cadence.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The ceiling [`watch_loop`] clamps `debounce` to (#1736).
///
/// The baseline imposes no upper bound of its own: `startWatching` hands
/// `debounceMs ?? 250` straight to `setTimeout`, and Node does not clamp
/// either — a delay over `2^31 - 1` ms draws a `TimeoutOverflowWarning` and
/// is silently run as 1 ms (probe:
/// `_tmp/20261106-issue-1736/r1/evidence/settimeout-overflow-probe.log`,
/// fixed Node v22.22.0), while anything representable below that is a real
/// timer measured in days. The ceiling is therefore ours to choose, and the
/// choice is semantic, not arithmetic: hot reload exists so that an edit
/// lands within the session that made it, and a quiet period longer than
/// any interactive session is not a quiet period — it is hot reload
/// switched off, the silent failure this fix exists to prevent. One hour
/// is four orders of magnitude above the 250 ms default (ample room for
/// intentional long-window batching) while still guaranteeing a reload
/// inside one sitting; at a day, "watching" would be indistinguishable
/// from "not watching". The bound sits far below `Instant`'s range, so
/// [`deadline_after`] stays a second line of defence, not the only one.
///
/// Because the clamp changes a delay the baseline honours verbatim — two
/// hours is well under Node's `2^31 - 1` ms overflow point and is a real
/// timer there — it is a registered deviation, not an arithmetic guard:
/// `specs/gateway-parity/diffs/A6.3-CONFIG-DEBOUNCE-CEILING-001.md`
/// (unapproved).
const MAX_DEBOUNCE: Duration = Duration::from_secs(60 * 60);

/// The ceiling [`watch_loop`] clamps `poll_interval` to (#1736).
///
/// The baseline has no counterpart to clamp against at all: it is
/// `fs.watch`-driven, so there is no polling cadence in the TS source and
/// the ceiling is purely ours. The failure it prevents is mechanical —
/// `tokio::time::interval` maps a period whose `deadline + period`
/// overflows to `Instant::far_future` (tokio 1.48's `Interval::poll_tick`
/// uses `checked_add(..).unwrap_or_else(Instant::far_future)`), so the
/// first tick fires and the rest never do: the watcher samples once and
/// stops while the handle still looks alive, with no log and no error
/// return. The semantic bound is the same argument as `MAX_DEBOUNCE`'s:
/// this poll stands in for event delivery, and a cadence that detects an
/// edit only after the editor has given up checking is the same silent
/// death. 60 s is ~3000x the 20 ms default; beyond it the detection latency
/// alone exceeds any edit-then-check loop, and the 250 ms debounce it
/// feeds becomes meaningless.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// The period the watcher actually ticks at: every sub-floor value — `ZERO`
/// included — lands on [`MIN_POLL_INTERVAL`], and every over-ceiling value
/// on [`MAX_POLL_INTERVAL`] (#1650, #1736).
///
/// Split out from `watch_loop` so the tests can pin the bounds themselves:
/// a test that only proves "a zero interval still samples" also passes a
/// regression that special-cases zero and lets any other sub-floor period
/// through.
fn effective_poll_interval(options: &WatchOptions) -> Duration {
    options
        .poll_interval
        .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

/// The quiet period the watcher actually debounces to: every over-ceiling
/// value lands on [`MAX_DEBOUNCE`] (#1736) — the same clamp shape
/// [`effective_poll_interval`] applies to `poll_interval`. No floor: a zero
/// debounce is legal (`sleep_until(now)` fires immediately) and needs no
/// #1650-style rescue.
fn effective_debounce(options: &WatchOptions) -> Duration {
    options.debounce.min(MAX_DEBOUNCE)
}

/// The debounce deadline for a caller-supplied duration: an unrepresentable
/// `now + duration` falls back to the ceiling — or to `now` itself if even
/// that overflows, an `Instant` within one hour of the clock's maximum —
/// never panics (#1736). With [`effective_debounce`] in front this is the
/// second line of defence, not the first.
fn deadline_after(now: Instant, duration: Duration) -> Instant {
    now.checked_add(duration)
        .unwrap_or_else(|| now.checked_add(MAX_DEBOUNCE).unwrap_or(now))
}

struct State {
    current: Snapshot,
    last_reload_diagnostics: Vec<JsValue>,
    next_version: f64,
}

struct Entry {
    seq: u64,
    key: usize,
    listener: Listener,
}

#[derive(Default)]
struct Listeners {
    next_seq: u64,
    entries: Vec<Entry>,
}

struct Inner {
    options: StoreOptions,
    state: Mutex<State>,
    listeners: Mutex<Listeners>,
    reloading: Mutex<Option<Arc<Flight>>>,
    /// #1320 fix-r1 F6: a flight settled during the current scheduler turn
    /// (cleared again by the marker task spawned at that settle, which runs
    /// once the settling task suspends). See [`Flight::run_or_wait`].
    settled_in_turn: AtomicBool,
    /// Test hook: runs after a flight is Done, before the slot is cleared.
    #[cfg(test)]
    settle_gate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Inner {
    /// Whether a flight settled during the current scheduler turn; see
    /// [`Flight::run_or_wait`] (#1320 fix-r1 F6).
    fn settled_in_turn(&self) -> bool {
        self.settled_in_turn.load(Ordering::Acquire)
    }
}

/// `PilotConfigStore`. Cloning shares the same store.
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Inner>,
}

impl ConfigStore {
    /// `createPilotConfigStoreSync`: loads the initial snapshot or fails.
    pub fn new(options: StoreOptions) -> Result<Self, Error> {
        let version = options.version.unwrap_or(1.0);
        let initial = load(&options, version)?;
        let next_version = snapshot_version(&initial) + 1.0;
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                state: Mutex::new(State {
                    current: Arc::new(initial),
                    last_reload_diagnostics: vec![],
                    next_version,
                }),
                listeners: Mutex::new(Listeners::default()),
                reloading: Mutex::new(None),
                settled_in_turn: AtomicBool::new(false),
                #[cfg(test)]
                settle_gate: Mutex::new(None),
            }),
        })
    }

    /// `getSnapshot`.
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.inner.state).current.clone()
    }

    /// `getDiagnostics`: the snapshot's diagnostics followed by those retained
    /// from the last failed reload.
    pub fn diagnostics(&self) -> Vec<JsValue> {
        let state = lock(&self.inner.state);
        let mut out = match state.current.get("diagnostics") {
            Some(JsValue::Array(items)) => items.clone(),
            _ => vec![],
        };
        out.extend(state.last_reload_diagnostics.iter().cloned());
        out
    }

    /// `reload(reason)`. The in-flight slot is claimed when this is called,
    /// not when the future is first polled, so calls made back to back share
    /// one reload exactly like the baseline's synchronous promise check. The
    /// baseline starts the load on a microtask (`Promise.resolve().then`),
    /// so a same-tick peer that lands after the claim joins this flight
    /// instead of starting a second load; the port opens that join window
    /// with one scheduler yield on the claiming future's first poll, except
    /// in a scheduler turn that already settled a flight (see
    /// [`Flight::run_or_wait`], #1320 fix-r1 F6).
    pub fn reload(&self, _reason: &str) -> impl Future<Output = ReloadResult> + Send + 'static {
        let mut slot = lock(&self.inner.reloading);
        let flight = match slot.as_ref() {
            Some(current) if current.joinable() => current.clone(),
            // Loading already started elsewhere: its result may predate
            // this call, so reload again once it is done.
            current => {
                let next = Arc::new(Flight {
                    after: current.cloned(),
                    ..Flight::default()
                });
                *slot = Some(next.clone());
                next
            }
        };
        drop(slot);
        let inner = self.inner.clone();
        async move { flight.run_or_wait(&inner).await }
    }

    /// `subscribe`. Subscribing the same `Arc` twice keeps one entry.
    pub fn subscribe(&self, listener: Listener) -> Subscription {
        let key = listener_key(&listener);
        let listener_handle = listener.clone();
        let mut listeners = lock(&self.inner.listeners);
        if !listeners.entries.iter().any(|entry| entry.key == key) {
            listeners.next_seq += 1;
            let seq = listeners.next_seq;
            listeners.entries.push(Entry { seq, key, listener });
        }
        Subscription {
            inner: Arc::downgrade(&self.inner),
            listener: listener_handle,
        }
    }

    /// Test support: the number of live listener entries (an `Arc` shared by
    /// two subscriptions counts once).
    #[cfg(test)]
    pub(crate) fn listener_count(&self) -> usize {
        lock(&self.inner.listeners).entries.len()
    }

    /// `startWatching`. Must be called inside a Tokio runtime. The returned
    /// handle stops watching when dropped or via [`WatchHandle::stop`].
    pub fn start_watching(&self, options: WatchOptions) -> Result<WatchHandle, Error> {
        let paths = self
            .watched_paths()?
            .into_iter()
            .filter_map(WatchedPath::new)
            .collect();
        let (sender, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(watch_loop(
            Arc::downgrade(&self.inner),
            paths,
            options,
            receiver,
        ));
        Ok(WatchHandle {
            task: Some(task),
            changes: sender,
        })
    }

    /// `getWatchedPaths`: the global config path and, with a project, the
    /// project config path, deduplicated in that order.
    fn watched_paths(&self) -> Result<Vec<PathBuf>, Error> {
        let options = &self.inner.options;
        let cwd = super::absolute_cwd(&options.cwd)?;
        // The loader reads the global path through `metadata::resolve(cwd, ..)`;
        // watch the same file (a relative `PILOTDECK_CONFIG_PATH` is relative
        // to `cwd`, and an empty one is `cwd` itself).
        let mut paths = vec![crate::metadata::resolve(
            &cwd,
            &super::global_config_path(&options.pilot_home, &options.env),
        )];
        if let Some(project) = super::project_config_path(&cwd, options.project.as_deref())?
            && !paths.contains(&project)
        {
            paths.push(project);
        }
        Ok(paths)
    }
}

fn load(options: &StoreOptions, version: f64) -> Result<JsValue, Error> {
    super::load_snapshot(
        &options.pilot_home,
        &options.cwd,
        &options.env,
        options.project.as_deref(),
        version,
        &(options.loaded_at)(),
    )
}

fn snapshot_version(snapshot: &JsValue) -> f64 {
    snapshot
        .get("version")
        .and_then(JsValue::as_number)
        .unwrap_or(f64::NAN)
}

fn listener_key(listener: &Listener) -> usize {
    Arc::as_ptr(listener) as *const u8 as usize
}

/// Returned by [`ConfigStore::subscribe`]. Dropping it keeps the listener
/// subscribed, like discarding the baseline's unsubscribe function. It holds
/// the listener so its address (the identity key) cannot be reused while an
/// unsubscribe is still possible.
#[derive(Clone)]
pub struct Subscription {
    inner: Weak<Inner>,
    listener: Listener,
}

impl Subscription {
    /// Remove the listener; idempotent.
    pub fn unsubscribe(&self) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.listeners)
                .entries
                .retain(|entry| entry.key != listener_key(&self.listener));
        }
    }
}

#[derive(Default)]
enum FlightState {
    #[default]
    Pending,
    /// Loading on this thread (listeners run synchronously on it).
    Running(std::thread::ThreadId),
    Done(ReloadResult),
}

#[derive(Default)]
struct Flight {
    state: Mutex<FlightState>,
    done: Notify,
    /// The flight this one must not start before (it was already loading
    /// when this one was claimed).
    after: Option<Arc<Flight>>,
}

impl Flight {
    /// Not started yet, or a reentrant call from a listener during this
    /// flight's publication (the only caller on the loading thread).
    fn joinable(&self) -> bool {
        match &*lock(&self.state) {
            FlightState::Pending => true,
            FlightState::Running(thread) => *thread == std::thread::current().id(),
            FlightState::Done(_) => false,
        }
    }

    async fn wait_done(&self) {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if matches!(&*lock(&self.state), FlightState::Done(_)) {
                return;
            }
            notified.await;
        }
    }

    async fn run_or_wait(self: &Arc<Self>, inner: &Arc<Inner>) -> ReloadResult {
        if let Some(previous) = &self.after {
            // A running flight always settles (see `Settle`).
            previous.wait_done().await;
        }
        // The baseline claims the in-flight slot synchronously but starts the
        // load on a microtask (`Promise.resolve().then`). A same-tick peer
        // that lands after the claim therefore joins this flight instead of
        // starting a second load, and a same-tick snapshot read still sees
        // the previous generation. One scheduler yield before any poller can
        // load reproduces that window; the state is re-checked under the
        // lock after the hop so exactly one poller still executes.
        //
        // #1320 fix-r1 F6: the hop is skipped in a scheduler turn that has
        // already settled a flight. A microtask never lets other scheduled
        // work run before it, but a scheduler yield does: tasks spawned by
        // the earlier flight's publication (the settlement chain) would run
        // between this claim and the load, inverting the golden
        // change-then-settle order (`runtime-refresh`'s
        // `concurrent-changes-failure-then-success`). A reload claimed after
        // that settle loads inline, in the claiming poll, like the
        // baseline's microtask. The join window reopens for the next turn:
        // the marker task spawned at the settle only runs once the settling
        // task has suspended (a single-threaded scheduler never polls a
        // freshly spawned task during the poll that spawned it; on worker
        // threads it may clear early, which only widens the window).
        let mut first = true;
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut hop = false;
            {
                let mut state = lock(&self.state);
                match &*state {
                    FlightState::Done(result) => return result.clone(),
                    FlightState::Running(_) => {}
                    // The turn's first flight opens the same-tick join
                    // window; later claims in the same turn load inline.
                    FlightState::Pending if first && !inner.settled_in_turn() => hop = true,
                    FlightState::Pending => {
                        *state = FlightState::Running(std::thread::current().id());
                        drop(state);
                        return self.execute(inner);
                    }
                }
            }
            if hop {
                first = false;
                tokio::task::yield_now().await;
                continue;
            }
            notified.await;
        }
    }

    fn execute(self: &Arc<Self>, inner: &Arc<Inner>) -> ReloadResult {
        // Settles the flight even if loading or diffing panics, so sharing
        // futures are never left waiting on a `Running` flight.
        struct Settle<'a> {
            flight: &'a Arc<Flight>,
            inner: &'a Arc<Inner>,
            result: Option<ReloadResult>,
        }
        impl Drop for Settle<'_> {
            fn drop(&mut self) {
                let result = self.result.take().unwrap_or_else(|| {
                    let mut error = Error::new("gateway_request_failed", "config reload panicked");
                    error.name = "Error";
                    Err(Arc::new(error))
                });
                *lock(&self.flight.state) = FlightState::Done(result);
                // #1320 fix-r1 F6: a flight settled in this scheduler turn;
                // claims later in the same turn load inline (see
                // `run_or_wait`). The marker task reopens the join window by
                // clearing the flag — it runs once the settling task has
                // suspended, never during the settling poll itself on a
                // single-threaded runtime. Outside a runtime (no marker task
                // possible) the flag stays clear so the hop keeps applying.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    self.inner.settled_in_turn.store(true, Ordering::Release);
                    let inner = Arc::downgrade(self.inner);
                    // A plain binding, not `let _`: dropping the join handle
                    // detaches the task, and clippy flags discarding a future
                    // value with `let _ =`.
                    let _marker = handle.spawn(async move {
                        if let Some(inner) = inner.upgrade() {
                            inner.settled_in_turn.store(false, Ordering::Release);
                        }
                    });
                }
                #[cfg(test)]
                if let Some(gate) = lock(&self.inner.settle_gate).clone() {
                    gate();
                }
                let mut slot = lock(&self.inner.reloading);
                if slot.as_ref().is_some_and(|f| Arc::ptr_eq(f, self.flight)) {
                    *slot = None;
                }
                drop(slot);
                self.flight.done.notify_waiters();
            }
        }
        let mut settle = Settle {
            flight: self,
            inner,
            result: None,
        };
        let result = reload_once(inner);
        settle.result = Some(result.clone());
        drop(settle);
        result
    }
}

fn reload_once(inner: &Arc<Inner>) -> ReloadResult {
    let (previous, version) = {
        let state = lock(&inner.state);
        (state.current.clone(), state.next_version)
    };
    match load(&inner.options, version) {
        Ok(next) => {
            let next = Arc::new(next);
            let changed_paths = diff_config_snapshots(&previous, &next);
            let change_classes = classify_config_changes(&changed_paths);
            {
                let mut state = lock(&inner.state);
                state.current = next.clone();
                state.next_version = snapshot_version(&next) + 1.0;
                state.last_reload_diagnostics.clear();
            }
            publish(
                inner,
                &ReloadEvent {
                    previous_snapshot: previous,
                    next_snapshot: next.clone(),
                    changed_paths,
                    change_classes,
                },
            );
            Ok(next)
        }
        Err(error) => {
            // `requires_host` marks a branch the native loader cannot run; it
            // is not a baseline PilotConfigError.
            if error.name == "PilotConfigError" && !error.requires_host {
                lock(&inner.state).last_reload_diagnostics = error.diagnostics.clone();
            }
            Err(Arc::new(error))
        }
    }
}

fn publish(inner: &Arc<Inner>, event: &ReloadEvent) {
    // JS `Set` iteration: visit, in insertion order, every entry present
    // after the previous one, re-reading the set between calls.
    let mut cursor = 0;
    loop {
        let next = {
            let listeners = lock(&inner.listeners);
            let index = listeners
                .entries
                .partition_point(|entry| entry.seq <= cursor);
            listeners
                .entries
                .get(index)
                .map(|entry| (entry.seq, entry.listener.clone()))
        };
        let Some((seq, listener)) = next else { break };
        cursor = seq;
        // Subscribers cannot block or break snapshot publication.
        let _ = catch_unwind(AssertUnwindSafe(|| listener(event)));
    }
}

/// Returned by [`ConfigStore::start_watching`].
pub struct WatchHandle {
    task: Option<JoinHandle<()>>,
    changes: mpsc::UnboundedSender<()>,
}

impl WatchHandle {
    /// Feed one change notification, exactly as a filesystem change of a
    /// watched path would (the baseline's `fs.watch` callback).
    pub fn notify_change(&self) {
        let _ = self.changes.send(());
    }

    /// Stop watching and wait until the watcher task has ended; a pending
    /// debounced reload is cancelled.
    pub async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Content bytes hashed per sample of one watched config file; beyond this a
/// file is sampled by metadata (length, mtime, ctime, inode) only.
pub const CONTENT_SAMPLE_LIMIT: u64 = 1024 * 1024;

/// #1739 test hook: content bytes actually read, keyed by path. The tests
/// assert that a metadata-identical sample skips the content read without
/// inferring it from timing; the key keeps parallel tests (each in its own
/// temp root) from observing each other's reads.
#[cfg(test)]
static CONTENT_READS: Mutex<Option<std::collections::HashMap<PathBuf, usize>>> = Mutex::new(None);

#[cfg(test)]
fn content_reads(path: &Path) -> usize {
    lock(&CONTENT_READS)
        .as_ref()
        .and_then(|reads| reads.get(path).copied())
        .unwrap_or(0)
}

struct WatchedPath {
    path: PathBuf,
    directory: bool,
    /// For a directory watched in place of a missing config file, that
    /// file's name: its content is sampled once it appears.
    target: Option<std::ffi::OsString>,
    /// The last full sample (content included for the watched config file).
    fingerprint: u64,
    /// The last metadata-only sample (#1739): identical metadata skips the
    /// content read. The one gap — a write that leaves every hashed field,
    /// ctime included, on the values already sampled, because ctime only
    /// records a tick — is spelled out in the module note and
    /// diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
    metadata_fingerprint: u64,
}

impl WatchedPath {
    /// `existsSync(path) ? path : dirname(path)`; `fs.watch` throws (and the
    /// path is skipped) when that target does not exist.
    fn new(path: PathBuf) -> Option<Self> {
        let (path, target) = if path.exists() {
            (path, None)
        } else {
            let target = path.file_name().map(ToOwned::to_owned);
            (path.parent()?.to_path_buf(), target)
        };
        if !path.exists() {
            return None;
        }
        // An existing directory (e.g. an empty config path resolving to cwd)
        // is fingerprinted by its entries, like `fs.watch` on a directory.
        let directory = path.is_dir();
        let metadata_fingerprint = fingerprint(&path, directory, target.as_deref(), false);
        let fingerprint = fingerprint(&path, directory, target.as_deref(), true);
        Some(Self {
            path,
            directory,
            target,
            fingerprint,
            metadata_fingerprint,
        })
    }

    fn changed(&mut self) -> bool {
        // #1739: metadata first. At the default cadence (two watched paths
        // every 20 ms) the per-tick content read costs up to ~19% of a core
        // (measured: ~1.9 ms per 1 MiB read on cb8, page-cache hot; a
        // metadata-only sample is ~1 µs). Only a metadata change walks on to
        // the content read; the single gap — a write that leaves every hashed
        // field, ctime included, on the values already sampled — is spelled
        // out in the module note and diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
        let next_metadata = fingerprint(&self.path, self.directory, self.target.as_deref(), false);
        if next_metadata == self.metadata_fingerprint {
            return false;
        }
        let next = fingerprint(&self.path, self.directory, self.target.as_deref(), true);
        self.metadata_fingerprint = next_metadata;
        std::mem::replace(&mut self.fingerprint, next) != next
    }
}

fn hash_entry(path: &Path, hasher: &mut DefaultHasher, read_content: bool) {
    use std::{io::Read, os::unix::fs::MetadataExt};
    match std::fs::metadata(path) {
        Ok(meta) => {
            true.hash(hasher);
            meta.len().hash(hasher);
            meta.is_dir().hash(hasher);
            meta.modified().ok().hash(hasher);
            // Every write (and `utimes`) bumps ctime, so a same-length rewrite
            // that restores mtime is normally still seen (a replacing rename
            // changes the inode too); the tick-granularity gap and the other
            // fields it needs are in the module note and
            // diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
            (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);
            if read_content && meta.is_file() && meta.len() <= CONTENT_SAMPLE_LIMIT {
                let mut content = Vec::new();
                let read = std::fs::File::open(path)
                    .and_then(|file| file.take(CONTENT_SAMPLE_LIMIT).read_to_end(&mut content))
                    .ok()
                    .map(|_| content);
                #[cfg(test)]
                {
                    if read.is_some() {
                        lock(&CONTENT_READS)
                            .get_or_insert_with(Default::default)
                            .entry(path.to_path_buf())
                            .and_modify(|count| *count += 1)
                            .or_insert(1);
                    }
                }
                read.hash(hasher);
            }
        }
        Err(_) => false.hash(hasher),
    }
}

fn fingerprint(
    path: &Path,
    directory: bool,
    target: Option<&std::ffi::OsStr>,
    sample_content: bool,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    if directory {
        // The directory's own metadata changes on every entry create/delete.
        hash_entry(path, &mut hasher, false);
        let mut names = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        for name in names {
            name.hash(&mut hasher);
            let read_content = sample_content && target == Some(name.as_os_str());
            hash_entry(&path.join(&name), &mut hasher, read_content);
        }
    } else {
        hash_entry(path, &mut hasher, sample_content);
    }
    hasher.finish()
}

async fn watch_loop(
    inner: Weak<Inner>,
    mut paths: Vec<WatchedPath>,
    options: WatchOptions,
    mut changes: mpsc::UnboundedReceiver<()>,
) {
    // #1650: a zero/sub-floor period would panic this detached task and stop
    // hot reload silently; see [`MIN_POLL_INTERVAL`]. #1736: an over-ceiling
    // period would strand the ticker at `Instant::far_future` after its first
    // tick, and an over-ceiling debounce would overflow `Instant + Duration`
    // here — both stop hot reload with the handle still "alive"; see
    // [`MAX_POLL_INTERVAL`] / [`MAX_DEBOUNCE`] and [`deadline_after`].
    let mut ticker = tokio::time::interval(effective_poll_interval(&options));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut deadline: Option<Instant> = None;
    loop {
        let timer = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            Some(()) = changes.recv() => {
                deadline = Some(deadline_after(Instant::now(), effective_debounce(&options)));
            }
            _ = ticker.tick() => {
                if inner.strong_count() == 0 {
                    return;
                }
                let mut changed = false;
                for path in &mut paths {
                    changed |= path.changed();
                }
                if changed {
                    deadline = Some(deadline_after(Instant::now(), effective_debounce(&options)));
                }
            }
            _ = timer => {
                deadline = None;
                let Some(inner) = inner.upgrade() else { return };
                // Reload diagnostics are retained on the store; the watcher
                // must not fail because a reload did.
                let _ = ConfigStore { inner }.reload("watch").await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
