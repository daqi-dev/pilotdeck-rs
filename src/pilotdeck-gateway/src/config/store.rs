//! Live configuration store, ported from the fixed TypeScript baseline's
//! `src/pilot/config/PilotConfigStore.ts` (`DefaultPilotConfigStore`).
//!
//! * `reload` calls made while a reload is in flight share it: they all
//!   resolve to the same snapshot (or the same error) and only one event is
//!   published. A listener that calls `reload` during publication also joins
//!   the in-flight reload, as in the baseline.
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
//!   change-time sampling this needs a filesystem whose ctime granularity is
//!   coarser than the burst.
//!
//! Each sample reads at most [`CONTENT_SAMPLE_LIMIT`] bytes of a watched
//! config file (the file itself, or the missing config file's name inside a
//! watched parent directory); larger files and other directory entries are
//! sampled by metadata only.
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
    sync::{Arc, Mutex, MutexGuard, Weak},
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
    pub debounce: Duration,
    /// How often watched paths are checked; stands in for `fs.watch` events.
    /// Kept well below `debounce` so the sampling merge window stays small.
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
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    /// load itself runs on the first poll of any sharing future.
    pub fn reload(&self, _reason: &str) -> impl Future<Output = ReloadResult> + Send + 'static {
        let flight = lock(&self.inner.reloading)
            .get_or_insert_with(|| Arc::new(Flight::default()))
            .clone();
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

    /// `startWatching`. Must be called inside a Tokio runtime. The returned
    /// handle stops watching when dropped or via [`WatchHandle::stop`].
    pub fn start_watching(&self, options: WatchOptions) -> WatchHandle {
        let paths = self
            .watched_paths()
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
        WatchHandle {
            task: Some(task),
            changes: sender,
        }
    }

    /// `getWatchedPaths`: the global config path and, with a project, the
    /// project config path, deduplicated in that order.
    fn watched_paths(&self) -> Vec<PathBuf> {
        let options = &self.inner.options;
        let cwd = super::absolute_cwd(&options.cwd);
        // The loader reads the global path through `metadata::resolve(cwd, ..)`;
        // watch the same file (a relative `PILOTDECK_CONFIG_PATH` is relative
        // to `cwd`, and an empty one is `cwd` itself).
        let mut paths = vec![crate::metadata::resolve(
            &cwd,
            &super::global_config_path(&options.pilot_home, &options.env),
        )];
        if let Some(project) = super::project_config_path(&cwd, options.project.as_deref())
            && !paths.contains(&project)
        {
            paths.push(project);
        }
        paths
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
    Running,
    Done(ReloadResult),
}

#[derive(Default)]
struct Flight {
    state: Mutex<FlightState>,
    done: Notify,
}

impl Flight {
    async fn run_or_wait(self: &Arc<Self>, inner: &Arc<Inner>) -> ReloadResult {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = lock(&self.state);
                match &*state {
                    FlightState::Done(result) => return result.clone(),
                    FlightState::Running => {}
                    FlightState::Pending => {
                        *state = FlightState::Running;
                        drop(state);
                        return self.execute(inner);
                    }
                }
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

struct WatchedPath {
    path: PathBuf,
    directory: bool,
    /// For a directory watched in place of a missing config file, that
    /// file's name: its content is sampled once it appears.
    target: Option<std::ffi::OsString>,
    fingerprint: u64,
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
        let fingerprint = fingerprint(&path, directory, target.as_deref());
        Some(Self {
            path,
            directory,
            target,
            fingerprint,
        })
    }

    fn changed(&mut self) -> bool {
        let next = fingerprint(&self.path, self.directory, self.target.as_deref());
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
            // that restores mtime is still seen; a replacing rename changes
            // the inode.
            (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);
            if read_content && meta.is_file() && meta.len() <= CONTENT_SAMPLE_LIMIT {
                let mut content = Vec::new();
                std::fs::File::open(path)
                    .and_then(|file| file.take(CONTENT_SAMPLE_LIMIT).read_to_end(&mut content))
                    .ok()
                    .map(|_| content)
                    .hash(hasher);
            }
        }
        Err(_) => false.hash(hasher),
    }
}

fn fingerprint(path: &Path, directory: bool, target: Option<&std::ffi::OsStr>) -> u64 {
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
            let read_content = target == Some(name.as_os_str());
            hash_entry(&path.join(&name), &mut hasher, read_content);
        }
    } else {
        hash_entry(path, &mut hasher, true);
    }
    hasher.finish()
}

async fn watch_loop(
    inner: Weak<Inner>,
    mut paths: Vec<WatchedPath>,
    options: WatchOptions,
    mut changes: mpsc::UnboundedReceiver<()>,
) {
    let mut ticker = tokio::time::interval(options.poll_interval);
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
                deadline = Some(Instant::now() + options.debounce);
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
                    deadline = Some(Instant::now() + options.debounce);
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
