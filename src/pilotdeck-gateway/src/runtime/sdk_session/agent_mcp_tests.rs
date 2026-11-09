use super::{
    SdkAgentMcpResolver, SdkAgentMcpServer, SdkSessionError, registry::resolve_agent_mcp_servers,
};
use crate::metadata::object;
use pilotdeck_protocol::{JsString, JsValue};

struct SessionServers;
impl SdkAgentMcpResolver for SessionServers {
    fn session_server(&self, session: &JsString, name: &JsString) -> Option<SdkAgentMcpServer> {
        if !session.equals_str("s") {
            return None;
        }
        match name.to_utf8().ok()?.as_str() {
            "tickets" | "disabled" => Some(SdkAgentMcpServer {
                config: object([("command", "ticket-server".into())]),
                enabled: !name.equals_str("disabled"),
            }),
            _ => None,
        }
    }
}

fn resolve(session: &str, specs: JsValue) -> Result<JsValue, SdkSessionError> {
    resolve_agent_mcp_servers(
        &session.into(),
        &"reviewer".into(),
        &specs,
        Some(&SessionServers),
    )
}

#[test]
fn references_use_only_enabled_servers_from_the_same_session() {
    let resolved = resolve("s", JsValue::Array(vec!["tickets".into()])).unwrap();
    assert_eq!(
        resolved
            .get("tickets")
            .and_then(|server| server.get("command")),
        Some(&JsValue::from("ticket-server"))
    );
    assert_eq!(
        resolve("other", "tickets".into()).unwrap_err().code,
        "SDK_AGENT_MCP_REFERENCE_NOT_FOUND"
    );
    assert_eq!(
        resolve("s", "disabled".into()).unwrap_err().code,
        "SDK_AGENT_MCP_REFERENCE_DISABLED"
    );
}

#[test]
fn inline_entries_preserve_names_and_reject_duplicate_inline_specs() {
    let config = object([("command", "inline-server".into())]);
    let entry = object([("inline", config.clone())]);
    assert_eq!(
        resolve("s", entry.clone()).unwrap().get("inline"),
        Some(&config)
    );
    assert_eq!(
        resolve("s", JsValue::Array(vec![entry.clone(), entry]))
            .unwrap_err()
            .code,
        "INVALID_MCP_SERVER"
    );
}
