use super::*;
use crate::{
    host::{Cancellation, HostFuture, ModuleError, modules::lifecycle::LifecyclePort},
    runtime::hooks::{
        HookRuntime, HookRuntimeOptions, LifecycleDispatchInput, LifecycleDispatchResult,
        LifecycleRuntime, exec::*,
    },
};
use std::future::Future;

#[derive(Clone)]
struct Probe {
    name: String,
    inputs: Arc<Mutex<Vec<JsValue>>>,
    dispatches: Arc<Mutex<Vec<JsValue>>>,
    results: Arc<Mutex<Vec<JsValue>>>,
    signals: Arc<Mutex<Vec<crate::runtime::hooks::Cancellation>>>,
    runtime: LifecycleRuntime,
    gate: Arc<tokio::sync::Notify>,
    entered: Arc<tokio::sync::Notify>,
}
impl Probe {
    fn new(name: &str) -> Self {
        let inputs = Arc::new(Mutex::new(vec![]));
        use crate::hooks::protocol::{HookCommand, HookMatcher};
        let settings = if name == "timeout" {
            crate::hooks::config::parse_hooks_config(
                &parse_json(
                    r#"{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"probe"}]}]}"#,
                )
                .expect("#741 prompt command settings"),
            )
            .settings
        } else {
            vec![(
                "UserPromptSubmit".into(),
                vec![HookMatcher {
                    hooks: vec![HookCommand::Callback {
                        name: "probe".into(),
                        condition: None,
                        status_message: None,
                    }],
                    ..Default::default()
                }],
            )]
        };
        let runtime = LifecycleRuntime::new(HookRuntime::with_options(
            settings,
            HookRuntimeOptions {
                shell: Some(Arc::new(TimedOut(inputs.clone()))),
                ..Default::default()
            },
        ));
        let probe = Self {
            name: name.into(),
            inputs,
            dispatches: Arc::default(),
            results: Arc::default(),
            signals: Arc::default(),
            runtime,
            gate: Arc::default(),
            entered: Arc::default(),
        };
        probe
            .runtime
            .hooks()
            .callback_executor()
            .register(&"probe".into(), Arc::new(probe.clone()))
            .expect("#741 prompt callback registration");
        probe
    }
    fn port(&self) -> PromptHooks {
        PromptHooks {
            port: Arc::new(self.clone()),
            cwd: "/fixture".into(),
            transcript_path: "/fixture/s.jsonl".into(),
        }
    }
}
impl CallbackHookHandler for Probe {
    fn call(&self, input: CallbackInput) -> CallbackFuture {
        self.inputs
            .lock()
            .expect("#741 prompt input lock")
            .push(JsValue::Object(input.hook_input));
        let probe = self.clone();
        Box::pin(async move {
            probe.entered.notify_one();
            if probe.name.ends_with("pending") {
                probe.gate.notified().await;
            }
            if probe.name == "throw" {
                return Err("injected callback".into());
            }
            Ok(CallbackResult::Text(match probe.name.as_str() {
                "block" | "block-complete-fails" => r#"{"continue":false,"stopReason":"blocked"}"#,
                "context" | "abort-after-submitted" => r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"extra"}}"#,
                _ => "{}",
            }.into()))
        })
    }
}
struct TimedOut(Arc<Mutex<Vec<JsValue>>>);
impl ShellPort for TimedOut {
    fn execute(&self, input: ShellRequest) -> ShellFuture {
        assert_eq!(input.timeout_ms, 600_000.0, "#741 prompt timeout budget");
        self.0.lock().expect("#741 prompt shell lock").push(
            parse_json(&input.stdin.to_utf8().expect("#741 prompt stdin UTF8"))
                .expect("#741 prompt stdin JSON"),
        );
        Box::pin(async {
            Ok(ShellResult {
                exit_code: None,
                stdout: vec![],
                stderr: vec![],
                timed_out: true,
            })
        })
    }
}
impl LifecyclePort for Probe {
    fn dispatch(&self, input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
        self.signals
            .lock()
            .expect("#741 prompt signal lock")
            .push(input.signal.clone().expect("#741 prompt signal"));
        self.dispatches
            .lock()
            .expect("#741 prompt dispatch lock")
            .push(object([
                ("event", JsValue::String(input.event.clone())),
                ("baseInput", JsValue::Object(input.base_input.clone())),
                (
                    "payload",
                    JsValue::Object(input.payload.clone().expect("#741 prompt payload")),
                ),
                (
                    "matchQuery",
                    JsValue::String(input.match_query.clone().expect("#741 prompt match query")),
                ),
                ("signal", "<signal>".into()),
            ]));
        assert!(input.env.is_none(), "#741 prompt host environment");
        let probe = self.clone();
        Box::pin(async move {
            if probe.name == "reject" {
                return Err(ModuleError::new("injected dispatch").into());
            }
            let result = probe.runtime.dispatch(input).await?;
            probe
                .results
                .lock()
                .expect("#741 prompt result lock")
                .push(result.to_value());
            Ok(result)
        })
    }
}
fn row(name: &str) -> JsValue {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/turn-prompt-hooks/oracle.json"
    ));
    let JsValue::Array(rows) = oracle.get("rows").expect("#741 prompt rows") else {
        panic!("#741 prompt rows array")
    };
    rows.iter()
        .find(|r| r.get("name") == Some(&name.into()))
        .expect("#741 prompt named row")
        .clone()
}
fn values(v: &JsValue) -> Vec<JsValue> {
    let JsValue::Array(values) = v else {
        panic!("#741 prompt array")
    };
    values.clone()
}
async fn replay_prompt(name: &str, row: JsValue) {
    let fixture = Fixture::new(
        match name {
            "accepted-fails" => "accepted_input",
            "block-complete-fails" => "turn_result",
            _ => "",
        },
        99,
    );
    let probe = Probe::new(name);
    let runner = fixture
        .runner()
        .with_prompt_hooks((name != "none").then(|| probe.port()));
    let mut recording = Recording::new(runner, Arc::new(()));
    let input = TurnInput::from_agent_input(
        row.get("rawInput").expect("#741 prompt raw input"),
        &JsValue::Null,
        values(row.get("history").expect("#741 prompt history")),
        values(row.get("synthetic").expect("#741 prompt synthetic")),
    )
    .expect("#741 prompt input");
    let mut events = vec![];
    let outcome: RecorderResult<()> = async {
        events.extend(advance(&mut recording, RecordingAction::Start).await?);
        events.extend(advance(&mut recording, RecordingAction::Accept(input.clone())).await?);
        if recording
            .runner()
            .expect("#741 prompt runner")
            .is_complete()
        {
            return Ok(());
        }
        events.extend(
            advance(
                &mut recording,
                RecordingAction::SubmitPrompt(input.input, Cancellation::default()),
            )
            .await?,
        );
        let runner = recording.runner().expect("#741 prompt submitted runner");
        if let Some(error) = runner.prompt_block_error() {
            events.extend(advance(&mut recording, RecordingAction::Fail(error)).await?);
        } else {
            assert_eq!(
                JsValue::Array(runner.messages().to_vec()),
                *row.get("loopMessages").expect("#741 prompt loop messages"),
                "#741 prompt loop input"
            );
            let result = values(row.get("events").expect("#741 prompt events"))
                .last()
                .expect("#741 prompt terminal")
                .get("result")
                .expect("#741 prompt result")
                .clone();
            events.extend(advance(&mut recording, RecordingAction::Complete(result)).await?);
        }
        Ok(())
    }
    .await;
    assert_eq!(
        outcome
            .err()
            .map(|e| JsValue::String(e.to_string().into()))
            .unwrap_or(JsValue::Null),
        *row.get("error").expect("#741 prompt error"),
        "#741 prompt rejection"
    );
    let expected: Vec<_> = values(row.get("events").expect("#741 prompt events"))
        .into_iter()
        .filter(|e| e.get("type") != Some(&"model_event".into()))
        .collect();
    assert_eq!(events, expected, "#741 prompt events");
    assert_eq!(
        fs::read_to_string(&fixture.key.transcript_path).expect("#741 prompt transcript"),
        row.get("transcript")
            .expect("#741 prompt transcript oracle")
            .as_string()
            .expect("#741 prompt transcript string")
            .to_utf8()
            .expect("#741 prompt transcript UTF8"),
        "#741 prompt transcript bytes"
    );
    assert_eq!(
        JsValue::Array(probe.inputs.lock().expect("#741 prompt input lock").clone()),
        *row.get("hookInput").expect("#741 prompt hook inputs"),
        "#741 prompt hook input"
    );
    assert_eq!(
        JsValue::Array(
            probe
                .dispatches
                .lock()
                .expect("#741 prompt dispatch lock")
                .clone()
        ),
        *row.get("dispatches").expect("#741 prompt dispatches"),
        "#741 prompt dispatch input"
    );
    // Callback Rust Text and TS Output differ only in diagnostic stdout; compare semantic effects/messages/errors.
    let actual = probe
        .results
        .lock()
        .expect("#741 prompt results lock")
        .clone();
    let expected = values(row.get("hookResults").expect("#741 prompt hook results"));
    assert_eq!(actual.len(), expected.len(), "#741 prompt dispatch count");
    for (a, e) in actual.iter().zip(&expected) {
        for key in ["effects", "messages", "blockingErrors", "nonBlockingErrors"] {
            assert_eq!(a.get(key), e.get(key), "#741 prompt hook result {key}");
        }
    }
    drop(recording);
    probe.runtime.dispose().await;
}
macro_rules! case {
    ($test:ident,$name:literal) => {
        #[tokio::test]
        async fn $test() {
            let row = row($name);
            tokio::time::timeout(Duration::from_secs(5), replay_prompt($name, row))
                .await
                .expect("#741 prompt bounded replay");
        }
    };
}
case!(none, "none");
case!(pass, "pass");
case!(blocks, "blocks");
case!(blocks_coerce, "blocks-coerce");
case!(blocks_null, "blocks-null");
#[tokio::test]
async fn blocks_empty() {
    let f = Fixture::new("", 0);
    let (runner, _) = f.runner().start().await.expect("#936 empty blocks start");
    let input = TurnInput {
        input: AgentInput::Blocks(vec![]),
        history: vec![],
        synthetic: vec![],
        metadata: None,
    };
    let (runner, accepted) = runner
        .accept(input.clone())
        .await
        .expect("#936 empty blocks accepted");
    assert_eq!(
        accepted[0].get("messages"),
        Some(&JsValue::Array(input.accepted_messages())),
        "#936 empty blocks accepted shape"
    );
    let (_, events) = tokio::time::timeout(
        Duration::from_secs(5),
        runner.submit_prompt(input.input, Cancellation::default()),
    )
    .await
    .expect("#936 empty blocks bound")
    .expect("#936 empty blocks submit");
    assert_eq!(
        events[0].get("prompt"),
        Some(&"".into()),
        "#936 empty blocks prompt"
    );
    replay_prompt("blocks-empty", row("blocks-empty")).await;
}
case!(abort_after_submitted, "abort-after-submitted");
case!(context, "context");
case!(block, "block");
case!(callback_throw, "throw");
case!(timeout, "timeout");
case!(reject, "reject");
case!(accepted_failure, "accepted-fails");
case!(block_complete_failure, "block-complete-fails");

async fn pending_prompt(name: &str, row: JsValue) {
    let f = Fixture::new("", 0);
    let probe = Probe::new(name);
    let mut recording = Recording::new(
        f.runner().with_prompt_hooks(Some(probe.port())),
        Arc::new(()),
    );
    advance(&mut recording, RecordingAction::Start)
        .await
        .expect("#741 prompt start");
    advance(&mut recording, RecordingAction::Accept(input()))
        .await
        .expect("#741 prompt accept");
    let cancel = Cancellation::default();
    recording
        .begin(RecordingAction::SubmitPrompt(
            AgentInput::Text("probe".into()),
            cancel.clone(),
        ))
        .expect("#741 prompt begin");
    for _ in 0..2 {
        let mut next = Box::pin(recording.settle());
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending(), "#741 prompt pending");
            std::task::Poll::Ready(())
        })
        .await;
    }
    probe.entered.notified().await;
    assert_eq!(
        row.get("beforeRelease")
            .expect("#741 prompt before release")
            .get("settled"),
        Some(&JsValue::Bool(false)),
        "#741 prompt TS pending"
    );
    assert_eq!(
        probe
            .dispatches
            .lock()
            .expect("#741 prompt dispatch lock")
            .len(),
        1,
        "#741 prompt once"
    );
    if name == "drop-pending" {
        drop(recording);
    } else {
        if name == "abort-pending" {
            cancel.cancel(None);
        } else {
            probe.gate.notify_one();
        }
        let events = recording.settle().await.expect("#741 prompt settle");
        assert_eq!(
            events
                .first()
                .expect("#741 prompt resumed event")
                .get("type"),
            Some(&"user_prompt_submitted".into()),
            "#741 prompt resume"
        );
        assert!(
            recording
                .begin(RecordingAction::SubmitPrompt(
                    AgentInput::Text("probe".into()),
                    cancel
                ))
                .is_ok(),
            "#741 prompt duplicate begin"
        );
        // If the once-only guard is mutated away, release the second callback
        // so the negative control reaches the duplicate assertion, not a timeout.
        probe.gate.notify_one();
        assert!(
            recording.settle().await.is_err(),
            "#741 prompt duplicate rejected"
        );
        drop(recording);
    }
    assert_eq!(
        f.registry
            .resolve(None)
            .expect("#741 prompt runtime")
            .lease_count(),
        0,
        "#741 prompt lease released"
    );
    assert_eq!(
        probe
            .signals
            .lock()
            .expect("#741 prompt signal lock")
            .first()
            .expect("#741 prompt recorded signal")
            .is_aborted(),
        name != "pending",
        "#741 prompt signal on cancel only"
    );
    probe.runtime.dispose().await;
}
macro_rules! pending_case {
    ($test:ident,$name:literal) => {
        #[tokio::test]
        async fn $test() {
            let row = row($name);
            tokio::time::timeout(Duration::from_secs(5), pending_prompt($name, row))
                .await
                .expect("#741 prompt pending bound");
        }
    };
}
pending_case!(pending, "pending");
pending_case!(abort_pending, "abort-pending");
pending_case!(drop_pending, "drop-pending");
#[test]
#[ignore = "requires fixed TS baseline and Node v22.22.0"]
fn regenerates_identically() {
    assert!(
        std::panic::catch_unwind(|| baseline::regenerates_identically("turn-prompt-hooks")).is_ok(),
        "#741 prompt fixture regeneration"
    );
}

#[tokio::test]
async fn before_accept() {
    let f = Fixture::new("", 0);
    let probe = Probe::new("pass");
    assert!(
        f.runner()
            .with_prompt_hooks(Some(probe.port()))
            .submit_prompt(AgentInput::Text("probe".into()), Cancellation::default())
            .await
            .is_err(),
        "#741 prompt requires accepted input"
    );
    assert!(
        probe
            .dispatches
            .lock()
            .expect("#741 prompt dispatch lock")
            .is_empty(),
        "#741 prompt no early dispatch"
    );
    probe.runtime.dispose().await;
}
