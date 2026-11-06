use super::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Host-supplied answer validator. An `Arc` so a caller can validate an
/// answer after dropping the bus guard: the validator is host code and must
/// never run while the guard is alive (#1442).
pub type AcceptsFn = Arc<dyn Fn(&JsValue) -> bool + Send + Sync>;
/// The observer's cloneable handle: broadcasts are delivered outside the bus
/// lock (#1442), so the shared closure must be `Send + Sync`.
type ObserverFn = dyn Fn(JsValue) -> Result<()> + Send + Sync;
/// Only the `#[cfg(test)]` legacy constructor takes the boxed observer form;
/// production stores the cloneable [`ObserverFn`] handle that `Outbox`
/// delivers after the guard drops (#1442/#1600).
#[cfg(test)]
type Observer = Box<ObserverFn>;
type SettleHook = Box<dyn Fn() -> Result<()> + Send>;

/// The waiting tool owns its validator and completion callbacks. The bus returns
/// the consumed entry; it never guesses a schema or completes a tool on its own.
pub struct PendingDialog {
    pub request_id: JsString,
    pub dialog_kind: JsString,
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub event: JsValue,
    pub accepts: AcceptsFn,
    pub resolve: Box<dyn Fn(DialogAnswer) + Send>,
    pub reject: Box<dyn Fn(String) + Send>,
    pub on_settled: Option<SettleHook>,
}

pub enum ConsumeResult {
    Entry(Box<ConsumedDialog>),
    NotPending,
    /// The request is still pending, but not the dialog the caller validated:
    /// a host callback reentered the bus between the snapshot and the consume
    /// and replaced it. Delivering would hand the new dialog an answer its own
    /// validator never saw, so this fails closed like [`Self::NotPending`].
    Replaced,
    LeaseRequired,
}

/// A dialog consumed from the bus. The settle hook has NOT run: it is host
/// code, so a guard-holding caller runs it after dropping the lock (#1442).
/// `lease` is the renderer lease removed with the entry; TS `remove()` only
/// mutates after `onSettled` succeeds, so a settle failure restores both
/// (GatewayUserDialogBus.ts:219-226) through [`Self::restore`].
pub struct ConsumedDialog {
    pub entry: PendingDialog,
    lease: Option<ActiveLease>,
    /// Where the entry sat in its session bucket. TS `remove()` leaves the
    /// bucket untouched when `onSettled` throws, so [`Self::restore`] puts it
    /// back at its original position instead of appending to the tail (which
    /// would reorder `list`).
    index: usize,
}

/// A session-rejected dialog plus its own "settled" broadcast. TS
/// `rejectSession` interleaves per entry `[onSettled (errors swallowed),
/// reject, emitChange]` (GatewayUserDialogBus.ts:198-213); the caller keeps
/// that order by running the callbacks before delivering the change.
pub struct RejectedDialog {
    pub entry: PendingDialog,
    pub change: Outbox,
}

/// Snapshot needed to validate an answer without holding the bus lock:
/// `accepts` is a host validator and must run after the guard drops (#1442).
pub struct ResponseSnapshot {
    pub dialog_kind: JsString,
    pub accepts: AcceptsFn,
}

/// Observer changes queued by a bus mutation. The bus only queues; the
/// lock-holding caller delivers after dropping its guard, because the
/// `on_change` observer is host code (#1442).
pub struct Outbox {
    events: Vec<JsValue>,
    observer: Option<Arc<ObserverFn>>,
}
impl Outbox {
    /// Delivers every queued change. Observer errors are ignored, like TS
    /// `emitChange`'s try/catch (GatewayUserDialogBus.ts:244-251).
    pub fn deliver(self) {
        for change in self.events {
            if let Some(observer) = &self.observer {
                // Rust observers report failure through Result (TS catches
                // throws); renderer observation stays best effort either way.
                let _ = observer(change);
            }
        }
    }
}

struct ActiveLease {
    lease: Lease,
    expires_at_ms: f64,
}

/// Per-session in-process state. Rust's exclusive mutable borrow serializes
/// callers; a host sharing this bus must keep that same exclusive ownership.
pub struct GatewayUserDialogBus {
    pending: HashMap<JsString, Vec<PendingDialog>>,
    leases: HashMap<(JsString, JsString), ActiveLease>,
    runtime: DialogRuntime,
    on_change: Option<Arc<ObserverFn>>,
    /// Broadcasts queued by mutations, delivered by the lock-holding caller
    /// after it drops the guard (#1442). TS emits synchronously inside each
    /// mutation; the queue keeps the emit ORDER while moving the observer
    /// invocation out of the lock.
    outbox: Vec<JsValue>,
    /// Whether the four user-dialog methods may serve this bus. Kept separate
    /// from `on_change`: the observer is a renderer-notification sink, not the
    /// capability signal. TS gates availability on *method presence*
    /// (`GatewayWsConnection.ts:481-490` checks `this.options.gateway.listUserDialogs`
    /// and friends) — `InProcessGateway` always defines those methods
    /// (`:2710`/`:2734`/`:2765`), so a live in-process bus is available with a
    /// zero-pending `{dialogs: []}` result regardless of any observer.
    connected: bool,
}
impl GatewayUserDialogBus {
    /// Legacy observer-coupled constructor: connected iff a change observer is
    /// supplied. Production must use [`Self::connected`]; this one exists only
    /// so tests can build the historical "bus unavailable" state explicitly
    /// (#1600). It is `#[cfg(test)]` so production code cannot reach for it by
    /// accident and reintroduce a state that no longer exists in production.
    #[cfg(test)]
    pub fn new(runtime: DialogRuntime, on_change: Option<Observer>) -> Self {
        let connected = on_change.is_some();
        Self {
            pending: HashMap::new(),
            leases: HashMap::new(),
            runtime,
            // #1442: broadcasts are delivered outside the lock, so the
            // observer is held as a cloneable Arc.
            on_change: on_change.map(Arc::from),
            outbox: Vec::new(),
            connected,
        }
    }

    /// Production constructor: the bus is live without a renderer notification
    /// observer. An observer only forwards observational `user_dialog_changed`
    /// frames (`createLocalGateway.ts:1057`); availability must not depend on
    /// it, otherwise registration-free zero-pending calls wrongly answer
    /// `CAPABILITY_UNAVAILABLE` where TS returns `{dialogs: []}`.
    pub fn connected(runtime: DialogRuntime) -> Self {
        Self {
            pending: HashMap::new(),
            leases: HashMap::new(),
            runtime,
            on_change: None,
            outbox: Vec::new(),
            connected: true,
        }
    }

    /// Returns whether the bus may serve the four methods.
    ///
    /// Unconnected semantics differ by method:
    /// - `list`, `claim`, `release`: surface `CAPABILITY_UNAVAILABLE`.
    /// - `respond`: return `{delivered: false}` silently — matches TS
    ///   `GatewayWsConnection.ts:490` (no guard) and
    ///   `InProcessGateway.ts:2672` (`delivered: false`).
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn register(&mut self, session: &JsString, entry: PendingDialog) {
        let change = object([
            ("type", "requested".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("request", entry.event.clone()),
        ]);
        let bucket = self.pending.entry(session.clone()).or_default();
        if let Some(index) = bucket
            .iter()
            .position(|value| value.request_id == entry.request_id)
        {
            bucket[index] = entry;
        } else {
            bucket.push(entry);
        }
        self.emit(change);
    }

    pub fn peek(&self, session: &JsString, request: &JsString) -> Option<&PendingDialog> {
        self.pending
            .get(session)?
            .iter()
            .find(|value| &value.request_id == request)
    }

    /// Data needed to validate an answer outside the bus lock. TS validates
    /// on the peeked entry (`InProcessGateway.ts:2668-2674`); the snapshot
    /// keeps that contract while `accepts` stops running under the guard.
    pub fn snapshot_for_response(
        &self,
        session: &JsString,
        request: &JsString,
    ) -> Option<ResponseSnapshot> {
        let entry = self.peek(session, request)?;
        Some(ResponseSnapshot {
            dialog_kind: entry.dialog_kind.clone(),
            accepts: entry.accepts.clone(),
        })
    }

    pub fn has_pending(&self, session: &JsString, request: &JsString) -> bool {
        self.peek(session, request).is_some()
    }

    pub fn pending_count(&self, session: &JsString) -> usize {
        self.pending.get(session).map_or(0, Vec::len)
    }

    /// Unconditionally consumes for native cancellation/turn cleanup, bypassing
    /// renderer leases. TS `remove()` runs `onSettled` before the mutation, and
    /// a throw leaves the dialog pending (GatewayUserDialogBus.ts:219-226);
    /// the settle hook now travels with the entry instead (#1442), so the
    /// caller runs it after dropping the guard and restores on failure.
    pub fn consume(
        &mut self,
        session: &JsString,
        request: &JsString,
        reason: &str,
    ) -> Option<ConsumedDialog> {
        let index = self
            .pending
            .get(session)?
            .iter()
            .position(|entry| &entry.request_id == request)?;
        let bucket = self.pending.get_mut(session).expect("pending session");
        let entry = bucket.remove(index);
        if bucket.is_empty() {
            self.pending.remove(session);
        }
        let lease = self.leases.remove(&(session.clone(), request.clone()));
        self.emit(object([
            ("type", "settled".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", JsValue::String(request.clone())),
            ("reason", reason.into()),
        ]));
        Some(ConsumedDialog {
            entry,
            lease,
            index,
        })
    }

    /// Puts a consumed dialog back after its settle hook failed outside the
    /// lock: TS `remove()` mutates nothing when `onSettled` throws, so the
    /// dialog stays pending, still leased, and broadcast-free
    /// (GatewayUserDialogBus.ts:220-222). The caller discards the queued
    /// "settled" change instead of delivering it.
    pub fn restore(&mut self, session: &JsString, consumed: ConsumedDialog) {
        let ConsumedDialog {
            entry,
            lease,
            index,
        } = consumed;
        if let Some(lease) = lease {
            self.leases
                .insert((session.clone(), entry.request_id.clone()), lease);
        }
        let bucket = self.pending.entry(session.clone()).or_default();
        match bucket
            .iter()
            .position(|value| value.request_id == entry.request_id)
        {
            Some(position) => bucket[position] = entry,
            None => bucket.insert(index.min(bucket.len()), entry),
        }
    }

    /// As in TS this does not validate values: callers must call `accepts`
    /// first. The settle hook does not run here (#1442); the caller runs it
    /// after dropping the guard and restores on failure.
    ///
    /// `validated` is the validator handle the caller just ran outside the
    /// guard. Between the snapshot and this call a host callback may reenter
    /// the bus and register a different dialog under the same request id
    /// (#1599); comparing the handle keeps the answer bound to the dialog it
    /// was validated against instead of silently delivering it to the new one.
    pub fn consume_for_response(
        &mut self,
        session: &JsString,
        request: &JsString,
        lease_id: Option<&JsString>,
        reason: &str,
        validated: Option<&AcceptsFn>,
    ) -> ConsumeResult {
        let Some(pending) = self.peek(session, request) else {
            return ConsumeResult::NotPending;
        };
        if validated.is_some_and(|validated| !Arc::ptr_eq(&pending.accepts, validated)) {
            return ConsumeResult::Replaced;
        }
        if let Some(lease) = self.active_lease(session, request)
            && Some(&lease.lease_id) != lease_id
        {
            return ConsumeResult::LeaseRequired;
        }
        ConsumeResult::Entry(Box::new(
            self.consume(session, request, reason)
                .expect("pending entry"),
        ))
    }

    /// Rejects every pending dialog of a session. The settle hooks and reject
    /// callbacks do not run here (#1442); the caller runs them after dropping
    /// the guard, keeping TS's per-entry `[onSettled (swallowed), reject,
    /// emitChange]` order (GatewayUserDialogBus.ts:198-213).
    pub fn reject_session(&mut self, session: &JsString, reason: &str) -> Vec<RejectedDialog> {
        let entries = self.pending.remove(session).unwrap_or_default();
        self.leases.retain(|(key, _), _| key != session);
        entries
            .into_iter()
            .map(|entry| {
                let change = object([
                    ("type", "settled".into()),
                    ("sessionKey", JsValue::String(session.clone())),
                    ("requestId", JsValue::String(entry.request_id.clone())),
                    ("reason", reason.into()),
                ]);
                RejectedDialog {
                    entry,
                    change: Outbox {
                        events: vec![change],
                        observer: self.on_change.clone(),
                    },
                }
            })
            .collect()
    }

    /// Takes every broadcast queued by a mutation, for delivery after the
    /// guard drops. The observer is host code: it never runs while the bus
    /// lock is held (#1442).
    pub fn drain_events(&mut self) -> Outbox {
        Outbox {
            events: std::mem::take(&mut self.outbox),
            observer: self.on_change.clone(),
        }
    }

    pub fn claim(
        &mut self,
        session: &JsString,
        request: &JsString,
        ttl_ms: f64,
        renew_lease_id: Option<&JsString>,
    ) -> Result<LeaseClaim> {
        if !self.has_pending(session, request) {
            return Ok(LeaseClaim::NotPending);
        }
        let existing = self.active_lease(session, request);
        if let Some(lease) = &existing
            && Some(&lease.lease_id) != renew_lease_id
        {
            return Ok(LeaseClaim::Busy {
                expires_at: lease.expires_at.clone(),
            });
        }
        let now = (self.runtime.now)();
        let lease = Lease {
            lease_id: existing
                .map(|lease| lease.lease_id)
                .unwrap_or_else(|| (self.runtime.uuid)().into()),
            expires_at: expiry(now, ttl_ms)?,
        };
        self.leases.insert(
            (session.clone(), request.clone()),
            ActiveLease {
                lease: lease.clone(),
                expires_at_ms: now as f64 + ttl_ms,
            },
        );
        self.lease_change(session, request, "claimed", Some(&lease.expires_at));
        Ok(LeaseClaim::Claimed(lease))
    }

    pub fn release(&mut self, session: &JsString, request: &JsString, lease_id: &JsString) -> bool {
        let Some(lease) = self.active_lease(session, request) else {
            return false;
        };
        if &lease.lease_id != lease_id {
            return false;
        }
        self.leases.remove(&(session.clone(), request.clone()));
        self.lease_change(session, request, "released", None);
        true
    }

    pub fn list(&mut self, session: &JsString) -> Vec<JsValue> {
        let entries: Vec<_> = self
            .pending
            .get(session)
            .into_iter()
            .flatten()
            .map(|entry| (entry.request_id.clone(), entry.event.clone()))
            .collect();
        entries
            .into_iter()
            .map(|(id, mut event)| {
                if let Some(lease) = self.active_lease(session, &id) {
                    set(
                        &mut event,
                        "lease",
                        object([("expiresAt", JsValue::String(lease.expires_at))]),
                    );
                }
                event
            })
            .collect()
    }

    fn active_lease(&mut self, session: &JsString, request: &JsString) -> Option<Lease> {
        let key = (session.clone(), request.clone());
        let active = self.leases.get(&key)?;
        if active.expires_at_ms > (self.runtime.now)() as f64 {
            return Some(active.lease.clone());
        }
        self.leases.remove(&key);
        self.lease_change(session, request, "expired", None);
        None
    }
    fn lease_change(
        &mut self,
        session: &JsString,
        request: &JsString,
        action: &str,
        expiry: Option<&JsString>,
    ) {
        let mut change = object([
            ("type", "lease_changed".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", JsValue::String(request.clone())),
            ("action", action.into()),
        ]);
        if let Some(expiry) = expiry {
            set(&mut change, "expiresAt", JsValue::String(expiry.clone()));
        }
        self.emit(change);
    }
    /// Queues a broadcast for the lock-holding caller. The observer itself is
    /// host code: it is only invoked by [`Outbox::deliver`] after the guard
    /// drops (#1442), never in place.
    fn emit(&mut self, change: JsValue) {
        self.outbox.push(change);
    }
}
