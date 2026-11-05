//! #1370 controlled storage stimulus tests: the plan's state × event × timing
//! matrix cells 1–15 (assembly cells 16–18 are covered by the switch-matrix
//! unit test in native_process/tests.rs and the e2e binary fixture).
use super::ledger::field;
use super::stimulus::StorageStimulus;
use super::*;
use crate::metadata::object;
use crate::{negative_assert, negative_assert_eq};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

/// Observation cap for every wait: a lost collector or a broken release must
/// fail the test, never hang it.
const OBSERVE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(10);
/// Barrier bound for explicit-release tests: long enough to release by hand,
/// short enough that a broken release fails fast instead of wedging.
const HELD_BOUND_MS: f64 = 3000.0;

fn serial() -> &'static Mutex<()> {
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    SERIAL.get_or_init(|| Mutex::new(()))
}
/// Re-arms the process-wide controlled entry for one test and restores the
/// previous value on drop, so parallel tests can never observe it.
struct GlobalGuard(Option<Arc<StorageStimulus>>);
impl Drop for GlobalGuard {
    fn drop(&mut self) {
        let _ = StorageStimulus::override_global(self.0.take());
    }
}
fn arm_global(dir: Option<&Path>) -> GlobalGuard {
    let previous = StorageStimulus::override_global(
        dir.map(|dir| Arc::new(StorageStimulus::control(dir.to_path_buf()))),
    );
    GlobalGuard(previous)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("pdrs-stimulus-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn control(&self) -> PathBuf {
        let dir = self.0.join("control");
        fs::create_dir(&dir).unwrap();
        dir
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_atomic(path: &Path, text: &str) {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text).unwrap();
    fs::rename(&temporary, path).unwrap();
}
fn arm(dir: &Path, text: &str) {
    write_atomic(&dir.join("stimulus.json"), text);
}
fn release(dir: &Path, seq: u64) {
    write_atomic(&dir.join("release.json"), &format!("{{\"seq\":{seq}}}"));
}
fn receipts(dir: &Path) -> Vec<JsValue> {
    let mut found: Vec<JsValue> = fs::read_dir(dir.join("witness"))
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| parse_json(&fs::read_to_string(path).unwrap()).unwrap())
        .collect();
    found.sort_by_key(|receipt| {
        field(receipt, "seq")
            .as_number()
            .expect("#1370 receipt seq") as u64
    });
    found
}
fn matches(receipt: &JsValue, site: &str, phase: &str, key: &str) -> bool {
    field(receipt, "site")
        .as_string()
        .is_some_and(|s| s.equals_str(site))
        && field(receipt, "phase")
            .as_string()
            .is_some_and(|p| p.equals_str(phase))
        && field(receipt, "key")
            .as_string()
            .is_some_and(|k| k.equals_str(key))
}
/// Waits for one receipt. The timeout label is a static per-call assertion so
/// a mutation kill lands on a bindable named assertion, never a plain panic.
fn wait_receipt(dir: &Path, site: &str, phase: &str, key: &str, timeout: &str) -> JsValue {
    let deadline = Instant::now() + OBSERVE;
    loop {
        for receipt in receipts(dir) {
            if matches(&receipt, site, phase, key) {
                return receipt;
            }
        }
        negative_assert!(Instant::now() < deadline, "{timeout}");
        thread::sleep(POLL);
    }
}
fn assert_no_receipt(dir: &Path, site: &str, phase: &str, key: &str) {
    for receipt in receipts(dir) {
        negative_assert!(
            !matches(&receipt, site, phase, key),
            "#1370 unexpected {site} {phase} receipt for key {key}"
        );
    }
}
fn seq_of(receipt: &JsValue) -> u64 {
    field(receipt, "seq")
        .as_number()
        .expect("#1370 receipt seq") as u64
}
fn count_receipts(dir: &Path, site: &str, phase: &str, key: &str) -> usize {
    receipts(dir)
        .iter()
        .filter(|receipt| matches(receipt, site, phase, key))
        .count()
}

/// The real session writer over the real JsonlTranscriptStore, keyed like the
/// adapter tests (the store creates its own parent directories).
fn writer_fixture(root: &Path) -> (Arc<TranscriptWriter>, PathBuf) {
    let path = root.join("chat/s1.jsonl");
    let key = TranscriptKey {
        project_root: PathBuf::from("/pdrs-fixture/project"),
        pilot_home: PathBuf::new(),
        session_id: "s1".into(),
        transcript_path: path.clone(),
    };
    let writer = Arc::new(TranscriptWriter::for_store(
        Arc::new(JsonlTranscriptStore::new()),
        key,
    ));
    (writer, path)
}
fn status(event: &str) -> JsValue {
    object([
        ("event", event.into()),
        ("kind", "info".into()),
        ("text", "status text".into()),
    ])
}
fn metadata_value(n: usize) -> JsValue {
    object([("title", JsValue::String(format!("t{n}").into()))])
}
fn agent_status_event(event: &str) -> JsValue {
    object([
        ("type", "agent_status".into()),
        ("event", event.into()),
        ("runId", "r".into()),
    ])
}
fn run() -> RunRef {
    RunRef {
        project_key: Some("project".into()),
        session_key: "session".into(),
        run_id: "r".into(),
    }
}
fn accept_input() -> RunAcceptInput {
    RunAcceptInput {
        run: run(),
        request_material: "m".into(),
    }
}
fn text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

// Plain concatenation instead of format! literals, so the JSON braces never
// fight the format-string parser.
fn barrier_fragment(site: &str, id: &str, key: &str, bound_ms: f64) -> String {
    let mut text = String::from("\"");
    text.push_str(site);
    text.push_str("\":{\"barrier\":{\"id\":\"");
    text.push_str(id);
    text.push_str("\",\"key\":\"");
    text.push_str(key);
    text.push_str("\",\"boundMs\":");
    text.push_str(&bound_ms.to_string());
    text.push_str("}}");
    text
}
fn fault_fragment(site: &str, id: &str, key: &str) -> String {
    let mut text = String::from("\"");
    text.push_str(site);
    text.push_str("\":{\"fault\":{\"id\":\"");
    text.push_str(id);
    text.push_str("\",\"key\":\"");
    text.push_str(key);
    text.push_str("\"}}");
    text
}
fn config_text(fragments: &[String]) -> String {
    let mut text = String::from("{");
    text.push_str(&fragments.join(","));
    text.push('}');
    text
}

// Matrix 1 + 2: unset keeps the production default — the process-wide entry
// itself stays unarmed, no control files are read, no receipts are published.
#[test]
fn stimulus_unset_is_inert() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    negative_assert!(
        StorageStimulus::global().is_none(),
        "#1370 unset stimulus keeps the production default"
    );
    let (writer, path) = writer_fixture(&scratch.0);
    let written =
        writer.record_agent_status_message(&"s1".into(), &"t1".into(), &status("turn_timeout"));
    negative_assert!(
        written.is_ok(),
        "#1370 unset stimulus writes the writer normally"
    );
    let written = writer.record_session_metadata(&"s1".into(), &"t1".into(), metadata_value(1));
    negative_assert!(
        written.is_ok(),
        "#1370 unset stimulus writes the writer normally"
    );
    let registry = FileRunRegistry::open(scratch.0.join("runs.json")).unwrap();
    registry.accept(&accept_input()).unwrap();
    let appended = registry.append(&run(), &agent_status_event("turn_timeout"));
    negative_assert!(
        appended.is_ok(),
        "#1370 unset stimulus appends registry events normally"
    );
    let transcript = text(&path);
    negative_assert!(
        transcript.contains("turn_timeout") && transcript.contains("session_metadata"),
        "#1370 unset stimulus transcript persists: {transcript}"
    );
    negative_assert_eq!(
        registry.events(&run(), None, None).unwrap().len(),
        1,
        "#1370 unset stimulus registry persists"
    );
    negative_assert!(
        !scratch.0.join("witness").exists(),
        "#1370 unset stimulus publishes no receipts"
    );
}

// Matrix 3 + 4 + 5: the barrier holds the underlying append under the commit
// lock; a second append queues with a FIFO witness receipt; release.json
// unblocks exactly the blocked write and file order is the queue order.
#[test]
fn writer_barrier_blocks_underlying_append() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            "writer",
            "b1",
            "turn_timeout",
            HELD_BOUND_MS,
        )]),
    );
    let _global = arm_global(Some(&control));
    let (writer, path) = writer_fixture(&scratch.0);
    let blocked = {
        let writer = writer.clone();
        thread::spawn(move || {
            writer
                .record_agent_status_message(&"s1".into(), &"t1".into(), &status("turn_timeout"))
                .expect("#1370 blocked write completes after release")
        })
    };
    let block = wait_receipt(
        &control,
        "writer",
        "block",
        "turn_timeout",
        "#1370 writer barrier blocks the underlying append",
    );
    let block_seq = seq_of(&block);
    negative_assert_eq!(
        field(&block, "boundMs").as_number(),
        Some(HELD_BOUND_MS),
        "#1370 block receipt states the configured bound"
    );
    negative_assert!(
        !text(&path).contains("turn_timeout"),
        "#1370 underlying append is held before release"
    );
    let queued = {
        let writer = writer.clone();
        thread::spawn(move || {
            writer
                .record_session_metadata(&"s1".into(), &"t2".into(), metadata_value(2))
                .expect("#1370 queued write completes after the blocked one")
        })
    };
    wait_receipt(
        &control,
        "writer",
        "queued",
        "session_metadata",
        "#1370 writer queued receipt while the barrier holds",
    );
    negative_assert!(
        !text(&path).contains("session_metadata"),
        "#1370 queued write stays off disk while the barrier holds"
    );
    release(&control, block_seq);
    blocked.join().unwrap();
    queued.join().unwrap();
    let release_receipt = wait_receipt(
        &control,
        "writer",
        "release",
        "turn_timeout",
        "#1370 writer release receipt after release.json",
    );
    negative_assert_eq!(
        field(&release_receipt, "bounded"),
        &JsValue::Undefined,
        "#1370 explicit release is not bounded"
    );
    wait_receipt(
        &control,
        "writer",
        "complete",
        "turn_timeout",
        "#1370 writer complete receipt after the real write",
    );
    let transcript = text(&path);
    let blocked_at = transcript
        .find("turn_timeout")
        .expect("#1370 blocked write lands");
    let queued_at = transcript
        .find("session_metadata")
        .expect("#1370 queued write lands");
    negative_assert!(
        blocked_at < queued_at,
        "#1370 file order is the queue order: {transcript}"
    );
    // The queued write was never blocked, so it owes no complete receipt.
    assert_no_receipt(&control, "writer", "complete", "session_metadata");
}

// Matrix 6: without a release the barrier self-releases at the bound.
#[test]
fn writer_barrier_bound_releases_without_release_file() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        r#"{"writer":{"barrier":{"id":"b1","key":"turn_timeout","boundMs":150}}}"#,
    );
    let _global = arm_global(Some(&control));
    let (writer, path) = writer_fixture(&scratch.0);
    let blocked = thread::spawn(move || {
        writer
            .record_agent_status_message(&"s1".into(), &"t1".into(), &status("turn_timeout"))
            .expect("#1370 bounded release finishes the write")
    });
    wait_receipt(
        &control,
        "writer",
        "block",
        "turn_timeout",
        "#1370 bounded writer block receipt",
    );
    let release = wait_receipt(
        &control,
        "writer",
        "release",
        "turn_timeout",
        "#1370 bound expiry self-releases the barrier",
    );
    negative_assert_eq!(
        field(&release, "bounded"),
        &JsValue::Bool(true),
        "#1370 bound expiry releases with bounded true"
    );
    blocked.join().unwrap();
    wait_receipt(
        &control,
        "writer",
        "complete",
        "turn_timeout",
        "#1370 bounded writer complete receipt",
    );
    negative_assert!(
        text(&path).contains("turn_timeout"),
        "#1370 the bounded write still lands"
    );
}

// Matrix 7 + 8 + 9: ids are single-shot — a fired fault never refires, a
// consumed barrier never re-blocks; a new id re-arms.
#[test]
fn barrier_and_fault_ids_are_single_shot() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[fault_fragment("writer", "f1", "session_metadata")]),
    );
    let _global = arm_global(Some(&control));
    let (writer, path) = writer_fixture(&scratch.0);
    let error = writer
        .record_session_metadata(&"s1".into(), &"t1".into(), metadata_value(1))
        .unwrap_err();
    let AdapterError::Store(LedgerError::Io(error)) = &error else {
        panic!("#1370 writer fault surfaces as a store io error: {error:?}")
    };
    negative_assert_eq!(
        error.raw_os_error(),
        Some(5),
        "#1370 writer fault is EIO (raw os error 5), not another kind"
    );
    wait_receipt(
        &control,
        "writer",
        "fault",
        "session_metadata",
        "#1370 writer fault receipt",
    );
    negative_assert_eq!(text(&path), "", "#1370 faulted write leaves no bytes");
    // Same id: the fault never refires.
    let second = writer.record_session_metadata(&"s1".into(), &"t2".into(), metadata_value(2));
    negative_assert!(second.is_ok(), "#1370 the fault fires only once");
    negative_assert_eq!(
        count_receipts(&control, "writer", "fault", "session_metadata"),
        1,
        "#1370 fault id is single-shot"
    );
    // A new id arms the barrier; it blocks once and never again.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            "writer",
            "b2",
            "session_metadata",
            HELD_BOUND_MS,
        )]),
    );
    let blocked = {
        let writer = writer.clone();
        thread::spawn(move || {
            writer
                .record_session_metadata(&"s1".into(), &"t3".into(), metadata_value(3))
                .expect("#1370 barrier write completes")
        })
    };
    let block = wait_receipt(
        &control,
        "writer",
        "block",
        "session_metadata",
        "#1370 re-armed writer block receipt",
    );
    release(&control, seq_of(&block));
    blocked.join().unwrap();
    wait_receipt(
        &control,
        "writer",
        "complete",
        "session_metadata",
        "#1370 re-armed writer complete receipt",
    );
    let third = writer.record_session_metadata(&"s1".into(), &"t4".into(), metadata_value(4));
    negative_assert!(third.is_ok(), "#1370 the barrier blocks only once");
    negative_assert_eq!(
        count_receipts(&control, "writer", "block", "session_metadata"),
        1,
        "#1370 barrier id is single-shot"
    );
}

// Matrix 10 (registry twin): the fault is the single EIO from inside the
// mutation chain and the file lock; disk and later reads stay unchanged.
#[test]
fn registry_fault_is_single_eio_in_lock() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry = FileRunRegistry::open(scratch.0.join("runs.json")).unwrap();
    registry.accept(&accept_input()).unwrap();
    let before = fs::read(scratch.0.join("runs.json")).unwrap();
    arm(
        &control,
        &config_text(&[fault_fragment(
            "registry",
            "f1",
            "agent_status:turn_timeout",
        )]),
    );
    let _global = arm_global(Some(&control));
    let error = registry.append(&run(), &agent_status_event("turn_timeout"));
    negative_assert!(
        matches!(&error, Err(RunRegistryError::Io(e)) if e.raw_os_error() == Some(5)),
        "#1370 registry fault is EIO inside the lock: {error:?}"
    );
    wait_receipt(
        &control,
        "registry",
        "fault",
        "agent_status:turn_timeout",
        "#1370 registry fault receipt",
    );
    negative_assert_eq!(
        fs::read(scratch.0.join("runs.json")).unwrap(),
        before,
        "#1370 faulted registry persist leaves disk unchanged"
    );
    negative_assert_eq!(
        registry.get(&run()).unwrap().unwrap().last_seq(),
        Some(0.0),
        "#1370 faulted registry append does not advance memory state"
    );
    let retried = registry.append(&run(), &agent_status_event("turn_timeout"));
    negative_assert!(retried.is_ok(), "#1370 the registry fault fires only once");
    negative_assert_eq!(
        count_receipts(&control, "registry", "fault", "agent_status:turn_timeout"),
        1,
        "#1370 registry fault id is single-shot"
    );
    negative_assert_eq!(
        registry.get(&run()).unwrap().unwrap().last_seq(),
        Some(1.0),
        "#1370 the retry lands after the fault"
    );
}

// Matrix 11: the barrier holds the mutation chain lock — even a plain get()
// (which shares the chain lock) cannot proceed until the release.
#[test]
fn registry_barrier_holds_chain_lock() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry = Arc::new(FileRunRegistry::open(scratch.0.join("runs.json")).unwrap());
    registry.accept(&accept_input()).unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            "registry",
            "b1",
            "agent_status:turn_timeout",
            HELD_BOUND_MS,
        )]),
    );
    let _global = arm_global(Some(&control));
    let blocked = {
        let registry = registry.clone();
        thread::spawn(move || {
            registry
                .append(&run(), &agent_status_event("turn_timeout"))
                .expect("#1370 blocked registry append completes")
        })
    };
    let block = wait_receipt(
        &control,
        "registry",
        "block",
        "agent_status:turn_timeout",
        "#1370 registry barrier blocks the in-lock persist",
    );
    let reader = {
        let registry = registry.clone();
        thread::spawn(move || registry.get(&run()).unwrap())
    };
    thread::sleep(Duration::from_millis(200));
    negative_assert!(
        !reader.is_finished(),
        "#1370 get() is held behind the barrier's chain lock"
    );
    release(&control, seq_of(&block));
    blocked.join().unwrap();
    let record = reader.join().unwrap().expect("#1370 run record exists");
    negative_assert_eq!(
        record.state(),
        Some(RunState::Running),
        "#1370 the blocked append is durable after release"
    );
    let release_receipt = wait_receipt(
        &control,
        "registry",
        "release",
        "agent_status:turn_timeout",
        "#1370 registry release receipt after release.json",
    );
    negative_assert_eq!(
        field(&release_receipt, "bounded"),
        &JsValue::Undefined,
        "#1370 explicit registry release is not bounded"
    );
    wait_receipt(
        &control,
        "registry",
        "complete",
        "agent_status:turn_timeout",
        "#1370 registry complete receipt after the real persist",
    );
}

// Matrix 12: the runRegistry.append port publishes the queued witness at entry
// and the single worker preserves completion order behind a held barrier.
// The serial mutex is a std lock held across awaits on purpose: it excludes
// the sync sibling tests that re-arm the same process-wide stimulus entry.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_port_appends_queue_with_witness_receipts() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    let port = Arc::new(
        crate::runtime::turn::publish::FileRegistry::new(
            FileRunRegistry::open(scratch.0.join("runs.json")).unwrap(),
        )
        .unwrap(),
    );
    use crate::runtime::turn::publish::TurnRunRegistry;
    port.accept(&accept_input()).await.unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            "registry",
            "b1",
            "agent_status:turn_timeout",
            HELD_BOUND_MS,
        )]),
    );
    let _global = arm_global(Some(&control));
    let blocked = {
        let port = port.clone();
        tokio::spawn(async move {
            port.append(&run(), &agent_status_event("turn_timeout"))
                .await
                .expect("#1370 blocked port append completes")
        })
    };
    let block = wait_receipt(
        &control,
        "registry",
        "block",
        "agent_status:turn_timeout",
        "#1370 registry port blocks the append",
    );
    let queued = {
        let port = port.clone();
        tokio::spawn(async move {
            port.append(&run(), &agent_status_event("input_accepted"))
                .await
                .expect("#1370 queued port append completes")
        })
    };
    wait_receipt(
        &control,
        "registry",
        "queued",
        "agent_status:input_accepted",
        "#1370 registry queued receipt is published while held",
    );
    release(&control, seq_of(&block));
    tokio::time::timeout(OBSERVE, blocked)
        .await
        .expect("#1370 blocked port append finishes within the observation cap")
        .unwrap();
    tokio::time::timeout(OBSERVE, queued)
        .await
        .expect("#1370 queued port append finishes within the observation cap")
        .unwrap();
    let events = port.events(&run()).await.unwrap();
    let first = field(&events[0], "event")
        .as_string()
        .expect("#1370 event name");
    let second = field(&events[1], "event")
        .as_string()
        .expect("#1370 event name");
    negative_assert!(
        first.equals_str("turn_timeout") && second.equals_str("input_accepted"),
        "#1370 single worker keeps completion order: first={first:?} second={second:?}"
    );
    wait_receipt(
        &control,
        "registry",
        "complete",
        "agent_status:turn_timeout",
        "#1370 registry port complete receipt",
    );
    // The queued write was never blocked, so it owes no complete receipt.
    assert_no_receipt(
        &control,
        "registry",
        "complete",
        "agent_status:input_accepted",
    );
}

// Matrix 13: two site barriers held at once release by their own block receipt
// sequence numbers and never release each other.
#[test]
fn two_site_barriers_release_by_sequence() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry = Arc::new(FileRunRegistry::open(scratch.0.join("runs.json")).unwrap());
    registry.accept(&accept_input()).unwrap();
    arm(
        &control,
        &config_text(&[
            barrier_fragment("writer", "w1", "session_metadata", HELD_BOUND_MS),
            barrier_fragment("registry", "r1", "agent_status:turn_timeout", HELD_BOUND_MS),
        ]),
    );
    let _global = arm_global(Some(&control));
    let (writer, path) = writer_fixture(&scratch.0);
    let writer_blocked = {
        let writer = writer.clone();
        thread::spawn(move || {
            writer
                .record_session_metadata(&"s1".into(), &"t1".into(), metadata_value(1))
                .expect("#1370 writer barrier completes")
        })
    };
    let writer_block = wait_receipt(
        &control,
        "writer",
        "block",
        "session_metadata",
        "#1370 two-site writer block receipt",
    );
    let registry_blocked = {
        let registry = registry.clone();
        thread::spawn(move || {
            registry
                .append(&run(), &agent_status_event("turn_timeout"))
                .expect("#1370 registry barrier completes")
        })
    };
    let registry_block = wait_receipt(
        &control,
        "registry",
        "block",
        "agent_status:turn_timeout",
        "#1370 two-site registry block receipt",
    );
    // Releasing the registry sequence frees only the registry barrier.
    release(&control, seq_of(&registry_block));
    registry_blocked.join().unwrap();
    wait_receipt(
        &control,
        "registry",
        "complete",
        "agent_status:turn_timeout",
        "#1370 two-site registry complete receipt",
    );
    thread::sleep(Duration::from_millis(200));
    negative_assert!(
        !writer_blocked.is_finished(),
        "#1370 writer barrier stays held behind the registry release"
    );
    negative_assert!(
        !text(&path).contains("session_metadata"),
        "#1370 two-site writer write stays held"
    );
    release(&control, seq_of(&writer_block));
    writer_blocked.join().unwrap();
    wait_receipt(
        &control,
        "writer",
        "complete",
        "session_metadata",
        "#1370 two-site writer complete receipt",
    );
    negative_assert!(
        text(&path).contains("session_metadata"),
        "#1370 writer write lands after its own release"
    );
    negative_assert_eq!(
        registry.get(&run()).unwrap().unwrap().last_seq(),
        Some(1.0),
        "#1370 registry write lands after its own release"
    );
}

// Matrix 14 + 15: a malformed control file and a missing control directory
// both fail the controlled operation closed.
#[test]
fn invalid_control_directory_fails_closed() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let control = scratch.control();
    let (writer, _path) = writer_fixture(&scratch.0);
    let registry = FileRunRegistry::open(scratch.0.join("runs.json")).unwrap();
    registry.accept(&accept_input()).unwrap();
    // Malformed JSON: both sites fail closed with a stimulus-marked error.
    arm(&control, "{ not json");
    {
        let _global = arm_global(Some(&control));
        let writer_error = writer
            .record_session_metadata(&"s1".into(), &"t1".into(), metadata_value(1))
            .err()
            .map(|error| error.to_string());
        negative_assert!(
            writer_error
                .as_deref()
                .is_some_and(|error| error.contains("stimulus")),
            "#1370 malformed stimulus.json fails the writer closed"
        );
        let registry_error = registry
            .append(&run(), &agent_status_event("turn_timeout"))
            .err()
            .map(|error| error.to_string());
        negative_assert!(
            registry_error
                .as_deref()
                .is_some_and(|error| error.contains("stimulus")),
            "#1370 malformed stimulus.json fails the registry closed: {registry_error:?}"
        );
    }
    // The directory disappearing mid-run fails closed with an io error.
    {
        let _global = arm_global(Some(&scratch.0.join("missing-control")));
        let writer_error =
            writer.record_session_metadata(&"s1".into(), &"t2".into(), metadata_value(2));
        negative_assert!(
            writer_error.is_err(),
            "#1370 missing control directory fails the writer closed"
        );
        let registry_error = registry.append(&run(), &agent_status_event("turn_timeout"));
        negative_assert!(
            registry_error.is_err(),
            "#1370 missing control directory fails the registry closed"
        );
    }
}

// Apparatus unit checks: the key derivation the #1368 stimulus plan declares.
#[test]
fn stimulus_keys_derive_as_declared() {
    let _serial = serial().lock().unwrap_or_else(|e| e.into_inner());
    let entry = |json: &str| parse_json(json).unwrap();
    let keys: Vec<String> = [
        r#"{"type":"agent_status_message","event":"turn_timeout"}"#,
        r#"{"type":"agent_status_message","event":"gateway_submit_failed"}"#,
        r#"{"type":"session_metadata"}"#,
        r#"{"type":"agent_status_message","event":""}"#,
    ]
    .iter()
    .map(|json| super::stimulus::writer_entry_key(&entry(json)))
    .collect();
    negative_assert_eq!(
        keys[0],
        "turn_timeout",
        "#1370 writer key is the event name"
    );
    negative_assert_eq!(
        keys[1],
        "gateway_submit_failed",
        "#1370 writer key is the event name"
    );
    negative_assert_eq!(keys[2], "session_metadata", "#1370 writer key is the type");
    negative_assert_eq!(
        keys[3],
        "agent_status_message",
        "#1370 an empty event falls back to the type"
    );
    let registry_keys: Vec<String> = [
        r#"{"type":"agent_status","event":"turn_timeout"}"#,
        r#"{"type":"error","code":"result_unknown"}"#,
        r#"{"type":"turn_completed"}"#,
    ]
    .iter()
    .map(|json| super::stimulus::registry_event_key(&entry(json)))
    .collect();
    negative_assert_eq!(
        registry_keys[0],
        "agent_status:turn_timeout",
        "#1370 registry key is agent_status:<event>"
    );
    negative_assert_eq!(
        registry_keys[1],
        "error:result_unknown",
        "#1370 registry key is error:<code>"
    );
    negative_assert_eq!(
        registry_keys[2],
        "turn_completed",
        "#1370 registry key is the type"
    );
}
