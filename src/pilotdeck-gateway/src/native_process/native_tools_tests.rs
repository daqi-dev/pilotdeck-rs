use super::*;
use crate::host::modules::capability::{ToolContext, ToolExecution};
use crate::runtime::project::interaction_permission::TurnPermission;

struct Harness {
    execution: Arc<dyn ToolExecution>,
    context: Arc<dyn ToolContext>,
    permission: TurnPermission,
    signal: Cancellation,
    root: PathBuf,
    _scratch: Scratch,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.signal.cancel();
        self.permission.close();
    }
}
fn harness(decision: &'static str, mode: &'static str) -> Harness {
    harness_with_sandbox(decision, mode, None)
}
fn harness_with_sandbox(
    decision: &'static str,
    mode: &'static str,
    sandbox: Option<&str>,
) -> Harness {
    harness_with_sdk(decision, mode, sandbox, None)
}
fn harness_with_sdk(
    decision: &'static str,
    mode: &'static str,
    sandbox: Option<&str>,
    sdk: Option<&str>,
) -> Harness {
    harness_with_provider(decision, mode, sandbox, sdk, None)
}
fn harness_with_provider(
    decision: &'static str,
    mode: &'static str,
    sandbox: Option<&str>,
    sdk: Option<&str>,
    provider: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
) -> Harness {
    harness_inner(decision, mode, sandbox, sdk, provider, false)
}
fn harness_inner(
    decision: &'static str,
    mode: &'static str,
    sandbox: Option<&str>,
    sdk: Option<&str>,
    provider: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
    with_hooks: bool,
) -> Harness {
    harness_with_hook_settings(decision, mode, sandbox, sdk, provider, with_hooks, vec![])
}
fn harness_with_hook_settings(
    decision: &'static str,
    mode: &'static str,
    sandbox: Option<&str>,
    sdk: Option<&str>,
    provider: Option<Arc<crate::runtime::tools::sandbox::NativeSandbox>>,
    with_hooks: bool,
    settings: crate::hooks::protocol::HooksSettings,
) -> Harness {
    let coordinator = Arc::new(GatewayInteractionCoordinator::default());
    let answerer = coordinator.clone();
    let answers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = JsObject::new();
    if let Some(mode) = sandbox {
        config.insert("sandboxMode".into(), mode.into());
    }
    let (scratch, tools, recorder, identity) =
        make_tools(config, coordinator, RuleSetHandle::empty());
    let root = tools.options.project.clone();
    let tools = tools.with_permission_mode_source(Arc::new(move || Some(mode.into())));
    let tools = if let Some(provider) = provider {
        tools.with_native_sandbox(provider)
    } else {
        tools
    };
    let signal = Cancellation::new();
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        signal.clone(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            sdk_hooks: None,
            sdk_permission_mode: sdk.map(Into::into),
            can_prompt: None,
            policy_can_prompt: None,
            emit: Arc::new(move |event| {
                answerer.decide_permission(
                    &"session".into(),
                    crate::runtime::interaction::PermissionDecision {
                        request_id: event.get("requestId").unwrap().as_string().unwrap().clone(),
                        decision: if decision == "allow_once" {
                            if answers.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                                "allow".into()
                            } else {
                                "deny".into()
                            }
                        } else {
                            decision.into()
                        },
                        remember: None,
                        reason: None,
                    },
                    None,
                )
            }),
        }),
    );
    let resource = InteractionPermission::default();
    let mut router = crate::host::modules::ModuleRouter::default();
    let catalog: Arc<dyn ToolCatalog> = ports.tool_execution.clone();
    let permission = resource
        .bind_turn(
            identity.session_id.clone(),
            identity.run_id.clone(),
            ports.interaction.permission,
            catalog,
            &mut router,
        )
        .unwrap();
    ports
        .tool_execution
        .bind_turn_permission(permission.clone())
        .unwrap();
    if with_hooks {
        let hooks_resource =
            crate::runtime::project::interaction_hooks::InteractionHooks::new(resource.clone());
        let hooks = hooks_resource
            .bind_turn(
                crate::host::modules::lifecycle::Binding {
                    session_id: identity.session_id.clone(),
                    turn_id: identity.turn_id.clone(),
                    cwd: root.to_string_lossy().as_ref().into(),
                    permission_mode: Some(mode.into()),
                    base_env: None,
                    signal: Some(signal.clone()),
                },
                identity.run_id.clone(),
                settings,
                Some(permission.clone()),
                &mut router,
            )
            .unwrap();
        ports.tool_execution.bind_turn_hooks(hooks).unwrap();
    }
    Harness {
        execution: ports.tool_execution,
        context: ports.tool_context,
        permission,
        signal,
        root,
        _scratch: scratch,
    }
}
async fn invoke(h: &Harness, name: &str, id: &str, input: JsValue) -> JsObject {
    let context = h.context.prepare(JsValue::Undefined).await.unwrap();
    let mut results = h
        .execution
        .execute_all(
            vec![ToolCall {
                id: id.into(),
                name: name.into(),
                input,
            }],
            context,
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    results.remove(0)
}
async fn approved(h: &Harness, name: &str, id: &str, input: JsValue) -> JsObject {
    let answer = h
        .permission
        .request(
            name.into(),
            id.into(),
            input.clone(),
            vec![],
            h.signal.clone(),
        )
        .await
        .unwrap();
    assert_eq!(answer.get("behavior"), Some(&"allow".into()));
    invoke(h, name, id, input).await
}
fn success(result: &JsObject) {
    assert_eq!(result.get("type"), Some(&"success".into()), "{result:?}");
}

#[tokio::test]
async fn native_catalog_has_workspace_write_and_foreground_shell_families() {
    let h = harness("allow", "default");
    let tools = h.execution.list().await.unwrap();
    for name in [
        "read_file",
        "glob",
        "grep",
        "get_current_time",
        "lsp",
        "write_file",
        "edit_file",
        "edit_notebook",
        "bash",
    ] {
        assert!(
            tools
                .iter()
                .any(|t| t.fields().get("name") == Some(&name.into())),
            "missing {name}"
        );
    }
}

#[tokio::test]
async fn real_write_edit_notebook_and_saved_bash_script_use_once_permissions() {
    let h = harness("allow", "default");
    success(
        &approved(
            &h,
            "write_file",
            "create",
            crate::metadata::object([
                ("file_path", "created.txt".into()),
                ("content", "first\n".into()),
            ]),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("created.txt")).unwrap(),
        "first\n"
    );
    success(
        &invoke(
            &h,
            "read_file",
            "read-created",
            crate::metadata::object([("file_path", "created.txt".into())]),
        )
        .await,
    );
    success(
        &approved(
            &h,
            "edit_file",
            "edit",
            crate::metadata::object([
                ("file_path", "created.txt".into()),
                ("old_string", "first".into()),
                ("new_string", "second".into()),
            ]),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("created.txt")).unwrap(),
        "second\n"
    );
    let notebook = r#"{"cells":[{"cell_type":"code","id":"cell-one","source":["1 + 1\n"],"metadata":{},"outputs":[],"execution_count":null}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
    success(
        &approved(
            &h,
            "write_file",
            "notebook",
            crate::metadata::object([
                ("file_path", "probe.ipynb".into()),
                ("content", notebook.into()),
            ]),
        )
        .await,
    );
    success(
        &invoke(
            &h,
            "read_file",
            "read-notebook",
            crate::metadata::object([("file_path", "probe.ipynb".into())]),
        )
        .await,
    );
    success(
        &approved(
            &h,
            "edit_notebook",
            "edit-cell",
            crate::metadata::object([
                ("notebook_path", "probe.ipynb".into()),
                ("cell_id", "cell-one".into()),
                ("new_source", "2 + 2\n".into()),
            ]),
        )
        .await,
    );
    assert!(
        std::fs::read_to_string(h.root.join("probe.ipynb"))
            .unwrap()
            .contains("2 + 2")
    );
    success(
        &approved(
            &h,
            "write_file",
            "script",
            crate::metadata::object([
                ("file_path", "run.sh".into()),
                (
                    "content",
                    "printf BASH-OK > bash.txt\ncat bash.txt\n".into(),
                ),
            ]),
        )
        .await,
    );
    let result = approved(
        &h,
        "bash",
        "shell",
        crate::metadata::object([
            ("command", "bash run.sh".into()),
            ("timeout", JsValue::Number(5000.0)),
        ]),
    )
    .await;
    success(&result);
    assert!(format!("{result:?}").contains("BASH-OK"));
    assert_eq!(
        std::fs::read_to_string(h.root.join("bash.txt")).unwrap(),
        "BASH-OK"
    );
    let replay = invoke(
        &h,
        "bash",
        "shell",
        crate::metadata::object([
            ("command", "bash run.sh".into()),
            ("timeout", JsValue::Number(5000.0)),
        ]),
    )
    .await;
    assert_ne!(replay.get("type"), Some(&"success".into()));
}

#[tokio::test]
async fn deny_and_plan_leave_workspace_unchanged() {
    let h = harness("deny", "default");
    let input = crate::metadata::object([
        ("file_path", "denied.txt".into()),
        ("content", "must not exist".into()),
    ]);
    let answer = h
        .permission
        .request(
            "write_file".into(),
            "deny".into(),
            input.clone(),
            vec![],
            h.signal.clone(),
        )
        .await
        .unwrap();
    assert_eq!(answer.get("behavior"), Some(&"deny".into()));
    let result = invoke(&h, "write_file", "deny", input).await;
    assert_ne!(result.get("type"), Some(&"success".into()));
    assert!(!h.root.join("denied.txt").exists());
    let p = harness("allow", "plan");
    let result = invoke(
        &p,
        "write_file",
        "plan-deny",
        crate::metadata::object([
            ("file_path", "outside-plan.txt".into()),
            ("content", "must not exist".into()),
        ]),
    )
    .await;
    assert_ne!(result.get("type"), Some(&"success".into()));
    assert!(!p.root.join("outside-plan.txt").exists());
}

#[tokio::test]
async fn native_bash_cancellation_drains_the_saved_script_and_its_child() {
    let h = harness("allow", "default");
    success(
        &approved(
            &h,
            "write_file",
            "pending-script",
            crate::metadata::object([
                ("file_path", "pending.sh".into()),
                (
                    "content",
                    "printf STARTED > started.txt\nsleep 30\nprintf LATE > never.txt\n".into(),
                ),
            ]),
        )
        .await,
    );
    let input = crate::metadata::object([
        ("command", "bash pending.sh".into()),
        ("timeout", JsValue::Number(5000.0)),
    ]);
    let answer = h
        .permission
        .request(
            "bash".into(),
            "cancel-shell".into(),
            input.clone(),
            vec![],
            h.signal.clone(),
        )
        .await
        .unwrap();
    assert_eq!(answer.get("behavior"), Some(&"allow".into()));
    let execution = h.execution.clone();
    let context = h.context.prepare(JsValue::Undefined).await.unwrap();
    let task = tokio::spawn(async move {
        execution
            .execute_all(
                vec![ToolCall {
                    id: "cancel-shell".into(),
                    name: "bash".into(),
                    input,
                }],
                context,
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !h.root.join("started.txt").exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "shell never reached owned witness"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.signal.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_ne!(result[0].get("type"), Some(&"success".into()));
    assert!(!h.root.join("never.txt").exists());
}

#[tokio::test]
async fn restricted_profiles_never_fall_back_to_unrestricted_native_effects() {
    for mode in ["read-only", "workspace-write"] {
        // bypass is deliberate: sandbox authority must remain stricter than
        // permission approval and must be checked before native IO.
        let h = harness_with_sandbox("allow", "bypassPermissions", Some(mode));
        success(
            &invoke(
                &h,
                "read_file",
                "seed",
                crate::metadata::object([("file_path", "probe.txt".into())]),
            )
            .await,
        );
        let original = std::fs::read(h.root.join("probe.txt")).unwrap();
        let notebook = r#"{"cells":[{"cell_type":"code","id":"one","source":["old\n"],"metadata":{},"outputs":[],"execution_count":null}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(h.root.join("guard.ipynb"), notebook).unwrap();
        success(
            &invoke(
                &h,
                "read_file",
                "seed-notebook",
                crate::metadata::object([("file_path", "guard.ipynb".into())]),
            )
            .await,
        );
        let cases = [
            (
                "bash",
                crate::metadata::object([("command", "printf BLOCKED > guard-bash.txt".into())]),
            ),
            (
                "write_file",
                crate::metadata::object([
                    ("file_path", "guard-write.txt".into()),
                    ("content", "BLOCKED".into()),
                ]),
            ),
            (
                "edit_file",
                crate::metadata::object([
                    ("file_path", "probe.txt".into()),
                    ("old_string", "PERMISSION-FIXTURE".into()),
                    ("new_string", "BLOCKED".into()),
                ]),
            ),
            (
                "edit_notebook",
                crate::metadata::object([
                    ("notebook_path", "guard.ipynb".into()),
                    ("cell_id", "one".into()),
                    ("new_source", "BLOCKED".into()),
                ]),
            ),
        ];
        for (name, input) in cases {
            let result = invoke(&h, name, "restricted", input).await;
            assert_ne!(
                result.get("type"),
                Some(&"success".into()),
                "{mode}: {name}"
            );
            assert!(
                format!("{result:?}").contains("Native execution-world sandbox is unavailable"),
                "{result:?}"
            );
        }
        assert!(!h.root.join("guard-bash.txt").exists());
        assert!(!h.root.join("guard-write.txt").exists());
        assert_eq!(std::fs::read(h.root.join("probe.txt")).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(h.root.join("guard.ipynb")).unwrap(),
            notebook
        );
    }
}

#[tokio::test]
async fn sdk_permission_mode_controls_real_workspace_write() {
    let allow = harness_with_sdk("deny", "default", None, Some("acceptEdits"));
    let file = allow
        .root
        .join("sdk-allowed.txt")
        .to_string_lossy()
        .into_owned();
    let input = crate::metadata::object([
        ("file_path", file.as_str().into()),
        ("content", "allowed".into()),
    ]);
    success(&invoke(&allow, "write_file", "sdk-allow", input).await);
    assert_eq!(
        std::fs::read_to_string(allow.root.join("sdk-allowed.txt")).unwrap(),
        "allowed"
    );
    let deny = harness_with_sdk("allow", "default", None, Some("dontAsk"));
    let file = deny
        .root
        .join("sdk-denied.txt")
        .to_string_lossy()
        .into_owned();
    let input = crate::metadata::object([
        ("file_path", file.as_str().into()),
        ("content", "denied".into()),
    ]);
    assert_ne!(
        invoke(&deny, "write_file", "sdk-deny", input)
            .await
            .get("type"),
        Some(&"success".into())
    );
    assert!(!deny.root.join("sdk-denied.txt").exists());
}

#[path = "native_background_tools_tests.rs"]
mod background_tool_tests;

#[path = "native_sandbox_tools_tests.rs"]
mod sandbox_provider_tests;

#[tokio::test]
async fn real_permission_lifecycle_prompts_then_consumes_the_single_call_grant() {
    let h = harness_inner("allow_once", "default", None, None, None, true);
    let input = crate::metadata::object([
        ("file_path", "prompted.txt".into()),
        ("content", "approved".into()),
    ]);
    success(&invoke(&h, "write_file", "same-call", input.clone()).await);
    assert_eq!(
        std::fs::read_to_string(h.root.join("prompted.txt")).unwrap(),
        "approved"
    );
    // The second permission callback answers deny; an accidentally retained
    // once grant would execute this identical call without consulting it.
    std::fs::remove_file(h.root.join("prompted.txt")).unwrap();
    let retry = invoke(&h, "write_file", "same-call", input).await;
    assert_ne!(retry.get("type"), Some(&"success".into()));
    assert!(!h.root.join("prompted.txt").exists());
}

#[tokio::test]
async fn tool_lifecycle_blocks_before_execution_and_observes_success_and_failure() {
    let raw = pilotdeck_protocol::parse_json(r#"{"PreToolUse":[{"matcher":"write_file","hooks":[{"type":"command","command":"printf '%s' '{\"decision\":\"block\",\"reason\":\"blocked by hook\"}'"}]}],"PostToolUse":[{"matcher":"read_file","hooks":[{"type":"command","command":"touch post-ran"}]}],"PostToolUseFailure":[{"matcher":"read_file","hooks":[{"type":"command","command":"touch failure-ran"}]}]}"#).unwrap();
    let settings = crate::hooks::config::parse_hooks_config(&raw).settings;
    let h = harness_with_hook_settings(
        "allow",
        "bypassPermissions",
        None,
        None,
        None,
        true,
        settings,
    );
    let write = invoke(
        &h,
        "write_file",
        "write",
        crate::metadata::object([
            ("file_path", "blocked.txt".into()),
            ("content", "bad".into()),
        ]),
    )
    .await;
    assert_eq!(write.get("type"), Some(&"error".into()), "{write:?}");
    assert!(!h.root.join("blocked.txt").exists());
    let read = invoke(
        &h,
        "read_file",
        "read",
        crate::metadata::object([("file_path", "probe.txt".into())]),
    )
    .await;
    success(&read);
    assert!(h.root.join("post-ran").exists());
    let failed = invoke(
        &h,
        "read_file",
        "missing",
        crate::metadata::object([("file_path", "missing.txt".into())]),
    )
    .await;
    assert_eq!(failed.get("type"), Some(&"error".into()));
    assert!(h.root.join("failure-ran").exists());
}

#[tokio::test]
async fn file_changed_observes_real_committed_create_update_and_not_a_rejected_write() {
    let raw = pilotdeck_protocol::parse_json(r#"{"FileChanged":[{"hooks":[{"type":"command","command":"cat >> file-events; printf '\\n' >> file-events"}]}]}"#).unwrap();
    let h = harness_with_hook_settings(
        "allow",
        "bypassPermissions",
        None,
        None,
        None,
        true,
        crate::hooks::config::parse_hooks_config(&raw).settings,
    );
    let write =
        |value| crate::metadata::object([("file_path", "observed.txt".into()), ("content", value)]);
    success(&invoke(&h, "write_file", "create", write("one".into())).await);
    let read = invoke(
        &h,
        "read_file",
        "read",
        crate::metadata::object([("file_path", "observed.txt".into())]),
    )
    .await;
    success(&read);
    success(&invoke(&h, "write_file", "update", write("two".into())).await);
    let rejected = invoke(
        &h,
        "write_file",
        "rejected",
        crate::metadata::object([
            ("file_path", ".git/config".into()),
            ("content", "three".into()),
        ]),
    )
    .await;
    assert_eq!(rejected.get("type"), Some(&"error".into()));
    let events = std::fs::read_to_string(h.root.join("file-events")).unwrap();
    let entries: Vec<_> = events
        .lines()
        .map(|line| pilotdeck_protocol::parse_json(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 2, "{events}");
    assert_eq!(entries[0].get("changeKind"), Some(&"create".into()));
    assert_eq!(entries[1].get("changeKind"), Some(&"update".into()));
    assert_eq!(entries[0].get("relativePath"), Some(&"observed.txt".into()));
}

#[test]
fn turn_and_session_hooks_use_their_distinct_emitters() {
    let (_scratch, tools, recorder, identity) =
        make_tools(JsObject::new(), Arc::default(), RuleSetHandle::empty());
    let turn_events = Arc::new(Mutex::new(Vec::new()));
    let session_events = Arc::new(Mutex::new(Vec::new()));
    let turn_sink = turn_events.clone();
    let session_sink = session_events.clone();
    let tools = tools.with_session_emitter(Arc::new(move |event| {
        session_sink.lock().unwrap().push(event);
        true
    }));
    let ports = tools.turn_with_authority(
        &identity,
        recorder,
        Cancellation::new(),
        Some(TurnInteractionAuthority {
            permission_mode: None,
            base_permission_mode: None,
            can_prompt: Some(true),
            policy_can_prompt: None,
            sdk_permission_mode: None,
            sdk_hooks: None,
            emit: Arc::new(move |event| {
                turn_sink.lock().unwrap().push(event);
                true
            }),
        }),
    );
    let event = crate::metadata::object([
        ("type", "started".into()),
        ("sessionId", JsValue::String(identity.session_id.clone())),
        ("hookName", "h".into()),
        ("hookEvent", "SessionStart".into()),
    ]);
    let event = event.as_object().unwrap();
    ports
        .interaction
        .hook_options
        .as_ref()
        .unwrap()
        .on_projection
        .as_ref()
        .unwrap()
        .project(event)
        .unwrap();
    assert_eq!(turn_events.lock().unwrap().len(), 1);
    assert_eq!(session_events.lock().unwrap().len(), 0);
    ports
        .interaction
        .session_hook_options
        .as_ref()
        .unwrap()
        .on_projection
        .as_ref()
        .unwrap()
        .project(event)
        .unwrap();
    assert_eq!(turn_events.lock().unwrap().len(), 1);
    assert_eq!(session_events.lock().unwrap().len(), 1);
}
