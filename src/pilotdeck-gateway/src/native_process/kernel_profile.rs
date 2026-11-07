//! The native product profile is deliberately explicit. These are binding
//! decisions, not a claim that every upstream product entry is accepted.
pub const BACKEND_METHODS: &[&str] = &[
    "active_turn_snapshot",
    "read_session_messages",
    "permission_decide",
    "permission_list",
    "reconnect_interaction",
    "steer_turn",
    "run_get",
    "run_reattach",
    "run_events",
];
pub const UNSUPPORTED_SESSION_METHODS: &[&str] = &[
    "delete_session",
    "background_tasks",
    "background_task_stop",
    "supported_agents",
    "elicitation_respond",
    "session_model_get",
    "session_model_set",
    "session_model_clear",
    "seed_read_state",
    "grant_session_permission",
    "cancel_steer",
    "hook_async_result",
    "fork_session",
    "replace_last_turn",
    "finalize_last_turn_replacement",
    "read_subagent_messages",
    "rewind_files",
    "record_agent_status_message",
    "set_mcp_servers",
    "mcp_server_reconnect",
    "mcp_server_toggle",
    "set_mcp_permission_mode_override",
    "usage_snapshot",
    "model_usage_snapshot",
];
pub const BOUND_CONFIG_METHODS: &[&str] = &[
    "set_permission_mode",
    "set_session_thinking",
    "set_output_style",
];
pub fn description() -> pilotdeck_protocol::JsValue {
    use pilotdeck_protocol::JsValue;
    crate::metadata::object([
        ("profile", "kernel-native-session-v1".into()),
        ("executionAuthority", "kernel".into()),
        ("historyAuthority", "kernel_journal".into()),
        (
            "configuration",
            "immutable_loop_and_model_with_turn_preferences".into(),
        ),
        (
            "turnPreferences",
            JsValue::Array(
                BOUND_CONFIG_METHODS
                    .iter()
                    .map(|method| JsValue::from(*method))
                    .collect(),
            ),
        ),
        ("supportsController", JsValue::Bool(false)),
        (
            "methods",
            JsValue::Array(BACKEND_METHODS.iter().map(|v| JsValue::from(*v)).collect()),
        ),
        (
            "historyTransfer",
            JsValue::Array(
                ["export_session_transcript", "restore_session_transcript"]
                    .into_iter()
                    .map(JsValue::from)
                    .collect(),
            ),
        ),
        (
            "unsupportedSessionMethods",
            JsValue::Array(
                UNSUPPORTED_SESSION_METHODS
                    .iter()
                    .map(|v| JsValue::from(*v))
                    .collect(),
            ),
        ),
        ("upstreamAcceptanceComplete", JsValue::Bool(false)),
    ])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_never_claims_frozen_configuration_mutations() {
        for method in BACKEND_METHODS {
            assert!(!UNSUPPORTED_SESSION_METHODS.contains(method));
        }
        for method in ["session_model_set", "set_mcp_servers", "rewind_files"] {
            assert!(UNSUPPORTED_SESSION_METHODS.contains(&method));
        }
    }
}
