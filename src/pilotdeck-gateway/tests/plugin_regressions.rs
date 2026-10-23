#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{fs, path::PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-484-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn runtime(&self) -> PluginRuntime {
        PluginRuntime::new(PluginRuntimeOptions {
            project_root: self.0.join("project"),
            pilot_home: self.0.join("home"),
            builtin_skills_root: None,
            builtin_plugins: Vec::new(),
            builtin_plugins_enabled: Default::default(),
        })
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn plugin_mcp_utf16_oracle() {
    let oracle = baseline::oracle(include_str!("fixtures/plugin-loader/regressions.json"));
    let JsValue::Array(cases) = oracle.get("instructions").unwrap() else {
        panic!("instructions array")
    };
    for case in cases {
        let scratch = Scratch::new();
        let path = scratch.0.join("home/plugins/p");
        fs::create_dir_all(&path).unwrap();
        // inputJson is a JSON string recorded by TS, including escaped lone
        // surrogates. Pass it through disk, never through serde String.
        let input = case
            .get("inputJson")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        fs::write(
            path.join("plugin.json"),
            format!(r#"{{"name":"p","mcpServers":{{"server":{{"instructions":{input}}}}}}}"#),
        )
        .unwrap();
        let runtime = scratch.runtime();
        runtime.refresh().await.unwrap();
        let actual = JsValue::Array(
            runtime
                .all_mcp_instructions()
                .await
                .into_iter()
                .map(|e| {
                    let mut o = JsObject::new();
                    o.insert("serverName".into(), JsValue::String(e.server_name));
                    o.insert("instructions".into(), JsValue::String(e.instructions));
                    JsValue::Object(o)
                })
                .collect(),
        );
        let expected = case
            .get("resultJson")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        assert_eq!(
            actual,
            parse_json(&expected).unwrap(),
            "{:?}",
            case.get("id")
        );
        assert_eq!(
            stringify_json(&actual).unwrap().unwrap(),
            expected,
            "UTF-16 serialization"
        );
        let mut session = runtime
            .acquire_session_contributions(Vec::new())
            .await
            .unwrap();
        let mut legacy = runtime.acquire_contribution_snapshot().await.unwrap();
        let instructions = runtime.all_mcp_instructions().await;
        assert_eq!(
            session
                .contributions
                .mcp_instructions
                .iter()
                .map(|e| &e.instructions)
                .collect::<Vec<_>>(),
            instructions
                .iter()
                .map(|e| &e.instructions)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            legacy
                .contributions
                .mcp_instructions
                .iter()
                .map(|e| e.instructions.as_ref().unwrap())
                .collect::<Vec<_>>(),
            instructions
                .iter()
                .map(|e| &e.instructions)
                .collect::<Vec<_>>()
        );
        session.release().await.unwrap();
        legacy.release().await.unwrap();
        runtime.dispose().await.unwrap();
    }
}
