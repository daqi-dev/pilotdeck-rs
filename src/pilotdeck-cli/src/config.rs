//! Shared loadPilotConfig validation; CLI-only unsupported branches stay explicit.
use crate::Failure;
use crate::paths::Env;
use std::path::Path;

pub fn check(env: &Env, cwd: &Path, pilot_home: &Path) -> Result<(), Failure> {
    // applyProjectBinding precedes the first fatal check in TS. The gateway's
    // read-only binding support does not implement the CLI's StaffDeck flow.
    if env.get("STAFFDECK_PROJECT_BINDING_ENABLED") == Some(std::ffi::OsStr::new("true")) {
        return Err(Failure::Todo(
            "StaffDeck project binding (applyProjectBinding)",
        ));
    }
    pilotdeck_gateway::config::load_project(pilot_home, cwd, &env.strings(), Some("."))
        .map(|_| ())
        .map_err(|error| {
            if error.requires_host {
                Failure::Todo("config requiring JavaScript host support")
            } else {
                Failure::Error(String::from_utf16_lossy(error.message.units()))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn message(result: Result<(), Failure>) -> String {
        match result {
            Ok(()) => "ok".into(),
            Err(Failure::Error(message)) => message,
            Err(Failure::Todo(what)) => format!("TODO {what}"),
        }
    }

    fn fixture(name: &str, yaml: &str) -> (PathBuf, Env) {
        let dir =
            std::env::temp_dir().join(format!("pdrs-cli-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("project")).unwrap();
        fs::write(dir.join("pilotdeck.yaml"), yaml).unwrap();
        let path = dir.join("pilotdeck.yaml").to_string_lossy().into_owned();
        (
            dir,
            Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", path.as_str())]),
        )
    }

    fn run(name: &str, yaml: &str, extra: Option<(&str, &str)>) -> String {
        let (dir, mut env) = fixture(name, yaml);
        if let Some((key, value)) = extra {
            let path = dir.join("pilotdeck.yaml").to_string_lossy().into_owned();
            env = Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", path.as_str()), (key, value)]);
        }
        let result = message(check(&env, &dir.join("project"), &dir));
        fs::remove_dir_all(dir).unwrap();
        result
    }

    #[test]
    fn prototype_keys_match_real_ts_merge_and_first_fatal() {
        // Reuse the pinned TS recorder output rather than deriving expectations
        // from either Rust entry. Env merge drops inherited fields in two rows.
        let fixture_data = pilotdeck_protocol::parse_json(include_str!(
            "../../pilotdeck-gateway/tests/fixtures/config/diagnostics.json"
        ))
        .unwrap();
        let pilotdeck_protocol::JsValue::Array(cases) = fixture_data.get("cases").unwrap() else {
            panic!("TS fixture cases must be an array");
        };
        for id in [
            "diag-2650-prototype-root",
            "diag-2651-prototype-root-pilot",
            "diag-2652-prototype-root",
            "diag-2653-prototype-root-pilot",
        ] {
            let row = cases
                .iter()
                .find(|row| {
                    row.get("id")
                        .and_then(|value| value.as_string())
                        .is_some_and(|value| value.equals_str(id))
                })
                .unwrap();
            // Only these ASCII cases enter serde; the complete golden corpus
            // also contains lone UTF-16 surrogates and needs the JS parser.
            let row: serde_json::Value =
                serde_json::from_str(&pilotdeck_protocol::stringify_json(row).unwrap().unwrap())
                    .unwrap();
            let (dir, _) = fixture(id, row["yaml"].as_str().unwrap());
            let mut env = pilotdeck_gateway::config::Env::from([(
                "PILOTDECK_CONFIG_PATH".into(),
                dir.join("pilotdeck.yaml").to_string_lossy().into_owned(),
            )]);
            for (key, value) in row["env"].as_object().unwrap() {
                env.insert(key.clone(), value.as_str().unwrap().into());
            }
            let pairs: Vec<_> = env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let cli = check(&Env::from_pairs(&pairs), &dir.join("project"), &dir);
            let gateway = pilotdeck_gateway::config::load_project(
                &dir,
                &dir.join("project"),
                &env,
                Some("."),
            );
            if row["load"]["ok"] == true {
                assert!(gateway.is_ok(), "{id}");
                assert_eq!(message(cli), "ok", "{id}");
            } else {
                let error = gateway.err().expect("TS first fatal");
                assert!(!error.requires_host, "{id}");
                assert_eq!(error.code, row["load"]["code"].as_str().unwrap(), "{id}");
                let expected = row["load"]["message"].as_str().unwrap();
                assert_eq!(
                    String::from_utf16_lossy(error.message.units()),
                    expected,
                    "{id}"
                );
                assert_eq!(message(cli), expected, "{id}");
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn cli_and_gateway_report_the_same_first_fatal() {
        // Keep this cross-entry test when removing the CLI's original parser.
        // Each row contains multiple faults, or an override that changes which
        // fault wins. Expected text is pinned independently of either loader.
        for (name, global, project, model, expected) in [
            (
                "both-yaml",
                "a: [\n",
                "b: [\n",
                None,
                "Failed to parse default config YAML.",
            ),
            (
                "project-yaml",
                "pilot: 1\nschemaVersion: 2\n",
                "b: [\n",
                None,
                "Failed to parse project config YAML.",
            ),
            (
                "default-root",
                "- 1\n",
                "pilot: 1\n",
                None,
                "default config root must be an object.",
            ),
            (
                "project-root",
                "schemaVersion: 2\n",
                "- 1\n",
                None,
                "project config root must be an object.",
            ),
            (
                "pilot-first",
                "pilot: null\nschemaVersion: 2\n",
                "",
                None,
                "YAML config must not contain a pilot section. Use PILOT_HOME for PilotHome.",
            ),
            (
                "schema-first",
                "schemaVersion: [2, null, x]\n",
                "",
                None,
                "Unsupported schemaVersion 2,,x.",
            ),
            (
                "missing-agent",
                "schemaVersion: 1\n",
                "",
                None,
                "Config must contain an agent section.",
            ),
            (
                "project-agent",
                "schemaVersion: 1\n",
                "agent: {}\n",
                None,
                "Config must contain a model section.",
            ),
            (
                "env-agent",
                "schemaVersion: 1\n",
                "",
                Some("p/m"),
                "Config must contain a model section.",
            ),
            (
                "project-override",
                "schemaVersion: 2\nagent: {}\n",
                "schemaVersion: 1\n",
                None,
                "Config must contain a model section.",
            ),
        ] {
            let (dir, mut env) = fixture(&format!("cross-{name}"), global);
            let cwd = dir.join("project");
            fs::create_dir_all(cwd.join(".pilotdeck")).unwrap();
            fs::write(cwd.join(".pilotdeck/pilotdeck.yaml"), project).unwrap();
            let mut gateway_env = pilotdeck_gateway::config::Env::from([(
                "PILOTDECK_CONFIG_PATH".into(),
                dir.join("pilotdeck.yaml").to_string_lossy().into_owned(),
            )]);
            if let Some(model) = model {
                gateway_env.insert("PILOT_AGENT_MODEL".into(), model.into());
                env = Env::from_pairs(&[
                    (
                        "PILOTDECK_CONFIG_PATH",
                        &gateway_env["PILOTDECK_CONFIG_PATH"],
                    ),
                    ("PILOT_AGENT_MODEL", model),
                ]);
            }
            let gateway =
                pilotdeck_gateway::config::load_project(&dir, &cwd, &gateway_env, Some("."))
                    .err()
                    .expect("fatal config");
            let cli = message(check(&env, &cwd, &dir));
            assert_eq!(
                gateway.message.to_utf8().unwrap(),
                expected,
                "gateway: {name}"
            );
            assert_eq!(cli, expected, "cli: {name}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn first_fatal_follows_ts_order() {
        assert_eq!(
            run("empty", "schemaVersion: 1\n", None),
            "Config must contain an agent section."
        );
        assert_eq!(
            run("model", "schemaVersion: 1\nagent: {}\n", None),
            "Config must contain a model section."
        );
        assert_eq!(
            run(
                "env-agent",
                "schemaVersion: 1\n",
                Some(("PILOT_AGENT_MODEL", "p/m"))
            ),
            "Config must contain a model section."
        );
        assert_eq!(
            run("version", "schemaVersion: '2'\n", None),
            "Unsupported schemaVersion 2."
        );
        assert_eq!(
            run("pilot", "pilot: 1\nschemaVersion: 3.5\n", None),
            "YAML config must not contain a pilot section. Use PILOT_HOME for PilotHome."
        );
        assert_eq!(
            run("root", "- 1\n", None),
            "default config root must be an object."
        );
        assert_eq!(
            run("yaml", "a: [\n", None),
            "Failed to parse default config YAML."
        );
        assert_eq!(
            run(
                "empty-providers",
                "schemaVersion: 1\nagent: {}\nmodel: {}\n",
                None
            ),
            "Model config must contain at least one provider."
        );
    }

    #[test]
    fn project_binding_runs_before_the_first_fatal_like_ts() {
        let binding = Some(("STAFFDECK_PROJECT_BINDING_ENABLED", "true"));
        for (name, yaml) in [
            ("binding-agent", "schemaVersion: 1\n"),
            ("binding-yaml", "a: [\n"),
        ] {
            assert_eq!(
                run(name, yaml, binding),
                "TODO StaffDeck project binding (applyProjectBinding)"
            );
        }
        assert_eq!(
            run(
                "binding-off",
                "schemaVersion: 1\n",
                Some(("STAFFDECK_PROJECT_BINDING_ENABLED", "1"))
            ),
            "Config must contain an agent section."
        );
    }

    #[test]
    fn missing_config_file_is_not_a_read_error() {
        // An empty or blank override stays "" (not nullish, not resolved to cwd), and
        // existsSync("") is false: no config source at all.
        // Values as recorded by probes/p1-config-path-empty (Node v22.22.0).
        for value in ["", "  ", "\t", "\u{feff}", "\u{3000} ", "\u{feff}\t"] {
            let env = Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", value)]);
            assert_eq!(
                message(check(
                    &env,
                    Path::new("/nonexistent-pdrs"),
                    Path::new("/nonexistent-pdrs")
                )),
                "Config must contain an agent section."
            );
        }
    }

    #[test]
    fn full_loader_uses_provider_environment_and_relative_config_path() {
        let yaml = "schemaVersion: 1\nagent: {model: test/m}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: https://model.invalid/v1\n      apiKey: ${D1_CONFIG_TEST_KEY}\n      models: {m: {}}\n";
        let (dir, _) = fixture("full-loader", yaml);
        let cwd = dir.join("project");
        let env = Env::from_pairs(&[
            ("PILOTDECK_CONFIG_PATH", "../pilotdeck.yaml"),
            ("D1_CONFIG_TEST_KEY", "fixture-key"),
        ]);
        assert_eq!(message(check(&env, &cwd, &dir)), "ok");
        let no_key = Env::from_pairs(&[("PILOTDECK_CONFIG_PATH", "../pilotdeck.yaml")]);
        assert_ne!(message(check(&no_key, &cwd, &dir)), "ok");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_dependent_config_remains_an_explicit_todo() {
        assert_eq!(
            run(
                "host-required",
                "schemaVersion: 1\nagent: {model: constructor/m}\nmodel: {}\n",
                None
            ),
            "TODO config requiring JavaScript host support"
        );
    }
}
