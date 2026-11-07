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
    dirty: HashMap<JsString, JsValue>,
    in_flight: HashMap<JsString, watch::Sender<bool>>,
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
    timer_exits: std::sync::atomic::AtomicUsize,
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
        if self
            .claimed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
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
            released = self.resume.wait(released).unwrap_or_else(|e| e.into_inner());
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
        if let Some(timer) = inner.timers.remove(channel_key) {
            timer.abort();
        }
        let shared = self.clone();
        let key = channel_key.clone();
        let handle = tokio::spawn(async move {
            #[cfg(test)]
            let _exit = TestExitGuard(shared.shared.clone());
            tokio::time::sleep(shared.shared.debounce).await;
            #[cfg(test)]
            shared.shared.park_timer_if_armed();
            shared
                .shared
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .timers
                .remove(&key);
            // The debounced path logs and swallows, like the TS `.catch`.
            if let Err(error) = shared.ensure_write(&key).await {
                eprintln!(
                    "[channels] failed to persist {} state: {error}",
                    String::from_utf16_lossy(key.units())
                );
            }
        });
        inner.timers.insert(channel_key.clone(), handle);
    }

    /// `flush` — cancels pending timers and awaits a drained write for every
    /// dirty or in-flight key. Returns the first write error, as the TS
    /// `Promise.all` rejection does.
    pub async fn flush(&self) -> Result<(), String> {
        {
            let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            for (_, timer) in inner.timers.drain() {
                timer.abort();
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
        for key in &keys {
            // TS rejects all awaiters with the same error; only the caller that
            // started the drain observes it here. Failure-path detail outside
            // the golden surface (writes do not fail in the replayed cases).
            self.ensure_write(key).await?;
        }
        Ok(())
    }

    /// `ensureWrite` — joins the in-flight drain for the key, or starts one.
    async fn ensure_write(&self, channel_key: &JsString) -> Result<(), String> {
        let waiter = {
            let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(done) = inner.in_flight.get(channel_key) {
                Some(done.subscribe())
            } else {
                inner
                    .in_flight
                    .insert(channel_key.clone(), watch::Sender::new(false));
                None
            }
        };
        if let Some(mut waiter) = waiter {
            let _ = waiter.wait_for(|done| *done).await;
            return Ok(());
        }
        let result = self.drain_writes(channel_key).await;
        let sender = self
            .shared
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
            .remove(channel_key);
        if let Some(sender) = sender {
            let _ = sender.send_replace(true);
        }
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
            self.write_to_disk(channel_key, &state).await?;
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
