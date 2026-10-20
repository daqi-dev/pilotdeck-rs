//! Production Rust turn runner -> gateway recovery/compaction -> provider retry.
//! The wire exchange stays in memory; no sockets or extra dependency are needed.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::{
    host::{
        HostError, HostFuture,
        modules::{
            budget,
            context::{self, ContextInput, ContextRuntime, Operation},
            model::{
                self, ModelExecution, ModelFuture, ModelStream, PrepareInput, PreparedInvocation,
                StreamFuture, StreamInput,
            },
            tool_context::*,
        },
    },
    runtime::{
        context::{
            DefaultContextRuntime, Options,
            compaction::{
                engine::{CompactionEngine, CompactionEngineOptions, port::*},
                policy::AutoCompactionPolicy,
                recovery::ContextOverflowRecovery,
            },
        },
        model::token_accounting::TokenBudgetManager,
    },
};
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, admission::SidecarBinding, parse_json, stringify_json,
};
use pilotdeck_transport::{
    default_factory::OriginalAgentFactory,
    server::{ExecutionClock, ExecutionServer},
    stdio::ConnectionOwner,
};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
const ORACLE: &str = include_str!("fixtures/compaction-orchestrator/oracle.json");
const SUMMARY: &str = "## Objective\nImplement the task.\n## Current State\nSource checked.\n## Remaining\nRun tests.\n## Files And Artifacts\nsrc/main.rs";
static UNDEFINED: JsValue = JsValue::Undefined;
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn a(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(a) = v {
        a
    } else {
        panic!("array expected: {v:?}")
    }
}
fn s(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn obj(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn row(values: impl IntoIterator<Item = JsValue>) -> JsValue {
    JsValue::Array(values.into_iter().collect())
}
#[derive(Default)]
struct State {
    main: AtomicUsize,
    summary: AtomicUsize,
    trace: Mutex<Vec<JsValue>>,
}
impl State {
    fn trace(&self, v: JsValue) {
        self.trace.lock().unwrap().push(v);
    }
    fn snapshot(&self) -> JsValue {
        let ratio = if self.main.load(Ordering::SeqCst) == 0 {
            0.01
        } else if self.summary.load(Ordering::SeqCst) == 0 {
            0.95
        } else {
            0.5
        };
        obj([
            ("tokens", JsValue::Number(ratio * 10000.0)),
            ("maxContextTokens", JsValue::Number(10000.0)),
            ("warningRatio", JsValue::Number(0.8)),
            ("blockingRatio", JsValue::Number(0.9)),
            ("state", if ratio >= 0.9 { "blocking" } else { "ok" }.into()),
            ("ratio", JsValue::Number(ratio)),
        ])
    }
}
impl CompactionBudgetPort for State {
    fn evaluate(&self, _: &[JsValue], _: f64, _: Option<f64>) -> Result<JsValue, HostError> {
        Ok(self.snapshot())
    }
    fn estimate_messages(&self, m: &[JsValue]) -> Result<f64, HostError> {
        CompactionBudgetPort::estimate_messages(&TokenBudgetManager::default(), m)
    }
}
impl budget::RequestBudgetEvaluator for State {
    fn evaluate_request_budget(
        &self,
        _: JsObject,
        _: budget::EvaluationOptions,
    ) -> HostFuture<JsValue> {
        let result = self.snapshot();
        Box::pin(async move { Ok(result) })
    }
}
struct SummaryModel(Arc<State>);
impl ModelExecution for SummaryModel {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "fixture".into(),
                "summary".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        self.0.summary.fetch_add(1, Ordering::SeqCst);
        self.0.trace(row([
            "summary".into(),
            JsValue::Number(a(f(&input.prepared.request, "messages")).len() as f64),
        ]));
        let events = vec![
            obj([("type", "text_delta".into()), ("text", SUMMARY.into())]),
            obj([
                ("type", "message_end".into()),
                ("finishReason", "stop".into()),
            ]),
        ];
        Box::pin(async move { Ok(Box::new(Stream(events.into())) as Box<dyn ModelStream>) })
    }
}
struct Stream(VecDeque<JsValue>);
impl ModelStream for Stream {
    fn next(&mut self) -> StreamFuture<'_, Option<JsValue>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
struct Runtime {
    native: DefaultContextRuntime,
    state: Arc<State>,
}
impl ContextRuntime for Runtime {
    fn supports(&self, _: Operation) -> bool {
        true
    }
    fn prepare_for_model(&self, input: ContextInput) -> HostFuture<JsValue> {
        Box::pin(async move {
            Ok(obj([
                ("messages", input.fields.get("messages").unwrap().clone()),
                ("systemPromptParts", JsValue::Array(vec![])),
                (
                    "tools",
                    input
                        .fields
                        .get("tools")
                        .cloned()
                        .unwrap_or(JsValue::Array(vec![])),
                ),
                ("diagnostics", JsValue::Array(vec![])),
                ("boundaries", JsValue::Array(vec![])),
            ]))
        })
    }
    fn try_auto_compact(&self, input: ContextInput) -> HostFuture<JsValue> {
        self.state.trace(row([
            "compact".into(),
            input
                .fields
                .get("budgetStage")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ]));
        self.native.try_auto_compact(input)
    }
    fn recover_from_model_error(&self, input: ContextInput) -> HostFuture<JsValue> {
        let future = self.native.recover_from_model_error(input);
        let state = self.state.clone();
        Box::pin(async move {
            let result = future.await?;
            state.trace(row(["recovery".into(), result.clone()]));
            Ok(result)
        })
    }
    fn capture_turn(&self, _: ContextInput) -> HostFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Null) })
    }
}
struct Environment;
impl TurnEnvironment for Environment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("no tools")
    }
}
struct Clock;
impl ExecutionClock for Clock {
    fn now_epoch_ms(&self) -> f64 {
        1000.0
    }
    fn now_iso(&self) -> JsString {
        "2026-10-19T00:00:00.000Z".into()
    }
    fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> {
        Ok(None)
    }
}
struct Output {
    sender: mpsc::Sender<JsValue>,
    pending: Vec<u8>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        while let Some(n) = self.pending.iter().position(|b| *b == b'\n') {
            let line = self.pending.drain(..=n).collect::<Vec<_>>();
            let value = parse_json(std::str::from_utf8(&line).unwrap()).unwrap();
            self.sender
                .send(value)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct GateRead(mpsc::Receiver<()>);
impl io::Read for GateRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}
struct Running {
    server: Arc<ExecutionServer>,
    gate: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<pilotdeck_transport::server::ShutdownReport>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.server.retire();
        self.gate.take();
        if let Some(worker) = self.worker.take() {
            assert!(worker.join().unwrap().complete());
        }
    }
}
#[test]
fn actual_turn_recovers_and_retries_through_native_context() {
    let oracle = baseline::oracle(ORACLE);
    let cases = a(f(&oracle, "loopCases"));
    assert_eq!(cases.len(), 2);
    for case in cases {
        let fail = f(case, "failRetry").as_bool().unwrap();
        let state = Arc::new(State::default());
        let mut options = CompactionEngineOptions::new(
            Arc::new(SummaryModel(state.clone())),
            "fixture".into(),
            "summary".into(),
        );
        options.uuid = Some(Arc::new(|| "compact-1".into()));
        let port = CompactionPort {
            budget: Some(state.clone()),
            policy: Some(Arc::new(AutoCompactionPolicy::default())),
            summary: Some(Arc::new(CompactionEngine::new(options))),
            recovery: Some(Arc::new(ContextOverflowRecovery::default())),
            ..CompactionPort::default()
        };
        let native = DefaultContextRuntime::new(Options {
            compaction: Some(Arc::new(port)),
            ..Options::default()
        });
        let runtime = Arc::new(Runtime {
            native,
            state: state.clone(),
        });
        let config = obj([
            ("cwd", "/pdrs-fixture/project".into()),
            ("provider", "fixture".into()),
            ("model", "main".into()),
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
        let context = context::Handler::new(runtime, identity, config, None);
        let budget = budget::Handler {
            ports: Some(budget::Ports {
                evaluate_request_budget: Some(state.clone()),
                ..budget::Ports::default()
            }),
            signal: None,
        };
        let model = model::Handler::default();
        let executor = tokio::runtime::Runtime::new().unwrap();
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "pilotdeck-agent-loop".into(),
                module_instance_id: "instance".into(),
                connection_generation: "generation".into(),
                capabilities_version: "1".into(),
            },
            128,
        );
        let clock = Arc::new(Clock);
        let server = ExecutionServer::new(
            owner.clone(),
            Arc::new(OriginalAgentFactory {
                clock: clock.clone(),
                process_cwd: "/pdrs-fixture/project".into(),
            }),
            clock,
            4,
        );
        let (sender, receiver) = mpsc::channel();
        let (gate, reader) = mpsc::channel();
        let worker_server = server.clone();
        let running = Running {
            server: server.clone(),
            gate: Some(gate),
            worker: Some(std::thread::spawn(move || {
                worker_server.serve(
                    io::BufReader::new(GateRead(reader)),
                    Output {
                        sender,
                        pending: vec![],
                    },
                    Duration::from_secs(2),
                )
            })),
        };
        let execute = obj([
            ("kind", "request".into()),
            ("messageId", "execute".into()),
            ("method", "execute".into()),
            ("runId", "run".into()),
            ("operationId", "op".into()),
            ("requestId", "initial".into()),
            ("sessionId", "s1".into()),
            ("turnId", "t1".into()),
            (
                "payload",
                obj([
                    ("provider", "fixture".into()),
                    ("model", "main".into()),
                    ("cwd", "/pdrs-fixture/project".into()),
                    ("maxContextTokens", JsValue::Number(10000.0)),
                    ("maxOutputTokens", JsValue::Number(1000.0)),
                    ("maxTurns", JsValue::Number(2.0)),
                    ("messages", f(case, "messages").clone()),
                    ("tools", JsValue::Array(vec![])),
                    (
                        "permissionContext",
                        obj([
                            ("mode", "bypassPermissions".into()),
                            ("canPrompt", JsValue::Bool(false)),
                            ("bypassAvailable", JsValue::Bool(true)),
                        ]),
                    ),
                    (
                        "hostModules",
                        obj([
                            (
                                "model",
                                obj([(
                                    "methods",
                                    row(["stream_next".into(), "close_stream".into()]),
                                )]),
                            ),
                            (
                                "context",
                                obj([(
                                    "methods",
                                    row([
                                        "prepare_for_model".into(),
                                        "try_auto_compact".into(),
                                        "recover_from_model_error".into(),
                                        "capture_turn".into(),
                                    ]),
                                )]),
                            ),
                            (
                                "budget",
                                obj([("methods", row(["evaluate_request_budget".into()]))]),
                            ),
                            (
                                "turn",
                                obj([("methods", row(["persist_compaction".into()]))]),
                            ),
                        ]),
                    ),
                ]),
            ),
        ]);
        server.receive(&execute).unwrap();
        let mut events = vec![];
        let mut persisted = 0;
        let mut terminal = None;
        for _ in 0..200 {
            let message = receiver
                .recv_timeout(Duration::from_secs(15))
                .expect("bounded native turn");
            if f(&message, "kind") == &JsValue::from("response") {
                continue;
            }
            if f(&message, "kind") == &JsValue::from("event") {
                if f(&message, "final") == &JsValue::Bool(true) {
                    terminal = Some(message);
                    break;
                }
                events.push(JsValue::String(
                    s(f(&message, "eventType"))
                        .strip_prefix("agent.")
                        .unwrap()
                        .into(),
                ));
                continue;
            }
            let operation = s(f(&message, "operation"));
            let payload = match operation.as_str() {
                "prepare_for_model"
                | "try_auto_compact"
                | "recover_from_model_error"
                | "capture_turn" => JsValue::Object(
                    executor
                        .block_on(context.call(message.clone(), &budget, &model))
                        .unwrap(),
                ),
                "evaluate_request_budget" => obj([("snapshot", state.snapshot())]),
                "persist_compaction" => {
                    persisted += 1;
                    obj([("result", JsValue::Null)])
                }
                "close_stream" => obj([("closed", JsValue::Bool(true))]),
                "stream_next" => {
                    let count = state.main.fetch_add(1, Ordering::SeqCst) + 1;
                    let request = f(f(&message, "payload"), "request");
                    state.trace(row([
                        "model".into(),
                        JsValue::Number(count as f64),
                        JsValue::Number(a(f(request, "messages")).len() as f64),
                    ]));
                    let model_events = if count == 1 {
                        vec![obj([
                            ("type", "error".into()),
                            (
                                "error",
                                obj([
                                    ("provider", "fixture".into()),
                                    ("protocol", "openai".into()),
                                    ("code", "context_overflow".into()),
                                    ("message", "context length exceeded".into()),
                                    ("retryable", JsValue::Bool(false)),
                                    ("recoverableViaCompact", JsValue::Bool(true)),
                                    ("maxContextTokens", JsValue::Number(10000.0)),
                                ]),
                            ),
                        ])]
                    } else if fail {
                        vec![obj([
                            ("type", "error".into()),
                            (
                                "error",
                                obj([
                                    ("provider", "fixture".into()),
                                    ("protocol", "openai".into()),
                                    ("code", "authentication".into()),
                                    ("message", "retry denied".into()),
                                    ("retryable", JsValue::Bool(false)),
                                ]),
                            ),
                        ])]
                    } else {
                        vec![
                            obj([
                                ("type", "message_start".into()),
                                ("role", "assistant".into()),
                            ]),
                            obj([("type", "text_delta".into()), ("text", "recovered".into())]),
                            obj([
                                ("type", "message_end".into()),
                                ("finishReason", "stop".into()),
                            ]),
                        ]
                    };
                    obj([
                        ("events", JsValue::Array(model_events)),
                        ("done", JsValue::Bool(true)),
                    ])
                }
                other => panic!("unexpected {other}: {message:?}"),
            };
            server
                .receive(&obj([
                    ("kind", "response".into()),
                    ("messageId", "ack".into()),
                    ("inReplyTo", f(&message, "messageId").clone()),
                    ("ok", JsValue::Bool(true)),
                    ("payload", payload),
                ]))
                .unwrap();
        }
        let terminal = terminal.expect("turn must terminate");
        let result = f(f(&terminal, "payload"), "result");
        let actual = obj([
            (
                "mainCalls",
                JsValue::Number(state.main.load(Ordering::SeqCst) as f64),
            ),
            (
                "summaryCalls",
                JsValue::Number(state.summary.load(Ordering::SeqCst) as f64),
            ),
            ("trace", JsValue::Array(state.trace.lock().unwrap().clone())),
            ("events", JsValue::Array(events)),
            ("result", f(result, "type").clone()),
            ("stopReason", f(result, "stopReason").clone()),
            ("persisted", JsValue::Number(persisted as f64)),
        ]);
        drop(running);
        assert_eq!(
            stringify_json(&actual).unwrap(),
            stringify_json(f(case, "expected")).unwrap(),
            "failRetry={fail}"
        );
    }
}
