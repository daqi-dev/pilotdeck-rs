// Actual TS loadPilotConfig runtime-binding facts; no provisioning or service IO.
use std::collections::BTreeMap;

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}
fn native_env(values: &serde_json::Map<String, serde_json::Value>, home: &Path) -> Env {
    let mut pairs: Vec<(String, String)> = values
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
        .collect();
    pairs.push(("HOME".into(), home.to_str().unwrap().into()));
    pairs.push(("PILOT_HOME".into(), home.to_str().unwrap().into()));
    Env::from_pairs(
        &pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>(),
    )
}
#[test]
fn readonly_project_bindings_match_thirty_real_ts_loader_cases() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/config-binding/oracle.json")).unwrap();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 30);
    for case in oracle["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        // Match the actual TS recorder topology: home and project are
        // siblings under <root>, and runtime paths originate in <root>/home.
        let root =
            std::env::temp_dir().join(format!("pdrs-cli-binding-{id}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let home = root.join("home");
        let cwd = root.join("project");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&cwd).unwrap();
        fs::write(home.join("pilotdeck.yaml"), case["yaml"].as_str().unwrap()).unwrap();
        fs::create_dir(home.join("staffdeck")).unwrap();
        if let Some(store) = case["store"].as_str() {
            fs::write(
                home.join("staffdeck/project-bindings.json"),
                store.replace("<project>", cwd.to_str().unwrap()),
            )
            .unwrap();
        }
        if let Some(sops) = case["sops"].as_str() {
            fs::create_dir(home.join("staffdeck/fixture-binding")).unwrap();
            fs::write(home.join("staffdeck/fixture-binding/sops.json"), sops).unwrap();
        }
        if let Some(lock) = case["lock"].as_str() {
            let value = match lock {
                "alive" => std::process::id().to_string(),
                "dead" => "2147483647".into(),
                value => value.into(),
            };
            fs::write(home.join("staffdeck/project-bindings.json.lock"), value).unwrap();
        }
        let env = native_env(case["env"].as_object().unwrap(), &home);
        let before = tree(&root);
        let expected = if case["nativeTodo"] == true {
            "TODO config requiring JavaScript host support"
        } else if case["result"]["ok"] == true {
            "ok"
        } else {
            case["result"]["message"].as_str().unwrap()
        };
        assert_eq!(
            message(check(&env, &cwd, &home)),
            expected.replace("<root>", root.to_str().unwrap()),
            "{id}"
        );
        let snapshot = pilotdeck_gateway::config::load_snapshot_bounded(
            &home,
            &cwd,
            &env.strings(),
            Some("."),
            1.0,
            "2026-01-01T00:00:00.000Z",
        );
        if case["nativeTodo"] == true {
            assert!(snapshot.err().unwrap().requires_host, "{id}");
        } else if case["result"]["ok"] == true {
            let snapshot = snapshot.unwrap();
            let modules = snapshot.get("config").unwrap().get("modules").unwrap();
            let actual: serde_json::Value = serde_json::from_str(
                &pilotdeck_protocol::stringify_json(modules)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let expected: serde_json::Value = serde_json::from_str(
                &case["result"]["modulesJson"]
                    .as_str()
                    .unwrap()
                    .replace("<root>", root.to_str().unwrap()),
            )
            .unwrap();
            assert_eq!(actual, expected, "{id} normalized runtime modules");
        } else {
            let error = snapshot.err().unwrap();
            assert!(!error.requires_host, "{id}");
            assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                expected,
                "{id} snapshot error"
            );
        }
        assert_eq!(tree(&root), before, "{id} read-only files");
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn existing_fixed_staffdeck_enabled_and_type_corpus_reaches_the_cli_loader() {
    for (raw, ids) in [
        (
            include_str!("../../pilotdeck-gateway/tests/fixtures/config/catalog.json"),
            vec![
                "staffdeck-number-modules",
                "staffdeck-string-modules",
                "staffdeck-false-modules",
                "staffdeck-null-modules",
                "staffdeck-disables-knowledge-sop",
                "staffdeck-case-sensitive",
            ],
        ),
        (
            include_str!("../../pilotdeck-gateway/tests/fixtures/config/diagnostics.json"),
            vec![
                "diag-2422-modules",
                "diag-2423-modules",
                "diag-2424-modules",
                "diag-2425-modules",
                "diag-2426-modules",
                "diag-2427-modules",
                "diag-2654-prototype-root",
                "diag-2655-prototype-root-pilot",
            ],
        ),
    ] {
        let corpus = pilotdeck_protocol::parse_json(raw).unwrap();
        let pilotdeck_protocol::JsValue::Array(cases) = corpus.get("cases").unwrap() else {
            panic!("cases array")
        };
        for id in ids {
            let row = cases
                .iter()
                .find(|row| {
                    row.get("id")
                        .and_then(|id| id.as_string())
                        .is_some_and(|id2| id2.equals_str(id))
                })
                .unwrap_or_else(|| panic!("missing fixed TS {id}"));
            let case: serde_json::Value =
                serde_json::from_str(&pilotdeck_protocol::stringify_json(row).unwrap().unwrap())
                    .unwrap();
            let (root, _) = fixture(&format!("fixed-{id}"), case["yaml"].as_str().unwrap());
            let env = native_env(case["env"].as_object().unwrap(), &root);
            let expected = if case["load"]["ok"] == true {
                "ok"
            } else {
                case["load"]["message"].as_str().unwrap()
            };
            assert_eq!(
                message(check(&env, &root.join("project"), &root)),
                expected,
                "{id}"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}
