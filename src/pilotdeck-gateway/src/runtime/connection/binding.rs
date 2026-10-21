//! WS identity is independent of sidecar ModuleBinding and project runtime leases.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsBinding {
    pub(super) connection_id: String,
    pub(super) generation: u64,
}
impl WsBinding {
    pub(super) fn fresh() -> Self {
        Self::issue(&GENERATION)
    }
    pub(super) fn issue(counter: &AtomicU64) -> Self {
        let mut generation = counter.load(Ordering::Relaxed);
        loop {
            assert!(generation < MAX_SAFE_INTEGER, "WS generation exhausted");
            match counter.compare_exchange_weak(
                generation,
                generation + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => generation = current,
            }
        }
        Self {
            connection_id: uuid::Uuid::new_v4().to_string(),
            generation: generation + 1,
        }
    }
    pub fn from_json(value: &Value) -> Result<Self, &'static str> {
        let id = value["connectionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Interaction connectionId must be a non-empty string.")?;
        let number = value["generation"]
            .as_f64()
            .filter(|n| {
                n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= MAX_SAFE_INTEGER as f64
            })
            .ok_or("Interaction connection generation must be a non-negative safe integer.")?;
        Ok(Self {
            connection_id: id.to_owned(),
            generation: number as u64,
        })
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn to_json(&self) -> Value {
        json!({"connectionId": self.connection_id, "generation": self.generation})
    }
}

/// Connection-boundary subset of the reconnect port. It owns no pending
/// producers/answers. A7.3 supplies their lifecycle and replay payloads.
#[derive(Default)]
pub struct BindingOwners(Mutex<HashMap<String, WsBinding>>);
impl BindingOwners {
    pub fn current(&self, owner: &str) -> Option<WsBinding> {
        self.0.lock().unwrap().get(owner).cloned()
    }
    pub fn reconnect(&self, owner: &str, next: WsBinding, previous: Option<&WsBinding>) -> Value {
        let mut bindings = self.0.lock().unwrap();
        let last = bindings.get(owner);
        let same = last == Some(&next) && previous.is_none_or(|p| Some(p) == last);
        if !same
            && (last
                .is_some_and(|last| previous != Some(last) || next.generation <= last.generation)
                || (last.is_none() && previous.is_some()))
        {
            let mut result = json!({"outcome":"stale_binding", "requests":[]});
            if let Some(last) = last {
                result["binding"] = last.to_json();
            }
            return result;
        }
        let outcome = if last.is_some() {
            "reconnected"
        } else {
            "no_pending"
        };
        bindings.insert(owner.to_owned(), next.clone());
        json!({"outcome":outcome, "binding":next.to_json(), "requests":[]})
    }
    /// Identity comparison and removal share one lock. Callers must pass the
    /// socket binding, never a separately queried current binding.
    pub fn disconnect(&self, owner: &str, binding: &WsBinding) -> bool {
        let mut bindings = self.0.lock().unwrap();
        if bindings.get(owner) != Some(binding) {
            return false;
        }
        bindings.remove(owner);
        true
    }
}
