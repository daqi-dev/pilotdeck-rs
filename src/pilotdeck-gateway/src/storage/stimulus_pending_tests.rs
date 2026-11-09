use super::*;
use crate::runtime::turn::publish::{FileRegistry, TurnRunRegistry};
use crate::storage::{FileRunRegistry, RunAcceptInput, RunRef};
use std::sync::mpsc;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-pending-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn arm(&self, id: &str) {
        fs::write(self.0.join("stimulus.json"), format!(r#"{{"registry":{{"barrier":{{"id":"{id}","key":"agent_status:turn_timeout","boundMs":1000}}}}}}"#)).unwrap();
    }
    fn receipts(&self) -> Vec<JsValue> {
        let Ok(entries) = fs::read_dir(self.0.join("witness")) else {
            return Vec::new();
        };
        let mut entries = entries
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        entries
            .into_iter()
            .filter(|path| path.extension().is_some_and(|s| s == "json"))
            .map(|path| parse_json(&fs::read_to_string(path).unwrap()).unwrap())
            .collect()
    }
    fn phase(&self, name: &str) -> Vec<JsValue> {
        self.receipts()
            .into_iter()
            .filter(|value| {
                field(value, "phase")
                    .as_string()
                    .is_some_and(|s| s.equals_str(name))
            })
            .collect()
    }
    fn wait_phase(&self, phase: &str) -> Vec<JsValue> {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            let values = self.phase(phase);
            if !values.is_empty() {
                return values;
            }
            assert!(
                Instant::now() < end,
                "expected real receipt missing: {phase}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn release(&self, seq: u64) {
        fs::write(self.0.join("release.json"), format!(r#"{{"seq":{seq}}}"#)).unwrap();
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn error_event() -> JsValue {
    object([
        ("type", "error".into()),
        ("code", "turn_timeout".into()),
        ("runId", "r".into()),
    ])
}
fn status_event() -> JsValue {
    object([
        ("type", "agent_status".into()),
        ("event", "turn_timeout".into()),
        ("runId", "r".into()),
    ])
}

fn scope(sequence: u64) -> RegistryAppendScope {
    RegistryAppendScope {
        registry: uuid::Uuid::from_u128(1),
        sequence,
        run: RunRef {
            project_key: Some("p".into()),
            session_key: "s".into(),
            run_id: "r".into(),
        },
    }
}

struct Held {
    stimulus: Arc<StorageStimulus>,
    thread: Option<thread::JoinHandle<io::Result<Applied>>>,
    seq: u64,
}
impl Held {
    fn start(scratch: &Scratch, stimulus: &Arc<StorageStimulus>) -> Self {
        Self::scoped(scratch, stimulus, scope(0))
    }
    fn scoped(
        scratch: &Scratch,
        stimulus: &Arc<StorageStimulus>,
        scope: RegistryAppendScope,
    ) -> Self {
        let copy = Arc::clone(stimulus);
        let thread = thread::spawn(move || {
            let _scope = RegistryAppendScopeGuard::enter(scope);
            copy.before_write(Site::Registry, "agent_status:turn_timeout")
        });
        let end = Instant::now() + Duration::from_secs(2);
        let seq = loop {
            if let Some(value) = scratch.phase("block").first() {
                break field(value, "seq").as_number().unwrap() as u64;
            }
            assert!(Instant::now() < end, "real barrier never entered");
            thread::sleep(Duration::from_millis(2));
        };
        Self {
            stimulus: Arc::clone(stimulus),
            thread: Some(thread),
            seq,
        }
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        let _ = fs::write(
            self.stimulus.dir.join("release.json"),
            format!(r#"{{"seq":{}}}"#, self.seq),
        );
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn queued_before_barrier_gets_exactly_one_bound_ticket_after_hold() {
    let scratch = Scratch::new();
    scratch.arm("nonce-a");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let (admission, job) = registry_append_ticket(Some(&stimulus), &error_event(), scope(1))
        .unwrap()
        .unwrap();
    admission.enqueued();
    assert!(scratch.phase("queued").is_empty());
    let held = Held::start(&scratch, &stimulus);
    let queued = scratch.wait_phase("queued");
    assert_eq!(queued.len(), 1);
    assert!(
        field(&queued[0], "barrierId")
            .as_string()
            .unwrap()
            .equals_str("nonce-a")
    );
    assert_eq!(
        field(&queued[0], "barrierSeq").as_number(),
        Some(held.seq as f64)
    );
    assert!(field(&queued[0], "seq").as_number().unwrap() > held.seq as f64);
    admission.enqueued();
    assert_eq!(scratch.phase("queued").len(), 1);
    drop(held);
    job.start(Duration::from_secs(1)).unwrap();
    drop(job);
    assert_eq!(scratch.phase("queued").len(), 1);
}

#[test]
fn unadmitted_dropped_started_and_old_nonce_jobs_never_get_tickets() {
    for mode in ["unadmitted", "dropped", "started", "old-nonce"] {
        let scratch = Scratch::new();
        scratch.arm("nonce-a");
        let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
        let (admission, job) = registry_append_ticket(Some(&stimulus), &error_event(), scope(1))
            .unwrap()
            .unwrap();
        match mode {
            "unadmitted" => {
                let held = Held::start(&scratch, &stimulus);
                assert!(scratch.phase("queued").is_empty());
                drop(held);
                drop(job);
            }
            "dropped" => {
                drop(job);
                admission.enqueued();
                let held = Held::start(&scratch, &stimulus);
                assert!(scratch.phase("queued").is_empty());
                drop(held);
            }
            "started" => {
                admission.enqueued();
                job.start(Duration::from_secs(1)).unwrap();
                admission.enqueued();
                let held = Held::start(&scratch, &stimulus);
                assert!(scratch.phase("queued").is_empty());
                drop(held);
                drop(job);
            }
            _ => {
                admission.enqueued();
                scratch.arm("nonce-b");
                let held = Held::start(&scratch, &stimulus);
                assert!(scratch.phase("queued").is_empty());
                drop(held);
                drop(job);
            }
        }
    }
}

#[test]
fn append_admitted_after_hold_is_witnessed_once() {
    let scratch = Scratch::new();
    scratch.arm("nonce-a");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let held = Held::start(&scratch, &stimulus);
    let (admission, job) = registry_append_ticket(Some(&stimulus), &error_event(), scope(1))
        .unwrap()
        .unwrap();
    admission.enqueued();
    admission.enqueued();
    assert_eq!(scratch.wait_phase("queued").len(), 1);
    drop(held);
    job.start(Duration::from_secs(1)).unwrap();
}

struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_file_registry_queued_before_hold_survives_dropped_awaiter() {
    let scratch = Scratch::new();
    scratch.arm("real-nonce");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let file =
        FileRunRegistry::open_with_stimulus(scratch.0.join("runs.json"), Some(stimulus)).unwrap();
    let run = RunRef {
        project_key: Some("p".into()),
        session_key: "s".into(),
        run_id: "r".into(),
    };
    file.accept(&RunAcceptInput {
        run: run.clone(),
        request_material: "{}".into(),
    })
    .unwrap();
    let registry = FileRegistry::new(file).unwrap();
    let (release, receive) = mpsc::channel();
    let release = Release(Some(release));
    let (entered, observe) = mpsc::channel();
    let first = registry.call(move |_| {
        let _ = entered.send(());
        let _ = receive.recv_timeout(Duration::from_secs(2));
        Ok(())
    });
    observe.recv_timeout(Duration::from_secs(1)).unwrap();
    let status = registry.append(&run, &status_event());
    let error = registry.append(&run, &error_event());
    drop(error); // Awaiter cancellation does not cancel accepted FIFO I/O.
    drop(release);
    first.await.unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while scratch.phase("queued").is_empty() {
        assert!(
            Instant::now() < end,
            "already-enqueued real append lost its witness"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let block = scratch.phase("block");
    let queued = scratch.wait_phase("queued");
    assert_eq!(queued.len(), 1);
    assert_eq!(field(&queued[0], "barrierSeq"), field(&block[0], "seq"));
    scratch.release(field(&block[0], "seq").as_number().unwrap() as u64);
    status.await.unwrap();
    assert_eq!(registry.events(&run).await.unwrap().len(), 2);
    assert_eq!(scratch.phase("queued").len(), 1);
}

#[test]
fn delayed_initializer_and_rearm_cannot_attach_old_sequence_to_new_barrier() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let _enter = runtime.enter();
    let scratch = Scratch::new();
    scratch.arm("old-arm");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let (release, receive) = mpsc::channel();
    let release = Release(Some(release));
    let (entered, observe) = mpsc::channel();
    let gate = runtime.spawn_blocking(move || {
        let _ = entered.send(());
        let _ = receive.recv_timeout(Duration::from_secs(2));
    });
    observe.recv_timeout(Duration::from_secs(1)).unwrap();
    let (admission, job) = registry_append_ticket(Some(&stimulus), &error_event(), scope(1))
        .unwrap()
        .unwrap();
    admission.enqueued(); // Arm read is now actually delayed behind the gate.
    assert!(!admission.ticket.state.lock().unwrap().initialized);
    scratch.arm("new-arm");
    let held = Held::scoped(&scratch, &stimulus, scope(2));
    drop(release);
    runtime.block_on(gate).unwrap();
    let end = Instant::now() + Duration::from_secs(1);
    while !admission.ticket.state.lock().unwrap().initialized {
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(2));
    }
    // Deliberately make a claim attempt while both real states still exist;
    // removing the event-sequence predicate must make this negative turn red.
    stimulus.publish_pending_registry();
    assert_eq!(stimulus.inner.lock().unwrap().registry.held, Some(held.seq));
    assert!(scratch.phase("queued").is_empty());
    drop(held);
    job.start(Duration::from_secs(1)).unwrap();
}

#[test]
fn different_run_cannot_borrow_a_matching_nonce_and_later_sequence() {
    let scratch = Scratch::new();
    scratch.arm("same-arm");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let mut other = scope(1);
    other.run.run_id = "different-run".into();
    let (admission, job) = registry_append_ticket(Some(&stimulus), &error_event(), other)
        .unwrap()
        .unwrap();
    admission.enqueued();
    let held = Held::start(&scratch, &stimulus);
    stimulus.publish_pending_registry();
    assert!(scratch.phase("queued").is_empty());
    drop(held);
    job.start(Duration::from_secs(1)).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_real_file_registries_sharing_stimulus_cannot_witness_each_other() {
    let scratch = Scratch::new();
    scratch.arm("shared-arm");
    let stimulus = Arc::new(StorageStimulus::control(&scratch.0));
    let run = scope(0).run;
    let open = |name| {
        let file =
            FileRunRegistry::open_with_stimulus(scratch.0.join(name), Some(Arc::clone(&stimulus)))
                .unwrap();
        file.accept(&RunAcceptInput {
            run: run.clone(),
            request_material: "{}".into(),
        })
        .unwrap();
        FileRegistry::new(file).unwrap()
    };
    let a = open("a.json");
    let b = open("b.json");
    let (release, receive) = mpsc::channel();
    let release = Release(Some(release));
    let (entered, observe) = mpsc::channel();
    let blocker = b.call(move |_| {
        let _ = entered.send(());
        let _ = receive.recv_timeout(Duration::from_secs(2));
        Ok(())
    });
    observe.recv_timeout(Duration::from_secs(1)).unwrap();
    let first = a.append(&run, &status_event());
    let end = Instant::now() + Duration::from_secs(1);
    while scratch.phase("block").is_empty() {
        assert!(Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // B's follower sequence is later than A's held event, and the RunRef and
    // nonce are intentionally equal: only the real registry instance differs.
    let prefix = b.append(&run, &error_event());
    let follower = b.append(&run, &error_event());
    tokio::time::sleep(Duration::from_millis(30)).await;
    stimulus.publish_pending_registry();
    assert!(scratch.phase("queued").is_empty());
    drop(release);
    blocker.await.unwrap();
    prefix.await.unwrap();
    follower.await.unwrap();
    let block = scratch.phase("block");
    scratch.release(field(&block[0], "seq").as_number().unwrap() as u64);
    first.await.unwrap();
    assert!(scratch.phase("queued").is_empty());
}
