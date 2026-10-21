//! Volatile SessionRouter reservations, keyed only by sessionKey.
use pilotdeck_protocol::JsString;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    shutdown: bool,
    turns: HashMap<JsString, JsString>,
    identities: HashMap<JsString, Arc<()>>,
    maintenance: HashSet<JsString>,
}

/// Identity captured atomically when a turn is admitted. Dropping this token
/// does not release admission; the consumer passes it to `end_turn` on cleanup.
#[derive(Clone, Debug)]
pub struct TurnReservation(Arc<()>);

/// Admission only: this does not create a live session, write a durable run, or
/// own completion. Each operation holds a short synchronous lock; no resource
/// creation, abort callback or completion wait runs under it.
#[derive(Default)]
pub struct SessionAdmission(Mutex<State>);

impl SessionAdmission {
    pub fn begin_turn(&self, session_key: &JsString, run_id: &JsString) -> Option<TurnReservation> {
        let mut state = self.0.lock().unwrap();
        if state.shutdown
            || state.turns.contains_key(session_key)
            || state.maintenance.contains(session_key)
        {
            return None;
        }
        state.turns.insert(session_key.clone(), run_id.clone());
        let identity = Arc::new(());
        state
            .identities
            .insert(session_key.clone(), identity.clone());
        Some(TurnReservation(identity))
    }

    pub fn has_active_turn(&self, session_key: &JsString) -> bool {
        self.0.lock().unwrap().turns.contains_key(session_key)
    }

    pub fn active_turn_run_id(&self, session_key: &JsString) -> Option<JsString> {
        self.0.lock().unwrap().turns.get(session_key).cloned()
    }

    /// SessionRouter.endTurn uses a truthiness check: an absent *or empty* ID
    /// clears the reservation. Fence cancellation deliberately differs.
    /// A nonempty ID also requires the identity returned by `begin_turn`:
    /// unlike TS's string-only cleanup, a stale consumer cannot clear a later
    /// reservation that reused its runId after close. None/empty stay unfenced.
    pub fn end_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reservation: Option<&TurnReservation>,
    ) {
        let mut state = self.0.lock().unwrap();
        if run_id.is_none_or(|id| {
            id.is_empty()
                || (state.turns.get(session_key) == Some(id)
                    && reservation.is_some_and(|reservation| {
                        state
                            .identities
                            .get(session_key)
                            .is_some_and(|current| Arc::ptr_eq(current, &reservation.0))
                    }))
        }) {
            state.turns.remove(session_key);
            state.identities.remove(session_key);
        }
    }

    pub(super) fn turn_identity(&self, key: &JsString) -> Option<Arc<()>> {
        self.0.lock().unwrap().identities.get(key).cloned()
    }

    /// A close that started draining an old writer cannot release a later
    /// reservation, even if the caller reused the same runId string.
    pub(super) fn end_identity(&self, key: &JsString, identity: Option<&Arc<()>>) {
        let mut state = self.0.lock().unwrap();
        if identity.is_some_and(|identity| {
            state
                .identities
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, identity))
        }) {
            state.turns.remove(key);
            state.identities.remove(key);
        }
    }

    /// Reservation for seed/compact; the live owner must release it in its
    /// finally/guard path. Actual maintenance execution belongs to that owner.
    pub fn begin_maintenance(&self, session_key: &JsString) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.shutdown || state.turns.contains_key(session_key) {
            return false;
        }
        state.maintenance.insert(session_key.clone())
    }

    pub fn end_maintenance(&self, session_key: &JsString) {
        self.0.lock().unwrap().maintenance.remove(session_key);
    }

    pub fn has_maintenance(&self, session_key: &JsString) -> bool {
        self.0.lock().unwrap().maintenance.contains(session_key)
    }

    /// Stop new reservations and clear volatile state, as Router.shutdown does.
    /// Draining live handles is a separate owner lifecycle operation.
    pub fn shutdown(&self) {
        let mut state = self.0.lock().unwrap();
        state.shutdown = true;
        state.turns.clear();
        state.identities.clear();
        state.maintenance.clear();
    }
}
