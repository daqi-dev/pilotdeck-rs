//! Port of baseline `src/adapters/channel/protocol/ChannelStatePersistence.ts`
//! (debounced, atomic mapper-state persistence per channel key).
//!
//! TS `save` arms a per-key `setTimeout(debounceMs)`; `flush` clears all
//! timers and awaits a write for every dirty or in-flight key. Writes drain
//! while the key stays dirty, land through a `.tmp` + rename, and serialize
//! with `JSON.stringify(state, null, 2)` (byte-identical to Node, including
//! the empty-container one-liners).
use pilotdeck_protocol::{JsString, JsValue, parse_json, quote_json_string, stringify_json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

/// TS default `debounceMs = 2000`.
pub const DEFAULT_DEBOUNCE_MS: u64 = 2000;

#[derive(Default)]
struct Inner {
    timers: HashMap<JsString, JoinHandle<()>>,
    /// Sequence of the live debounce timer per key; every `save` bumps it.
    ///
    /// `clearTimeout` cannot be expressed with `JoinHandle::abort`: abort only
    /// takes effect at the task's next yield point, and a timer that already
    /// woke up spends its whole synchronous section (deleting the *new* timer
    /// handle, registering `in_flight`, claiming the newest `dirty` state)
    /// before its first await. The cancel then lands inside `write_to_disk`,
    /// after the state left `dirty`: the write is lost and `in_flight` keeps
    /// an entry nobody completes. Each timer instead carries the sequence it
    /// was armed with and retires itself, untouched, when it is stale.
    timer_seq: HashMap<JsString, u64>,
    dirty: HashMap<JsString, JsValue>,
    /// The live drain per key. The payload is the drain's outcome once it
    /// settles (`None` while it runs): a waiter gets the same result the
    /// drainer did — TS hands the waiter the drainer's very promise, so a
    /// failed write rejects every awaiter of it, not just the starter.
    in_flight: HashMap<JsString, watch::Sender<Option<Result<(), String>>>>,
}

struct Shared {
    state_dir: PathBuf,
    debounce: Duration,
    inner: Mutex<Inner>,
    #[cfg(test)]
    hooks: TestHooks,
}

/// Test-only rendezvous points. The `#2073` window cannot be hit reliably by
/// sleeping, so tests pin it: the next debounce task parks after `sleep`
/// returns and before it takes `inner` — the stretch where `JoinHandle::abort`
/// is recorded but cannot land yet — and every task counts its exit so the
/// test knows when the superseded task has settled. Production builds do not
/// contain any of this.
#[cfg(test)]
#[derive(Default)]
struct TestHooks {
    wake_gate: Mutex<Option<Arc<TestWakeGate>>>,
    drain_gate: Mutex<Option<Arc<DrainGate>>>,
    timer_exits: std::sync::atomic::AtomicUsize,
}

/// Parks the first drain right after it claimed the dirty state, so a test can
/// cancel it mid-write and observe how the in-flight entry and the claimed
/// state are recovered.
#[cfg(test)]
pub(crate) struct DrainGate {
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[cfg(test)]
pub(crate) struct TestWakeGate {
    claimed: std::sync::atomic::AtomicBool,
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    released: Mutex<bool>,
    resume: std::sync::Condvar,
}

#[cfg(test)]
impl TestWakeGate {
    /// Parks the first debounce task that wakes after the gate was armed.
    fn park(&self) {
        if self.claimed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let sender = self
            .entered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
        let mut released = self.released.lock().unwrap_or_else(|e| e.into_inner());
        while !*released {
            released = self
                .resume
                .wait(released)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.resume.notify_all();
    }
}

#[cfg(test)]
struct TestExitGuard(Arc<Shared>);

#[cfg(test)]
impl Drop for TestExitGuard {
    fn drop(&mut self) {
        self.0
            .hooks
            .timer_exits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Shared {
    fn park_timer_if_armed(&self) {
        let gate = self
            .hooks
            .wake_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(gate) = gate {
            gate.park();
        }
    }

    /// Takes the drain gate, so only the first drain after `park_next_drain`
    /// is parked; the retry a waiter starts runs straight through.
    fn take_drain_gate(&self) -> Option<Arc<DrainGate>> {
        self.hooks
            .drain_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// Handle is cheap to clone; all state lives behind one lock, like the TS
/// object's private fields.
#[derive(Clone)]
pub struct ChannelStatePersistence {
    shared: Arc<Shared>,
}

impl ChannelStatePersistence {
    pub fn new(state_dir: PathBuf) -> Self {
        Self::with_debounce(state_dir, DEFAULT_DEBOUNCE_MS)
    }

    /// TS `debounceMs` override (used by tests to shorten the window).
    pub fn with_debounce(state_dir: PathBuf, debounce_ms: u64) -> Self {
        Self {
            shared: Arc::new(Shared {
                state_dir,
                debounce: Duration::from_millis(debounce_ms),
                inner: Mutex::new(Inner::default()),
                #[cfg(test)]
                hooks: TestHooks::default(),
            }),
        }
    }

    /// Arms the next debounce task to park after it wakes up and before it
    /// takes the state lock; the receiver fires once it is parked.
    #[cfg(test)]
    pub(crate) fn park_next_timer(&self) -> std::sync::mpsc::Receiver<()> {
        let (sender, receiver) = std::sync::mpsc::channel();
        *self
            .shared
            .hooks
            .wake_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(TestWakeGate {
            claimed: std::sync::atomic::AtomicBool::new(false),
            entered: Mutex::new(Some(sender)),
            released: Mutex::new(false),
            resume: std::sync::Condvar::new(),
        }));
        receiver
    }

    /// Releases the parked debounce task.
    #[cfg(test)]
    pub(crate) fn release_parked_timer(&self) {
        let gate = self
            .shared
            .hooks
            .wake_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(gate) = gate {
            gate.release();
        }
    }

    /// How many debounce tasks have settled (finished or been dropped).
    #[cfg(test)]
    pub(crate) fn timer_exits(&self) -> usize {
        self.shared
            .hooks
            .timer_exits
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Arms the next drain to park after it claimed the dirty state.
    #[cfg(test)]
    pub(crate) fn park_next_drain(&self) -> Arc<DrainGate> {
        let gate = Arc::new(DrainGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *self
            .shared
            .hooks
            .drain_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(gate.clone());
        gate
    }

    /// How many tasks are waiting on the key's in-flight entry.
    #[cfg(test)]
    pub(crate) fn in_flight_waiters(&self, channel_key: &JsString) -> usize {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
            .get(channel_key)
            .map_or(0, watch::Sender::receiver_count)
    }

    /// `load` — missing file or invalid JSON yields `None`.
    pub async fn load(&self, channel_key: &JsString) -> Option<JsValue> {
        let raw = tokio::fs::read_to_string(self.file_path(channel_key))
            .await
            .ok()?;
        parse_json(&raw).ok()
    }

    /// `save` — records the latest state and (re)arms the debounce timer.
    pub fn save(&self, channel_key: &JsString, state: JsValue) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.dirty.insert(channel_key.clone(), state);
        // `clearTimeout`: retire the pending timer by bumping the sequence (see
        // `Inner::timer_seq`). The old handle is dropped, not aborted — a timer
        // that already woke up must finish its synchronous section, and it is
        // the sequence check inside that section that retires it.
        let seq = {
            let seq = inner.timer_seq.entry(channel_key.clone()).or_insert(0);
            *seq += 1;
            *seq
        };
        inner.timers.remove(channel_key);
        let shared = self.clone();
        let key = channel_key.clone();
        let handle = tokio::spawn(async move {
            #[cfg(test)]
            let _exit = TestExitGuard(shared.shared.clone());
            tokio::time::sleep(shared.shared.debounce).await;
            #[cfg(test)]
            shared.shared.park_timer_if_armed();
            {
                let mut inner = shared
                    .shared
                    .inner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Superseded by a newer `save` (or by `flush`): return without
                // deleting the live timer handle, claiming `dirty`, or touching
                // `in_flight`. The live timer writes every queued state.
                if inner.timer_seq.get(&key) != Some(&seq) {
                    return;
                }
                inner.timers.remove(&key);
            }
            // The debounced path logs and swallows, like the TS `.catch`.
            // #2558: the log line is best-effort too (std panics when a raw
            // console write fails); a panic here would kill the debounce task
            // after it already did the timer bookkeeping above.
            if let Err(error) = shared.ensure_write(&key).await {
                crate::native_process::channels::console_log_line(
                    &mut std::io::stderr(),
                    &format!(
                        "[channels] failed to persist {} state: {error}",
                        String::from_utf16_lossy(key.units())
                    ),
                );
            }
        });
        inner.timers.insert(channel_key.clone(), handle);
    }

    /// `flush` — cancels pending timers and awaits a drained write for every
    /// dirty or in-flight key. Returns the first write error in key order,
    /// not `Promise.all`'s temporally first rejection — a registered
    /// intentional deviation (`A8.3-CH-WEIXIN-FLUSH-001`).
    pub async fn flush(&self) -> Result<(), String> {
        {
            let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            // `clearTimeout` for every pending timer. The handles are dropped
            // without aborting; bumping every sequence is what retires a timer
            // that has already woken up (`Inner::timer_seq`).
            inner.timers.clear();
            for seq in inner.timer_seq.values_mut() {
                *seq += 1;
            }
        }
        let mut keys: Vec<JsString> = {
            let inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .dirty
                .keys()
                .chain(inner.in_flight.keys())
                .cloned()
                .collect()
        };
        keys.sort_by(|a, b| a.units().cmp(b.units()));
        keys.dedup();
        // TS `Promise.all(keys.map((key) => this.ensureWrite(key)))`: every
        // drain starts before the first await settles (each future's first
        // poll runs the lock section that claims the key), and one key's
        // rejection neither cancels nor starves the others. A sequential `?`
        // here would leave the keys after the failure unwritten — the P3
        // probe pinned the TS shape (`every key attempted before any
        // rejection: true`). The error is the first one in key order, the
        // deterministic stand-in for `Promise.all`'s first temporal
        // rejection — a registered intentional deviation, not an
        // equivalence claim (`A8.3-CH-WEIXIN-FLUSH-001`: the TS flush
        // rejects with the temporally first of two failing keys).
        let outcomes =
            futures_util::future::join_all(keys.iter().map(|key| self.ensure_write(key))).await;
        for outcome in outcomes {
            outcome?;
        }
        Ok(())
    }

    /// `ensureWrite` — joins the in-flight drain for the key, or starts one.
    async fn ensure_write(&self, channel_key: &JsString) -> Result<(), String> {
        loop {
            let waiter = {
                let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                match inner.in_flight.get(channel_key) {
                    Some(done) => Some(done.subscribe()),
                    None => {
                        inner
                            .in_flight
                            .insert(channel_key.clone(), watch::Sender::new(None));
                        None
                    }
                }
            };
            let Some(mut waiter) = waiter else { break };
            // The entry is removed by the drainer's guard, however its task
            // ends. A settled payload is the drainer's outcome — the waiter
            // observes the same failure the drainer did, as the TS waiter
            // (handed the drainer's own promise) does. A closed sender means
            // that drainer was dropped before it settled anything: start a
            // fresh drain instead of reporting a result that never reached
            // the disk (and never hangs).
            match waiter.wait_for(|done| done.is_some()).await {
                Ok(outcome) => {
                    return outcome
                        .clone()
                        .expect("#1725 the wait_for predicate guarantees a payload");
                }
                Err(_) => continue,
            }
        }
        let mut guard = InFlightGuard {
            shared: self.shared.as_ref(),
            channel_key,
            outcome: None,
        };
        let result = self.drain_writes(channel_key).await;
        guard.outcome = Some(result.clone());
        drop(guard);
        result
    }

    /// `drainWrites` — keeps writing while the key keeps being marked dirty.
    async fn drain_writes(&self, channel_key: &JsString) -> Result<(), String> {
        loop {
            let state = {
                let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.dirty.remove(channel_key)
            };
            let Some(state) = state else {
                return Ok(());
            };
            // TS removes the entry before writing and lets a failed write drop
            // it; cancellation has no JS counterpart, and dropping the claimed
            // state would be a silent lost write, so `ClaimedState` puts it
            // back unless the future reached the write.
            let mut claim = ClaimedState {
                shared: self.shared.as_ref(),
                channel_key,
                state: Some(state),
            };
            #[cfg(test)]
            if let Some(gate) = self.shared.take_drain_gate() {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            let result = self
                .write_to_disk(
                    channel_key,
                    claim.state.as_ref().expect("#2073 claimed state"),
                )
                .await;
            claim.state = None;
            result?;
        }
    }

    /// `writeToDisk` — atomic tmp + rename; the tmp file is removed on error.
    async fn write_to_disk(&self, channel_key: &JsString, state: &JsValue) -> Result<(), String> {
        let file = self.file_path(channel_key);
        let dir = &self.shared.state_dir;
        if let Err(error) = tokio::fs::create_dir_all(dir).await {
            return Err(error.to_string());
        }
        let temporary = dir.join(format!(
            ".{}.state.{}.tmp",
            String::from_utf16_lossy(channel_key.units()),
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        let write = tokio::fs::write(&temporary, stringify_pretty(state)).await;
        if let Err(error) = write {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.to_string());
        }
        if let Err(error) = tokio::fs::rename(&temporary, &file).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.to_string());
        }
        Ok(())
    }

    fn file_path(&self, channel_key: &JsString) -> PathBuf {
        self.shared.state_dir.join(format!(
            "{}.state.json",
            String::from_utf16_lossy(channel_key.units())
        ))
    }
}

/// Clears the key's in-flight entry however the drain ends — the TS
/// `.finally(() => inFlight.delete(key))`. A drainer that returns (even with a
/// write error) settles its waiters on that outcome; one whose task is dropped
/// mid-write settles nothing, so the senders close and the waiters can start a
/// fresh drain instead of reporting a result that never reached the disk.
struct InFlightGuard<'a> {
    shared: &'a Shared,
    channel_key: &'a JsString,
    /// The drain's outcome, set once `drainWrites` returned; a dropped future
    /// leaves it `None`, so its waiters restart instead of inheriting a
    /// result that never happened.
    outcome: Option<Result<(), String>>,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        let sender = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
            .remove(self.channel_key);
        if let (Some(sender), Some(outcome)) = (sender, self.outcome.take()) {
            let _ = sender.send_replace(Some(outcome));
        }
    }
}

/// A state taken out of `dirty` for writing. `drainWrites` removes the entry
/// before the write (as the TS original does); JS has no cancellation, so a
/// future dropped mid-write has no TS counterpart, and the claimed state is
/// put back instead of being lost (a newer `save` keeps its own value).
struct ClaimedState<'a> {
    shared: &'a Shared,
    channel_key: &'a JsString,
    state: Option<JsValue>,
}

impl Drop for ClaimedState<'_> {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        self.shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dirty
            .entry(self.channel_key.clone())
            .or_insert(state);
    }
}

/// `JSON.stringify(value, null, 2)` for the persisted state and credentials
/// files: objects and arrays print one element per line with two-space
/// indents, empty containers stay on one line, `undefined`-valued keys are
/// omitted.
pub(crate) fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let child_indent = "  ".repeat(depth + 1);
    match value {
        JsValue::Object(object) => {
            let entries: Vec<_> = object
                .entries()
                .iter()
                .filter(|(_, value)| !matches!(value, JsValue::Undefined))
                .collect();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, entry)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&child_indent);
                out.push_str(&quote_json_string(key));
                out.push_str(": ");
                write_pretty(entry, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&indent);
            out.push('}');
        }
        JsValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&child_indent);
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&indent);
            out.push(']');
        }
        JsValue::String(text) => out.push_str(&quote_json_string(text)),
        // Scalars reuse the compact encoder's exact JS text (numbers keep
        // their ECMAScript formatting, `undefined` prints `null`).
        other => out.push_str(&match stringify_json(other) {
            Ok(Some(text)) => text,
            Ok(None) | Err(_) => "null".into(),
        }),
    }
}
