//! Replays every public tool result and approval request in the fixed TS run.
use super::*;
use crate::{
    host::modules::tool_context::ToolRuntimeContext,
    runtime::tools::{
        ExecutableTool, ToolRunContext,
        builtin::plan_todo::{Kind, Tool},
    },
};

fn normalized(value: &JsValue, project: &str) -> serde_json::Value {
    let encoded = pilotdeck_protocol::stringify_json(value).unwrap().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(&encoded.replace(project, "<PROJECT>")).unwrap();
    fn dates(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "createdAt" {
                        *value = "2026-01-02T03:04:05.678Z".into();
                    } else {
                        dates(value);
                    }
                }
            }
            serde_json::Value::Array(values) => values.iter_mut().for_each(dates),
            _ => {}
        }
    }
    dates(&mut value);
    value
}

#[tokio::test]
async fn every_fixed_ts_plan_todo_result_request_and_snapshot_matches_native_service() {
    let oracle =
        pilotdeck_protocol::parse_json(include_str!("../../tests/fixtures/plan-todo/oracle.json"))
            .unwrap();
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let (scratch, mut tools, _, identity) =
        make_tools(JsObject::new(), coordinator.clone(), RuleSetHandle::empty());
    let project = scratch.0.join("project");
    let key = crate::storage::TranscriptKey {
        project_root: project.clone(),
        pilot_home: scratch.0.join("home"),
        session_id: identity.session_id.clone(),
        transcript_path: scratch.0.join("contract.jsonl"),
    };
    let storage = crate::runtime::session::factory::FactoryStorage::new(
        Arc::new(crate::storage::JsonlTranscriptStore::new()),
        key,
    );
    let mut started = JsObject::new();
    started.insert("type".into(), "turn_started".into());
    storage
        .recorder
        .writer()
        .record_session_event_checked(&identity.session_id, &identity.turn_id, started)
        .unwrap();
    tools.binding.recorder = storage.recorder.clone();
    tools.binding.projection = storage.projection.clone();
    let files = crate::native_process::plan_todo::NativePlanFiles::prepare(project.clone())
        .await
        .unwrap();
    std::fs::write(
        project.join(".pilotdeck/plans/sample.md"),
        "# Small plan\n\nFirst step\nSecond step",
    )
    .unwrap();
    let tools = tools.with_plan_files(Arc::new(files));
    let questions = Arc::new(Mutex::new(Vec::new()));
    let emitted = questions.clone();
    let answerer = coordinator.clone();
    let ports = tools.turn_with_authority(
        &identity,
        storage.recorder.clone(),
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: None,
            sdk_models: None,
            can_prompt: Some(true),
            policy_can_prompt: None,
            emit: Arc::new(move |event| {
                let mut question = event.as_object().unwrap().clone();
                let id = question
                    .get("requestId")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .clone();
                question.insert("type".into(), JsValue::Undefined);
                question.insert("requestId".into(), JsValue::Undefined);
                emitted.lock().unwrap().push(JsValue::Object(question));
                answerer.respond_elicitation(
                    &"session".into(),
                    &id,
                    crate::metadata::object([
                        ("type", "answered".into()),
                        (
                            "answers",
                            crate::metadata::object([(
                                "What should happen next?",
                                "execute_plan".into(),
                            )]),
                        ),
                    ]),
                    None,
                )
            }),
        }),
    );
    ports
        .tool_context
        .bind_interaction_profile(crate::runtime::project::InteractionProfile {
            name: crate::runtime::project::InteractionProfileName::Interactive,
            policy_mode: "interactive",
            can_prompt: true,
            question_provider: "gateway",
            permission_provider: "gateway",
        })
        .unwrap();
    let invocation = ports
        .tool_context
        .prepare(JsValue::Undefined)
        .await
        .unwrap();
    let host = invocation
        .runtime
        .downcast::<ToolRuntimeContext>()
        .ok()
        .unwrap();
    let service = host
        .plan_todo
        .as_ref()
        .unwrap()
        .downcast_ref::<crate::host::modules::capability::PlanTodoContext>()
        .unwrap()
        .0
        .clone();
    let JsValue::Array(rows) = oracle.get("results").unwrap() else {
        panic!("rows")
    };
    assert_eq!(rows.len(), 9);
    let project_text = project.to_string_lossy();
    for (index, row) in rows.iter().enumerate() {
        if index == 8 {
            service
                .call(crate::metadata::object([(
                    "payload",
                    crate::metadata::object([
                        ("sessionId", "session".into()),
                        ("turnId", "turn".into()),
                        ("method", "mark_tool_progress".into()),
                        ("toolName", "write_file".into()),
                    ]),
                )]))
                .await
                .unwrap();
        }
        let mut context = ToolRunContext::new(host.clone());
        context.fields.insert(
            "permissionMode".into(),
            if (1..=4).contains(&index) {
                "plan".into()
            } else {
                "default".into()
            },
        );
        let kind = match row
            .get("name")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap()
            .as_str()
        {
            "enter" => Kind::Enter,
            "exit" => Kind::Exit,
            "todo" => Kind::Todo,
            _ => panic!("name"),
        };
        let encoded = pilotdeck_protocol::stringify_json(row.get("input").unwrap())
            .unwrap()
            .unwrap();
        let input =
            pilotdeck_protocol::parse_json(&encoded.replace("<PROJECT>", &project_text)).unwrap();
        let result = match Tool::new(kind).execute(input, context).await {
            Ok(output) => {
                let mut fields = JsObject::new();
                fields.insert("content".into(), JsValue::Array(output.content));
                if let Some(data) = output.data {
                    fields.insert("data".into(), data);
                }
                if let Some(metadata) = output.metadata {
                    fields.insert("metadata".into(), JsValue::Object(metadata));
                }
                crate::metadata::object([("value", JsValue::Object(fields))])
            }
            Err(error) => crate::metadata::object([(
                "error",
                crate::metadata::object([
                    ("name", "PilotDeckToolRuntimeError".into()),
                    ("code", JsValue::String(error.code.into())),
                    ("message", JsValue::String(error.message)),
                ]),
            )]),
        };
        assert_eq!(
            normalized(&result, &project_text),
            normalized(row.get("result").unwrap(), "<PROJECT>"),
            "result row {index}"
        );
        let snapshot = service
            .call(crate::metadata::object([(
                "payload",
                crate::metadata::object([
                    ("sessionId", "session".into()),
                    ("turnId", "turn".into()),
                    ("method", "read".into()),
                ]),
            )]))
            .await
            .unwrap();
        assert_eq!(
            normalized(snapshot.get("snapshot").unwrap(), &project_text),
            normalized(row.get("snapshot").unwrap(), "<PROJECT>"),
            "snapshot row {index}"
        );
    }
    assert_eq!(
        normalized(
            &JsValue::Array(questions.lock().unwrap().clone()),
            &project_text
        ),
        normalized(oracle.get("questions").unwrap(), "<PROJECT>")
    );
}
