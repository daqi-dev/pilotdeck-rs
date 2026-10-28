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
#[path = "common/scratch.rs"]
mod scratch;

use scratch::Scratch;
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
    let gate = Arc::new(move || gate.park());
    Arc::new(
        EdgeClawMemoryService::with_dependencies(
            options,
            ServiceDependencies {
                home: Some(dir.join("home")),
                clock: Some(Arc::new(|| NOW.to_owned())),
                now_ms: Some(Arc::new(|| NOW_MS)),
                transport: Some(Arc::new(transport)),
                dream_tail_gate: Some(gate),
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

fn spawn_dream(
    service: Arc<EdgeClawMemoryService>,
    transport: RaceTransport,
    plan: (u16, String),
    refine: (u16, String),
    meta: (u16, String),
    reports: mpsc::Sender<DreamReport>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        transport.register(plan, refine, meta);
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
    let dream_a = spawn_dream(
        service.clone(),
        transport.clone(),
        plan_a,
        refine_a,
        meta_a,
        reports_tx.clone(),
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

    // The first dream is now parked strictly between its install and its
    // replace. Start the second dream on another thread: while the first is
    // parked, the second must not be able to install its own snapshot.
    let dream_b = spawn_dream(
        service.clone(),
        transport,
        plan_b,
        refine_b,
        meta_b,
        reports_tx,
    );
    match events.recv_timeout(Duration::from_secs(5)) {
        Ok(GateEvent::SecondParked) => {
            gate.release_all();
            panic!(
                "#776 tail race: the second dream installed its last-dream \
                 snapshot while the first was parked between install and \
                 replace — the dream success tail must share one root \
                 operation scope"
            );
        }
        Ok(GateEvent::FirstParked) => {
            gate.release_all();
            panic!("#776 tail race: the tail gate fired twice for one dream")
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The expected outcome: the second dream is stuck on the root
            // operation locks the first dream holds across its whole tail.
            // Drain anything that arrived in the same instant.
            if let Ok(event) = events.try_recv() {
                gate.release_all();
                panic!(
                    "#776 tail race: the second dream installed its \
                     last-dream snapshot while the first was parked between \
                     install and replace — the dream success tail must share \
                     one root operation scope ({event:?} arrived late)"
                );
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            gate.release_all();
            panic!("#776 tail race: the tail gate event channel closed early")
        }
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
