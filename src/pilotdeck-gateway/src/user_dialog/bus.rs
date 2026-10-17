use super::*;
use std::collections::HashMap;

type Validator = Box<dyn Fn(&JsValue) -> bool + Send>;
type SettleHook = Box<dyn Fn() -> Result<()> + Send>;
type Observer = Box<dyn Fn(JsValue) -> Result<()> + Send>;

/// The waiting tool owns its validator and completion callbacks. The bus returns
/// the consumed entry; it never guesses a schema or completes a tool on its own.
pub struct PendingDialog {
    pub request_id: JsString,
    pub dialog_kind: JsString,
    pub tool_call_id: JsString,
    pub tool_name: JsString,
    pub event: JsValue,
    pub accepts: Validator,
    pub resolve: Box<dyn Fn(DialogAnswer) + Send>,
    pub reject: Box<dyn Fn(String) + Send>,
    pub on_settled: Option<SettleHook>,
}

pub enum ConsumeResult {
    Entry(Box<PendingDialog>),
    NotPending,
    LeaseRequired,
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
    on_change: Option<Observer>,
}
impl GatewayUserDialogBus {
    pub fn new(runtime: DialogRuntime, on_change: Option<Observer>) -> Self {
        Self {
            pending: HashMap::new(),
            leases: HashMap::new(),
            runtime,
            on_change,
        }
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

    pub fn has_pending(&self, session: &JsString, request: &JsString) -> bool {
        self.peek(session, request).is_some()
    }

    pub fn pending_count(&self, session: &JsString) -> usize {
        self.pending.get(session).map_or(0, Vec::len)
    }

    /// Unconditionally consumes for native cancellation/turn cleanup, bypassing
    /// renderer leases. The settle hook runs before any authoritative mutation.
    pub fn consume(
        &mut self,
        session: &JsString,
        request: &JsString,
        reason: &str,
    ) -> Result<Option<PendingDialog>> {
        let Some(entry) = self.peek(session, request) else {
            return Ok(None);
        };
        if let Some(settle) = &entry.on_settled {
            settle()?;
        }
        let bucket = self.pending.get_mut(session).expect("pending session");
        let index = bucket
            .iter()
            .position(|entry| &entry.request_id == request)
            .expect("pending request");
        let entry = bucket.remove(index);
        if bucket.is_empty() {
            self.pending.remove(session);
        }
        self.leases.remove(&(session.clone(), request.clone()));
        self.emit(object([
            ("type", "settled".into()),
            ("sessionKey", JsValue::String(session.clone())),
            ("requestId", JsValue::String(request.clone())),
            ("reason", reason.into()),
        ]));
        Ok(Some(entry))
    }

    /// As in TS this does not validate values: callers must call `accepts` first.
    pub fn consume_for_response(
        &mut self,
        session: &JsString,
        request: &JsString,
        lease_id: Option<&JsString>,
        reason: &str,
    ) -> Result<ConsumeResult> {
        if !self.has_pending(session, request) {
            return Ok(ConsumeResult::NotPending);
        }
        if let Some(lease) = self.active_lease(session, request)
            && Some(&lease.lease_id) != lease_id
        {
            return Ok(ConsumeResult::LeaseRequired);
        }
        Ok(ConsumeResult::Entry(Box::new(
            self.consume(session, request, reason)?
                .expect("pending entry"),
        )))
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

    pub fn reject_session(&mut self, session: &JsString, reason: &str) {
        if let Some(bucket) = self.pending.get(session) {
            for entry in bucket {
                // Cleanup is allowed to release the waiting tool even when its
                // journal removal fails; recovery will surface the stale record.
                if let Some(settle) = &entry.on_settled {
                    let _ = settle();
                }
                (entry.reject)(reason.to_owned());
                self.emit(object([
                    ("type", "settled".into()),
                    ("sessionKey", JsValue::String(session.clone())),
                    ("requestId", JsValue::String(entry.request_id.clone())),
                    ("reason", reason.into()),
                ]));
            }
        }
        self.pending.remove(session);
        self.leases.retain(|(key, _), _| key != session);
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
        &self,
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
    fn emit(&self, change: JsValue) {
        // Rust observers report failure through Result (TS catches throws).
        if let Some(observer) = &self.on_change {
            let _ = observer(change);
        }
    }
}
