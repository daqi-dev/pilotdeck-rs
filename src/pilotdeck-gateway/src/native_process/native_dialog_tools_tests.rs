use super::*;
use crate::metadata::object;

#[tokio::test]
async fn native_session_tools_publish_durable_dialogs_and_deliver_all_four_answers() {
    let mut config = JsObject::new();
    config.insert("userDialogKinds".into(), JsValue::Array(["input", "select", "confirm", "form"].map(JsValue::from).to_vec()));
    let (_scratch, tools, recorder, identity) = make_tools(config, Arc::default(), RuleSetHandle::empty());
    let bus = Arc::new(std::sync::Mutex::new(crate::user_dialog::GatewayUserDialogBus::connected(Default::default())));
    let dialog = crate::native_process::assembly::user_dialog::prepare_dialogs(
        tools.options.home.clone(), tools.options.project.clone(), identity.session_id.clone(), false, bus.clone()).await.unwrap();
    let lifecycle = dialog.port.clone();
    let chat = crate::native_process::methods::session_dir::bounded_chat_dir(&tools.options.home, &tools.options.project).unwrap();
    let journal = chat.join("session.jsonl.dialogs.json");
    let observed = Arc::new(std::sync::Mutex::new(Vec::new())); let events = observed.clone(); let persisted = journal.clone();
    let signal = Cancellation::new();
    let ports = tools.with_user_dialog(Some(dialog)).turn_with_authority(&identity, recorder, signal.clone(), Some(TurnInteractionAuthority {
        permission_mode: None, base_permission_mode: None, can_prompt: Some(false), policy_can_prompt: None,
        sdk_hooks: None, sdk_permission_mode: None,
        emit: Arc::new(move |event| {
            if event.get("type") == Some(&"user_dialog_request".into()) {
                let bytes = std::fs::read_to_string(&persisted).expect("journal precedes the emitted dialog");
                let state = pilotdeck_protocol::parse_json(&bytes).unwrap();
                assert!(matches!(state.get("dialogs"),Some(JsValue::Array(rows)) if rows.iter().any(|row| row.get("requestId") == event.get("requestId"))));
                events.lock().unwrap().push(event);
            }
            true
        }),
    }));
    let mut router = crate::host::modules::ModuleRouter::default();
    let permission = InteractionPermission::default().bind_turn(identity.session_id.clone(), identity.run_id,
        ports.interaction.permission, ports.tool_execution.clone(), &mut router).unwrap();
    ports.tool_execution.bind_turn_permission(permission.clone()).unwrap();
    let context = ports.tool_context.prepare(JsValue::Undefined).await.unwrap();
    for (index, name, input, answer, field) in [
        (0, "request_user_input", object([("prompt", "Input?".into())]), JsValue::String(JsString::from_utf16(vec![0xd800, 0x4e2d])), "answer"),
        (1, "request_user_choice", object([("prompt", "Choose?".into()), ("options", JsValue::Array(vec![object([("value", "A".into())]), object([("value", "B".into())])]))]), "B".into(), "selected"),
        (2, "request_user_confirmation", object([("prompt", "Confirm?".into())]), JsValue::Bool(false), "confirmed"),
        (3, "request_user_form", object([
            ("prompt", "Form?".into()),
            ("schema", object([
                ("type", "object".into()),
                ("properties", object([("name", object([("type", "string".into())]))])),
            ])),
        ]), object([("name", "value".into())]), "values"),
    ] {
        let id = JsString::from(format!("dialog-call-{index}"));
        let decision = permission.request(name.into(), id.clone(), input.clone(), vec![], signal.clone()).await.unwrap();
        assert_eq!(decision.get("behavior"), Some(&"allow".into()));
        let execution = ports.tool_execution.clone(); let context = context.clone();
        let task = tokio::spawn(async move { execution.execute_all(vec![ToolCall { id, name: name.into(), input }], context).await });
        let request = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(request) = observed.lock().unwrap().get(index).cloned() { break request; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("native registry and context must reach the live dialog channel");
        let id = request.get("requestId").and_then(JsValue::as_string).unwrap();
        let consumed = {
            let mut bus = bus.lock().unwrap();
            assert!((bus.peek(&identity.session_id, id).unwrap().accepts)(&answer));
            bus.consume(&identity.session_id, id, "answered").unwrap()
        };
        if let Some(settled) = consumed.entry.on_settled { settled().unwrap(); }
        (consumed.entry.resolve)(crate::user_dialog::DialogAnswer::Answered(answer.clone()));
        let result = tokio::time::timeout(Duration::from_secs(3), task).await.unwrap().unwrap().unwrap();
        assert_eq!(result[0].get("data").and_then(|v| v.get(field)), Some(&answer));
        assert!(!journal.exists(), "settlement durably removes the pending request");
    }
    signal.cancel(); lifecycle.close().await; permission.close();
    assert_eq!(bus.lock().unwrap().pending_count(&identity.session_id), 0);
}
