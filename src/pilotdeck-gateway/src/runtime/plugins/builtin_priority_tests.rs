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
async fn actual_builtin_content_same_key_override_disabled_and_source_priority_match_fixed_ts() {
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
        let builtin = stage_native_builtin_skills(
            &options(&root, Some(assets)),
            &mut StyleBudget::native_skills(),
        )
        .unwrap();
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
fn missing_builtin_assets_publish_no_empty_placeholder() {
    let scratch = Scratch::new();
    let result = stage_native_builtin_skills(
        &options(&scratch.0, Some(scratch.0.join("missing"))),
        &mut StyleBudget::native_skills(),
    )
    .unwrap();
    assert!(result.plugins.is_empty());
}
