//! Two-thread regression for the dream success tail's shared root operation
//! scope (fix-r1 P1-1, issue #776).
//!
//! TS `service.ts:826-865` runs install → replace → dispose → state writes
//! as one synchronous segment, so no other dream's tail can interleave; the
//! same parking probe on real TS (`_tmp/20261026-issue-776/fix-r1/probe/
//! tail-race.mts`, Node v22.22.0) shows contiguous `[install, replace]` pairs
//! in both release orders and a rollback that still succeeds afterwards.
//! With per-call root scopes, two Rust threads sharing one service could
//! interleave `D1.install → D2.install → D2.replace → D1.replace`, leave the
//! live roots from one dream and `last_dream` metadata from the other, and
//! the rollback version guard would then refuse.
//!
//! This test drives both dreams on real OS threads (each with its own
//! current-thread runtime, like the TS event loop each dream would own) and
//! parks the first dream inside the `dream_tail_gate` seam — strictly between
//! its install and its replace. Deterministic latches only: every wait is a
//! bounded channel receive with a dedicated failure message, and the parked
//! thread holds the tail scope for exactly as long as the test decides.
//!
//! #1036 P2-3: "the second dream did not install" is no longer inferred from
//! a silent 5s timeout. On a slow CI machine the second dream can still be
//! inside its earlier LLM awaits when such a window expires, and a per-call
//! scope regression would then fake a pass. Instead the test proves, with no
//! time inference at all: (a) while the first dream is parked, BOTH root
//! operation locks are held — `try_lock` from the test thread must fail, so
//! a per-call scope regression fails immediately and deterministically;
//! (b) the second dream's first root-lock acquisition attempt arrives
//! through the file-memory `lock_test_hook` (it blocks on the very first
//! repository call of `dream`, long before its tail — a service-level
//! pre-scope seam would never fire for the blocked thread), so with both
//! locks provably held, the second dream's first root acquisition cannot
//! complete until the first dream releases it.
#![allow(clippy::too_many_lines)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response};
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        llm::config::{LlmOptions, build_llm_config_with},
        service::{DreamTrigger, EdgeClawMemoryService, ServiceDependencies, ServiceOptions},
    },
    model::http::{BodyFailure, ModelHttpTransport, TransportFuture},
};
use pilotdeck_protocol::{JsObject, JsValue};
// This file is compiled as a library test module (`--lib`), so it reuses the
// crate's one `test_scratch` module instead of loading scratch.rs a second time
// (clippy::duplicate_mod). The file-memory lock machinery below is likewise
// reached through the crate (`pub(crate)` since #1036).
use crate::runtime::memory::edgeclaw::file_memory::{absolute, lock_test_hook, operation_lock};
use crate::test_scratch::Scratch;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const NOW: &str = "2026-10-22T00:00:00.000Z";
const NOW_MS: i64 = 1_792_627_200_000; // Date.parse("2026-10-22T00:00:00.000Z")

// -- kind-keyed, per-thread scripted transport --------------------------------

/// `"You are the …"` from the stub server in the replay fixture: enough of the
/// system prompt to route a response by dream request kind.
fn kind_from_body(body: &str) -> &'static str {
    const NEEDLE: &str = "\"content\":\"You are the ";
    let Some(start) = body.find(NEEDLE) else {
        return "unknown";
    };
    let rest = &body[start + NEEDLE.len()..];
    if rest.starts_with("Project Dream cluster planner") {
        "plan"
    } else if rest.starts_with("Project Dream refine engine") {
        "refine"
    } else if rest.starts_with("Dream project metadata reviewer") {
        "meta"
    } else {
        "unknown"
    }
}

/// One thread's `(status, body)` responses, keyed by dream request kind.
type ThreadScript = HashMap<&'static str, (u16, String)>;

/// Both dreams share one transport (one LLM endpoint); each thread registers
/// its own `plan`/`refine` responses before its dream starts, and every
/// metadata review gets the same no-change answer. Responses are keyed by
/// calling thread and request kind, so arrival order across the two threads
/// cannot change what a dream observes.
#[derive(Clone, Default)]
struct RaceTransport {
    scripts: Arc<Mutex<HashMap<thread::ThreadId, ThreadScript>>>,
}

impl RaceTransport {
    fn register(&self, plan: (u16, String), refine: (u16, String), meta: (u16, String)) {
        let id = thread::current().id();
        let mut scripts = self.scripts.lock().expect("#776 race scripts lock");
        let script = scripts.entry(id).or_default();
        assert!(
            script.is_empty(),
            "#776 race transport: a thread registered its script twice"
        );
        script.insert("plan", plan);
        script.insert("refine", refine);
        script.insert("meta", meta);
    }
}

impl ModelHttpTransport for RaceTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let scripts = self.scripts.clone();
        TransportFuture::io(async move {
            let body = request
                .into_body()
                .collect()
                .await
                .expect("#776 collect request body")
                .to_bytes();
            let text = String::from_utf8_lossy(&body).into_owned();
            let (status, response_body) = {
                let id = thread::current().id();
                let scripts = scripts.lock().expect("#776 race scripts lock");
                scripts
                    .get(&id)
                    .and_then(|script| script.get(kind_from_body(&text)))
                    .cloned()
                    .unwrap_or((599, "script exhausted".into()))
            };
            let body: BoxBody<Bytes, BodyFailure> = Full::new(Bytes::from(response_body))
                .map_err(|error| match error {})
                .boxed();
            Ok(Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(body)
                .expect("#776 race transport response"))
        })
    }
}

fn response(value: &Value) -> (u16, String) {
    (
        200,
        json!({"choices": [{"message": {"content": value.to_string()}}]}).to_string(),
    )
}

/// Declarative seed inputs become `JsValue` for the store call, as in the
/// replay fixture.
fn js_from(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(value) => JsValue::Bool(*value),
        Value::Number(number) => JsValue::Number(number.as_f64().unwrap_or_default()),
        Value::String(value) => JsValue::String(value.as_str().into()),
        Value::Array(items) => JsValue::Array(items.iter().map(js_from).collect()),
        Value::Object(entries) => {
            let mut object = JsObject::new();
            for (key, item) in entries {
                object.insert(key.as_str().into(), js_from(item));
            }
            JsValue::Object(object)
        }
    }
}

fn js_object_from(value: &Value) -> JsObject {
    let converted = js_from(value);
    if let JsValue::Object(object) = &converted {
        object.clone()
    } else {
        panic!("#776 tail race: expected an object, got {converted:?}")
    }
}

// -- the tail gate: park between install and replace --------------------------

/// The first dream to reach its tail parks between install and replace; the
/// second parks too, if it can get there at all. Every park registers a
/// release sender under the same lock as its event, so the event order and
/// the release order agree.
struct TailGate {
    events: Mutex<mpsc::Sender<GateEvent>>,
    parked: Mutex<Vec<mpsc::Sender<()>>>,
    first: AtomicBool,
}

#[derive(Debug)]
enum GateEvent {
    FirstParked,
    SecondParked,
}

impl TailGate {
    fn new() -> (Arc<Self>, mpsc::Receiver<GateEvent>) {
        let (events, receiver) = mpsc::channel();
        (
            Arc::new(Self {
                events: Mutex::new(events),
                parked: Mutex::new(Vec::new()),
                first: AtomicBool::new(false),
            }),
            receiver,
        )
    }
    /// Release every parked dream. Called on the failure paths, so a panic
    /// never leaves the two dream threads parked forever (a hung test binary
    /// would hide the dedicated failure message from the negative control).
    fn release_all(&self) {
        let mut parked = self.parked.lock().expect("#776 tail race parked lock");
        for release in parked.drain(..) {
            let _ = release.send(());
        }
    }
    fn park(&self) {
        let (release, parked) = mpsc::channel();
        {
            let mut parked = self.parked.lock().expect("#776 tail race parked lock");
            parked.push(release);
        }
        let first = !self.first.swap(true, Ordering::SeqCst);
        let event = if first {
            GateEvent::FirstParked
        } else {
            GateEvent::SecondParked
        };
        self.events
            .lock()
            .expect("#776 tail race event lock")
            .send(event)
            .expect("#776 tail race event channel");
        parked
            .recv()
            .expect("#776 tail race release channel dropped");
    }
}

// -- the race -----------------------------------------------------------------

fn build_service(
    dir: &Path,
    transport: RaceTransport,
    gate: Arc<TailGate>,
) -> Arc<EdgeClawMemoryService> {
    // Fully explicit LLM options, as in the replay fixture: no env and no
    // openclaw store are read, and port 9 never gets dialed because the
    // scripted transport answers before any socket opens.
    let llm = LlmOptions {
        provider: Some("p".into()),
        model: Some("m".into()),
        model_ref: Some("p/m".into()),
        api_type: Some("openai-completions".into()),
        base_url: Some("http://127.0.0.1/v1".into()),
        api_key: Some("fixture-key".into()),
        headers: None,
    };
    let config = build_llm_config_with(Some(&llm), None, &|_| None);
    let mut options = ServiceOptions::new(dir.join("workspace"));
    options.root_dir = Some(dir.join("root"));
    let park_gate = Arc::new(move || gate.park());
    Arc::new(
        EdgeClawMemoryService::with_dependencies(
            options,
            ServiceDependencies {
                home: Some(dir.join("home")),
                clock: Some(Arc::new(|| NOW.to_owned())),
                now_ms: Some(Arc::new(|| NOW_MS)),
                transport: Some(Arc::new(transport)),
                dream_tail_gate: Some(park_gate),
                llm_config: Some(config),
                ..ServiceDependencies::default()
            },
        )
        .expect("#776 tail race construct test service"),
    )
}

/// What one thread reports back about its dream: enough to prove both tails
/// really ran to a successful install + replace.
#[derive(Debug)]
struct DreamReport {
    status: &'static str,
    rewritten_projects: f64,
    error: Option<String>,
}

/// One lock-acquisition attempt from the second dream's thread: the
/// operation lock's registry key (the hook fires pre-`lock`, so an attempt
/// observed here has NOT acquired the lock yet).
#[derive(Debug)]
struct LockProbe {
    key: usize,
}

fn spawn_dream(
    service: Arc<EdgeClawMemoryService>,
    transport: RaceTransport,
    plan: (u16, String),
    refine: (u16, String),
    meta: (u16, String),
    reports: mpsc::Sender<DreamReport>,
    lock_probe: Option<mpsc::Sender<LockProbe>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        transport.register(plan, refine, meta);
        if let Some(lock_probe) = lock_probe {
            // The second dream: forward every lock attempt this thread makes,
            // pre-`lock` (the hook fires inside `OperationGuard::enter` right
            // before `Mutex::lock`). While the first dream is parked, both
            // root locks are provably held, so the second dream's first root
            // attempt is literally parked inside `Mutex::lock` — the
            // deterministic "second dream did not install" signal.
            lock_test_hook::set(move |event| {
                if !event.acquired {
                    let _ = lock_probe.send(LockProbe { key: event.key });
                }
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#776 tail race runtime");
        let outcome = runtime.block_on(service.dream(DreamTrigger::Manual));
        let report = match outcome {
            Ok(outcome) => DreamReport {
                status: outcome.status,
                rewritten_projects: outcome.rewritten_projects,
                error: None,
            },
            Err(error) => DreamReport {
                status: "error",
                rewritten_projects: 0.0,
                error: Some(format!("{error}")),
            },
        };
        reports.send(report).expect("#776 tail race report channel");
    })
}

fn recv_report(gate: &TailGate, reports: &mpsc::Receiver<DreamReport>, which: &str) -> DreamReport {
    reports
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| {
            gate.release_all();
            panic!("#776 tail race: the {which} dream never completed (bounded wait)")
        })
}

#[test]
fn dream_tail_cannot_interleave_across_threads() {
    let scratch = Scratch::new("edgeclaw-dream-tail-race");
    let dir = scratch.path();
    fs::create_dir_all(dir.join("workspace")).expect("#776 tail race workspace dir");
    fs::create_dir_all(dir.join("home")).expect("#776 tail race home dir");

    let transport = RaceTransport::default();
    let (gate, events) = TailGate::new();
    let service = build_service(dir, transport.clone(), gate.clone());

    // Two project seeds, exactly like the replay fixture's dream cells: the
    // first dream plans one cluster over both and merges them into one file.
    let store = service.repository.get_file_memory_store();
    let paths: Vec<String> = ["Alpha one", "Alpha two"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let input = json!({
                "type": "project",
                "scope": "project",
                "name": name,
                "description": format!("{name} description"),
                "body": format!("## Summary\n{name} body"),
                "capturedAt": NOW,
                "sourceSessionKey": format!("race-seed-{}", index + 1),
            });
            let upserted = store
                .execute("upsertCandidate", &js_object_from(&input))
                .expect("#776 tail race seed upsert");
            let relative = upserted
                .get("relativePath")
                .expect("#776 seed relativePath");
            match relative {
                JsValue::String(path) => path.to_utf8().expect("#776 seed path is UTF-8"),
                other => panic!("#776 tail race: seed relativePath is not a string: {other:?}"),
            }
        })
        .collect();

    // Dream A merges both seeds and keeps its metadata unchanged; dream B's
    // plan proposes no cluster but its metadata review updates the project
    // description, so B's staged snapshot still differs from A's merged roots.
    // An interleaved `install(A) → install(B) → replace(B) → replace(A)`
    // would leave the live roots and the `last_dream` metadata from different
    // dreams.
    let plan_a = response(&json!({
        "clusters": [{
            "member_relative_paths": paths,
            "reason": "race merge",
        }],
        "summary": "race plan summary",
    }));
    let refine_a = response(&json!({
        "name": "Merged Note A",
        "description": "merged description",
        "markdown": "## Summary\nmerged body",
        "summary": "race refine summary",
    }));
    let meta_a = response(&json!({
        "should_update": false,
        "reason": "race no meta change",
    }));
    let plan_b = response(&json!({
        "clusters": [],
        "summary": "race no clusters",
    }));
    let refine_b = refine_a.clone();
    let meta_b = response(&json!({
        "should_update": true,
        "reason": "race meta update",
        "description": "race updated project description",
    }));

    let (reports_tx, reports) = mpsc::channel();
    let (lock_tx, lock_rx) = mpsc::channel();
    let dream_a = spawn_dream(
        service.clone(),
        transport.clone(),
        plan_a,
        refine_a,
        meta_a,
        reports_tx.clone(),
        None,
    );
    let parked = events
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| {
            panic!("#776 tail race: the first dream never reached its install/replace tail")
        });
    assert!(
        matches!(parked, GateEvent::FirstParked),
        "#776 tail race: unexpected first gate event"
    );

    // #1036 P2-3: the first dream is parked strictly between its install and
    // its replace, so the shared tail scope must hold BOTH root operation
    // locks. Prove it without any time inference: `try_lock` from this
    // thread must fail. A regression back to per-call scopes frees the locks
    // here and fails this assertion deterministically — no silent-timeout
    // interpretation is possible.
    let workspace_lock = operation_lock(
        &absolute(service.repository.workspace_memory.get_root_dir())
            .expect("#1036 tail race resolve workspace root"),
    )
    .expect("#1036 tail race workspace operation lock");
    let global_lock = operation_lock(
        &absolute(service.repository.global_user_memory.get_root_dir())
            .expect("#1036 tail race resolve global root"),
    )
    .expect("#1036 tail race global operation lock");
    let workspace_key = Arc::as_ptr(&workspace_lock) as usize;
    let global_key = Arc::as_ptr(&global_lock) as usize;
    for (name, lock) in [("workspace", &workspace_lock), ("global", &global_lock)] {
        match lock.try_lock() {
            Ok(guard) => {
                drop(guard);
                gate.release_all();
                panic!(
                    "#1036 tail race: the {name} root operation lock was free while \
                     the first dream was parked between install and replace — the \
                     dream success tail must hold one shared root operation scope"
                );
            }
            // Only `WouldBlock` proves the parked dream holds this lock; a
            // poisoned lock is a hard failure, not ownership evidence.
            Err(std::sync::TryLockError::WouldBlock) => {}
            Err(std::sync::TryLockError::Poisoned(_)) => {
                gate.release_all();
                panic!(
                    "#1036 tail race: the {name} root operation lock was poisoned \
                     before the try_lock proof — ownership of the parked dream \
                     cannot be established"
                );
            }
        }
    }

    // The first dream provably holds both root locks. Start the second dream
    // on another thread: while the first is parked, the second must not be
    // able to install its own snapshot.
    let dream_b = spawn_dream(
        service.clone(),
        transport,
        plan_b,
        refine_b,
        meta_b,
        reports_tx,
        Some(lock_tx),
    );
    // Deterministic blocking signal (issue #1036 P2-3): the second dream
    // blocks at its very first repository call — `get_overview` inside
    // `reconcile_auto_dream_anchor`, long before any LLM await — so its
    // first root-lock attempt arrives through the hook while the first dream
    // is still parked. The attempt is forwarded pre-`lock`, so this is an
    // "about to lock" signal, not proof of a blocked thread; what the signal
    // does prove, together with both root locks held above, is that the
    // second dream's first root acquisition cannot complete until the first
    // dream releases it. Non-root attempts (free locks simply acquire and
    // the dream proceeds) are skipped until a root attempt arrives.
    loop {
        let probe = lock_rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| {
                gate.release_all();
                panic!(
                    "#1036 tail race: the second dream never attempted a root \
                     operation lock while the first was parked (bounded wait)"
                )
            });
        if probe.key == workspace_key || probe.key == global_key {
            break;
        }
    }
    // It cannot have parked at the tail gate either — that would mean its
    // install already ran while the first dream was parked.
    if let Ok(event) = events.try_recv() {
        gate.release_all();
        panic!(
            "#1036 tail race: the second dream reached the tail gate while \
             the first was parked ({event:?}) — its install must have run \
             without the shared root operation scope"
        );
    }

    // Release the first dream: its replace, dispose and state writes run to
    // completion while the second is still waiting.
    {
        let mut parked = gate.parked.lock().expect("#776 tail race parked lock");
        let release = if parked.is_empty() {
            panic!("#776 tail race: the first release handle is missing")
        } else {
            parked.remove(0)
        };
        release
            .send(())
            .expect("#776 tail race first release channel");
    }
    let report_a = recv_report(&gate, &reports, "first");
    assert_eq!(
        report_a.status, "success",
        "#776 tail race: the first dream failed: {:?}",
        report_a.error
    );
    assert_eq!(
        report_a.rewritten_projects, 1.0,
        "#776 tail race: the first dream did not merge its cluster"
    );

    // Now the second dream proceeds through its own tail and parks.
    let parked = match events.recv_timeout(Duration::from_secs(30)) {
        Ok(event) => event,
        Err(_) => {
            let report = reports.try_recv().ok();
            gate.release_all();
            panic!(
                "#776 tail race: the second dream never reached its install/replace tail \
                 (early report: {report:?})"
            );
        }
    };
    assert!(
        matches!(parked, GateEvent::SecondParked),
        "#776 tail race: unexpected second gate event"
    );
    {
        let mut parked = gate.parked.lock().expect("#776 tail race parked lock");
        let release = if parked.is_empty() {
            panic!("#776 tail race: the second release handle is missing")
        } else {
            parked.remove(0)
        };
        release
            .send(())
            .expect("#776 tail race second release channel");
    }
    let report_b = recv_report(&gate, &reports, "second");
    assert_eq!(
        report_b.status, "success",
        "#776 tail race: the second dream failed: {:?}",
        report_b.error
    );

    dream_a
        .join()
        .expect("#776 tail race: the first dream thread panicked");
    dream_b
        .join()
        .expect("#776 tail race: the second dream thread panicked");

    // Both dreams succeeded and the live roots and `last_dream` metadata come
    // from the same dream, so the rollback version guard must still accept.
    service
        .rollback_last_dream()
        .expect("#776 tail race: rollback after both dreams must stay available");
    service.close().expect("#776 tail race close service");
}
