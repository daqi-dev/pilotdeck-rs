//! IM channel mapper persistence (`ChannelStatePersistence.ts:14-104`).
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const DEFAULT_DEBOUNCE_MS: u64 = 2000;

struct PendingWrite {
    cancelled: Arc<AtomicBool>,
}

pub struct ChannelStatePersistence {
    state_dir: PathBuf,
    debounce_ms: u64,
    dirty: Mutex<HashMap<String, Value>>,
    pending: Mutex<HashMap<String, PendingWrite>>,
}

impl ChannelStatePersistence {
    pub fn new(state_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            state_dir,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            dirty: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn file_path(&self, channel_key: &str) -> PathBuf {
        self.state_dir.join(format!("{channel_key}.state.json"))
    }

    pub fn load(&self, channel_key: &str) -> Option<Value> {
        let path = self.file_path(channel_key);
        std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    pub fn save(self: &Arc<Self>, channel_key: &str, state: Value) {
        self.dirty
            .lock()
            .unwrap()
            .insert(channel_key.to_owned(), state);
        let mut pending = self.pending.lock().unwrap();
        if let Some(existing) = pending.remove(channel_key) {
            existing.cancelled.store(true, Ordering::SeqCst);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(self);
        let key = channel_key.to_owned();
        let debounce_ms = self.debounce_ms;
        let flag = cancelled.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(debounce_ms));
            if flag.load(Ordering::SeqCst) {
                return;
            }
            if let Some(store) = weak.upgrade() {
                store.flush_key(&key);
            }
        });
        pending.insert(
            channel_key.to_owned(),
            PendingWrite {
                cancelled,
            },
        );
    }

    pub fn flush(self: &Arc<Self>) {
        let keys = self
            .pending
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(existing) = self.pending.lock().unwrap().remove(&key) {
                existing.cancelled.store(true, Ordering::SeqCst);
            }
            self.flush_key(&key);
        }
    }

    fn flush_key(&self, channel_key: &str) {
        let state = self.dirty.lock().unwrap().remove(channel_key);
        let Some(state) = state else {
            return;
        };
        if let Err(error) = write_state(&self.file_path(channel_key), &state) {
            eprintln!("[channels] failed to persist {channel_key} state: {error}");
        }
    }

    pub fn saver(self: &Arc<Self>, channel_key: &'static str) -> Arc<dyn Fn(Value) + Send + Sync> {
        let store = self.clone();
        Arc::new(move |state| store.save(channel_key, state))
    }
}

fn write_state(path: &Path, state: &Value) -> std::io::Result<()> {
    let parent = path.parent().expect("channel state directory");
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.state.{}.tmp",
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("channel"),
        uuid::Uuid::new_v4().as_simple()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn debounced_save_writes_channel_key_state_json() {
        let root = std::env::temp_dir().join(format!("pdrs-channel-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = ChannelStatePersistence::new(root.clone());
        store.save(
            "telegram",
            json!({"activeByChatId":{"42":"telegram:chat=42:general"},"projectByChatId":{}}),
        );
        assert!(!store.file_path("telegram").exists());
        thread::sleep(Duration::from_millis(2100));
        assert!(store.file_path("telegram").exists());
        store.save(
            "telegram",
            json!({"activeByChatId":{"42":"telegram:chat=42:s_new"},"projectByChatId":{}}),
        );
        thread::sleep(Duration::from_millis(2100));
        let saved: Value =
            serde_json::from_slice(&std::fs::read(store.file_path("telegram")).unwrap()).unwrap();
        assert_eq!(
            saved["activeByChatId"]["42"].as_str().unwrap(),
            "telegram:chat=42:s_new"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn second_save_within_debounce_resets_timer() {
        let root = std::env::temp_dir().join(format!("pdrs-channel-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = ChannelStatePersistence::new(root.clone());
        store.save(
            "telegram",
            json!({"activeByChatId":{"42":"first"},"projectByChatId":{}}),
        );
        thread::sleep(Duration::from_millis(1500));
        store.save(
            "telegram",
            json!({"activeByChatId":{"42":"second"},"projectByChatId":{}}),
        );
        thread::sleep(Duration::from_millis(1500));
        assert!(
            !store.file_path("telegram").exists(),
            "first timer must not fire after reset"
        );
        thread::sleep(Duration::from_millis(700));
        let saved: Value =
            serde_json::from_slice(&std::fs::read(store.file_path("telegram")).unwrap()).unwrap();
        assert_eq!(saved["activeByChatId"]["42"].as_str().unwrap(), "second");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn flush_cancels_debounce_and_writes_immediately() {
        let root = std::env::temp_dir().join(format!("pdrs-channel-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = ChannelStatePersistence::new(root.clone());
        store.save(
            "telegram",
            json!({"activeByChatId":{"42":"flushed"},"projectByChatId":{}}),
        );
        let started = std::time::Instant::now();
        store.flush();
        assert!(store.file_path("telegram").exists());
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "flush must not wait for debounce"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
