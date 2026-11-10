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

#[test]
fn explicit_asr_install_command_survives_unrelated_cwd() {
    let mut env = Env::new();
    env.insert(
        "PILOTDECK_FUNASR_INSTALL_COMMAND".into(),
        "local-installer --offline".into(),
    );
    let paths = builtin_paths(
        &env,
        Path::new("/generation/bin/pilotdeck-gateway"),
        Path::new("/project"),
    )
    .unwrap();
    assert_eq!(paths.install_command, "local-installer --offline");
}
#[test]
fn source_generation_asr_installer_is_resolved_and_shell_quoted() {
    let root =
        std::env::temp_dir().join(format!("pdrs-asr-install-{}-' space", uuid::Uuid::new_v4()));
    let generation = root.join("generation");
    let scripts = generation.join("source/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("install-asr.sh"), "#!/bin/sh\n").unwrap();
    let executable = generation.join("bin/pilotdeck-gateway");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, b"physical gateway fixture").unwrap();
    let paths = builtin_paths(&Env::new(), &executable, Path::new("/unrelated-project")).unwrap();
    assert_eq!(
        paths.install_command,
        format!(
            "sh '{}'",
            scripts
                .join("install-asr.sh")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\'', "'\"'\"'")
        )
    );
    assert!(!paths.install_command.contains("npm run"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn absent_source_installer_does_not_invent_a_command() {
    let paths = builtin_paths(
        &Env::new(),
        Path::new("/absent-generation/bin/pilotdeck"),
        Path::new("/unrelated-project"),
    )
    .unwrap();
    assert!(paths.install_command.is_empty());
}
#[cfg(unix)]
#[test]
fn stable_executable_symlink_pins_physical_gateway_and_installer_generation() {
    let root = std::env::temp_dir().join(format!("pdrs-asr-symlink-{}", uuid::Uuid::new_v4()));
    let physical = root.join("generation/bin/pilotdeck");
    let script = root.join("generation/source/scripts/install-asr.sh");
    std::fs::create_dir_all(physical.parent().unwrap()).unwrap();
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&physical, b"physical CLI fixture").unwrap();
    std::fs::write(&script, b"#!/bin/sh\n").unwrap();
    let stable = root.join("stable-pilotdeck");
    std::os::unix::fs::symlink(&physical, &stable).unwrap();
    assert_eq!(
        sibling_gateway(&stable),
        physical
            .canonicalize()
            .unwrap()
            .with_file_name("pilotdeck-gateway")
    );
    let paths = builtin_paths(&Env::new(), &stable, Path::new("/unrelated-project")).unwrap();
    assert_eq!(
        paths.install_command,
        format!("sh '{}'", script.canonicalize().unwrap().display())
    );
    std::fs::remove_dir_all(root).unwrap();
}
