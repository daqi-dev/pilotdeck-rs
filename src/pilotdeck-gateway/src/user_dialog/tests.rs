use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-user-dialog-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        // #3143 fix-r2 (DS sibling#6): a cleanup failure must not turn a test
        // failure into a double panic (abort) -- ignored the way the
        // family's fixed shapes report it (the result already carries
        // the original failure).
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> JsValue {
    let oracle = include_str!("../../tests/fixtures/user-dialog/oracle.json");
    assert!(
        !oracle.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    crate::test_baseline::oracle(oracle)
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap()
}
fn text(value: &JsValue, name: &str) -> String {
    string(value, name).unwrap().to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(array) = value {
        array
    } else {
        panic!("not an array: {value:?}")
    }
}
fn number(value: &JsValue, name: &str) -> f64 {
    field(value, name).as_number().unwrap()
}
fn key(fixture: &JsValue) -> StoreKey {
    let key = field(fixture, "key");
    StoreKey {
        project_root: string(key, "projectRoot").unwrap().clone(),
        pilot_home: string(key, "pilotHome").unwrap().clone(),
        session_id: string(key, "sessionId").unwrap().clone(),
    }
}
fn dialog(fixture: &JsValue) -> StoredDialog {
    StoredDialog {
        request: field(fixture, "request").clone(),
        created_at: string(fixture, "createdAt").unwrap().clone(),
    }
}
fn runtime(clock: &Arc<AtomicI64>, id: &'static str) -> DialogRuntime {
    let clock = clock.clone();
    DialogRuntime {
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
        uuid: Arc::new(move || id.to_owned()),
    }
}
fn store_options(path: &Path, clock: &Arc<AtomicI64>) -> StoreOptions {
    let mut options = StoreOptions::new(path);
    options.runtime = runtime(clock, "fixed-id");
    options
}
fn store(path: &Path, clock: &Arc<AtomicI64>) -> FileGatewayUserDialogStore {
    FileGatewayUserDialogStore::new(store_options(path, clock)).unwrap()
}
fn clock(fixture: &JsValue) -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(number(fixture, "initialMs") as i64))
}
fn dialogs_value(dialogs: Vec<StoredDialog>) -> JsValue {
    JsValue::Array(dialogs.iter().map(StoredDialog::to_value).collect())
}
fn optional_id(value: &JsValue, name: &str) -> Option<JsString> {
    optional_string(value, name).unwrap()
}
fn claim_value(claim: LeaseClaim, bus: bool) -> JsValue {
    match claim {
        LeaseClaim::Claimed(lease) if bus => object([
            ("claimed", JsValue::Bool(true)),
            (
                "lease",
                object([
                    ("leaseId", JsValue::String(lease.lease_id)),
                    ("expiresAt", JsValue::String(lease.expires_at)),
                ]),
            ),
        ]),
        LeaseClaim::Claimed(lease) => object([
            ("claimed", JsValue::Bool(true)),
            ("leaseId", JsValue::String(lease.lease_id)),
            ("expiresAt", JsValue::String(lease.expires_at)),
        ]),
        LeaseClaim::Busy { expires_at } => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", "claimed".into()),
            ("expiresAt", JsValue::String(expires_at)),
        ]),
        LeaseClaim::NotPending => object([
            ("claimed", JsValue::Bool(false)),
            ("reason", "not_pending".into()),
        ]),
    }
}
fn owner_value(claim: OwnerClaim) -> JsValue {
    match claim {
        OwnerClaim::Owned {
            owner_id,
            expires_at,
        } => object([
            ("owned", JsValue::Bool(true)),
            ("ownerId", JsValue::String(owner_id)),
            ("expiresAt", JsValue::String(expires_at)),
        ]),
        OwnerClaim::Busy { expires_at } => object([
            ("owned", JsValue::Bool(false)),
            ("reason", "owned".into()),
            ("expiresAt", JsValue::String(expires_at)),
        ]),
        OwnerClaim::NotPending => object([
            ("owned", JsValue::Bool(false)),
            ("reason", "not_pending".into()),
        ]),
    }
}
fn result_value(result: Result<JsValue>) -> JsValue {
    result.unwrap_or_else(|error| object([("error", JsValue::from(error.to_string().as_str()))]))
}
fn equal(actual: &JsValue, expected: &JsValue, context: &str) {
    assert_eq!(json(actual).unwrap(), json(expected).unwrap(), "{context}");
}
fn store_step(
    store: &FileGatewayUserDialogStore,
    clock: &AtomicI64,
    fixture: &JsValue,
    action: &JsValue,
) -> Result<JsValue> {
    let key = key(fixture);
    let request_id = optional_id(action, "id").unwrap_or_else(|| "dialog-1".into());
    let lease = optional_id(action, "lease");
    let owner = optional_id(action, "owner").unwrap_or_default();
    let ttl = action
        .get("ttl")
        .and_then(JsValue::as_number)
        .unwrap_or(1000.0);
    Ok(match text(action, "op").as_str() {
        "put" => {
            store.put(
                &key,
                &StoredDialog {
                    request: action
                        .get("request")
                        .unwrap_or_else(|| field(fixture, "request"))
                        .clone(),
                    created_at: optional_id(action, "createdAt")
                        .unwrap_or_else(|| string(fixture, "createdAt").unwrap().clone()),
                },
            )?;
            JsValue::Null
        }
        "list" => dialogs_value(store.list(&key)?),
        "listLive" => dialogs_value(store.list_live(&key)?),
        "claim" => claim_value(
            store.claim_live(&key, &request_id, ttl, lease.as_ref())?,
            false,
        ),
        "release" => {
            JsValue::Bool(store.release_live(&key, &request_id, lease.as_ref().unwrap())?)
        }
        "owner" => owner_value(store.claim_live_owner(&key, &request_id, &owner, ttl)?),
        "renewOwner" => JsValue::Bool(store.renew_live_owner(&key, &request_id, &owner, ttl)?),
        "releaseOwner" => JsValue::Bool(store.release_live_owner(&key, &request_id, &owner)?),
        "submit" => JsValue::Bool(store.submit_live_answer(
            &key,
            &request_id,
            lease.as_ref(),
            &DialogAnswer::from_value(field(action, "result"))?,
        )?),
        "take" => store
            .take_live_answer(&key, &request_id)?
            .map_or(JsValue::Null, |answer| answer.to_value()),
        "remove" => {
            store.remove(&key, &request_id)?;
            JsValue::Null
        }
        "clear" => {
            store.clear(&key)?;
            JsValue::Null
        }
        "advance" => JsValue::Number(
            (clock.fetch_add(number(action, "ms") as i64, Ordering::SeqCst)
                + number(action, "ms") as i64) as f64,
        ),
        "reopen" => JsValue::Null,
        op => panic!("unhandled {op}"),
    })
}
fn seed_hosted(store: &FileGatewayUserDialogStore, clock: &AtomicI64, fixture: &JsValue) {
    for step in &array(field(fixture, "steps"))[..10] {
        equal(
            &result_value(store_step(store, clock, fixture, field(step, "action"))),
            field(step, "result"),
            "seed",
        );
    }
}

/// This normal CI test compares every byte, including whitespace, field order,
/// surrogate escapes and JS number spelling. No ignored/Node dependency.
#[test]
fn rust_writes_match_real_ts_golden_bytes_for_every_transition() {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let mut store = store(&scratch.0, &clock);
    let path = store.record_path(&key(&fixture)).unwrap();
    assert_eq!(
        path.strip_prefix(&scratch.0).unwrap().to_str().unwrap(),
        text(&fixture, "relativePath")
    );
    assert!(path.strip_prefix(&scratch.0).unwrap().components().count() > 2);
    for (index, step) in array(field(&fixture, "steps")).iter().enumerate() {
        let action = field(step, "action");
        if text(action, "op") == "reopen" {
            store = super::tests::store(&scratch.0, &clock);
        }
        equal(
            &result_value(store_step(&store, &clock, &fixture, action)),
            field(step, "result"),
            &format!("store step {index}"),
        );
        match field(step, "disk") {
            JsValue::Null => assert!(!path.exists(), "step {index}"),
            JsValue::String(expected) => assert_eq!(
                fs::read(&path).unwrap(),
                expected.to_utf8().unwrap().as_bytes(),
                "raw bytes step {index}"
            ),
            _ => panic!("invalid fixture"),
        }
        if index == 9 {
            assert_eq!(
                fs::read(&path).unwrap(),
                include_bytes!("../../tests/fixtures/user-dialog/hosted.json")
            );
        }
        assert!(!path.with_file_name("dialogs.json.lock").exists());
    }
}

#[test]
fn reads_real_ts_files_and_matches_reader_admission() {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    clock.fetch_add(5000, Ordering::SeqCst);
    let store = store(&scratch.0, &clock);
    let key = key(&fixture);
    let path = store.record_path(&key).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for case in array(field(&fixture, "readCases")) {
        fs::write(&path, text(case, "disk")).unwrap();
        let actual = (|| -> Result<JsValue> {
            Ok(object([
                ("list", dialogs_value(store.list(&key)?)),
                ("live", dialogs_value(store.list_live(&key)?)),
                (
                    "answer",
                    store
                        .take_live_answer(&key, &"dialog-1".into())?
                        .map_or(JsValue::Null, |a| a.to_value()),
                ),
            ]))
        })()
        .unwrap_or_else(|error| {
            object([(
                "invalid",
                JsValue::Bool(error.to_string().ends_with(": invalid store record")),
            )])
        });
        equal(&actual, field(case, "result"), &text(case, "name"));
    }
    fs::write(&path, b"{broken").unwrap();
    assert!(
        store
            .list(&key)
            .unwrap_err()
            .to_string()
            .starts_with("Unable to read Gateway user-dialog store")
    );
    // Node's readFile('utf8') substitutes invalid UTF-8; JSON strings can contain it.
    let raw = include_bytes!("../../tests/fixtures/user-dialog/hosted.json");
    let mut bytes = raw.to_vec();
    let at = bytes.windows(4).position(|b| b == b"tool").unwrap();
    bytes[at] = 0xff;
    fs::write(&path, bytes).unwrap();
    assert!(store.list(&key).is_ok());
}

#[derive(Default)]
struct Callbacks {
    settle_fails: AtomicBool,
    settled: AtomicUsize,
    rejected: Mutex<Vec<String>>,
    resolved: Mutex<Vec<DialogAnswer>>,
}
fn pending(request: &JsValue, callbacks: &Arc<Callbacks>) -> PendingDialog {
    let settle = callbacks.clone();
    let reject = callbacks.clone();
    let resolve = callbacks.clone();
    PendingDialog {
        request_id: string(request, "requestId").unwrap().clone(),
        dialog_kind: string(request, "dialogKind").unwrap().clone(),
        tool_call_id: string(request, "toolCallId").unwrap().clone(),
        tool_name: string(request, "toolName").unwrap().clone(),
        event: request.clone(),
        accepts: Arc::new(|value| value.as_string().is_some()),
        resolve: Box::new(move |answer| resolve.resolved.lock().unwrap().push(answer)),
        reject: Box::new(move |error| reject.rejected.lock().unwrap().push(error)),
        on_settled: Some(Box::new(move || {
            if settle.settle_fails.load(Ordering::SeqCst) {
                return Err(DialogError::Invalid("settle failed".into()));
            }
            settle.settled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })),
    }
}

/// #1442 caller contract: run a consumed dialog's settle hook outside the
/// bus. A failure restores the entry (with its lease) — the TS remove()
/// fail-closed contract (GatewayUserDialogBus.ts:219-226) — and the caller
/// drops the queued broadcast instead of delivering it.
fn settle_or_restore(
    bus: &mut GatewayUserDialogBus,
    session: &JsString,
    consumed: ConsumedDialog,
) -> Result<PendingDialog> {
    let settled = consumed
        .entry
        .on_settled
        .as_ref()
        .map_or(Ok(()), |hook| hook());
    let error = match settled {
        Err(error) => error,
        Ok(()) => return Ok(consumed.entry),
    };
    assert!(
        bus.restore(session, consumed).is_none(),
        "#1599 the entry comes back when no reentrant registration took its id"
    );
    Err(error)
}
#[test]
fn real_ts_bus_trace_covers_two_renderers_renew_release_expiry_and_broadcast_failure() {
    let fixture = fixture();
    let clock = clock(&fixture);
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let mut bus = GatewayUserDialogBus::new(
        runtime(&clock, "bus-lease"),
        Some(Box::new(move |event| {
            observed.lock().unwrap().push(event);
            Err(DialogError::Invalid("observer offline".into()))
        })),
    );
    let callbacks = Arc::new(Callbacks::default());
    let session = "session".into();
    let request = "dialog-1".into();
    for (index, step) in array(field(&fixture, "busSteps")).iter().enumerate() {
        let action = field(step, "action");
        let lease = optional_id(action, "lease");
        let result = (|| -> Result<JsValue> {
            Ok(match text(action, "op").as_str() {
                "register" => {
                    bus.register(&session, pending(field(&fixture, "request"), &callbacks));
                    bus.drain_events().deliver();
                    JsValue::Null
                }
                "claim" => {
                    let claim =
                        bus.claim(&session, &request, number(action, "ttl"), lease.as_ref());
                    bus.drain_events().deliver();
                    claim_value(claim?, true)
                }
                "list" => {
                    let listed = bus.list(&session);
                    bus.drain_events().deliver();
                    JsValue::Array(listed)
                }
                "consume" => {
                    let consumed = bus.consume_for_response(
                        &session,
                        &request,
                        lease.as_ref(),
                        "answered",
                        None,
                    );
                    let outbox = bus.drain_events();
                    match consumed {
                        ConsumeResult::Entry(consumed) => {
                            match settle_or_restore(&mut bus, &session, *consumed) {
                                Ok(entry) => {
                                    outbox.deliver();
                                    object([("entry", entry.event)])
                                }
                                Err(error) => {
                                    // Fail-closed: drop the queued broadcast.
                                    drop(outbox);
                                    return Err(error);
                                }
                            }
                        }
                        ConsumeResult::NotPending => {
                            outbox.deliver();
                            object([("reason", "not_pending".into())])
                        }
                        ConsumeResult::Replaced => {
                            outbox.deliver();
                            object([("reason", "replaced".into())])
                        }
                        ConsumeResult::LeaseRequired => {
                            outbox.deliver();
                            object([("reason", "lease_required".into())])
                        }
                    }
                }
                "release" => {
                    let released = bus.release(&session, &request, lease.as_ref().unwrap());
                    bus.drain_events().deliver();
                    JsValue::Bool(released)
                }
                "advance" => JsValue::Number(
                    (clock.fetch_add(number(action, "ms") as i64, Ordering::SeqCst)
                        + number(action, "ms") as i64) as f64,
                ),
                "settleFails" => {
                    callbacks
                        .settle_fails
                        .store(field(action, "value").as_bool().unwrap(), Ordering::SeqCst);
                    JsValue::Null
                }
                "reject" => {
                    // #1442: the settle hooks, reject callbacks and changes
                    // run outside the bus, in TS's per-entry order
                    // [onSettled (swallowed), reject, emit]
                    // (GatewayUserDialogBus.ts:198-213).
                    for rejected in bus.reject_session(&session, "closed") {
                        let _ = rejected
                            .entry
                            .on_settled
                            .as_ref()
                            .map_or(Ok(()), |hook| hook());
                        (rejected.entry.reject)("closed".into());
                        rejected.change.deliver();
                    }
                    JsValue::Null
                }
                op => panic!("unhandled {op}"),
            })
        })();
        equal(
            &result_value(result),
            field(step, "result"),
            &format!("bus result {index}"),
        );
        equal(
            &JsValue::Array(events.lock().unwrap().clone()),
            field(step, "events"),
            &format!("bus events {index}"),
        );
        assert_eq!(bus.pending_count(&session), number(step, "count") as usize);
        assert_eq!(
            callbacks.settled.load(Ordering::SeqCst),
            number(step, "settled") as usize
        );
        equal(
            &JsValue::Array(
                callbacks
                    .rejected
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().into())
                    .collect(),
            ),
            field(step, "rejected"),
            "reject callbacks",
        );
    }
}

// A6 consumer contract driver, intentionally not an RPC handler. The real TS
// caller fixtures above establish validate-before-consume and projection order.
#[test]
fn wrong_values_do_not_consume_waiter_and_snapshots_are_detached() {
    let fixture = fixture();
    let clock = clock(&fixture);
    let callbacks = Arc::new(Callbacks::default());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-lease"), None);
    let session = "session".into();
    let request = "dialog-1".into();
    bus.register(&session, pending(field(&fixture, "request"), &callbacks));
    let mut snapshot = bus.list(&session);
    set(&mut snapshot[0], "prompt", "renderer mutation".into());
    equal(
        &bus.peek(&session, &request).unwrap().event,
        field(&fixture, "request"),
        "immutable event",
    );
    for expected in array(field(&fixture, "responses")) {
        let value = field(expected, "value");
        let entry = bus.peek(&session, &request).unwrap();
        if !(entry.accepts)(value) {
            assert_eq!(
                text(field(expected, "error"), "code"),
                "INVALID_USER_DIALOG_RESPONSE"
            );
            assert!(callbacks.resolved.lock().unwrap().is_empty());
        } else {
            let ConsumeResult::Entry(consumed) =
                bus.consume_for_response(&session, &request, None, "answered", None)
            else {
                panic!("not consumed")
            };
            // #1442: the settle hook and resolve run outside the bus.
            let entry = settle_or_restore(&mut bus, &session, *consumed).unwrap();
            bus.drain_events().deliver();
            (entry.resolve)(DialogAnswer::Answered(value.clone()));
            assert_eq!(
                field(field(expected, "result"), "delivered").as_bool(),
                Some(true)
            );
        }
        assert_eq!(
            bus.pending_count(&session),
            number(expected, "pending") as usize
        );
    }
    assert_eq!(callbacks.resolved.lock().unwrap().len(), 1);
    assert_eq!(callbacks.settled.load(Ordering::SeqCst), 1);
}

#[test]
fn crash_reopen_and_owner_expiry_do_not_transfer_renderer_or_native_waiter() {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let key = key(&fixture);
    let request = "dialog-1".into();
    let owner = "owner-A".into();
    let lease = "fixed-id".into();
    let first = store(&scratch.0, &clock);
    seed_hosted(&first, &clock, &fixture);
    let before = fs::read(first.record_path(&key).unwrap()).unwrap();
    drop(first); // In-process waiters vanish, durable state survives.
    let reopened = store(&scratch.0, &clock);
    assert_eq!(reopened.list(&key).unwrap(), vec![dialog(&fixture)]);
    assert_eq!(reopened.list_live(&key).unwrap(), vec![dialog(&fixture)]);
    let callbacks = Arc::new(Callbacks::default());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-lease"), None);
    assert_eq!(bus.pending_count(&"session".into()), 0);
    equal(
        &object([(
            "dialogs",
            JsValue::Array(
                reopened
                    .list_live(&key)
                    .unwrap()
                    .into_iter()
                    .map(|d| d.request)
                    .collect(),
            ),
        )]),
        field(&fixture, "hostedProjection"),
        "hosted projection",
    );
    bus.register(
        &"session".into(),
        pending(field(&fixture, "request"), &callbacks),
    );
    equal(
        &object([("dialogs", JsValue::Array(bus.list(&"session".into())))]),
        field(&fixture, "liveProjection"),
        "live projection",
    );
    let consumed = bus
        .consume(&"session".into(), &request, "closed")
        .expect("pending entry");
    settle_or_restore(&mut bus, &"session".into(), consumed).unwrap();
    bus.drain_events().deliver();
    clock.fetch_add(1000, Ordering::SeqCst);
    assert!(reopened.list_live(&key).unwrap().is_empty());
    assert_eq!(
        fs::read(reopened.record_path(&key).unwrap()).unwrap(),
        before,
        "list must not write expired owner"
    );
    assert!(
        !reopened
            .renew_live_owner(&key, &request, &owner, 1000.0)
            .unwrap()
    );
    assert!(matches!(
        reopened.claim_live(&key, &request, 1000.0, None).unwrap(),
        LeaseClaim::Busy { .. }
    ));
    assert!(
        !reopened
            .submit_live_answer(&key, &request, None, &DialogAnswer::Cancelled(None))
            .unwrap()
    );
    assert!(
        reopened
            .submit_live_answer(&key, &request, Some(&lease), &DialogAnswer::Cancelled(None))
            .unwrap()
    );
    let durable = reopened.list(&key).unwrap().remove(0);
    let recovered = object([
        ("type", "user_dialog_terminated".into()),
        ("request", durable.request),
        ("reason", "gateway_restarted".into()),
        ("terminatedAt", JsValue::String(durable.created_at)),
        ("recovery", "next_turn_context".into()),
    ]);
    equal(
        &object([("dialogs", JsValue::Array(vec![recovered]))]),
        field(&fixture, "recoveredProjection"),
        "recovery contract projection",
    );
    // Owner release never releases the renderer's bearer lease.
    assert!(matches!(
        reopened
            .claim_live_owner(&key, &request, &"owner-B".into(), 5000.0)
            .unwrap(),
        OwnerClaim::Owned { .. }
    ));
    assert!(
        reopened
            .release_live_owner(&key, &request, &"owner-B".into())
            .unwrap()
    );
    assert!(matches!(
        reopened.claim_live(&key, &request, 1000.0, None).unwrap(),
        LeaseClaim::Busy { .. }
    ));
    clock.fetch_add(2000, Ordering::SeqCst);
    assert!(!reopened.release_live(&key, &request, &lease).unwrap());
    assert!(matches!(
        reopened.claim_live(&key, &request, 1000.0, None).unwrap(),
        LeaseClaim::Claimed(_)
    ));
}

fn failure_contract(fault: store::Fault) {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let key = key(&fixture);
    let id = "dialog-1".into();
    let mut store = store(&scratch.0, &clock);
    seed_hosted(&store, &clock, &fixture);
    let path = store.record_path(&key).unwrap();
    let before = fs::read(&path).unwrap();
    store.fault = Some(fault);
    assert!(
        store.take_live_answer(&key, &id).is_err(),
        "must not publish a consumed answer on IO failure"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        store
            .submit_live_answer(
                &key,
                &id,
                Some(&"fixed-id".into()),
                &DialogAnswer::Cancelled(None)
            )
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1,
        "only committed file; no lock/temp leakage"
    );
    store.fault = None;
    assert!(store.take_live_answer(&key, &id).unwrap().is_some());
    assert!(store.take_live_answer(&key, &id).unwrap().is_none());
}
#[test]
fn partial_write_failure_preserves_answer_and_releases_lock() {
    failure_contract(store::Fault::Write);
}
#[test]
fn rename_failure_preserves_answer_and_cleans_temporary_file() {
    failure_contract(store::Fault::Rename);
}

#[test]
fn concurrent_independent_stores_have_one_renderer_winner() {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let key = key(&fixture);
    let store = store(&scratch.0, &clock);
    store.put(&key, &dialog(&fixture)).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for renderer in ["renderer-A", "renderer-B"] {
        let mut options = store_options(&scratch.0, &clock);
        options.runtime = runtime(&clock, renderer);
        let barrier = barrier.clone();
        let key = key.clone();
        threads.push(thread::spawn(move || {
            let store = FileGatewayUserDialogStore::new(options).unwrap();
            barrier.wait();
            store
                .claim_live(&key, &"dialog-1".into(), 1000.0, None)
                .unwrap()
        }));
    }
    barrier.wait();
    let claims: Vec<_> = threads
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim, LeaseClaim::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim, LeaseClaim::Busy { .. }))
            .count(),
        1
    );
    assert_eq!(store.list(&key).unwrap().len(), 1);
}

#[test]
fn lock_conflict_is_bounded_and_stale_crash_lock_is_reclaimed() {
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let key = key(&fixture);
    let mut options = store_options(&scratch.0, &clock);
    options.lock_timeout = Duration::from_millis(20);
    options.lock_retry = Duration::from_millis(2);
    let store = FileGatewayUserDialogStore::new(options).unwrap();
    store.put(&key, &dialog(&fixture)).unwrap();
    let path = store.record_path(&key).unwrap();
    let lock = path.with_file_name("dialogs.json.lock");
    fs::write(&lock, "other-process\n").unwrap();
    let before = fs::read(&path).unwrap();
    let error = store
        .claim_live(&key, &"dialog-1".into(), 1000.0, None)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("Timed out waiting for Gateway user-dialog store lock")
    );
    assert_eq!(fs::read(&lock).unwrap(), b"other-process\n");
    assert_eq!(fs::read(&path).unwrap(), before);
    let handle = fs::File::options().write(true).open(&lock).unwrap();
    handle
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(60)))
        .unwrap();
    assert!(matches!(
        store
            .claim_live(&key, &"dialog-1".into(), 1000.0, None)
            .unwrap(),
        LeaseClaim::Claimed(_)
    ));
    assert!(!lock.exists());
}

#[test]
fn session_isolation_native_cleanup_and_duplicate_registration() {
    let fixture = fixture();
    let clock = clock(&fixture);
    let callbacks = Arc::new(Callbacks::default());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-lease"), None);
    let id = "dialog-1".into();
    for session in ["a", "b"] {
        bus.register(
            &session.into(),
            pending(field(&fixture, "request"), &callbacks),
        );
    }
    bus.claim(&"a".into(), &id, 1000.0, None).unwrap();
    bus.register(&"a".into(), pending(field(&fixture, "request"), &callbacks));
    assert_eq!(bus.pending_count(&"a".into()), 1);
    assert!(matches!(
        bus.consume_for_response(&"a".into(), &id, None, "answered", None),
        ConsumeResult::LeaseRequired
    ));
    let consumed = bus
        .consume(&"a".into(), &id, "abort")
        .expect("consumed entry");
    settle_or_restore(&mut bus, &"a".into(), consumed).unwrap();
    bus.drain_events().deliver();
    assert!(bus.has_pending(&"b".into(), &id));
    let scratch = Scratch::new();
    let store = store(&scratch.0, &clock);
    let first = key(&fixture);
    let mut second = first.clone();
    second.project_root = "/different/project".into();
    store.put(&first, &dialog(&fixture)).unwrap();
    assert!(store.list(&second).unwrap().is_empty());
    store.put(&second, &dialog(&fixture)).unwrap();
    store.clear(&first).unwrap();
    assert_eq!(store.list(&second).unwrap().len(), 1);
}

#[test]
#[ignore = "requires PDRS_BASELINE and its pinned Node/tsx installation"]
fn real_ts_reads_rust_and_continues_store() {
    let baseline =
        std::env::var_os("PDRS_BASELINE").expect("PDRS_BASELINE must name fixed TS baseline");
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let store = store(&scratch.0, &clock);
    seed_hosted(&store, &clock, &fixture);
    let output = Command::new("node")
        .arg("--import")
        .arg(Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/user-dialog/generate.mts"))
        .arg(&scratch.0)
        .arg("--verify-rust")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS: real TS"));
    let dialogs = store.list(&key(&fixture)).unwrap();
    assert_eq!(dialogs.len(), 2);
    assert!(
        string(&dialogs[1].request, "requestId")
            .unwrap()
            .equals_str("ts-after-rust")
    );
    assert!(
        store
            .take_live_answer(&key(&fixture), &"dialog-1".into())
            .unwrap()
            .is_none()
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn put_without_string_request_id_appends_like_ts_then_reads_reject() {
    // TS put（FileGatewayUserDialogStore.ts:93-114）不校验 requestId：写盘成功，之后读端 admission 拒绝整份记录。
    let fixture = fixture();
    let scratch = Scratch::new();
    let clock = clock(&fixture);
    let store = store(&scratch.0, &clock);
    let key = key(&fixture);
    let request =
        pilotdeck_protocol::parse_json(r#"{"type":"user_dialog_request","dialogKind":"input"}"#)
            .unwrap();
    let dialog = StoredDialog {
        request,
        created_at: JsString::from("2026-10-18T00:00:00.000Z"),
    };
    store
        .put(&key, &dialog)
        .expect("TS writes without validating requestId");
    assert!(store.record_path(&key).unwrap().exists());
    assert!(
        store.list(&key).is_err(),
        "read admission rejects the stored record, as in TS"
    );
}

#[test]
fn put_with_non_object_request_writes_like_ts() {
    // 真实 TS 探针（协调者实测）：request 为 null / 数组 / 字符串时 TS put 均写盘成功（不抛 TypeError），Rust 同。
    let fixture = fixture();
    let clock = clock(&fixture);
    for raw in ["null", "[]", "\"x\""] {
        let scratch = Scratch::new();
        let store = store(&scratch.0, &clock);
        let key = key(&fixture);
        let request = pilotdeck_protocol::parse_json(raw).unwrap();
        let dialog = StoredDialog {
            request,
            created_at: JsString::from("x"),
        };
        store
            .put(&key, &dialog)
            .unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert!(store.record_path(&key).unwrap().exists(), "{raw}");
    }
}

#[test]
fn put_null_request_into_nonempty_record_fails_without_writing_like_ts() {
    // 真实 TS 探针（_tmp/20261018-a68-dialog/ts-probe-nonempty.log）：已有 dialog 时 request=null 抛 TypeError、不写盘；
    // 字符串/数字/数组/缺 requestId 的对象照常追加。
    let fixture = fixture();
    let clock = clock(&fixture);
    let first = pilotdeck_protocol::parse_json(
        r#"{"type":"user_dialog_request","requestId":"dialog-1","dialogKind":"input"}"#,
    )
    .unwrap();
    for (raw, ok) in [
        ("null", false),
        ("\"x\"", true),
        ("1", true),
        ("[]", true),
        (r#"{"type":"user_dialog_request"}"#, true),
    ] {
        let scratch = Scratch::new();
        let store = store(&scratch.0, &clock);
        let key = key(&fixture);
        store
            .put(
                &key,
                &StoredDialog {
                    request: first.clone(),
                    created_at: JsString::from("x"),
                },
            )
            .unwrap();
        let before = fs::read(store.record_path(&key).unwrap()).unwrap();
        let dialog = StoredDialog {
            request: pilotdeck_protocol::parse_json(raw).unwrap(),
            created_at: JsString::from("x"),
        };
        assert_eq!(store.put(&key, &dialog).is_ok(), ok, "{raw}");
        if !ok {
            assert_eq!(
                fs::read(store.record_path(&key).unwrap()).unwrap(),
                before,
                "{raw}: must not write"
            );
        }
    }
}

fn numbered_pending(request_id: &str, accepts: AcceptsFn) -> PendingDialog {
    PendingDialog {
        request_id: request_id.into(),
        dialog_kind: "input".into(),
        tool_call_id: format!("call-{request_id}").into(),
        tool_name: "request_user_input".into(),
        event: crate::metadata::object([("requestId", JsValue::String(request_id.into()))]),
        accepts,
        resolve: Box::new(|_answer| {}),
        reject: Box::new(|_error| {}),
        on_settled: None,
    }
}

fn listed_request_ids(bus: &mut GatewayUserDialogBus, session: &JsString) -> Vec<String> {
    bus.list(session)
        .iter()
        .map(|event| {
            event
                .get("requestId")
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok())
                .unwrap_or_default()
        })
        .collect()
}

/// #1599: a settle failure must put the dialog back where it was. TS
/// `remove()` mutates nothing when `onSettled` throws
/// (GatewayUserDialogBus.ts:219-226), so appending to the bucket tail would
/// reorder `user_dialog_list` against the baseline.
#[test]
fn restore_keeps_the_consumed_dialog_at_its_original_position() {
    use crate::negative_assert_eq;

    let clock = clock(&fixture());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-restore-order"), None);
    let session: JsString = "restore-order".into();
    for id in ["alpha", "beta", "gamma"] {
        bus.register(&session, numbered_pending(id, Arc::new(|_value| true)));
    }
    let consumed = bus
        .consume(&session, &"beta".into(), "abort")
        .expect("beta is pending");
    assert!(
        bus.restore(&session, consumed).is_none(),
        "#1599 the entry is restored, not handed back as a replaced waiter"
    );
    negative_assert_eq!(
        listed_request_ids(&mut bus, &session),
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "#1599 restore puts the consumed dialog back at its original position"
    );
}

/// #1599: the settle hook may reenter the bus and remove or insert other
/// members before failing. TS `remove()` leaves the bucket untouched when the
/// hook throws, so the consumed entry keeps its place *relative to the
/// survivors*; a saved numeric index would put it on the wrong side of them.
#[test]
fn restore_keeps_the_relative_position_after_a_reentrant_removal() {
    use crate::negative_assert_eq;

    let clock = clock(&fixture());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-restore-shift"), None);
    let session: JsString = "restore-shift".into();
    for id in ["alpha", "beta", "gamma"] {
        bus.register(&session, numbered_pending(id, Arc::new(|_value| true)));
    }
    let consumed = bus
        .consume(&session, &"beta".into(), "abort")
        .expect("beta is pending");
    // The settle hook reentered and consumed the member before beta, then
    // failed.
    bus.consume(&session, &"alpha".into(), "abort")
        .expect("alpha is pending");
    assert!(
        bus.restore(&session, consumed).is_none(),
        "#1599 the entry is restored, not handed back as a replaced waiter"
    );
    negative_assert_eq!(
        listed_request_ids(&mut bus, &session),
        vec!["beta".to_string(), "gamma".to_string()],
        "#1599 restore keeps the consumed dialog before the member that followed it"
    );
}

/// #1599: the bus guard is released between the snapshot and the consume, so
/// a host callback can reenter and replace the dialog under the same request
/// id. The answer was validated against the old dialog and must not be
/// delivered to the new one, even when the replacement reuses the very same
/// validator handle.
#[test]
fn consume_for_response_refuses_a_dialog_replaced_after_the_snapshot() {
    use crate::negative_assert;

    let clock = clock(&fixture());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-replaced"), None);
    let session: JsString = "replaced".into();
    let request: JsString = "replaced-request".into();
    let first: AcceptsFn = Arc::new(|_value| true);
    bus.register(
        &session,
        numbered_pending("replaced-request", first.clone()),
    );
    let snapshot = bus
        .snapshot_for_response(&session, &request)
        .expect("the dialog is pending");
    // A host callback reenters the bus and registers a different dialog under
    // the same request id. It may reuse the same host validator handle (a
    // shared `Arc<dyn Fn>`), so identity must come from the registration, not
    // from the validator pointer.
    bus.register(
        &session,
        numbered_pending("replaced-request", first.clone()),
    );
    let consumed = bus.consume_for_response(&session, &request, None, "answered", Some(&snapshot));
    negative_assert!(
        matches!(consumed, ConsumeResult::Replaced),
        "#1599 an answer validated against a replaced dialog must not be delivered"
    );
    negative_assert!(
        matches!(
            bus.consume_for_response(&session, &request, None, "answered", None),
            ConsumeResult::Entry(_)
        ),
        "#1599 without a validated snapshot the pending dialog is consumed as before"
    );
}

/// #1599: a settle hook is host code and runs outside the guard. If it
/// reenters the bus and registers a new dialog under the same request id
/// before failing, that registration is the live one — TS `remove()` throws
/// before it mutates (GatewayUserDialogBus.ts:219-226), so the reentrant entry
/// survives. Restoring the consumed entry (and its lease) over it would drop
/// the new waiter.
#[test]
fn restore_keeps_a_registration_that_replaced_the_consumed_dialog() {
    use crate::negative_assert;

    let clock = clock(&fixture());
    let mut bus = GatewayUserDialogBus::new(runtime(&clock, "bus-restore-conflict"), None);
    let session: JsString = "restore-conflict".into();
    let request: JsString = "dialog".into();
    bus.register(
        &session,
        numbered_pending("dialog", Arc::new(|_value| true)),
    );
    bus.register(&session, numbered_pending("tail", Arc::new(|_value| true)));
    let consumed = bus
        .consume(&session, &request, "answered")
        .expect("dialog is pending");
    // The settle hook reentered and registered the replacement, then failed.
    let replacement: AcceptsFn = Arc::new(|_value| false);
    let generation = bus.register(&session, numbered_pending("dialog", replacement.clone()));
    let dropped = bus.restore(&session, consumed);
    // The reentrant registration is the live one, so check that first: a
    // `restore` that drops the conflict branch does not merely misplace the
    // consumed entry, it re-inserts it (and its generation and lease) over the
    // replacement. That shows up on the snapshot, while "did the displaced
    // waiter come back" only tells us whether the early return ran at all.
    let snapshot = bus
        .snapshot_for_response(&session, &request)
        .expect("the reentrant registration is still pending");
    negative_assert!(
        snapshot.generation == generation,
        "#1599 restore must not overwrite the reentrant registration"
    );
    negative_assert!(
        Arc::ptr_eq(&snapshot.accepts, &replacement),
        "#1599 the reentrant registration keeps its own validator"
    );
    negative_assert!(
        matches!(&dropped, Some(entry) if entry.request_id == request),
        "#1599 the displaced waiter comes back to be ended"
    );
    negative_assert!(
        listed_request_ids(&mut bus, &session) == vec!["tail".to_string(), "dialog".to_string()],
        "#1599 restore must not duplicate or drop either registration"
    );
}
