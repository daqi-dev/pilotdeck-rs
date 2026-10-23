//! Real TS observations and explicitly approved security/boundedness deviations.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::runtime::plugins::{PluginSource, loader};
use pilotdeck_protocol::{JsValue, stringify_json};
use scratch::Scratch;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn fixture() -> Value {
    let value = baseline::oracle(include_str!("fixtures/plugin-loader/traversal.json"));
    serde_json::from_str(&stringify_json(&value).unwrap().unwrap())
        .expect("UTF-8 traversal records")
}
fn case(id: &str) -> Value {
    fixture()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}
fn commands(commands: &[loader::LoadedPluginCommand], root: &Path) -> Value {
    json!(commands.iter().map(|c| json!({"name":c.name,"path":c.path.to_string_lossy().replace(root.to_str().unwrap(),"<root>"),"content":c.content,"frontmatter":c.frontmatter,"isSkill":c.is_skill})).collect::<Vec<_>>())
}

// Reap on every exit path, including failed try_wait and assertion unwinding.
struct ReapedChild(Child);
impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[cfg(unix)]
fn clique(k: usize) {
    assert_eq!(
        case(&format!("clique-{k}"))["timedOut"],
        true,
        "real TS clique observation"
    );
    let scratch = Scratch::new("484-clique");
    let base = scratch.path().join("commands");
    fs::create_dir(&base).unwrap();
    for i in 0..k {
        let dir = base.join(format!("d{i}"));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("a.md"), "a").unwrap();
    }
    for i in 0..k {
        for j in 0..k {
            if i != j {
                std::os::unix::fs::symlink(format!("../d{j}"), base.join(format!("d{i}/link{j}")))
                    .unwrap();
            }
        }
    }
    let start = Instant::now();
    let mut child = ReapedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "plugin_markdown_clique_child",
                "--ignored",
                "--nocapture",
            ])
            .env("PDRS_PLUGIN_CLIQUE_BASE", &base)
            .env("PDRS_PLUGIN_CLIQUE_SIZE", k.to_string())
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    loop {
        match child.0.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "clique child failed: {status}");
                break;
            }
            Ok(None) => {}
            Err(error) => panic!("clique child wait failed: {error}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "markdown clique exceeded 1s; child killed and reaped"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "markdown clique exceeded 1s"
    );
}
#[test]
#[cfg(unix)]
fn plugin_markdown_clique_03() {
    clique(3);
}
#[test]
#[cfg(unix)]
fn plugin_markdown_clique_07() {
    clique(7);
}
#[test]
#[cfg(unix)]
fn plugin_markdown_clique_10() {
    clique(10);
}
#[test]
#[cfg(unix)]
fn plugin_markdown_clique_20() {
    clique(20);
}
#[test]
#[ignore = "subprocess helper for bounded clique tests"]
fn plugin_markdown_clique_child() {
    let base = std::env::var_os("PDRS_PLUGIN_CLIQUE_BASE").expect("only invoked by parent");
    let k: usize = std::env::var("PDRS_PLUGIN_CLIQUE_SIZE")
        .unwrap()
        .parse()
        .unwrap();
    let commands = loader::load_plugin_commands("clique", Path::new(&base)).unwrap();
    assert_eq!(commands.len(), k, "each directory inode expanded once");
}

#[test]
#[cfg(unix)]
fn plugin_markdown_alias_first_path() {
    let scratch = Scratch::new("484-alias");
    let root = scratch.path().join("aliases");
    fs::create_dir_all(root.join("commands")).unwrap();
    fs::create_dir(root.join("shared")).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    fs::write(root.join("shared/a.md"), "a").unwrap();
    for name in ["left", "right"] {
        std::os::unix::fs::symlink("../shared", root.join("commands").join(name)).unwrap();
    }
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    let mut expected = case("aliases")["result"].as_array().unwrap()[..1].to_vec();
    // The TS direct loader probe takes pluginName="p"; the disk loader names
    // commands by the directory slug. Normalize that input parameter only.
    expected[0]["name"] = json!(
        expected[0]["name"]
            .as_str()
            .unwrap()
            .replacen("p:", "aliases:", 1)
    );
    // Global de-dup drops the second TS alias. Both lexical name and full path
    // of the first encounter must still match the real TS contribution.
    assert_eq!(
        commands(&plugin.commands, scratch.path()),
        json!(expected),
        "first alias retains TS name and path"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_root_boundary() {
    let scratch = Scratch::new("484-boundary");
    let boundary = scratch.path().join("boundary");
    let root = boundary.join("p");
    fs::create_dir_all(boundary.join("p-outside/secret")).unwrap();
    fs::write(boundary.join("p-outside/secret/leak.md"), "SECRET").unwrap();
    fs::write(boundary.join("p-outside/file.md"), "SECRET").unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    for dir in ["commands", "skills", "output-styles"] {
        let base = root.join(dir);
        fs::create_dir(&base).unwrap();
        fs::write(base.join("a.md"), "safe").unwrap();
        for (target, name) in [
            ("../../p-outside/secret", "directory"),
            ("../../p-outside/file.md", "file.md"),
            ("directory", "chain"),
            ("../../p-outside/secret/leak.md", "SKILL.md"),
        ] {
            std::os::unix::fs::symlink(target, base.join(name)).unwrap();
        }
    }
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    let observed = case("boundary-plugin");
    for (key, actual) in [
        ("commands", &plugin.commands),
        ("skills", &plugin.skills),
        ("outputStyles", &plugin.output_styles),
    ] {
        let safe: Vec<_> = observed["result"][key]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["content"] != "SECRET")
            .cloned()
            .collect();
        assert_eq!(
            commands(actual, scratch.path()),
            json!(safe),
            "root boundary skips directory/file/chain leaks: {key}"
        );
    }
    let styles = loader::load_plugin_output_styles_from_path(&root, PluginSource::Global).unwrap();
    assert_eq!(
        commands(&styles.output_styles, scratch.path()),
        commands(&plugin.output_styles, scratch.path()),
        "style-only root boundary"
    );
    // Configured base may itself be an escaping symlink or contain '..'.
    fs::write(root.join("plugin.json"),r#"{"name":"p","commands":"../p-outside","skills":"../p-outside","outputStyles":"../p-outside"}"#).unwrap();
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    assert!(
        plugin.commands.is_empty() && plugin.skills.is_empty() && plugin.output_styles.is_empty(),
        "configured base cannot escape root"
    );
}

#[test]
fn plugin_markdown_entry_budget() {
    assert_eq!(case("entries-4097")["result"]["commandCount"], 4097);
    let scratch = Scratch::new("484-entries");
    let root = scratch.path().join("p");
    let base = root.join("commands");
    fs::create_dir_all(&base).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    for i in 0..4097 {
        fs::write(base.join(format!("{i:05}.md")), "").unwrap();
    }
    assert_eq!(
        loader::load_plugin_commands("p", &base)
            .err()
            .expect("budget error")
            .message,
        "Plugin markdown traversal limit exceeded.",
        "entry budget must fail, not truncate"
    );
    assert!(
        loader::load_plugin_from_path(&root, PluginSource::Global)
            .unwrap()
            .commands
            .is_empty(),
        "configured traversal error discards entire directory"
    );
}
#[test]
fn plugin_markdown_depth_budget() {
    assert_eq!(case("depth-65")["result"].as_array().unwrap().len(), 1);
    let scratch = Scratch::new("484-depth");
    let base = scratch.path().join("commands");
    let mut leaf = base.clone();
    for _ in 0..65 {
        leaf = leaf.join("d");
    }
    fs::create_dir_all(&leaf).unwrap();
    fs::write(leaf.join("a.md"), "a").unwrap();
    assert_eq!(
        loader::load_plugin_commands("p", &base)
            .err()
            .expect("budget error")
            .message,
        "Plugin markdown traversal limit exceeded.",
        "depth budget must fail, not truncate"
    );
}

#[tokio::test]
async fn plugin_mcp_server_names_utf16() {
    // Parse this record with JsValue: serde's String cannot hold lone surrogates.
    let oracle = baseline::oracle(include_str!("fixtures/plugin-loader/traversal.json"));
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("cases")
    };
    let case = cases
        .iter()
        .find(|c| {
            c.get("id")
                .unwrap()
                .as_string()
                .unwrap()
                .equals_str("server-names")
        })
        .unwrap();
    let scratch = Scratch::new("484-names");
    let path = scratch.path().join("home/plugins/p");
    fs::create_dir_all(&path).unwrap();
    let input = case
        .get("inputJson")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    fs::write(
        path.join("plugin.json"),
        format!(r#"{{"name":"p","mcpServers":{input}}}"#),
    )
    .unwrap();
    let runtime = pilotdeck_gateway::runtime::plugins::PluginRuntime::new(
        pilotdeck_gateway::runtime::plugins::PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: Vec::new(),
            builtin_plugins_enabled: Default::default(),
        },
    );
    runtime.refresh().await.unwrap();
    let instructions = runtime.all_mcp_instructions().await;
    let actual = JsValue::Array(
        instructions
            .iter()
            .map(|e| {
                let mut o = pilotdeck_protocol::JsObject::new();
                o.insert("serverName".into(), JsValue::String(e.server_name.clone()));
                o.insert(
                    "instructions".into(),
                    JsValue::String(e.instructions.clone()),
                );
                JsValue::Object(o)
            })
            .collect(),
    );
    assert_eq!(
        stringify_json(&actual).unwrap().unwrap(),
        case.get("resultJson")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap(),
        "server names retain UTF-16 identity and sort order"
    );
    let mut session = runtime
        .acquire_session_contributions(Vec::new())
        .await
        .unwrap();
    let mut legacy = runtime.acquire_contribution_snapshot().await.unwrap();
    assert_eq!(
        session
            .contributions
            .mcp_instructions
            .iter()
            .map(|e| (&e.server_name, &e.instructions))
            .collect::<Vec<_>>(),
        instructions
            .iter()
            .map(|e| (&e.server_name, &e.instructions))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        legacy
            .contributions
            .mcp_instructions
            .iter()
            .map(|e| &e.server_name)
            .collect::<Vec<_>>(),
        instructions
            .iter()
            .map(|e| &e.server_name)
            .collect::<Vec<_>>()
    );
    session.release().await.unwrap();
    legacy.release().await.unwrap();
    runtime.dispose().await.unwrap();
}
