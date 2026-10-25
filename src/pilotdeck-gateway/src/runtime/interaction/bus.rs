use super::{reconnect, *};
use std::sync::atomic::Ordering;

/// Callback value is the original answer/decision; no schema or normalization
/// belongs to a pending bus. IDs preserve unpaired UTF-16 code units.
#[derive(Clone)]
pub struct PendingEntry {
    pub request_id: JsString,
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub resolve: Resolve,
    pub reject: Reject,
}

/// An explicit cancellation capability tied to one exact registration. An old
/// disposed handle cannot delete a replacement with the same request ID.
#[derive(Clone)]
pub struct Registration {
    state: Shared,
    bus: u64,
    owner: JsString,
    token: u64,
    active: Arc<AtomicBool>,
}
impl Registration {
    pub fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    pub fn dispose(&self) {
        let mut state = self.state.lock().unwrap();
        if !self.active.swap(false, Ordering::SeqCst) {
            return;
        }
        reconnect::remove(&mut state, self.token, &self.owner);
        if let Some(bucket) = state
            .buckets
            .iter_mut()
            .find(|b| b.bus == self.bus && b.owner == self.owner)
        {
            bucket
                .entries
                .lock()
                .unwrap()
                .retain(|e| e.token != self.token);
        }
        state
            .buckets
            .retain(|b| !b.entries.lock().unwrap().is_empty());
    }
}

#[derive(Clone)]
pub struct PendingBus<const PERMISSION: bool> {
    port: InteractionReconnectPort,
    id: u64,
}
pub type GatewayPermissionBus = PendingBus<true>;
pub type GatewayElicitationBus = PendingBus<false>;
impl<const P: bool> Default for PendingBus<P> {
    fn default() -> Self {
        Self::new(InteractionReconnectPort::default())
    }
}
impl<const P: bool> PendingBus<P> {
    pub fn new(port: InteractionReconnectPort) -> Self {
        let id = port.state.lock().unwrap().serial();
        Self { port, id }
    }
    pub fn reconnect_port(&self) -> &InteractionReconnectPort {
        &self.port
    }
    fn kind(&self) -> InteractionKind {
        if P {
            InteractionKind::Permission
        } else {
            InteractionKind::Question
        }
    }
    pub fn register(
        &self,
        owner: &JsString,
        entry: PendingEntry,
        payload: Option<JsValue>,
    ) -> Result<Registration, JsString> {
        self.register_with_payload(owner, entry, || Ok(reconnect::replay_payload(payload)))
            .map_err(|error| error.message)
    }
    /// Adapt host values after registration validation, before publishing pending
    /// state. The arena is only borrowed for this synchronous call.
    pub fn register_producer(
        &self,
        owner: &JsString,
        entry: PendingEntry,
        nodes: &[ProducerValue],
        root: usize,
    ) -> Result<Registration, ReplayValueError> {
        self.register_with_payload(owner, entry, || to_interaction_replay_value(nodes, root))
    }
    fn register_with_payload(
        &self,
        owner: &JsString,
        entry: PendingEntry,
        payload: impl FnOnce() -> Result<Option<JsValue>, ReplayValueError>,
    ) -> Result<Registration, ReplayValueError> {
        let mut state = self.port.state.lock().unwrap();
        let bucket = if let Some(index) = state
            .buckets
            .iter()
            .position(|b| b.bus == self.id && &b.owner == owner)
        {
            index
        } else {
            state.buckets.push(Bucket {
                bus: self.id,
                owner: owner.clone(),
                entries: Arc::default(),
            });
            state.buckets.len() - 1
        };
        if state.buckets[bucket]
            .entries
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.entry.request_id == entry.request_id)
        {
            return Err(message(&[
                self.kind().label().into(),
                " request ".into(),
                entry.request_id.clone(),
                " is already pending for session ".into(),
                owner.clone(),
                ".".into(),
            ])
            .into());
        }
        if owner.is_empty() || entry.request_id.is_empty() {
            return Err(JsString::from(
                "Interaction pending request requires ownerId and requestId.",
            )
            .into());
        }
        if state.requests.iter().any(|r| {
            &r.owner == owner && r.kind == self.kind() && r.entry.request_id == entry.request_id
        }) {
            return Err(message(&[
                "Interaction request ".into(),
                entry.request_id.clone(),
                " is already pending for ".into(),
                owner.clone(),
                ".".into(),
            ])
            .into());
        }
        state.replay_owners.insert(owner.clone());
        let payload = payload()?;
        let token = state.serial();
        let active = Arc::new(AtomicBool::new(true));
        let binding = reconnect::current(&state, owner);
        state.requests.push(Replay {
            token,
            owner: owner.clone(),
            kind: self.kind(),
            entry: entry.clone(),
            payload,
            binding,
        });
        state.buckets[bucket].entries.lock().unwrap().push(Stored {
            token,
            entry,
            active: active.clone(),
            settled_by: None,
        });
        Ok(Registration {
            state: self.port.state.clone(),
            bus: self.id,
            owner: owner.clone(),
            token,
            active,
        })
    }
    pub fn consume(
        &self,
        owner: &JsString,
        request: &JsString,
        binding: Option<&InteractionBinding>,
    ) -> Option<PendingEntry> {
        let mut state = self.port.state.lock().unwrap();
        if binding.is_some_and(|binding| {
            !reconnect::is_current(&state, owner, self.kind(), request, binding)
        }) {
            return None;
        }
        let index = state
            .buckets
            .iter()
            .position(|b| b.bus == self.id && &b.owner == owner)?;
        let stored = {
            let mut entries = state.buckets[index].entries.lock().unwrap();
            let index = entries.iter().position(|e| {
                &e.entry.request_id == request
                    && (e.active.load(Ordering::SeqCst)
                        || e.settled_by == Some(std::thread::current().id()))
            })?;
            entries.remove(index)
        };
        stored.active.store(false, Ordering::SeqCst);
        reconnect::remove(&mut state, stored.token, owner);
        state
            .buckets
            .retain(|b| !b.entries.lock().unwrap().is_empty());
        Some(stored.entry)
    }
    pub fn has_pending(&self, owner: &JsString, request: &JsString) -> bool {
        self.port.state.lock().unwrap().buckets.iter().any(|b| {
            b.bus == self.id
                && &b.owner == owner
                && b.entries
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| &e.entry.request_id == request)
        })
    }
    pub fn pending_count(&self, owner: &JsString) -> usize {
        self.port
            .state
            .lock()
            .unwrap()
            .buckets
            .iter()
            .find(|b| b.bus == self.id && &b.owner == owner)
            .map_or(0, |b| b.entries.lock().unwrap().len())
    }
    pub fn has_pending_session(&self, owner: &JsString) -> bool {
        self.pending_count(owner) > 0
    }
    pub fn snapshot(&self, owner: &JsString) -> Vec<JsValue> {
        reconnect::snapshot(&self.port.state.lock().unwrap(), owner, Some(self.kind()))
    }
    pub fn reconnect(
        &self,
        owner: &JsString,
        next: InteractionBinding,
        previous: Option<&InteractionBinding>,
    ) -> Result<JsValue, JsString> {
        self.port.reconnect(owner, next, previous)
    }
    pub fn disconnect(
        &self,
        owner: &JsString,
        binding: &InteractionBinding,
    ) -> Result<bool, JsString> {
        self.port.disconnect(owner, binding)
    }
    pub fn current_binding(&self, owner: &JsString) -> Option<InteractionBinding> {
        self.port.current_binding(owner)
    }
    pub fn reject_session(&self, owner: &JsString, reason: &JsString) {
        let entries = self
            .port
            .state
            .lock()
            .unwrap()
            .buckets
            .iter()
            .find(|b| b.bus == self.id && &b.owner == owner)
            .map(|b| b.entries.clone());
        let Some(entries) = entries else {
            return;
        };
        let mut cursor = 0;
        loop {
            // TS holds the original live Map through nested reject/deny calls,
            // including after bySession deletes it. Revisit settled entries only
            // for synchronous callback reentry, never from another thread.
            let entry = {
                let mut state = self.port.state.lock().unwrap();
                let mut entries = entries.lock().unwrap();
                let entry = entries
                    .iter_mut()
                    .find(|e| e.token > cursor && bulk_available(e))
                    .map(|stored| {
                        cursor = stored.token;
                        stored.settled_by = Some(std::thread::current().id());
                        stored.active.store(false, Ordering::SeqCst);
                        stored.entry.clone()
                    });
                if entry.is_some() {
                    reconnect::remove(&mut state, cursor, owner);
                } else {
                    // TS runs the exhausted loop and bySession.delete as
                    // adjacent statements; a separate lock acquisition would
                    // reopen a window where a cross-thread register is
                    // silently deleted without any single-thread TS
                    // equivalent. Real TS: a register the live Map still
                    // holds is settled by the loop; one landing after the
                    // delete keeps a fresh bucket.
                    state
                        .buckets
                        .retain(|b| b.bus != self.id || &b.owner != owner);
                }
                entry
            };
            let Some(entry) = entry else {
                break;
            };
            (entry.reject)(reason.clone());
        }
    }
    pub fn dispose(&self, reason: Option<&JsString>) {
        let reason = reason.cloned().unwrap_or_else(|| "gateway_disposed".into());
        let owners: Vec<_> = self
            .port
            .state
            .lock()
            .unwrap()
            .buckets
            .iter()
            .filter(|b| b.bus == self.id)
            .map(|b| b.owner.clone())
            .collect();
        for owner in owners {
            if P {
                self.deny(&owner, &reason);
            } else {
                self.reject_session(&owner, &reason);
            }
        }
    }
    fn deny(&self, owner: &JsString, reason: &JsString) {
        // TS removes the permission bucket before callbacks (rejectSession
        // instead removes it after callbacks). Reentrant registration survives this
        // deny, unless an enclosing reject later deletes the owner as in TS.
        let entries = {
            let mut state = self.port.state.lock().unwrap();
            let Some(index) = state
                .buckets
                .iter()
                .position(|b| b.bus == self.id && &b.owner == owner)
            else {
                return;
            };
            state.buckets.remove(index).entries
        };
        let mut cursor = 0;
        loop {
            let entry = {
                let mut state = self.port.state.lock().unwrap();
                let mut entries = entries.lock().unwrap();
                entries
                    .iter_mut()
                    .find(|e| e.token > cursor && bulk_available(e))
                    .map(|stored| {
                        cursor = stored.token;
                        stored.settled_by = Some(std::thread::current().id());
                        stored.active.store(false, Ordering::SeqCst);
                        stored.entry.clone()
                    })
                    .inspect(|_| reconnect::remove(&mut state, cursor, owner))
            };
            let Some(entry) = entry else {
                break;
            };
            (entry.resolve)(object([
                ("requestId", JsValue::String(entry.request_id)),
                ("decision", "deny".into()),
                ("reason", JsValue::String(reason.clone())),
            ]));
        }
    }
}
impl PendingBus<true> {
    pub fn deny_session(&self, owner: &JsString, reason: &JsString) {
        self.deny(owner, reason);
    }
}
fn message(parts: &[JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|s| s.units().iter().copied())
            .collect(),
    )
}

// An inactive entry may still belong to the original TS Map during synchronous
// reentry (or dispose on a detached deny Map). Cross-thread bulk calls must not
// settle an entry already claimed by another bulk operation.
fn bulk_available(stored: &Stored) -> bool {
    stored
        .settled_by
        .is_none_or(|thread| thread == std::thread::current().id())
}
