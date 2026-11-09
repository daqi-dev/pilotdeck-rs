//! Ask mode is an execution policy even when the permission mode is bypass.
use super::{ExecutableTool, named, truthy};
use pilotdeck_protocol::{JsString, JsValue};
fn is_allowed(tool: &dyn ExecutableTool) -> bool {
    if tool
        .fields()
        .get("kind")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str("mcp"))
    {
        let empty = JsValue::Object(Default::default());
        return tool
            .is_read_only(&empty)
            .ok()
            .is_some_and(|value| truthy(&value));
    }
    let Some(name) = tool.fields().get("name").and_then(JsValue::as_string) else {
        return false;
    };
    [
        "read_file",
        "get_current_time",
        "grep",
        "glob",
        "web_search",
        "web_fetch",
        "ask_user_question",
        "read_skill",
        "structured_output",
        "agent",
        "execute_code",
        "bash",
        "task_list",
        "task_output",
        "task_wait",
    ]
    .iter()
    .any(|allowed| name.equals_str(allowed))
}
pub(super) fn violation(tool: &dyn ExecutableTool, input: &JsValue) -> Option<JsString> {
    let name = tool.fields().get("name").and_then(JsValue::as_string)?;
    if !is_allowed(tool) {
        return Some(tool_violation(name));
    }
    if name.equals_str("bash") {
        let command = input
            .get("command")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        if !command.is_empty()
            && super::builtin::bash::is_read_only_shell_command(&String::from_utf16_lossy(
                command.units(),
            ))
        {
            return None;
        }
        let command = if command.len_utf16() > 120 {
            let mut units = command.units()[..120].to_vec();
            units.push(0x2026);
            JsString::from_utf16(units)
        } else {
            command
        };
        return Some(named(
            "[ASK_MODE_VIOLATION] bash command \"",
            &command,
            "\" is BLOCKED because it is not read-only.\n\nIn ask mode, bash is restricted to read-only commands only (pwd, ls, cat, head, wc, git status, git diff, git log, git show, find without write actions, etc.).",
        ));
    }
    if !name.equals_str("agent")
        && !tool
            .is_read_only(input)
            .ok()
            .is_some_and(|value| truthy(&value))
    {
        return Some(tool_violation(name));
    }
    None
}
fn tool_violation(name: &JsString) -> JsString {
    named(
        "[ASK_MODE_VIOLATION] Tool \"",
        name,
        "\" is BLOCKED in ask mode.\n\nAsk mode is read-only. Tools may inspect files, search, ask questions, fetch/read external content, or launch read-only subagents, but they cannot modify files or create side effects.\n\nDo NOT retry this tool. It will fail again while ask mode is active.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{HostError, modules::capability::ToolDefinition};
    use crate::runtime::tools::{ToolFuture, ToolOutput, ToolRunContext};
    use pilotdeck_protocol::JsObject;
    struct Fixture {
        fields: JsObject,
        read_only: bool,
    }
    impl ToolDefinition for Fixture {
        fn fields(&self) -> &JsObject {
            &self.fields
        }
        fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
            Ok(JsValue::Bool(self.read_only))
        }
    }
    impl ExecutableTool for Fixture {
        fn execute(&self, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
            Box::pin(async { unreachable!("pure policy fixture must never execute") })
        }
    }
    #[test]
    fn matches_real_ts_ask_policy_oracle() {
        let golden = pilotdeck_protocol::parse_json(include_str!("ask_mode_oracle.json")).unwrap();
        let JsValue::Array(cases) = golden.get("cases").unwrap() else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 12);
        for case in cases {
            let mut fields = JsObject::new();
            fields.insert("name".into(), case.get("name").unwrap().clone());
            fields.insert(
                "kind".into(),
                case.get("kind").cloned().unwrap_or_else(|| "custom".into()),
            );
            let fixture = Fixture {
                fields,
                read_only: case.get("readOnly") == Some(&JsValue::Bool(true)),
            };
            assert_eq!(
                is_allowed(&fixture),
                case.get("allowed") == Some(&JsValue::Bool(true)),
                "{:?}",
                case.get("id")
            );
            let input = case
                .get("input")
                .cloned()
                .unwrap_or_else(|| JsValue::Object(Default::default()));
            assert_eq!(
                violation(&fixture, &input).map(JsValue::String),
                case.get("violation").cloned(),
                "{:?}",
                case.get("id")
            );
        }
    }
}
