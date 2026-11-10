use super::*;
use pilotdeck_protocol::JsObject;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-native-declarations-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn object(value: &JsValue) -> &JsObject {
    value.as_object().unwrap()
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(values) = value else {
        panic!("expected array")
    };
    values
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    object(value).get(name).unwrap()
}
fn assert_commands(actual: &[LoadedPluginCommand], expected: &JsValue, root: &Path, id: &str) {
    let expected = array(expected);
    assert_eq!(actual.len(), expected.len(), "{id}: contribution count");
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.name, text(field(expected, "name")), "{id}: name");
        assert_eq!(
            actual.path.strip_prefix(root).unwrap().to_string_lossy(),
            text(field(expected, "path")),
            "{id}: path"
        );
        assert_eq!(
            actual.content,
            text(field(expected, "content")),
            "{id}: content"
        );
        assert_eq!(
            JsValue::Bool(actual.is_skill),
            *field(expected, "isSkill"),
            "{id}: isSkill"
        );
        // Frontmatter is an existing HashMap surface, so compare object key
        // order only after sorting it in both the TS-derived fixture and Rust.
        let sorted: std::collections::BTreeMap<_, _> = actual.frontmatter.iter().collect();
        let actual = parse_json(&serde_json::to_string(&sorted).unwrap()).unwrap();
        assert_eq!(actual, *field(expected, "frontmatter"), "{id}: frontmatter");
    }
}

#[test]
fn native_declarations_replay_fixed_ts_loader_oracle() {
    let oracle = parse_json(include_str!(
        "../../../../../tests/fixtures/native-plugin-declarations/oracle.json"
    ))
    .unwrap();
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 15);
    let scratch = Scratch::new();
    for case in cases {
        let id = text(field(case, "id"));
        let root = scratch.0.join(&id);
        let plugin = root.join("disk-name");
        fs::create_dir_all(&plugin).unwrap();
        for file in array(field(case, "files")) {
            let path = root.join(text(field(file, "path")));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text(field(file, "content"))).unwrap();
        }
        for (name, target) in object(field(case, "links")).entries() {
            std::os::unix::fs::symlink(text(target), plugin.join(name.to_utf8().unwrap())).unwrap();
        }
        let expected = field(case, "expectedNative");
        let actual = load_plugin(&plugin, PluginSource::Project, &mut StyleBudget::new());
        if field(expected, "ok") == &JsValue::Bool(false) {
            assert_eq!(
                actual.unwrap_err().message,
                text(field(expected, "error")),
                "{id}"
            );
            continue;
        }
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(actual.name, text(field(expected, "name")), "{id}");
        assert_eq!(actual.source, PluginSource::Project, "{id}");
        assert_commands(&actual.commands, field(expected, "commands"), &root, &id);
        assert_commands(&actual.skills, field(expected, "skills"), &root, &id);
        assert_commands(
            &actual.output_styles,
            field(expected, "outputStyles"),
            &root,
            &id,
        );
        let hooks = actual.hooks_config.map(|settings| {
            let mut object = JsObject::new();
            for (event, matchers) in settings {
                object.insert(
                    event,
                    JsValue::Array(matchers.iter().map(|matcher| matcher.to_value()).collect()),
                );
            }
            JsValue::Object(object)
        });
        assert_eq!(
            hooks.as_ref(),
            object(expected).get("hooksConfig"),
            "{id}: hooks"
        );
        assert_eq!(
            actual.mcp_servers.as_ref(),
            object(expected).get("mcpServers"),
            "{id}: MCP raw strings/order"
        );
        assert_eq!(
            actual.lsp_servers.as_ref(),
            object(expected).get("lspServers"),
            "{id}: LSP declaration"
        );
    }
}

#[test]
fn native_manifest_byte_and_value_budgets_fail_before_projection() {
    let scratch = Scratch::new();
    let plugin = scratch.0.join("p");
    fs::create_dir_all(&plugin).unwrap();
    let raw = r#"{"name":"p","unknown":[1,2,3]}"#;
    fs::write(plugin.join("plugin.json"), raw).unwrap();
    let mut bytes = StyleBudget {
        remaining_bytes: (raw.len() - 1) as u64,
        remaining_entries: 100,
    };
    assert_eq!(
        read_manifest(&plugin, &mut bytes).unwrap_err().code,
        Some("STYLE_RELOAD_LIMIT")
    );
    let mut entries = StyleBudget {
        remaining_bytes: 1024,
        remaining_entries: 5,
    };
    assert_eq!(
        read_manifest(&plugin, &mut entries).unwrap_err().code,
        Some("STYLE_RELOAD_LIMIT")
    );
    // Root + name + array + its three children, including ignored metadata.
    let mut exact = StyleBudget {
        remaining_bytes: raw.len() as u64,
        remaining_entries: 6,
    };
    assert!(read_manifest(&plugin, &mut exact).is_ok());
    assert_eq!((exact.remaining_bytes, exact.remaining_entries), (0, 0));
}

#[test]
fn native_hook_budget_is_not_swallowed_by_ts_ordinary_io_catch() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("hooks.json"), "{\"PreToolUse\":[]}").unwrap();
    let manifest = parse_json(r#"{"name":"p","hooks":"hooks.json"}"#).unwrap();
    let mut budget = StyleBudget {
        remaining_bytes: 1,
        remaining_entries: 100,
    };
    assert_eq!(
        load_native_plugin_hooks_from_manifest(&scratch.0, &manifest, &mut budget)
            .unwrap_err()
            .code,
        Some("STYLE_RELOAD_LIMIT")
    );
}

#[test]
fn native_manifest_and_hook_fifos_are_rejected_without_reading() {
    let scratch = Scratch::new();
    let plugin = scratch.0.join("p");
    fs::create_dir_all(&plugin).unwrap();
    let manifest = plugin.join("plugin.json");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&manifest)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        read_manifest(&plugin, &mut StyleBudget::new())
            .unwrap_err()
            .message,
        "Plugin manifest is not a regular file."
    );
    fs::remove_file(manifest).unwrap();
    let hook = scratch.0.join("outside-hook.json");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(hook)
            .status()
            .unwrap()
            .success()
    );
    let manifest = parse_json(r#"{"name":"p","hooks":"../outside-hook.json"}"#).unwrap();
    assert_eq!(
        load_native_plugin_hooks_from_manifest(&plugin, &manifest, &mut StyleBudget::new())
            .unwrap_err()
            .message,
        "Hook config is not a regular file."
    );
}

#[test]
fn native_discovery_and_manifest_allow_legitimate_configured_aliases() {
    let scratch = Scratch::new();
    let actual = scratch.0.join("real");
    let discovery = scratch.0.join("plugins");
    fs::create_dir_all(&actual).unwrap();
    fs::create_dir_all(&discovery).unwrap();
    fs::write(scratch.0.join("manifest.json"), r#"{"name":"visible"}"#).unwrap();
    std::os::unix::fs::symlink("../manifest.json", actual.join("plugin.json")).unwrap();
    std::os::unix::fs::symlink(&actual, discovery.join("lexical")).unwrap();
    let found = validate_native_style_paths(
        &[(discovery.clone(), PluginSource::Project)],
        &mut StyleBudget::new(),
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, discovery.join("lexical"));
    let loaded = load_plugin(&found[0].path, found[0].source, &mut StyleBudget::new()).unwrap();
    assert_eq!(loaded.name, "visible");
    assert_eq!(loaded.path, discovery.join("lexical"));
}

#[tokio::test]
async fn native_declaration_budget_failure_keeps_generation_and_old_contribution_lease() {
    use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
    let scratch = Scratch::new();
    let project = scratch.0.join("project");
    let home = scratch.0.join("home");
    let plugin = project.join(".pilotdeck/plugins/p");
    fs::create_dir_all(plugin.join("commands")).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(plugin.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    fs::write(plugin.join("commands/a.md"), "RETAINED").unwrap();
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: isolated_builtin_switches(),
    });
    runtime.refresh().await.unwrap();
    let mut lease = runtime.acquire_contribution_snapshot().await.unwrap();
    let raw = format!(
        "{{\"name\":\"p\",\"unknown\":[{}]}}",
        vec!["0"; 10_000].join(",")
    );
    fs::write(plugin.join("plugin.json"), raw).unwrap();
    let failure = runtime.refresh().await.unwrap_err();
    assert_eq!(failure.code, Some("STYLE_RELOAD_LIMIT"));
    assert_eq!(runtime.generation().await, 1);
    assert_eq!(lease.contributions.commands[0].content, "RETAINED");
    assert_eq!(runtime.snapshot().await[0].commands[0].content, "RETAINED");
    lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
}

// These isolated disk-loader fixtures predate the default first-party MCP
// declarations. Disable those declarations so exact budget boundaries count
// only the fixture under test, while the separate builtin tests exercise them.
fn isolated_builtin_switches() -> std::collections::HashMap<String, bool> {
    [
        ("browser-use".into(), false),
        ("funasr".into(), false),
        ("windows-skills".into(), false),
    ]
    .into_iter()
    .collect()
}

fn declaration_options(root: &Path) -> PluginRuntimeOptions {
    PluginRuntimeOptions {
        project_root: root.join("project"),
        pilot_home: root.join("home"),
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: isolated_builtin_switches(),
    }
}

#[test]
fn native_manifest_stage_charges_once_at_the_real_value_boundary() {
    let scratch = Scratch::new();
    let options = declaration_options(&scratch.0);
    let plugin = options.project_root.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&plugin).unwrap();
    // One directory entry + root object + name value + ignored array + N
    // scalar values. 9996 is exactly10000; the old double read rejected it.
    for (n, accepted) in [(9996, true), (9997, false)] {
        let raw = format!(
            "{{\"name\":\"p\",\"ignored\":[{}]}}",
            vec!["0"; n].join(",")
        );
        fs::write(plugin.join("plugin.json"), raw).unwrap();
        let result = stage_native_style_plugins(&options);
        if accepted {
            assert_eq!(
                result
                    .expect("native manifest charged once at exact value boundary")
                    .plugins
                    .len(),
                1
            );
        } else {
            assert_eq!(
                result
                    .err()
                    .expect("native manifest over value boundary rejects before projection")
                    .code,
                Some("STYLE_RELOAD_LIMIT"),
                "native manifest over value boundary rejects before projection"
            );
        }
    }
}

#[test]
fn native_manifest_byte_boundary_is_single_read_and_bounded() {
    use std::io::Write;
    let scratch = Scratch::new();
    let options = declaration_options(&scratch.0);
    let plugin = options.project_root.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&plugin).unwrap();
    let path = plugin.join("plugin.json");
    let mut file = fs::File::create(&path).unwrap();
    let prefix = br#"{"name":"p"}"#;
    file.write_all(prefix).unwrap();
    let mut left = 64 * 1024 * 1024 - prefix.len();
    let spaces = [b' '; 8192];
    while left > 0 {
        let n = left.min(spaces.len());
        file.write_all(&spaces[..n]).unwrap();
        left -= n;
    }
    drop(file);
    assert_eq!(
        stage_native_style_plugins(&options)
            .expect("native manifest exact byte boundary is accepted once")
            .plugins
            .len(),
        1
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b" ")
        .unwrap();
    assert_eq!(
        stage_native_style_plugins(&options)
            .err()
            .expect("native manifest cap plus one must reject")
            .code,
        Some("STYLE_RELOAD_LIMIT"),
        "native manifest cap plus one must reject"
    );
}

#[test]
fn native_manifest_token_preflight_counts_values_before_materialization() {
    for (raw, entries) in [
        (r#"{"key":"value","nested":[true,false,null,1]}"#, 7),
        (r#"{"same":1,"same":2}"#, 3),
        (r#"{"escaped\"key":"a\\\"b"}"#, 2),
    ] {
        let mut budget = StyleBudget {
            remaining_bytes: 0,
            remaining_entries: entries,
        };
        check_manifest_tokens(raw, &mut budget).unwrap();
        assert_eq!(
            budget.remaining_entries, 0,
            "native manifest token count includes values, not object keys"
        );
        let mut below = StyleBudget {
            remaining_bytes: 0,
            remaining_entries: entries - 1,
        };
        assert_eq!(
            check_manifest_tokens(raw, &mut below).unwrap_err().code,
            Some("STYLE_RELOAD_LIMIT")
        );
    }
}

#[tokio::test]
async fn native_ordinary_loader_failures_follow_real_ts_staging() {
    use crate::runtime::plugins::PluginRuntime;
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf, fs::Permissions);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, self.1.clone());
        }
    }
    for id in [
        "empty-root",
        "missing-manifest",
        "manifest-directory",
        "broken-root-link",
        "root-link-loop",
        "hook-directory",
        "hook-empty-path",
        "hook-chmod000",
        "manifest-chmod000",
        "discovery-chmod000",
    ] {
        let scratch = Scratch::new();
        let options = declaration_options(&scratch.0);
        let dir = options.project_root.join(".pilotdeck/plugins");
        let plugin = dir.join("p");
        fs::create_dir_all(&dir).unwrap();
        let mut restore = None;
        if id == "broken-root-link" {
            std::os::unix::fs::symlink("missing", &plugin).unwrap();
        } else if id == "root-link-loop" {
            std::os::unix::fs::symlink("p", &plugin).unwrap();
        } else if id != "empty-root" {
            fs::create_dir_all(&plugin).unwrap();
            if id == "manifest-directory" {
                fs::create_dir(plugin.join("plugin.json")).unwrap();
            } else if id != "missing-manifest" {
                let raw = match id {
                    "hook-directory" => r#"{"name":"p","hooks":"hook-dir"}"#,
                    "hook-empty-path" => r#"{"name":"p","hooks":""}"#,
                    "hook-chmod000" => r#"{"name":"p","hooks":"hooks.json"}"#,
                    _ => r#"{"name":"p"}"#,
                };
                fs::write(plugin.join("plugin.json"), raw).unwrap();
                let mode_path = match id {
                    "hook-directory" => {
                        fs::create_dir(plugin.join("hook-dir")).unwrap();
                        None
                    }
                    "hook-chmod000" => {
                        let file = plugin.join("hooks.json");
                        fs::write(&file, r#"{"PreToolUse":[]}"#).unwrap();
                        Some(file)
                    }
                    "manifest-chmod000" => Some(plugin.join("plugin.json")),
                    "discovery-chmod000" => Some(dir.clone()),
                    _ => None,
                };
                if let Some(path) = mode_path {
                    let original = fs::metadata(&path).unwrap().permissions();
                    restore = Some(Restore(path.clone(), original));
                    fs::set_permissions(path, fs::Permissions::from_mode(0o0)).unwrap();
                }
            }
        }
        let runtime = PluginRuntime::new_native_style_only(options);
        let report = runtime
            .refresh_with_report()
            .await
            .unwrap_or_else(|e| panic!("native ordinary staging {id}: {e}"));
        let loaded = usize::from(id.starts_with("hook-"));
        let failed = usize::from(matches!(
            id,
            "missing-manifest" | "manifest-directory" | "manifest-chmod000"
        ));
        assert_eq!(
            report.next.len(),
            loaded,
            "native ordinary staging {id}: loaded"
        );
        assert_eq!(
            report.staged_failures.len(),
            failed,
            "native ordinary staging {id}: failures"
        );
        if loaded > 0 {
            assert!(
                report.next[0].hooks_config.is_none(),
                "native ordinary hook error yields no hooks"
            );
        }
        drop(restore);
        runtime.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn native_manifest_missing_after_load_keeps_the_real_ts_previous_generation() {
    use crate::runtime::plugins::PluginRuntime;
    let scratch = Scratch::new();
    let options = declaration_options(&scratch.0);
    let plugin = options.project_root.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    let runtime = PluginRuntime::new_native_style_only(options);
    runtime.refresh().await.unwrap();
    fs::remove_file(plugin.join("plugin.json")).unwrap();
    let report = runtime.refresh_with_report().await.unwrap();
    assert_eq!(
        report.generation, 1,
        "native failed existing manifest retains generation"
    );
    assert_eq!(
        report.next.len(),
        1,
        "native failed existing manifest retains plugin"
    );
    assert_eq!(report.staged_failures.len(), 1);
    runtime.dispose().await.unwrap();
}

#[test]
fn native_hook_value_budget_applies_before_json_materialization() {
    let scratch = Scratch::new();
    let options = declaration_options(&scratch.0);
    let plugin = options.project_root.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"p","hooks":"hooks.json"}"#,
    )
    .unwrap();
    // Directory entry + manifest object/name/hooks + hook object/array = 6.
    for (n, accepted) in [(9994, true), (9995, false)] {
        fs::write(
            plugin.join("hooks.json"),
            format!("{{\"ignored\":[{}]}}", vec!["0"; n].join(",")),
        )
        .unwrap();
        let result = stage_native_style_plugins(&options);
        if accepted {
            assert_eq!(
                result
                    .expect("native hook exact value boundary is accepted")
                    .plugins
                    .len(),
                1
            );
        } else {
            assert_eq!(
                result
                    .err()
                    .expect("native hook value budget rejects before projection")
                    .code,
                Some("STYLE_RELOAD_LIMIT"),
                "native hook value budget rejects before projection"
            );
        }
    }
}
