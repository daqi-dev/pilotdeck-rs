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
//!   to watch matches the baseline — an existing path is watched itself, a
//!   missing one through its parent directory, and a path whose parent is
//!   missing is not watched — except that a non-`NotFound` startup stat
//!   failure keeps the path itself with an unknown mode for the first
//!   `changed()` to re-probe (#1792 r1); relative config paths are anchored at
//!   [`StoreOptions::cwd`] like the loader. Polling differs from `fs.watch` in
//!   three observable ways: (1) latency — the debounce starts when a change
//!   is *sampled*, up to one poll interval after it happened; (2) sampling
//!   merge — two changes whose sampled times fall within one debounce window
//!   yield one reload although their real spacing was slightly over 250 ms
//!   (with 20 ms polling, spacings in roughly 250–270 ms depending on tick
//!   phase); (3) net-zero bursts — changes that leave both the content and
//!   the sampled metadata identical between two samples trigger no reload,
//!   where `fs.watch` would produce an identity reload (version+1, empty
//!   `changedPaths`). A burst whose content differs but whose metadata does
//!   not is the separate cell registered in
//!   `diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.md`, not this one. Ledger:
//!   `accepted-diffs.md` A6.3-CONFIG-POLL-001, corrected by
//!   `diffs/A6.3-CONFIG-POLL-001-R1.md` (both unapproved).
//!
//! Each sample compares metadata first (#1739): when the whole metadata
//! fingerprint — length, mtime, ctime, inode, device, plus, for a parent
//! directory watch, every other entry the fingerprint covers — is unchanged,
//! the sample skips the content read and treats the content as unchanged.
//! That inference is sound except in one cell. ctime carries only a tick
//! number, so a write that leaves ctime on the tick the fingerprint already
//! recorded is invisible *to that field*; if it also leaves every other
//! hashed field unchanged — same length, same `is_dir`, same mtime, same
//! inode and device, and for a parent directory watch every other covered
//! entry unchanged — then the whole fingerprint is identical and the content
//! change is missed. That window runs from the sample which recorded the
//! tick to the end of the tick — at most one granularity wide (ns on
//! APFS/ext4, 1 s on HFS+), independent of the sampling interval, and empty
//! when the tick has already ended at sample time (the usual case on
//! APFS/ext4) — the same caveat as (3) above. For the one entry the sample
//! reads — the watched config file, and only when it is a regular file of at
//! most [`CONTENT_SAMPLE_LIMIT`] bytes — a rewrite meeting all of those
//! conditions and landing in the window is *not* seen, where the pre-#1739
//! per-tick content comparison did see it whenever the two content hashes
//! differ (a read that failed on one side only hashes as absent and differs
//! from present bytes, so the old comparison caught that too). Larger files
//! and the other entries of a watched directory were metadata-only before
//! #1739 too, so this cell adds nothing for them. Measured on HFS+ (1 s
//! ctime granularity) in `_tmp/20261106-issue-1736/r2/evidence/`; ledger:
//! `diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.md` (corrected by
//! `diffs/A6.3-CONFIG-METADATA-SHORTCUT-001-R1.md`, unapproved).
//!
//! A sample that fails any read — a `stat`, a `read_dir`, or the content
//! read of the watched config file — is *incomplete* and commits no metadata
//! fingerprint (#1792). A `NotFound` is not a failure but a complete
//! observation of absence (#1792 r1): a dangling directory entry or a
//! vanished directory is stable, observable state and must not keep
//! `metadata_fingerprint` permanently `None`; only a failure that can hide
//! content (permission, descriptor exhaustion, I/O) does. What an incomplete
//! sample reports splits by what failed. A failed content read still
//! observed the metadata, so the change — content folded to unknown — is
//! reported once while the read keeps failing and once more when it
//! recovers. A failed `stat` or `read_dir` (*obscured* sample, #1792 r2)
//! observed nothing trustworthy: it commits nothing, is compared only
//! against the previous obscured sample — a change among the entries that
//! *are* observable is real even while one entry stays unreadable, and the
//! same unreadable state stays quiet — and recovery compares against the
//! last complete sample: a change (a delete included) made during the
//! outage is reported, an unchanged recovery is not. A startup *probe*
//! failure (`WatchedPath::new`'s stat) keeps the path watched with an
//! unknown mode instead of dropping it (#1792 r1), and the first complete
//! sample after it must report, because the config may have changed while
//! the probe — and every sample — was failing: an identity reload (version+1,
//! empty `changedPaths`) when nothing changed in between, where the
//! baseline's `fs.watch` starts watching without reading and produces no
//! such event (registered in `diffs/A6.3-CONFIG-POLL-001-R1.md`). Obscured
//! samples commit no full fingerprint either, so a persistently unreadable
//! path is not a reload storm and its folded absence cannot be mistaken
//! for "unchanged". The price of retrying is that while a path stays in the
//! incomplete state every tick re-attempts the reads instead of
//! short-circuiting on metadata — the failed reads themselves are cheap
//! errors, and the content read is bounded by [`CONTENT_SAMPLE_LIMIT`] per
//! tick per watched config file, at the cadence [`MIN_POLL_INTERVAL`] caps.
//! One further #1792 r2 rule: the tick that *reports* a change commits no
//! metadata shortcut either — the metadata-only and the full sample happen
//! at different times, and a match on the first is not evidence the second
//! saw the same state — so one extra content read lands on the tick after
//! every reported change.
//!
//! #1987 narrows three windows in those rules. The first obscured tick
//! after a complete one — the transition, with no folded obscured
//! predecessor — also compares the watched config file itself against its
//! fingerprint from the last complete sample, so an obstruction starting on
//! the same tick as a rewrite of that file is reported immediately instead of
//! after the obstruction clears (the folded chain only covers the later
//! obscured ticks; every other entry's contribution in an obscured fold is
//! hashed from failed reads and is not comparable). A mixed tick — the
//! metadata-only sample failed but the folded one succeeded — is a complete
//! observation: it is compared against the last complete fingerprint and
//! committed, not routed through the obscured path (which used to defer the
//! change a tick and leave a complete folded value as the obscured chain's
//! baseline, false-reporting on a later readability flap). And the startup
//! takes one full sample, content included, committing no metadata
//! shortcut: the first tick re-samples in full (one extra bounded content
//! read per watched path) so a write landing between the startup samples —
//! absorbed by the full one while the committed metadata predated it — is
//! reported instead of never.
//! Only when metadata changed — or the last sample was incomplete — is up
//! to [`CONTENT_SAMPLE_LIMIT`] bytes of the watched config file read (the
//! file itself, or the missing config file's name inside a watched parent
//! directory); larger files and other directory entries are sampled by
//! metadata only.
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
/// inside the detached watcher task: `Drop` only aborts, and only
/// [`WatchHandle::stop`] ever joins the task — where the death is at most
/// reported after the fact ([`WatchStop::Panicked`], #1920), never prevented —
/// so a zero `poll_interval` would still stop config hot reload with no
/// signal until a `stop` happened to be called.
///
/// Half the 20 ms default rather than the 1 ms floors elsewhere in the tree:
/// every tick stats the watched paths, and only a metadata change (or an
/// incomplete sample, #1792) walks on to the content read (`fingerprint`,
/// up to 2 MiB), so the floor is also the worst-case sampling bill a typo
/// can buy.
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
/// second line of defence, not the first; its own inner fallback
/// (`checked_add(MAX_DEBOUNCE).unwrap_or(now)`) is unreachable from the
/// watcher path, where every duration arrives already clamped to ≤ 1 h with
/// `now` far below the clock's maximum — it only guards a direct call with
/// an unclamped `Duration::MAX` (the negative control `deadline-add-unchecked`
/// binds that direct-call test), so do not read it as a hot-path defence.
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

/// How [`WatchHandle::stop`] found the watcher task (#1920).
///
/// `stop` aborts the task before joining it, so the cancelled shape is the
/// normal one; this enum exists so the other two are no longer swallowed the
/// way `let _ = task.await` used to swallow them. A watcher that died by panic
/// looks exactly like a stopped one through every other observable —
/// `is_finished()` (tokio marks the panicked task complete), a released
/// `Weak`, "no reload" — which is #1650's silent hot-reload death, so the
/// fact of the panic has to leave the API to be observable at all. The panic
/// message itself already reaches stderr through the default panic hook;
/// the enum carries the queryable fact, not a copy of the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchStop {
    /// The task returned on its own before the abort took effect (the store
    /// was dropped and the loop's owner check ended it).
    Finished,
    /// The task was aborted by this very `stop` — the normal path.
    Cancelled,
    /// The task had died by panic: hot reload had been silently off since.
    Panicked,
}

impl WatchHandle {
    /// Feed one change notification, exactly as a filesystem change of a
    /// watched path would (the baseline's `fs.watch` callback).
    pub fn notify_change(&self) {
        let _ = self.changes.send(());
    }

    /// Stop watching and wait until the watcher task has ended; a pending
    /// debounced reload is cancelled. Returns how the task actually ended
    /// (#1920): the abort this stop issues makes [`WatchStop::Cancelled`] the
    /// normal shape, [`WatchStop::Finished`] means the loop had already
    /// returned on its own (the store was dropped first), and
    /// [`WatchStop::Panicked`] means the watcher had died by panic — the
    /// #1650 silent death that `let _ = task.await` used to hide.
    pub async fn stop(mut self) -> WatchStop {
        let Some(task) = self.task.take() else {
            // The task was already taken out of this handle (a test joined it
            // itself); there is no watcher left to classify.
            return WatchStop::Finished;
        };
        task.abort();
        match task.await {
            // Measured on this runtime (tokio 1.48): abort-then-join joins
            // `Err(JoinError::Cancelled)` and a task that died by panic joins
            // `Err(JoinError::Panic(..))`, so `is_panic()` — checked first,
            // like #1899's `watcher_stop` — leaves `is_cancelled()` as the
            // only other shape.
            Err(err) if err.is_panic() => WatchStop::Panicked,
            Err(_) => WatchStop::Cancelled,
            Ok(()) => WatchStop::Finished,
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
/// file is sampled by metadata (length, `is_dir`, mtime, ctime, inode,
/// device) only.
/// The bound exists for the same reason as [`MIN_POLL_INTERVAL`]: the content
/// read is only paid when metadata changed (or the previous sample was
/// incomplete, #1792), and one MiB keeps that worst case a fraction of the
/// 20 ms default cadence even when it is paid on every tick of a flapping
/// path.
pub const CONTENT_SAMPLE_LIMIT: u64 = 1024 * 1024;

/// #1739 test hook: content bytes actually read, keyed by path. The tests
/// assert that a metadata-identical sample skips the content read without
/// inferring it from timing; the key keeps parallel tests (each in its own
/// temp root) from observing each other's reads.
#[cfg(test)]
static CONTENT_READS: Mutex<Option<std::collections::HashMap<PathBuf, usize>>> = Mutex::new(None);

/// #1792 r1 test hook: the read operation a failure can be armed for. Every
/// `stat` and `read_dir` of a watched path goes through [`stat_path`] /
/// [`read_dir_path`], and the content read through [`read_should_fail`], so a
/// transient failure can be injected for exactly one operation without
/// touching the filesystem (no chmod, no mtime rewrite — those change metadata
/// and would mask the bug).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ReadOp {
    Stat,
    ReadDir,
    /// #1987 B6: one step of a `read_dir` iteration, distinct from opening
    /// the directory — the iterator can fail mid-listing (EMFILE-class) after
    /// yielding some entries, and a partial entry list must obscure the
    /// sample exactly like a failed open.
    ReadDirEntry,
    Content,
}

/// #1792 test hook: the next N reads of each (path, op) fail, keyed by path
/// for the same parallel-test isolation as [`CONTENT_READS`]; only armed in
/// test builds.
#[cfg(test)]
static READ_FAILURES: Mutex<Option<std::collections::HashMap<(PathBuf, ReadOp), usize>>> =
    Mutex::new(None);

#[cfg(test)]
fn content_reads(path: &Path) -> usize {
    lock(&CONTENT_READS)
        .as_ref()
        .and_then(|reads| reads.get(path).copied())
        .unwrap_or(0)
}

/// Whether this read is one of the N armed failures (#1792 test hook).
#[cfg(test)]
fn read_should_fail(path: &Path, op: ReadOp) -> bool {
    match lock(&READ_FAILURES)
        .as_mut()
        .and_then(|failures| failures.get_mut(&(path.to_path_buf(), op)))
    {
        Some(remaining) if *remaining > 0 => {
            *remaining -= 1;
            true
        }
        _ => false,
    }
}

#[cfg(not(test))]
fn read_should_fail(_path: &Path, _op: ReadOp) -> bool {
    false
}

/// Every `stat` of a watched path goes through here: one place for the
/// #1792 test hook to inject a transient failure.
fn stat_path(path: &Path) -> std::io::Result<std::fs::Metadata> {
    if read_should_fail(path, ReadOp::Stat) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "#1792 test hook",
        ));
    }
    std::fs::metadata(path)
}

/// The `read_dir` iteration of a watched directory (#1987 B6): every step
/// goes through the failure hook, so an iteration error — not only a failed
/// open — can be injected for tests. Zero-cost when no failures are armed.
struct WatchedDirEntries {
    path: PathBuf,
    entries: std::fs::ReadDir,
}

impl Iterator for WatchedDirEntries {
    type Item = std::io::Result<std::fs::DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.entries.next()?;
        if read_should_fail(&self.path, ReadOp::ReadDirEntry) {
            return Some(Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "#1987 test hook",
            )));
        }
        Some(entry)
    }
}

/// Every `read_dir` of a watched directory goes through here, like
/// [`stat_path`]; the returned iterator's steps do too (#1987 B6).
fn read_dir_path(path: &Path) -> std::io::Result<WatchedDirEntries> {
    if read_should_fail(path, ReadOp::ReadDir) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "#1792 test hook",
        ));
    }
    Ok(WatchedDirEntries {
        path: path.to_path_buf(),
        entries: std::fs::read_dir(path)?,
    })
}

/// Whether this content read is one of the N armed failures (#1792 test hook).
fn content_read_should_fail(path: &Path) -> bool {
    read_should_fail(path, ReadOp::Content)
}

/// Arms the next `failures` reads of `path`'s `op` to fail (#1792 test hook).
#[cfg(test)]
fn arm_read_failures(path: &Path, op: ReadOp, failures: usize) {
    lock(&READ_FAILURES)
        .get_or_insert_with(Default::default)
        .insert((path.to_path_buf(), op), failures);
}

/// Disarms any leftover failures for this (path, op) only (#1792 test hook).
/// Scoped to one key on purpose: the hook is process-global while libtest runs
/// tests in parallel, so wiping any wider here would erase a sibling test's
/// armed failures between its arm and its read and turn that test flaky.
#[cfg(test)]
fn clear_read_failures(path: &Path, op: ReadOp) {
    if let Some(failures) = lock(&READ_FAILURES).as_mut() {
        failures.remove(&(path.to_path_buf(), op));
    }
}

/// Arms the next `failures` content reads of `path` to fail (#1792 test hook).
#[cfg(test)]
fn arm_content_read_failures(path: &Path, failures: usize) {
    arm_read_failures(path, ReadOp::Content, failures);
}

/// Disarms any leftover content-read failures for `path` only (#1792 test
/// hook); see [`clear_read_failures`].
#[cfg(test)]
fn clear_content_read_failures(path: &Path) {
    clear_read_failures(path, ReadOp::Content);
}

/// #1987 A2 test hook: a one-shot write injected between the startup samples
/// of exactly one watched path — with the pre-fix two-sample startup, between
/// the metadata-only and the full sample, the window a write could land in and
/// be silently absorbed (the full sample took it in while the committed
/// metadata pre-dated it). Keyed to the path and consumed on its one fire so
/// libtest's parallel `sampled()` calls on other tests' paths cannot re-arm
/// or re-fire it.
#[cfg(test)]
static STARTUP_SAMPLE_WRITE: Mutex<Option<(PathBuf, Vec<u8>)>> = Mutex::new(None);

/// Fires [`STARTUP_SAMPLE_WRITE`] if a test armed it for `path` (#1987 test
/// hook).
#[cfg(test)]
fn startup_sample_gate(path: &Path) {
    let write = {
        let mut slot = lock(&STARTUP_SAMPLE_WRITE);
        match slot.clone() {
            Some((gate_path, bytes)) if gate_path == path => {
                *slot = None;
                Some((gate_path, bytes))
            }
            _ => None,
        }
    };
    if let Some((gate_path, bytes)) = write {
        std::fs::write(&gate_path, bytes).expect("#1987 A2 gate write");
    }
}

#[cfg(not(test))]
fn startup_sample_gate(_path: &Path) {}

/// #1920 test hook: a one-shot gate that makes exactly one store's watcher
/// task die by panic on its first poll — keyed to the store's `Inner`
/// allocation and consumed on its one fire, so libtest's parallel watchers
/// on other tests' stores can neither re-arm nor re-fire it (the same
/// per-store isolation as the other hooks above). It exists because
/// `WatchHandle::stop`'s panic arm needs a watcher that really died by panic,
/// and no public input produces one anymore: #1650 and #1736 clamp every
/// option that used to panic the loop.
#[cfg(test)]
static WATCH_LOOP_PANIC_GATE: Mutex<Option<usize>> = Mutex::new(None);

/// Arms the next watcher of this store to die by panic on its first poll
/// (#1920 test hook).
#[cfg(test)]
fn arm_watch_loop_panic(inner: &Arc<Inner>) {
    *lock(&WATCH_LOOP_PANIC_GATE) = Some(Arc::as_ptr(inner) as usize);
}

/// Fires [`WATCH_LOOP_PANIC_GATE`] if a test armed it for this store's
/// watcher (#1920 test hook).
#[cfg(test)]
fn watch_loop_panic_gate(inner: &Weak<Inner>) {
    let armed = {
        let mut slot = lock(&WATCH_LOOP_PANIC_GATE);
        match *slot {
            Some(key) if key == inner.as_ptr() as usize => {
                *slot = None;
                true
            }
            _ => false,
        }
    };
    if armed {
        panic!("#1920 test hook: simulated watch_loop failure");
    }
}

#[cfg(not(test))]
fn watch_loop_panic_gate(_inner: &Weak<Inner>) {}

struct WatchedPath {
    path: PathBuf,
    /// Whether [`path`] is sampled as a directory (by its entries); `None`
    /// while the startup probe failed (#1792 r1) — the first `changed()`
    /// re-probes instead of trusting a missing classification.
    directory: Option<bool>,
    /// For a directory watched in place of a missing config file, that
    /// file's name: its content is sampled once it appears.
    target: Option<std::ffi::OsString>,
    /// The last full sample (content included for the watched config file);
    /// `None` until a sample has been taken at all (#1792 r1: a startup
    /// probe failure defers sampling to the first `changed()`).
    fingerprint: Option<u64>,
    /// The last complete metadata-only sample (#1739): identical metadata
    /// skips the content read. `None` means the last sample was incomplete
    /// (#1792) — a read that failed — and the next tick must re-sample
    /// instead of short-circuiting on stale metadata. The one gap — a write
    /// that leaves every hashed field, ctime included, on the values already
    /// sampled, because ctime only records a tick — is spelled out in the
    /// module note and diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
    metadata_fingerprint: Option<u64>,
    /// #1792 r2: the folded sample of the last `Obscured` tick. A persistent
    /// obstruction — one unreadable sibling entry such as a `loop -> loop`
    /// symlink — obscures every sample, and without this slot the watcher
    /// would go silent for good while the config file itself stays readable
    /// (the baseline reported such changes and `fs.watch` reloads on them).
    /// It is compared only against other obscured samples, never committed
    /// into [`Self::fingerprint`], so a delete during an outage cannot be
    /// mistaken for "unchanged".
    obscured_fingerprint: Option<u64>,
    /// #1987 A1: the watched config file's own fingerprint from the last
    /// complete sample. The first obscured tick after a complete one has no
    /// folded obscured predecessor to compare against, so a change landing
    /// on that same tick — the obstruction starting and the target being
    /// rewritten together — would wait for the obstruction to clear before
    /// being reported. Comparing the target against this slot closes that
    /// window; only the target, because the obscured folded sample hashes
    /// every other entry from failed reads and is not comparable against the
    /// complete one. Later obscured ticks are covered by the folded chain.
    complete_target_fingerprint: Option<u64>,
}

impl WatchedPath {
    /// `existsSync(path) ? path : dirname(path)`, with `fs.watch` throwing
    /// (and the path being skipped) when that target does not exist — except
    /// that a stat failure must not be read as absence (#1792 r1): a
    /// transient EMFILE/EACCES/EIO at startup used to silently fall back to
    /// the parent or drop the path, closing detection for good. A failed
    /// stat keeps the path with an unknown mode for the first `changed()` to
    /// re-probe.
    fn new(path: PathBuf) -> Option<Self> {
        match stat_path(&path) {
            Ok(meta) => Some(Self::sampled(path, None, meta.is_dir())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let target = path.file_name().map(ToOwned::to_owned);
                let parent = path.parent()?.to_path_buf();
                match stat_path(&parent) {
                    Ok(meta) => Some(Self::sampled(parent, target, meta.is_dir())),
                    // The parent is missing too: `fs.watch` would throw and
                    // the baseline skips the path.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    // The parent exists but its mode is unknown; keep it.
                    Err(_) => Some(Self::probing(parent, target)),
                }
            }
            // The stat itself failed: keep the path rather than fall back to
            // the parent (or drop it).
            Err(_) => Some(Self::probing(path, None)),
        }
    }

    /// A watched path whose startup probe succeeded: sampled immediately,
    /// like every pre-r1 startup.
    fn sampled(path: PathBuf, target: Option<std::ffi::OsString>, directory: bool) -> Self {
        // #1987 A2: one sample, content included, and no metadata shortcut.
        // The startup used to take a metadata-only sample before the full one
        // and commit the *first* one's fingerprint, so a write landing
        // between the two was absorbed by the full sample while the committed
        // metadata kept differing — neither short-circuited around nor
        // reported, the D11 guard never reached the startup path. With nothing
        // committed the first tick re-samples in full (one extra bounded
        // content read per watched path, [`CONTENT_SAMPLE_LIMIT`]) and
        // compares against this sample, which the write preceded: it
        // reports. Swapping the two samples instead would leave the mirrored
        // window open (the write between them would land in the trailing
        // metadata, and the next tick short-circuits on it), so the commit
        // has to go regardless — and with it the #1819 double stat.
        let (fingerprint, status, target_sample) =
            fingerprint(&path, directory, target.as_deref(), true);
        // #1987 A2 test hook: fires right after the startup sample. Pre-fix
        // it fired between the metadata-only and the full sample; the
        // negative control restores that order around it.
        startup_sample_gate(&path);
        // #1792 r2: a startup sample that is already obscured commits the
        // folded sample to the obscured slot only, so the first complete
        // sample after recovery reports (there is no trustworthy previous
        // full sample to compare against).
        let (fingerprint, obscured_fingerprint) = if status == SampleStatus::Obscured {
            (None, Some(fingerprint))
        } else {
            (Some(fingerprint), None)
        };
        Self {
            path,
            directory: Some(directory),
            target,
            fingerprint,
            // #1792 keeps applying: an incomplete startup sample (any stat,
            // `read_dir`, or content read that failed) commits no metadata
            // fingerprint, and after #1987 A2 a complete one does not either.
            metadata_fingerprint: None,
            obscured_fingerprint,
            // #1987 A1: seed the target comparison slot from a complete
            // startup sample.
            complete_target_fingerprint: if status == SampleStatus::Complete {
                target_sample
            } else {
                None
            },
        }
    }

    /// A watched path whose startup probe failed (#1792 r1): the mode is
    /// unknown and nothing has been sampled yet.
    fn probing(path: PathBuf, target: Option<std::ffi::OsString>) -> Self {
        Self {
            path,
            directory: None,
            target,
            fingerprint: None,
            metadata_fingerprint: None,
            obscured_fingerprint: None,
            complete_target_fingerprint: None,
        }
    }

    /// #1792 r2: compares an obscured sample against the last obscured one —
    /// a change among the entries that *are* observable is real even while
    /// another entry stays unreadable. The first obscured tick after a
    /// complete one has no folded predecessor; for the watched config file
    /// itself that gap is closed by
    /// [`Self::observe_target_on_transition`] (#1987 A1).
    fn observe_obscured(&mut self, sample: u64) -> bool {
        let previous = self.obscured_fingerprint.replace(sample);
        previous.is_some_and(|previous| previous != sample)
    }

    /// #1987 A1: the first obscured sample after a complete one also compares
    /// the watched config file's own fingerprint against the last complete
    /// sample's ([`Self::complete_target_fingerprint`]) — the obstruction
    /// starting and the target changing on the same tick must not defer the
    /// report until the obstruction clears. Only the target: every other
    /// entry's contribution in the obscured folded sample is hashed from
    /// failed reads and is not comparable against the complete one, and
    /// later obscured ticks are already covered by the folded chain
    /// ([`Self::observe_obscured`]). A target that was not observed (its own
    /// stat failed) or a never-seeded slot reports nothing — nothing
    /// trustworthy was observed either way.
    fn observe_target_on_transition(&self, sample: Option<u64>) -> bool {
        match (self.complete_target_fingerprint, sample) {
            (Some(previous), Some(current)) => previous != current,
            _ => false,
        }
    }

    fn changed(&mut self) -> bool {
        // #1792 r2: a watched path can change type (a directory replaced by a
        // file, or the reverse). Without this the directory branch would hit
        // `read_dir` ENOTDIR on every tick, mark the sample obscured forever
        // and never report again. One cheap stat per tick keeps the mode in
        // step with the path (D15).
        if let Some(directory) = self.directory
            && let Ok(meta) = stat_path(&self.path)
            && meta.is_dir() != directory
        {
            self.directory = Some(meta.is_dir());
            self.metadata_fingerprint = None;
            // #1987 A1: the mode flip changes what the target's fingerprint
            // covers; a seed sampled in the other mode is not comparable.
            self.complete_target_fingerprint = None;
        }
        // #1792 r2: a startup probe that failed leaves the mode unknown;
        // re-probe every tick until one classifies it. A `NotFound` here is
        // not evidence of a file — the path may still appear as a directory
        // later — so the mode stays unknown and this tick samples the
        // absence as a file without ever short-circuiting (D10).
        let directory = match self.directory {
            Some(directory) => directory,
            None => match stat_path(&self.path) {
                Ok(meta) => {
                    let directory = meta.is_dir();
                    self.directory = Some(directory);
                    directory
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => {
                    self.metadata_fingerprint = None;
                    return false;
                }
            },
        };
        let unclassified = self.directory.is_none();
        // #1739: metadata first. At the default cadence (two watched paths
        // every 20 ms) the per-tick content read costs up to ~19% of a core
        // (measured: ~1.9 ms per 1 MiB read on cb8, page-cache hot; a
        // metadata-only sample is ~1 µs). Only a metadata change — or an
        // incomplete sample, #1792 — walks on to the content read; the single
        // gap — a write that leaves every hashed field, ctime included, on
        // the values already sampled — is spelled out in the module note and
        // diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
        let (next_metadata, metadata_status, _) =
            fingerprint(&self.path, directory, self.target.as_deref(), false);
        let (next, status, target_sample) = if metadata_status == SampleStatus::Obscured {
            // #1792 r2: a failed `stat` or `read_dir` observed nothing
            // trustworthy, so this folded sample is not evidence by itself —
            // but the folded sample of the previous obscured tick is the only
            // thing there is to compare against while the obstruction
            // persists (D16). Never commit it into `fingerprint`: it hashes a
            // failed stat exactly like a missing entry (D9).
            let (next, status, target_sample) =
                fingerprint(&self.path, directory, self.target.as_deref(), true);
            // #1987 A4: a mixed tick — the metadata sample failed but the
            // folded one succeeded — is a complete observation. Routing it
            // through the obscured path compared a complete folded sample
            // against the previous obscured one and left it as the obscured
            // chain's baseline, decoupling the watch from the last complete
            // fingerprint; it falls through to the complete path below
            // instead, compared against [`Self::fingerprint`] and committed
            // like any other complete sample.
            if status == SampleStatus::Obscured {
                self.metadata_fingerprint = None;
                // #1987 A1: the transition tick — no folded obscured
                // predecessor — still compares the target against the last
                // complete sample.
                let transition = self.obscured_fingerprint.is_none();
                let mut changed = self.observe_obscured(next);
                if transition {
                    changed |= self.observe_target_on_transition(target_sample);
                }
                return changed;
            }
            (next, status, target_sample)
        } else {
            if !unclassified
                && metadata_status == SampleStatus::Complete
                && self.metadata_fingerprint == Some(next_metadata)
            {
                return false;
            }
            fingerprint(&self.path, directory, self.target.as_deref(), true)
        };
        if status == SampleStatus::Obscured {
            // Same rule as the metadata branch above, reached when the
            // failure landed between the two samples.
            self.metadata_fingerprint = None;
            // #1987 A1: the transition tick — no folded obscured
            // predecessor — still compares the target against the last
            // complete sample.
            let transition = self.obscured_fingerprint.is_none();
            let mut changed = self.observe_obscured(next);
            if transition {
                changed |= self.observe_target_on_transition(target_sample);
            }
            return changed;
        }
        // #1792 r2: the config may have changed while the startup probe (and
        // every sample) was failing, so the first complete sample with no
        // trustworthy predecessor reports (D8): `None` is not "changed from
        // something" but "nothing trustworthy was ever sampled".
        self.obscured_fingerprint = None;
        let previous = self.fingerprint.replace(next);
        let changed = previous != Some(next);
        // #1987 A1: seed the target comparison slot from every complete
        // sample, so the next obstruction's transition tick has a baseline.
        if status == SampleStatus::Complete {
            self.complete_target_fingerprint = target_sample;
        }
        // #1792 r2: a sample that reports a change must not also commit the
        // short-circuit metadata — the two samples are taken at different
        // times, and a match on the first is not evidence the second saw the
        // same state (D11).
        self.metadata_fingerprint = if changed
            || unclassified
            || metadata_status != SampleStatus::Complete
            || status != SampleStatus::Complete
        {
            None
        } else {
            Some(next_metadata)
        };
        changed
    }
}

/// How much of one sample was actually observed (#1792 r1): what an
/// incomplete sample may report depends on which read failed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleStatus {
    /// Every read in the sample succeeded. A `NotFound` counts: it is a
    /// complete observation of absence, and the entry's appearance changes
    /// the hashed name list (so a dangling symlink must not keep the sample
    /// incomplete forever).
    Complete,
    /// Only the content read failed: the metadata was observed, the content
    /// was not.
    ContentUnknown,
    /// A `stat` or `read_dir` failed with something other than `NotFound`:
    /// the sample says nothing trustworthy about the path. An obscured sample
    /// commits nothing; it is compared against the previous obscured sample
    /// only (see [`WatchedPath::observe_obscured`], #1792 r2).
    Obscured,
}

impl SampleStatus {
    /// Combine one entry's status into a whole-sample status: an obscured
    /// entry hides everything; an unknown content read is the weakest
    /// incompleteness.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Complete, Self::Complete) => Self::Complete,
            (Self::Obscured, _) | (_, Self::Obscured) => Self::Obscured,
            _ => Self::ContentUnknown,
        }
    }
}

/// Hashes one path's metadata (and, when asked, content) into `hasher`.
/// Returns how much of this entry's sample was observed; an incomplete
/// return makes the whole sample incomplete, which [`WatchedPath::changed`]
/// turns into a next-tick retry (#1792) instead of a committed fingerprint.
///
/// #1987 A1: when `target_hasher` is set (the watched config file itself),
/// the same fields — and content, when read — are folded into it in
/// parallel, building the target's own fingerprint for
/// [`WatchedPath::observe_target_on_transition`]. A failed stat hashes into
/// the folded `hasher` only: nothing about the target was observed.
fn hash_entry(
    path: &Path,
    hasher: &mut DefaultHasher,
    target_hasher: Option<&mut DefaultHasher>,
    read_content: bool,
) -> SampleStatus {
    use std::{io::Read, os::unix::fs::MetadataExt};
    match stat_path(path) {
        Ok(meta) => {
            // The content read happens before any hashing so both hashers —
            // the folded one and, when set, the target's own (#1987 A1) — can
            // be fed the very same fields from the same read.
            let mut read: Option<Vec<u8>> = None;
            let read_attempted =
                read_content && meta.is_file() && meta.len() <= CONTENT_SAMPLE_LIMIT;
            if read_attempted {
                let mut content = Vec::new();
                read = if content_read_should_fail(path) {
                    // #1792 test hook: emulate a transient read failure
                    // without touching the file's metadata.
                    None
                } else {
                    std::fs::File::open(path)
                        .and_then(|file| file.take(CONTENT_SAMPLE_LIMIT).read_to_end(&mut content))
                        .ok()
                        .map(|_| content)
                };
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
            }
            let hash_fields = |hasher: &mut DefaultHasher| {
                true.hash(hasher);
                meta.len().hash(hasher);
                meta.is_dir().hash(hasher);
                meta.modified().ok().hash(hasher);
                // Every write (and `utimes`) bumps ctime, so a same-length
                // rewrite that restores mtime is normally still seen (a
                // replacing rename changes the inode too); the
                // tick-granularity gap and the other fields it needs are in
                // the module note and
                // diffs/A6.3-CONFIG-METADATA-SHORTCUT-001.
                (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);
                if read_attempted {
                    read.hash(hasher);
                }
            };
            hash_fields(hasher);
            if let Some(target_hasher) = target_hasher {
                hash_fields(target_hasher);
            }
            if read_attempted && read.is_none() {
                SampleStatus::ContentUnknown
            } else {
                SampleStatus::Complete
            }
        }
        Err(error) => {
            false.hash(hasher);
            // #1792 r1: a missing entry is a complete observation — it is not
            // there, and its appearance changes the hashed name list. Only a
            // failure that can hide content (permission, descriptor, I/O)
            // makes the sample incomplete and worth retrying next tick.
            if error.kind() == std::io::ErrorKind::NotFound {
                SampleStatus::Complete
            } else {
                SampleStatus::Obscured
            }
        }
    }
}

/// One sample of a watched path. Returns the fingerprint, how much of the
/// sample was observed (an incomplete sample must not commit a metadata
/// fingerprint, #1792), and — for a directory watch whose content is sampled
/// — the watched config file's own fingerprint (#1987 A1). `None` for that
/// third element when the target was not observed (its read failed, or the
/// listing itself failed before reaching it) or when only metadata was
/// sampled; a target absent from a successfully read listing hashes as the
/// absence it is (a complete observation, like a vanished directory, D17).
fn fingerprint(
    path: &Path,
    directory: bool,
    target: Option<&std::ffi::OsStr>,
    sample_content: bool,
) -> (u64, SampleStatus, Option<u64>) {
    let mut hasher = DefaultHasher::new();
    let mut status = SampleStatus::Complete;
    // #1987 A1: the target's own fingerprint is folded in parallel with the
    // whole-directory one, by the same fields, so the two are comparable.
    let mut target_hasher = DefaultHasher::new();
    // `Some` once the target entry itself was sampled — its status decides
    // whether the parallel fold observed anything trustworthy.
    let mut target_status: Option<SampleStatus> = None;
    let mut listing_obscured = false;
    if directory {
        // The directory's own metadata changes on every entry create/delete.
        status = status.merge(hash_entry(path, &mut hasher, None, false));
        let mut names = Vec::new();
        match read_dir_path(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => names.push(entry.file_name()),
                        // A partial entry list would silently drop names.
                        Err(_) => listing_obscured = true,
                    }
                }
            }
            Err(error) => {
                // #1792 r1: a vanished directory is a complete, empty
                // observation; any other failure hides the entry list.
                if error.kind() != std::io::ErrorKind::NotFound {
                    listing_obscured = true;
                }
            }
        }
        if listing_obscured {
            status = status.merge(SampleStatus::Obscured);
        }
        names.sort();
        for name in names {
            name.hash(&mut hasher);
            let read_content = sample_content && target == Some(name.as_os_str());
            let entry_status = if read_content {
                let status = hash_entry(
                    &path.join(&name),
                    &mut hasher,
                    Some(&mut target_hasher),
                    true,
                );
                target_status = Some(status);
                status
            } else {
                hash_entry(&path.join(&name), &mut hasher, None, false)
            };
            status = status.merge(entry_status);
        }
    } else {
        status = status.merge(hash_entry(path, &mut hasher, None, sample_content));
    }
    // #1987 A1: only a fully observed target — or a fully read listing that
    // does not contain it — yields a comparable fingerprint. A partial
    // listing that has not reached the target observed nothing about it, and
    // an unobserved one (its stat failed) is hashed into the folded sample
    // exactly like a missing entry there, which is not comparable evidence.
    let target_sample = if directory && sample_content {
        match target_status {
            Some(SampleStatus::Complete) => Some(target_hasher.finish()),
            None if !listing_obscured => Some(target_hasher.finish()),
            _ => None,
        }
    } else {
        None
    };
    (hasher.finish(), status, target_sample)
}

async fn watch_loop(
    inner: Weak<Inner>,
    mut paths: Vec<WatchedPath>,
    options: WatchOptions,
    mut changes: mpsc::UnboundedReceiver<()>,
) {
    // #1920 test hook: kill exactly the armed store's watcher by panic.
    watch_loop_panic_gate(&inner);
    // #1650: a zero/sub-floor period would panic this detached task and stop
    // hot reload silently; see [`MIN_POLL_INTERVAL`]. #1736: an over-ceiling
    // period would strand the ticker at `Instant::far_future` after its first
    // tick, and an over-ceiling debounce would overflow `Instant + Duration`
    // here — both stop hot reload with the handle still "alive"; see
    // [`MAX_POLL_INTERVAL`] / [`MAX_DEBOUNCE`] and [`deadline_after`].
    let mut ticker = tokio::time::interval(effective_poll_interval(&options));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    let debounce = effective_debounce(&options);
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
                deadline = Some(deadline_after(Instant::now(), debounce));
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
                    deadline = Some(deadline_after(Instant::now(), debounce));
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
