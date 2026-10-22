#[path = "common/baseline.rs"]
mod baseline;
// Replays real TS fixture inputs against the disk bridge and existing runtime.
use pilotdeck_gateway::runtime::plugins::{
    LoadedPlugin, PilotExtensionPaths, PluginRefreshResult, PluginRuntime, PluginRuntimeOptions,
    PluginSource,
    loader::{self, DiscoveredPluginPath, LoadedPluginCommand, PluginKind},
};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn oracle() -> Value {
    static FIXTURE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture = baseline::oracle(include_str!("fixtures/plugin-loader/oracle.json"));
            json_value(&fixture)
        })
        .clone()
}

fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(&stringify_json(value).unwrap().unwrap()).unwrap()
}
fn assert_json(actual: &Value, expected: &Value, context: &str) {
    assert_eq!(
        parse_json(&actual.to_string()).unwrap(),
        parse_json(&expected.to_string()).unwrap(),
        "{context}"
    );
}
fn capture<T>(
    result: Result<T, impl std::fmt::Display>,
    convert: impl FnOnce(T) -> Value,
) -> Value {
    match result {
        Ok(value) => json!({"value":convert(value)}),
        Err(e) => json!({"error":e.to_string()}),
    }
}

#[test]
fn plugin_loader_manifest_oracle() {
    for case in oracle()["manifestCases"].as_array().unwrap() {
        let input = parse_json(&case["raw"].to_string()).unwrap();
        let actual = capture(loader::parse_plugin_manifest(&input), |v| json_value(&v));
        let expected = if case.get("error").is_some() {
            json!({"error":case["error"]})
        } else {
            json!({"value":case["value"]})
        };
        assert_json(&actual, &expected, &format!("manifest {:?}", case["raw"]));
    }
}
#[test]
fn plugin_loader_validation_oracle() {
    let fixture = oracle();
    for case in fixture["nameCases"].as_array().unwrap() {
        assert_eq!(
            json!(loader::validate_marketplace_name(
                case["name"].as_str().unwrap()
            )),
            case["error"],
            "{case}"
        );
    }
    for case in fixture["referenceCases"].as_array().unwrap() {
        let input = parse_json(&case["reference"].to_string()).unwrap();
        assert_json(
            &json_value(&loader::resolve_marketplace_reference(&input)),
            &case["value"],
            "marketplace reference",
        );
    }
    let root = Scratch::new();
    for case in fixture["sourcePathCases"].as_array().unwrap() {
        let path = case["path"]
            .as_str()
            .unwrap()
            .replace("<root>", root.0.to_str().unwrap());
        let allowed = case["allowedRoot"]
            .as_str()
            .unwrap()
            .replace("<root>", root.0.to_str().unwrap());
        assert_eq!(
            json!(loader::validate_plugin_source_path(
                Path::new(&path),
                Path::new(&allowed)
            )),
            case["value"],
            "{case}"
        );
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-rust-plugin-loader-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        // Restore test-owned permissions before traversing/removing the fixture.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut stack = vec![self.0.clone()];
            while let Some(path) = stack.pop() {
                if fs::symlink_metadata(&path).is_ok_and(|s| s.file_type().is_symlink()) {
                    continue;
                }
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
                if let Ok(entries) = fs::read_dir(path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
            }
        }
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn setup(root: &Path, scenario: &Value) {
    for file in scenario["files"].as_array().unwrap() {
        let path = root.join(file["path"].as_str().unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if file.get("mode").is_some() {
            if file["dir"] == true {
                fs::create_dir_all(path).unwrap();
            }
            continue;
        }
        if let Some(target) = file["link"].as_str() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(root.join(target), path).unwrap();
            #[cfg(not(unix))]
            panic!("plugin symlink fixture requires Unix");
        } else if file["dir"] == true {
            fs::create_dir_all(path).unwrap();
        } else {
            fs::write(path, file["text"].as_str().unwrap()).unwrap();
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in scenario["files"].as_array().unwrap() {
            if let Some(mode) = file["mode"].as_u64() {
                let path = root.join(file["path"].as_str().unwrap());
                fs::set_permissions(&path, fs::Permissions::from_mode(mode as u32)).unwrap();
                // Independent native oracle: chmod must actually deny access.
                if fs::metadata(&path).unwrap().is_dir() {
                    assert!(
                        fs::read_dir(path).is_err(),
                        "native unreadable directory probe must fail"
                    );
                } else {
                    assert!(
                        fs::read(path).is_err(),
                        "native unreadable file probe must fail"
                    );
                }
            }
        }
    }
}
fn source(value: &Value) -> PluginSource {
    match value.as_str().unwrap() {
        "builtin" => PluginSource::Builtin,
        "global" => PluginSource::Global,
        "project" => PluginSource::Project,
        _ => panic!("unknown source"),
    }
}
fn command(c: &LoadedPluginCommand) -> Value {
    json!({"name":c.name,"path":c.path,"content":c.content,"frontmatter":c.frontmatter,"isSkill":c.is_skill})
}
fn plugin(p: &LoadedPlugin) -> Value {
    let mut out = json!({"name":p.name,"path":p.path,"source":p.source.as_str(),"commands":p.commands.iter().map(command).collect::<Vec<_>>(),"skills":p.skills.iter().map(command).collect::<Vec<_>>(),"outputStyles":p.output_styles.iter().map(command).collect::<Vec<_>>()});
    if let Some(hooks) = &p.hooks_config {
        let hooks: serde_json::Map<String, Value> = hooks
            .iter()
            .map(|(event, matchers)| {
                (
                    String::from_utf16_lossy(event.units()),
                    Value::Array(matchers.iter().map(|m| json_value(&m.to_value())).collect()),
                )
            })
            .collect();
        out["hooksConfig"] = Value::Object(hooks);
    }
    if let Some(servers) = &p.mcp_servers {
        out["mcpServers"] = json_value(servers);
    }
    if let Some(servers) = &p.lsp_servers {
        out["lspServers"] = json_value(servers);
    }
    out
}
fn report(r: &PluginRefreshResult) -> Value {
    json!({"generation":r.generation,"next":r.next.iter().map(|p|plugin(p)).collect::<Vec<_>>(),"stagedFailures":r.staged_failures.iter().map(|f|json!({"path":f.path,"source":f.source.as_str(),"kind":if f.kind==PluginKind::Plugin {"plugin"}else{"skill"}})).collect::<Vec<_>>()})
}
fn discovered(paths: &[DiscoveredPluginPath]) -> Value {
    json!(
        paths
            .iter()
            .map(|p| json!({"path":p.path,"source":p.source.as_str()}))
            .collect::<Vec<_>>()
    )
}
fn normalize(value: Value, root: &Path) -> Value {
    // Test outputs have shallow, bounded structure (the fixture itself).
    serde_json::from_str(&value.to_string().replace(root.to_str().unwrap(), "<root>")).unwrap()
}
fn options(root: &Path, scenario: &Value) -> PluginRuntimeOptions {
    let builtins = scenario["builtins"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|b| {
                    Arc::new(LoadedPlugin {
                        name: b["name"].as_str().unwrap().into(),
                        path: root.join(b["path"].as_str().unwrap()),
                        source: source(&b["source"]),
                        commands: b["commands"]
                            .as_array()
                            .map(|cs| {
                                cs.iter()
                                    .map(|c| LoadedPluginCommand {
                                        name: c["name"].as_str().unwrap().into(),
                                        path: root
                                            .join(b["path"].as_str().unwrap())
                                            .join("fake.md"),
                                        content: c["content"].as_str().unwrap().into(),
                                        frontmatter: HashMap::new(),
                                        is_skill: false,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        skills: vec![],
                        output_styles: vec![],
                        hooks_config: None,
                        mcp_servers: None,
                        lsp_servers: None,
                        dispose: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    PluginRuntimeOptions {
        project_root: root.join("project"),
        pilot_home: root.join("home"),
        builtin_skills_root: (scenario["builtinSkills"] == true)
            .then(|| root.join("builtin-skills")),
        builtin_plugins: builtins,
        builtin_plugins_enabled: scenario["enabled"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_bool().unwrap()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}
async fn replay(id: &str) {
    let fixture = oracle();
    let scenario = fixture["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap();
    let scratch = Scratch::new();
    let root = &scratch.0;
    setup(root, scenario);
    let options = options(root, scenario);
    let paths = PilotExtensionPaths::resolve(&options.project_root, &options.pilot_home);
    let candidates = loader::discover_plugin_paths(&[
        (paths.global_plugins_dir, PluginSource::Global),
        (paths.project_plugins_dir, PluginSource::Project),
    ]);
    let mut dirs = vec![];
    if let Some(dir) = &options.builtin_skills_root {
        dirs.push((dir.clone(), PluginSource::Builtin));
    }
    dirs.extend([
        (paths.global_skills_dir, PluginSource::Global),
        (paths.project_skills_dir, PluginSource::Project),
    ]);
    let skills = loader::discover_skill_paths(&dirs);
    assert_json(
        &normalize(discovered(&candidates), root),
        &scenario["discovered"],
        &format!("{id}: discovered plugins"),
    );
    assert_json(
        &normalize(discovered(&skills), root),
        &scenario["skills"],
        &format!("{id}: discovered skills"),
    );
    let mut attempts = vec![];
    for (candidates, kind) in [(&candidates, "plugin"), (&skills, "skill")] {
        for c in candidates {
            let result = if kind == "plugin" {
                loader::load_plugin_from_path(&c.path, c.source)
            } else {
                loader::load_skill_from_path(&c.path, c.source)
            };
            let mut attempt = capture(result, |p| plugin(&p));
            attempt["path"] = json!(c.path);
            attempt["source"] = json!(c.source.as_str());
            attempt["kind"] = json!(kind);
            attempts.push(attempt);
        }
    }
    let style_attempts: Vec<_> = candidates.iter().map(|c| {
        let mut value = capture(loader::load_plugin_output_styles_from_path(&c.path, c.source), |loaded| json!({"name":loaded.name,"path":loaded.path,"source":loaded.source.as_str(),"outputStyles":loaded.output_styles.iter().map(command).collect::<Vec<_>>()}));
        value["path"] = json!(c.path); value["source"] = json!(c.source.as_str()); value
    }).collect();
    assert_json(
        &normalize(json!(style_attempts), root),
        &scenario["styleAttempts"],
        &format!("{id}: style-only loader"),
    );
    for (index, (actual, expected)) in attempts
        .iter()
        .zip(scenario["attempts"].as_array().unwrap())
        .enumerate()
    {
        assert_json(
            &normalize(actual.clone(), root),
            expected,
            &format!("{id}: load attempt {index}"),
        );
    }
    assert_eq!(
        attempts.len(),
        scenario["attempts"].as_array().unwrap().len()
    );
    assert_json(
        &capture(loader::load_standalone_skill("absent", root), |c| {
            command(&c)
        }),
        &scenario["noSkill"],
        &format!("{id}: absent skill"),
    );
    let runtime = PluginRuntime::new(options);
    let first = runtime.refresh_with_report().await.unwrap();
    assert_json(
        &normalize(report(&first), root),
        &scenario["steps"][0],
        &format!("{id}: refresh"),
    );
    let active = json!(
        runtime
            .snapshot()
            .await
            .iter()
            .map(|p| plugin(p))
            .collect::<Vec<_>>()
    );
    let mut lease = runtime.acquire_command_catalog().await.unwrap();
    let catalog = command_catalog(&lease.contributions.commands);
    assert_json(
        &catalog,
        &scenario["catalog"],
        &format!("{id}: final command catalog"),
    );
    if let Some(invalid) = scenario["invalid"].as_array() {
        for f in invalid {
            write(
                root,
                f["path"].as_str().unwrap(),
                f["text"].as_str().unwrap(),
            );
        }
        let failed = runtime.refresh_with_report().await.unwrap();
        assert_json(
            &normalize(report(&failed), root),
            &scenario["steps"][1],
            &format!("{id}: failed staging"),
        );
        for path in scenario["remove"].as_array().unwrap() {
            fs::remove_dir_all(root.join(path.as_str().unwrap())).unwrap();
        }
        let deleted = runtime.refresh_with_report().await.unwrap();
        assert_json(
            &normalize(report(&deleted), root),
            &scenario["steps"][2],
            &format!("{id}: deleted plugin"),
        );
        // Old generation remains held throughout refresh; release still works.
        assert_json(
            &command_catalog(&lease.contributions.commands),
            &scenario["catalog"],
            "held command catalog",
        );
    }
    lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
    assert_json(
        &normalize(active, root),
        &scenario["active"],
        &format!("{id}: active registry"),
    );
}
macro_rules! scenario {
    ($name:ident,$id:literal) => {
        #[tokio::test]
        async fn $name() {
            replay($id).await
        }
    };
}
scenario!(plugin_loader_empty, "empty-and-missing");
scenario!(plugin_loader_contributions, "valid-contributions");
scenario!(plugin_loader_invalid, "invalid-and-loose-option-types");
scenario!(plugin_loader_priority, "duplicates-and-priority");
scenario!(
    plugin_loader_symlinks_permissions,
    "symlinks-and-unreadable"
);
scenario!(plugin_loader_skills, "skills-root-only");
scenario!(plugin_loader_builtins, "builtin-loading-and-disable");
scenario!(plugin_loader_hooks_paths, "hook-files-and-configured-paths");
scenario!(plugin_loader_order, "discovery-unicode-order");
scenario!(plugin_loader_refresh_rollback, "refresh-staging-rollback");

/// Keep the loader boundary independently testable when the pre-existing
/// registry has a mismatch; these expectations are the raw TS refresh.next.
#[tokio::test]
async fn plugin_loader_staging_oracle() {
    let fixture = oracle();
    for scenario in fixture["scenarios"].as_array().unwrap() {
        let scratch = Scratch::new();
        setup(&scratch.0, scenario);
        let options = options(&scratch.0, scenario);
        let result = loader::load_from_disk(&options).await;
        let actual = json!(result.plugins.iter().map(|p| plugin(p)).collect::<Vec<_>>());
        assert_json(
            &normalize(actual, &scratch.0),
            &scenario["steps"][0]["next"],
            &format!("{}: staged plugins", scenario["id"]),
        );
        let failures = json!(result.staged_failures.iter().map(|f|json!({"path":f.path,"source":f.source.as_str(),"kind":if f.kind==PluginKind::Plugin {"plugin"}else{"skill"}})).collect::<Vec<_>>());
        assert_json(
            &normalize(failures, &scratch.0),
            &scenario["steps"][0]["stagedFailures"],
            &format!("{}: staged failures", scenario["id"]),
        );
    }
}

#[test]
#[ignore = "requires Node 22 and PDRS_BASELINE; explicit two-run generation is recorded in RESULT"]
fn plugin_loader_regenerates_identically() {
    baseline::regenerates_identically("plugin-loader");
}

#[tokio::test]
async fn plugin_loader_server_values_oracle() {
    let fixture = oracle();
    for case in fixture["serverCases"].as_array().unwrap() {
        let scratch = Scratch::new();
        write(&scratch.0, "plugin.json", case["raw"].as_str().unwrap());
        let loaded = loader::load_plugin_from_path(&scratch.0, PluginSource::Global).unwrap();
        assert_eq!(
            pilotdeck_protocol::stringify_json(loaded.mcp_servers.as_ref().unwrap())
                .unwrap()
                .unwrap(),
            case["mcp"].as_str().unwrap()
        );
        assert_eq!(
            pilotdeck_protocol::stringify_json(loaded.lsp_servers.as_ref().unwrap())
                .unwrap()
                .unwrap(),
            case["lsp"].as_str().unwrap()
        );
        let runtime = PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.0.join("project"),
            pilot_home: scratch.0.join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![Arc::new(loaded)],
            builtin_plugins_enabled: Default::default(),
        });
        runtime.refresh().await.unwrap();
        let mcp = JsValue::Object(runtime.mcp_servers().await);
        let lsp = JsValue::Object(runtime.lsp_servers().await);
        assert_eq!(
            pilotdeck_protocol::stringify_json(&mcp).unwrap().unwrap(),
            case["mcp"].as_str().unwrap(),
            "merged MCP must retain order and deep values"
        );
        assert_eq!(
            pilotdeck_protocol::stringify_json(&lsp).unwrap().unwrap(),
            case["lsp"].as_str().unwrap(),
            "merged LSP must retain order and deep values"
        );
        runtime.dispose().await.unwrap();
    }
}

fn command_catalog(
    commands: &[pilotdeck_gateway::runtime::plugins::PluginCommandContribution],
) -> Value {
    json!(
        commands
            .iter()
            .map(|c| {
                let mut v = json!({"name":c.name,"content":c.content,"namespace":c.namespace});
                if let Some(s) = &c.description {
                    v["description"] = json!(s);
                }
                if let Some(s) = &c.argument_hint {
                    v["argumentHint"] = json!(s);
                }
                v
            })
            .collect::<Vec<_>>()
    )
}
