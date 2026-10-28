//! #347: compiled only by sdk.rs's cfg(test) module, never by the product.
use super::*;
use crate::runtime::sdk_session::SdkSessionProjects;
use std::{
    sync::{Condvar, MutexGuard, TryLockError, mpsc},
    time::{Duration, Instant},
};

use crate::test_baseline as baseline;

const WAIT: Duration = Duration::from_secs(10);
const GATE_WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeEvent {
    Started,
    WouldBlock,
    Acquired,
}

pub(super) struct LockProbe {
    operation: &'static str,
    sender: mpsc::Sender<ProbeEvent>,
    delay: Duration,
}

impl SdkMcpConfig {
    // This returns the actual acquired guard. In particular the Acquired
    // branch never drops/reacquires it between observing and using the lock.
    pub(super) fn lock_servers_for_test(&self, operation: &str) -> MutexGuard<'_, ServerTable> {
        let probe = {
            let mut armed = lock(&self.servers_probe);
            if armed.as_ref().is_some_and(|p| p.operation == operation) {
                armed.take()
            } else {
                None
            }
        };
        let Some(probe) = probe else {
            return lock(&self.servers);
        };
        // A dropped receiver (coordinator cancellation) must not panic while
        // holding the product guard. Its release guard has already opened doors.
        let _ = probe.sender.send(ProbeEvent::Started);
        std::thread::sleep(probe.delay); // after marker, before the real probe
        match self.servers.try_lock() {
            Ok(guard) => {
                let _ = probe.sender.send(ProbeEvent::Acquired);
                guard
            }
            Err(TryLockError::WouldBlock) => {
                let _ = probe.sender.send(ProbeEvent::WouldBlock);
                lock(&self.servers)
            }
            Err(TryLockError::Poisoned(error)) => {
                let _ = probe.sender.send(ProbeEvent::Acquired);
                error.into_inner() // same poison recovery as production lock()
            }
        }
    }
}

fn observe(
    config: &SdkMcpConfig,
    operation: &'static str,
    delay: Duration,
) -> mpsc::Receiver<ProbeEvent> {
    let (sender, receiver) = mpsc::channel();
    *lock(&config.servers_probe) = Some(LockProbe {
        operation,
        sender,
        delay,
    });
    receiver
}

fn require_contention(receiver: &mpsc::Receiver<ProbeEvent>, cell: &str) {
    require_contention_with_timeout(receiver, cell, WAIT);
}

fn require_contention_with_timeout(
    receiver: &mpsc::Receiver<ProbeEvent>,
    cell: &str,
    timeout: Duration,
) {
    assert_eq!(
        receiver
            .recv_timeout(timeout)
            .unwrap_or_else(|e| panic!("#347 fix-r4b {cell} start missing: {e}")),
        ProbeEvent::Started,
        "#347 fix-r4b {cell} first observation must be the start marker"
    );
    assert_eq!(
        receiver
            .recv_timeout(timeout)
            .unwrap_or_else(|e| panic!("#347 fix-r4b {cell} competition missing: {e}")),
        ProbeEvent::WouldBlock,
        "#347 fix-r4b {cell} must observe WouldBlock at the real servers lock"
    );
}

#[derive(Default)]
struct Gate {
    entered: (Mutex<bool>, Condvar),
    release: (Mutex<bool>, Condvar),
}

fn wait_flag(flag: &(Mutex<bool>, Condvar), bound: Duration, message: &str) {
    let deadline = Instant::now() + bound;
    let mut value = lock(&flag.0);
    while !*value {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "#347 fix-r4b {message}");
        value = flag
            .1
            .wait_timeout(value, remaining)
            .unwrap_or_else(|e| panic!("#347 fix-r4b {message}: condvar poisoned: {e}"))
            .0;
    }
}

fn hold_at_gate(gate: &Gate) {
    *lock(&gate.entered.0) = true;
    gate.entered.1.notify_all();
    wait_flag(
        &gate.release,
        GATE_WAIT,
        "predecessor gate was never released",
    );
}

fn wait_entered(gate: &Gate) {
    wait_flag(&gate.entered, WAIT, "predecessor never entered its gate");
}

fn release_gate(gate: &Gate) {
    *lock(&gate.release.0) = true;
    gate.release.1.notify_all();
}

struct ReleaseOnDrop(Vec<Arc<Gate>>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        for gate in &self.0 {
            release_gate(gate);
        }
    }
}

#[derive(Default)]
struct GatedProjects {
    inner: SdkSessionProjects,
    armed: Mutex<bool>,
    set_gate: Arc<Gate>,
    delete_gate: Arc<Gate>,
}
impl GatedProjects {
    fn release_on_drop(&self) -> ReleaseOnDrop {
        ReleaseOnDrop(vec![self.set_gate.clone(), self.delete_gate.clone()])
    }
}
impl SdkSessionProjectsTable for GatedProjects {
    fn project(&self, key: &JsValue) -> Option<JsString> {
        self.inner.project(key)
    }
    fn entries(&self) -> Vec<(JsValue, JsString)> {
        self.inner.entries()
    }
    fn set(&self, key: &JsValue, root: JsString) {
        if *lock(&self.armed) {
            hold_at_gate(&self.set_gate);
        }
        self.inner.set(key, root);
    }
    fn delete(&self, key: &JsValue) {
        if *lock(&self.armed) {
            hold_at_gate(&self.delete_gate);
        }
        self.inner.delete(key);
    }
}

struct Environment(Arc<Gate>);
impl SdkMcpEnvironment for Environment {
    fn env(&self, name: &JsString) -> Option<JsString> {
        if !name.equals_str("GATE") {
            return None;
        }
        hold_at_gate(&self.0);
        Some("node".into())
    }
    fn home(&self) -> JsString {
        "/pdrs-fixture/home".into()
    }
}
struct Project;
impl SdkMcpProject for Project {
    fn project_root(&self) -> JsString {
        "/pdrs-fixture/project".into()
    }
    fn refresh(&self) -> SdkMcpFuture<'_, Result<(), SdkMcpError>> {
        Box::pin(async { Ok(()) })
    }
    fn snapshot(&self) -> SdkMcpFuture<'_, Vec<Arc<LoadedPlugin>>> {
        Box::pin(async { Vec::new() })
    }
}
struct Resolver;
impl SdkMcpProjectResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        _: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        Box::pin(async { Ok(Arc::new(Project) as Arc<dyn SdkMcpProject>) })
    }
}
struct Plugins;
impl SdkMcpSessionPlugins for Plugins {
    fn plugins(&self, _: &JsValue) -> Vec<Arc<LoadedPlugin>> {
        Vec::new()
    }
}
fn config(gate: Arc<Gate>, projects: Arc<GatedProjects>) -> Arc<SdkMcpConfig> {
    Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(Resolver),
        close_session: Arc::new(|_, _| Box::pin(async { Ok(()) })),
        session_plugins: Arc::new(Plugins),
        permission_rule_sets: Arc::new(GatewaySessionPermissionRuleSetRegistry::new()),
        environment: Arc::new(Environment(gate)),
        projects,
    }))
}
fn input(command: &str) -> JsValue {
    input_for("b", command)
}

fn input_for(name: &str, command: &str) -> JsValue {
    let mut server = JsObject::new();
    server.insert("type".into(), JsValue::String("stdio".into()));
    server.insert("command".into(), JsValue::String(command.into()));
    let mut servers = JsObject::new();
    servers.insert(name.into(), JsValue::Object(server));
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String("s".into()));
    input.insert("servers".into(), JsValue::Object(servers));
    JsValue::Object(input)
}
fn toggle_input() -> JsValue {
    let mut input = JsObject::new();
    input.insert("sessionKey".into(), JsValue::String("s".into()));
    input.insert("serverName".into(), JsValue::String("b".into()));
    input.insert("enabled".into(), JsValue::Bool(false));
    JsValue::Object(input)
}

async fn join<T>(task: tokio::task::JoinHandle<T>, what: &str) -> T {
    // is_finished is used ONLY for bounded cleanup, never as lock evidence.
    let deadline = Instant::now() + WAIT + GATE_WAIT;
    while !task.is_finished() {
        assert!(
            Instant::now() < deadline,
            "#347 fix-r4b {what} did not finish after release"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    task.await
        .unwrap_or_else(|e| panic!("#347 fix-r4b {what} task failed: {e}"))
}

fn check_ts(config: &SdkMcpConfig, id: &str) {
    let oracle = baseline::oracle(include_str!("oracle.json"));
    let rows = match oracle.get("cases") {
        Some(JsValue::Array(rows)) => rows,
        _ => panic!("#347 fix-r4b TS cases must be an array"),
    };
    assert_eq!(
        rows.len(),
        3,
        "#347 fix-r4b TS matrix has exactly three cells"
    );
    let row = rows
        .iter()
        .find(|row| row.get("id") == Some(&JsValue::String(id.into())))
        .expect("#347 fix-r4b TS matrix cell must exist");
    let key = JsValue::String("s".into());
    let servers = config.session_mcp_servers(&key);
    assert_eq!(
        row.get("servers"),
        Some(&JsValue::Bool(!servers.is_empty())),
        "#347 fix-r4b {id} TS servers"
    );
    assert_eq!(
        row.get("projects"),
        Some(&JsValue::Bool(
            config.options.projects.project(&key).is_some()
        )),
        "#347 fix-r4b {id} TS projects"
    );
    let names = JsValue::Array(
        servers
            .iter()
            .map(|(name, _)| JsValue::String(name.clone()))
            .collect(),
    );
    assert_eq!(
        row.get("names"),
        Some(&names),
        "#347 fix-r4b {id} TS exact names"
    );
    let project = config
        .options
        .projects
        .project(&key)
        .map_or(JsValue::Null, JsValue::String);
    assert_eq!(
        row.get("projectRoot"),
        Some(&project),
        "#347 fix-r4b {id} TS project root"
    );
    let enabled = servers
        .first()
        .and_then(|(_, v)| v.get("enabled"))
        .cloned()
        .unwrap_or(JsValue::Null);
    assert_eq!(
        row.get("enabled"),
        Some(&enabled),
        "#347 fix-r4b {id} TS enabled"
    );
}

async fn toggle_cell(delay: Duration, release: bool) {
    let gate = Arc::new(Gate::default());
    let config = config(gate.clone(), Arc::new(GatedProjects::default()));
    config
        .set_mcp_servers(&input("node"))
        .await
        .expect("#347 fix-r4b toggle setup set");
    // Created BEFORE spawning either operation, so unwinding/cancellation
    // cannot skip the only release as in the reviewed P0.
    let cleanup = ReleaseOnDrop(vec![gate.clone()]);
    let first = config.clone();
    let setter = tokio::spawn(async move { first.set_mcp_servers(&input("${env:GATE}")).await });
    wait_entered(&gate);
    let receiver = observe(&config, "toggle", delay);
    let second = config.clone();
    let toggler = tokio::spawn(async move { second.toggle_mcp_server(&toggle_input()).await });
    require_contention(&receiver, "toggle");
    if release {
        release_gate(&gate);
    }
    join(setter, "toggle predecessor")
        .await
        .expect("#347 fix-r4b toggle set result");
    join(toggler, "toggle successor")
        .await
        .expect("#347 fix-r4b toggle result");
    drop(cleanup);
    check_ts(&config, "set-then-toggle");
}

async fn clear_set_cell(delay: Duration) {
    let projects = Arc::new(GatedProjects::default());
    let config = config(Arc::new(Gate::default()), projects.clone());
    config
        .set_mcp_servers(&input_for("a", "node"))
        .await
        .expect("#347 fix-r4b cell3 setup set");
    *lock(&projects.armed) = true;
    let cleanup = projects.release_on_drop();
    let first = config.clone();
    let clear = tokio::task::spawn_blocking(move || {
        first.clear_sdk_session_state(&JsValue::String("s".into()))
    });
    wait_entered(&projects.delete_gate);
    release_gate(&projects.set_gate);
    let receiver = observe(&config, "set", delay);
    let second = config.clone();
    let setter = tokio::spawn(async move { second.set_mcp_servers(&input("node")).await });
    require_contention(&receiver, "cell3");
    release_gate(&projects.delete_gate);
    join(clear, "cell3 clear").await;
    join(setter, "cell3 set")
        .await
        .expect("#347 fix-r4b cell3 set result");
    drop(cleanup);
    check_ts(&config, "clear-then-set");
}

async fn set_clear_cell(delay: Duration) {
    let projects = Arc::new(GatedProjects::default());
    let config = config(Arc::new(Gate::default()), projects.clone());
    *lock(&projects.armed) = true;
    let cleanup = projects.release_on_drop();
    let first = config.clone();
    let setter = tokio::spawn(async move { first.set_mcp_servers(&input("node")).await });
    wait_entered(&projects.set_gate);
    release_gate(&projects.delete_gate);
    let receiver = observe(&config, "clear", delay);
    let second = config.clone();
    let clear = tokio::task::spawn_blocking(move || {
        second.clear_sdk_session_state(&JsValue::String("s".into()))
    });
    require_contention(&receiver, "cell4");
    release_gate(&projects.set_gate);
    join(setter, "cell4 set")
        .await
        .expect("#347 fix-r4b cell4 set result");
    join(clear, "cell4 clear").await;
    drop(cleanup);
    check_ts(&config, "set-then-clear");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_and_toggle_do_not_lose_updates() {
    toggle_cell(Duration::ZERO, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clear_and_set_commits_share_one_critical_section() {
    clear_set_cell(Duration::ZERO).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_commit_cannot_outlive_the_shared_table_write() {
    set_clear_cell(Duration::ZERO).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_marker_delay_preserves_all_three_cells() {
    toggle_cell(Duration::from_secs(3), true).await;
    clear_set_cell(Duration::from_secs(3)).await;
    set_clear_cell(Duration::from_secs(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "intentional failure, run by negative.py --bounded-sample in a subprocess"]
async fn unreleased_predecessor_fails_boundedly() {
    toggle_cell(Duration::ZERO, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unwinding_releases_the_predecessor() {
    let gate = Arc::new(Gate::default());
    let cleanup = ReleaseOnDrop(vec![gate.clone()]);
    let config = config(gate.clone(), Arc::new(GatedProjects::default()));
    let first = config.clone();
    let task = tokio::spawn(async move { first.set_mcp_servers(&input("${env:GATE}")).await });
    wait_entered(&gate);
    let caught = std::panic::catch_unwind(move || {
        let _cleanup = cleanup;
        panic!("#347 fix-r4b simulated coordinator panic");
    });
    assert!(caught.is_err(), "#347 fix-r4b panic path must run");
    join(task, "unwind predecessor")
        .await
        .expect("#347 fix-r4b unwind predecessor result");
    assert!(
        *lock(&gate.release.0),
        "#347 fix-r4b unwind must release predecessor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_releases_the_predecessor() {
    let gate = Arc::new(Gate::default());
    let cleanup = ReleaseOnDrop(vec![gate.clone()]);
    let config = config(gate.clone(), Arc::new(GatedProjects::default()));
    let first = config.clone();
    let task = tokio::spawn(async move { first.set_mcp_servers(&input("${env:GATE}")).await });
    wait_entered(&gate);
    let (sender, receiver) = mpsc::channel();
    let coordinator = tokio::spawn(async move {
        let _cleanup = cleanup;
        sender
            .send(())
            .expect("#347 fix-r4b cancellation coordinator started");
        std::future::pending::<()>().await;
    });
    receiver
        .recv_timeout(WAIT)
        .expect("#347 fix-r4b cancellation coordinator must start");
    coordinator.abort();
    let deadline = Instant::now() + WAIT;
    while !coordinator.is_finished() {
        assert!(
            Instant::now() < deadline,
            "#347 fix-r4b coordinator cancellation must finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        coordinator
            .await
            .expect_err("#347 fix-r4b coordinator must be cancelled")
            .is_cancelled(),
        "#347 fix-r4b cancellation reason"
    );
    join(task, "cancelled predecessor")
        .await
        .expect("#347 fix-r4b cancelled predecessor result");
    assert!(
        *lock(&gate.release.0),
        "#347 fix-r4b cancel must release predecessor"
    );
}

#[test]
fn missing_observations_fail_closed() {
    for started in [false, true] {
        for connected in [false, true] {
            let (sender, receiver) = mpsc::channel();
            if started {
                sender
                    .send(ProbeEvent::Started)
                    .expect("#347 fix-r4b missing-observation setup");
            }
            let sender = connected.then_some(sender);
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                require_contention_with_timeout(&receiver, "missing", Duration::from_millis(20));
            }))
            .expect_err("#347 fix-r4b missing observation must fail closed");
            let message = failure
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| failure.downcast_ref::<&str>().copied())
                .expect("#347 fix-r4b missing-observation panic must contain text");
            let expected = if started {
                "competition missing"
            } else {
                "start missing"
            };
            assert!(
                message.contains(expected),
                "#347 fix-r4b missing observation must identify its stage: {message}"
            );
            drop(sender);
        }
    }
}

#[test]
fn acquired_probe_retains_the_real_guard() {
    for poisoned in [false, true] {
        let config = config(
            Arc::new(Gate::default()),
            Arc::new(GatedProjects::default()),
        );
        if poisoned {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = lock(&config.servers);
                panic!("#347 fix-r4b deliberate servers poison");
            }));
        }
        let receiver = observe(&config, "clear", Duration::ZERO);
        let guard = config.lock_servers_for_test("clear");
        assert_eq!(
            receiver
                .recv_timeout(WAIT)
                .expect("#347 fix-r4b acquired start missing"),
            ProbeEvent::Started,
            "#347 fix-r4b acquired start event"
        );
        assert_eq!(
            receiver
                .recv_timeout(WAIT)
                .expect("#347 fix-r4b acquired result missing"),
            ProbeEvent::Acquired,
            "#347 fix-r4b unlocked probe must report Acquired"
        );
        assert!(
            matches!(config.servers.try_lock(), Err(TryLockError::WouldBlock)),
            "#347 fix-r4b acquired probe must retain the real guard"
        );
        drop(guard);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "intentional observation timeout, run as a bounded subprocess"]
async fn missing_competition_releases_toggle_predecessor() {
    toggle_cell(Duration::from_secs(11), true).await;
}

#[test]
#[ignore = "requires PDRS_BASELINE and Node v22.22.0"]
fn real_ts_matrix_regenerates_identically() {
    baseline::regenerates_identically("sdk-mcp-p2");
}
