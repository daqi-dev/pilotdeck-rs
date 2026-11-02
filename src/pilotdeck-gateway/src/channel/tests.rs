//! Real TS lifecycle scenarios replayed through controlled local channels.
use super::{loader::*, protocol::*, server::ChannelServer};
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
        crate::negative_assert_eq!(actual, case["expected"], "#1327 lifecycle oracle");
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
        crate::negative_assert_eq!(actual, case["expected"], "#1327 loader oracle");
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
    crate::negative_assert_eq!(
        replay(case).await,
        case["expected"],
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
    crate::negative_assert_eq!(
        replay_with_claim_race(case, true).await,
        case["expected"],
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
    crate::negative_assert_eq!(
        replay(case).await,
        case["expected"],
        "#1327 late stop failure oracle"
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
