use super::*;
use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::PathBuf};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn authorized_legacy_assets_reach_the_native_catalog_and_read_skill() {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills")
        .canonicalize()
        .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(assets.join("upstream-assets.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["commit"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), 109);
    let mut expected_paths = BTreeSet::new();
    for file in files {
        let path = assets.join(file["path"].as_str().unwrap());
        let bytes = fs::read(&path).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), file["sha256"]);
        if path.file_name().unwrap() == "SKILL.md" {
            expected_paths.insert(path);
        }
    }
    assert_eq!(expected_paths.len(), 32);
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-bundled-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(scratch.0.join("home")).unwrap();
    fs::create_dir(scratch.0.join("project")).unwrap();
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: scratch.0.join("project"),
        pilot_home: scratch.0.join("home"),
        builtin_skills_root: Some(assets),
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    });
    runtime.refresh().await.unwrap();
    let mut lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    let actual_paths = lease
        .contributions
        .skills
        .iter()
        .map(|skill| skill.path.canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_paths, expected_paths,
        "all 32 packaged skills reach the actual native generation"
    );
    let native = NativeSkills::from_contributions(&lease.contributions.skills);
    assert_eq!(native.list_skills().len(), 32);
    let weather = lease
        .contributions
        .skills
        .iter()
        .find(|skill| skill.path.ends_with("weather/SKILL.md"))
        .unwrap();
    let output = native.read(&weather.name.as_str().into());
    assert!(
        output.content.iter().any(|block| {
            block
                .get("text")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .is_some_and(|text| text.contains("wttr.in") && text.contains("weather/SKILL.md"))
        }),
        "bundled weather recipe and its real path reach read_skill"
    );
    lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
}
