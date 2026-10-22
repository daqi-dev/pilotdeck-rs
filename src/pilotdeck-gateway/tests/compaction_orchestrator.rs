//! Real TS policy, stage ordering, recovery and durable transcript replay.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            context::{ContextInput, ContextRuntime, compact_with_recorder},
            model::{
                ModelExecution, ModelFuture, ModelStream, PrepareInput, PreparedInvocation,
                StreamFuture, StreamInput,
            },
            turn::persist_compaction_to_recorder,
        },
    },
    runtime::{
        context::{
            DefaultContextRuntime, Options,
            compaction::{
                MicroCompactionEngine, MicroCompactionEngineOptions, MicroCompactionInput,
                MicroCompactionResult, SnipCallOptions, SnipEngine, SnipEngineOptions, SnipResult,
                engine::{
                    CompactionEngine, CompactionEngineOptions, CompactionInput, CompactionResult,
                    ProtectedTools, build_post_compact_messages, port::*,
                    truncate_checkpoint_with_change,
                },
                policy::AutoCompactionPolicy,
                recovery::ContextOverflowRecovery,
            },
        },
        model::token_accounting::TokenBudgetManager,
    },
    storage::{JsonlTranscriptStore, TranscriptKey, recorder::Recorder},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use scratch::Scratch;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
const ORACLE: &str = include_str!("fixtures/compaction-orchestrator/oracle.json");
const SUMMARY: &str = "## Objective\nImplement the task.\n## Current State\nSource checked.\n## Remaining\nRun tests.\n## Files And Artifacts\nsrc/main.rs";
static UNDEFINED: JsValue = JsValue::Undefined;
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn a(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = v {
        v
    } else {
        panic!("expected array: {v:?}")
    }
}
fn s(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn flag(v: &JsValue, k: &str) -> bool {
    f(v, k).as_bool() == Some(true)
}
fn obj(items: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in items {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn same(actual: &JsValue, expected: &JsValue, name: &str) {
    let actual = stringify_json(actual).unwrap().unwrap();
    let expected = stringify_json(expected).unwrap().unwrap();
    if actual != expected {
        let offset = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(actual.len().min(expected.len()));
        let snippet = |s: &str| {
            String::from_utf8_lossy(
                &s.as_bytes()[offset.saturating_sub(80).min(s.len())..(offset + 200).min(s.len())],
            )
            .into_owned()
        };
        panic!(
            "{name}: byte offset {offset}, actual={} expected={}",
            snippet(&actual),
            snippet(&expected)
        );
    }
}
fn input(fields: &JsValue) -> ContextInput {
    ContextInput {
        fields: fields.as_object().unwrap().clone(),
        abort_signal: None,
        budget_evaluator: None,
    }
}
fn error(e: HostError) -> JsValue {
    JsValue::String(
        e.module_error
            .map_or_else(|| e.message.into(), |e| e.message),
    )
}
type Trace = Arc<Mutex<Vec<JsValue>>>;

#[test]
fn policy_and_recovery_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let policy = AutoCompactionPolicy::default();
    let cases = a(f(&oracle, "policyCases"));
    assert_eq!(cases.len(), 4);
    for case in cases {
        same(
            &policy
                .evaluate_snapshot(f(case, "snapshot").clone())
                .unwrap(),
            f(case, "expected"),
            "policy",
        );
    }
    let cases = a(f(&oracle, "policyBudgetCases"));
    assert_eq!(cases.len(), 4);
    for case in cases {
        same(
            &policy
                .evaluate(
                    a(f(case, "messages")),
                    f(case, "context").as_number().unwrap(),
                    f(case, "reserved").as_number(),
                )
                .unwrap(),
            f(case, "expected"),
            "policy-budget",
        );
    }
    let cases = a(f(&oracle, "recoveryCases"));
    assert_eq!(cases.len(), 41);
    for case in cases {
        let options = f(case, "options");
        let recovery = ContextOverflowRecovery {
            truncate_first_keep_ratio: f(options, "truncateFirstKeepRatio")
                .as_number()
                .unwrap_or(0.5),
            truncate_second_keep_ratio: f(options, "truncateSecondKeepRatio")
                .as_number()
                .unwrap_or(0.25),
        };
        same(
            &recovery.decide(f(case, "input").clone()).unwrap(),
            f(case, "expected"),
            "recovery",
        );
    }
    let cases = a(f(&oracle, "malformedRecoveryCases"));
    assert_eq!(cases.len(), 3);
    for case in cases {
        same(
            &error(
                ContextOverflowRecovery::default()
                    .decide(f(case, "input").clone())
                    .unwrap_err(),
            ),
            f(case, "expected"),
            "malformed-recovery",
        );
    }
    let cases = a(f(&oracle, "identityCases"));
    assert_eq!(cases.len(), 2);
    for case in cases {
        let mut messages = a(f(case, "messages")).to_vec();
        if flag(case, "nan")
            && let JsValue::Object(ref mut message) = messages[0]
        {
            message.insert(
                "metadata".into(),
                obj([("value", JsValue::Number(f64::NAN))]),
            );
        }
        let result = truncate_checkpoint_with_change(&messages, 0.1);
        same(
            &obj([
                ("messages", JsValue::Array(result.messages)),
                ("changed", JsValue::Bool(result.changed)),
            ]),
            f(case, "expected"),
            "native-message-identity",
        );
    }
}
#[tokio::test]
async fn runtime_fallback_matches_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let runtime = DefaultContextRuntime::new(Options::default());
    let cases = a(f(&oracle, "fallbackCases"));
    assert_eq!(cases.len(), 20);
    for case in cases {
        same(
            &runtime
                .recover_from_model_error(input(f(case, "input")))
                .await
                .unwrap(),
            f(case, "expected"),
            "fallback",
        );
    }
    same(
        &runtime
            .try_auto_compact(input(&obj([
                ("messages", JsValue::Array(vec![])),
                ("maxContextTokens", JsValue::Number(12.0)),
            ])))
            .await
            .unwrap(),
        f(&oracle, "noProviderResult"),
        "no-provider",
    );
}
#[derive(Clone)]
struct Budget {
    snapshots: Vec<JsValue>,
    index: Arc<AtomicUsize>,
    trace: Trace,
    estimate: Option<f64>,
    evaluator: bool,
}
impl Budget {
    fn next(&self, messages: &[JsValue]) -> JsValue {
        let i = self
            .index
            .fetch_add(1, Ordering::SeqCst)
            .min(self.snapshots.len() - 1);
        let snapshot = self.snapshots[i].clone();
        self.trace.lock().unwrap().push(obj([
            ("stage", "budget".into()),
            ("messages", JsValue::Array(messages.to_vec())),
            ("snapshot", snapshot.clone()),
        ]));
        snapshot
    }
}
impl CompactionBudgetPort for Budget {
    fn evaluate(&self, messages: &[JsValue], _: f64, _: Option<f64>) -> Result<JsValue, HostError> {
        assert!(!self.evaluator, "must use request evaluator");
        Ok(self.next(messages))
    }
    fn estimate_messages(&self, messages: &[JsValue]) -> Result<f64, HostError> {
        self.estimate.map_or_else(
            || CompactionBudgetPort::estimate_messages(&TokenBudgetManager::default(), messages),
            Ok,
        )
    }
}
impl pilotdeck_gateway::host::modules::context::CompactionBudgetEvaluator for Budget {
    fn evaluate(&self, messages: JsValue) -> HostFuture<JsValue> {
        let result = self.next(a(&messages));
        Box::pin(async move { Ok(result) })
    }
}
struct Model {
    modes: Vec<JsValue>,
    index: AtomicUsize,
    requests: Trace,
}
impl ModelExecution for Model {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "fixture".into(),
                "summary-model".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        self.requests.lock().unwrap().push(input.prepared.request);
        let i = self.index.fetch_add(1, Ordering::SeqCst);
        let fail = self
            .modes
            .get(i)
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("error"));
        let events = if fail {
            vec![obj([
                ("type", "error".into()),
                (
                    "error",
                    obj([("message", "fixture summary failure".into())]),
                ),
            ])]
        } else {
            vec![
                obj([("type", "text_delta".into()), ("text", SUMMARY.into())]),
                obj([
                    ("type", "message_end".into()),
                    ("finishReason", "stop".into()),
                ]),
            ]
        };
        Box::pin(async move { Ok(Box::new(Stream(events.into())) as Box<dyn ModelStream>) })
    }
}
struct Stream(VecDeque<JsValue>);
impl ModelStream for Stream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
struct Summary {
    engine: CompactionEngine,
    trace: Trace,
    fail: bool,
}
impl CompactionSummaryPort for Summary {
    fn run(&self, i: CompactionInput) -> HostFuture<CompactionResult> {
        let mut fields = vec![
            ("trigger", JsValue::String(i.trigger.clone())),
            ("messages", JsValue::Array(i.messages.clone())),
        ];
        if let Some(n) = i.keep_tail_ratio {
            fields.push(("keepTailRatio", JsValue::Number(n)));
        }
        fields.extend([
            (
                "effectiveContextTokens",
                JsValue::Number(i.effective_context_tokens.unwrap()),
            ),
            (
                "targetPostTokens",
                JsValue::Number(i.target_post_tokens.unwrap()),
            ),
        ]);
        if matches!(i.protected_tool_names, ProtectedTools::Disabled) {
            fields.push(("protectedToolNames", JsValue::Null));
        }
        if let Some(n) = i.max_output_tokens {
            fields.push(("maxOutputTokens", JsValue::Number(n)));
        }
        if let Some(b) = i.cache_reset {
            fields.push(("cacheReset", JsValue::Bool(b)));
        }
        fields.extend([
            ("sessionId", JsValue::String(i.session_id.clone().unwrap())),
            ("turnId", JsValue::String(i.turn_id.clone().unwrap())),
        ]);
        self.trace
            .lock()
            .unwrap()
            .push(obj([("stage", "summary".into()), ("input", obj(fields))]));
        let engine = self.engine.clone();
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err(ModuleError::new("fixture summary port failure").into())
            } else {
                engine.run(i).await
            }
        })
    }
}
struct Micro {
    engine: MicroCompactionEngine,
    trace: Trace,
}
impl CompactionMicroPort for Micro {
    fn apply(&self, i: &MicroCompactionInput<'_>) -> Result<MicroCompactionResult, HostError> {
        let mut fields = vec![
            ("stage", "micro".into()),
            ("trimToTokens", JsValue::Number(i.trim_to_tokens.unwrap())),
        ];
        if let Some(n) = i.keep_latest {
            fields.push(("keepLatest", JsValue::Number(n)));
        }
        if i.protected_tool_names == Some(None) {
            fields.push(("protectedToolNames", JsValue::Null));
        }
        self.trace.lock().unwrap().push(obj(fields));
        CompactionMicroPort::apply(&self.engine, i)
    }
}
struct Snip {
    engine: SnipEngine,
    trace: Trace,
}
impl CompactionSnipPort for Snip {
    fn snip(&self, m: &[JsValue], options: SnipCallOptions) -> Result<SnipResult, HostError> {
        self.trace.lock().unwrap().push(obj([
            ("stage", "snip".into()),
            (
                "options",
                obj([(
                    "targetTotalTokens",
                    JsValue::Number(options.target_total_tokens.unwrap()),
                )]),
            ),
        ]));
        CompactionSnipPort::snip(&self.engine, m, options)
    }
}
fn recorder(scratch: &Scratch) -> Arc<Recorder> {
    let entries = AtomicUsize::new(0);
    let operations = AtomicUsize::new(0);
    Arc::new(
        Recorder::with_sources(
            Arc::new(JsonlTranscriptStore::new()),
            TranscriptKey {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: scratch.path().join("home"),
                session_id: "s1".into(),
                transcript_path: scratch.path().join("s1.jsonl"),
            },
            Arc::new(|| Ok("2026-10-19T00:00:00.000Z".into())),
            Arc::new(move || {
                let n = entries.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(format!("{n:08x}-0000-4000-8000-{n:012x}"))
            }),
        )
        .with_operation_ids(Arc::new(move || {
            Ok(format!(
                "op-{}",
                operations.fetch_add(1, Ordering::SeqCst) + 1
            ))
        })),
    )
}
#[tokio::test]
async fn orchestrator_and_durable_transcript_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "orchestrationCases"));
    assert_eq!(cases.len(), 35);
    for case in cases {
        let def = f(case, "definition");
        let name = s(f(def, "name"));
        let trace: Trace = Arc::default();
        let requests: Trace = Arc::default();
        let mut snapshots = a(f(case, "snapshots")).to_vec();
        if let JsValue::Array(ratios) = f(def, "ratios") {
            for (snapshot, ratio) in snapshots.iter_mut().zip(ratios) {
                if ratio.as_string().is_some_and(|s| s.equals_str("NaN"))
                    && let JsValue::Object(snapshot) = snapshot
                {
                    snapshot.insert("tokens".into(), JsValue::Number(f64::NAN));
                    snapshot.insert("ratio".into(), JsValue::Number(f64::NAN));
                }
            }
        }
        let budget = Arc::new(Budget {
            snapshots,
            index: Arc::new(AtomicUsize::new(0)),
            trace: trace.clone(),
            estimate: f(def, "estimate").as_number(),
            evaluator: flag(def, "evaluator"),
        });
        let model = Arc::new(Model {
            modes: if f(def, "modes").is_undefined() {
                vec![]
            } else {
                a(f(def, "modes")).to_vec()
            },
            index: AtomicUsize::new(0),
            requests: requests.clone(),
        });
        let mut options =
            CompactionEngineOptions::new(model, "fixture".into(), "summary-model".into());
        let ids = AtomicUsize::new(0);
        options.uuid = Some(Arc::new(move || {
            format!("compact-{}", ids.fetch_add(1, Ordering::SeqCst) + 1).into()
        }));
        let mut port = CompactionPort {
            budget: (!flag(def, "noBudget"))
                .then(|| budget.clone() as Arc<dyn CompactionBudgetPort>),
            policy: (!flag(def, "noPolicy")).then(|| {
                Arc::new(AutoCompactionPolicy::default()) as Arc<dyn CompactionPolicyPort>
            }),
            summary: (!flag(def, "noSummary")).then(|| {
                Arc::new(Summary {
                    engine: CompactionEngine::new(options),
                    trace: trace.clone(),
                    fail: flag(def, "throwSummary"),
                }) as Arc<dyn CompactionSummaryPort>
            }),
            micro: flag(def, "micro").then(|| {
                Arc::new(Micro {
                    engine: MicroCompactionEngine::new(MicroCompactionEngineOptions::default()),
                    trace: trace.clone(),
                }) as Arc<dyn CompactionMicroPort>
            }),
            snip: flag(def, "snip").then(|| {
                Arc::new(Snip {
                    engine: SnipEngine::new(SnipEngineOptions::default()),
                    trace: trace.clone(),
                }) as Arc<dyn CompactionSnipPort>
            }),
            recovery: Some(Arc::new(ContextOverflowRecovery::default())),
            ..CompactionPort::default()
        };
        let extra = flag(def, "builderMany").then(|| a(f(f(case, "input"), "messages")).to_vec());
        port.build_post_compact_messages = Arc::new(move |r| {
            let mut m = build_post_compact_messages(r);
            if let Some(extra) = &extra {
                m.extend_from_slice(extra);
            }
            m
        });
        let log = trace.clone();
        let clone_truncate = flag(def, "cloneTruncate");
        port.truncate_head_preserving_checkpoint = Arc::new(move |m, r| {
            log.lock().unwrap().push(obj([
                ("stage", "truncate".into()),
                ("keepRatio", JsValue::Number(r)),
            ]));
            if clone_truncate {
                CheckpointTruncation {
                    messages: m.to_vec(),
                    changed: !m.is_empty(),
                }
            } else {
                truncate_checkpoint_with_change(m, r)
            }
        });
        let runtime = Arc::new(DefaultContextRuntime::new(Options {
            compaction: Some(Arc::new(port)),
            ..Options::default()
        }));
        let scratch = Scratch::new("orchestrator-replay");
        let path = scratch.path().join("s1.jsonl");
        let recorder = recorder(&scratch);
        recorder.start_turn(&"s1".into(), &"t1".into()).unwrap();
        let mut recovery = vec![];
        if flag(def, "retry") {
            recovery.push(
                runtime
                    .recover_from_model_error(input(&obj([
                        (
                            "error",
                            obj([
                                ("code", "context_overflow".into()),
                                ("maxContextTokens", JsValue::Number(10000.0)),
                            ]),
                        ),
                        ("hasAttemptedCompact", JsValue::Bool(false)),
                    ])))
                    .await
                    .unwrap(),
            );
        }
        let mut i = input(f(case, "input"));
        if flag(def, "evaluator") {
            i.budget_evaluator = Some(budget);
        }
        let result = compact_with_recorder(runtime.clone(), recorder.clone(), i).await;
        let before = std::fs::read_to_string(&path).unwrap();
        let mut response = match result {
            Ok(r) => obj([("result", r)]),
            Err(e) => obj([("error", error(e))]),
        };
        if let JsValue::Object(ref mut o) = response {
            o.insert("beforePersist".into(), JsValue::String(before.into()));
        }
        let result = f(&response, "result");
        if f(result, "type")
            .as_string()
            .is_some_and(|s| s.equals_str("compacted"))
            && !f(result, "result").is_undefined()
        {
            let r = f(result, "result");
            let trigger = f(f(case, "input"), "trigger");
            let boundary = obj([
                ("kind", "compact".into()),
                ("subtype", "compact_boundary".into()),
                (
                    "compactMetadata",
                    obj([
                        ("compactionId", f(r, "compactionId").clone()),
                        (
                            "trigger",
                            if trigger.is_undefined() {
                                "auto".into()
                            } else {
                                trigger.clone()
                            },
                        ),
                        ("preTokens", f(r, "preTokens").clone()),
                        ("postTokens", f(r, "postTokens").clone()),
                        ("messagesSummarized", f(r, "messagesSummarized").clone()),
                    ]),
                ),
            ]);
            persist_compaction_to_recorder(
                recorder,
                "s1".into(),
                "t1".into(),
                Arc::new(|reason| panic!("{reason}")),
            )(
                boundary.as_object().unwrap().clone(),
                a(f(result, "messages")).to_vec(),
            )
            .await
            .unwrap();
        }
        if flag(def, "retryFails") {
            recovery.push(
                runtime
                    .recover_from_model_error(input(&obj([
                        ("error", obj([("code", "context_overflow".into())])),
                        ("hasAttemptedCompact", JsValue::Bool(true)),
                    ])))
                    .await
                    .unwrap(),
            );
        }
        let actual = obj([
            ("response", response),
            ("trace", JsValue::Array(trace.lock().unwrap().clone())),
            ("requests", JsValue::Array(requests.lock().unwrap().clone())),
            ("recovery", JsValue::Array(recovery)),
            (
                "transcript",
                JsValue::String(std::fs::read_to_string(&path).unwrap().into()),
            ),
        ]);
        same(&actual, f(case, "expected"), &name);
    }
}

#[test]
#[ignore = "requires permission for the shared Git provenance guard"]
fn generator_twice_is_byte_identical() {
    let baseline = std::env::var("PDRS_BASELINE").expect("PDRS_BASELINE");
    let git = std::env::var("PDRS_GIT").expect("PDRS_GIT");
    let scratch = Scratch::new("orchestrator-generation");
    let mut outputs = vec![];
    for i in 0..2 {
        let dir = scratch.path().join(format!("run-{i}"));
        let output = std::process::Command::new("node")
            .arg("--import")
            .arg(std::path::Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/compaction-orchestrator/generate.mts"),
            )
            .arg(&dir)
            .env("PDRS_BASELINE", &baseline)
            .env("PDRS_GIT", &git)
            .env("LANG", "en_US.UTF-8")
            .env_remove("LC_ALL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        outputs.push(std::fs::read(dir.join("oracle.json")).unwrap());
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], ORACLE.as_bytes());
}

struct HostRuntime(Arc<DefaultContextRuntime>);
impl ContextRuntime for HostRuntime {
    fn supports(&self, operation: pilotdeck_gateway::host::modules::context::Operation) -> bool {
        self.0.supports(operation)
    }
    fn prepare_for_model(&self, input: ContextInput) -> HostFuture<JsValue> {
        Box::pin(async move {
            Ok(obj([
                ("messages", input.fields.get("messages").unwrap().clone()),
                ("systemPrompt", "preview".into()),
                ("tools", JsValue::Array(vec![])),
            ]))
        })
    }
    fn try_auto_compact(&self, input: ContextInput) -> HostFuture<JsValue> {
        self.0.try_auto_compact(input)
    }
    fn recover_from_model_error(&self, input: ContextInput) -> HostFuture<JsValue> {
        self.0.recover_from_model_error(input)
    }
}
fn snapshot(ratio: f64) -> JsValue {
    obj([
        ("tokens", JsValue::Number(ratio * 10000.0)),
        ("maxContextTokens", JsValue::Number(10000.0)),
        ("warningRatio", JsValue::Number(0.8)),
        ("blockingRatio", JsValue::Number(0.9)),
        (
            "state",
            if ratio >= 0.9 {
                "blocking"
            } else if ratio >= 0.8 {
                "warning"
            } else {
                "ok"
            }
            .into(),
        ),
        ("ratio", JsValue::Number(ratio)),
    ])
}
struct HostBudget {
    calls: Trace,
    pass: AtomicUsize,
    mode: String,
}
impl pilotdeck_gateway::host::modules::budget::RequestBudgetEvaluator for HostBudget {
    fn evaluate_request_budget(
        &self,
        request: JsObject,
        options: pilotdeck_gateway::host::modules::budget::EvaluationOptions,
    ) -> HostFuture<JsValue> {
        self.calls.lock().unwrap().push(obj([
            ("request", JsValue::Object(request)),
            (
                "options",
                obj([
                    (
                        "maxContextTokens",
                        JsValue::Number(options.max_context_tokens),
                    ),
                    (
                        "reservedOutputTokens",
                        JsValue::Number(options.reserved_output_tokens.unwrap()),
                    ),
                ]),
            ),
        ]));
        let mut result = snapshot(if self.mode == "provider-overflow" {
            1.05
        } else if self.pass.fetch_add(1, Ordering::SeqCst) == 0 {
            0.95
        } else {
            0.5
        });
        if self.mode == "invalid-budget"
            && let JsValue::Object(ref mut o) = result
        {
            o.insert("ratio".into(), JsValue::Number(-1.0));
        }
        Box::pin(async move { Ok(result) })
    }
}
struct Environment;
impl pilotdeck_gateway::host::modules::tool_context::TurnEnvironment for Environment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("context identity does not build the tool environment")
    }
}
#[tokio::test]
async fn dispatcher_budget_branches_drive_native_orchestrator() {
    use pilotdeck_gateway::host::modules::{budget, context::Handler, model, tool_context::*};
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "hostCases"));
    assert_eq!(cases.len(), 6);
    for case in cases {
        let mode = s(f(case, "mode"));
        let model = Arc::new(Model {
            modes: vec![],
            index: AtomicUsize::new(0),
            requests: Arc::default(),
        });
        let mut options =
            CompactionEngineOptions::new(model, "fixture".into(), "summary-model".into());
        let ids = AtomicUsize::new(0);
        options.uuid = Some(Arc::new(move || {
            format!("compact-{}", ids.fetch_add(1, Ordering::SeqCst) + 1).into()
        }));
        let port = CompactionPort {
            budget: Some(Arc::new(Budget {
                snapshots: vec![snapshot(0.01)],
                index: Arc::new(AtomicUsize::new(0)),
                trace: Arc::default(),
                estimate: None,
                evaluator: false,
            })),
            policy: Some(Arc::new(AutoCompactionPolicy::default())),
            summary: Some(Arc::new(CompactionEngine::new(options))),
            recovery: Some(Arc::new(ContextOverflowRecovery::default())),
            ..CompactionPort::default()
        };
        let runtime = Arc::new(HostRuntime(Arc::new(DefaultContextRuntime::new(Options {
            compaction: Some(Arc::new(port)),
            ..Options::default()
        }))));
        let config = obj([
            ("cwd", "/pdrs-fixture/project".into()),
            ("provider", "p".into()),
            ("model", "m".into()),
            ("maxContextTokens", JsValue::Number(10000.0)),
        ])
        .as_object()
        .unwrap()
        .clone();
        let identity = ContextRequestIdentity::new(
            TurnBinding {
                config: config.clone(),
                input: obj([("sessionId", "s1".into()), ("turnId", "t1".into())])
                    .as_object()
                    .unwrap()
                    .clone(),
                checkpoint: HostToolCheckpoint::default(),
                abort_signal: None,
                environment: Arc::new(Environment),
            },
            ToolContextPorts::default(),
        );
        let calls: Trace = Arc::default();
        let handler = Handler::new(runtime, identity, config, None);
        let budget = budget::Handler {
            ports: (mode != "without-budget").then(|| budget::Ports {
                evaluate_request_budget: Some(Arc::new(HostBudget {
                    calls: calls.clone(),
                    pass: AtomicUsize::new(0),
                    mode: mode.clone(),
                })),
                ..budget::Ports::default()
            }),
            signal: None,
        };
        let response = handler
            .call(
                obj([(
                    "payload",
                    obj([
                        ("operation", "try_auto_compact".into()),
                        ("input", f(case, "input").clone()),
                    ]),
                )]),
                &budget,
                &model::Handler::default(),
            )
            .await;
        let response = match response {
            Ok(value) => JsValue::Object(value),
            Err(e) => {
                let e = e.module_error.unwrap();
                obj([
                    ("error", JsValue::String(e.message)),
                    ("code", e.code.map_or(JsValue::Undefined, JsValue::String)),
                ])
            }
        };
        same(
            &obj([
                ("response", response),
                ("calls", JsValue::Array(calls.lock().unwrap().clone())),
            ]),
            f(case, "expected"),
            &mode,
        );
    }
}

// The same owner seam used by session consumers, rather than a second lock in
// CompactionOrchestrator. The guard covers the entire maintenance future.
#[tokio::test]
async fn compaction_maintenance_reentry_matches_real_ts() {
    use pilotdeck_gateway::runtime::{
        project::{DisposeFuture, RuntimeResult},
        session::{LiveSession, LiveSessionHandle},
    };
    struct Session;
    impl LiveSession for Session {
        fn snapshot(&self) -> JsValue {
            JsValue::Undefined
        }
        fn abort(&self, _: &JsString) -> RuntimeResult {
            Ok(())
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }
    let oracle = baseline::oracle(ORACLE);
    let handle = LiveSessionHandle::new(Arc::new(Session), None);
    let first = handle.begin_maintenance().unwrap();
    let second = handle
        .clone()
        .begin_maintenance()
        .err()
        .expect("reject reentry");
    assert_eq!(
        JsValue::String(second.message.into()),
        f(f(&oracle, "admissionProbe"), "handleSecond").clone()
    );
    assert!(handle.begin_activity().is_err());
    drop(first);
    // Both successful completion and an early error release the RAII slot.
    let fail = || -> Result<(), &'static str> {
        let _maintenance = handle.begin_maintenance().unwrap();
        Err("summary failure")
    };
    assert!(fail().is_err());
    drop(handle.begin_maintenance().unwrap());
    let turn = handle.begin_activity().unwrap();
    assert_eq!(
        JsValue::String(handle.begin_maintenance().err().unwrap().message.into()),
        f(f(&oracle, "admissionProbe"), "handleSecond").clone()
    );
    drop(turn);
    drop(handle.begin_maintenance().unwrap());
}

#[tokio::test]
async fn compaction_input_values_match_real_ts() {
    use pilotdeck_gateway::host::modules::context::CompactionBudgetEvaluator;
    struct Evaluator;
    impl CompactionBudgetEvaluator for Evaluator {
        fn evaluate(&self, messages: JsValue) -> HostFuture<JsValue> {
            Box::pin(async move {
                Ok(obj([
                    ("tokens", JsValue::Number(0.0)),
                    ("maxContextTokens", JsValue::Number(100.0)),
                    ("ratio", JsValue::Number(0.0)),
                    ("state", "ok".into()),
                    ("observed", messages),
                ]))
            })
        }
    }
    struct Skip;
    impl CompactionPolicyPort for Skip {
        fn evaluate_snapshot(&self, snapshot: JsValue) -> Result<JsValue, HostError> {
            Ok(obj([("type", "skip".into()), ("snapshot", snapshot)]))
        }
    }
    let oracle = baseline::oracle(include_str!("fixtures/compaction-input/oracle.json"));
    for case in a(f(&oracle, "cases")) {
        let mode = s(f(case, "mode"));
        let port = CompactionPort {
            budget: (mode != "disabled")
                .then(|| Arc::new(TokenBudgetManager::default()) as Arc<dyn CompactionBudgetPort>),
            policy: Some(Arc::new(Skip)),
            ..Default::default()
        };
        let mut call = input(f(case, "input"));
        if mode == "evaluator" {
            call.budget_evaluator = Some(Arc::new(Evaluator));
        }
        let result = port.try_auto_compact(call, 100.0).await;
        let result = match result {
            Ok(value) => obj([("result", value)]),
            Err(e) => obj([("error", error(e))]),
        };
        same(
            &result,
            f(case, "expected"),
            &format!("input {mode}: {:?}", f(case, "input")),
        );
    }
    let runtime = DefaultContextRuntime::new(Options {
        max_context_tokens: 100.0,
        ..Default::default()
    });
    for case in a(f(&oracle, "fallbackCases")) {
        same(
            &runtime
                .try_auto_compact(input(f(case, "input")))
                .await
                .unwrap(),
            f(case, "expected"),
            "fallback input",
        );
    }
    struct Snapshot(JsValue);
    impl CompactionBudgetPort for Snapshot {
        fn evaluate(&self, _: &[JsValue], _: f64, _: Option<f64>) -> Result<JsValue, HostError> {
            Ok(self.0.clone())
        }
        fn estimate_messages(&self, _: &[JsValue]) -> Result<f64, HostError> {
            Ok(0.0)
        }
    }
    struct Trigger;
    impl CompactionPolicyPort for Trigger {
        fn evaluate_snapshot(&self, snapshot: JsValue) -> Result<JsValue, HostError> {
            Ok(obj([("type", "trigger".into()), ("snapshot", snapshot)]))
        }
    }
    struct Summary(Arc<Mutex<JsValue>>);
    impl CompactionSummaryPort for Summary {
        fn run(&self, i: CompactionInput) -> HostFuture<CompactionResult> {
            *self.0.lock().unwrap() = obj([
                (
                    "effective",
                    JsValue::Number(i.effective_context_tokens.unwrap()),
                ),
                ("target", JsValue::Number(i.target_post_tokens.unwrap())),
            ]);
            Box::pin(async { Err(ModuleError::new("stop").into()) })
        }
    }
    for case in a(f(&oracle, "summaryCases")) {
        let captured = Arc::new(Mutex::new(JsValue::Undefined));
        let port = CompactionPort {
            budget: Some(Arc::new(Snapshot(f(case, "snapshot").clone()))),
            policy: Some(Arc::new(Trigger)),
            summary: Some(Arc::new(Summary(captured.clone()))),
            ..Default::default()
        };
        assert!(
            port.try_auto_compact(input(&obj([("messages", JsValue::Array(vec![]))])), 100.0)
                .await
                .is_err()
        );
        same(
            &captured.lock().unwrap(),
            f(case, "expected"),
            "effective context coercion",
        );
    }
    for case in a(f(&oracle, "projectionCases")) {
        same(
            &project_compaction_budget(f(case, "input")),
            f(case, "expected"),
            "shared projection",
        );
    }
}

#[test]
#[ignore = "requires fixed TS baseline and Node 22"]
fn compaction_input_generator_twice_is_byte_identical() {
    baseline::regenerates_identically("compaction-input");
}
