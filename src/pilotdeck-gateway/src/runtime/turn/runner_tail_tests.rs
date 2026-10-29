use super::super::{Recording, RecordingAction, TurnInput};
use super::*;
use crate::{
    runtime::project::{ProjectRuntimeRegistry, ResourceBundle},
    storage::{JsonlTranscriptStore, TranscriptKey},
    test_baseline as baseline,
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{fs, path::Path, sync::atomic::AtomicUsize, time::Duration};
const NOW: &str = "2026-10-27T00:00:00.000Z";
fn json(text: &str) -> JsValue {
    parse_json(text).expect("#742 tail JSON")
}
fn string(v: &JsValue) -> String {
    v.as_string()
        .expect("#742 tail string")
        .to_utf8()
        .expect("#742 tail UTF8")
}
fn array(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = v {
        v
    } else {
        panic!("#742 tail array")
    }
}
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.get(key).expect("#742 tail field")
}
fn row(name: &str) -> JsValue {
    let fixture = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner-tail/oracle.json"
    ));
    array(field(&fixture, "rows"))
        .iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .expect("#742 tail oracle row")
        .clone()
}
struct Fixture {
    root: PathBuf,
    path: PathBuf,
    registry: ProjectRuntimeRegistry,
    recorder: Arc<Recorder>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-tail-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("workspace")).expect("#742 tail directory");
        let path = root.join("session.jsonl");
        let key = TranscriptKey {
            project_root: root.clone(),
            pilot_home: root.join("home"),
            session_id: "session-742".into(),
            transcript_path: path.clone(),
        };
        let recorder = Arc::new(Recorder::new(Arc::new(JsonlTranscriptStore::new()), key));
        let id = AtomicUsize::new(0);
        *recorder
            .writer()
            .test_sources
            .lock()
            .expect("#742 tail sources") = Some((
            Arc::new(|| Ok(NOW.into())),
            Arc::new(move || {
                Ok(format!(
                    "entry-{:04}",
                    id.fetch_add(1, Ordering::SeqCst) + 1
                ))
            }),
        ));
        let recorder = Arc::new(Recorder::from_writer(recorder.writer().clone()));
        let registry =
            ProjectRuntimeRegistry::new(&root, Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())));
        Self {
            root,
            path,
            registry,
            recorder,
        }
    }
    fn runner(
        &self,
        tail: Arc<SessionTail>,
        cancel: Cancellation,
        options: JsValue,
        turn: &str,
    ) -> TurnRunner {
        TurnRunner::new(
            self.recorder.clone(),
            Arc::new(self.registry.acquire(None).expect("#742 tail lease")),
            "session-742".into(),
            turn.into(),
        )
        .with_tail(tail, cancel, options)
    }
    fn disk(&self) -> String {
        if self.path.exists() {
            fs::read_to_string(&self.path).expect("#742 tail disk")
        } else {
            String::new()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("#742 tail cleanup");
    }
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("#742 tail bounded wait")
}
async fn title_done(tail: &SessionTail) {
    let pending = tail
        .state
        .lock()
        .expect("#742 tail pending lock")
        .pending
        .clone()
        .expect("#742 tail pending title");
    bounded(pending.done.cancelled()).await;
}
fn checkpoint(f: &Fixture, tail: &SessionTail, row: &JsValue, label: &str) {
    let snapshot = array(field(row, "snapshots"))
        .iter()
        .find(|s| s.get("label") == Some(&label.into()))
        .expect("#742 tail checkpoint");
    assert_eq!(
        tail.snapshot(),
        *field(snapshot, "metadata"),
        "#742 tail checkpoint metadata {label}"
    );
    assert_eq!(
        f.disk(),
        string(field(snapshot, "diskUtf8")),
        "#742 tail checkpoint disk {label}"
    );
}
async fn replay(name: &str, row: &JsValue) {
    let config = field(row, "config");
    let mode = |key| config.get(key).map(string).unwrap_or_default();
    let title = mode("title");
    let suggestion = mode("suggestion");
    let file = mode("file");
    let listing = mode("listing");
    let f = Fixture::new();
    if ["updated", "same"].contains(&file.as_str()) {
        fs::write(f.root.join("workspace/report.txt"), "old\n").expect("#742 tail initial file");
    }
    let title_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let title_entered = Cancellation::default();
    let title_signal = Arc::new(Mutex::new(None));
    let title_inputs = Arc::new(Mutex::new(Vec::new()));
    let suggestion_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let suggestion_entered = Cancellation::default();
    let suggestion_inputs = Arc::new(Mutex::new(Vec::new()));
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let cfg = TailConfig {
        cwd: Some(f.root.join("workspace")),
        collect_file_artifacts: file != "disabled",
        auto_generate_title: !title.is_empty(),
        title_provider: (!title.is_empty()).then(|| TitleProvider {
            provider_id: Some("native-llm".into()),
            model_provenance: Some(object([
                ("provider", "probe".into()),
                ("model", "aux-title".into()),
            ])),
            generate: Arc::new({
                let gate = title_gate.clone();
                let entered = title_entered.clone();
                let signals = title_signal.clone();
                let inputs = title_inputs.clone();
                let mode = title.clone();
                move |input| {
                    *signals.lock().expect("#742 tail title signal") = Some(input.signal.clone());
                    inputs
                        .lock()
                        .expect("#742 tail title inputs")
                        .push(input.clone());
                    entered.cancel(None);
                    let gate = gate.clone();
                    let mode = mode.clone();
                    Box::pin(async move {
                        if mode == "abort-cooperative" {
                            input.signal.cancelled().await;
                            return Ok(None);
                        }
                        let permit = gate.acquire().await.expect("#742 tail title release");
                        permit.forget();
                        match mode.as_str() {
                            "failure" => Ok(None),
                            "port-failure" => Err("TITLE_MODEL_BOOM".into()),
                            _ => Ok(Some("矩阵 标题".into())),
                        }
                    })
                }
            }),
        }),
        suggestion_generator: (!suggestion.is_empty()).then(|| {
            Arc::new({
                let gate = suggestion_gate.clone();
                let entered = suggestion_entered.clone();
                let mode = suggestion.clone();
                let inputs = suggestion_inputs.clone();
                move |input| {
                    inputs
                        .lock()
                        .expect("#742 tail suggestion input")
                        .push(input);
                    entered.cancel(None);
                    let mode = mode.clone();
                    let gate = gate.clone();
                    Box::pin(async move {
                        if mode == "delayed" {
                            let permit =
                                gate.acquire().await.expect("#742 tail suggestion release");
                            permit.forget();
                        }
                        match mode.as_str() {
                            "empty" | "null" | "failure" => Ok(None),
                            "injected-failure" => Err("SUGGESTION_INJECTED_BOOM".into()),
                            _ => Ok(Some("请 继续 检查输出".into())),
                        }
                    }) as GenerateFuture
                }
            }) as SuggestionGenerator
        }),
        debug: Arc::new({
            let diagnostics = diagnostics.clone();
            move |message| {
                diagnostics
                    .lock()
                    .expect("#742 tail diagnostics")
                    .push(message)
            }
        }),
    };
    let mut tail = SessionTail::new(f.recorder.clone(), "session-742".into(), cfg);
    Arc::get_mut(&mut tail)
        .expect("#742 tail unique config")
        .now = Arc::new(|| NOW.into());
    let mut events = vec![];
    for index in 0..if listing == "two" { 2 } else { 1 } {
        let turn = format!("turn-{}", index + 1);
        let raw = if index == 0 {
            field(row, "input").clone()
        } else {
            object([
                ("type", "text".into()),
                ("text", "  第二轮\n  继续  ".into()),
            ])
        };
        let options = if index == 0 {
            field(row, "submitOptions").clone()
        } else {
            json(r#"{"modelSelection":{"mode":"model","provider":"probe","model":"second-model"}}"#)
        };
        let synthetic = options
            .get("syntheticMessages")
            .map(array)
            .unwrap_or(&[])
            .to_vec();
        let input = TurnInput::from_agent_input(&raw, &options, vec![], synthetic)
            .expect("#742 tail input");
        let cancel = Cancellation::default();
        let runner = f.runner(tail.clone(), cancel.clone(), options, &turn);
        let (runner, out) = runner.start().await.expect("#742 tail start");
        events.extend(out);
        let (runner, out) = runner
            .accept(input.clone())
            .await
            .expect("#742 tail accept");
        events.extend(out);
        let (mut runner, out) = runner
            .submit_prompt(input.input, cancel.clone())
            .await
            .expect("#742 tail prompt");
        events.extend(out);
        runner.begin_tail_work();
        if !title.is_empty() {
            bounded(title_entered.cancelled()).await;
        }
        if index == 0 {
            checkpoint(&f, &tail, row, "main-held");
        }
        if ["before", "failure", "port-failure"].contains(&title.as_str()) {
            title_gate.add_permits(1);
            title_done(&tail).await;
        }
        if ["abort-cooperative", "abort-ignored"].contains(&title.as_str()) || file == "aborted" {
            cancel.cancel(None);
            if title == "abort-cooperative" {
                title_done(&tail).await;
            }
        }
        if !file.is_empty() {
            fs::write(
                f.root.join("workspace/report.txt"),
                if file == "same" {
                    "old\n"
                } else {
                    "hello matrix\n"
                },
            )
            .expect("#742 tail write file");
            if file != "unreported" {
                let tool = object([
                    ("type", "success".into()),
                    ("toolCallId", "write-1".into()),
                    ("toolName", "write_file".into()),
                    (
                        "content",
                        JsValue::Array(vec![object([
                            ("type", "text".into()),
                            ("text", "written".into()),
                        ])]),
                    ),
                    (
                        "data",
                        if file == "explicit" {
                            object([("filePath", "report.txt".into())])
                        } else {
                            object([("path", "report.txt".into())])
                        },
                    ),
                ]);
                runner.observe_tool_result(&tool);
                events.push(runner.event("tool_result", [("result", tool)]));
            }
        }
        let aborted = cancel.is_cancelled();
        let message = json(
            r#"{"role":"assistant","content":[{"type":"text","text":"  已完成矩阵检查。  "}]}"#,
        );
        if !aborted {
            let draft = object([
                ("type", "assistant_message".into()),
                ("message", message.clone()),
            ]);
            f.recorder
                .writer()
                .record_session_event(
                    &"session-742".into(),
                    &turn.clone().into(),
                    draft
                        .as_object()
                        .expect("#742 tail assistant draft")
                        .clone(),
                )
                .expect("#742 tail durable assistant");
            events.push(runner.event("assistant_message", [("message", message.clone())]));
        }
        let mut result = JsObject::new();
        result.insert(
            "type".into(),
            if aborted { "aborted" } else { "success" }.into(),
        );
        result.insert("sessionId".into(), "session-742".into());
        result.insert("turnId".into(), turn.as_str().into());
        if !aborted {
            result.insert("finalMessage".into(), message);
        }
        for (k, v) in [
            (
                "stopReason",
                if aborted {
                    "aborted_streaming"
                } else {
                    "completed"
                }
                .into(),
            ),
            ("usage", json(r#"{"inputTokens":3,"outputTokens":4}"#)),
            ("permissionDenials", JsValue::Array(vec![])),
            ("turns", JsValue::Number(1.0)),
            ("startedAt", NOW.into()),
            ("completedAt", NOW.into()),
        ] {
            result.insert(k.into(), v);
        }
        let mut recording = Recording::new(runner, Arc::new(()));
        recording
            .begin(RecordingAction::Complete(JsValue::Object(result)))
            .expect("#742 tail complete begin");
        if suggestion == "delayed" {
            let next = recording.settle();
            tokio::pin!(next);
            tokio::select! {biased;out=&mut next=>panic!("#742 tail suggestion holds terminal: {:?}",out.map(|v|v.len())),_=suggestion_entered.cancelled()=>{}}
            checkpoint(&f, &tail, row, "suggestion-held");
            if title == "during-suggestion" {
                title_gate.add_permits(1);
                title_done(&tail).await;
            }
            suggestion_gate.add_permits(1);
            events.extend(bounded(next).await.expect("#742 tail delayed suggestion"));
        }
        while recording.is_pending() {
            let out = bounded(recording.settle())
                .await
                .expect("#742 tail complete settle");
            if out.first().and_then(|v| v.get("type")) == Some(&"file_artifacts".into()) {
                assert!(
                    !f.disk().contains("\"type\":\"turn_result\""),
                    "#742 tail artifact precedes durable result"
                );
            }
            if out.first().and_then(|v| v.get("type")) == Some(&"prompt_suggestion".into()) {
                assert!(
                    !f.disk().contains("\"isSnapshot\":true"),
                    "#742 tail suggestion precedes snapshot"
                );
            }
            events.extend(out);
        }
        drop(recording);
        assert_eq!(
            f.registry
                .resolve(None)
                .expect("#742 tail runtime")
                .lease_count(),
            0,
            "#742 tail title must not retain turn lease"
        );
        if index == 0 {
            checkpoint(&f, &tail, row, "before-late-title");
        }
        if ["after", "abort-ignored", "abort-after", "dispose"].contains(&title.as_str()) {
            if title == "abort-after" {
                cancel.cancel(None);
            }
            if title == "dispose" {
                tail.dispose();
            }
            title_gate.add_permits(1);
            title_done(&tail).await;
        }
    }
    assert_eq!(
        stringify_json(&JsValue::Array(events)),
        stringify_json(field(row, "events")),
        "#742 tail event bytes {name}"
    );
    assert_eq!(
        f.disk(),
        string(field(row, "transcript")),
        "#742 tail transcript bytes {name}"
    );
    assert_eq!(
        tail.snapshot(),
        *field(row, "metadata"),
        "#742 tail final metadata {name}"
    );
    if let Some(signal) = title_signal.lock().expect("#742 tail signal lock").as_ref() {
        assert_eq!(
            signal.is_cancelled(),
            ["abort-cooperative", "abort-ignored", "dispose"].contains(&title.as_str()),
            "#742 tail title abort linkage"
        );
    }
    let log = diagnostics.lock().expect("#742 tail diagnostics lock");
    let expected = if suggestion == "injected-failure" {
        vec![JsString::from(
            "[prompt-suggestion] generation skipped (provider_error): SUGGESTION_INJECTED_BOOM",
        )]
    } else {
        vec![]
    };
    assert_eq!(*log, expected, "#742 tail runner diagnostics");
}
macro_rules! case {
    ($id:ident,$name:literal) => {
        #[tokio::test]
        async fn $id() {
            // #1043: the oracle provenance check (650-source closure, one git
            // call per source under PDRS_BASELINE) is not part of the gate
            // budget; `bounded` keeps bounding only the replayed async work.
            let row = row($name);
            bounded(replay($name, &row)).await;
        }
    };
}
case!(title_before, "title-before");
case!(title_after, "title-after");
case!(title_during_suggestion, "title-during-suggestion");
case!(title_abort_cooperative, "title-abort-cooperative");
case!(title_abort_ignored, "title-abort-ignored");
case!(title_abort_after_terminal, "title-abort-after-terminal");
case!(title_dispose_after_terminal, "title-dispose-after-terminal");
case!(title_failure_native, "title-failure-native");
case!(title_failure_port, "title-failure-port");
case!(files_created, "files-created");
case!(files_none, "files-none");
case!(files_updated, "files-updated");
case!(files_explicit_path, "files-explicit-path");
case!(files_same_bytes, "files-same-bytes");
case!(files_unreported_write, "files-unreported-write");
case!(files_collection_disabled, "files-collection-disabled");
case!(files_aborted, "files-aborted");
case!(suggestion_on, "suggestion-on");
case!(suggestion_off, "suggestion-off");
case!(suggestion_empty, "suggestion-empty");
case!(suggestion_null, "suggestion-null");
case!(suggestion_failure_native, "suggestion-failure-native");
case!(suggestion_failure_injected, "suggestion-failure-injected");
case!(suggestion_delayed, "suggestion-delayed");
case!(listing_two_turns, "listing-two-turns");
case!(listing_utf16_limit, "listing-utf16-limit");
case!(listing_empty_model, "listing-empty-model");
case!(listing_empty_no_model, "listing-empty-no-model");
case!(listing_synthetic, "listing-synthetic");
case!(listing_multiblock, "listing-multiblock");
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn regenerates_identically() {
    assert!(
        std::panic::catch_unwind(|| baseline::regenerates_identically("turn-runner-tail")).is_ok(),
        "#742 tail fixture regeneration"
    );
}

#[tokio::test]
async fn collector_policy() {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-runner-tail/oracle.json"
    ));
    for row in array(field(&oracle, "collectorRows")) {
        let f = Fixture::new();
        let cfg = field(row, "config");
        let root = f.root.join("workspace");
        let path = string(field(cfg, "path"));
        let name = string(field(cfg, "name"));
        fs::create_dir_all(root.join(&path).parent().expect("#742 collector parent"))
            .expect("#742 collector directory");
        let fifo = cfg.get("fifo") == Some(&JsValue::Bool(true));
        if fifo {
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(root.join(&path))
                    .status()
                    .expect("#742 collector mkfifo")
                    .success(),
                "#742 collector FIFO created"
            );
        }
        if let Some(before) = cfg.get("before") {
            fs::write(root.join(&path), string(before)).expect("#742 collector before");
        }
        let allowed = if cfg.get("allowed") == Some(&JsValue::Bool(true)) {
            vec![PathBuf::from(&path)]
        } else {
            vec![]
        };
        let cwd = if cfg.get("rootLink") == Some(&JsValue::Bool(true)) {
            let alias = f.root.join("workspace-link");
            std::os::unix::fs::symlink(&root, &alias).expect("#742 collector root symlink");
            alias
        } else {
            root.clone()
        };
        let mut collector = artifacts::Collector::start(cwd.clone(), &allowed);
        let mut peer = (cfg.get("overlap") == Some(&JsValue::Bool(true)))
            .then(|| artifacts::Collector::start(cwd.clone(), &allowed));
        if !fifo {
            fs::write(root.join(&path), "artifact").expect("#742 collector write");
        }
        collector.observe(field(row, "result"));
        if let Some(peer) = &mut peer {
            peer.observe(field(row, "result"));
        }
        assert_eq!(
            JsValue::Array(collector.finish("complete", &|| NOW.into())),
            *field(row, "artifacts"),
            "#742 collector policy {name}"
        );
        if let Some(peer) = peer {
            assert_eq!(
                JsValue::Array(peer.finish("complete", &|| NOW.into())),
                *field(row, "peer"),
                "#742 collector peer {name}"
            );
        }
    }
}

#[tokio::test]
async fn pending_suggestion_drop_and_title_never_finishes() {
    bounded(async {
        let f=Fixture::new();let suggestion_entered=Cancellation::default();let dropped=Arc::new(AtomicBool::new(false));
        struct Mark(Arc<AtomicBool>);impl Drop for Mark {fn drop(&mut self){self.0.store(true,Ordering::SeqCst);}}
        let tail=SessionTail::new(f.recorder.clone(),"session-742".into(),TailConfig {
            auto_generate_title:true,
            title_provider:Some(TitleProvider {provider_id:None,model_provenance:None,generate:Arc::new(|_|Box::pin(std::future::pending()))}),
            suggestion_generator:Some(Arc::new({let entered=suggestion_entered.clone();let dropped=dropped.clone();move |_|{let mark=Mark(dropped.clone());let entered=entered.clone();Box::pin(async move {let _mark=mark;entered.cancel(None);std::future::pending().await})}})),
            ..TailConfig::default()
        });
        let cancel=Cancellation::default();let runner=f.runner(tail.clone(),cancel.clone(),JsValue::Null,"turn-1");
        let (runner,_)=runner.start().await.expect("#742 never start");
        let input=TurnInput {input:super::super::AgentInput::Text("request".into()),history:vec![],synthetic:vec![],metadata:None};
        let (runner,_)=runner.accept(input.clone()).await.expect("#742 never accept");
        let (mut runner,_)=runner.submit_prompt(input.input,cancel).await.expect("#742 never prompt");runner.begin_tail_work();
        let mut recording=Recording::new(runner,Arc::new(()));
        recording.begin(RecordingAction::Complete(json(r#"{"type":"success","sessionId":"session-742","turnId":"turn-1","finalMessage":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#))).expect("#742 never complete begin");
        { let settle=recording.settle();tokio::pin!(settle);tokio::select! {out=&mut settle=>panic!("#742 never suggestion must hold {:?}",out.map(|v|v.len())),_=suggestion_entered.cancelled()=>{}} }
        assert!(f.disk().contains("\"type\":\"turn_result\""),"#742 never durable result before suggestion");
        assert_eq!(f.registry.resolve(None).expect("#742 never registry").lease_count(),1,"#742 never pending turn lease");
        drop(recording);
        assert!(dropped.load(Ordering::SeqCst),"#742 never drop cancels suggestion future");
        assert_eq!(f.registry.resolve(None).expect("#742 never release registry").lease_count(),0,"#742 never drop releases lease");
        tail.dispose();title_done(&tail).await;
    }).await;
}

#[tokio::test]
async fn title_start_after_dispose() {
    let f = Fixture::new();
    let tail = SessionTail::new(
        f.recorder.clone(),
        "session-742".into(),
        TailConfig {
            auto_generate_title: true,
            title_provider: Some(TitleProvider {
                provider_id: None,
                model_provenance: None,
                generate: Arc::new(|_| {
                    panic!("#742 title disposed session must not invoke provider")
                }),
            }),
            ..TailConfig::default()
        },
    );
    tail.dispose();
    tail.begin_title(
        "turn-1".into(),
        "request".into(),
        2,
        Cancellation::default(),
        &JsValue::Null,
    );
    assert!(
        tail.state
            .lock()
            .expect("#742 title disposed state")
            .pending
            .is_none(),
        "#742 title disposed session must not register pending"
    );
}

#[tokio::test]
async fn title_port_reentrance_and_detach_race() {
    bounded(async {
        let f = Fixture::new();
        let slot = Arc::new(Mutex::new(std::sync::Weak::<SessionTail>::new()));
        let started = Cancellation::default();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let tail = SessionTail::new(
            f.recorder.clone(),
            "session-742".into(),
            TailConfig {
                auto_generate_title: true,
                title_provider: Some(TitleProvider {
                    provider_id: None,
                    model_provenance: None,
                    generate: Arc::new({
                        let slot = slot.clone();
                        let gate = gate.clone();
                        let started = started.clone();
                        move |_| {
                            let tail = slot
                                .lock()
                                .expect("#742 title weak lock")
                                .upgrade()
                                .expect("#742 title live session");
                            assert!(
                                tail.state.try_lock().is_ok(),
                                "#742 title port outside state lock"
                            );
                            let _snapshot = tail.snapshot();
                            let gate = gate.clone();
                            let started = started.clone();
                            Box::pin(async move {
                                started.cancel(None);
                                let permit = gate.acquire().await.expect("#742 title gate");
                                permit.forget();
                                Ok(Some("late".into()))
                            })
                        }
                    }),
                }),
                ..TailConfig::default()
            },
        );
        *slot.lock().expect("#742 title weak install") = Arc::downgrade(&tail);
        let cancel = Cancellation::default();
        tail.begin_title(
            "turn-1".into(),
            "request".into(),
            2,
            cancel.clone(),
            &JsValue::Null,
        );
        bounded(started.cancelled()).await;
        let pending = tail
            .state
            .lock()
            .expect("#742 title pending state")
            .pending
            .clone()
            .expect("#742 title pending");
        // The title task has polled the provider and is asleep inside select.
        // Make both unlink and source cancellation ready before it is polled again.
        pending.detach();
        cancel.cancel(None);
        gate.add_permits(1);
        title_done(&tail).await;
        assert!(
            !pending.signal.is_cancelled(),
            "#742 title detached source must stay detached"
        );
        assert_eq!(
            tail.snapshot().get("aiTitle"),
            Some(&"late".into()),
            "#742 title detach still saves"
        );
    })
    .await;
}

#[tokio::test]
async fn metadata_write_retains_owner_and_serial_lock() {
    use crate::storage::{StoreResult, TranscriptRead, TranscriptStore};
    struct GateStore {
        disk: JsonlTranscriptStore,
        gate: Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                std::sync::mpsc::Receiver<()>,
            )>,
        >,
    }
    impl TranscriptStore for GateStore {
        fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
            if entry.get("type") == Some(&"session_metadata".into())
                && let Some((started, release)) =
                    self.gate.lock().expect("#742 metadata gate lock").take()
            {
                started.send(()).expect("#742 metadata gate start");
                release
                    .recv_timeout(Duration::from_secs(5))
                    .expect("#742 metadata bounded worker");
            }
            self.disk.append(key, entry)
        }
        fn read(&self, key: &TranscriptKey, limit: Option<u64>) -> StoreResult<TranscriptRead> {
            self.disk.read(key, limit)
        }
    }
    bounded(async {
        let mut f=Fixture::new();let (started,entered)=tokio::sync::oneshot::channel();let (release,wait)=std::sync::mpsc::channel();
        let store=Arc::new(GateStore {disk:JsonlTranscriptStore::new(),gate:Mutex::new(Some((started,wait)))});
        f.recorder=Arc::new(Recorder::new(store,TranscriptKey {project_root:f.root.clone(),pilot_home:f.root.join("home"),session_id:"session-742".into(),transcript_path:f.path.clone()}));
        let tail=SessionTail::new(f.recorder.clone(),"session-742".into(),TailConfig::default());
        let runner=f.runner(tail.clone(),Cancellation::default(),JsValue::Null,"turn-1");let (runner,_)=runner.start().await.expect("#742 metadata start");
        let owner=Arc::new(());let mut recording=Recording::new(runner,owner.clone());
        recording.begin(RecordingAction::Accept(TurnInput {input:super::super::AgentInput::Text("request".into()),history:vec![],synthetic:vec![],metadata:None})).expect("#742 metadata begin");
        {let settle=recording.settle();tokio::pin!(settle);tokio::select!{out=&mut settle=>panic!("#742 metadata did not block {:?}",out.map(|v|v.len())),out=entered=>out.expect("#742 metadata worker entered")}}
        drop(recording);
        assert!(tail.writes.try_lock().is_err(),"#742 metadata cancelled worker retains serial lock");
        assert_eq!(Arc::strong_count(&owner),2,"#742 metadata cancelled worker retains active owner");
        assert_eq!(f.registry.resolve(None).expect("#742 metadata registry").lease_count(),1,"#742 metadata cancelled worker retains lease");
        release.send(()).expect("#742 metadata release");
        bounded(async {while Arc::strong_count(&owner)>1 {tokio::task::yield_now().await;}}).await;
        assert!(tail.writes.try_lock().is_ok(),"#742 metadata worker releases lock");
        assert_eq!(tail.snapshot().get("firstPrompt"),Some(&"request".into()),"#742 metadata completes merge after cancellation");
    }).await;
}
