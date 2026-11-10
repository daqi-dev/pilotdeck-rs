use super::*;
use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, path::PathBuf};
const ORACLE: &str = include_str!("../../tests/fixtures/native-windows-builtin/oracle.json");
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn output_text(output: ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|v| v.get("text").and_then(JsValue::as_string))
        .map(|v| v.to_utf8().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}
#[tokio::test]
async fn windows_builtin_is_real_cross_platform_content_with_public_disable_and_short_lookup() {
    let golden: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    assert!(
        golden["defaultBuiltinNames"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "windows-skills")
    );
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../skills/.builtin-plugins/windows-skills/skills/powershell-safe-invocation/SKILL.md",
    );
    let bytes = fs::read(source).unwrap();
    assert_eq!(
        hash(&bytes),
        "6748cbc46061399ad2ce3270cfd7ecd7ac37fc11aef18a38183bd6bdf0eb3d2a"
    );
    for enabled in [true, false] {
        let scratch = Scratch(
            std::env::temp_dir().join(format!("pdrs-windows-skill-{}", uuid::Uuid::new_v4())),
        );
        let assets = scratch.0.join("assets");
        let skill = assets
            .join(".builtin-plugins/windows-skills/skills/powershell-safe-invocation/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, &bytes).unwrap();
        let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: scratch.0.join("project"),
            pilot_home: scratch.0.join("home"),
            builtin_skills_root: Some(assets.clone()),
            builtin_plugins: vec![],
            builtin_plugins_enabled: HashMap::from([("windows-skills".into(), enabled)]),
        });
        runtime.refresh().await.unwrap();
        let skills = runtime.all_skills().await;
        let expected = golden["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["enabled"] == enabled)
            .unwrap();
        assert_eq!(skills.len(), expected["skills"].as_array().unwrap().len());
        assert!(
            skills.iter().all(|v| v.name != ".builtin-plugins"
                && v.name != "windows-skills"
                && v.name != "powershell-safe-invocation"),
            "private source is never enumerated as standalone"
        );
        let catalog = NativeSkills::from_contributions(&skills);
        let tools = ToolRegistry::new(None, None);
        catalog.register(&tools);
        assert!(tools.has(&"read_skill".into()));
        let qualified =
            output_text(catalog.read(&"windows-skills:powershell-safe-invocation".into()));
        let short = output_text(catalog.read(&"powershell-safe-invocation".into()));
        if enabled {
            assert_eq!(
                skills[0].name,
                expected["skills"][0]["name"].as_str().unwrap()
            );
            assert_eq!(
                skills[0].description.as_deref(),
                expected["skills"][0]["description"].as_str()
            );
            assert_eq!(skills[0].path, skill);
            assert_eq!(
                hash(skills[0].content.as_bytes()),
                expected["skills"][0]["contentSha256"].as_str().unwrap()
            );
            assert!(qualified.contains("<name>windows-skills:powershell-safe-invocation</name>"));
            assert!(qualified.contains(skill.to_str().unwrap()));
            let body = qualified
                .split_once("</path>\n")
                .unwrap()
                .1
                .strip_suffix("\n</skill>")
                .unwrap();
            assert_eq!(
                hash(body.as_bytes()),
                expected["readQualifiedSha256"].as_str().unwrap()
            );
            assert_eq!(
                hash(short.as_bytes()),
                expected["readShortSha256"].as_str().unwrap()
            );
        } else {
            assert!(
                qualified.contains("not found")
                    && !qualified.contains("# PowerShell Safe Invocation")
            );
            assert!(short.contains("not found"));
        }
        runtime.dispose().await.unwrap();
    }
}
