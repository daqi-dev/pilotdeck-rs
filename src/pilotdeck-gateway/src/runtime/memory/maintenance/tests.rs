use super::*;
use crate::runtime::refresh::*;
use pilotdeck_protocol::stringify_json;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;
type Events = Arc<Mutex<Vec<Value>>>;
fn value(v: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(v).unwrap().unwrap()).unwrap()
}
fn error(e: &MemoryProviderError) -> Value {
    let mut v = json!({"name":e.name,"message":String::from_utf16_lossy(e.message.units())});
    if let Some(code) = &e.code {
        v["code"] = code.clone().into();
    }
    v
}
fn failure(name: &str, message: &str) -> MemoryProviderError {
    let mut e = MemoryProviderError::new(message);
    e.name = name.into();
    e
}
fn oracle(name: &str) -> Vec<Value> {
    let all: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/memory-maintenance/oracle.json"
    ))
    .unwrap();
    all["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}
struct Service {
    id: &'static str,
    events: Events,
    pending: Mutex<VecDeque<oneshot::Sender<Result<(), MemoryProviderError>>>>,
    closed: AtomicBool,
    panic_first: AtomicBool,
    immediate: bool,
}
impl MemoryMaintenanceService for Service {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()> {
        assert!(
            !self.panic_first.swap(false, Ordering::SeqCst),
            "first poll panic"
        );
        self.events
            .lock()
            .unwrap()
            .push(json!(["call", self.id, reason]));
        if self.closed.load(Ordering::SeqCst) {
            let observed = oracle("disposed")
                .into_iter()
                .find(|v| v[0] == "error")
                .unwrap();
            let mut e = failure(
                observed[1]["name"].as_str().unwrap(),
                observed[1]["message"].as_str().unwrap(),
            );
            e.code = Some(observed[1]["code"].as_str().unwrap().into());
            return Box::pin(async { Err(e) });
        }
        if self.immediate {
            return Box::pin(async { Ok(()) });
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().push_back(tx);
        Box::pin(async { rx.await.expect("test owns service completion") })
    }
}
impl Service {
    fn settle(&self, result: Result<(), MemoryProviderError>) {
        self.pending
            .lock()
            .unwrap()
            .pop_front()
            .expect("controlled predecessor exists")
            .send(result)
            .unwrap();
    }
}
struct Runtime {
    service: Option<Arc<Service>>,
}
impl MaintenanceRuntime for Runtime {
    fn project_root(&self) -> JsString {
        "/project".into()
    }
    fn memory_service(&self) -> Option<Arc<dyn MemoryMaintenanceService>> {
        self.service.clone().map(|s| s as _)
    }
}
struct Telemetry(Events);
impl MaintenanceTelemetry for Telemetry {
    fn track_feature_loop_stage(&self, input: JsValue) {
        self.0
            .lock()
            .unwrap()
            .push(json!(["success", value(&input)]));
    }
    fn track_error(&self, e: &MemoryProviderError, input: JsValue) {
        self.0
            .lock()
            .unwrap()
            .push(json!(["error", error(e), value(&input)]));
    }
}
// These unrelated refresh collaborators must never run on afterTurnCompleted.
struct Unused;
impl RefreshConfigStore for Unused {
    fn subscribe(&self, _: ConfigListener) -> Unsubscribe {
        panic!("unused")
    }
    fn reload(&self, _: &'static str) -> BoxFuture<ConfigReloadResult> {
        panic!("unused")
    }
}
impl RefreshRegistry for Unused {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        panic!("unused")
    }
    fn invalidate(&self, _: Option<&JsString>) {
        panic!("unused")
    }
}
async fn drain() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
async fn replay(name: &str) -> (Weak<Runtime>, Vec<Arc<Mutex<State>>>) {
    let events = Events::default();
    let make = |id| {
        Arc::new(Service {
            id,
            events: events.clone(),
            pending: Mutex::default(),
            closed: AtomicBool::new(false),
            panic_first: AtomicBool::new(false),
            immediate: name == "before_await",
        })
    };
    let a = make("A");
    let b = make("B");
    let old = Arc::new(Runtime {
        service: (name != "absent").then(|| a.clone()),
    });
    let weak = Arc::downgrade(&old);
    let current = Arc::new(Mutex::new(old.clone()));
    let failing = Arc::new(AtomicBool::new(false));
    let (e, c, f) = (events.clone(), current.clone(), failing.clone());
    let d = events.clone();
    let controller = Arc::new(ProjectMemoryMaintenanceController::new(
        Arc::new(move |key| {
            e.lock().unwrap().push(json!([
                "resolve",
                String::from_utf16_lossy(key.unwrap().units())
            ]));
            if f.load(Ordering::SeqCst) {
                return Err(RuntimeError::new("resolve failed").with_js_error(
                    crate::runtime::project::RuntimeJsError {
                        name: "TypeError".into(),
                        code: None,
                        message: "resolve failed".into(),
                        requires_host: false,
                    },
                ));
            }
            Ok(c.lock().unwrap().clone())
        }),
        Arc::new(Telemetry(events.clone())),
        Some(Arc::new(move |message, e| {
            d.lock().unwrap().push(json!([
                "diagnostic",
                String::from_utf16_lossy(message.units()),
                error(e)
            ]));
        })),
    ));
    let schedule = || {
        if let Err(e) = controller.schedule(Some(&"/project".into())) {
            let e = e.js_error().unwrap();
            events.lock().unwrap().push(json!(["throw",{"name":e.name,"message":String::from_utf16_lossy(e.message.units())}]));
        }
    };
    schedule();
    events.lock().unwrap().push(json!(["returned"]));
    if name == "resolver_error" {
        failing.store(true, Ordering::SeqCst);
        schedule();
        let bundle = RefreshBundle::new(RefreshOptions {
            config_store: Arc::new(Unused),
            registry: Arc::new(Unused),
            memory_maintenance: controller.clone(),
            router: Arc::new(|| None),
            project_root: "/project".into(),
            memory_diagnostics: None,
            sdk: None,
            log: Arc::new(ConsoleLog),
        });
        let e = bundle
            .after_turn_completed(TurnCompleted {
                session_key: "s".into(),
                project_key: None,
                run_id: None,
            })
            .unwrap_err();
        let e = e.js_error().unwrap();
        events.lock().unwrap().push(json!(["refresh_reject",{"name":e.name,"message":String::from_utf16_lossy(e.message.units())}]));
        failing.store(false, Ordering::SeqCst);
    }
    if [
        "coalesce",
        "before_await",
        "never",
        "cancelled",
        "reject_requested",
        "disposed",
    ]
    .contains(&name)
    {
        schedule();
        schedule();
    }
    if ["generations", "never"].contains(&name) {
        *current.lock().unwrap() = Arc::new(Runtime {
            service: Some(b.clone()),
        });
        schedule();
        b.settle(Ok(()));
        drain().await;
    }
    if name == "disposed" {
        a.closed.store(true, Ordering::SeqCst);
        events.lock().unwrap().push(json!(["closed"]));
    }
    if name == "settling" {
        let (c, e) = (Arc::downgrade(&controller), events.clone());
        *controller.0.settling.lock().unwrap() = Some(Box::new(move || {
            let c = c.upgrade().unwrap();
            let state = c.0.states.lock().unwrap()[0].1.clone();
            {
                let state = state.lock().unwrap();
                assert!(!state.requested);
                assert!(state.in_flight);
            }
            e.lock().unwrap().push(json!(["settling"]));
            c.schedule(Some(&"/project".into())).unwrap();
        }));
    }
    if !["never", "absent", "before_await"].contains(&name) {
        a.settle(match name {
            "reject" | "reject_requested" => Err(failure("TypeError", "maintenance failed")),
            "cancelled" => Err(failure("AbortError", "cancelled")),
            _ => Ok(()),
        });
        drain().await;
        if !a.pending.lock().unwrap().is_empty() {
            a.settle(Ok(()));
            drain().await;
        }
    }
    // Bounded runnable-task draining only; never await the intentionally pending A.
    drain().await;
    assert_eq!(*events.lock().unwrap(), oracle(name), "{name}");
    if name != "never" {
        assert!(a.pending.lock().unwrap().is_empty());
        assert!(
            controller
                .0
                .states
                .lock()
                .unwrap()
                .iter()
                .all(|(_, s)| !s.lock().unwrap().in_flight)
        );
    }
    let states = controller
        .0
        .states
        .lock()
        .unwrap()
        .iter()
        .map(|(_, s)| s.clone())
        .collect();
    (weak, states)
}
macro_rules! cases { ($($name:ident),*) => { $(#[test] fn $name() {
    let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (weak, states) = executor.block_on(replay(stringify!($name))); drop(executor);
    assert!(states.iter().all(|s| !s.lock().unwrap().in_flight), "cancelled worker releases flight");
    assert!(weak.upgrade().is_none(), "executor shutdown releases pending generation");
})* }; }
cases!(
    idle,
    coalesce,
    settling,
    reject,
    reject_requested,
    generations,
    disposed,
    absent,
    resolver_error,
    never,
    cancelled,
    before_await
);

#[test]
fn concurrent_requests_and_first_poll_unwind() {
    let executor = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _enter = executor.enter();
    let events = Events::default();
    let service = Arc::new(Service {
        id: "A",
        events: events.clone(),
        pending: Mutex::default(),
        closed: AtomicBool::new(false),
        panic_first: AtomicBool::new(true),
        immediate: false,
    });
    let runtime = Arc::new(Runtime {
        service: Some(service.clone()),
    });
    let controller = Arc::new(ProjectMemoryMaintenanceController::new(
        Arc::new(move |_| Ok(runtime.clone())),
        Arc::new(Telemetry(events.clone())),
        None,
    ));
    let c = controller.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || c.schedule(None))).is_err()
    );
    assert!(
        !controller.0.states.lock().unwrap()[0]
            .1
            .lock()
            .unwrap()
            .in_flight
    );
    controller.schedule(None).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (c, b) = (controller.clone(), barrier.clone());
            scope.spawn(move || {
                b.wait();
                c.schedule(None).unwrap();
            });
        }
    });
    assert_eq!(service.pending.lock().unwrap().len(), 1);
    service.settle(Ok(()));
    executor.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while service.pending.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            service.settle(Ok(()));
            while controller.0.states.lock().unwrap()[0]
                .1
                .lock()
                .unwrap()
                .in_flight
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both controlled calls settle");
    });
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e[0] == "call")
            .count(),
        2
    );
}
