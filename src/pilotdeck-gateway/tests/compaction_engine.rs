//! Real TS engine and durable manual replay; property order is intentional.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::{
    host::{
        Cancellation, HostError, HostFuture, ModuleError,
        modules::{
            budget,
            context::{
                ContextInput, ContextRuntime, Handler as ContextHandler, Operation,
                compact_with_recorder,
            },
            model::{
                ModelExecution, ModelFuture, ModelStream, PrepareInput, PreparedInvocation,
                StreamFuture, StreamInput,
            },
            tool_context::{
                ContextRequestIdentity, HostToolCheckpoint, ToolContextPorts, TurnBinding,
                TurnEnvironment,
            },
            turn::persist_compaction_to_recorder,
        },
    },
    runtime::{
        context::{
            DefaultContextRuntime, Options,
            compaction::engine::{
                CompactionEngine, CompactionEngineOptions, CompactionInput, CompactionLifecycle,
                ProtectedTools, build_post_compact_messages,
                port::{
                    CompactionBudgetPort, CompactionPort, CompactionRunner,
                    project_compaction_budget,
                },
                strip_multimedia::strip_multimedia_from_messages,
                truncate_head, truncate_head_preserving_checkpoint,
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
use tokio::sync::Notify;

const ORACLE: &str = include_str!("fixtures/compaction-engine/oracle.json");
const SUMMARY: &str = "## Objective\nImplement the task.\n## Current State\nSource checked.\n## Remaining\nRun tests.\n## Files And Artifacts\nsrc/main.rs";
static UNDEFINED: JsValue = JsValue::Undefined;
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn s(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn a(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(a) = v {
        a
    } else {
        panic!("array expected")
    }
}
fn text(s: &str) -> JsValue {
    JsValue::String(s.into())
}
fn obj(items: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (k, v) in items {
        out.insert(k.into(), v);
    }
    JsValue::Object(out)
}
fn bytes(v: &JsValue) -> String {
    stringify_json(v).unwrap().unwrap()
}
fn same(actual: &JsValue, expected: &JsValue, name: &str) {
    let actual = bytes(actual);
    let expected = bytes(expected);
    if actual != expected {
        let scratch = Scratch::new("compaction-mismatch");
        // A small useful mismatch, without flooding a model request into logs.
        let offset = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(actual.len().min(expected.len()));
        let start = offset.saturating_sub(100);
        let snippet = |value: &str| {
            String::from_utf8_lossy(
                &value.as_bytes()[start.min(value.len())..(offset + 300).min(value.len())],
            )
            .into_owned()
        };
        panic!(
            "{name}: byte mismatch at {offset}; actual_len={} expected_len={}; actual={} expected={} scratch={}",
            actual.len(),
            expected.len(),
            snippet(&actual),
            snippet(&expected),
            scratch.path().display()
        );
    }
}
fn names(v: &JsValue) -> Option<Vec<String>> {
    match v {
        JsValue::Array(v) => Some(v.iter().map(s).collect()),
        _ => None,
    }
}

#[derive(Clone)]
struct MockModel {
    scenario: JsValue,
    requests: Arc<Mutex<Vec<JsValue>>>,
    pass: Arc<AtomicUsize>,
    usage: bool,
    signal: Arc<Mutex<Option<Cancellation>>>,
}
impl ModelExecution for MockModel {
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
        let index = self.pass.load(Ordering::SeqCst);
        let mode = match f(&self.scenario, "modes") {
            JsValue::Array(modes) => s(&modes[index]),
            _ => f(&self.scenario, "mode")
                .as_string()
                .map(|v| v.to_utf8().unwrap())
                .unwrap_or_else(|| "ok".into()),
        };
        let reply = f(&self.scenario, "reply")
            .as_string()
            .cloned()
            .unwrap_or_else(|| SUMMARY.into());
        let finish = f(&self.scenario, "finishReason")
            .as_string()
            .cloned()
            .unwrap_or_else(|| "stop".into());
        let mut events = VecDeque::new();
        let error = match mode.as_str() {
            "throw" => Some("fixture iterator failure"),
            "timeout" => Some("fixture timeout"),
            _ => None,
        };
        if mode == "error" {
            let error = f(&self.scenario, "errorMessage")
                .as_string()
                .cloned()
                .unwrap_or_else(|| "fixture model failure".into());
            events.push_back(obj([
                ("type", text("error")),
                ("error", obj([("message", JsValue::String(error))])),
            ]));
        } else if error.is_none() {
            let split = 15.min(reply.len_utf16());
            events.push_back(obj([
                ("type", text("thinking_delta")),
                ("text", text("ignored")),
            ]));
            for part in [&reply.units()[..split], &reply.units()[split..]] {
                events.push_back(obj([
                    ("type", text("text_delta")),
                    ("text", JsValue::String(JsString::from_utf16(part.to_vec()))),
                ]));
            }
            if self.usage {
                events.push_back(obj([
                    ("type", text("usage")),
                    (
                        "usage",
                        obj([
                            ("inputTokens", JsValue::Number(101.0)),
                            ("outputTokens", JsValue::Number(22.0)),
                            ("cacheReadTokens", JsValue::Number(3.0)),
                            ("cacheWriteTokens", JsValue::Number(4.0)),
                        ]),
                    ),
                ]));
            }
            events.push_back(obj([
                ("type", text("message_end")),
                ("finishReason", JsValue::String(finish)),
            ]));
        }
        let signal = self.signal.lock().unwrap().clone();
        Box::pin(async move {
            Ok(Box::new(MockStream {
                events,
                error,
                timeout: mode == "timeout",
                signal,
            }) as Box<dyn ModelStream>)
        })
    }
}
struct MockStream {
    events: VecDeque<JsValue>,
    error: Option<&'static str>,
    timeout: bool,
    /// Present only for the abort scenarios. The provider observes the abort
    /// after the first yielded event, so the next engine select takes the
    /// cancellation branch (the TS generator throws on its next step).
    signal: Option<Cancellation>,
}
impl ModelStream for MockStream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move {
            if self.timeout {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                self.timeout = false;
            }
            if let Some(error) = self.error.take() {
                return Err(ModuleError::new(error));
            }
            let next = self.events.pop_front();
            if next.is_some()
                && let Some(signal) = &self.signal
                && !signal.is_cancelled()
            {
                signal.cancel(None);
            }
            Ok(next)
        })
    }
}
struct Lifecycle {
    events: Arc<Mutex<Vec<JsValue>>>,
    fail: bool,
}
impl CompactionLifecycle for Lifecycle {
    fn dispatch(&self, event: &'static str, payload: JsValue) -> HostFuture<()> {
        self.events
            .lock()
            .unwrap()
            .push(obj([("event", text(event)), ("payload", payload)]));
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err(ModuleError::new("fixture lifecycle failure").into())
            } else {
                Ok(())
            }
        })
    }
}
fn abort_signal(scenario: &JsValue, pass: usize) -> Option<Cancellation> {
    let spec = match f(scenario, "abort") {
        JsValue::Array(specs) => specs.get(pass),
        value if !value.is_undefined() => Some(value),
        _ => None,
    }?;
    let cancellation = Cancellation::default();
    if f(spec, "when")
        .as_string()
        .is_some_and(|when| when.equals_str("before"))
    {
        cancellation.cancel(f(spec, "reason").as_string().cloned());
    }
    Some(cancellation)
}
fn input(scenario: &JsValue, signal: Option<Cancellation>) -> CompactionInput {
    let v = f(scenario, "input");
    let mut input = CompactionInput::new("manual", a(f(scenario, "messages")).to_vec());
    input.signal = signal;
    if let JsValue::Array(entries) = f(scenario, "orderedInputEntries") {
        let mut ordered = JsObject::new();
        for entry in entries {
            let entry = a(entry);
            ordered.insert(entry[0].as_string().unwrap().clone(), entry[1].clone());
        }
        let message = input.messages.iter_mut().rev().nth(1).unwrap();
        if let JsValue::Object(message) = message
            && let Some(JsValue::Array(blocks)) = message.get("content")
        {
            let mut block = blocks[0].as_object().unwrap().clone();
            block.insert("input".into(), JsValue::Object(ordered));
            message.insert(
                "content".into(),
                JsValue::Array(vec![JsValue::Object(block)]),
            );
        }
    }
    input.session_id = Some("s1".into());
    input.turn_id = Some("t1".into());
    input.keep_tail_ratio = f(v, "keepTailRatio").as_number();
    input.target_post_tokens = f(v, "targetPostTokens").as_number();
    input.max_output_tokens = f(v, "maxOutputTokens").as_number();
    input.cache_reset = f(v, "cacheReset").as_bool();
    input.user_instruction = f(v, "userInstruction").as_string().cloned();
    input.protected_tool_names = match f(v, "protectedToolNames") {
        JsValue::Null => ProtectedTools::Disabled,
        JsValue::Array(_) => ProtectedTools::Names(names(f(v, "protectedToolNames")).unwrap()),
        _ => ProtectedTools::Default,
    };
    if let JsValue::Array(v) = f(v, "attachments") {
        input.attachments = v.clone();
    }
    if let JsValue::Array(v) = f(v, "hookResults") {
        input.hook_results = v.clone();
    }
    input
}

#[tokio::test]
async fn engine_replays_ts_requests_results_lifecycle_and_events_in_byte_order() {
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "engineCases"));
    assert_eq!(cases.len(), 46);
    for scenario in cases {
        let name = s(f(scenario, "name"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let pass = Arc::new(AtomicUsize::new(0));
        let clock = Arc::new(AtomicUsize::new(1_000_000));
        let signal = Arc::new(Mutex::new(None));
        let model = Arc::new(MockModel {
            scenario: scenario.clone(),
            requests: requests.clone(),
            pass: pass.clone(),
            usage: true,
            signal: signal.clone(),
        });
        let mut options =
            CompactionEngineOptions::new(model, "fixture".into(), "summary-model".into());
        let opts = f(scenario, "options");
        options.system_prompt = f(opts, "systemPrompt").as_string().cloned();
        options.max_output_tokens = f(opts, "maxOutputTokens").as_number();
        options.protected_tool_names = names(f(opts, "protectedToolNames"));
        if let Some(scale) = f(scenario, "estimateScale").as_number() {
            options.estimate_messages =
                Some(Arc::new(move |messages| Ok(messages.len() as f64 * scale)));
        }
        let ids = Arc::new(AtomicUsize::new(0));
        options.uuid = Some(Arc::new(move || {
            format!("compact-{}", ids.fetch_add(1, Ordering::SeqCst) + 1).into()
        }));
        let timer = clock.clone();
        options.clock_ms = Some(Arc::new(move || timer.load(Ordering::SeqCst) as f64));
        options.lifecycle = Some(Arc::new(Lifecycle {
            events: lifecycle.clone(),
            fail: false,
        }));
        let emitter = events.clone();
        options.event_emitter = Some(Arc::new(move |e| emitter.lock().unwrap().push(e)));
        let engine = CompactionEngine::new(options);
        let offsets = match f(scenario, "offsets") {
            JsValue::Array(v) => v.clone(),
            _ => vec![JsValue::Number(0.0)],
        };
        let mut passes = Vec::new();
        for (index, offset) in offsets.iter().enumerate() {
            pass.store(index, Ordering::SeqCst);
            clock.store(
                1_000_000 + offset.as_number().unwrap() as usize,
                Ordering::SeqCst,
            );
            let abort = abort_signal(scenario, index);
            *signal.lock().unwrap() = abort.clone();
            let result = engine.run(input(scenario, abort)).await.unwrap();
            passes.push(obj([
                ("result", result.to_value()),
                (
                    "postMessages",
                    JsValue::Array(build_post_compact_messages(&result)),
                ),
            ]));
        }
        let actual = obj([
            ("passes", JsValue::Array(passes)),
            ("requests", JsValue::Array(requests.lock().unwrap().clone())),
            (
                "lifecycle",
                JsValue::Array(lifecycle.lock().unwrap().clone()),
            ),
            ("events", JsValue::Array(events.lock().unwrap().clone())),
        ]);
        same(&actual, f(scenario, "expected"), &name);
    }
}
#[test]
fn deterministic_projections_replay_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let strip = a(f(&oracle, "stripCases"));
    assert_eq!(strip.len(), 1);
    for case in strip {
        same(
            &JsValue::Array(strip_multimedia_from_messages(a(f(case, "input")))),
            f(case, "expected"),
            "strip",
        );
    }
    let truncate = a(f(&oracle, "truncateCases"));
    assert_eq!(truncate.len(), 7);
    for case in truncate {
        let messages = a(f(case, "messages"));
        let ratio = f(case, "ratio").as_number().unwrap();
        same(
            &obj([
                ("raw", JsValue::Array(truncate_head(messages, ratio))),
                (
                    "checkpoint",
                    JsValue::Array(truncate_head_preserving_checkpoint(messages, ratio)),
                ),
            ]),
            f(case, "expected"),
            "truncate",
        );
    }
    let projection = a(f(&oracle, "projectionCases"));
    assert_eq!(projection.len(), 6);
    for case in projection {
        same(
            &project_compaction_budget(f(case, "input")),
            f(case, "expected"),
            "projection",
        );
    }
}

struct FakeRunner {
    mode: String,
    messages: Vec<JsValue>,
}
struct IdentityEnvironment;
impl TurnEnvironment for IdentityEnvironment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("context identity must not build a tool environment")
    }
}
impl CompactionRunner for FakeRunner {
    fn auto_compact(&self, _input: ContextInput) -> HostFuture<JsValue> {
        let mode = self.mode.clone();
        let messages = self.messages.clone();
        Box::pin(async move {
            if mode == "provider-throw" {
                Err(ModuleError::new("fixture provider failure").into())
            } else if mode == "abort-throw" {
                // Real provider abort text; the durable bracket's catch must
                // persist exactly this message.
                Err(ModuleError::new("This operation was aborted").into())
            } else {
                let mut result = obj([
                    ("type", text("compacted")),
                    (
                        "messages",
                        if mode == "missing-messages" {
                            JsValue::Undefined
                        } else {
                            JsValue::Array(messages)
                        },
                    ),
                    ("tier", text("full")),
                    ("snapshot", obj([])),
                ]);
                if mode != "no-result"
                    && let JsValue::Object(ref mut value) = result
                {
                    value.insert(
                        "result".into(),
                        if mode == "missing-messages" {
                            obj([("summaryMessage", text(SUMMARY))])
                        } else {
                            obj([("error", text("broken"))])
                        },
                    );
                }
                Ok(result)
            }
        })
    }
}
#[tokio::test]
async fn manual_host_and_durable_transcript_replay_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "manualCases"));
    assert_eq!(cases.len(), 12);
    for case in cases {
        let name = s(f(case, "name"));
        let scratch = Scratch::new("manual-compaction");
        let path = scratch.path().join("s1.jsonl");
        let entries = Arc::new(AtomicUsize::new(0));
        let operations = Arc::new(AtomicUsize::new(0));
        let recorder = Arc::new(
            Recorder::with_sources(
                Arc::new(JsonlTranscriptStore::new()),
                TranscriptKey {
                    project_root: "/pdrs-fixture/project".into(),
                    pilot_home: scratch.path().join("home"),
                    session_id: "s1".into(),
                    transcript_path: path.clone(),
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
        );
        recorder.start_turn(&"s1".into(), &"t1".into()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Arc::new(MockModel {
            scenario: obj([("mode", text(&name))]),
            requests: requests.clone(),
            pass: Arc::new(AtomicUsize::new(0)),
            usage: false,
            signal: Arc::new(Mutex::new(None)),
        });
        let mut opts =
            CompactionEngineOptions::new(model, "fixture".into(), "summary-model".into());
        opts.uuid = Some(Arc::new(|| "compact-1".into()));
        if name == "lifecycle-throw" {
            opts.lifecycle = Some(Arc::new(Lifecycle {
                events: Arc::new(Mutex::new(Vec::new())),
                fail: true,
            }));
        }
        let mut port = CompactionPort {
            budget: Some(Arc::new(TokenBudgetManager::default())),
            summary: if name == "no-summary" {
                None
            } else {
                Some(Arc::new(CompactionEngine::new(opts)))
            },
            ..CompactionPort::default()
        };
        if name == "builder-assistant" {
            port.build_post_compact_messages = Arc::new(|_| {
                vec![obj([
                    ("role", text("assistant")),
                    (
                        "content",
                        JsValue::Array(vec![obj([
                            ("type", text("text")),
                            ("text", text("custom replacement")),
                        ])]),
                    ),
                ])]
            });
        } else if name == "builder-empty" {
            port.build_post_compact_messages = Arc::new(|_| Vec::new());
        }
        if [
            "invalid-result",
            "provider-throw",
            "abort-throw",
            "missing-messages",
            "no-result",
        ]
        .contains(&name.as_str())
        {
            port.auto_compact = Some(Arc::new(FakeRunner {
                mode: name.clone(),
                messages: a(f(f(case, "input"), "messages")).to_vec(),
            }));
        }
        let runtime: Arc<dyn ContextRuntime> = Arc::new(DefaultContextRuntime::new(Options {
            compaction: Some(Arc::new(port)),
            ..Options::default()
        }));
        let identity = ContextRequestIdentity::new(
            TurnBinding {
                config: JsObject::new(),
                input: obj([("sessionId", text("s1")), ("turnId", text("t1"))])
                    .as_object()
                    .unwrap()
                    .clone(),
                checkpoint: HostToolCheckpoint::default(),
                abort_signal: None,
                environment: Arc::new(IdentityEnvironment),
            },
            ToolContextPorts::default(),
        );
        let handler = ContextHandler::new(runtime, identity, JsObject::new(), None)
            .with_recorder(recorder.clone());
        assert!(handler.methods().contains(&"try_auto_compact"));
        let outcome = handler
            .call(
                obj([(
                    "payload",
                    obj([
                        ("operation", text("try_auto_compact")),
                        ("input", f(case, "input").clone()),
                    ]),
                )]),
                &budget::Handler::default(),
                &pilotdeck_gateway::host::modules::model::Handler::default(),
            )
            .await
            .map(|value| value.get("result").unwrap().clone());
        let before = std::fs::read_to_string(&path).unwrap();
        let mut response = match outcome {
            Ok(result) => obj([("result", result)]),
            Err(error) => obj([("error", error_text(&error))]),
        };
        if f(f(&response, "result"), "type")
            .as_string()
            .is_some_and(|s| s.equals_str("compacted"))
        {
            let result = f(&response, "result");
            let summary = f(result, "result");
            let boundary = obj([
                ("kind", text("compact")),
                ("subtype", text("compact_boundary")),
                (
                    "compactMetadata",
                    obj([
                        ("compactionId", f(summary, "compactionId").clone()),
                        ("trigger", text("manual")),
                        ("preTokens", f(summary, "preTokens").clone()),
                        ("postTokens", f(summary, "postTokens").clone()),
                        (
                            "messagesSummarized",
                            f(summary, "messagesSummarized").clone(),
                        ),
                    ]),
                ),
            ]);
            persist_compaction_to_recorder(
                recorder,
                "s1".into(),
                "t1".into(),
                Arc::new(|reason| panic!("unexpected abort: {reason}")),
            )(
                boundary.as_object().unwrap().clone(),
                a(f(result, "messages")).to_vec(),
            )
            .await
            .unwrap();
            if let JsValue::Object(ref mut response) = response {
                response.insert("boundary".into(), boundary);
            }
        }
        let actual = obj([
            ("response", response),
            ("requests", JsValue::Array(requests.lock().unwrap().clone())),
            ("beforePersist", text(&before)),
            ("transcript", text(&std::fs::read_to_string(&path).unwrap())),
        ]);
        same(&actual, f(case, "expected"), &format!("manual-{name}"));
    }
}
struct PendingRuntime {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl ContextRuntime for PendingRuntime {
    fn supports(&self, operation: Operation) -> bool {
        operation == Operation::TryAutoCompact
    }
    fn prepare_for_model(&self, _: ContextInput) -> HostFuture<JsValue> {
        panic!("prepare_for_model is not used by the durable compaction bracket")
    }
    fn try_auto_compact(&self, _: ContextInput) -> HostFuture<JsValue> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Err(ModuleError::new("fixture provider failure").into())
        })
    }
}

/// The host aborts module-callback tasks on terminal-drain timeout. A bracket
/// that already wrote `compaction_started` must still record its terminal fact,
/// like the uncancelled TS promise chain in `durableContextRuntime.ts:79-116`.
#[tokio::test]
async fn durable_compaction_failure_terminal_survives_caller_abort() {
    let scratch = Scratch::new("durable-compaction-abort");
    let path = scratch.path().join("s1.jsonl");
    let entries = Arc::new(AtomicUsize::new(0));
    let operations = Arc::new(AtomicUsize::new(0));
    let recorder = Arc::new(
        Recorder::with_sources(
            Arc::new(JsonlTranscriptStore::new()),
            TranscriptKey {
                project_root: "/pdrs-fixture/project".into(),
                pilot_home: scratch.path().join("home"),
                session_id: "s1".into(),
                transcript_path: path.clone(),
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
    );
    recorder.start_turn(&"s1".into(), &"t1".into()).unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime: Arc<dyn ContextRuntime> = Arc::new(PendingRuntime {
        entered: entered.clone(),
        release: release.clone(),
    });
    let input = ContextInput {
        fields: obj([
            ("sessionId", text("s1")),
            ("turnId", text("t1")),
            ("trigger", text("manual")),
            (
                "messages",
                JsValue::Array(vec![obj([("role", text("user"))])]),
            ),
            ("maxContextTokens", JsValue::Number(20_000.0)),
        ])
        .as_object()
        .unwrap()
        .clone(),
        abort_signal: None,
        budget_evaluator: None,
    };
    let task = tokio::spawn(compact_with_recorder(runtime, recorder, input));
    entered.notified().await;
    let started = std::fs::read_to_string(&path).unwrap();
    assert!(
        started.contains("\"type\":\"compaction_started\""),
        "bracket must start before the caller abort: {started}"
    );
    task.abort();
    assert!(
        task.await.unwrap_err().is_cancelled(),
        "the caller task must really be cancelled"
    );
    release.notify_one();
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let transcript = std::fs::read_to_string(&path).unwrap();
            if transcript.contains("\"type\":\"compaction_failed\"")
                && transcript.contains("\"error\":\"fixture provider failure\"")
            {
                break transcript;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("a started durable compaction must still record a terminal fact");
    assert!(
        terminal.contains("\"type\":\"compaction_started\""),
        "{terminal}"
    );
    assert!(
        !terminal.contains("\"type\":\"compaction_completed\""),
        "{terminal}"
    );
}

#[test]
fn budget_port_preserves_ts_manager_configuration() {
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "budgetCases"));
    assert_eq!(cases.len(), 3);
    for case in cases {
        let options = f(case, "options");
        let mut manager = TokenBudgetManager::default();
        if let Some(n) = f(options, "perMessageOverhead").as_number() {
            manager.per_message_overhead = n;
        }
        if let Some(n) = f(options, "multimediaTokens").as_number() {
            manager.multimedia_tokens = n;
        }
        if let Some(n) = f(options, "warningRatio").as_number() {
            manager.warning_ratio = n;
        }
        if let Some(n) = f(options, "blockingRatio").as_number() {
            manager.blocking_ratio = n;
        }
        let actual = CompactionBudgetPort::evaluate(
            &manager,
            a(f(case, "messages")),
            f(case, "context").as_number().unwrap(),
            f(case, "reserved").as_number(),
        )
        .unwrap();
        same(&actual, f(case, "expected"), "configured budget");
    }
}
fn error_text(error: &HostError) -> JsValue {
    JsValue::String(
        error
            .module_error
            .as_ref()
            .map_or_else(|| error.message.clone().into(), |e| e.message.clone()),
    )
}

#[test]
#[ignore = "requires fixed TS checkout and explicit permission for shared provenance guard Git"]
fn generator_twice_is_byte_identical() {
    let baseline = std::env::var("PDRS_BASELINE").expect("PDRS_BASELINE");
    let git = std::env::var("PDRS_GIT").expect("absolute PDRS_GIT");
    let scratch = Scratch::new("compaction-engine-generate");
    let mut outputs = Vec::new();
    for index in 0..2 {
        let output = scratch.path().join(format!("run-{index}"));
        let result = baseline::node_command()
            .arg("--import")
            .arg(std::path::Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/compaction-engine/generate.mts"),
            )
            .arg(&output)
            .env("PDRS_BASELINE", &baseline)
            .env("PDRS_GIT", &git)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        outputs.push(std::fs::read(output.join("oracle.json")).unwrap());
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], ORACLE.as_bytes());
}
