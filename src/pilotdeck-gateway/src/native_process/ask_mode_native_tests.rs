use super::*;
#[cfg(unix)]
#[tokio::test]
async fn ask_execution_blocks_bypass_bash_and_file_writes_with_real_agent_mode_positive_control() {
    for mode in ["ask", "agent"] {
        let config = crate::native_process::assembly::value([("runMode", JsValue::from(mode))]);
        let (_scratch, tools, recorder, identity) =
            make_tools(config, Arc::default(), RuleSetHandle::empty());
        let shell_marker = tools.options.project.join("refused-shell.txt");
        let file_marker = tools.options.project.join("refused-write.txt");
        let tools =
            tools.with_permission_mode_source(Arc::new(|| Some("bypassPermissions".into())));
        let ports = tools.turn(&identity, recorder, Cancellation::new());
        assert_eq!(
            ports.interaction.permission.context.mode.as_js_string(),
            &JsString::from("bypassPermissions")
        );
        let invocation = ports
            .tool_context
            .prepare(JsValue::Undefined)
            .await
            .unwrap();
        let result = ports
            .tool_execution
            .execute_all(
                vec![
                    ToolCall {
                        id: "unsafe-bash".into(),
                        name: "Bash".into(),
                        input: crate::metadata::object([(
                            "command",
                            "printf fixture > refused-shell.txt".into(),
                        )]),
                    },
                    ToolCall {
                        id: "unsafe-write".into(),
                        name: "write_file".into(),
                        input: crate::metadata::object([
                            ("file_path", "refused-write.txt".into()),
                            ("content", "fixture".into()),
                        ]),
                    },
                ],
                invocation,
            )
            .await
            .unwrap();
        assert_eq!(result.len(), 2);
        if mode == "ask" {
            for result in &result {
                assert_eq!(
                    result.get("error").and_then(|error| error.get("code")),
                    Some(&"ask_mode_violation".into())
                );
            }
            assert!(!shell_marker.exists());
            assert!(!file_marker.exists());
            let invocation = ports
                .tool_context
                .prepare(JsValue::Undefined)
                .await
                .unwrap();
            let read = ports
                .tool_execution
                .execute_all(
                    vec![ToolCall {
                        id: "safe-bash".into(),
                        name: "bash".into(),
                        input: crate::metadata::object([("command", "pwd".into())]),
                    }],
                    invocation,
                )
                .await
                .unwrap();
            assert_eq!(read[0].get("type"), Some(&"success".into()));
        } else {
            for result in &result {
                assert_eq!(result.get("type"), Some(&"success".into()));
            }
            assert_eq!(std::fs::read(shell_marker).unwrap(), b"fixture");
            assert_eq!(std::fs::read(file_marker).unwrap(), b"fixture");
        }
    }
}
