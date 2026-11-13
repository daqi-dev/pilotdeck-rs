//! Real TS lifecycle scenarios replayed through controlled local channels.
use super::{loader::*, protocol::*, server::ChannelServer, slack};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

const LIFECYCLE: &str = include_str!("../../tests/fixtures/channel-lifecycle/oracle.json");
const LOADER: &str = include_str!("../../tests/fixtures/channel-loader/oracle.json");
fn oracle(raw: &str) -> Value {
    crate::test_baseline::oracle(raw);
    serde_json::from_str(raw).expect("#1327 oracle JSON")
}
fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .expect("#1327 oracle text field")
        .into()
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("#1327 oracle string array")
        .iter()
        .map(|v| v.as_str().expect("#1327 oracle string").into())
        .collect()
}
/// The terminal comparison reads `expected` through a checked accessor instead
/// of a bare index (#1808, the #1729 shape): a present-but-wrong-type
/// container fails with a `#1327` diagnostic instead of a generic object
/// mismatch, and an absent one cannot shrink the case to a blind compare.
///
/// Deliberately narrower than sms/webhook's `recorded_checked`: every case in
/// these oracles must carry an object `expected`, so absent is an error rather
/// than a `fail_open` switch, and the two-state `Result<&Value, _>` keeps the
/// call sites free of an `Option` hop. Sharing one helper across all five
/// harnesses is follow-up work, not this lane (#1813 owns the other files).
fn recorded_expected_checked(value: &Value) -> Result<&Value, String> {
    match value.get("expected") {
        None => Err("#1327 expected must be an object, got <missing>".into()),
        Some(expected) if expected.is_object() => Ok(expected),
        Some(expected) => Err(format!("#1327 expected must be an object, got {expected}")),
    }
}
fn recorded_expected(value: &Value) -> &Value {
    recorded_expected_checked(value).unwrap_or_else(|error| panic!("{error}"))
}
#[test]
fn recorded_expected_rejects_wrong_type() {
    crate::negative_assert!(
        recorded_expected_checked(&json!({"expected": []})).is_err(),
        "#1327 #1808 rejects expected type drift"
    );
    crate::negative_assert!(
        recorded_expected_checked(&json!({})).is_err(),
        "#1327 #1808 rejects absent expected"
    );
}
#[test]
#[should_panic(expected = "#1327 expected must be an object")]
fn recorded_expected_panics_on_wrong_type() {
    recorded_expected(&json!({"expected": []}));
}
#[test]
#[should_panic(expected = "#1327 expected must be an object, got <missing>")]
fn recorded_expected_panics_when_absent() {
    recorded_expected(&json!({}));
}
fn error_value(error: ChannelError) -> Value {
    if let ChannelError::Aggregate { errors, .. } = &error {
        json!({"error":error.to_js_string(),"errors":errors.iter().map(ChannelError::to_js_string).collect::<Vec<_>>()})
    } else {
        json!({"error":error.to_js_string()})
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("#1327 lifecycle bounded wait")
}
async fn until(predicate: impl Fn() -> bool) {
    bounded(async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
#[derive(Default)]
struct Record {
    traces: BTreeMap<String, Vec<Value>>,
    reports: BTreeMap<String, Vec<Value>>,
    logs: Vec<Value>,
    host_closes: usize,
}
impl Record {
    fn event(&mut self, id: &str, event: Value) {
        self.traces
            .get_mut(id)
            .expect("#1327 registered mock")
            .push(event);
    }
    fn snapshot(&self) -> Value {
        json!({"traces":self.traces,"reports":self.reports,"hostCloses":self.host_closes})
    }
}
type Gate = Arc<watch::Sender<bool>>;
type Gates = Arc<Mutex<HashMap<String, Gate>>>;
fn gate(gates: &Gates, name: &str) -> Gate {
    gates
        .lock()
        .expect("#1327 gate mutex")
        .entry(name.into())
        .or_insert_with(|| Arc::new(watch::Sender::new(false)))
        .clone()
}
async fn released(gate: Gate) {
    let mut rx = gate.subscribe();
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("#1327 gate sender retained");
    }
}
#[derive(Default)]
struct MockState {
    open: bool,
    starts: u64,
    stops: u64,
    flights: Vec<tokio::task::JoinHandle<()>>,
}
struct Mock {
    id: String,
    key: ChannelKey,
    script: Value,
    state: Arc<Mutex<MockState>>,
    record: Arc<Mutex<Record>>,
    gates: Gates,
    config: Arc<JsValue>,
}
impl Mock {
    fn event(&self, event: Value) {
        self.record
            .lock()
            .expect("#1327 record mutex")
            .event(&self.id, event);
    }
    fn receive(&self, message: String, gate_name: &str) {
        let mut state = self.state.lock().expect("#1327 mock mutex");
        self.event(json!({"event":if state.open {"accepted"}else{"rejected"},"message":message}));
        if !state.open {
            return;
        }
        let record = self.record.clone();
        let id = self.id.clone();
        let wait = gate(&self.gates, gate_name);
        state.flights.push(tokio::spawn(async move {
            released(wait).await;
            record
                .lock()
                .expect("#1327 record mutex")
                .event(&id, json!({"event":"complete","message":message}));
        }));
    }
}
impl ChannelAdapter for Mock {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        assert!(
            deps.config
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(c, &self.config)),
            "#1327 config identity boundary"
        );
        let attempt = {
            let mut state = self.state.lock().expect("#1327 mock mutex");
            state.starts += 1;
            state.starts
        };
        self.event(json!({"event":"start-begin"}));
        let fail = self.script["failStarts"]
            .as_array()
            .is_some_and(|xs| xs.contains(&json!(attempt)));
        let start_gate = self.script["startGate"]
            .as_str()
            .map(|name| gate(&self.gates, name));
        let handle = Arc::new(MockHandle {
            id: self.id.clone(),
            script: self.script.clone(),
            state: self.state.clone(),
            record: self.record.clone(),
            gates: self.gates.clone(),
        });
        Box::pin(async move {
            if let Some(gate) = start_gate {
                released(gate).await;
            }
            if fail {
                return Err(ChannelError::error(format!("{} start failed", handle.id)));
            }
            handle.state.lock().expect("#1327 mock mutex").open = true;
            handle
                .record
                .lock()
                .expect("#1327 record mutex")
                .event(&handle.id, json!({"event":"start-end"}));
            Ok(handle as Arc<dyn ChannelHandle>)
        })
    }
}
struct MockHandle {
    id: String,
    script: Value,
    state: Arc<Mutex<MockState>>,
    record: Arc<Mutex<Record>>,
    gates: Gates,
}
impl ChannelHandle for MockHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let (attempt, flights) = {
            let mut state = self.state.lock().expect("#1327 mock mutex");
            state.open = false;
            state.stops += 1;
            (state.stops, std::mem::take(&mut state.flights))
        };
        self.record
            .lock()
            .expect("#1327 record mutex")
            .event(&self.id, json!({"event":"stop-begin","reason":reason}));
        let fail = self.script["failStops"]
            .as_array()
            .is_some_and(|xs| xs.contains(&json!(attempt)));
        let stop_gate = self.script["stopGate"]
            .as_str()
            .map(|name| gate(&self.gates, name));
        let id = self.id.clone();
        let state = self.state.clone();
        let record = self.record.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            if let Some(gate) = stop_gate {
                released(gate).await;
            }
            if fail {
                state
                    .lock()
                    .expect("#1327 mock mutex")
                    .flights
                    .extend(flights);
                return Err(ChannelError::error(format!("{id} stop failed")));
            }
            for flight in flights {
                flight.await.expect("#1327 in-flight task completed");
            }
            record
                .lock()
                .expect("#1327 record mutex")
                .event(&id, json!({"event":"stop-end","reason":reason}));
            Ok(())
        })
    }
}

async fn replay(case: &Value) -> Value {
    replay_with_claim_race(case, false).await
}
async fn replay_with_claim_race(case: &Value, claim_race: bool) -> Value {
    let record = Arc::new(Mutex::new(Record::default()));
    let gates = Gates::default();
    let config =
        Arc::new(pilotdeck_protocol::parse_json("{\"probe\":true}").expect("#1327 config JSON"));
    let reporter = record.clone();
    let logger = record.clone();
    let host = record.clone();
    let close_error = case["closeError"].as_str().map(str::to_owned);
    let server = ChannelServer::new(
        ChannelStartDeps {
            config: Some(config.clone()),
            report_status: Some(Arc::new(move |update| {
                let state = match update.state {
                    ChannelState::Starting => "starting",
                    ChannelState::Connected => "connected",
                    ChannelState::Stopped => "stopped",
                    ChannelState::Failed => "failed",
                };
                let mut value = json!({"state":state,"message":update.message});
                if let Some(error) = update.error {
                    value["error"] = json!(error);
                }
                reporter
                    .lock()
                    .expect("#1327 record mutex")
                    .reports
                    .entry(update.key.to_string())
                    .or_default()
                    .push(value);
            })),
            log: Some(Arc::new(move |level, message| {
                logger
                    .lock()
                    .expect("#1327 record mutex")
                    .logs
                    .push(json!([level, message]))
            })),
            turn_gateway: None,
        },
        Arc::new(move || {
            host.lock().expect("#1327 record mutex").host_closes += 1;
            let error = close_error.clone();
            Box::pin(async move { error.map_or(Ok(()), |e| Err(ChannelError::error(e))) })
        }),
    );
    let close_actors = Arc::new(Mutex::new(Vec::new()));
    let hook_hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let atomic_snapshot = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut channels: HashMap<String, Arc<Mock>> = HashMap::new();
    let mut pending: HashMap<String, BoxFuture<Value>> = HashMap::new();
    let mut results = Vec::new();
    let mut snapshots = Vec::new();
    for step in case["steps"].as_array().expect("#1327 lifecycle steps") {
        let mut future: Option<BoxFuture<Value>> = None;
        match step["op"].as_str().expect("#1327 lifecycle operation") {
            "arm-hook" => {
                let kind = text(step, "kind");
                let target = server.clone();
                let hits = hook_hits.clone();
                let atomic = atomic_snapshot.clone();
                let gates = gates.clone();
                let actors = close_actors.clone();
                let fired = std::sync::atomic::AtomicBool::new(false);
                let (close_entered, close_entry) = std::sync::mpsc::channel();
                let close_entry = Mutex::new(close_entry);
                server.set_hook(Some(Arc::new(move |point, key| {
                    use std::sync::atomic::Ordering::SeqCst;
                    if claim_race && point == "close-entered" {
                        close_entered
                            .send(())
                            .expect("#1327 claim race close entry");
                        return;
                    }
                    if *key != ChannelKey::new("a") {
                        return;
                    }
                    if claim_race {
                        if kind == "replace-snapshot" && point == "replace-snapshotted" {
                            assert!(
                                !fired.swap(true, SeqCst),
                                "#1327 claim race target hook only once"
                            );
                            hits.lock().expect("#1327 target hooks").push(kind.clone());
                            let (observed, observation) = std::sync::mpsc::channel();
                            let locked = !target.state_available();
                            let target = target.clone();
                            let gates = gates.clone();
                            let actor = tokio::task::spawn_blocking(move || {
                                // The replacement is paused after its snapshot.
                                // If its lock is held, close must wait for claim;
                                // otherwise close completes its synchronous claim
                                // before the replacement is allowed to continue.
                                let close = target.close();
                                if !locked {
                                    // Clean up the broken path without using a
                                    // wait-host timeout as mutation evidence.
                                    gate(&gates, "stop").send_replace(true);
                                    observed
                                        .send(())
                                        .expect("#1327 claim race close observation");
                                }
                                close
                            });
                            actors.lock().expect("#1327 claim race actors").push(actor);
                            close_entry
                                .lock()
                                .expect("#1327 claim race close entry receiver")
                                .recv_timeout(Duration::from_secs(5))
                                .expect("#1327 claim race close entered snapshot window");
                            if !locked {
                                observation
                                    .recv_timeout(Duration::from_secs(5))
                                    .expect("#1327 claim race close claimed before replacement");
                            }
                        }
                        return;
                    }
                    if kind == "replace-snapshot" && point == "replace-checked" {
                        let locked = !target.state_available();
                        atomic.store(locked, SeqCst);
                        if !locked {
                            // Reproduce close in the broken gap, but release the
                            // old tail so a failing assertion never relies on timeout.
                            fired.store(true, SeqCst);
                            hits.lock().expect("#1327 target hooks").push(kind.clone());
                            drop(target.close());
                            gate(&gates, "stop").send_replace(true);
                        }
                    }
                    if point == "stop-claimed" && !fired.swap(true, SeqCst) {
                        hits.lock().expect("#1327 target hooks").push(kind.clone());
                        drop(target.close());
                    }
                })));
            }
            "wait-host" => {
                until(|| record.lock().expect("#1327 host record").host_closes == 1).await;
                // Complete close before the next snapshot/release step: the
                // predecessor's asynchronous stop is still gated at this point.
                let actors =
                    std::mem::take(&mut *close_actors.lock().expect("#1327 claim race actors"));
                for actor in actors {
                    let close = bounded(actor)
                        .await
                        .expect("#1327 claim race close actor completed");
                    assert_eq!(
                        bounded(close)
                            .await
                            .map_or_else(error_value, |()| Value::Null),
                        Value::Null,
                        "#1327 claim race close result null"
                    );
                }
            }
            "create" => {
                let id = text(step, "id");
                record
                    .lock()
                    .expect("#1327 record mutex")
                    .traces
                    .insert(id.clone(), Vec::new());
                channels.insert(
                    id.clone(),
                    Arc::new(Mock {
                        id,
                        key: ChannelKey::new(text(step, "key")),
                        script: step.clone(),
                        state: Arc::new(Mutex::new(MockState::default())),
                        record: record.clone(),
                        gates: gates.clone(),
                        config: config.clone(),
                    }),
                );
            }
            "hot" => {
                let operation = server.hot_start_channel(channels[&text(step, "id")].clone());
                future = Some(Box::pin(async move {
                    operation.await.map_or_else(error_value, |()| Value::Null)
                }));
            }
            "close" => {
                let operation = server.close();
                future = Some(Box::pin(async move {
                    operation.await.map_or_else(error_value, |()| Value::Null)
                }));
            }
            "reconcile" => {
                let operation = server.reconcile_channels(ReconcileInput {
                    channels: strings(&step["ids"])
                        .iter()
                        .map(|id| channels[id].clone() as Arc<dyn ChannelAdapter>)
                        .collect(),
                    managed_channel_keys: strings(&step["managed"])
                        .into_iter()
                        .map(ChannelKey::new)
                        .collect(),
                });
                future = Some(Box::pin(async move {
                    operation.await.map_or_else(error_value,|result|json!({"started":result.started.iter().map(ToString::to_string).collect::<Vec<_>>(),"stopped":result.stopped.iter().map(ToString::to_string).collect::<Vec<_>>()}))
                }));
            }
            "background" => server.start_channels(
                strings(&step["ids"])
                    .iter()
                    .map(|id| channels[id].clone() as Arc<dyn ChannelAdapter>)
                    .collect(),
            ),
            "receive" => {
                channels[&text(step, "id")].receive(text(step, "message"), &text(step, "gate"))
            }
            "release" => {
                gate(&gates, &text(step, "gate")).send_replace(true);
            }
            "wait" => {
                until(|| {
                    record.lock().expect("#1327 record mutex").traces[&text(step, "id")]
                        .iter()
                        .any(|v| v["event"] == step["event"])
                })
                .await
            }
            "wait-status" => {
                until(|| {
                    record
                        .lock()
                        .expect("#1327 record mutex")
                        .reports
                        .get(&text(step, "key"))
                        .and_then(|reports| reports.last())
                        .is_some_and(|report| report["state"] == step["state"])
                })
                .await
            }
            "wait-log" => {
                until(|| {
                    record
                        .lock()
                        .expect("#1327 record mutex")
                        .logs
                        .iter()
                        .any(|v| v[1] == step["text"])
                })
                .await
            }
            "snapshot" => snapshots.push(record.lock().expect("#1327 record mutex").snapshot()),
            "drop" => {
                drop(
                    pending
                        .remove(&text(step, "pending"))
                        .expect("#1327 pending waiter to abandon"),
                );
            }
            "await" => results.push(
                bounded(
                    pending
                        .remove(&text(step, "pending"))
                        .expect("#1327 pending operation"),
                )
                .await,
            ),
            other => panic!("#1327 unknown lifecycle operation {other}"),
        }
        if let Some(future) = future {
            if let Some(label) = step["pending"].as_str() {
                pending.insert(label.into(), future);
            } else {
                results.push(bounded(future).await);
            }
        }
    }
    server.set_hook(None);
    crate::negative_assert!(
        atomic_snapshot.load(std::sync::atomic::Ordering::SeqCst),
        "#1327 replace snapshot atomic"
    );
    let hits = hook_hits.lock().expect("#1327 target hooks");
    if let Some(step) = case["steps"]
        .as_array()
        .expect("#1327 hook steps")
        .iter()
        .find(|s| s["op"] == "arm-hook")
    {
        assert_eq!(
            *hits,
            vec![text(step, "kind")],
            "#1327 target hook hit exactly once"
        );
    }
    let record = record.lock().expect("#1327 record mutex");
    let mut actual = json!({"traces":record.traces,"reports":record.reports,"results":results,"snapshots":snapshots,"logs":record.logs,"hostCloses":record.host_closes});
    if !hits.is_empty() {
        actual["hookHits"] = json!(*hits);
    }
    actual
}
#[tokio::test]
async fn lifecycle_matches_real_ts() {
    let oracle = oracle(LIFECYCLE);
    let cases = lifecycle_cases(&oracle);
    for case in cases {
        let actual = replay(case).await;
        let expected = recorded_expected(case).clone();
        crate::negative_assert_eq!(actual, expected, "#1327 lifecycle oracle");
    }
}

struct Stub(ChannelKey);
impl ChannelAdapter for Stub {
    fn channel_key(&self) -> &ChannelKey {
        &self.0
    }
    fn start(
        &self,
        _deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("loader stub must not start")) })
    }
}
fn describe(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined => json!("<undefined>"),
        JsValue::Object(object) => Value::Object(
            object
                .own_entries()
                .into_iter()
                .map(|(k, v)| (k.to_utf8().expect("#1327 loader key"), describe(v)))
                .collect(),
        ),
        JsValue::Array(items) => Value::Array(items.iter().map(describe).collect()),
        _ => serde_json::from_str(
            &pilotdeck_protocol::stringify_json(value)
                .expect("#1327 stringify value")
                .expect("#1327 defined value"),
        )
        .expect("#1327 loader value"),
    }
}
#[test]
fn loader_matches_real_ts() {
    let oracle = oracle(LOADER);
    let cases = loader_cases(&oracle);
    for case in cases {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut mounts = ChannelLoaderMounts::default();
        for key in CHANNEL_LOADER_KEYS {
            let output = events.clone();
            let failure = case["failures"][key].clone();
            let import_fails = case["importFailures"][key].as_str().map(str::to_owned);
            mounts.register(
                key,
                Arc::new(move |options: &JsObject| {
                    if let Some(error) = &import_fails {
                        return Err(ChannelError::error(error));
                    }
                    output.lock().expect("#1327 loader recording").push(
                        json!({"kind":"construction","key":key,"options":options.own_entries().into_iter().map(|(key,value)|json!([key.to_utf8().expect("#1327 option key"),describe(value)])).collect::<Vec<_>>()}),
                    );
                    if let Some(message) = failure["error"].as_str() {
                        return Err(ChannelError::error(message));
                    }
                    if let Some(message) = failure["plain"].as_str() {
                        return Err(ChannelError::Plain(message.into()));
                    }
                    Ok(Arc::new(Stub(ChannelKey::new(key))) as Arc<dyn ChannelAdapter>)
                }),
            );
        }
        let adapters = pilotdeck_protocol::parse_json(&case["adapters"].to_string())
            .expect("#1327 adapters JSON");
        let result = load_enabled_channels(
            if case["adaptersUndefined"] == true {
                None
            } else {
                Some(&adapters)
            },
            &LoadEnabledChannelsOptions {
                pilot_home: case["options"]["pilotHome"].as_str().map(JsString::from),
            },
            &mounts,
            &mut |e| {
                events
                    .lock()
                    .expect("#1327 loader events")
                    .push(json!({"kind":"error","error":e}))
            },
        );
        let actual = json!({"result":result.iter().map(|c|c.channel_key().to_string()).collect::<Vec<_>>(),"events":*events.lock().expect("#1327 loader recording")});
        let expected = recorded_expected(case).clone();
        crate::negative_assert_eq!(actual, expected, "#1327 loader oracle");
    }
}
#[test]
fn registry_missing_and_replacement() {
    let mut mounts = ChannelLoaderMounts::default();
    mounts.register(
        "discord",
        Arc::new(|_| Ok(Arc::new(Stub(ChannelKey::new("old"))))),
    );
    mounts.register(
        "discord",
        Arc::new(|_| Ok(Arc::new(Stub(ChannelKey::new("new"))))),
    );
    let config = pilotdeck_protocol::parse_json(
        r#"{"telegram":{"enabled":true},"discord":{"enabled":true},"unknown":{"enabled":true}}"#,
    )
    .expect("#1327 registry config");
    let mut errors = Vec::new();
    let channels = load_enabled_channels(
        Some(&config),
        &LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |e| errors.push(e),
    );
    crate::negative_assert_eq!(
        channels
            .iter()
            .map(|c| c.channel_key().to_string())
            .collect::<Vec<_>>(),
        ["new"],
        "#1327 registry continue and replace"
    );
    assert_eq!(
        errors,
        [
            "[adapters] Failed to load channel \"telegram\": Error: channel module not registered: telegram"
        ],
        "#1327 registry missing error"
    );
    assert!(
        load_enabled_channels(
            None,
            &LoadEnabledChannelsOptions { pilot_home: None },
            &mounts,
            &mut |_| panic!("#1327 unconfigured error")
        )
        .is_empty(),
        "#1327 no configuration"
    );
}

struct ProbeHandle {
    stops: std::sync::atomic::AtomicUsize,
    panic_stop: u8,
}
impl ChannelHandle for ProbeHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(self.panic_stop != 1, "injected synchronous stop panic");
        let panic_async = self.panic_stop == 2;
        Box::pin(async move {
            assert!(!panic_async, "injected asynchronous stop panic");
            Ok(())
        })
    }
}
struct ProbeAdapter {
    key: ChannelKey,
    handle: Arc<ProbeHandle>,
    panic_start: bool,
}
impl ChannelAdapter for ProbeAdapter {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        _deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let handle = self.handle.clone();
        let panic_start = self.panic_start;
        Box::pin(async move {
            assert!(!panic_start, "injected start panic");
            Ok(handle as Arc<dyn ChannelHandle>)
        })
    }
}
fn probe(panic_start: bool, panic_stop: u8) -> Arc<ProbeAdapter> {
    Arc::new(ProbeAdapter {
        key: ChannelKey::new("a"),
        handle: Arc::new(ProbeHandle {
            stops: std::sync::atomic::AtomicUsize::new(0),
            panic_stop,
        }),
        panic_start,
    })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_thread_status_order() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let states = Arc::new(Mutex::new(Vec::new()));
    let seen = states.clone();
    let server = ChannelServer::new(
        ChannelStartDeps {
            report_status: Some(Arc::new(move |update| {
                if update.state == ChannelState::Connected {
                    entered_tx
                        .send(())
                        .expect("#1327 cross-thread status entered");
                    release_rx
                        .lock()
                        .expect("#1327 cross-thread status gate")
                        .recv_timeout(Duration::from_secs(5))
                        .expect("#1327 cross-thread status released");
                }
                seen.lock()
                    .expect("#1327 cross-thread status record")
                    .push(update.state);
            })),
            ..Default::default()
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    let adapter = probe(false, 0);
    let start = server.hot_start_channel(adapter.clone());
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("#1327 cross-thread status callback began");
    bounded(server.close())
        .await
        .expect("#1327 cross-thread status close while callback blocked");
    release_tx
        .send(())
        .expect("#1327 cross-thread status release");
    bounded(start)
        .await
        .expect("#1327 cross-thread status start completed");
    crate::negative_assert_eq!(
        *states.lock().expect("#1327 cross-thread status record"),
        [
            ChannelState::Starting,
            ChannelState::Connected,
            ChannelState::Stopped
        ],
        "#1327 cross-thread status order"
    );
    assert_eq!(
        adapter
            .handle
            .stops
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#1327 cross-thread status exactly one stop"
    );
}
#[tokio::test]
async fn adapter_panics_do_not_strand_lifecycle() {
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    let error = bounded(server.hot_start_channel(probe(true, 0)))
        .await
        .expect_err("#1327 panic start rejection");
    assert_eq!(
        error.message(),
        "channel start task panicked",
        "#1327 panic start error"
    );
    bounded(server.hot_start_channel(probe(false, 0)))
        .await
        .expect("#1327 panic queue successor");
    bounded(server.close())
        .await
        .expect("#1327 panic queue close");
    for mode in [1, 2] {
        let server = ChannelServer::new(
            ChannelStartDeps::default(),
            Arc::new(|| Box::pin(async { Ok(()) })),
        );
        bounded(server.hot_start_channel(probe(false, mode)))
            .await
            .expect("#1327 panic stop setup");
        let closed = bounded(server.close()).await;
        crate::negative_assert!(closed.is_err(), "#1327 panic stop rejection");
        let first = closed.expect_err("#1327 checked panic stop rejection");
        assert_eq!(
            bounded(server.close()).await,
            Err(first),
            "#1327 panic memoized close"
        );
    }
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| panic!("injected host panic")),
    );
    let first = bounded(server.close())
        .await
        .expect_err("#1327 panic host rejection");
    assert_eq!(
        bounded(server.close()).await,
        Err(first),
        "#1327 panic host memoized close"
    );
}
#[tokio::test]
async fn status_callback_can_reenter_close() {
    let slot = Arc::new(Mutex::new(None::<ChannelServer>));
    let target = slot.clone();
    let server = ChannelServer::new(
        ChannelStartDeps {
            report_status: Some(Arc::new(move |update| {
                if update.state == ChannelState::Connected {
                    drop(
                        target
                            .lock()
                            .expect("#1327 reentrant server slot")
                            .as_ref()
                            .expect("#1327 reentrant server installed")
                            .close(),
                    );
                }
            })),
            ..Default::default()
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    *slot.lock().expect("#1327 reentrant server slot") = Some(server.clone());
    let adapter = probe(false, 0);
    bounded(server.hot_start_channel(adapter.clone()))
        .await
        .expect("#1327 reentrant start");
    let rejected = bounded(server.hot_start_channel(probe(false, 0))).await;
    crate::negative_assert!(
        rejected.is_err(),
        "#1327 reentrant callback initiated close"
    );
    let rejected = rejected.expect_err("#1327 checked reentrant rejection");
    assert_eq!(
        rejected.message(),
        "channel server is closing",
        "#1327 reentrant closing gate"
    );
    bounded(server.close())
        .await
        .expect("#1327 reentrant close");
    assert_eq!(
        adapter
            .handle
            .stops
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "#1327 reentrant one stop"
    );
    slot.lock().expect("#1327 reentrant server slot").take();
}

fn lifecycle_cases(oracle: &Value) -> &[Value] {
    let cases = oracle["cases"].as_array().expect("#1327 lifecycle cases");
    crate::negative_assert!(
        !cases.is_empty(),
        "#1327 lifecycle oracle cases must be non-empty"
    );
    crate::negative_assert_eq!(cases.len(), 19, "#1327 lifecycle oracle case count");
    crate::negative_assert_eq!(
        oracle["expectedCaseCount"],
        json!(19),
        "#1327 lifecycle generator declaration"
    );
    cases
}
fn loader_cases(oracle: &Value) -> &[Value] {
    let cases = oracle["cases"].as_array().expect("#1327 loader cases");
    crate::negative_assert!(
        !cases.is_empty(),
        "#1327 loader oracle cases must be non-empty"
    );
    crate::negative_assert_eq!(cases.len(), 8, "#1327 loader oracle case count");
    crate::negative_assert_eq!(
        oracle["expectedCaseCount"],
        json!(8),
        "#1327 loader generator declaration"
    );
    cases
}
#[tokio::test]
async fn replace_close_keeps_previous_snapshot() {
    let oracle = oracle(LIFECYCLE);
    let case = lifecycle_cases(&oracle)
        .iter()
        .find(|c| c["name"] == "close-after-replace-check")
        .expect("#1327 replace close fixture present");
    let expected = recorded_expected(case).clone();
    crate::negative_assert_eq!(
        replay(case).await,
        expected,
        "#1327 replace close aggregate"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_close_claim_is_atomic() {
    let oracle = oracle(LIFECYCLE);
    let case = lifecycle_cases(&oracle)
        .iter()
        .find(|c| c["name"] == "close-after-replace-check")
        .expect("#1327 claim race fixture present");
    let expected = recorded_expected(case).clone();
    crate::negative_assert_eq!(
        replay_with_claim_race(case, true).await,
        expected,
        "#1327 replace claim atomic"
    );
}
#[tokio::test]
async fn late_start_stop_failure_matches_real_ts() {
    let oracle = oracle(LIFECYCLE);
    let case = lifecycle_cases(&oracle)
        .iter()
        .find(|c| c["name"] == "late-start-stop-fails")
        .expect("#1327 late stop failure fixture present");
    let expected = recorded_expected(case).clone();
    crate::negative_assert_eq!(
        replay(case).await,
        expected,
        "#1327 late stop failure oracle"
    );
}

/// A channel whose `stop` always fails: the failure `finish_stop` must
/// classify (not a panic, which would unwind past the restore branch).
struct FailingStopHandle {
    stops: std::sync::atomic::AtomicUsize,
}
impl ChannelHandle for FailingStopHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(ChannelError::error("injected stop failure")) })
    }
}
struct FailingStopAdapter {
    key: ChannelKey,
    handle: Arc<FailingStopHandle>,
}
impl ChannelAdapter for FailingStopAdapter {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        _deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let handle = self.handle.clone();
        Box::pin(async move { Ok(handle as Arc<dyn ChannelHandle>) })
    }
}
fn failing_stop() -> Arc<FailingStopAdapter> {
    Arc::new(FailingStopAdapter {
        key: ChannelKey::new("a"),
        handle: Arc::new(FailingStopHandle {
            stops: std::sync::atomic::AtomicUsize::new(0),
        }),
    })
}

/// #1353 A8.5-pr2 (A10): a stop that fails while the server is closing must
/// not restore the handle to `running`. The closing gate has already
/// rejected every later start, so a restored handle could never be stopped
/// again — the flush that follows the drain must not observe a live entry.
/// The same failure outside a close keeps the TS restore behavior.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_stop_during_close_restores_nothing() {
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    let adapter = failing_stop();
    bounded(server.hot_start_channel(adapter.clone()))
        .await
        .expect("#1353 failing-stop setup start");
    let closed = bounded(server.close()).await;
    let error = closed.expect_err("#1353 the failed stop surfaces from close");
    assert_eq!(
        error.message(),
        "PilotDeck server shutdown failed",
        "#1353 the stop failure is the close failure"
    );
    crate::negative_assert_eq!(
        server.running_keys(),
        Vec::<String>::new(),
        "#1353 A10: a failed stop during close restores no running handle"
    );
    // Control: outside a close the same failure restores the handle, as in TS
    // (`pilotdeckServer.ts` stop error path re-fills `runningHandles`).
    let standalone = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    let other = failing_stop();
    bounded(standalone.hot_start_channel(other.clone()))
        .await
        .expect("#1353 standalone restore setup start");
    bounded(standalone.reconcile_channels(ReconcileInput {
        channels: vec![],
        managed_channel_keys: vec![ChannelKey::new("a")],
    }))
    .await
    .expect_err("#1353 the standalone stop failure surfaces");
    crate::negative_assert_eq!(
        standalone.running_keys(),
        vec!["a".to_owned()],
        "#1353 A10: a failed stop outside close keeps the TS restore"
    );
}

struct PrefixHandle {
    open: std::sync::atomic::AtomicBool,
    reenter: Mutex<Option<ChannelServer>>,
}
impl ChannelHandle for PrefixHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.open.store(false, std::sync::atomic::Ordering::SeqCst);
        let close = self
            .reenter
            .lock()
            .expect("#1327 prefix reentry slot")
            .take()
            .map(|server| server.close());
        Box::pin(async move {
            if let Some(close) = close {
                close.await?;
            }
            Ok(())
        })
    }
}
struct PrefixAdapter(Arc<PrefixHandle>);
impl ChannelAdapter for PrefixAdapter {
    fn channel_key(&self) -> &ChannelKey {
        static KEY: std::sync::OnceLock<ChannelKey> = std::sync::OnceLock::new();
        KEY.get_or_init(|| ChannelKey::new("a"))
    }
    fn start(
        &self,
        _deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let handle = self.0.clone();
        Box::pin(async move {
            handle.open.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(handle as Arc<dyn ChannelHandle>)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_close_seals_ingress_before_host() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let handle = Arc::new(PrefixHandle {
        open: false.into(),
        reenter: Mutex::new(None),
    });
    let (events, observed) = std::sync::mpsc::channel();
    let host_events = events.clone();
    let host_handle = handle.clone();
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(move || {
            host_events
                .send(("host", host_handle.open.load(SeqCst)))
                .expect("#1327 prefix host observation");
            Box::pin(async { Ok(()) })
        }),
    );
    bounded(server.hot_start_channel(Arc::new(PrefixAdapter(handle))))
        .await
        .expect("#1327 prefix setup");
    let (release, blocked) = std::sync::mpsc::channel();
    let blocked = Mutex::new(blocked);
    let hits = Arc::new(AtomicUsize::new(0));
    let hit = hits.clone();
    server.set_hook(Some(Arc::new(move |point, key| {
        if point == "stop-claimed" && *key == ChannelKey::new("a") {
            hit.fetch_add(1, SeqCst);
            events
                .send(("claimed", false))
                .expect("#1327 prefix claim observation");
            blocked
                .lock()
                .expect("#1327 prefix gate mutex")
                .recv_timeout(Duration::from_secs(5))
                .expect("#1327 prefix gate release");
        } else if point == "close-prefix-wait" {
            events
                .send(("prefix-wait", false))
                .expect("#1327 prefix wait observation");
        }
    })));
    let disable = server.reconcile_channels(ReconcileInput {
        channels: vec![],
        managed_channel_keys: vec![ChannelKey::new("a")],
    });
    assert_eq!(
        observed
            .recv_timeout(Duration::from_secs(5))
            .expect("#1327 prefix claim reached"),
        ("claimed", false),
        "#1327 prefix target claim"
    );
    let close = server.close();
    // Either a real wait notification or an early host close arrives. Release
    // and collect both operations before judging, including the mutant path.
    let first = observed
        .recv_timeout(Duration::from_secs(5))
        .expect("#1327 prefix close observation");
    release.send(()).expect("#1327 prefix release sender");
    bounded(disable)
        .await
        .expect("#1327 prefix disable completion");
    bounded(close).await.expect("#1327 prefix close completion");
    server.set_hook(None);
    assert_eq!(hits.load(SeqCst), 1, "#1327 prefix target hit exactly once");
    crate::negative_assert_eq!(
        first,
        ("prefix-wait", false),
        "#1327 disable close waits stop prefix"
    );
    let host = observed
        .try_iter()
        .find(|event| event.0 == "host")
        .expect("#1327 prefix host completed");
    assert_eq!(
        host,
        ("host", false),
        "#1327 host closes after ingress sealed"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_driver_panic_bounds_waiters() {
    // #2787: the close driver task dies mid-close (panic injected at the
    // close-prefix-wait transition) while a stop prefix is still held, so the
    // waiters in `wait_closed` can never observe a completion value. The
    // #2757-shaped fix gives the driver exclusive ownership of the completion
    // sender: its death closes the watch channel and `wait_closed` resolves
    // with the driver-dropped error instead of hanging. Two workers: one runs
    // the disable drive task parked in the stop-claimed hook, the other runs
    // the close driver that panics.
    let handle = Arc::new(PrefixHandle {
        open: false.into(),
        reenter: Mutex::new(None),
    });
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    bounded(server.hot_start_channel(Arc::new(PrefixAdapter(handle))))
        .await
        .expect("#2787 panic setup start");
    let (release, blocked) = std::sync::mpsc::channel();
    let blocked = Mutex::new(blocked);
    let (events, observed) = std::sync::mpsc::channel();
    server.set_hook(Some(Arc::new(move |point, key| {
        if point == "stop-claimed" && *key == ChannelKey::new("a") {
            events
                .send("claimed")
                .expect("#2787 panic claim observation");
            // Hold the stop prefix: the close driver must observe a nonzero
            // prefix count and enter its close-prefix-wait transition.
            blocked
                .lock()
                .expect("#2787 panic gate mutex")
                .recv_timeout(Duration::from_secs(5))
                .expect("#2787 panic gate release");
        } else if point == "close-prefix-wait" {
            panic!("#2787 injected close driver panic");
        }
    })));
    let disable = server.reconcile_channels(ReconcileInput {
        channels: vec![],
        managed_channel_keys: vec![ChannelKey::new("a")],
    });
    assert_eq!(
        observed
            .recv_timeout(Duration::from_secs(5))
            .expect("#2787 panic claim reached"),
        "claimed",
        "#2787 panic target claim"
    );
    let close = server.close();
    // Red without the fix: `wait_closed` parks on a watch value the dead
    // driver can never set and the 5s `bounded` timeout fires. Green with it:
    // the driver's dropped sender closes the channel.
    let error = bounded(close)
        .await
        .expect_err("#2787 bounded close driver panic");
    assert_eq!(
        error.message(),
        "channel close driver dropped",
        "#2787 close driver panic bounds waiters"
    );
    release.send(()).expect("#2787 panic release sender");
    bounded(disable)
        .await
        .expect("#2787 panic disable completion");
    server.set_hook(None);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_prefix_can_reenter_close() {
    let handle = Arc::new(PrefixHandle {
        open: false.into(),
        reenter: Mutex::new(None),
    });
    let server = ChannelServer::new(
        ChannelStartDeps::default(),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    bounded(server.hot_start_channel(Arc::new(PrefixAdapter(handle.clone()))))
        .await
        .expect("#1327 prefix reentry setup");
    *handle.reenter.lock().expect("#1327 prefix reentry slot") = Some(server.clone());
    let result = bounded(server.reconcile_channels(ReconcileInput {
        channels: vec![],
        managed_channel_keys: vec![ChannelKey::new("a")],
    }))
    .await
    .expect("#1327 prefix reentry completes");
    assert_eq!(
        result.stopped,
        vec![ChannelKey::new("a")],
        "#1327 prefix reentry stopped"
    );
    bounded(server.close())
        .await
        .expect("#1327 prefix reentry memoized close");
}

// #1386 A8.3 (#1302 B): the 11 unmigrated channels refuse native startup
// through `reject_unmigrated_channels`, the seam production assembly calls
// from `native_process/assembly.rs` (before `NativeChannels` is constructed,
// #1763). The seam takes no mounts and no console callback, so a rejection can
// never come after an adapter construction or a `console.error` line — there
// is nothing to construct or log through.
const UNMIGRATED_REJECTION_PHRASE: &str = crate::channel::loader::UNMIGRATED_REASON;
/// The eight keys #1302 B keeps in Rust (outside `UNMIGRATED_CHANNEL_KEYS`);
/// `reject_unmigrated_channels` must leave them alone. #2301 closed the
/// transition: the six loader-table keys construct at startup through
/// `native_process::channels::production_loader_mounts`
/// (`load_enabled_channels` now has its production caller), `weixin` /
/// `feishu` through the bundle constructors — so every key here is built,
/// not merely tolerated. The name stays `unmigrated_subset_channels_stay_loadable`
/// (the 13 `A8.3-CHANNEL-*-001` entries cite it by name); the terminal
/// reading is "an implemented key must not be rejected".
const SUBSET_CHANNEL_KEYS: [&str; 8] = [
    "weixin",
    "feishu",
    "apiServer",
    "webhook",
    "wecomCallback",
    "sms",
    "bluebubbles",
    "slack",
];

fn adapters_config(entries: &str) -> JsValue {
    pilotdeck_protocol::parse_json(entries).expect("#1386 adapters config JSON")
}

fn enabled_adapters(keys: &[&str]) -> JsValue {
    adapters_config(&format!(
        "{{{}}}",
        keys.iter()
            .map(|key| format!("\"{key}\":{{\"enabled\":true}}"))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

fn rejection_of(adapters: &JsValue, assertion: &'static str) -> String {
    let result = reject_unmigrated_channels(Some(adapters));
    crate::negative_assert!(result.is_err(), "{assertion}");
    result
        .expect_err("#1386 checked unmigrated rejection")
        .to_js_string()
}

fn assert_rejects_unmigrated(key: &str) {
    let message = rejection_of(
        &enabled_adapters(&[key]),
        "#1386 unmigrated channel must reject startup",
    );
    crate::negative_assert!(
        message.contains(&format!("channel \"{key}\"")),
        "#1386 rejection names the channel key"
    );
    crate::negative_assert!(
        message.contains(UNMIGRATED_REJECTION_PHRASE),
        "#1386 rejection carries the unmigrated phrase"
    );
    crate::negative_assert_eq!(
        message,
        format!("Error: [adapters] channel \"{key}\" {UNMIGRATED_REJECTION_PHRASE}"),
        "#1386 rejection message shape"
    );
}

#[test]
fn rejects_unmigrated_telegram() {
    assert_rejects_unmigrated("telegram");
}
#[test]
fn rejects_unmigrated_discord() {
    assert_rejects_unmigrated("discord");
}
#[test]
fn rejects_unmigrated_matrix() {
    assert_rejects_unmigrated("matrix");
}
#[test]
fn rejects_unmigrated_mattermost() {
    assert_rejects_unmigrated("mattermost");
}
#[test]
fn rejects_unmigrated_signal() {
    assert_rejects_unmigrated("signal");
}
#[test]
fn rejects_unmigrated_whatsapp() {
    assert_rejects_unmigrated("whatsapp");
}
#[test]
fn rejects_unmigrated_dingtalk() {
    assert_rejects_unmigrated("dingtalk");
}
#[test]
fn rejects_unmigrated_email() {
    assert_rejects_unmigrated("email");
}
#[test]
fn rejects_unmigrated_homeassistant() {
    assert_rejects_unmigrated("homeassistant");
}
#[test]
fn rejects_unmigrated_qq() {
    assert_rejects_unmigrated("qq");
}
#[test]
fn rejects_unmigrated_wecom() {
    assert_rejects_unmigrated("wecom");
}

// #1579 P1: the frozen TS baseline gates these 11 keys with two different
// expressions. `ChannelAdapterBundle.createQQ` / `createWeCom` open with
// `if (settings?.enabled !== true) return undefined`, so `enabled: 1` or
// `"true"` builds nothing and the TS gateway starts — rejecting those configs
// on the Rust side would abort startup where TS does not.
#[test]
fn unmigrated_bundle_channels_require_strict_boolean_true() {
    for key in STRICT_TRUE_CHANNEL_KEYS {
        for truthy in ["1", "-1", "\"true\"", "\"1\"", "[]", "{}"] {
            let adapters = adapters_config(&format!("{{\"{key}\":{{\"enabled\":{truthy}}}}}"));
            crate::negative_assert!(
                reject_unmigrated_channels(Some(&adapters)).is_ok(),
                "#1386 bundle channel requires strict enabled === true"
            );
        }
        assert_rejects_unmigrated(key);
    }
}

// The other nine keys are consumed by `loadEnabledChannels`'s
// `if (!cfg?.enabled) continue`, i.e. JS truthiness — so a non-boolean truthy
// `enabled` must still reject.
#[test]
fn unmigrated_loader_channels_use_js_truthiness() {
    for key in UNMIGRATED_CHANNEL_KEYS {
        if STRICT_TRUE_CHANNEL_KEYS.contains(key) {
            continue;
        }
        for truthy in ["1", "-1", "\"true\"", "\"1\"", "[]", "{}"] {
            let adapters = adapters_config(&format!("{{\"{key}\":{{\"enabled\":{truthy}}}}}"));
            crate::negative_assert!(
                reject_unmigrated_channels(Some(&adapters)).is_err(),
                "#1386 loader channel uses JS truthiness"
            );
        }
    }
}

// #1579 r3 P1: both suites above derive their key set from the constant, so a
// wrong membership flips the TS-parity property with every test still green
// (drop `qq` and the strict suite simply stops looking at it; add `telegram`
// and the truthiness suite skips it). Pin the membership and the 2/9 split.
#[test]
fn strict_true_channel_keys_are_exactly_qq_and_wecom() {
    let expected: &[&str] = &["qq", "wecom"];
    crate::negative_assert!(
        STRICT_TRUE_CHANNEL_KEYS == expected,
        "#1386 strict-true channel key set must be exactly qq and wecom"
    );
    let loader_keys = UNMIGRATED_CHANNEL_KEYS
        .iter()
        .filter(|key| !STRICT_TRUE_CHANNEL_KEYS.contains(key))
        .count();
    crate::negative_assert!(
        loader_keys == 9,
        "#1386 nine loader keys keep JS truthiness next to the two strict keys"
    );
}

// #1386 pin, re-judged terminal by #2301: during the transition this pinned
// "the subset keys stay loadable" against the silent skip the
// `A8.3-CHANNEL-LOADER-SUBSET-001` entry registered. The wiring landed, so
// the same cells now pin the permanent invariant underneath it — every key
// here is implemented and constructed at startup, which is exactly why the
// rejection gate must leave it alone. The name is kept (the 13
// `A8.3-CHANNEL-*-001` entries cite it as their contrast control).
#[test]
fn unmigrated_subset_channels_stay_loadable() {
    for key in SUBSET_CHANNEL_KEYS {
        let result = reject_unmigrated_channels(Some(&enabled_adapters(&[key])));
        crate::negative_assert!(result.is_ok(), "#1386 subset channel must not be rejected");
    }
    let all = enabled_adapters(&SUBSET_CHANNEL_KEYS);
    crate::negative_assert!(
        reject_unmigrated_channels(Some(&all)).is_ok(),
        "#1386 subset channel must not be rejected"
    );
    // Subset channels enabled next to disabled unmigrated keys still start.
    let mut mixed = String::new();
    for key in SUBSET_CHANNEL_KEYS {
        mixed.push_str(&format!("\"{key}\":{{\"enabled\":true}},"));
    }
    for key in UNMIGRATED_CHANNEL_KEYS {
        mixed.push_str(&format!("\"{key}\":{{\"enabled\":false}},"));
    }
    let mixed = adapters_config(&format!("{{{}}}", mixed.trim_end_matches(',')));
    crate::negative_assert!(
        reject_unmigrated_channels(Some(&mixed)).is_ok(),
        "#1386 subset channel must not be rejected"
    );
}

// `slack::register` (#1344): the loader-table mount takes over the key, so an
// enabled `slack` config now builds a channel instead of refusing startup.
// Construction only — `SlackChannel::new` resolves the token chain and never
// opens a socket, so no mock is needed (start is not exercised here).
#[test]
fn loader_mount_builds_slack_channel() {
    let mut mounts = ChannelLoaderMounts::default();
    slack::register(&mut mounts);
    crate::negative_assert!(
        mounts.is_registered("slack"),
        "#1344 slack registers a loader-table mount"
    );
    let adapters = adapters_config(
        r#"{"slack":{"enabled":true,"token":"xoxb-1","extra":{"appToken":"xoxa-1"}},"telegram":{"enabled":false}}"#,
    );
    let mut errors = Vec::new();
    let channels = load_enabled_channels(
        Some(&adapters),
        &LoadEnabledChannelsOptions { pilot_home: None },
        &mounts,
        &mut |message| errors.push(message),
    );
    crate::negative_assert!(errors.is_empty(), "#1344 loader errors: {errors:?}");
    crate::negative_assert_eq!(channels.len(), 1, "#1344 exactly the slack channel loads");
    crate::negative_assert_eq!(
        channels[0].channel_key().to_string(),
        "slack",
        "#1344 the loaded channel is slack"
    );
}

#[test]
fn unmigrated_falsy_or_absent_enabled_does_not_reject() {
    // The empty config object and missing adapters are the TS
    // `if (!adapters) return []` no-configuration path.
    let empty = adapters_config("{}");
    crate::negative_assert!(
        reject_unmigrated_channels(Some(&empty)).is_ok(),
        "#1386 disabled unmigrated channel must not reject"
    );
    crate::negative_assert!(
        reject_unmigrated_channels(None).is_ok(),
        "#1386 disabled unmigrated channel must not reject"
    );
    // Every falsy `enabled` spelling of the `cfg?.enabled` gate, and a key
    // present with `enabled` absent entirely.
    for falsy in ["false", "0", "-0", "\"\"", "null"] {
        for key in UNMIGRATED_CHANNEL_KEYS {
            let adapters = adapters_config(&format!("{{\"{key}\":{{\"enabled\":{falsy}}}}}"));
            crate::negative_assert!(
                reject_unmigrated_channels(Some(&adapters)).is_ok(),
                "#1386 disabled unmigrated channel must not reject"
            );
        }
    }
    let absent = adapters_config(
        "{\"telegram\":{},\"discord\":{},\"slack\":{},\"matrix\":{},\"mattermost\":{},\
         \"signal\":{},\"whatsapp\":{},\"bluebubbles\":{},\"dingtalk\":{},\"email\":{},\
         \"homeassistant\":{},\"qq\":{},\"wecom\":{}}",
    );
    crate::negative_assert!(
        reject_unmigrated_channels(Some(&absent)).is_ok(),
        "#1386 disabled unmigrated channel must not reject"
    );
}

#[test]
fn unmigrated_rejection_order_is_deterministic() {
    // All 11 enabled: the first key in UNMIGRATED_CHANNEL_KEYS order —
    // reverse CHANNEL_LOADER_KEYS order, then `qq`, then `wecom` — is
    // reported.
    let all = enabled_adapters(UNMIGRATED_CHANNEL_KEYS);
    let message = rejection_of(&all, "#1386 unmigrated rejection order");
    crate::negative_assert!(
        message.contains("channel \"homeassistant\""),
        "#1386 unmigrated rejection order"
    );
    // `qq` and `wecom` sit outside CHANNEL_LOADER_KEYS; alone they still
    // reject, and `qq` is reported before `wecom`.
    let pair = enabled_adapters(&["qq", "wecom"]);
    let message = rejection_of(&pair, "#1386 unmigrated rejection order");
    crate::negative_assert!(
        message.contains("channel \"qq\""),
        "#1386 unmigrated rejection order"
    );
    // An in-table key wins over the out-of-table `wecom`.
    let mixed = enabled_adapters(&["wecom", "email"]);
    let message = rejection_of(&mixed, "#1386 unmigrated rejection order");
    crate::negative_assert!(
        message.contains("channel \"email\""),
        "#1386 unmigrated rejection order"
    );
    // The reverse of a CHANNEL_LOADER_KEYS adjacency: `mattermost` is
    // reported before `matrix` even though the loader table lists
    // `matrix` first.
    let reversed = enabled_adapters(&["matrix", "mattermost"]);
    let message = rejection_of(&reversed, "#1386 unmigrated rejection order");
    crate::negative_assert!(
        message.contains("channel \"mattermost\""),
        "#1386 unmigrated rejection order"
    );
    // The table's tail: `discord` is reported before `telegram`.
    let tail = enabled_adapters(&["discord", "telegram"]);
    let message = rejection_of(&tail, "#1386 unmigrated rejection order");
    crate::negative_assert!(
        message.contains("channel \"discord\""),
        "#1386 unmigrated rejection order"
    );
}

// #1853 → #2301: the fail-closed key-set partition over the channel tables,
// terminal state. Every `CHANNEL_LOADER_KEYS` entry must be exactly one of:
// an unmigrated rejection key (`UNMIGRATED_CHANNEL_KEYS` — enabling refuses
// startup), or an implemented loader-mount key — a module that has landed
// (#1333 apiServer, #1334 webhook, #1336 wecomCallback, #1339 sms, #1343
// bluebubbles, #1344 slack) *and* is wired into the production assembly
// (`native_process::channels::production_loader_mounts`, #2301). The two
// transition groups the #1853 r2 trichotomy kept apart (silent-subset vs
// native-mount) merge here now that `load_enabled_channels` has its
// production caller; the rename records that flip without relaxing any
// fail-closed cell. A key in neither group is silently skipped by the native
// assembly — not rejected, not constructed, no TS fallback — which is the
// failure mode `A8.3-CHANNEL-LOADER-SUBSET-001` registered and #2301 closed.
// The implemented group is pinned as data on purpose: deriving it as the set
// difference would absorb any future loader-table key into the skip and keep
// this test green.
#[test]
fn loader_keys_partition_rejected_and_implemented() {
    // The implemented loader-mount group as of #2301 (terminal wiring).
    const IMPLEMENTED_LOADER_KEYS: [&str; 6] = [
        "slack",
        "bluebubbles",
        "wecomCallback",
        "sms",
        "apiServer",
        "webhook",
    ];
    let rejected: Vec<&str> = CHANNEL_LOADER_KEYS
        .iter()
        .copied()
        .filter(|key| UNMIGRATED_CHANNEL_KEYS.contains(key))
        .collect();
    crate::negative_assert_eq!(
        rejected.len(),
        9,
        "#1853 nine loader keys sit in the unmigrated rejection table"
    );
    // Covering: every loader key belongs to exactly one of the two groups.
    // This is the fail-closed cell — a key added to `CHANNEL_LOADER_KEYS`
    // without joining a group lands here, not inside a set difference that
    // would absorb it.
    let mut unassigned = Vec::new();
    for key in CHANNEL_LOADER_KEYS {
        if !UNMIGRATED_CHANNEL_KEYS.contains(&key) && !IMPLEMENTED_LOADER_KEYS.contains(&key) {
            unassigned.push(key);
        }
    }
    crate::negative_assert!(
        unassigned.is_empty(),
        "#2301 every loader key is rejected or implemented"
    );
    // Reverse closure for the implemented group (the cell above only
    // constrains loader-table keys): every implemented entry must itself be
    // outside the rejection table, a loader key, and a `SUBSET_CHANNEL_KEYS`
    // member — the module-level whitelist and this local pin cannot drift
    // apart in either direction, and the disjointness is asserted on the
    // tables themselves because both "not refused" behavior cells would pass
    // for a key sitting in both pinned groups.
    for key in IMPLEMENTED_LOADER_KEYS {
        crate::negative_assert!(
            !UNMIGRATED_CHANNEL_KEYS.contains(&key),
            "#2301 every implemented loader key is outside the rejection table"
        );
        crate::negative_assert!(
            CHANNEL_LOADER_KEYS.contains(&key),
            "#2301 every implemented loader key is a loader-table key"
        );
        crate::negative_assert!(
            SUBSET_CHANNEL_KEYS.contains(&key),
            "#2301 every implemented loader key is in the module subset whitelist"
        );
    }
    for key in SUBSET_CHANNEL_KEYS {
        crate::negative_assert!(
            IMPLEMENTED_LOADER_KEYS.contains(&key) || BUNDLE_CHANNEL_KEYS.contains(&key),
            "#2301 every module subset key is implemented or a bundle key"
        );
    }
    // #2367: the bundle group's reverse closure into the same whitelist. The
    // loop above iterates the whitelist, so it cannot notice a bundle key the
    // whitelist lost — dropping `weixin`/`feishu` kept every cell green while
    // `unmigrated_subset_channels_stay_loadable` silently tested one key
    // fewer.
    for key in BUNDLE_CHANNEL_KEYS {
        crate::negative_assert!(
            SUBSET_CHANNEL_KEYS.contains(key),
            "#2367 every bundle key is in the module subset whitelist"
        );
    }
    // Reverse closure: a rejection-table entry that is neither a loader key
    // nor one of the two bundle-built strict keys refuses configs TS accepts.
    for key in UNMIGRATED_CHANNEL_KEYS {
        crate::negative_assert!(
            CHANNEL_LOADER_KEYS.contains(key) || STRICT_TRUE_CHANNEL_KEYS.contains(key),
            "#1853 every rejection-table key is a loader key or a strict bundle key"
        );
    }
    // Bundle side: the two `ChannelAdapterBundle` keys live outside the loader
    // table; both are implemented and constructed at startup (`weixin` through
    // `create_startup_weixin`, `feishu` since #1329), and
    // `native_process::channels_tests::bundle_keys_match_the_assembly` pins
    // `BUNDLE_CHANNEL_KEYS == IMPLEMENTED_BUNDLE_CHANNEL_KEYS`.
    for key in BUNDLE_CHANNEL_KEYS {
        crate::negative_assert!(
            !CHANNEL_LOADER_KEYS.contains(key),
            "#1853 bundle keys stay outside the loader table"
        );
    }
    // The behavior grid the partition describes, one cell per class; the
    // exhaustive per-key families are the tests above and the real-entrypoint
    // suites in `native_process::channels_tests`:
    // - rejected: enabling refuses, naming the key (also
    //   `startup_refuses_every_unmigrated_channel`);
    // - implemented: not refused, outside the bundle table, and constructed
    //   at startup through the production mounts (also
    //   `unmigrated_subset_channels_stay_loadable`, the
    //   `loader_mount_builds_*_channel` family, and
    //   `native_process::channels_tests::production_mounts_cover_every_
    //   non_rejected_loader_key` / `startup_builds_the_enabled_loader_
    //   channels`).
    for key in &rejected {
        assert_rejects_unmigrated(key);
    }
    for key in IMPLEMENTED_LOADER_KEYS {
        let adapters = enabled_adapters(&[key]);
        crate::negative_assert!(
            reject_unmigrated_channels(Some(&adapters)).is_ok(),
            "#2301 implemented loader key is not refused by the unmigrated gate"
        );
        crate::negative_assert!(
            !BUNDLE_CHANNEL_KEYS.contains(&key),
            "#2301 implemented loader key is not a bundle key"
        );
    }
}

/// #1738: `ActiveChat::admit` is the single place that decides "this turn owns
/// the chat". The webhook path needs the decision to come from the `insert`
/// itself (its `contains` check runs in an earlier lock section, across the
/// `/new` reply), so the reject branch is only reachable when a second turn of
/// the same chat is already in flight — that branch is exercised here directly.
#[test]
fn active_chat_admission_is_first_come_only_and_releases_on_drop() {
    let active = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let guard = super::ActiveChat::admit(active.clone(), "chat-1".to_string())
        .expect("#1738 the first admit owns the slot");
    assert!(active.lock().expect("active set").contains("chat-1"));

    assert!(
        super::ActiveChat::admit(active.clone(), "chat-1".to_string()).is_none(),
        "#1738 a second turn must not be admitted while the first holds the slot"
    );
    assert!(
        active.lock().expect("active set").contains("chat-1"),
        "#1738 the rejected admit must leave the owner's slot in place"
    );

    drop(guard);
    assert!(!active.lock().expect("active set").contains("chat-1"));
    assert!(
        super::ActiveChat::admit(active.clone(), "chat-1".to_string()).is_some(),
        "#1738 dropping the guard frees the chat for the next turn"
    );
}

// #1763 / #1694 §7: `weixin` and `feishu` are `ChannelAdapterBundle` keys
// outside both the loader table and `UNMIGRATED_CHANNEL_KEYS`, so the
// unmigrated seam above never looks at them. The assembly closes that hole by
// declaring which bundle keys it actually constructs; an enabled one it
// cannot build refuses startup instead of silently skipping.
#[test]
fn bundle_gate_rejects_an_enabled_key_with_no_implementation() {
    let adapters = enabled_adapters(&["feishu"]);
    let result = reject_unimplemented_bundle_channels(Some(&adapters), &["weixin"]);
    crate::negative_assert!(
        result.is_err(),
        "#1763 enabled bundle key without an implementation must reject"
    );
    crate::negative_assert_eq!(
        result
            .expect_err("#1763 checked bundle rejection")
            .to_js_string(),
        format!("Error: [adapters] channel \"feishu\" {UNMIGRATED_REJECTION_PHRASE}"),
        "#1763 bundle rejection message shape"
    );
}

#[test]
fn bundle_gate_accepts_an_implemented_key() {
    for key in BUNDLE_CHANNEL_KEYS {
        let result = reject_unimplemented_bundle_channels(Some(&enabled_adapters(&[key])), &[key]);
        crate::negative_assert!(
            result.is_ok(),
            "#1763 implemented bundle key must not be rejected"
        );
    }
}

// Every bundle key now has a native constructor (`weixin` since #1694,
// `feishu` since #1329), so the production declaration must cover the whole
// table: dropping a key would reject an enabled bundle channel the assembly
// can in fact build, and adding one ahead of its construction would let a
// config start with a channel that is never created.
#[test]
fn implemented_bundle_keys_are_exactly_the_assembled_bundle_keys() {
    crate::negative_assert_eq!(
        crate::native_process::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS,
        &["feishu", "weixin"],
        "#1763 assembly declares exactly the bundle keys it constructs"
    );
    // The table equality (every `BUNDLE_CHANNEL_KEYS` entry has a constructor
    // and nothing else is listed) lives with the assembly constant, in
    // `native_process::channels_tests::bundle_keys_match_the_assembly`.
    crate::negative_assert!(
        !BUNDLE_CHANNEL_KEYS
            .iter()
            .any(|key| UNMIGRATED_CHANNEL_KEYS.contains(key)),
        "#1763 bundle gate covers the keys the unmigrated seam does not"
    );
}

// Both bundle constructors gate on strict `=== true`, like qq / wecom: a raw
// object with `enabled: 1` builds nothing in TS and must not reject.
#[test]
fn bundle_gate_requires_strict_boolean_true() {
    for key in BUNDLE_CHANNEL_KEYS {
        for truthy in ["1", "-1", "\"true\"", "\"1\"", "[]", "{}"] {
            let adapters = adapters_config(&format!("{{\"{key}\":{{\"enabled\":{truthy}}}}}"));
            crate::negative_assert!(
                reject_unimplemented_bundle_channels(Some(&adapters), &[]).is_ok(),
                "#1763 bundle gate requires strict enabled === true"
            );
        }
        for falsy in ["false", "0", "null", "\"\""] {
            let adapters = adapters_config(&format!("{{\"{key}\":{{\"enabled\":{falsy}}}}}"));
            crate::negative_assert!(
                reject_unimplemented_bundle_channels(Some(&adapters), &[]).is_ok(),
                "#1763 disabled bundle key must not reject"
            );
        }
        let absent = adapters_config(&format!("{{\"{key}\":{{}}}}"));
        crate::negative_assert!(
            reject_unimplemented_bundle_channels(Some(&absent), &[]).is_ok(),
            "#1763 bundle key without enabled must not reject"
        );
    }
    crate::negative_assert!(
        reject_unimplemented_bundle_channels(None, &[]).is_ok(),
        "#1763 no adapters must not reject"
    );
}
