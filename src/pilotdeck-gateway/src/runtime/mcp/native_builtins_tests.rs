use super::*;
#[test]
fn cli_and_control_resolve_actual_gateway_sibling() {
    for name in ["pilotdeck", "pilotdeck-control", "pilotdeck-gateway"] {
        let exe = Path::new("/owned/generation/bin").join(name);
        let servers = funasr_servers(&exe).unwrap();
        let server = servers.get("funasr").unwrap();
        let command = server
            .get("command")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        assert_eq!(
            command,
            if cfg!(windows) {
                "/owned/generation/bin/pilotdeck-gateway.exe"
            } else {
                "/owned/generation/bin/pilotdeck-gateway"
            }
        );
        let json = pilotdeck_protocol::stringify_json(server).unwrap().unwrap();
        assert!(json.contains("--builtin-mcp"));
        assert!(!json.contains("funasr-local-mcp"));
        assert!(!json.contains("NODE_EXECUTABLE"));
    }
}
#[test]
fn browser_spec_retains_third_party_mcp_and_per_session_contract() {
    let spec = browser_servers();
    let text = pilotdeck_protocol::stringify_json(&spec).unwrap().unwrap();
    assert!(text.contains("PLAYWRIGHT_MCP_ENTRYPOINT"));
    assert!(text.contains("--allow-unrestricted-file-access"));
    assert!(text.contains("\"perSession\":true"));
}
