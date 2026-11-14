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
    sync::Arc,
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
/// Barrier bound for the publication-path witness, where the receipt must land
/// before the collector releases: the bound has to outlive the whole
/// observation cap, or a bound-expiry release would unblock the predecessor and
/// a receipt written by a lazily dispatched append would look correct.
const OUTLIVED_BOUND_MS: f64 = 30000.0;
/// The release guard's own budget (`BarrierRelease::drop`): re-armed on every
/// pass, and only a receipt that actually advanced may extend it (the
/// `drop-budget-refreshed-in-release-branch` control pins the first half; the
/// `guard-budget-stretched` control pins that it is not silently stretched).
const GUARD_BUDGET: Duration = Duration::from_secs(2);
/// Scheduling allowance on top of `GUARD_BUDGET` when the stalled-guard test
/// measures when the guard's thread ended: thread startup delay and poll
/// granularity eat into it, so a correct guard lands in
/// `GUARD_BUDGET ..= GUARD_BUDGET + GUARD_STALL_ALLOWANCE`.
const GUARD_STALL_ALLOWANCE: Duration = Duration::from_secs(1);
/// The shared (site, key) pairs the fragments, release guards, and receipt
/// queries in this file use (#1781): one constant per pair, so every site/key
/// routes through one name and a retargeted key cannot leave the guard
/// releasing a barrier the queries never watched — or the queries waiting on
/// a barrier the guard never released. Honest scope: this is name-level
/// convergence only — the pair stays a positional tuple, so a `.0`/`.1` swap
/// still compiles (struct-shaped pairs are the P2 follow-up), and the
/// constants make drift visible to review, not impossible to the compiler.
/// The producer side of each registry key is the event literal inside the
/// `agent_status_event("…")` appends below (`agent_status:<event>` per
/// `registry_event_key`); the reverse assertions bind these same constants, so
/// a drifted producer literal fails them instead of vacating them silently.
const REGISTRY_TURN_TIMEOUT: (&str, &str) = ("registry", "agent_status:turn_timeout");
const REGISTRY_INPUT_ACCEPTED: (&str, &str) = ("registry", "agent_status:input_accepted");
const REGISTRY_CANCELLED: (&str, &str) = ("registry", "agent_status:cancelled");
const WRITER_SESSION_METADATA: (&str, &str) = ("writer", "session_metadata");
const WRITER_TURN_TIMEOUT: (&str, &str) = ("writer", "turn_timeout");

/// The controlled entry for one test: a dedicated instance injected into the
/// test's own writer and registry stores at construction, so an armed
/// stimulus can never be observed by any other test in the same process.
fn armed(dir: &Path) -> Arc<StorageStimulus> {
    Arc::new(StorageStimulus::control(dir))
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
    wait_receipt_after(dir, site, phase, key, 0, timeout)
}

/// Waits for one receipt whose sequence number is above `after` (#1805): a key
/// that already completed an earlier round has that round's receipts on disk,
/// and only the sequence keeps the two rounds apart.
fn wait_receipt_after(
    dir: &Path,
    site: &str,
    phase: &str,
    key: &str,
    after: u64,
    timeout: &str,
) -> JsValue {
    let deadline = Instant::now() + OBSERVE;
    loop {
        for receipt in receipts(dir) {
            if matches(&receipt, site, phase, key) && seq_of(&receipt) > after {
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
/// Pair-shaped queries (#1781): the shared (site, key) constants stay
/// positional tuples (struct-shaped pairs are the #2951 follow-up), so these
/// unpack the pair exactly once per call instead of leaving a `.0`/`.1` swap
/// face at the call site. Coverage is deliberately partial: only the 7 direct
/// `wait_receipt`/`assert_no_receipt` pair call sites converted in this
/// change go through these; the rest of the file still unfolds constants
/// positionally — `wait_receipt_after` (no pair variant), constructor-style
/// `BarrierRelease::new`/`barrier_fragment`, and the `matches`/`count_receipts`
/// scans — converging those is follow-up work, not a claim made here.
fn wait_receipt_pair(dir: &Path, pair: (&str, &str), phase: &str, timeout: &str) -> JsValue {
    let (site, key) = pair;
    wait_receipt(dir, site, phase, key, timeout)
}
fn assert_no_receipt_pair(dir: &Path, pair: (&str, &str), phase: &str) {
    let (site, key) = pair;
    assert_no_receipt(dir, site, phase, key)
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
/// The highest sequence number of a `phase` receipt for `site`/`key` that is
/// newer than `after`, if any (#1805: the current barrier for a key is its
/// latest `block` receipt, and only receipts newer than it belong to that
/// round — an earlier round's receipts must not read as the current state).
/// Deliberately non-panicking (`BarrierRelease::drop` calls it while a failed
/// assertion is unwinding, where a second panic aborts the process) and
/// order-independent: `read_dir` returns entries in no defined order, so the
/// caller must pick by sequence, never by the first match. Filters by the same
/// `.json` extension `receipts()` uses, so a `*.json.tmp` write in flight (or
/// any other sidecar) is never counted as a receipt.
fn receipt_seq(dir: &Path, site: &str, phase: &str, key: &str, after: u64) -> Option<u64> {
    let entries = fs::read_dir(dir.join("witness")).ok()?;
    let mut seqs: Vec<u64> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(receipt) = parse_json(&text) else {
            continue;
        };
        if matches(&receipt, site, phase, key)
            && let Some(seq) = field(&receipt, "seq").as_number()
            && seq > after as f64
        {
            seqs.push(seq as u64);
        }
    }
    seqs.sort_unstable();
    seqs.pop()
}
/// Releases a still-held barrier before `Scratch` deletes its control directory.
/// Without it a failing assertion leaves the worker polling a removed directory
/// until its bound re-creates the control files, so `/tmp/pdrs-stimulus-*` is
/// never removed. Declared after `Scratch`, this drops first (reverse order).
///
/// Coverage is bounded by what the fixture can know: the release sequence only
/// exists once the barrier has been consumed and written its `block` receipt,
/// so a failure between `arm` and that receipt still leaves the window B3
/// describes. From the block receipt on, this guard covers every later
/// assertion, including the ones before the test's own `release` call. `site`
/// is a parameter because the writer barrier and the two-site case hold
/// barriers on the writer track as well as the registry one.
struct BarrierRelease {
    control: PathBuf,
    site: &'static str,
    key: &'static str,
}
impl BarrierRelease {
    fn new(control: &Path, site: &'static str, key: &'static str) -> Self {
        Self {
            control: control.to_path_buf(),
            site,
            key,
        }
    }
}
impl Drop for BarrierRelease {
    fn drop(&mut self) {
        // Re-sample on every pass, never once: a worker that only reaches the
        // barrier while this guard is already spinning has not written its
        // `block` receipt yet, and a one-shot sample would then skip the release
        // entirely — the leak this guard exists to stop.
        //
        // #1805: every check pairs with the *current* barrier — a `block`
        // receipt for this site/key newer than the previous round's. A key
        // that completed an earlier round still has that round's release and
        // complete receipts on disk, and an unpaired read would call that
        // history "already released" and skip the release the current barrier
        // is waiting on.
        let observe =
            |phase: &str, after: u64| receipt_seq(&self.control, self.site, phase, self.key, after);
        let sample =
            |barrier: Option<u64>| (barrier.and_then(|seq| observe("release", seq)), barrier);
        // Anchor semantics, honestly: `observe("block", 0)` reads the highest
        // `block` receipt on disk, which is only *provisionally* the current
        // round's. Until this round's `block` receipt lands, the anchor is the
        // previous round's, and that round's release/complete then pair as
        // "already released and complete" — the guard breaks without writing
        // release.json, the current barrier waits out its bound, and once
        // `Scratch` has removed the tree the worker re-creates
        // `/tmp/pdrs-stimulus-*` (the B3 leak). Kept as-is: receipts alone
        // cannot distinguish "this round has not triggered yet" from "this
        // round is the last block on disk", so a per-round floor needs state
        // the fixture does not have.
        let mut deadline = Instant::now() + GUARD_BUDGET;
        let mut observed = sample(observe("block", 0));
        loop {
            // The temp name below is guard-specific on purpose: `write_atomic`
            // (behind the `release()` helper) stages the same final
            // `release.json` through `release.json.tmp`, and this `Drop` can
            // run on its own thread while the test body is still mid-release.
            // A shared temp name leaves a window where one side's rename finds
            // the other side's file gone — a plain ENOENT panic, or a release
            // stamped with the wrong seq (#1781 review: mutual-overwrite
            // window). The sidecar never reaches a receipt scan anyway —
            // `receipt_seq` reads `witness/`, the sidecar lives in `control/`
            // — and its `tmp` extension would fail the `.json` filter even
            // if it did.
            if observed.0.is_none()
                && let Some(seq) = observed.1
            {
                // Non-panicking release: a `.unwrap()` here could abort during unwind.
                let temporary = self.control.join("release.json.guard.tmp");
                if fs::write(&temporary, format!("{{\"seq\":{seq}}}")).is_ok() {
                    let _ = fs::rename(&temporary, self.control.join("release.json"));
                }
            }
            // Only a receipt that actually advanced earns the durable write behind
            // it a fresh budget — the barrier the worker reaches late must not
            // leave that write with whatever is left of the block-receipt wait.
            // A stalled or failing barrier extends nothing, so `Drop` still ends:
            // at most the `block` and `release` transitions can push it out.
            let current = sample(observe("block", 0));
            if current != observed {
                observed = current;
                deadline = Instant::now() + GUARD_BUDGET;
            }
            // The durable write behind the barrier lands after the release; wait
            // for its `complete` receipt so `Scratch` does not remove the
            // directory (and race the worker into re-creating it) before that
            // write finishes.
            if observed
                .1
                .and_then(|seq| observe("complete", seq))
                .is_some()
                || Instant::now() >= deadline
            {
                break;
            }
            thread::sleep(POLL);
        }
    }
}

/// The real session writer over the real JsonlTranscriptStore, keyed like the
/// adapter tests (the store creates its own parent directories). The stimulus
/// handle is injected into the store, so only this writer witnesses it.
fn writer_fixture(
    root: &Path,
    stimulus: Option<Arc<StorageStimulus>>,
) -> (Arc<TranscriptWriter>, PathBuf) {
    let path = root.join("chat/s1.jsonl");
    let key = TranscriptKey {
        project_root: PathBuf::from("/pdrs-fixture/project"),
        pilot_home: PathBuf::new(),
        session_id: "s1".into(),
        transcript_path: path.clone(),
    };
    let writer = Arc::new(TranscriptWriter::for_store(
        Arc::new(JsonlTranscriptStore::with_stimulus(stimulus)),
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
    let run = run();
    RunAcceptInput {
        session_key_raw: JsValue::String(run.session_key.clone()),
        run,
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
    let scratch = Scratch::new();
    negative_assert!(
        StorageStimulus::global().is_none(),
        "#1370 unset stimulus keeps the production default"
    );
    let (writer, path) = writer_fixture(&scratch.0, None);
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
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            WRITER_TURN_TIMEOUT.0,
            "b1",
            WRITER_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    let _release_guard =
        BarrierRelease::new(&control, WRITER_TURN_TIMEOUT.0, WRITER_TURN_TIMEOUT.1);
    let (writer, path) = writer_fixture(&scratch.0, Some(armed(&control)));
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
        WRITER_TURN_TIMEOUT.0,
        "block",
        WRITER_TURN_TIMEOUT.1,
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
        WRITER_SESSION_METADATA.0,
        "queued",
        WRITER_SESSION_METADATA.1,
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
        WRITER_TURN_TIMEOUT.0,
        "release",
        WRITER_TURN_TIMEOUT.1,
        "#1370 writer release receipt after release.json",
    );
    negative_assert_eq!(
        field(&release_receipt, "bounded"),
        &JsValue::Undefined,
        "#1370 explicit release is not bounded"
    );
    wait_receipt(
        &control,
        WRITER_TURN_TIMEOUT.0,
        "complete",
        WRITER_TURN_TIMEOUT.1,
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
    assert_no_receipt(
        &control,
        WRITER_SESSION_METADATA.0,
        "complete",
        WRITER_SESSION_METADATA.1,
    );
}

// Matrix 6: without a release the barrier self-releases at the bound.
#[test]
fn writer_barrier_bound_releases_without_release_file() {
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            WRITER_TURN_TIMEOUT.0,
            "b1",
            WRITER_TURN_TIMEOUT.1,
            150.0,
        )]),
    );
    let (writer, path) = writer_fixture(&scratch.0, Some(armed(&control)));
    let blocked = thread::spawn(move || {
        writer
            .record_agent_status_message(&"s1".into(), &"t1".into(), &status("turn_timeout"))
            .expect("#1370 bounded release finishes the write")
    });
    wait_receipt(
        &control,
        WRITER_TURN_TIMEOUT.0,
        "block",
        WRITER_TURN_TIMEOUT.1,
        "#1370 bounded writer block receipt",
    );
    let release = wait_receipt(
        &control,
        WRITER_TURN_TIMEOUT.0,
        "release",
        WRITER_TURN_TIMEOUT.1,
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
        WRITER_TURN_TIMEOUT.0,
        "complete",
        WRITER_TURN_TIMEOUT.1,
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
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[fault_fragment(
            WRITER_SESSION_METADATA.0,
            "f1",
            WRITER_SESSION_METADATA.1,
        )]),
    );
    let (writer, path) = writer_fixture(&scratch.0, Some(armed(&control)));
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
        WRITER_SESSION_METADATA.0,
        "fault",
        WRITER_SESSION_METADATA.1,
        "#1370 writer fault receipt",
    );
    negative_assert_eq!(text(&path), "", "#1370 faulted write leaves no bytes");
    // Same id: the fault never refires.
    let second = writer.record_session_metadata(&"s1".into(), &"t2".into(), metadata_value(2));
    negative_assert!(second.is_ok(), "#1370 the fault fires only once");
    negative_assert_eq!(
        count_receipts(
            &control,
            WRITER_SESSION_METADATA.0,
            "fault",
            WRITER_SESSION_METADATA.1
        ),
        1,
        "#1370 fault id is single-shot"
    );
    // A new id arms the barrier; it blocks once and never again.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            WRITER_SESSION_METADATA.0,
            "b2",
            WRITER_SESSION_METADATA.1,
            HELD_BOUND_MS,
        )]),
    );
    let _release_guard = BarrierRelease::new(
        &control,
        WRITER_SESSION_METADATA.0,
        WRITER_SESSION_METADATA.1,
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
        WRITER_SESSION_METADATA.0,
        "block",
        WRITER_SESSION_METADATA.1,
        "#1370 re-armed writer block receipt",
    );
    release(&control, seq_of(&block));
    blocked.join().unwrap();
    wait_receipt(
        &control,
        WRITER_SESSION_METADATA.0,
        "complete",
        WRITER_SESSION_METADATA.1,
        "#1370 re-armed writer complete receipt",
    );
    let third = writer.record_session_metadata(&"s1".into(), &"t4".into(), metadata_value(4));
    negative_assert!(third.is_ok(), "#1370 the barrier blocks only once");
    negative_assert_eq!(
        count_receipts(
            &control,
            WRITER_SESSION_METADATA.0,
            "block",
            WRITER_SESSION_METADATA.1
        ),
        1,
        "#1370 barrier id is single-shot"
    );
}

// Matrix 10 (registry twin): the fault is the single EIO from inside the
// mutation chain and the file lock; disk and later reads stay unchanged.
#[test]
fn registry_fault_is_single_eio_in_lock() {
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry =
        FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
            .unwrap();
    registry.accept(&accept_input()).unwrap();
    let before = fs::read(scratch.0.join("runs.json")).unwrap();
    arm(
        &control,
        &config_text(&[fault_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "f1",
            REGISTRY_TURN_TIMEOUT.1,
        )]),
    );
    let error = registry.append(&run(), &agent_status_event("turn_timeout"));
    negative_assert!(
        matches!(&error, Err(RunRegistryError::Io(e)) if e.raw_os_error() == Some(5)),
        "#1370 registry fault is EIO inside the lock: {error:?}"
    );
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "fault",
        REGISTRY_TURN_TIMEOUT.1,
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
        count_receipts(
            &control,
            REGISTRY_TURN_TIMEOUT.0,
            "fault",
            REGISTRY_TURN_TIMEOUT.1
        ),
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
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry = Arc::new(
        FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
            .unwrap(),
    );
    registry.accept(&accept_input()).unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "b1",
            REGISTRY_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    // Bound by `registry-barrier-outside-lock`, so this case takes the failure
    // path on every negative-control run: without the guard the blocked thread
    // outlives `Scratch` and re-creates `/tmp/pdrs-stimulus-*`.
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
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
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
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
        REGISTRY_TURN_TIMEOUT.0,
        "release",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 registry release receipt after release.json",
    );
    negative_assert_eq!(
        field(&release_receipt, "bounded"),
        &JsValue::Undefined,
        "#1370 explicit registry release is not bounded"
    );
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "complete",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 registry complete receipt after the real persist",
    );
}

// Matrix 12: the runRegistry.append port publishes the queued witness at entry
// and the single worker preserves completion order behind a held barrier. The
// stimulus is injected into the wrapped registry, so the port captures the
// same instance handle at construction and no other test can observe it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_port_appends_queue_with_witness_receipts() {
    let scratch = Scratch::new();
    let control = scratch.control();
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let port = Arc::new(
        crate::runtime::turn::publish::FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    use crate::runtime::turn::publish::TurnRunRegistry;
    port.accept(&accept_input()).await.unwrap();
    // `HELD_BOUND_MS` is deliberate here, not the `OUTLIVED_BOUND_MS` rule
    // above: this case waits for the `queued` receipt *while held*. A bound
    // expiry would release the barrier, make the second append owe no receipt
    // and fail the very `wait_receipt` that proves the entry tick — a false
    // FAIL, never the lazy-dispatch false PASS the long bound guards against.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "b1",
            REGISTRY_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
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
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
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
    wait_receipt_pair(
        &control,
        REGISTRY_INPUT_ACCEPTED,
        "queued",
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
        REGISTRY_TURN_TIMEOUT.0,
        "complete",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 registry port complete receipt",
    );
    // The queued write was never blocked, so it owes no complete receipt.
    assert_no_receipt_pair(&control, REGISTRY_INPUT_ACCEPTED, "complete");
}

// Matrix 12b: the same queued witness through the real publication path. The
// port alone is not enough: `Publication` serializes by predecessor, so the
// second append's future is only first polled after the blocked one settles.
// The receipt is owed to the entry, not to the first poll, so it must be
// decided and dispatched when `append` is invoked — with the barrier still
// held — otherwise the collector waits for a receipt that never comes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_queued_receipt_survives_predecessor_serialization() {
    use crate::runtime::turn::coordinator::TurnEventCoordinator;
    use crate::runtime::turn::publish::{FileRegistry, TurnPublisher, TurnRunRegistry};
    let scratch = Scratch::new();
    let control = scratch.control();
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let registry = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    registry.accept(&accept_input()).await.unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "p1",
            REGISTRY_TURN_TIMEOUT.1,
            OUTLIVED_BOUND_MS,
        )]),
    );
    // #2692: a string session key is its own router-domain key.
    let publisher = TurnPublisher::start(
        run(),
        run().session_key,
        Some(registry.clone()),
        Arc::new(TurnEventCoordinator::new()),
    );
    negative_assert!(
        publisher.gateway_event(agent_status_event("turn_timeout")),
        "#1370 publication admits the blocked event"
    );
    let block = wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 publication blocks the first append",
    );
    negative_assert!(
        publisher.gateway_event(agent_status_event("input_accepted")),
        "#1370 publication admits the event queued behind the barrier"
    );
    // The second append's future is not polled until the blocked one settles,
    // so this receipt only exists if the entry tick decided it.
    wait_receipt_pair(
        &control,
        REGISTRY_INPUT_ACCEPTED,
        "queued",
        "#1370 publication publishes the queued receipt while the barrier is held",
    );
    release(&control, seq_of(&block));
    tokio::time::timeout(OBSERVE, publisher.finish())
        .await
        .expect("#1370 publication finishes within the observation cap");
    let events = registry.events(&run()).await.unwrap();
    negative_assert_eq!(
        events.len(),
        2,
        "#1370 publication persists the queued append after the blocked one"
    );
    let first = field(&events[0], "event")
        .as_string()
        .expect("#1370 event name");
    negative_assert!(
        first.equals_str("turn_timeout"),
        "#1370 publication keeps FIFO behind the barrier: first={first:?}"
    );
}

// Matrix 12c: a queued receipt that cannot be written fails the append before
// the durable job reaches the worker. Otherwise the caller would see a failed
// persistence while the event landed anyway, and a retry would write it twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_queued_witness_fails_the_append_before_the_worker() {
    use crate::runtime::turn::publish::{FileRegistry, TurnRunRegistry};
    let scratch = Scratch::new();
    let control = scratch.control();
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let port = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    port.accept(&accept_input()).await.unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "w1",
            REGISTRY_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    let blocked = {
        let port = port.clone();
        tokio::spawn(async move {
            port.append(&run(), &agent_status_event("turn_timeout"))
                .await
        })
    };
    let block = wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 witness failure fixture blocks the first append",
    );
    // Sabotage exactly the next receipt: the atomic rename cannot replace a
    // non-empty directory, so this one write fails and nothing else does.
    let queued_path = control
        .join("witness")
        .join(format!("{:06}.json", seq_of(&block) + 1));
    fs::create_dir(&queued_path).unwrap();
    fs::write(queued_path.join("occupied"), "x").unwrap();
    let failed = port
        .append(&run(), &agent_status_event("input_accepted"))
        .await;
    negative_assert!(
        failed.is_err(),
        "#1370 a failed queued witness fails the append"
    );
    release(&control, seq_of(&block));
    tokio::time::timeout(OBSERVE, blocked)
        .await
        .expect("#1370 blocked append finishes within the observation cap")
        .expect("#1370 blocked append did not panic")
        .expect("#1370 blocked append persists");
    let events = port.events(&run()).await.unwrap();
    let durable = events.iter().any(|event| {
        field(event, "event")
            .as_string()
            .is_some_and(|n| n.equals_str("input_accepted"))
    });
    negative_assert!(
        !durable,
        "#1370 a failed queued witness never dispatches the durable append"
    );
}

// Matrix 12d: a barrier that fails while held must release its hold. A hold
// left behind stamps every later entry at the site with a receipt it does not
// owe, so the collector would see a queued witness that never existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_barrier_releases_its_hold_before_later_entries() {
    use crate::runtime::turn::publish::{FileRegistry, TurnRunRegistry};
    let scratch = Scratch::new();
    let control = scratch.control();
    let port = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    port.accept(&accept_input()).await.unwrap();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "f1",
            REGISTRY_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    // A directory is not a release marker: reading it fails the held barrier.
    fs::create_dir(control.join("release.json")).unwrap();
    let failed = port
        .append(&run(), &agent_status_event("turn_timeout"))
        .await;
    negative_assert!(
        failed.is_err(),
        "#1370 an unreadable release fails the held append"
    );
    let appended = port
        .append(&run(), &agent_status_event("input_accepted"))
        .await;
    negative_assert!(
        appended.is_ok(),
        "#1370 the site accepts entries again after a failed barrier"
    );
    let (site, key) = REGISTRY_INPUT_ACCEPTED;
    let stamped = receipts(&control)
        .iter()
        .any(|receipt| matches(receipt, site, "queued", key));
    negative_assert!(
        !stamped,
        "#1370 a failed barrier releases its hold before later entries"
    );
}

// Matrix 12e: a witnessed append keeps its FIFO slot. Its job is queued on the
// entry tick like every other append; only its execution waits for the receipt.
// Queueing it after the receipt settles instead lets a later plain append (one
// that owes no receipt, because the hold is gone) overtake it, and the durable
// order stops matching the host's call order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn witnessed_append_keeps_its_fifo_slot_before_a_later_plain_append() {
    use crate::runtime::turn::publish::{FileRegistry, TurnRunRegistry};
    let scratch = Scratch::new();
    let control = scratch.control();
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let port = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    port.accept(&accept_input()).await.unwrap();
    // The bound must outlive the observation cap: a barrier that self-released
    // here would leave the second append owing no receipt, and the ordering
    // assertion would pass without ever exercising the witnessed slot.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "q1",
            REGISTRY_TURN_TIMEOUT.1,
            OUTLIVED_BOUND_MS,
        )]),
    );
    let blocked = {
        let port = port.clone();
        tokio::spawn(async move {
            port.append(&run(), &agent_status_event("turn_timeout"))
                .await
        })
    };
    let block = wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 fifo fixture blocks the first append",
    );
    // Entry tick while the barrier is held: this append owes a receipt, and its
    // job takes the slot right behind the blocked one. The future is kept
    // unpolled on purpose — the slot must not depend on when it is first polled.
    let witnessed = port.append(&run(), &agent_status_event("input_accepted"));
    // The receipt is written on the entry tick, before the future is polled, so
    // seeing it proves this append really took the witnessed branch.
    wait_receipt_pair(
        &control,
        REGISTRY_INPUT_ACCEPTED,
        "queued",
        "#1370 fifo fixture owes the second append a receipt",
    );
    release(&control, seq_of(&block));
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "release",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 fifo fixture releases the barrier",
    );
    // The hold is gone with the release, so this append owes no receipt and is
    // queued on its own tick — behind the witnessed one only if that one was
    // queued at entry.
    let plain = port.append(&run(), &agent_status_event("cancelled"));
    tokio::time::timeout(OBSERVE, blocked)
        .await
        .expect("#1370 fifo blocked append finishes within the observation cap")
        .expect("#1370 fifo blocked append did not panic")
        .expect("#1370 fifo blocked append persists");
    tokio::time::timeout(OBSERVE, witnessed)
        .await
        .expect("#1370 fifo witnessed append finishes within the observation cap")
        .expect("#1370 fifo witnessed append persists after the blocked one");
    tokio::time::timeout(OBSERVE, plain)
        .await
        .expect("#1370 fifo plain append finishes within the observation cap")
        .expect("#1370 fifo plain append persists");
    // The third append entered after the release cleared the hold, so it owes
    // no queued receipt. Without this reverse assertion a regression that
    // published `release` before clearing the hold would stamp `cancelled` with
    // a receipt and still keep the FIFO order below, passing unnoticed.
    assert_no_receipt_pair(&control, REGISTRY_CANCELLED, "queued");
    let events = port.events(&run()).await.unwrap();
    let names: Vec<String> = events
        .iter()
        .map(|event| {
            field(event, "event")
                .as_string()
                .expect("#1370 fifo event name")
                .to_utf8()
                .expect("#1370 fifo event name is UTF-8")
        })
        .collect();
    negative_assert_eq!(
        names,
        vec![
            "turn_timeout".to_string(),
            "input_accepted".to_string(),
            "cancelled".to_string()
        ],
        "#1370 a witnessed append keeps its FIFO slot before a later plain append"
    );
}

// Matrix 12f: a caller may invoke `append` and withhold the returned future
// (legal Rust). The witnessed job must not park the single registry worker on
// that withheld future: the receipt write settles it, so the append behind it
// still completes. When the settle came from the caller's first poll instead, a
// withheld future starved every later append for up to `WITNESS_SETTLE` (60 s),
// far beyond the observation cap.
//
// The barrier must be *held* when the withheld append enters, otherwise it owes
// no receipt and takes the plain path — the case would then pass without ever
// reaching the witnessed branch. Hence the order below: trip the barrier with a
// first append, wait for its `block` receipt (that is when the hold is set), and
// only then invoke the append under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn withheld_witnessed_future_does_not_park_the_registry_worker() {
    use crate::runtime::turn::publish::{FileRegistry, TurnRunRegistry};
    let scratch = Scratch::new();
    let control = scratch.control();
    let _release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let port = Arc::new(
        FileRegistry::new(
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
                .unwrap(),
        )
        .unwrap(),
    );
    port.accept(&accept_input()).await.unwrap();
    // The bound must outlive the observation cap: a barrier that self-released
    // would clear the hold and hand the second append the plain path.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "h1",
            REGISTRY_TURN_TIMEOUT.1,
            OUTLIVED_BOUND_MS,
        )]),
    );
    let blocked = {
        let port = port.clone();
        tokio::spawn(async move {
            port.append(&run(), &agent_status_event("turn_timeout"))
                .await
        })
    };
    let block = wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 withheld fixture trips the barrier",
    );
    // Invoke the witnessed append while the barrier is held, then deliberately
    // never poll its future.
    let withheld = port.append(&run(), &agent_status_event("input_accepted"));
    // The receipt is written from the entry tick, not from the caller's first
    // poll, so its presence proves both that this append took the witnessed
    // branch and that the settle does not depend on the withheld future.
    wait_receipt_pair(
        &control,
        REGISTRY_INPUT_ACCEPTED,
        "queued",
        "#1370 withheld fixture owes the unpolled append a receipt",
    );
    release(&control, seq_of(&block));
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "release",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 withheld fixture releases the barrier",
    );
    // The witnessed job holds the FIFO slot ahead of this append, and its settle
    // is already recorded, so this append completes within the observation cap.
    // Were the settle taken from the caller's first poll, the witnessed job
    // would sit on the worker for `WITNESS_SETTLE` and this one would starve.
    let later = port.append(&run(), &agent_status_event("cancelled"));
    let later = tokio::time::timeout(OBSERVE, later).await;
    // Receipt-bound, not a plain `expect`: the negative control that keeps the
    // settle sender alive without sending must be killed *at this assertion*,
    // which only a `negative_assert!` can record (#1648 fix-r2).
    negative_assert!(
        later.is_ok(),
        "#1370 a withheld witnessed future does not park the registry worker"
    );
    later
        .expect("#1370 the append behind a withheld witnessed future finished")
        .expect("#1370 the append behind a withheld witnessed future persists");
    // Entered after the release cleared the hold, so it owes no receipt.
    assert_no_receipt_pair(&control, REGISTRY_CANCELLED, "queued");
    tokio::time::timeout(OBSERVE, blocked)
        .await
        .expect("#1370 withheld blocked append finishes within the observation cap")
        .expect("#1370 withheld blocked append did not panic")
        .expect("#1370 withheld blocked append persists");
    let events = port.events(&run()).await.unwrap();
    let names: Vec<String> = events
        .iter()
        .map(|event| {
            field(event, "event")
                .as_string()
                .expect("#1370 withheld event name")
                .to_utf8()
                .expect("#1370 withheld event name is UTF-8")
        })
        .collect();
    negative_assert_eq!(
        names,
        vec![
            "turn_timeout".to_string(),
            "input_accepted".to_string(),
            "cancelled".to_string()
        ],
        "#1370 a withheld witnessed future keeps its FIFO slot"
    );
    drop(withheld);
}

// Matrix 13: two site barriers held at once release by their own block receipt
// sequence numbers and never release each other.
#[test]
fn two_site_barriers_release_by_sequence() {
    let scratch = Scratch::new();
    let control = scratch.control();
    let stimulus = armed(&control);
    let registry = Arc::new(
        FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(stimulus.clone()))
            .unwrap(),
    );
    registry.accept(&accept_input()).unwrap();
    arm(
        &control,
        &config_text(&[
            barrier_fragment(
                WRITER_SESSION_METADATA.0,
                "w1",
                WRITER_SESSION_METADATA.1,
                HELD_BOUND_MS,
            ),
            barrier_fragment(
                REGISTRY_TURN_TIMEOUT.0,
                "r1",
                REGISTRY_TURN_TIMEOUT.1,
                HELD_BOUND_MS,
            ),
        ]),
    );
    let _writer_release_guard = BarrierRelease::new(
        &control,
        WRITER_SESSION_METADATA.0,
        WRITER_SESSION_METADATA.1,
    );
    let _registry_release_guard =
        BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    let (writer, path) = writer_fixture(&scratch.0, Some(stimulus));
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
        WRITER_SESSION_METADATA.0,
        "block",
        WRITER_SESSION_METADATA.1,
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
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1370 two-site registry block receipt",
    );
    // Releasing the registry sequence frees only the registry barrier.
    release(&control, seq_of(&registry_block));
    registry_blocked.join().unwrap();
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "complete",
        REGISTRY_TURN_TIMEOUT.1,
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
        WRITER_SESSION_METADATA.0,
        "complete",
        WRITER_SESSION_METADATA.1,
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
    let scratch = Scratch::new();
    let control = scratch.control();
    // Malformed JSON: both sites fail closed with a stimulus-marked error.
    arm(&control, "{ not json");
    {
        let stimulus = armed(&control);
        let (writer, _path) = writer_fixture(&scratch.0, Some(stimulus.clone()));
        let registry =
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(stimulus))
                .unwrap();
        registry.accept(&accept_input()).unwrap();
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
        let stimulus = armed(&scratch.0.join("missing-control"));
        let (writer, _path) = writer_fixture(&scratch.0, Some(stimulus.clone()));
        let registry =
            FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(stimulus))
                .unwrap();
        registry.accept(&accept_input()).unwrap();
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

// #1805 item 1: a stalled barrier — the block receipt is on disk but the
// worker died inside `wait_for_release` (release.json unreadable), so no
// release or complete receipt can ever land. The guard itself must still end
// within its budget: only receipts that actually advance may extend it, never
// the release-writing branch's own retries.
#[test]
fn stalled_barrier_guard_still_ends_within_its_budget() {
    let scratch = Scratch::new();
    let control = scratch.control();
    arm(
        &control,
        &config_text(&[barrier_fragment(
            WRITER_TURN_TIMEOUT.0,
            "s1",
            WRITER_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    // Sabotage the release path: release.json as a directory makes the
    // worker's read fail (so its append errors out instead of blocking) and
    // the guard's own write fail (so no release or complete ever lands).
    fs::create_dir(control.join("release.json")).expect("#1805 sabotage the release path");
    let (writer, path) = writer_fixture(&scratch.0, Some(armed(&control)));
    let blocked = thread::spawn(move || {
        writer
            .record_agent_status_message(&"s1".into(), &"t1".into(), &status("turn_timeout"))
            .expect_err("#1805 the stalled write fails instead of hanging")
    });
    wait_receipt(
        &control,
        WRITER_TURN_TIMEOUT.0,
        "block",
        WRITER_TURN_TIMEOUT.1,
        "#1805 the stalled barrier writes its block receipt",
    );
    blocked
        .join()
        .expect("#1805 the stalled worker thread ends");
    negative_assert!(
        !text(&path).contains("turn_timeout"),
        "#1805 the stalled write lands no bytes"
    );
    // Drop the guard on its own thread and sample it: the fixed guard ends
    // after its 2 s budget, while the budget-refresh regression spins forever
    // — which must surface as this named assertion, never a hung binary.
    let guard = BarrierRelease::new(&control, WRITER_TURN_TIMEOUT.0, WRITER_TURN_TIMEOUT.1);
    // Sample the clock before the spawn, not after: the guard arms its own
    // deadline from its own `Instant::now()` inside `drop`, and only a start
    // sampled no later than that makes the lower bound below airtight.
    let started = Instant::now();
    let dropping = thread::spawn(move || drop(guard));
    // Bounded poll matched to the claim, not one fixed sample: the assertion
    // under test says the guard ends *within its budget*, while the old
    // "sleep 5 s, then check once" let any regression that stretched the
    // budget pass green. Measure when the thread actually ends and bound it
    // by GUARD_BUDGET from both sides — nothing but the deadline can end a
    // stalled guard, so a correct one cannot finish early — plus
    // GUARD_STALL_ALLOWANCE for thread startup and poll granularity; the
    // 5 s loop cap below is only the backstop so an infinite spin still
    // lands on the named assertion instead of hanging the binary.
    let mut ended = None;
    while started.elapsed() < Duration::from_secs(5) {
        if dropping.is_finished() {
            ended = Some(started.elapsed());
            break;
        }
        thread::sleep(POLL);
    }
    negative_assert!(
        ended.is_some_and(|elapsed| {
            GUARD_BUDGET <= elapsed && elapsed <= GUARD_BUDGET + GUARD_STALL_ALLOWANCE
        }),
        "#1805 the stalled release guard ends within its budget"
    );
    dropping
        .join()
        .expect("#1805 the stalled release guard thread ends");
}

// #1805 item 2: the release guard pairs its receipts with the *current*
// barrier. A key that already completed a full round leaves block, release,
// and complete receipts on disk; an unpaired guard reads that history as
// "already released" and skips the release the new barrier is waiting on.
// Pairing by sequence — only receipts newer than the current block receipt
// count — keeps the two rounds apart.
#[test]
fn release_guard_pairs_receipts_with_the_current_barrier() {
    let scratch = Scratch::new();
    let control = scratch.control();
    let registry = Arc::new(
        FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(armed(&control)))
            .expect("#1805 registry fixture opens"),
    );
    registry
        .accept(&accept_input())
        .expect("#1805 registry fixture accepts the run");
    // Round one: a complete block -> release -> complete cycle on the shared
    // key, so round two starts with history an unpaired guard would mistake
    // for its own state.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "p1",
            REGISTRY_TURN_TIMEOUT.1,
            HELD_BOUND_MS,
        )]),
    );
    let first = {
        let registry = registry.clone();
        thread::spawn(move || {
            registry
                .append(&run(), &agent_status_event("turn_timeout"))
                .expect("#1805 first round append completes")
        })
    };
    let first_block = wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        "#1805 first round blocks the append",
    );
    release(&control, seq_of(&first_block));
    first.join().expect("#1805 first round append thread ends");
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "release",
        REGISTRY_TURN_TIMEOUT.1,
        "#1805 first round release receipt",
    );
    wait_receipt(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "complete",
        REGISTRY_TURN_TIMEOUT.1,
        "#1805 first round complete receipt",
    );
    // Round two: the same key under a fresh single-shot id, with a bound that
    // outlives the observation cap, so a guard that skips the release cannot
    // hide behind a bound expiry inside the cap.
    arm(
        &control,
        &config_text(&[barrier_fragment(
            REGISTRY_TURN_TIMEOUT.0,
            "p2",
            REGISTRY_TURN_TIMEOUT.1,
            OUTLIVED_BOUND_MS,
        )]),
    );
    let guard = BarrierRelease::new(&control, REGISTRY_TURN_TIMEOUT.0, REGISTRY_TURN_TIMEOUT.1);
    // Failure-path note (same B3 shape `registry_barrier_holds_chain_lock`
    // records): if an assertion fails once `second` has tripped this barrier,
    // the blocked thread stays on the chain lock until its 30 s bound expires,
    // `Scratch` drops first and removes the tree, and the worker then
    // re-creates `/tmp/pdrs-stimulus-*` outliving the test process. Accepted
    // rather than worked around: the bound must outlive the observation cap,
    // or a guard that skips the release could hide behind a bound expiry
    // inside the cap — the false PASS this test exists to exclude.
    let second = {
        let registry = registry.clone();
        thread::spawn(move || {
            registry
                .append(&run(), &agent_status_event("turn_timeout"))
                .expect("#1805 second round append completes")
        })
    };
    let second_block = wait_receipt_after(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "block",
        REGISTRY_TURN_TIMEOUT.1,
        seq_of(&first_block),
        "#1805 second round blocks the append",
    );
    drop(guard);
    let second_release = wait_receipt_after(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "release",
        REGISTRY_TURN_TIMEOUT.1,
        seq_of(&second_block),
        "#1805 the release guard releases the current barrier",
    );
    second
        .join()
        .expect("#1805 second round append thread ends");
    negative_assert_eq!(
        field(&second_release, "bounded"),
        &JsValue::Undefined,
        "#1805 the guard's release is an explicit release, not a bound expiry"
    );
    wait_receipt_after(
        &control,
        REGISTRY_TURN_TIMEOUT.0,
        "complete",
        REGISTRY_TURN_TIMEOUT.1,
        seq_of(&second_block),
        "#1805 second round complete receipt",
    );
}
