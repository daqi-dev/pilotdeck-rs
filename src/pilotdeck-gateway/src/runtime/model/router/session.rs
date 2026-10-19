//! `SessionRouterStore`: bounded, TTL'd, LRU-ordered volatile routing state.
use pilotdeck_protocol::JsString;
use std::sync::{Arc, Mutex};

const DEFAULT_CAPACITY: usize = 500;
const DEFAULT_TTL_MS: f64 = 60.0 * 60.0 * 1000.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionRoutingState {
    pub session_id: JsString,
    pub is_subagent: bool,
    pub token_saver_tier: Option<JsString>,
    pub sticky_provider: Option<JsString>,
    pub sticky_model: Option<JsString>,
    pub orchestrating: bool,
    pub last_usage: Option<pilotdeck_protocol::JsValue>,
    pub updated_at: f64,
}

struct Slot {
    key: JsString,
    state: SessionRoutingState,
    expires_at: f64,
}

pub struct SessionRouterStore {
    capacity: usize,
    ttl_ms: f64,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    map: Mutex<Vec<Slot>>,
}

impl SessionRouterStore {
    pub fn new(now: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
            ttl_ms: DEFAULT_TTL_MS,
            now,
            map: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self, session_id: &JsString, is_subagent: bool) -> Option<SessionRoutingState> {
        let key = make_key(session_id, is_subagent);
        let now = (self.now)();
        let mut map = self.lock();
        get_slot(&mut map, &key, now)
    }

    /// Atomically read the main-agent slot and write its replacement while the
    /// same lock is held. Mirrors TS `invalidateSticky`, whose synchronous
    /// `get` + `set` cannot interleave with another turn on the event loop.
    /// Returns the state observed before the write (if any).
    pub fn update<F>(&self, session_id: &JsString, update: F) -> Option<SessionRoutingState>
    where
        F: FnOnce(Option<SessionRoutingState>) -> SessionRoutingState,
    {
        let key = make_key(session_id, false);
        let mut map = self.lock();
        let current = get_slot(&mut map, &key, (self.now)());
        let state = update(current.clone());
        let expires_at = (self.now)() + self.ttl_ms;
        set_slot(&mut map, self.capacity, state, expires_at);
        current
    }

    pub fn set(&self, state: SessionRoutingState) {
        let expires_at = (self.now)() + self.ttl_ms;
        let mut map = self.lock();
        set_slot(&mut map, self.capacity, state, expires_at);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Slot>> {
        self.map.lock().expect("session router store")
    }
}

fn get_slot(map: &mut Vec<Slot>, key: &JsString, now: f64) -> Option<SessionRoutingState> {
    let index = map.iter().position(|slot| slot.key == *key)?;
    if map[index].expires_at < now {
        map.remove(index);
        return None;
    }
    let slot = map.remove(index);
    let state = slot.state.clone();
    map.push(Slot {
        key: key.clone(),
        ..slot
    });
    Some(state)
}

fn set_slot(map: &mut Vec<Slot>, capacity: usize, state: SessionRoutingState, expires_at: f64) {
    let key = make_key(&state.session_id, state.is_subagent);
    if let Some(index) = map.iter().position(|slot| slot.key == key) {
        map.remove(index);
    } else {
        while map.len() >= capacity {
            map.remove(0);
        }
    }
    map.push(Slot {
        key,
        state,
        expires_at,
    });
}

fn make_key(session_id: &JsString, is_subagent: bool) -> JsString {
    if !is_subagent {
        return session_id.clone();
    }
    let mut units = session_id.units().to_vec();
    units.extend(":sub".encode_utf16());
    JsString::from_utf16(units)
}
