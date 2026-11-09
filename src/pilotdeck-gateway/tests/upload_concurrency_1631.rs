//! #1631 regression — interleaved `complete`/`cancel` on one upload id.
//!
//! `complete` and `cancel` judge the record re-read inside `mutate`'s per-id
//! gate. A barrier-aligned pair racing on the same store instance can
//! therefore no longer both report success, and the cancel path can no
//! longer delete the files behind a completion that was already reported.
//!
//! #2276: the rendezvous is the store's pre-mutate hook, fired between each
//! caller's outside-`get()` snapshot and the gate, so "both callers passed
//! the snapshot, neither entered the gate" is a scheduled fact instead of a
//! scheduler coincidence. The rendezvous is deadline-bounded (fix-r1): a
//! racer that never arrives turns the wait into a panic, not a hang.
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::upload::{UploadError, UploadStore, UploadStoreOptions};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const ROUNDS: usize = 64;
const WALL_CAP: Duration = Duration::from_secs(120);

struct Fixture {
    store: Arc<UploadStore>,
    id: String,
    task_dir: PathBuf,
    file: PathBuf,
}

/// Fresh project + store per round, one active upload with bytes on disk.
fn setup(project: &Path) -> Fixture {
    let manifest =
        parse_json(r#"[{"clientFileId":"f1","name":"a.txt","relativePath":"a.txt","size":8}]"#)
            .unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let resolved = project.to_path_buf();
    let listed = project.to_path_buf();
    let mut opts = UploadStoreOptions::new(
        move |_: &JsString| Ok(resolved.clone()),
        move || Ok(vec![listed.clone()]),
    );
    let tick = counter.clone();
    opts.uuid = Some(Arc::new(move || {
        format!("upload-{}", tick.fetch_add(1, Ordering::SeqCst) + 1)
    }));
    let store = Arc::new(UploadStore::new(opts));
    let id = "upload-1".to_string();

    store.create("p", &manifest, None).unwrap();
    store.write_part(&id, "f1", b"payload!".as_slice()).unwrap();
    let task_dir = project.join(".tmp/chat-uploads").join(&id);
    let file = task_dir.join("files/f1");
    assert!(file.exists(), "precondition — uploaded bytes in place");
    Fixture {
        store,
        id,
        task_dir,
        file,
    }
}

fn status_of(record: &JsValue) -> String {
    String::from_utf16_lossy(
        record
            .get("status")
            .and_then(JsValue::as_string)
            .map(|s| s.units().to_vec())
            .unwrap_or_default()
            .as_slice(),
    )
}

fn disk_status(task_dir: &Path) -> String {
    fs::read_to_string(task_dir.join("metadata.json"))
        .ok()
        .and_then(|raw| parse_json(&raw).ok())
        .map(|record| status_of(&record))
        .unwrap_or_else(|| "<no metadata.json>".to_string())
}

fn outcome(result: &Result<JsValue, UploadError>) -> String {
    match result {
        Ok(record) => format!("Ok(status={})", status_of(record)),
        Err(e) => format!("Err({}, {})", e.code, e.message_string()),
    }
}

enum Op {
    Complete,
    Cancel,
}

/// #2276 review fix-r1: the rendezvous is bounded. The pre-fix shape used a
/// plain `Barrier`, whose `wait()` never times out — a racer that returned
/// or panicked before reaching the pre-mutate hook (its outside `get()`
/// failed, say) left its partner blocked forever, turning a would-be red
/// test into a hung test binary. The deadline below makes "the other racer
/// never came" a panic instead. The cap is injectable (`Rendezvous::with_cap`)
/// so the deadline arm itself has a cheap test (fix-r2, DS r2 boundary#1)
/// instead of being a branch no suite ever executes.
const RENDEZVOUS_CAP: Duration = Duration::from_secs(60);

/// The #2276 rendezvous, with the hard deadline from `RENDEZVOUS_CAP`.
struct Rendezvous {
    arrived: Mutex<usize>,
    both: Condvar,
    cap: Duration,
}

impl Rendezvous {
    fn with_cap(cap: Duration) -> Arc<Self> {
        Arc::new(Self {
            arrived: Mutex::new(0),
            both: Condvar::new(),
            cap,
        })
    }

    fn new() -> Arc<Self> {
        Self::with_cap(RENDEZVOUS_CAP)
    }

    /// Wait for the second party but never past the rendezvous cap.
    fn meet(&self) {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        self.both.notify_all();
        let deadline = Instant::now() + self.cap;
        while *arrived < 2 {
            let (guard, timeout) = self
                .both
                .wait_timeout(arrived, deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            arrived = guard;
            if timeout.timed_out() && *arrived < 2 {
                // Read the count and drop the guard before panicking, so the
                // mutex is not poisoned and the diagnostic survives (the
                // partner's join would otherwise only see a PoisonError).
                let count = *arrived;
                let cap = self.cap;
                drop(arrived);
                panic!(
                    "the racing pair never met: only {count} of 2 callers \
                     reached the rendezvous within {cap:?}"
                );
            }
        }
    }
}

fn race_pair(
    store: &Arc<UploadStore>,
    id: &str,
    first: Op,
    second: Op,
) -> (Result<JsValue, UploadError>, Result<JsValue, UploadError>) {
    // #2276: both callers rendezvous at the pre-mutate hook — after their
    // outside-`get()` snapshots, before either touches the gate — so the
    // interleaving window is deterministic, not a scheduler coincidence.
    let rendezvous = Rendezvous::new();
    let gate = rendezvous.clone();
    store.set_pre_mutate_hook(Some(Arc::new(move || {
        gate.meet();
    })));
    let spawn = |op: Op, store: Arc<UploadStore>, id: String| {
        thread::spawn(move || match op {
            Op::Complete => store.complete(&id),
            Op::Cancel => store.cancel(&id),
        })
    };
    let t_first = spawn(first, store.clone(), id.to_string());
    let t_second = spawn(second, store.clone(), id.to_string());
    let results = (
        t_first.join().expect("first thread panicked"),
        t_second.join().expect("second thread panicked"),
    );
    // Clear the rendezvous: a later sequential call on the same store must
    // not block on a rendezvous nobody else is approaching.
    store.set_pre_mutate_hook(None);
    results
}

fn assert_exactly_one_ok(
    round: usize,
    a: &Result<JsValue, UploadError>,
    b: &Result<JsValue, UploadError>,
) {
    let ok_count = usize::from(a.is_ok()) + usize::from(b.is_ok());
    assert_eq!(
        ok_count,
        1,
        "round {round}: both calls must not be able to report success — first={} second={}",
        outcome(a),
        outcome(b),
    );
}

/// #2276: the wall-clock cap may cut the loop short on a slow host, but it
/// must never allow zero rounds — a pair that was never raced is no evidence
/// at all, so an empty run is a red test, not a green one.
fn assert_rounds_floor(ran: usize) {
    assert!(
        ran >= 1,
        "WALL_CAP allowed zero rounds — the pair was never raced",
    );
}

#[test]
fn interleaved_complete_and_cancel_cannot_both_win() {
    let scratch = scratch::Scratch::new("1631-cc");
    let start = Instant::now();
    let mut ran = 0usize;
    for round in 1..=ROUNDS {
        if start.elapsed() > WALL_CAP {
            break;
        }
        let project = scratch.path().join(format!("r{round:03}"));
        fs::create_dir_all(&project).unwrap();
        let f = setup(&project);
        let (complete_res, cancel_res) = race_pair(&f.store, &f.id, Op::Complete, Op::Cancel);
        let disk = disk_status(&f.task_dir);
        let file_exists = f.file.exists();
        assert_exactly_one_ok(round, &complete_res, &cancel_res);
        ran += 1;

        match (&complete_res, &cancel_res) {
            (Ok(record), Err(e)) => {
                assert_eq!(status_of(record), "completed", "round {round}");
                assert_eq!(disk, "completed", "round {round}: disk status");
                assert!(
                    file_exists,
                    "round {round}: completed upload's bytes were deleted"
                );
                assert!(
                    f.task_dir.join("files").exists(),
                    "round {round}: files dir of a completed upload was removed"
                );
                assert_eq!(
                    e.code, "UPLOAD_ALREADY_COMPLETED",
                    "round {round}: losing cancel must refuse"
                );
            }
            (Err(e), Ok(record)) => {
                assert_eq!(status_of(record), "cancelled", "round {round}");
                assert_eq!(disk, "cancelled", "round {round}: disk status");
                assert!(
                    !file_exists && !f.task_dir.join("files").exists(),
                    "round {round}: cancelled upload's bytes must be gone"
                );
                assert_eq!(
                    e.code, "UPLOAD_INVALID_STATE",
                    "round {round}: losing complete must refuse"
                );
                assert_eq!(e.message_string(), "Upload is cancelled.");
            }
            _ => unreachable!("exactly-one-Ok already asserted"),
        }
    }
    assert_rounds_floor(ran);
}

#[test]
fn interleaved_complete_and_complete_has_one_winner() {
    let scratch = scratch::Scratch::new("1631-cm");
    let start = Instant::now();
    let mut ran = 0usize;
    for round in 1..=ROUNDS {
        if start.elapsed() > WALL_CAP {
            break;
        }
        let project = scratch.path().join(format!("r{round:03}"));
        fs::create_dir_all(&project).unwrap();
        let f = setup(&project);
        let (first, second) = race_pair(&f.store, &f.id, Op::Complete, Op::Complete);
        let disk = disk_status(&f.task_dir);
        assert_exactly_one_ok(round, &first, &second);
        ran += 1;

        match (&first, &second) {
            (Ok(record), Err(e)) | (Err(e), Ok(record)) => {
                assert_eq!(status_of(record), "completed", "round {round}");
                assert_eq!(disk, "completed", "round {round}: disk status");
                assert!(
                    f.file.exists(),
                    "round {round}: completed upload's bytes were deleted"
                );
                assert_eq!(
                    e.code, "UPLOAD_INVALID_STATE",
                    "round {round}: losing complete must refuse"
                );
                assert_eq!(e.message_string(), "Upload is completed.");
            }
            _ => unreachable!("exactly-one-Ok already asserted"),
        }
    }
    assert_rounds_floor(ran);
}

/// #2275/#2276 on the integration face: a cancel×cancel pair both report
/// `Ok`, but exactly one transition happens — one event, one metadata write —
/// and the second winner is a settled no-op whose bytes on disk never move.
/// The losing double-delete stays quiet (`remove_tree` treats a vanished
/// directory as success), so the event stream and the stable bytes are the
/// observable half of "no double delete".
#[test]
fn interleaved_cancel_and_cancel_wins_exactly_once() {
    let scratch = scratch::Scratch::new("1631-cx");
    let start = Instant::now();
    let mut ran = 0usize;
    for round in 1..=ROUNDS {
        if start.elapsed() > WALL_CAP {
            break;
        }
        let project = scratch.path().join(format!("r{round:03}"));
        fs::create_dir_all(&project).unwrap();
        let f = setup(&project);
        let sub = f.store.subscribe(&f.id);
        let (first, second) = race_pair(&f.store, &f.id, Op::Cancel, Op::Cancel);
        ran += 1;
        let first = first.expect("a cancel of an active upload is Ok");
        let second = second.expect("a settled cancel is a no-op Ok, not a refusal");
        assert_eq!(status_of(&first), "cancelled", "round {round}");
        assert_eq!(status_of(&second), "cancelled", "round {round}");
        assert_eq!(
            disk_status(&f.task_dir),
            "cancelled",
            "round {round}: disk status"
        );
        assert!(
            !f.file.exists() && !f.task_dir.join("files").exists(),
            "round {round}: cancelled upload's bytes must be gone"
        );
        // Exactly one event: the winning transition emits; the settled
        // no-op must not.
        assert_eq!(
            status_of(
                &sub.receiver
                    .recv_timeout(Duration::from_secs(30))
                    .expect("round {round}: the winning cancel emits exactly one event")
            ),
            "cancelled",
            "round {round}"
        );
        assert!(
            sub.receiver.try_recv().is_err(),
            "round {round}: the settled no-op must not emit a second event"
        );
        // The sequential third cancel is bytes-stable: same metadata bytes,
        // no further event.
        let metadata = f.task_dir.join("metadata.json");
        let before = fs::read(&metadata).unwrap();
        let third = f.store.cancel(&f.id).expect("a settled cancel stays Ok");
        assert_eq!(status_of(&third), "cancelled", "round {round}");
        assert_eq!(
            fs::read(&metadata).unwrap(),
            before,
            "round {round}: a no-op cancel must not rewrite the metadata"
        );
        assert!(
            sub.receiver.try_recv().is_err(),
            "round {round}: a no-op cancel must not emit"
        );
    }
    assert_rounds_floor(ran);
}

/// fix-r2 (DS r2 boundary#1): the deadline arm of the rendezvous is itself
/// under test — a lone caller with a tiny cap must fail fast with the
/// arrival-count diagnostic, not hang the binary until some external
/// timeout. The three interleaved tests above always deliver both callers,
/// so without this case the panic branch the fix-r1 guard added would be
/// dead code in every suite.
#[test]
fn rendezvous_deadline_panics_when_the_pair_never_meets() {
    let rendezvous = Rendezvous::with_cap(Duration::from_millis(50));
    let lone = rendezvous.clone();
    let handle = thread::spawn(move || lone.meet());
    let err = handle
        .join()
        .expect_err("a lone caller must hit the rendezvous deadline");
    let msg = err
        .downcast_ref::<String>()
        .expect("panic payload is a formatted String");
    assert!(
        msg.contains("only 1 of 2 callers"),
        "the diagnostic must report the arrival count, got: {msg}"
    );
    // The panicking caller drops its guard before panicking, so the mutex
    // is not poisoned: a second `meet()` on the SAME rendezvous locks
    // cleanly — it takes the lock, pushes the arrival count to 2 and
    // returns without waiting (a poisoned lock would surface as a
    // PoisonError panic here instead).
    let paired = rendezvous;
    let second = thread::spawn(move || paired.meet());
    second
        .join()
        .expect("the uncontended lock after the deadline panic must not be poisoned");
}
