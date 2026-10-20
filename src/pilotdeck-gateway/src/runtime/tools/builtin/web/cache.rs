//! URL content cache for `web_fetch` (baseline
//! `tool/builtin/web/urlContentCache.ts`, W8): an LRU keyed by the URL as the
//! model wrote it, a 15-minute TTL checked lazily on read, and a 50 MiB budget
//! over each entry's content size (`Math.max(1, contentBytes)`).
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub const CACHE_TTL_MS: f64 = 15.0 * 60.0 * 1000.0;
pub const MAX_CACHE_SIZE_BYTES: usize = 50 * 1024 * 1024;

/// `FetchedCacheEntry` (the persisted-binary fields are never set: W10 is a
/// stub in the baseline too).
#[derive(Clone, Debug, PartialEq)]
pub struct FetchedEntry {
    pub bytes: usize,
    pub code: u16,
    pub code_text: String,
    pub content: String,
    pub content_type: String,
}

struct Node {
    url: String,
    entry: FetchedEntry,
    size: usize,
    expires_at: f64,
}

/// `Date.now()` as epoch milliseconds.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn system_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

/// `WebFetchUrlCache`. Insertion order is recency order (oldest first), as
/// in the baseline's `Map` with delete-then-set on every hit.
pub struct UrlCache {
    nodes: Mutex<(VecDeque<Node>, usize)>,
    now: Clock,
}

impl Default for UrlCache {
    fn default() -> Self {
        Self::new(None)
    }
}

impl UrlCache {
    pub fn new(now: Option<Clock>) -> Self {
        Self {
            nodes: Mutex::new((VecDeque::new(), 0)),
            now: now.unwrap_or_else(|| Arc::new(system_now)),
        }
    }

    /// `get(url)`: an expired entry is removed; a hit becomes the newest.
    pub fn get(&self, url: &str) -> Option<FetchedEntry> {
        let mut state = self.nodes.lock().unwrap();
        let (nodes, total) = &mut *state;
        let index = nodes.iter().position(|node| node.url == url)?;
        let node = nodes.remove(index)?;
        if (self.now)() >= node.expires_at {
            *total -= node.size;
            return None;
        }
        let entry = node.entry.clone();
        nodes.push_back(node);
        Some(entry)
    }

    /// `set(url, entry, contentBytes)`.
    pub fn set(&self, url: &str, entry: FetchedEntry, content_bytes: usize) {
        let size = content_bytes.max(1);
        if size > MAX_CACHE_SIZE_BYTES {
            return;
        }
        let mut state = self.nodes.lock().unwrap();
        let (nodes, total) = &mut *state;
        if let Some(index) = nodes.iter().position(|node| node.url == url) {
            let existing = nodes.remove(index).expect("index in range");
            *total -= existing.size;
        }
        while *total + size > MAX_CACHE_SIZE_BYTES {
            let Some(oldest) = nodes.pop_front() else {
                break;
            };
            *total -= oldest.size;
        }
        nodes.push_back(Node {
            url: url.to_string(),
            entry,
            size,
            expires_at: (self.now)() + CACHE_TTL_MS,
        });
        *total += size;
    }

    pub fn clear(&self) {
        let mut state = self.nodes.lock().unwrap();
        state.0.clear();
        state.1 = 0;
    }

    /// Total accounted bytes (for tests).
    pub fn total_bytes(&self) -> usize {
        self.nodes.lock().unwrap().1
    }

    /// Cached URLs, oldest first (for tests).
    pub fn urls(&self) -> Vec<String> {
        self.nodes
            .lock()
            .unwrap()
            .0
            .iter()
            .map(|node| node.url.clone())
            .collect()
    }
}
