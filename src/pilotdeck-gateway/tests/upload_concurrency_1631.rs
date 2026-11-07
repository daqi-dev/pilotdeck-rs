//! #1631 regression — interleaved `complete`/`cancel` on one upload id.
//!
//! `complete` and `cancel` judge the record re-read inside `mutate`'s per-id
//! gate. A barrier-aligned pair racing on the same store instance can
//! therefore no longer both report success, and the cancel path can no
//! longer delete the files behind a completion that was already reported.
use pilotdeck_gateway::upload::{UploadError, UploadStore, UploadStoreOptions};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const ROUNDS: usize = 64;
const WALL_CAP: Duration = Duration::from_secs(120);

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = Path::new("/tmp").join(format!(
            "pdrs-1631-{}-{}-{}",
            tag,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).expect("scratch dir");
        Self(fs::canonicalize(&dir).unwrap())
    }
}

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

fn race_pair(
    store: &Arc<UploadStore>,
    id: &str,
    first: Op,
    second: Op,
) -> (Result<JsValue, UploadError>, Result<JsValue, UploadError>) {
    let barrier = Arc::new(Barrier::new(2));
    let spawn = |op: Op, store: Arc<UploadStore>, id: String, gate: Arc<Barrier>| {
        thread::spawn(move || {
            gate.wait();
            match op {
                Op::Complete => store.complete(&id),
                Op::Cancel => store.cancel(&id),
            }
        })
    };
    let t_first = spawn(first, store.clone(), id.to_string(), barrier.clone());
    let t_second = spawn(second, store.clone(), id.to_string(), barrier.clone());
    (
        t_first.join().expect("first thread panicked"),
        t_second.join().expect("second thread panicked"),
    )
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

#[test]
fn interleaved_complete_and_cancel_cannot_both_win() {
    let scratch = Scratch::new("cc");
    let start = Instant::now();
    for round in 1..=ROUNDS {
        if start.elapsed() > WALL_CAP {
            break;
        }
        let project = scratch.0.join(format!("r{round:03}"));
        fs::create_dir_all(&project).unwrap();
        let f = setup(&project);
        let (complete_res, cancel_res) = race_pair(&f.store, &f.id, Op::Complete, Op::Cancel);
        let disk = disk_status(&f.task_dir);
        let file_exists = f.file.exists();
        assert_exactly_one_ok(round, &complete_res, &cancel_res);

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
}

#[test]
fn interleaved_complete_and_complete_has_one_winner() {
    let scratch = Scratch::new("cm");
    let start = Instant::now();
    for round in 1..=ROUNDS {
        if start.elapsed() > WALL_CAP {
            break;
        }
        let project = scratch.0.join(format!("r{round:03}"));
        fs::create_dir_all(&project).unwrap();
        let f = setup(&project);
        let (first, second) = race_pair(&f.store, &f.id, Op::Complete, Op::Complete);
        let disk = disk_status(&f.task_dir);
        assert_exactly_one_ok(round, &first, &second);

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
}
