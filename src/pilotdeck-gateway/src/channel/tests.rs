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
    let mut channels: HashMap<String, Arc<Mock>> = HashMap::new();
    let mut pending: HashMap<String, BoxFuture<Value>> = HashMap::new();
    let mut results = Vec::new();
    let mut snapshots = Vec::new();
    for step in case["steps"].as_array().expect("#1327 lifecycle steps") {
        let mut future: Option<BoxFuture<Value>> = None;
        match step["op"].as_str().expect("#1327 lifecycle operation") {
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
    let record = record.lock().expect("#1327 record mutex");
    json!({"traces":record.traces,"reports":record.reports,"results":results,"snapshots":snapshots,"logs":record.logs,"hostCloses":record.host_closes})
}
#[tokio::test]
async fn lifecycle_matches_real_ts() {
    let oracle = oracle(LIFECYCLE);
    for case in oracle["cases"].as_array().expect("#1327 lifecycle cases") {
        let actual = replay(case).await;
        assert_eq!(
            actual, case["expected"],
            "#1327 lifecycle oracle {}",
            case["name"]
        );
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
    for case in oracle["cases"].as_array().expect("#1327 loader cases") {
        let constructions = Arc::new(Mutex::new(Vec::new()));
        let mut mounts = ChannelLoaderMounts::default();
        for key in CHANNEL_LOADER_KEYS {
            let output = constructions.clone();
            let failure = case["failures"][key].clone();
            let import_fails = case["importFailures"][key].as_str().map(str::to_owned);
            mounts.register(
                key,
                Arc::new(move |options: &JsObject| {
                    if let Some(error) = &import_fails {
                        return Err(ChannelError::error(error));
                    }
                    output.lock().expect("#1327 loader recording").push(
                        json!({"key":key,"options":describe(&JsValue::Object(options.clone()))}),
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
        let mut errors = Vec::new();
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
            &mut |e| errors.push(e),
        );
        let actual = json!({"result":result.iter().map(|c|c.channel_key().to_string()).collect::<Vec<_>>(),"constructions":*constructions.lock().expect("#1327 loader recording"),"errors":errors});
        assert_eq!(
            actual, case["expected"],
            "#1327 loader oracle {}",
            case["name"]
        );
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
    assert_eq!(
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
    assert_eq!(
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
        let first = bounded(server.close())
            .await
            .expect_err("#1327 panic stop rejection");
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
    let rejected = bounded(server.hot_start_channel(probe(false, 0)))
        .await
        .expect_err("#1327 reentrant callback initiated close");
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
