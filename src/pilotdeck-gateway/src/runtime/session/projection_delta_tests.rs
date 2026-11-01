use super::*;
use pilotdeck_protocol::parse_json;
use std::time::{Duration, Instant};

fn js(text: &str) -> JsValue {
    parse_json(text).expect("#1253 test JSON")
}

#[derive(Default)]
struct Log(Mutex<Vec<JsValue>>);
impl TranscriptStore for Log {
    fn append(&self, _: &TranscriptKey, entry: &JsValue) -> crate::storage::StoreResult<()> {
        lock(&self.0).push(entry.clone());
        Ok(())
    }
    fn read(
        &self,
        _: &TranscriptKey,
        _: Option<u64>,
    ) -> crate::storage::StoreResult<TranscriptRead> {
        Ok(TranscriptRead {
            entries: lock(&self.0).clone(),
            diagnostics: vec![],
        })
    }
}
struct Cache;
impl ProjectionCheckpointStore for Cache {
    fn load(&self) -> Option<JsValue> {
        None
    }
    fn save(&self, _: &JsValue) -> RuntimeResult {
        Ok(())
    }
}
fn store() -> ProjectedStore {
    let key = TranscriptKey {
        project_root: PathBuf::from("/unused-1253"),
        pilot_home: PathBuf::from("/unused-1253"),
        session_id: "session-1253".into(),
        transcript_path: PathBuf::from("/unused-1253/log.jsonl"),
    };
    let inner = Arc::new(Log::default());
    let projection = Arc::new(SessionProjection::default());
    let cache = Arc::new(ProjectionCache::new(
        Arc::new(Cache),
        inner.clone(),
        key.clone(),
        projection.clone(),
    ));
    // Isolate append projection work from asynchronous checkpoint capture.
    cache.dispose();
    ProjectedStore {
        _writer_lease: Arc::default(),
        inner,
        key,
        projection,
        cache,
    }
}
fn append(store: &ProjectedStore, entry: &JsValue) {
    store
        .append(&store.key, entry)
        .expect("#1253 durable append");
}

#[test]
#[ignore = "bounded projection benchmark; elapsed time is not a CI assertion"]
fn non_metadata_append_benchmark() {
    let started = Instant::now();
    for kind in [
        "accepted_input",
        "durable_message",
        "agent_status",
        "turn_result",
        "completed_message",
    ] {
        for count in [256, 512, 1024] {
            let store = store();
            let completed = kind == "completed_message";
            if completed {
                append(
                    &store,
                    &js(
                        r#"{"type":"turn_result","sequence":1,"turnId":"done","result":{"usage":{},"permissionDenials":[]}}"#,
                    ),
                );
            }
            let event_kind = if completed { "durable_message" } else { kind };
            let entries: Vec<_> = (1..=count).map(|n| {
                let sequence = n + usize::from(completed);
                let turn = if completed { "done".to_owned() } else { format!("turn-{sequence}") };
                js(&format!(r#"{{"type":"{event_kind}","sequence":{sequence},"turnId":"{turn}","messages":[{{"role":"user","content":[{{"type":"text","text":"request"}}]}}],"message":{{"role":"assistant","content":[{{"type":"text","text":"answer"}}]}},"result":{{"usage":{{"inputTokens":1}},"permissionDenials":[]}}}}"#))
            }).collect();
            let begin = Instant::now();
            for entry in &entries {
                append(&store, entry);
            }
            let elapsed = begin.elapsed();
            assert_eq!(
                store
                    .projection
                    .snapshot()
                    .expect("#1253 benchmark projection"),
                project(&lock(&store.projection.entries)).expect("#1253 benchmark replay"),
                "#1253 benchmark equals replay"
            );
            println!(
                "#1253 append kind={kind} n={count} elapsed_us={} ns_per_append={}",
                elapsed.as_micros(),
                elapsed.as_nanos() / count as u128
            );
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "#1253 benchmark bounded to 120 seconds"
            );
        }
    }
}

#[test]
fn restore_live_suffix_rebuilds_completion_index() {
    let oracle = oracle();
    let row = array(field(&oracle, "cases"))
        .expect("#1253 suffix cases")
        .iter()
        .find(|row| is(field(row, "name"), "restore"))
        .expect("#1253 suffix case");
    let steps = array(field(row, "steps")).expect("#1253 suffix steps");
    let store = store();
    append(&store, field(&steps[0], "entry"));
    let stale_read = vec![field(&steps[0], "entry").clone()];
    // Deterministically commit completion after the reader's captured prefix,
    // then restore that prefix while the newer completion is already live.
    append(&store, field(&steps[1], "entry"));
    lock(&store.projection.completed_turns).clear();
    store
        .cache
        .restore(stale_read)
        .expect("#1253 restore merges live suffix");
    for step in &steps[2..] {
        append(&store, field(step, "entry"));
        assert_eq!(
            store.projection.snapshot().expect("#1253 suffix snapshot"),
            expected(field(step, "expected")),
            "#1253 completed index includes merged live suffix"
        );
    }
}

fn oracle() -> JsValue {
    crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/session-factory/append-delta/oracle.json"
    ))
}
fn expected(value: &JsValue) -> DurableSessionState {
    DurableSessionState {
        messages: array(field(value, "messages"))
            .expect("#1253 oracle messages")
            .to_vec(),
        usage: spread(field(value, "usage")),
        permission_denials: array(field(value, "permissionDenials"))
            .expect("#1253 oracle denials")
            .to_vec(),
        metadata: spread(field(value, "metadata")),
    }
}
#[test]
fn delta_matches_ts_matrix() {
    let oracle = oracle();
    for row in array(field(&oracle, "cases")).expect("#1253 oracle cases") {
        let store = store();
        for step in array(field(row, "steps")).expect("#1253 oracle steps") {
            append(&store, field(step, "entry"));
            let actual = store.projection.snapshot().expect("#1253 delta snapshot");
            assert_eq!(
                actual,
                expected(field(step, "expected")),
                "#1253 delta equals TS at every event: {:?}",
                field(step, "entry")
            );
            assert_eq!(
                actual,
                project(&lock(&store.projection.entries)).expect("#1253 replay snapshot"),
                "#1253 delta equals full replay"
            );
        }
    }
}

#[test]
fn restored_completion_index_matches_ts() {
    let oracle = oracle();
    let row = array(field(&oracle, "cases"))
        .expect("#1253 restore cases")
        .iter()
        .find(|row| is(field(row, "name"), "restore"))
        .expect("#1253 restore case");
    let steps = array(field(row, "steps")).expect("#1253 restore steps");
    // The final context record is projection-only (legacy out-of-turn). Every
    // prefix before it is also a valid recorder log, so test all restore cuts.
    for split in 0..steps.len() {
        let store = store();
        let prefix: Vec<_> = steps[..split]
            .iter()
            .map(|s| field(s, "entry").clone())
            .collect();
        for entry in &prefix {
            append(&store, entry);
        }
        // Clear just the cached index to ensure restore, not previous appends,
        // supplies completed turns (the live suffix merge uses this path too).
        lock(&store.projection.completed_turns).clear();
        store
            .cache
            .restore(prefix)
            .expect("#1253 restore valid recorder prefix");
        for step in &steps[split..] {
            append(&store, field(step, "entry"));
            assert_eq!(
                store
                    .projection
                    .snapshot()
                    .expect("#1253 restored snapshot"),
                expected(field(step, "expected")),
                "#1253 restore rebuilds completed turns at split {split}"
            );
        }
    }
}

#[test]
fn ordinary_append_never_replays_history() {
    let store = store();
    append(
        &store,
        &js(
            r#"{"type":"turn_result","sequence":1,"turnId":"done","result":{"usage":{},"permissionDenials":[]}}"#,
        ),
    );
    PROJECTION_VISITS.with(|visits| visits.set(0));
    for n in 2..130 {
        let entry = js(&format!(
            r#"{{"sequence":{n},"type":"accepted_input","turnId":"done","messages":[{{"role":"user","content":[{{"type":"text","text":"history"}}]}}]}}"#
        ));
        append(&store, &entry);
    }
    for (offset, draft) in [
        r#""type":"assistant_message","message":{"role":"assistant","content":[]}"#,
        r#""type":"tool_result_message","message":{"role":"user","content":[]}"#,
        r#""type":"durable_message","message":{"role":"assistant","content":[]}"#,
        r#""type":"context_snapshot","runtimeContextMessages":[]"#,
        r#""type":"agent_status","status":"working""#,
        r#""type":"session_metadata","metadata":{"title":"delta"}"#,
        r#""type":"control_boundary","boundary":{"kind":"snip"}"#,
    ]
    .iter()
    .enumerate()
    {
        append(
            &store,
            &js(&format!(
                r#"{{"sequence":{},"turnId":"done",{draft}}}"#,
                130 + offset
            )),
        );
    }
    store
        .projection
        .snapshot()
        .expect("#1253 bounded delta snapshot");
    assert_eq!(
        PROJECTION_VISITS.with(|visits| visits.get()),
        0,
        "#1253 ordinary append must not replay historical entries"
    );
}

#[test]
fn invalid_delta_keeps_last_valid_and_compaction_recovers() {
    let store = store();
    append(
        &store,
        &js(r#"{"type":"accepted_input","sequence":1,"messages":[{"role":"user","content":[]}]}"#),
    );
    let before = store
        .projection
        .snapshot()
        .expect("#1253 initial valid projection");
    append(
        &store,
        &js(
            r#"{"type":"accepted_input","sequence":2,"messages":[{"role":"user","content":[]},null],"metadata":{"modelSelection":{"mode":"auto"}}}"#,
        ),
    );
    assert!(
        store.projection.snapshot().is_err(),
        "#1253 invalid delta remains observable"
    );
    assert_eq!(
        *lock(&store.projection.last_valid),
        before,
        "#1253 invalid delta cannot partially mutate last valid state"
    );
    append(
        &store,
        &js(r#"{"type":"session_metadata","sequence":3,"metadata":{"title":"after error"}}"#),
    );
    assert!(
        store.projection.snapshot().is_err(),
        "#1253 later delta cannot erase historical error"
    );
    append(
        &store,
        &js(
            r#"{"type":"control_boundary","sequence":4,"turnId":"pending","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"assistant","content":[]}]}}}"#,
        ),
    );
    let recovered = store
        .projection
        .snapshot()
        .expect("#1253 compact hides invalid historical message");
    assert_eq!(
        recovered.messages,
        vec![js(r#"{"role":"assistant","content":[]}"#)],
        "#1253 complete compact snapshot recovers projection"
    );
    assert_eq!(
        recovered.metadata.get("title"),
        Some(&"after error".into()),
        "#1253 recovery preserves metadata"
    );
}

#[test]
fn completion_during_invalid_projection_survives_recovery() {
    let store = store();
    append(
        &store,
        &js(r#"{"type":"accepted_input","sequence":1,"messages":null}"#),
    );
    append(
        &store,
        &js(
            r#"{"type":"turn_result","sequence":2,"turnId":"done","result":{"usage":{},"permissionDenials":[]}}"#,
        ),
    );
    assert!(
        store.projection.snapshot().is_err(),
        "#1253 completed invalid history still errors"
    );
    append(
        &store,
        &js(
            r#"{"type":"control_boundary","sequence":3,"turnId":"compact","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"assistant","content":[]}]}}}"#,
        ),
    );
    append(
        &store,
        &js(
            r#"{"type":"durable_message","sequence":4,"turnId":"done","message":{"role":"assistant","content":[{"type":"text","text":"late"}]}}"#,
        ),
    );
    assert_eq!(
        store
            .projection
            .snapshot()
            .expect("#1253 recovered completion snapshot")
            .messages,
        vec![
            js(r#"{"role":"assistant","content":[]}"#),
            js(r#"{"role":"assistant","content":[{"type":"text","text":"late"}]}"#)
        ],
        "#1253 completion index survives invalid projection"
    );
}

#[test]
fn reload_retry_budget_and_concurrent_writer() {
    use std::sync::{atomic::AtomicUsize, mpsc};
    for changes in [0, 1, 8] {
        let store = store();
        let storage =
            FactoryStorage::with_checkpoint_store(store.inner, store.key, Arc::new(Cache));
        let writer = storage.recorder.writer().clone();
        let (request, requested) = mpsc::channel();
        let (reply, replied) = mpsc::channel();
        let append_thread = std::thread::spawn(move || {
            for n in 0..changes {
                requested
                    .recv_timeout(Duration::from_secs(5))
                    .expect("#1253 concurrent append request");
                writer
                    .record_session_metadata(
                        &"session-1253".into(),
                        &"writer".into(),
                        js(&format!(r#"{{"title":"{n}"}}"#)),
                    )
                    .expect("#1253 concurrent metadata append");
                reply.send(()).expect("#1253 concurrent append reply");
            }
        });
        let replied = Mutex::new(replied);
        let calls = Arc::new(AtomicUsize::new(0));
        let attempts = calls.clone();
        let source = ReloadSource {
            session_id: "session-1253".into(),
            cwd: storage.cwd.clone(),
            transcript_path: storage.recorder.writer().path().to_owned(),
            projection: storage.projection.clone(),
            file_state: Arc::new(move || {
                if attempts.fetch_add(1, Ordering::SeqCst) < changes {
                    request.send(()).expect("#1253 release concurrent append");
                    lock(&replied)
                        .recv_timeout(Duration::from_secs(5))
                        .expect("#1253 concurrent writer finishes");
                }
                Ok(SeedState::default())
            }),
        };
        let snapshot = source.snapshot_consistent(&storage.recorder);
        append_thread
            .join()
            .expect("#1253 concurrent writer joined");
        if changes == 8 {
            assert_eq!(
                snapshot
                    .expect_err("#1253 eight conflicts must fail")
                    .message,
                "Session changed during runtime reload; retry.",
                "#1253 retry exhaustion is explicit"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                8,
                "#1253 bounded retry budget is eight"
            );
        } else {
            let snapshot = snapshot.expect("#1253 stable reload succeeds");
            assert_eq!(
                snapshot.transcript_writer_state.sequence, changes as u64,
                "#1253 reload writer version agrees"
            );
            assert_eq!(
                snapshot.state,
                storage
                    .projection
                    .snapshot()
                    .expect("#1253 final projection"),
                "#1253 reload state agrees"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                changes + 1,
                "#1253 retry until stable"
            );
        }
    }
}

#[test]
#[ignore = "requires fixed Node v22.22.0 / ICU 78.2 and real TS baseline"]
fn append_delta_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-factory/append-delta");
}

#[test]
#[ignore = "requires fixed Node and real TS baseline"]
fn tail_generator_cleans_only_owned_work() {
    use std::fs;
    for collision in [false, true] {
        let root = std::env::temp_dir().join(format!("pdrs-1253-tail-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("run-existing")).expect("#1253 create foreign work");
        fs::write(root.join("run-existing/keep"), "foreign").expect("#1253 foreign marker");
        fs::create_dir_all(root.join("collector")).expect("#1253 foreign collector");
        fs::write(root.join("collector/keep"), "foreign").expect("#1253 foreign collector marker");
        if collision {
            fs::write(root.join("oracle.json"), "foreign oracle")
                .expect("#1253 seed output collision");
        }
        let base =
            PathBuf::from(std::env::var_os("PDRS_BASELINE").expect("#1253 TS baseline configured"));
        let mut command = crate::test_baseline::node_command();
        command
            .arg("--disable-warning=ExperimentalWarning")
            .arg("--import")
            .arg(base.join("node_modules/tsx/dist/loader.mjs"))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/turn-runner-tail/generate.mts"),
            )
            .arg(&root);
        let (status, stdout, stderr) = crate::test_baseline::run_generator_with_deadline(
            &mut command,
            "turn-runner-tail",
            Duration::from_secs(120),
        )
        .expect("#1253 bounded tail generator");
        assert!(
            status.success() != collision,
            "#1253 tail generator succeeds: {}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        let mut names: Vec<_> = fs::read_dir(&root)
            .expect("#1253 list generator output")
            .map(|entry| {
                entry
                    .expect("#1253 output entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                ".generation-owner",
                "collector",
                "oracle.json",
                "raw",
                "run-existing"
            ],
            "#1253 tail generator removes only its owned work root"
        );
        assert_eq!(
            fs::read_to_string(root.join("run-existing/keep"))
                .expect("#1253 foreign work retained"),
            "foreign",
            "#1253 foreign run root untouched"
        );
        assert_eq!(
            fs::read_to_string(root.join("collector/keep"))
                .expect("#1253 foreign collector retained"),
            "foreign",
            "#1253 foreign collector untouched"
        );
        assert_eq!(
            fs::read_dir(root.join("collector"))
                .expect("#1253 list foreign collector")
                .count(),
            1,
            "#1253 foreign collector contains only its original file"
        );
        assert!(
            root.join("raw/index.json").is_file(),
            "#1253 raw evidence retained"
        );
        if collision {
            assert!(
                String::from_utf8_lossy(&stderr).contains("EEXIST"),
                "#1253 failed generation reaches exclusive oracle write"
            );
            assert_eq!(
                fs::read_to_string(root.join("oracle.json")).expect("#1253 read collided oracle"),
                "foreign oracle",
                "#1253 failed generation preserves foreign oracle"
            );
        }
        fs::remove_dir_all(root).expect("#1253 remove owned test scratch");
    }
}
