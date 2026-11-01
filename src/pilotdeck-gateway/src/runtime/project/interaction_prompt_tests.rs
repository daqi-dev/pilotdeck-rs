//! Real InteractionTurn hook binding and lazy sidecar execution.
use super::*;

async fn run_prompt(mode: &'static str) {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let runtime = registry.resolve(None).expect("#741 prompt live runtime");
    let recorder = test_recorder();
    let started = Arc::new(AtomicUsize::new(0));
    let calls = started.clone();
    let owner = registry.clone();
    let cwd = scratch.0.clone();
    let session = LiveSessionAdapter::with_interaction(
        recorder.clone(),
        Vec::new,
        move |identity, _| {
            let lease = owner.acquire(None).expect("#741 prompt live lease");
            let calls = calls.clone();
            let cwd = cwd.clone();
            Box::pin(async move {
                let mut input = options();
                input.permission.context.cwd = cwd.to_string_lossy().as_ref().into();
                let output = if mode == "block" {
                    r#"{"continue":false,"stopReason":"blocked"}"#
                } else {
                    r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"extra"}}"#
                };
                let wait = if mode.starts_with("pending") {
                    "while [ ! -f release ]; do sleep 0.01; done; "
                } else {
                    ""
                };
                input.hooks = command_hook_settings(&format!(
                    "cat > hook-input.json; printf '%s' \"$$\" > hook-pid; {wait}printf '%s' '{output}'"
                ));
                input.hooks[0].0 = "UserPromptSubmit".into();
                Ok(InteractionHost {
                    client: Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        client("live-payload").await
                    }),
                    lease,
                    router: router(&identity),
                    input,
                })
            })
        },
    );
    let raw = parse_json(r#"{"type":"text","text":"probe"}"#).expect("#741 prompt live input");
    let mut turn = session
        .submit_turn_for_test(raw.clone(), wire_options())
        .expect("#741 prompt live submit");
    let mut events = vec![];
    for expected in ["turn_started", "input_accepted"] {
        let event = bounded_prompt(turn.next())
            .await
            .expect("#741 prompt live next")
            .expect("#741 prompt live preflight");
        assert_eq!(
            event.get("type"),
            Some(&expected.into()),
            "#741 prompt live preflight order"
        );
        events.push(event);
    }
    if mode.starts_with("pending") {
        let mut next = turn.next();
        bounded_prompt(async {loop {
            tokio::select! { result=&mut next=>panic!("#741 prompt live hook escaped: {result:?}"), _=tokio::time::sleep(Duration::from_millis(5))=>{} }
            if std::fs::read_to_string(scratch.0.join("hook-pid")).ok().and_then(|s|s.parse::<i32>().ok()).is_some() {break;}
        }}).await;
        drop(next);
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "#741 prompt live no premature execute"
        );
        // No release file is created: abort/drop must terminate the actual shell.
        if mode == "pending-drop" {
            drop(turn);
        } else {
            session
                .abort(&"stop".into())
                .expect("#741 prompt live abort");
            while let Some(event) = bounded_prompt(turn.next())
                .await
                .expect("#741 prompt live cancelled next")
            {
                events.push(event);
            }
            assert_eq!(
                events
                    .last()
                    .and_then(|e| e.get("result"))
                    .and_then(|r| r.get("stopReason")),
                Some(&"aborted_streaming".into()),
                "#741 prompt live abort terminal"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.get("type") == Some(&"turn_completed".into()))
                    .count(),
                1,
                "#741 prompt live one abort"
            );
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.get("type").cloned())
                    .collect::<Vec<_>>(),
                [
                    "turn_started",
                    "input_accepted",
                    "user_prompt_submitted",
                    "turn_completed"
                ]
                .map(|kind| Some(kind.into())),
                "#741 prompt live cancelled sequence"
            );
            assert_eq!(
                events
                    .last()
                    .expect("#741 prompt live cancelled terminal")
                    .get("messages"),
                Some(&JsValue::Array(vec![crate::metadata::object([
                    ("role", "user".into()),
                    (
                        "content",
                        JsValue::Array(vec![crate::metadata::object([
                            ("type", "text".into()),
                            ("text", "probe".into()),
                        ])])
                    ),
                ])])),
                "#741 prompt live cancelled messages exclude unfinished hook"
            );
            drop(turn);
        }
    } else {
        if mode == "context-abort" {
            let event = bounded_prompt(turn.next())
                .await
                .expect("#741 prompt live submitted next")
                .expect("#741 prompt live submitted event");
            assert_eq!(
                event.get("type"),
                Some(&"user_prompt_submitted".into()),
                "#741 prompt live submitted before abort"
            );
            events.push(event);
            session
                .abort(&"stop".into())
                .expect("#741 prompt live context abort");
        }
        while let Some(event) = bounded_prompt(turn.next())
            .await
            .expect("#741 prompt live next")
        {
            events.push(event);
        }
        assert_eq!(
            events
                .get(2)
                .expect("#741 prompt live submitted event")
                .get("type"),
            Some(&"user_prompt_submitted".into()),
            "#741 prompt live submitted"
        );
        assert_eq!(
            started.load(Ordering::SeqCst),
            usize::from(mode == "pass"),
            "#741 prompt live execute gate"
        );
        let result = events.last().expect("#741 prompt live terminal");
        if mode == "block" {
            assert_eq!(
                result.get("result").and_then(|r| r.get("type")),
                Some(&"error".into()),
                "#741 prompt live blocked result"
            );
        } else {
            let JsValue::Array(messages) = result
                .get("messages")
                .expect("#741 prompt live loop payload")
            else {
                panic!("#741 prompt live messages array")
            };
            assert_eq!(messages.len(), 2, "#741 prompt live context forwarded");
            assert_eq!(
                messages
                    .get(1)
                    .expect("#741 prompt live context message")
                    .get("metadata")
                    .and_then(|m| m.get("purpose")),
                Some(&"hook_context".into()),
                "#741 prompt live context"
            );
        }
        drop(turn);
    }
    assert_eq!(
        started.load(Ordering::SeqCst),
        usize::from(mode == "pass"),
        "#741 prompt live spawn count"
    );
    bounded_prompt(async {
        while runtime.lease_count() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let pid = std::fs::read_to_string(scratch.0.join("hook-pid"))
        .expect("#741 prompt live hook pid")
        .parse::<i32>()
        .expect("#741 prompt live pid integer");
    let pid = rustix::process::Pid::from_raw(pid).expect("#741 prompt live pid positive");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rustix::process::test_kill_process(pid) {
                Err(rustix::io::Errno::SRCH) => break,
                Ok(()) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("#741 prompt live process probe: {error}"),
            }
        }
    })
    .await
    .expect("#741 prompt live hook reaped");
    let observed = parse_json(
        &std::fs::read_to_string(scratch.0.join("hook-input.json"))
            .expect("#741 prompt live recorded input"),
    )
    .expect("#741 prompt live hook JSON");
    assert_eq!(
        observed.get("transcriptPath"),
        Some(&JsValue::String(
            recorder.writer().path().to_string_lossy().as_ref().into()
        )),
        "#741 prompt live transcript path"
    );
    assert_eq!(
        observed.get("prompt"),
        Some(&"probe".into()),
        "#741 prompt live original prompt"
    );
    assert_eq!(
        observed
            .as_object()
            .expect("#741 prompt live object")
            .entries()
            .len(),
        5,
        "#741 prompt live host input fields"
    );
    let bytes =
        std::fs::read_to_string(recorder.writer().path()).expect("#741 prompt live transcript");
    assert_eq!(
        bytes
            .lines()
            .filter(
                |l| parse_json(l).expect("#741 prompt live entry").get("type")
                    == Some(&"turn_result".into())
            )
            .count(),
        usize::from(mode != "pending-drop"),
        "#741 prompt live complete once"
    );
    let successor = session
        .submit(raw, wire_options_for("next"))
        .expect("#741 prompt live slot released");
    drop(successor);
    bounded_prompt(registry.dispose())
        .await
        .expect("#741 prompt live registry cleanup");
}
async fn bounded_prompt<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("#741 prompt live bound")
}

/// `completed_event` stamps the bound module id into the metadata of
/// non-durable terminal messages (TS markModuleOwnedEvent); that projection
/// is orthogonal to what the execute-snapshot test below pins, so both sides
/// are compared with it stripped. An emptied metadata object is dropped
/// entirely: the captured payload carries no metadata on the bare input
/// message.
fn stripped_of_module_id(messages: &JsValue) -> JsValue {
    let JsValue::Array(entries) = messages else {
        panic!("#1220 prompt live messages array");
    };
    JsValue::Array(
        entries
            .iter()
            .map(|message| {
                let Some(object) = message.as_object() else {
                    return message.clone();
                };
                let mut rebuilt = JsObject::new();
                for (name, value) in object.entries() {
                    if name.equals_str("metadata")
                        && let Some(metadata) = value.as_object()
                    {
                        let mut fields = JsObject::new();
                        for (field, item) in metadata.entries() {
                            if !field.equals_str("moduleId") {
                                fields.insert(field.clone(), item.clone());
                            }
                        }
                        if !fields.entries().is_empty() {
                            rebuilt.insert(name.clone(), JsValue::Object(fields));
                        }
                        continue;
                    }
                    rebuilt.insert(name.clone(), value.clone());
                }
                JsValue::Object(rebuilt)
            })
            .collect(),
    )
}

/// #1220 fix-r3: the aborted terminal of a hooked interaction turn reports
/// exactly the execute-time payload. The turn's execute already happened
/// when the cancel arrives, so the synthesized aborted terminal must be
/// built from the execute snapshot (live.rs `execute_messages`) — including
/// the UserPromptSubmit hook's injected context message, never the bare
/// loop input and never the in-turn durable assistant message the peer
/// streams afterwards. The peer itself captures the payload it received at
/// execute time and echoes it to a file, so the comparison is independent
/// of the projection path under test.
#[tokio::test]
async fn prompt_host_aborted_terminal_reports_the_execute_payload_with_hook_context() {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let runtime = registry.resolve(None).expect("#1220 prompt live runtime");
    let started = Arc::new(AtomicUsize::new(0));
    let calls = started.clone();
    let owner = registry.clone();
    let cwd = scratch.0.clone();
    let peer_scratch = scratch.0.clone();
    let session = LiveSessionAdapter::with_interaction(
        test_recorder(),
        Vec::new,
        move |identity, _| {
            let lease = owner.acquire(None).expect("#1220 prompt live lease");
            let calls = calls.clone();
            let cwd = cwd.clone();
            let peer_scratch = peer_scratch.clone();
            Box::pin(async move {
                let mut input = options();
                input.permission.context.cwd = cwd.to_string_lossy().as_ref().into();
                input.hooks = command_hook_settings(
                    "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"UserPromptSubmit\",\"additionalContext\":\"extra\"}}'",
                );
                input.hooks[0].0 = "UserPromptSubmit".into();
                Ok(InteractionHost {
                    client: Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        client_with_arg("live-prompt-compaction", &peer_scratch.to_string_lossy())
                            .await
                    }),
                    lease,
                    router: router(&identity),
                    input,
                })
            })
        },
    );
    let raw = parse_json(r#"{"type":"text","text":"probe"}"#).expect("#1220 prompt live input");
    let mut turn = session
        .submit_turn_for_test(raw, wire_options())
        .expect("#1220 prompt live submit");
    for expected in [
        "turn_started",
        "input_accepted",
        "user_prompt_submitted",
        "model_request_started",
        "assistant_message",
    ] {
        let event = bounded_prompt(turn.next())
            .await
            .expect("#1220 prompt live next")
            .expect("#1220 prompt live preflight");
        assert_eq!(
            event.get("type"),
            Some(&expected.into()),
            "#1220 prompt live preflight order"
        );
    }
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "#1220 prompt live execute happened"
    );
    // The peer echoes the exact execute payload atomically once the
    // assistant_message above (published after the echo) was consumed, so
    // the file is already there; read it rather than inferring the payload
    // from consumed events.
    let payload = parse_json(
        &std::fs::read_to_string(scratch.0.join("execute-messages.json"))
            .expect("#1220 prompt live execute payload file"),
    )
    .expect("#1220 prompt live execute payload JSON");
    let JsValue::Array(payload_messages) = &payload else {
        panic!("#1220 prompt live execute payload array");
    };
    assert_eq!(
        payload_messages.len(),
        2,
        "#1220 prompt live execute payload carries the hook context"
    );
    assert_eq!(
        payload_messages
            .get(1)
            .expect("#1220 prompt live context message")
            .get("metadata")
            .and_then(|metadata| metadata.get("purpose")),
        Some(&"hook_context".into()),
        "#1220 prompt live execute payload carries the hook context"
    );
    session
        .abort(&"compaction_persistence_failed".into())
        .expect("#1220 prompt live abort");
    let terminal = bounded_prompt(turn.next())
        .await
        .expect("#1220 prompt live terminal next")
        .expect("#1220 prompt live aborted terminal");
    assert_eq!(
        terminal.get("type"),
        Some(&"turn_completed".into()),
        "#1220 prompt live aborted terminal type"
    );
    assert_eq!(
        terminal.get("result").and_then(|result| result.get("type")),
        Some(&"aborted".into()),
        "#1220 prompt live aborted terminal result"
    );
    assert_eq!(
        stripped_of_module_id(
            terminal
                .get("messages")
                .expect("#1220 prompt live terminal messages")
        ),
        stripped_of_module_id(&payload),
        "#1220 prompt live aborted terminal reports exactly the execute payload messages"
    );
    assert!(
        matches!(bounded_prompt(turn.next()).await, Ok(None)),
        "#1220 prompt live one terminal only"
    );
    drop(turn);
    bounded_prompt(async {
        while runtime.lease_count() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    bounded_prompt(registry.dispose())
        .await
        .expect("#1220 prompt live registry cleanup");
}
macro_rules! case {
    ($name:ident,$mode:literal) => {
        #[tokio::test]
        async fn $name() {
            // Total bound also covers a broken stream that keeps returning events.
            tokio::time::timeout(Duration::from_secs(30), run_prompt($mode))
                .await
                .expect("#741 prompt live total bound");
        }
    };
}
case!(prompt_host_pass, "pass");
case!(prompt_host_context_abort, "context-abort");
case!(prompt_host_block, "block");
case!(prompt_host_abort_pending, "pending-abort");
case!(prompt_host_drop_pending, "pending-drop");
