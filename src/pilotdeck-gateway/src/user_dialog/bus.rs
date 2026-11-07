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
    /// The neighbours the entry had in its session bucket. TS `remove()`
    /// leaves the bucket untouched when `onSettled` throws, so [`Self::restore`]
    /// puts the entry back where it sat instead of appending to the tail
    /// (which would reorder `list`). Ids rather than an index: a settle hook is
    /// host code and may reenter the bus to remove or insert other members, so
    /// a stale numeric index would land the entry on the wrong side of them.
    predecessor: Option<JsString>,
    successor: Option<JsString>,
    /// Registration number of the consumed entry, put back by [`Self::restore`]
    /// so the restored dialog keeps its identity (#1599).
    generation: u64,
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
    /// Registration number this snapshot was taken from. A replacement under
    /// the same request id gets a new number, so the answer stays bound to the
    /// dialog its validator was written for (#1599).
    pub generation: u64,
}

/// Observer changes queued by a bus mutation. The bus only queues; the
/// lock-holding caller delivers after dropping its guard, because the
/// `on_change` observer is host code (#1442).
pub struct Outbox {
    events: Vec<JsValue>,
    observer: Option<Arc<ObserverFn>>,
}
impl Outbox {
    /// The changes this consume already caused, minus the rollback-able
    /// "settled" broadcast of `request`. A failing settle hook restores the
    /// entry and drops that one event — TS `remove()` throws before it emits
    /// (GatewayUserDialogBus.ts:219-226) — but effects that happened earlier in
    /// the same consume, such as a lease expiring inside
    /// `consume_for_response`, already occurred and must still reach the
    /// renderer.
    #[must_use]
    pub fn without_settled(self, session: &JsString, request: &JsString) -> Self {
        let settled = JsString::from("settled");
        let events = self
            .events
            .into_iter()
            .filter(|event| {
                let matches = |key: &str, wanted: &JsString| {
                    event
                        .get(key)
                        .and_then(JsValue::as_string)
                        .is_some_and(|value| value == wanted)
                };
                !(matches("type", &settled)
                    && matches("sessionKey", session)
                    && matches("requestId", request))
            })
            .collect();
        Self {
            events,
            observer: self.observer,
        }
    }

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
    /// Registration identity. Every `register` for a `(session, requestId)`
    /// gets a fresh number, so a caller that snapshotted the previous
    /// registration can tell it was replaced even when the replacement reuses
    /// the same validator handle (#1599).
    next_generation: u64,
    generations: HashMap<(JsString, JsString), u64>,
    /// Registration numbers whose entries were consumed under a `(session,
    /// requestId)` key and whose answers may still be in flight (#2027).
    /// Without them a missing entry is ambiguous: a poll cannot tell "another
    /// registration took the id" (its waiter will never be answered, so it
    /// must be ended) from "the registration's own consumer already took the
    /// entry and is about to answer it" (ending it would drop that answer).
    ///
    /// This is a set, not the last generation: a same-id registration that
    /// publishes while an older answer is in flight can be consumed too, and
    /// overwriting the older record would make that in-flight answer look
    /// like a take-over. `forget_taken` drops one registration's record once
    /// its waiter settles, so the map does not grow with dialog count.
    taken: HashMap<(JsString, JsString), Vec<u64>>,
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
            next_generation: 0,
            generations: HashMap::new(),
            taken: HashMap::new(),
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
            next_generation: 0,
            generations: HashMap::new(),
            taken: HashMap::new(),
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

    /// Registers a dialog and returns its registration generation. TS replaces
    /// a same-id entry silently (`GatewayUserDialogBus.ts:150-160`); the
    /// generation is how a caller that snapshotted the previous entry learns
    /// that its answer no longer belongs to the pending dialog (#1599).
    pub fn register(&mut self, session: &JsString, entry: PendingDialog) -> u64 {
        let change = object([
            ("type", "requested".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("request", entry.event.clone()),
        ]);
        self.next_generation += 1;
        let generation = self.next_generation;
        self.generations
            .insert((session.clone(), entry.request_id.clone()), generation);
        // The `taken` record is *not* cleared here (#2027): it belongs to the
        // registration that was just replaced, and it is how that waiter's own
        // in-flight answer (already consumed, not yet resolved) stays visible.
        // Consumers compare it by generation, so a newer registration reads it
        // as "not mine" while the older one still sees its answer on the way.
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
        generation
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
            generation: self
                .generations
                .get(&(session.clone(), request.clone()))
                .copied()
                .unwrap_or_default(),
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
    ///
    /// The returned value owns a pending dialog: dropping it without running
    /// [`PendingDialog::on_settled`] and without [`Self::restore`] leaks the
    /// waiter, so the result is `#[must_use]`.
    #[must_use = "run on_settled outside the guard, then restore on failure or deliver the answer"]
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
        let predecessor = index
            .checked_sub(1)
            .and_then(|before| bucket.get(before))
            .map(|entry| entry.request_id.clone());
        let successor = bucket.get(index + 1).map(|entry| entry.request_id.clone());
        let entry = bucket.remove(index);
        if bucket.is_empty() {
            self.pending.remove(session);
        }
        let lease = self.leases.remove(&(session.clone(), request.clone()));
        let generation = self
            .generations
            .remove(&(session.clone(), request.clone()))
            .unwrap_or_default();
        // Remember which registration owned the entry that just went away: a
        // poll that finds no entry can then tell a take-over from its own
        // in-flight answer (#2027). Later consumes under the same id append
        // instead of overwriting, because their answers can be in flight at
        // the same time.
        let taken = self
            .taken
            .entry((session.clone(), request.clone()))
            .or_default();
        if !taken.contains(&generation) {
            taken.push(generation);
        }
        self.emit(object([
            ("type", "settled".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", JsValue::String(request.clone())),
            ("reason", reason.into()),
        ]));
        Some(ConsumedDialog {
            entry,
            lease,
            predecessor,
            successor,
            generation,
        })
    }

    /// Puts a consumed dialog back after its settle hook failed outside the
    /// lock: TS `remove()` mutates nothing when `onSettled` throws, so the
    /// dialog stays pending, still leased, and broadcast-free
    /// (GatewayUserDialogBus.ts:220-222). The caller discards the queued
    /// "settled" change instead of delivering it.
    ///
    /// A settle hook is host code: it can reenter the bus and register a new
    /// dialog under the same request id before failing. TS `remove()` throws
    /// before it mutates, so that reentrant registration survives; writing the
    /// old entry and lease back would overwrite it (#1599). The reentrant
    /// registration wins and the consumed entry is returned to the caller,
    /// which must end its waiter explicitly after dropping the guard instead of
    /// leaving it pending forever (#1599).
    #[must_use = "end the returned waiter when a reentrant registration won"]
    pub fn restore(
        &mut self,
        session: &JsString,
        consumed: ConsumedDialog,
    ) -> Option<PendingDialog> {
        let ConsumedDialog {
            entry,
            lease,
            predecessor,
            successor,
            generation,
        } = consumed;
        let bucket = self.pending.entry(session.clone()).or_default();
        if bucket
            .iter()
            .any(|value| value.request_id == entry.request_id)
        {
            return Some(entry);
        }
        let request_id = entry.request_id.clone();
        // Prefer the old successor (insert before it); otherwise the old
        // predecessor (insert after it); otherwise the tail. Either neighbour
        // may have been consumed by a reentrant host callback in the meantime.
        let position = successor
            .and_then(|after| bucket.iter().position(|value| value.request_id == after))
            .or_else(|| {
                predecessor
                    .and_then(|before| bucket.iter().position(|value| value.request_id == before))
                    .map(|before| before + 1)
            })
            .unwrap_or(bucket.len());
        bucket.insert(position, entry);
        self.generations
            .insert((session.clone(), request_id.clone()), generation);
        // The entry is back, so the rollback of a failed settle hook also
        // rolls back this registration's "answer in flight" record (#2027).
        // Only its own generation: a same-id registration consumed while the
        // hook ran still has an answer on the way and must stay visible.
        let key = (session.clone(), request_id.clone());
        if let Some(taken) = self.taken.get_mut(&key) {
            if let Some(index) = taken.iter().position(|value| *value == generation) {
                taken.remove(index);
            }
            if taken.is_empty() {
                self.taken.remove(&key);
            }
        }
        if let Some(lease) = lease {
            self.leases.insert((session.clone(), request_id), lease);
        }
        None
    }

    /// Whether the dialog currently pending under `request` is registration
    /// `generation`. A host callback can reenter the bus between a snapshot
    /// and a consume and re-register the same request id (#1599), which
    /// replaces the entry; comparing the registration number keeps an answer
    /// bound to the dialog it was validated against even when the replacement
    /// reuses the same validator handle.
    pub fn pending_matches(&self, session: &JsString, request: &JsString, generation: u64) -> bool {
        self.peek(session, request).is_some()
            && self
                .generations
                .get(&(session.clone(), request.clone()))
                .is_some_and(|current| *current == generation)
    }

    /// Whether `request` is still pending but no longer registration
    /// `generation`: a host callback replaced it while the caller held an
    /// answer for the old one (#1599).
    pub fn pending_replaced(
        &self,
        session: &JsString,
        request: &JsString,
        generation: u64,
    ) -> bool {
        self.peek(session, request).is_some() && !self.pending_matches(session, request, generation)
    }

    /// Whether an entry taken from `request` belonged to registration
    /// `generation` and its answer may still be in flight (#2027). Only a
    /// consumer that took this registration's own dialog will answer its
    /// waiter, so a poll that finds the id gone must not end it; any other
    /// consumer (or another registration taking the id) leaves the waiter with
    /// nothing to answer through.
    pub fn consumed_own(&self, session: &JsString, request: &JsString, generation: u64) -> bool {
        self.taken
            .get(&(session.clone(), request.clone()))
            .is_some_and(|taken| taken.contains(&generation))
    }

    /// Drops registration `generation`'s "answer in flight" record (#2027).
    /// Called once its waiter has settled: from then on nothing can be
    /// answered through that registration, and keeping the record would make
    /// the map grow with the number of dialogs the gateway ever served.
    pub fn forget_taken(&mut self, session: &JsString, request: &JsString, generation: u64) {
        let key = (session.clone(), request.clone());
        if let Some(taken) = self.taken.get_mut(&key) {
            if let Some(index) = taken.iter().position(|value| *value == generation) {
                taken.remove(index);
            }
            if taken.is_empty() {
                self.taken.remove(&key);
            }
        }
    }

    /// As in TS this does not validate values: callers must call `accepts`
    /// first. The settle hook does not run here (#1442); the caller runs it
    /// after dropping the guard and restores on failure.
    ///
    /// `validated` is the snapshot the caller just validated against outside
    /// the guard. Between that snapshot and this call a host callback may
    /// reenter the bus and register a different dialog under the same request
    /// id (#1599); comparing the registration keeps the answer bound to the
    /// dialog it was validated against instead of silently delivering it to
    /// the new one.
    #[must_use = "run on_settled outside the guard, then restore or deliver"]
    pub fn consume_for_response(
        &mut self,
        session: &JsString,
        request: &JsString,
        lease_id: Option<&JsString>,
        reason: &str,
        validated: Option<&ResponseSnapshot>,
    ) -> ConsumeResult {
        if self.peek(session, request).is_none() {
            return ConsumeResult::NotPending;
        }
        if validated
            .is_some_and(|snapshot| !self.pending_matches(session, request, snapshot.generation))
        {
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
    #[must_use = "run each settle/reject outside the guard and deliver the events"]
    pub fn reject_session(&mut self, session: &JsString, reason: &str) -> Vec<RejectedDialog> {
        let entries = self.pending.remove(session).unwrap_or_default();
        self.leases.retain(|(key, _), _| key != session);
        // The caller rejects every entry it just took, so record whose
        // registrations those were: a poll that finds the id gone in the
        // meantime must not end a waiter that is about to be rejected here
        // (#2027).
        for entry in &entries {
            let key = (session.clone(), entry.request_id.clone());
            let generation = self.generations.remove(&key).unwrap_or_default();
            let taken = self.taken.entry(key).or_default();
            if !taken.contains(&generation) {
                taken.push(generation);
            }
        }
        self.generations.retain(|(key, _), _| key != session);
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
    #[must_use = "queued broadcasts must be delivered after the guard drops"]
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
