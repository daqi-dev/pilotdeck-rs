use super::*;
use crate::runtime::plugins::PluginRuntime;
use serde_json::{Value, json};
use std::collections::HashMap;
const ORACLE: &str =
    include_str!("../../../tests/fixtures/native-builtin-skill-priority/oracle.json");
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-builtin-skill-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn skill(path: &Path, name: &str, body: &str) {
    fs::create_dir_all(path).unwrap();
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {body} description\n---\n{body}\n"),
    )
    .unwrap();
}
fn options(root: &Path, assets: Option<PathBuf>) -> PluginRuntimeOptions {
    PluginRuntimeOptions {
        project_root: root.join("project"),
        pilot_home: root.join("home"),
        builtin_skills_root: assets,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: HashMap::new(),
    }
}
fn golden(mode: &str) -> Value {
    serde_json::from_str::<Value>(ORACLE).unwrap()["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["mode"] == mode)
        .unwrap()
        .clone()
}
fn normalize(value: Value, root: &Path) -> Value {
    serde_json::from_str(&value.to_string().replace(root.to_str().unwrap(), "<root>")).unwrap()
}

#[tokio::test]
async fn programmatic_builtin_same_key_override_disabled_and_priority_match_fixed_ts() {
    let scratch = Scratch::new();
    for mode in [
        "builtin-only",
        "same-key-disk",
        "disabled-builtin-keeps-disk",
        "project-wins",
    ] {
        let root = scratch.0.join(mode);
        let assets = root.join("assets");
        let disk = root.join("disk");
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        skill(
            &assets.join("browser-use"),
            "browser-use",
            "BUILTIN CONTENT",
        );
        // This oracle explicitly tests a caller-supplied programmatic builtin
        // with a standalone skill. Production browser-use has a distinct real
        // browser-use-install asset and MCP, tested separately below.
        let builtin = LoadFromDiskResult {
            plugins: vec![Arc::new(
                load_skill_from_path(&assets.join("browser-use"), PluginSource::Builtin).unwrap(),
            )],
            staged_failures: Vec::new(),
        };
        assert_eq!(builtin.plugins.len(), 1);
        assert_eq!(
            builtin.plugins[0].skills[0].content, "BUILTIN CONTENT\n",
            "real bounded builtin asset bytes, not an empty identity"
        );
        let mut opts = options(
            &root,
            if mode == "builtin-only" {
                None
            } else {
                Some(disk.clone())
            },
        );
        opts.builtin_plugins = builtin.plugins;
        if mode != "builtin-only" {
            for (name, body) in [
                ("a-first", "FIRST CONTENT"),
                ("browser-use", "DISK CONTENT"),
                ("z-last", "LAST CONTENT"),
            ] {
                skill(&disk.join(name), name, body);
            }
        }
        if mode == "project-wins" {
            skill(
                &root.join("home/skills/browser-use"),
                "browser-use",
                "GLOBAL CONTENT",
            );
            skill(
                &root.join("project/.pilotdeck/skills/browser-use"),
                "browser-use",
                "PROJECT CONTENT",
            );
        }
        if mode == "disabled-builtin-keeps-disk" {
            opts.builtin_plugins_enabled
                .insert("browser-use".into(), false);
        }
        // The shared registry owns slot retention/name-source dedup, just as in TS.
        let runtime = PluginRuntime::new(opts);
        runtime.refresh().await.unwrap();
        let skills = runtime.all_skills().await;
        let expected = golden(mode);
        let actual=json!(skills.iter().map(|s|json!({"name":s.name,"path":s.path,"description":s.description,"content":s.content})).collect::<Vec<_>>());
        assert_eq!(
            normalize(actual, &scratch.0),
            expected["skills"],
            "{mode}: actual catalog including selected path/content/description"
        );
        for selected in &skills {
            assert_eq!(
                selected.content,
                expected["read"][&selected.name].as_str().unwrap(),
                "{mode}: fixed TS loadSkillPrompt bytes selected by read_skill"
            );
        }
        runtime.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn bounded_native_generation_keeps_builtin_slot_with_real_standalone_content() {
    let scratch = Scratch::new();
    let root = scratch.0.join("same-key-disk");
    let disk = root.join("disk");
    for (name, body) in [
        ("a-first", "FIRST CONTENT"),
        ("browser-use", "DISK CONTENT"),
        ("z-last", "LAST CONTENT"),
    ] {
        skill(&disk.join(name), name, body);
    }
    let opts = options(&root, Some(disk));
    let runtime = PluginRuntime::new_native_style_only(opts);
    runtime.refresh().await.unwrap();
    let actual=json!(runtime.all_skills().await.iter().map(|s|json!({"name":s.name,"path":s.path,"description":s.description,"content":s.content})).collect::<Vec<_>>());
    assert_eq!(
        normalize(actual, &scratch.0),
        golden("same-key-disk")["skills"]
    );
    runtime.refresh().await.unwrap();
    assert_eq!(runtime.all_skills().await[0].content, "DISK CONTENT\n");
    runtime.dispose().await.unwrap();
}

#[tokio::test]
async fn disabling_builtin_keeps_standalone_and_deleting_asset_retires_its_skill() {
    let scratch = Scratch::new();
    let root = scratch.0.join("disabled-builtin-keeps-disk");
    let disk = root.join("disk");
    for (name, body) in [
        ("a-first", "FIRST CONTENT"),
        ("browser-use", "DISK CONTENT"),
        ("z-last", "LAST CONTENT"),
    ] {
        skill(&disk.join(name), name, body);
    }
    let mut opts = options(&root, Some(disk.clone()));
    opts.builtin_plugins_enabled
        .insert("browser-use".into(), false);
    let runtime = PluginRuntime::new_native_style_only(opts);
    runtime.refresh().await.unwrap();
    let actual=json!(runtime.all_skills().await.iter().map(|s|json!({"name":s.name,"path":s.path,"description":s.description,"content":s.content})).collect::<Vec<_>>());
    assert_eq!(
        normalize(actual, &scratch.0),
        golden("disabled-builtin-keeps-disk")["skills"]
    );
    fs::remove_dir_all(disk.join("browser-use")).unwrap();
    runtime.refresh().await.unwrap();
    assert!(
        runtime
            .all_skills()
            .await
            .iter()
            .all(|s| s.name != "browser-use")
    );
    runtime.dispose().await.unwrap();
}

#[test]
fn missing_skill_assets_still_publish_real_mcp_consumers() {
    let scratch = Scratch::new();
    let result = stage_native_builtin_skills(
        &options(&scratch.0, Some(scratch.0.join("missing"))),
        &mut StyleBudget::native_skills(),
    )
    .unwrap();
    assert_eq!(
        result
            .plugins
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["browser-use", "funasr"]
    );
    assert!(
        result
            .plugins
            .iter()
            .all(|p| p.skills.is_empty() && p.mcp_servers.is_some())
    );
}

#[tokio::test]
async fn actual_browser_builtin_asset_mcp_disable_and_whole_plugin_override() {
    let scratch = Scratch::new();
    let assets = scratch.0.join("assets");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/.builtin-plugins/browser-use/skills/browser-use-install/SKILL.md");
    let target = assets.join(".builtin-plugins/browser-use/skills/browser-use-install");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("SKILL.md"), fs::read(source).unwrap()).unwrap();
    let mut opts = options(&scratch.0, Some(assets.clone()));
    opts.builtin_plugins_enabled
        .insert("windows-skills".into(), false);
    let runtime = PluginRuntime::new_native_style_only(opts);
    runtime.refresh().await.unwrap();
    let skills = runtime.all_skills().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "browser-use:browser-use-install");
    assert!(skills[0].content.contains("# Browser-Use Install"));
    {
        use sha2::Digest as _;
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(skills[0].content.as_bytes())),
            "7f08d42435047c552676a5a03ab92b32696f7ad1e22f574186dcc6567ed0ffd3",
            "actual fixed TS builtin loadSkillPrompt body"
        );
    }

    let servers = runtime.mcp_servers().await;
    assert!(servers.get_property("browser-use").is_some());
    assert!(servers.get_property("funasr").is_some());
    runtime.dispose().await.unwrap();
    let mut opts = options(&scratch.0, Some(assets.clone()));
    opts.builtin_plugins_enabled
        .insert("windows-skills".into(), false);
    opts.builtin_plugins_enabled
        .insert("browser-use".into(), false);
    let runtime = PluginRuntime::new_native_style_only(opts);
    runtime.refresh().await.unwrap();
    assert!(runtime.all_skills().await.is_empty());
    assert!(
        runtime
            .mcp_servers()
            .await
            .get_property("browser-use")
            .is_none()
    );
    runtime.dispose().await.unwrap();
    let mut opts = options(&scratch.0, Some(assets.clone()));
    opts.builtin_plugins_enabled
        .insert("windows-skills".into(), false);
    skill(
        &assets.join("browser-use"),
        "browser-use",
        "OVERRIDE BY REAL STANDALONE",
    );
    let runtime = PluginRuntime::new_native_style_only(opts);
    runtime.refresh().await.unwrap();
    let skills = runtime.all_skills().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "browser-use");
    assert!(skills[0].content.contains("OVERRIDE BY REAL STANDALONE"));
    assert!(
        runtime
            .mcp_servers()
            .await
            .get_property("browser-use")
            .is_none()
    );
    assert!(runtime.mcp_servers().await.get_property("funasr").is_some());
    runtime.dispose().await.unwrap();
}
