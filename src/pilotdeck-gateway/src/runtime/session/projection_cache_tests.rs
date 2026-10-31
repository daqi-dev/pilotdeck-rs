use super::*;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{
    sync::{Condvar, mpsc},
    time::Duration,
};

fn js(text: &str) -> JsValue {
    parse_json(text).expect("#1011 valid test JSON")
}
fn oracle() -> JsValue {
    static ORACLE: OnceLock<JsValue> = OnceLock::new();
    ORACLE
        .get_or_init(|| {
            crate::test_baseline::oracle(include_str!(
                "../../../tests/fixtures/session-factory/projection-cache/oracle.json"
            ))
        })
        .clone()
}
fn case(name: &str) -> JsValue {
    let oracle = oracle();
    ["cases", "supplemental"]
        .iter()
        .flat_map(|key| array(field(&oracle, key)).expect("#1011 oracle case array"))
        .find(|row| is(field(row, "name"), name))
        .expect("#1011 oracle case exists")
        .clone()
}
fn transcript(snapshot: &JsValue) -> Vec<JsValue> {
    let text = field(field(snapshot, "transcript"), "utf8")
        .as_string()
        .expect("#1011 transcript bytes");
    String::from_utf16_lossy(text.units())
        .lines()
        .map(js)
        .collect()
}
fn cache_bytes(snapshot: &JsValue) -> Option<String> {
    field(field(snapshot, "cache"), "utf8")
        .as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
}
struct Gate {
    entered: mpsc::SyncSender<()>,
    released: Mutex<bool>,
    wake: Condvar,
}
impl Gate {
    fn new() -> (Arc<Self>, mpsc::Receiver<()>) {
        let (entered, receipt) = mpsc::sync_channel(1);
        (
            Arc::new(Self {
                entered,
                released: Mutex::new(false),
                wake: Condvar::new(),
            }),
            receipt,
        )
    }
    fn wait(&self) {
        self.entered.send(()).expect("#1011 gate entered receiver");
        let (_released, timeout) = self
            .wake
            .wait_timeout_while(lock(&self.released), Duration::from_secs(10), |ready| {
                !*ready
            })
            .expect("#1011 gate condition");
        assert!(!timeout.timed_out(), "#1011 gate release within budget");
    }
    fn open(&self) {
        *lock(&self.released) = true;
        self.wake.notify_all();
    }
}
#[derive(Default)]
struct Log {
    entries: Mutex<Vec<JsValue>>,
    append_gate: Mutex<Option<Arc<Gate>>>,
    read_gate: Mutex<Option<Arc<Gate>>>,
    fail_append: AtomicBool,
    fail_after_commit: AtomicBool,
    after_read: Mutex<Option<Box<dyn Fn() + Send>>>,
}
impl TranscriptStore for Log {
    fn append(&self, _: &TranscriptKey, entry: &JsValue) -> crate::storage::StoreResult<()> {
        let gate = lock(&self.append_gate).take();
        if let Some(gate) = gate {
            gate.wait();
        }
        if self.fail_append.swap(false, Ordering::SeqCst) {
            return Err(crate::storage::LedgerError::Invalid("APPEND_1011".into()));
        }
        lock(&self.entries).push(entry.clone());
        if self.fail_after_commit.swap(false, Ordering::SeqCst) {
            return Err(crate::storage::LedgerError::Invalid(
                "AFTER_COMMIT_1011".into(),
            ));
        }
        Ok(())
    }
    fn read(
        &self,
        _: &TranscriptKey,
        _: Option<u64>,
    ) -> crate::storage::StoreResult<TranscriptRead> {
        let gate = lock(&self.read_gate).take();
        if let Some(gate) = gate {
            gate.wait();
        }
        let entries = lock(&self.entries).clone();
        if let Some(hook) = &*lock(&self.after_read) {
            hook();
        }
        Ok(TranscriptRead {
            entries,
            diagnostics: vec![],
        })
    }
}
#[derive(Default)]
struct Cache {
    bytes: Mutex<Option<String>>,
    load_gate: Mutex<Option<Arc<Gate>>>,
    save_gate: Mutex<Option<Arc<Gate>>>,
    saves: Mutex<usize>,
    fail_save: AtomicBool,
}
impl ProjectionCheckpointStore for Cache {
    fn load(&self) -> Option<JsValue> {
        let gate = lock(&self.load_gate).take();
        if let Some(gate) = gate {
            gate.wait();
        }
        lock(&self.bytes).as_ref().and_then(|s| parse_json(s).ok())
    }
    fn save(&self, envelope: &JsValue) -> RuntimeResult {
        let gate = lock(&self.save_gate).take();
        if let Some(gate) = gate {
            gate.wait();
        }
        if self.fail_save.load(Ordering::SeqCst) {
            return Err(RuntimeError::new("SAVE_1011"));
        }
        *lock(&self.saves) += 1;
        *lock(&self.bytes) = Some(
            stringify_json(envelope)
                .expect("#1011 cache JSON")
                .expect("#1011 defined cache")
                + "\n",
        );
        Ok(())
    }
}
struct Harness {
    storage: Arc<FactoryStorage>,
    log: Arc<Log>,
    cache: Arc<Cache>,
}
impl Harness {
    fn new(snapshot: &JsValue) -> Self {
        let log = Arc::new(Log::default());
        *lock(&log.entries) = transcript(snapshot);
        let cache = Arc::new(Cache::default());
        *lock(&cache.bytes) = cache_bytes(snapshot);
        let storage = Self::storage(log.clone(), cache.clone());
        Self {
            storage,
            log,
            cache,
        }
    }
    fn storage(log: Arc<Log>, cache: Arc<Cache>) -> Arc<FactoryStorage> {
        let storage = Arc::new(FactoryStorage::with_checkpoint_store(
            log,
            TranscriptKey {
                project_root: PathBuf::from("/unused-1011"),
                pilot_home: PathBuf::from("/unused-1011"),
                session_id: "session-1011".into(),
                transcript_path: PathBuf::from("/unused-1011/log.jsonl"),
            },
            cache,
        ));
        let sequence = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        *lock(&storage.recorder.writer().test_sources) = Some((
            Arc::new(|| Ok("2026-10-31T00:00:00.000Z".into())),
            Arc::new(move || {
                Ok(format!(
                    "storage-1-entry-{}",
                    sequence.fetch_add(1, Ordering::SeqCst) + 1
                ))
            }),
        ));
        storage
    }
}
fn append(storage: &FactoryStorage, title: &str, turn: &str) {
    let mut metadata = JsObject::new();
    metadata.insert("title".into(), JsValue::String(title.into()));
    metadata.insert("titleSource".into(), JsValue::String("user".into()));
    storage
        .recorder
        .writer()
        .record_session_metadata(
            &"session-1011".into(),
            &turn.into(),
            JsValue::Object(metadata),
        )
        .expect("#1011 append succeeds");
}
fn expected_state(snapshot: &JsValue, storage: usize) -> DurableSessionState {
    let storages = array(field(snapshot, "storages")).expect("#1011 snapshot storages");
    let values = field(field(&storages[storage], "projection"), "values");
    DurableSessionState {
        messages: array(field(
            field(values, "agent-transcript.conversation"),
            "messages",
        ))
        .expect("#1011 conversation messages")
        .to_vec(),
        usage: spread(field(
            field(values, "agent-transcript.turn-summary"),
            "usage",
        )),
        permission_denials: array(field(
            field(values, "agent-transcript.turn-summary"),
            "permissionDenials",
        ))
        .expect("#1011 permission denials")
        .to_vec(),
        metadata: spread(field(values, "agent-transcript.metadata")),
    }
}
fn assert_snapshot(h: &Harness, expected: &JsValue, index: usize) {
    let storages = array(field(expected, "storages")).expect("#1011 observed storages");
    let projection = field(&storages[index], "projection");
    if !field(projection, "error").is_undefined() {
        assert!(
            h.storage.projection.snapshot().is_err(),
            "#1011 disposed driver rejects snapshots"
        );
    } else {
        assert_eq!(
            h.storage
                .projection
                .snapshot()
                .expect("#1011 readable projection"),
            expected_state(expected, index),
            "#1011 projection equals TS durable state"
        );
    }
    assert_eq!(
        *lock(&h.log.entries),
        transcript(expected),
        "#1011 durable log entries equal TS including parent chain"
    );
    let writer = h.storage.recorder.writer().snapshot_state();
    assert_eq!(
        writer.sequence as f64,
        field(field(&storages[index], "writer"), "sequence")
            .as_number()
            .expect("#1011 oracle sequence"),
        "#1011 writer sequence equals TS"
    );
    match (
        h.cache.load(),
        cache_bytes(expected).and_then(|s| parse_json(&s).ok()),
    ) {
        (Some(actual), Some(expected)) => {
            assert_eq!(
                field(&actual, "asOfSequence"),
                field(&expected, "asOfSequence"),
                "#1011 checkpoint watermark equals TS"
            );
            assert_eq!(
                field(&actual, "anchor"),
                field(&expected, "anchor"),
                "#1011 checkpoint anchor equals TS"
            );
            // Rust registers exactly these two rows (see envelope()); the
            // other 11 rows are not faked and cold-replay from the log.
            for name in ["agent-transcript.metadata", "agent-transcript.turn-summary"] {
                assert_eq!(
                    field(field(&actual, "projections"), name),
                    field(field(&expected, "projections"), name),
                    "#1011 checkpoint row equals TS"
                );
            }
        }
        (None, None) => assert_eq!(
            *lock(&h.cache.bytes),
            cache_bytes(expected),
            "#1011 absent or corrupt cache bytes retained"
        ),
        _ => panic!("#1011 checkpoint presence equals TS"),
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("#1011 async operation bounded")
}
async fn matrix(name: &str) {
    let row = case(name);
    let snapshots = array(field(&row, "snapshots")).expect("#1011 main snapshots");
    let h = Harness::new(&snapshots[0]);
    bounded(h.storage.restore())
        .await
        .expect("#1011 initial restore");
    assert_snapshot(&h, &snapshots[0], 0);
    let (state, event) = name.split_once('-').expect("#1011 matrix name");
    let (gate, entered) = Gate::new();
    // Hold the actual persistence/cache operation, never a timer approximation.
    match event {
        "load" | "recreate" => *lock(&h.cache.load_gate) = Some(gate.clone()),
        "append" => *lock(&h.log.append_gate) = Some(gate.clone()),
        "checkpoint" | "shutdown" => *lock(&h.cache.save_gate) = Some(gate.clone()),
        "drain" => *lock(&h.log.read_gate) = Some(gate.clone()),
        _ => panic!("#1011 known event"),
    }
    let storage = h.storage.clone();
    let log = h.log.clone();
    let cache = h.cache.clone();
    let event = event.to_string();
    let action_event = event.clone();
    let recreated = Arc::new(Mutex::new(None));
    let observed_recreated = recreated.clone();
    let action = tokio::spawn(async move {
        match action_event.as_str() {
            "load" => storage.restore().await.map(|_| ()),
            "append" => tokio::task::spawn_blocking(move || append(&storage, "appended", "append"))
                .await
                .map_err(|e| RuntimeError::new(e.to_string())),
            "checkpoint" | "drain" => storage.flush().await,
            "shutdown" => storage.close_owned().await,
            "recreate" => {
                // The factory's existing writer fence closes before restoring
                // the replacement. The TS baseline closes the predecessor later.
                storage.close_owned().await?;
                let next = Harness::storage(log, cache);
                *lock(&observed_recreated) = Some(next.clone());
                next.restore().await?;
                Ok(())
            }
            _ => unreachable!("#1011 known action"),
        }
    });
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 action reached gate")
    })
    .await
    .expect("#1011 gate join");
    assert!(!action.is_finished(), "#1011 held operation is pending");
    if event != "recreate" {
        assert_eq!(
            h.storage
                .projection
                .snapshot()
                .expect("#1011 held projection"),
            expected_state(&snapshots[1], 0),
            "#1011 held projection equals TS"
        );
        assert_eq!(
            *lock(&h.cache.bytes),
            cache_bytes(&snapshots[1]),
            "#1011 held cache unchanged"
        );
    } else {
        assert!(
            h.storage.projection.snapshot().is_err(),
            "#1011 fenced predecessor disposed before replacement load"
        );
        let next = lock(&recreated)
            .clone()
            .expect("#1011 held replacement exists");
        assert_eq!(
            next.projection
                .snapshot()
                .expect("#1011 replacement held projection"),
            expected_state(&snapshots[1], 1),
            "#1011 held replacement projection equals TS"
        );
    }

    gate.open();
    bounded(action)
        .await
        .expect("#1011 action joins")
        .expect("#1011 action fulfilled");
    if event == "recreate" {
        let next = Harness {
            storage: lock(&recreated).clone().expect("#1011 actual replacement"),
            log: h.log.clone(),
            cache: h.cache.clone(),
        };
        assert_snapshot(&next, &snapshots[2], 1);
        assert!(
            h.storage.projection.snapshot().is_err(),
            "#1011 predecessor disposed"
        );
    } else {
        assert_snapshot(&h, &snapshots[2], 0);
    }
    if state == "fresh" || state == "stale" {
        // Replay and hydrate produce equal projection values, so the snapshot
        // oracle alone cannot see that the valid rows were actually consumed.
        let storage = if event == "recreate" {
            lock(&recreated)
                .clone()
                .expect("#1011 hydrated replacement exists")
        } else {
            h.storage.clone()
        };
        assert_eq!(
            *lock(&storage.cache.hydrated_rows),
            2,
            "#1011 valid cache is hydrated"
        );
    }
    assert_ne!(
        state, "concurrent",
        "#1011 concurrency has dedicated scenario"
    );
}
macro_rules! matrix_tests { ($($name:ident => $case:literal),* $(,)?) => { $(
    #[tokio::test(flavor="multi_thread", worker_threads=2)] async fn $name() { matrix($case).await; }
)* }; }
matrix_tests! {
missing_load=>"missing-load", missing_append=>"missing-append", missing_recreate=>"missing-recreate", missing_checkpoint=>"missing-checkpoint", missing_drain=>"missing-drain", missing_shutdown=>"missing-shutdown",
fresh_load=>"fresh-load", fresh_append=>"fresh-append", fresh_recreate=>"fresh-recreate", fresh_checkpoint=>"fresh-checkpoint", fresh_drain=>"fresh-drain", fresh_shutdown=>"fresh-shutdown",
stale_load=>"stale-load", stale_append=>"stale-append", stale_recreate=>"stale-recreate", stale_checkpoint=>"stale-checkpoint", stale_drain=>"stale-drain", stale_shutdown=>"stale-shutdown",
corrupt_load=>"corrupt-load", corrupt_append=>"corrupt-append", corrupt_recreate=>"corrupt-recreate", corrupt_checkpoint=>"corrupt-checkpoint", corrupt_drain=>"corrupt-drain", corrupt_shutdown=>"corrupt-shutdown",
}
async fn restore_case(name: &str) {
    let row = case(name);
    let h = Harness::new(field(&row, "before"));
    if name.starts_with("cold-") {
        h.storage.restore().await.expect("#1011 cold seed restore");
        h.storage
            .close_owned()
            .await
            .expect("#1011 cold seed dispose");
        *lock(&h.cache.bytes) = cache_bytes(field(&row, "before"));
        assert!(
            h.storage.projection.snapshot().is_err(),
            "#1011 cold predecessor disposed before replacement"
        );
        let storage = Harness::storage(h.log.clone(), h.cache.clone());
        let (gate, entered) = Gate::new();
        *lock(&h.cache.load_gate) = Some(gate.clone());
        let candidate = storage.clone();
        let load = tokio::spawn(async move { candidate.restore().await });
        tokio::task::spawn_blocking(move || {
            entered
                .recv_timeout(Duration::from_secs(10))
                .expect("#1011 cold cache gate reached")
        })
        .await
        .expect("#1011 cold gate join");
        assert!(!load.is_finished(), "#1011 cold load pending at held");
        assert_eq!(
            storage
                .projection
                .snapshot()
                .expect("#1011 cold held projection"),
            expected_state(field(&row, "held"), 1),
            "#1011 cold held equals TS"
        );
        assert_eq!(
            *lock(&h.cache.bytes),
            cache_bytes(field(&row, "held")),
            "#1011 cold load does not write cache"
        );
        gate.open();
        bounded(load)
            .await
            .expect("#1011 cold load joins")
            .expect("#1011 cold load succeeds");
        let h = Harness {
            storage,
            log: h.log,
            cache: h.cache,
        };
        assert_snapshot(&h, field(&row, "settled"), 1);
        if name == "cold-fresh" || name == "cold-stale" {
            // Replay and hydrate produce equal projection values here; only
            // this counter observes that the replacement actually read them.
            assert_eq!(
                *lock(&h.storage.cache.hydrated_rows),
                2,
                "#1011 valid cache is hydrated"
            );
        }
    } else {
        h.storage
            .restore()
            .await
            .expect("#1011 supplemental restore fulfilled");
        assert_snapshot(&h, field(&row, "settled"), 0);
        if name == "invalid-codec" {
            // Only the conversation row's codec is unsupported; conversation
            // always replays, so the metadata and turn-summary rows of the
            // same envelope still hydrate.
            assert_eq!(
                *lock(&h.storage.cache.hydrated_rows),
                2,
                "#1011 invalid codec row falls back alone"
            );
        } else {
            assert_eq!(
                *lock(&h.storage.cache.hydrated_rows),
                0,
                "#1011 invalid envelope is rejected before hydrate"
            );
        }
    }
}

macro_rules! restore_tests { ($($name:ident => $case:literal),* $(,)?) => { $(
    #[tokio::test(flavor="multi_thread", worker_threads=2)] async fn $name() { restore_case($case).await; }
)* }; }
restore_tests! { cold_missing=>"cold-missing", cold_fresh=>"cold-fresh", cold_stale=>"cold-stale", cold_corrupt=>"cold-corrupt", invalid_anchor=>"invalid-anchor", invalid_future=>"invalid-future", invalid_wrong_session=>"invalid-wrong-session", invalid_row_watermark=>"invalid-row-watermark", invalid_codec=>"invalid-codec" }

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn generator_cleans_owned_work() {
    let scratch = std::env::temp_dir().join(format!("pdrs-1011-cleanup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).expect("#1011 generator scratch");
    // Foreign work directories must survive: only the generator's own root is removable.
    std::fs::create_dir(scratch.join("work-foreign")).expect("#1011 foreign work sentinel");
    let base = std::env::var("PDRS_BASELINE").expect("#1011 pinned baseline configured");
    let mut command = std::process::Command::new("node");
    command
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(format!("{base}/node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/session-factory/projection-cache/generate.mts"),
        )
        .arg(&scratch);
    let (status, stdout, stderr) = crate::test_baseline::run_generator_with_deadline(
        &mut command,
        "session-factory/projection-cache",
        Duration::from_secs(180),
    )
    .expect("#1011 generator bounded run");
    assert!(
        status.success(),
        "#1011 generator completed: {}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let mut names: Vec<_> = std::fs::read_dir(&scratch)
        .expect("#1011 inspect generator OUT")
        .map(|entry| {
            entry
                .expect("#1011 OUT entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [".generation-owner", "oracle.json", "work-foreign"],
        "#1011 generator left files behind in OUT"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join(".generation-owner"))
            .expect("#1011 read generator owner"),
        "session-factory/projection-cache\n",
        "#1011 generator owner identifies fixture"
    );
    std::fs::remove_dir_all(scratch).expect("#1011 remove owned test scratch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_append_keeps_both_commits() {
    let row = case("concurrent-append");
    let snaps = array(field(&row, "snapshots")).expect("#1011 concurrent snapshots");
    let h = Harness::new(&snaps[0]);
    h.storage.restore().await.expect("#1011 concurrent restore");
    let (gate, entered) = Gate::new();
    *lock(&h.log.append_gate) = Some(gate.clone());
    let first_storage = h.storage.clone();
    let first =
        tokio::task::spawn_blocking(move || append(&first_storage, "concurrent-1", "concurrent-1"));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 first append entered")
    })
    .await
    .expect("#1011 entered join");
    let second_storage = h.storage.clone();
    let (started, started_rx) = mpsc::sync_channel(1);
    let second = tokio::task::spawn_blocking(move || {
        started.send(()).expect("#1011 second launch");
        append(&second_storage, "concurrent-2", "concurrent-2");
    });
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("#1011 second started");
    assert!(
        !first.is_finished() && !second.is_finished(),
        "#1011 pending predecessor blocks second commit"
    );
    assert_eq!(
        h.storage
            .projection
            .snapshot()
            .expect("#1011 concurrent held projection"),
        expected_state(&snaps[1], 0),
        "#1011 concurrent held equals TS"
    );
    assert_eq!(
        *lock(&h.cache.bytes),
        cache_bytes(&snaps[1]),
        "#1011 cache read progresses with pending writer"
    );
    gate.open();
    bounded(first).await.expect("#1011 first append joined");
    bounded(second).await.expect("#1011 second append joined");
    append(&h.storage, "appended", "append");
    assert_snapshot(&h, &snaps[2], 0);
    let entries = lock(&h.log.entries);
    assert_eq!(
        entries.len(),
        5,
        "#1011 concurrent append retains every durable record"
    );
    for pair in entries.windows(2) {
        assert_eq!(
            field(&pair[1], "parentEntryId"),
            field(&pair[0], "entryId"),
            "#1011 concurrent parent chain intact"
        );
    }
}

#[tokio::test]
async fn turn_result_schedules_and_binding_dispose_waits() {
    let row = case("turn-result");
    let h = Harness::new(field(&row, "before"));
    h.storage
        .restore()
        .await
        .expect("#1011 turn result restore");
    let committed = transcript(field(&row, "settled"));
    h.storage
        .recorder
        .writer()
        .record_entry(committed.last().expect("#1011 turn result entry"))
        .expect("#1011 turn result commit");
    let cache = h.storage.cache.clone();
    bounded(tokio::task::spawn_blocking(move || cache.dispose()))
        .await
        .expect("#1011 binding disposal joined");
    assert_snapshot(&h, field(&row, "settled"), 0);
    assert_eq!(
        *lock(&h.cache.saves),
        1,
        "#1011 binding dispose does not schedule another save"
    );
}
#[tokio::test]
async fn save_rejection_is_disposable() {
    let row = case("save-reject");
    let h = Harness::new(field(&row, "before"));
    h.storage
        .restore()
        .await
        .expect("#1011 save rejection restore");
    h.cache.fail_save.store(true, Ordering::SeqCst);
    h.storage
        .flush()
        .await
        .expect("#1011 cache save failure does not reject flush");
    assert_snapshot(&h, field(&row, "settled"), 0);
    assert_eq!(
        lock(&h.storage.cache.diagnostic).as_deref(),
        Some("SAVE_1011"),
        "#1011 save diagnostic retained"
    );
}
#[tokio::test]
async fn capture_retries_three_times() {
    let row = case("capture-race");
    let h = Harness::new(field(&row, "before"));
    h.storage
        .restore()
        .await
        .expect("#1011 capture race restore");
    let weak = Arc::downgrade(&h.storage);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = attempts.clone();
    *lock(&h.log.after_read) = Some(Box::new(move || {
        let n = observed.fetch_add(1, Ordering::SeqCst) + 1;
        append(
            &weak.upgrade().expect("#1011 capture storage lives"),
            &format!("race-{n}"),
            &format!("race-{n}"),
        );
    }));
    h.storage
        .flush()
        .await
        .expect("#1011 capture exhaustion fail soft");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "#1011 capture retry budget is exactly three"
    );
    assert_snapshot(&h, field(&row, "settled"), 0);
    assert_eq!(
        lock(&h.storage.cache.diagnostic).as_deref(),
        Some("Could not capture a consistent projection checkpoint for session-1011."),
        "#1011 capture exhaustion diagnostic equals TS"
    );
}
#[test]
fn oversized_checkpoint_is_missing() {
    let root = std::env::temp_dir().join(format!("pdrs-1011-oversized-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("#1011 oversized scratch");
    let path = root.join("log.jsonl.projections.json");
    let envelope = "{\"format\":\"pilotdeck.session-projections\",\"version\":1}";
    std::fs::write(&path, envelope).expect("#1011 bounded checkpoint written");
    assert!(
        FileProjectionCheckpointStore(path.clone())
            .load()
            .is_some_and(|value| is(field(&value, "format"), "pilotdeck.session-projections")),
        "#1011 bounded checkpoint still loads"
    );
    // Trailing whitespace keeps the file parseable, so only the size bound
    // rejects it once it passes the transcript read limit.
    let padding = crate::storage::TRANSCRIPT_MAX_BYTES + 1 - envelope.len() as u64;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("#1011 oversized reopen");
    std::io::Write::write_all(&mut file, &vec![b' '; padding as usize])
        .expect("#1011 oversized padding");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("#1011 oversized stat")
            .len(),
        crate::storage::TRANSCRIPT_MAX_BYTES + 1,
        "#1011 oversized fixture size"
    );
    assert!(
        FileProjectionCheckpointStore(path).load().is_none(),
        "#1011 oversized checkpoint is treated as missing"
    );
    std::fs::remove_dir_all(root).expect("#1011 oversized cleanup");
}

#[tokio::test]
async fn predecessor_rejection_does_not_consume_sequence() {
    let row = case("predecessor-reject");
    let h = Harness::new(field(&row, "before"));
    h.storage
        .restore()
        .await
        .expect("#1011 predecessor restore");
    h.log.fail_append.store(true, Ordering::SeqCst);
    let error = h
        .storage
        .recorder
        .writer()
        .record_session_metadata(
            &"session-1011".into(),
            &"first".into(),
            js(r#"{"title":"first","titleSource":"user"}"#),
        )
        .expect_err("#1011 failed predecessor surfaces error");
    assert!(
        error.to_string().contains("APPEND_1011"),
        "#1011 predecessor injected error retained"
    );
    append(&h.storage, "second", "second");
    h.storage.flush().await.expect("#1011 successor flush");
    assert_snapshot(&h, field(&row, "settled"), 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_waiter_keeps_admitted_append() {
    let row = case("cancel-waiter");
    let h = Harness::new(field(&row, "before"));
    h.storage.restore().await.expect("#1011 cancel restore");
    let (gate, entered) = Gate::new();
    *lock(&h.log.append_gate) = Some(gate.clone());
    let first = tokio::spawn(queued_metadata(&h.storage, "first", "first"));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 cancel append started")
    })
    .await
    .expect("#1011 cancel gate join");
    let held = field(field(&row, "details"), "held");
    assert_eq!(
        h.storage
            .projection
            .snapshot()
            .expect("#1011 pending projection readable"),
        expected_state(held, 0),
        "#1011 never completed predecessor stays uncommitted at held observation"
    );
    assert_eq!(
        *lock(&h.cache.bytes),
        cache_bytes(held),
        "#1011 pending predecessor permits cache reads"
    );
    first.abort();
    let cancelled = bounded(first).await.expect_err("#1011 waiter is cancelled");
    assert!(
        cancelled.is_cancelled(),
        "#1011 cancellation rejects only the waiting task"
    );
    assert!(
        is(
            field(field(field(&row, "details"), "waiter"), "status"),
            "rejected"
        ),
        "#1011 TS waiter rejected"
    );
    let second = tokio::spawn(queued_metadata(&h.storage, "second", "second"));
    gate.open();
    bounded(second)
        .await
        .expect("#1011 cancelled predecessor successor joins")
        .expect("#1011 cancelled predecessor successor commits");
    h.storage.flush().await.expect("#1011 cancel flush");
    assert_snapshot(&h, field(&row, "settled"), 0);
}

#[test]
fn guard_includes_library_wiring() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/session-factory/projection-cache/negative_controls.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("--inputs")
        .output()
        .expect("#1011 inventory process starts");
    assert!(
        output.status.success(),
        "#1011 inventory process succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths: Vec<String> =
        serde_json::from_slice(&output.stdout).expect("#1011 input inventory JSON");
    assert!(
        paths
            .iter()
            .any(|path| path == "src/pilotdeck-gateway/src/lib.rs"),
        "#1011 guard includes library wiring"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_restore_keeps_interleaved_commit() {
    let row = case("concurrent-load");
    let snaps = array(field(&row, "snapshots")).expect("#1011 concurrent load snapshots");
    let h = Harness::new(&snaps[0]);
    h.storage
        .restore()
        .await
        .expect("#1011 active load initial restore");
    let (gate, entered) = Gate::new();
    *lock(&h.cache.load_gate) = Some(gate.clone());
    let storage = h.storage.clone();
    let restoring = tokio::spawn(async move { storage.restore().await });
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 active load read then cache gate")
    })
    .await
    .expect("#1011 active load gate join");
    append(&h.storage, "concurrent-1", "concurrent-1");
    append(&h.storage, "concurrent-2", "concurrent-2");
    gate.open();
    bounded(restoring)
        .await
        .expect("#1011 active restore join")
        .expect("#1011 active restore succeeds");
    assert_snapshot(&h, &snaps[2], 0);
    assert_eq!(
        lock(&h.storage.projection.entries).len(),
        4,
        "#1011 active restore retains concurrent suffix"
    );
}

fn queued_metadata(storage: &FactoryStorage, title: &str, turn: &str) -> BoxFuture<RuntimeResult> {
    let mut value = JsObject::new();
    value.insert("title".into(), JsValue::String(title.into()));
    value.insert("titleSource".into(), JsValue::String("user".into()));
    storage.append_metadata(turn.into(), JsValue::Object(value))
}
async fn queued_matrix(event: &str) {
    let row = case(&format!("concurrent-{event}"));
    let snaps = array(field(&row, "snapshots")).expect("#1011 queued snapshots");
    let h = Harness::new(&snaps[0]);
    h.storage.restore().await.expect("#1011 queued restore");
    assert_snapshot(&h, &snaps[0], 0);
    // The concurrent cells hold the same valid before-cache as fresh-*, so the
    // queued restore must have consumed its rows too; only this sees it.
    assert_eq!(
        *lock(&h.storage.cache.hydrated_rows),
        2,
        "#1011 valid cache is hydrated"
    );
    let (gate, entered) = Gate::new();
    *lock(&h.log.append_gate) = Some(gate.clone());
    let first = tokio::spawn(queued_metadata(&h.storage, "concurrent-1", "concurrent-1"));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 queued first gate")
    })
    .await
    .expect("#1011 queued gate join");
    let second = tokio::spawn(queued_metadata(&h.storage, "concurrent-2", "concurrent-2"));
    let operation = match event {
        "append" => queued_metadata(&h.storage, "appended", "append"),
        "load" => {
            let loaded = h.storage.restore();
            Box::pin(async move { loaded.await.map(|_| ()) })
        }
        "checkpoint" | "drain" => h.storage.flush(),
        "shutdown" | "recreate" => h.storage.close_owned(),
        _ => panic!("#1011 queue event"),
    };
    let operation = tokio::spawn(operation);
    assert!(
        !first.is_finished() && !second.is_finished() && !operation.is_finished(),
        "#1011 all admitted operations remain pending behind predecessor"
    );
    assert_eq!(
        h.storage
            .projection
            .snapshot()
            .expect("#1011 queue held projection"),
        expected_state(&snaps[1], 0),
        "#1011 queue held projection equals TS"
    );
    assert_eq!(
        *lock(&h.cache.bytes),
        cache_bytes(&snaps[1]),
        "#1011 queue held cache equals TS"
    );
    gate.open();
    bounded(first)
        .await
        .expect("#1011 queue first joined")
        .expect("#1011 queue first committed");
    bounded(second)
        .await
        .expect("#1011 queue second joined")
        .expect("#1011 queue second committed");
    bounded(operation)
        .await
        .expect("#1011 queued event joined")
        .expect("#1011 queued event fulfilled");
    if event == "recreate" {
        assert!(
            h.storage.projection.snapshot().is_err(),
            "#1011 queued predecessor disposed"
        );
        let next = Harness {
            storage: Harness::storage(h.log.clone(), h.cache.clone()),
            log: h.log.clone(),
            cache: h.cache.clone(),
        };
        next.storage
            .restore()
            .await
            .expect("#1011 fenced replacement restore");
        assert_eq!(
            next.storage.recorder.snapshot_state().0,
            4,
            "#1011 fenced recreation retains both predecessor commits"
        );
        assert_eq!(
            next.storage
                .projection
                .snapshot()
                .expect("#1011 fenced projection")
                .metadata
                .get("title"),
            Some(&JsValue::String("concurrent-2".into())),
            "#1011 fenced replacement is current rather than TS stale handle"
        );
        assert_eq!(
            *lock(&next.log.entries),
            transcript(&snaps[2]),
            "#1011 fenced recreation durable log equals TS"
        );
        let cache = next.cache.load().expect("#1011 recreation cache");
        let expected = js(&cache_bytes(&snaps[2]).expect("#1011 TS recreation cache"));
        assert_eq!(
            field(&cache, "anchor"),
            field(&expected, "anchor"),
            "#1011 fenced recreation anchor equals TS"
        );
        assert_eq!(
            field(&cache, "asOfSequence"),
            field(&expected, "asOfSequence"),
            "#1011 recreation cache equals TS watermark"
        );
    } else {
        assert_snapshot(&h, &snaps[2], 0);
    }
}
macro_rules! queued_tests { ($($name:ident => $event:literal),* $(,)?) => { $(
    #[tokio::test(flavor="multi_thread",worker_threads=2)] async fn $name() { queued_matrix($event).await; }
)* }; }
queued_tests! { concurrent_load=>"load", concurrent_append=>"append", concurrent_recreate=>"recreate", concurrent_checkpoint=>"checkpoint", concurrent_drain=>"drain", concurrent_shutdown=>"shutdown" }

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn predecessor_pending_queue_holds_successors() {
    let row = case("predecessor-pending");
    let h = Harness::new(field(&row, "before"));
    h.storage
        .restore()
        .await
        .expect("#1011 pending queue restore");
    let (gate, entered) = Gate::new();
    *lock(&h.log.append_gate) = Some(gate.clone());
    let first = tokio::spawn(queued_metadata(&h.storage, "first", "first"));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("#1011 pending queue gate")
    })
    .await
    .expect("#1011 pending queue gate join");
    let second = tokio::spawn(queued_metadata(&h.storage, "second", "second"));
    let flush = tokio::spawn(h.storage.flush());
    assert!(
        !first.is_finished() && !second.is_finished() && !flush.is_finished(),
        "#1011 never-released predecessor holds admitted successors"
    );
    assert_eq!(
        *lock(&h.cache.bytes),
        cache_bytes(field(field(&row, "details"), "held")),
        "#1011 cache progresses independently of pending queue"
    );
    gate.open();
    bounded(first)
        .await
        .expect("#1011 released predecessor joins")
        .expect("#1011 released predecessor commits");
    bounded(second)
        .await
        .expect("#1011 pending successor join")
        .expect("#1011 pending successor committed");
    bounded(flush)
        .await
        .expect("#1011 pending flush join")
        .expect("#1011 pending flush completed");
    assert_snapshot(&h, field(&row, "settled"), 0);
}

#[tokio::test]
async fn active_corrupt_transcript_rejected() {
    let row = case("fresh-load");
    let snapshots = array(field(&row, "snapshots")).expect("#1011 corrupt active snapshots");
    let h = Harness::new(&snapshots[0]);
    h.storage.restore().await.expect("#1011 valid active seed");
    let state = h
        .storage
        .projection
        .snapshot()
        .expect("#1011 valid seed state");
    let writer = h.storage.recorder.snapshot_state();
    let duplicate = lock(&h.log.entries)
        .last()
        .cloned()
        .expect("#1011 duplicate seed");
    lock(&h.log.entries).push(duplicate);
    assert!(
        h.storage.restore().await.is_err(),
        "#1011 active restore rejects invalid event chain"
    );
    assert_eq!(
        h.storage.recorder.snapshot_state(),
        writer,
        "#1011 rejected active restore preserves writer"
    );
    assert_eq!(
        h.storage
            .projection
            .snapshot()
            .expect("#1011 preserved state readable"),
        state,
        "#1011 rejected active restore preserves projection"
    );
}

#[tokio::test]
async fn committed_error_requires_recreation() {
    let row = case("fresh-load");
    let snapshots = array(field(&row, "snapshots")).expect("#1011 commit error snapshots");
    let h = Harness::new(&snapshots[0]);
    h.storage
        .restore()
        .await
        .expect("#1011 commit error initial restore");
    h.log.fail_after_commit.store(true, Ordering::SeqCst);
    assert!(
        queued_metadata(&h.storage, "uncertain", "uncertain")
            .await
            .is_err(),
        "#1011 injected post-commit failure"
    );
    let error = h
        .storage
        .restore()
        .await
        .expect_err("#1011 active restore refuses ahead-of-writer log");
    assert_eq!(
        error.message, "Session transcript is ahead of its writer; recreate the session.",
        "#1011 uncertain commit requires recreation"
    );
    // The fenced writer resolves later appends without writing (TS close),
    // so the uncertain commit cannot be duplicated by the stale writer.
    queued_metadata(&h.storage, "forbidden", "forbidden")
        .await
        .expect("#1011 fenced writer drops later appends");
    assert_eq!(
        lock(&h.log.entries).len(),
        3,
        "#1011 uncertain commit remains single durable record"
    );
}
