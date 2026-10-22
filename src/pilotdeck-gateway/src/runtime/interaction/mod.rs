//! Volatile permission/question round-trips; no turn or transport wiring.
//!
//! TS buses own no timer. A producer's timeout/abort disposes its registration
//! or consumes the pending entry; a response and cleanup serialize at the same
//! mutex. Only the winning consume returns the callback. Registrations require
//! explicit disposal (dropping a handle, like dropping a JS reference, does not
//! cancel). Callbacks run outside the mutex and may reenter the services.
mod bus;
mod coordinator;
mod reconnect;

pub use bus::{GatewayElicitationBus, GatewayPermissionBus, PendingEntry, Registration};
pub use coordinator::{GatewayInteractionCoordinator, InteractionOptions, PermissionDecision};
pub use reconnect::{InteractionBinding, InteractionKind, InteractionReconnectPort};

use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

type Resolve = Arc<dyn Fn(JsValue) + Send + Sync>;
type Reject = Arc<dyn Fn(JsString) + Send + Sync>;
pub type DeliveredObserver = Arc<dyn Fn(&JsString, &JsString) + Send + Sync>;

fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn set(value: &mut JsValue, key: &str, child: JsValue) {
    if let JsValue::Object(value) = value {
        value.insert(key.into(), child);
    }
}

#[derive(Default)]
struct State {
    serial: u64,
    requests: Vec<Replay>,
    bindings: Vec<(JsString, InteractionBinding)>,
    last: Vec<(JsString, InteractionBinding)>,
    buckets: Vec<Bucket>,
}
impl State {
    fn serial(&mut self) -> u64 {
        self.serial = self
            .serial
            .checked_add(1)
            .expect("interaction identity exhausted");
        self.serial
    }
}
struct Replay {
    token: u64,
    owner: JsString,
    kind: InteractionKind,
    entry: PendingEntry,
    payload: Option<JsValue>,
    binding: Option<InteractionBinding>,
}
struct Stored {
    token: u64,
    entry: PendingEntry,
    active: Arc<AtomicBool>,
}
struct Bucket {
    id: u64,
    bus: u64,
    owner: JsString,
    entries: Vec<Stored>,
}
type Shared = Arc<Mutex<State>>;

#[cfg(test)]
mod tests;
