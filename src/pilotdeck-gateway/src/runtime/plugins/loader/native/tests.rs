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
        builtin_plugins_enabled: Default::default(),
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
