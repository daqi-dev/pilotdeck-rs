#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

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
        fs::remove_dir_all(&self.0).unwrap();
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
                    o.insert("serverName".into(), JsValue::String(e.server_name.into()));
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

// A process boundary is essential: tokio::timeout cannot cancel spawn_blocking
// traversal, and dropping its runtime would otherwise hang the test harness.
#[test]
#[cfg(unix)]
fn plugin_markdown_cycle_is_bounded() {
    let scratch = Scratch::new();
    let path = scratch.0.join("home/plugins/cycle");
    fs::create_dir_all(path.join("commands")).unwrap();
    fs::write(path.join("plugin.json"), r#"{"name":"cycle"}"#).unwrap();
    fs::write(path.join("commands/a.md"), "a").unwrap();
    for name in ["link0", "link1"] {
        std::os::unix::fs::symlink("..", path.join("commands").join(name)).unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "plugin_markdown_cycle_child",
            "--ignored",
            "--nocapture",
        ])
        .env("PDRS_PLUGIN_CYCLE_ROOT", &scratch.0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "cycle child failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("markdown cycle exceeded 3s; child killed and reaped");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "subprocess helper for bounded cycle test"]
fn plugin_markdown_cycle_child() {
    let root = std::env::var_os("PDRS_PLUGIN_CYCLE_ROOT").expect("only invoked by cycle parent");
    let runtime = PluginRuntime::new(PluginRuntimeOptions {
        project_root: PathBuf::from(&root).join("project"),
        pilot_home: PathBuf::from(root).join("home"),
        builtin_skills_root: None,
        builtin_plugins: Vec::new(),
        builtin_plugins_enabled: Default::default(),
    });
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let report = runtime.refresh_with_report().await.unwrap();
        assert!(report.staged_failures.is_empty());
        assert_eq!(report.next.len(), 1);
        assert_eq!(
            report.next[0].commands.len(),
            1,
            "cyclic subtree must be skipped"
        );
        assert_eq!(report.next[0].commands[0].name, "cycle:a");
        runtime.dispose().await.unwrap();
    });
}

#[test]
#[cfg(unix)]
fn plugin_markdown_acyclic_aliases_are_preserved() {
    let scratch = Scratch::new();
    let commands = scratch.0.join("commands");
    fs::create_dir(&commands).unwrap();
    fs::create_dir(scratch.0.join("shared")).unwrap();
    fs::write(scratch.0.join("shared/a.md"), "a").unwrap();
    for name in ["left", "right"] {
        std::os::unix::fs::symlink("../shared", commands.join(name)).unwrap();
    }
    let result =
        pilotdeck_gateway::runtime::plugins::loader::load_plugin_commands("alias", &commands)
            .unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/plugin-loader/regressions.json")).unwrap();
    assert_eq!(
        serde_json::json!(result.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()),
        fixture["aliases"]
    );
}
