//! Process-level exit codes for the config branches that remain unimplemented.
use std::{fs, process::Command};

#[test]
fn staffdeck_binding_and_valid_local_gateway_still_exit_two() {
    let root = std::env::temp_dir().join(format!("pdrs-d1-config-exit-{}", std::process::id()));
    fs::create_dir_all(root.join("project")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let config = root.join("pilotdeck.yaml");
    for (binding, yaml, expected) in [
        (
            "true",
            "a: [\n",
            "StaffDeck project binding (applyProjectBinding)",
        ),
        (
            "true",
            "schemaVersion: 1\n",
            "StaffDeck project binding (applyProjectBinding)",
        ),
        (
            "false",
            "schemaVersion: 1\nagent: {model: test/m}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: https://model.invalid/v1\n      apiKey: fixture\n      models: {m: {}}\n",
            "local Gateway and CliChannel for a valid config",
        ),
        (
            "false",
            "schemaVersion: 1\nagent: {model: constructor/m}\nmodel: {}\n",
            "config requiring JavaScript host support",
        ),
    ] {
        fs::write(&config, yaml).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .current_dir(root.join("project"))
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PILOT_HOME", root.join("home"))
            .env("PILOTDECK_CONFIG_PATH", &config)
            .env("PILOTDECK_BUNDLED_SKILLS_DIR", root.join("missing-skills"))
            .env("STAFFDECK_PROJECT_BINDING_ENABLED", binding)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .ends_with(&format!(
                    "pilotdeck-rs TODO: {expected} is not implemented yet.\n"
                ))
        );
    }
    fs::remove_dir_all(root).unwrap();
}
