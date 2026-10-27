//! Real TS observations and explicitly approved security/boundedness deviations.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/exact_receipt.rs"]
mod exact_receipt;
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::runtime::plugins::{PluginSource, loader};
use pilotdeck_protocol::{JsValue, stringify_json};
use scratch::Scratch;
use serde_json::{Value, json};
#[cfg(unix)]
use std::{
    collections::HashSet,
    io::Read,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use std::{fs, path::Path};

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
fn budget_case(id: &str) -> Value {
    let value = baseline::oracle(include_str!("fixtures/plugin-loader/budget.json"));
    let value: Value = serde_json::from_str(&stringify_json(&value).unwrap().unwrap()).unwrap();
    value["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}
fn unbounded_case(id: &str) -> Value {
    let value = baseline::oracle(include_str!("fixtures/plugin-loader/unbounded.json"));
    let value: Value = serde_json::from_str(&stringify_json(&value).unwrap().unwrap()).unwrap();
    value["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}
fn boundary_case(id: &str) -> Value {
    let value = baseline::oracle(include_str!("fixtures/plugin-loader/boundary.json"));
    let value: Value = serde_json::from_str(&stringify_json(&value).unwrap().unwrap()).unwrap();
    value["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}

fn toctou_case(id: &str) -> Value {
    let value = baseline::oracle(include_str!("fixtures/plugin-loader/toctou.json"));
    let value: Value = serde_json::from_str(&stringify_json(&value).unwrap().unwrap()).unwrap();
    value["cases"]
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
#[cfg(unix)]
struct ReapedChild(Child);
#[cfg(unix)]
impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// A generous hang guard, never a performance/linearity acceptance threshold.
// Poll completion before checking the deadline so a descheduled parent accepts
// a child which has already finished. Drop also reaps on wait errors/panics.
#[cfg(unix)]
fn wait_for_traversal(mut child: ReapedChild, child_test: &str, label: &str) {
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("#273 traversal child wait") {
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "{label} hang guard exceeded 20s; child killed and reaped"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = String::new();
    if let Some(mut pipe) = child.0.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.0.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    // #273: libtest exits 0 even when --exact matches nothing after a rename;
    // only the libtest receipt proves the traversal child really ran its test.
    assert!(
        exact_receipt::single_pass(&status, child_test, &stdout),
        "#273 traversal rerun matched no test: {label}: {status}\n{stdout}\n{stderr}"
    );
}

#[cfg(unix)]
fn assert_directory_expansions(commands: &[loader::LoadedPluginCommand], unique: usize) {
    use std::os::unix::fs::MetadataExt;
    // These graph fixtures put exactly one Markdown file in each non-root
    // physical directory. Each completed expansion therefore emits one item;
    // the empty root is expanded once separately. Count work, never elapsed time.
    assert_eq!(
        commands.len(),
        unique,
        "one contribution per directory expansion"
    );
    let identities: HashSet<_> = commands
        .iter()
        .map(|command| {
            let metadata = fs::metadata(command.path.parent().unwrap()).unwrap();
            (metadata.dev(), metadata.ino())
        })
        .collect();
    assert_eq!(
        identities.len(),
        unique,
        "each physical directory expanded once"
    );
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
    let child = ReapedChild(
        Command::new(std::env::current_exe().expect("#273 clique child path"))
            .args([
                "--exact",
                "plugin_markdown_clique_child",
                "--ignored",
                "--nocapture",
            ])
            .env("PDRS_PLUGIN_CLIQUE_BASE", &base)
            .env("PDRS_PLUGIN_CLIQUE_SIZE", k.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("#273 clique child spawn"),
    );
    wait_for_traversal(child, "plugin_markdown_clique_child", "markdown clique");
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
#[cfg(unix)]
#[ignore = "subprocess helper for bounded clique tests"]
fn plugin_markdown_clique_child() {
    let base = std::env::var_os("PDRS_PLUGIN_CLIQUE_BASE").expect("only invoked by parent");
    let k: usize = std::env::var("PDRS_PLUGIN_CLIQUE_SIZE")
        .unwrap()
        .parse()
        .unwrap();
    let commands = loader::load_plugin_commands("clique", Path::new(&base)).unwrap();
    assert_directory_expansions(&commands, k);
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

#[cfg(unix)]
fn boundary_contributions(root: &Path, scratch: &Path) -> Value {
    let plugin = loader::load_plugin_from_path(root, PluginSource::Global).unwrap();
    let styles = loader::load_plugin_output_styles_from_path(root, PluginSource::Global).unwrap();
    json!({
        "commands": commands(&plugin.commands, scratch),
        "skills": commands(&plugin.skills, scratch),
        "outputStyles": commands(&plugin.output_styles, scratch),
        "styleOnly": commands(&styles.output_styles, scratch),
    })
}

#[test]
#[cfg(unix)]
fn plugin_markdown_configured_base_symlink() {
    let scratch = Scratch::new("484-base-symlink");
    let root = scratch.path().join("base-symlink");
    fs::create_dir(&root).unwrap();
    fs::create_dir(scratch.path().join("outside")).unwrap();
    fs::write(scratch.path().join("outside/SKILL.md"), "SECRET").unwrap();
    fs::write(
        root.join("plugin.json"),
        r#"{"name":"p","commands":"linked","skills":"linked","outputStyles":"linked"}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink("../outside", root.join("linked")).unwrap();
    let mut expected = boundary_case("configured-base-symlink")["result"].clone();
    // ROOT-001 projects out the real TS contributions escaping the plugin.
    for family in expected.as_object_mut().unwrap().values_mut() {
        assert_eq!(
            family.as_array().unwrap().len(),
            1,
            "TS observes base symlink escape"
        );
        family
            .as_array_mut()
            .unwrap()
            .retain(|c| c["content"] != "SECRET");
    }
    assert_eq!(
        boundary_contributions(&root, scratch.path()),
        expected,
        "configured base symlink stays inside plugin root across four loaders"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_plugin_root_symlink() {
    let scratch = Scratch::new("484-root-symlink");
    let real = scratch.path().join("real-root");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    for dir in ["commands", "skills", "output-styles"] {
        fs::create_dir(real.join(dir)).unwrap();
        fs::write(real.join(dir).join("SKILL.md"), "safe").unwrap();
    }
    let root = scratch.path().join("root-symlink");
    std::os::unix::fs::symlink("real-root", &root).unwrap();
    assert_eq!(
        boundary_contributions(&root, scratch.path()),
        boundary_case("plugin-root-symlink")["result"],
        "plugin root symlink retains TS lexical contributions across four loaders"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_absolute_configured_path() {
    let scratch = Scratch::new("484-absolute");
    let root = scratch.path().join("absolute");
    let observed = boundary_case("absolute-configured-path");
    // Use the recorded /private/tmp absolute operand unchanged. All fixture
    // writes still go below our owned root, as Node path.join does.
    let absolute = observed["input"]["configured"].as_str().unwrap();
    let inside = root.join(absolute.trim_start_matches('/'));
    fs::create_dir_all(&inside).unwrap();
    fs::write(inside.join("SKILL.md"), "safe absolute").unwrap();
    fs::write(
        root.join("plugin.json"),
        json!({"name":"p","commands":absolute,"skills":absolute,"outputStyles":absolute})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        boundary_contributions(&root, scratch.path()),
        observed["result"],
        "absolute configuration follows TS join across four loaders"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_outside_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o600));
        }
    }
    let scratch = Scratch::new("484-outside-denied");
    let root = scratch.path().join("denied-plugin");
    fs::create_dir_all(root.join("items")).unwrap();
    fs::create_dir(scratch.path().join("outside")).unwrap();
    let denied = scratch.path().join("outside/denied.md");
    fs::write(&denied, "SECRET denied").unwrap();
    fs::write(root.join("items/SKILL.md"), "safe").unwrap();
    fs::write(
        root.join("plugin.json"),
        r#"{"name":"p","commands":"items","skills":"items","outputStyles":"items"}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink("../../outside/denied.md", root.join("items/denied.md")).unwrap();
    let _restore = Restore(denied.clone());
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).unwrap();
    assert_eq!(
        fs::read(&denied).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "native permission oracle"
    );
    for family in boundary_case("outside-unreadable-file")["result"]
        .as_object()
        .unwrap()
        .values()
    {
        assert!(
            family.as_array().unwrap().is_empty(),
            "TS discards configured directory on outside read error"
        );
    }
    assert_eq!(
        boundary_contributions(&root, scratch.path()),
        boundary_case("outside-unreadable-file-safe")["result"],
        "ROOT-001 retains safe contributions when outside Markdown is unreadable"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_toctou_races_skip_replaced_entries() {
    // The raced Rust loads are replayed against `control` by the deterministic
    // unit tests in `loader/markdown/tests.rs` (hook fires on sub/b.md itself).
    // Here: the recorded TS facts that make each race a divergence at all.
    for (id, read) in [
        ("leaf-symlink", "SECRET"),
        ("leaf-hardlink", "SECRET"),
        ("ancestor-symlink", "SECRET"),
        ("leaf-moved-symlink", "inside"),
        ("ancestor-moved-symlink", "inside"),
    ] {
        let case = toctou_case(id);
        let contents = |key: &str| -> Vec<Value> {
            case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["content"].clone())
                .collect()
        };
        assert_eq!(
            contents("checked"),
            [json!("safe"), json!("inside")],
            "{id}"
        );
        assert_eq!(contents("replaced"), [json!("safe"), json!(read)], "{id}");
        assert_eq!(contents("control"), [json!("safe")], "{id}");
    }
}

#[test]
#[cfg(unix)]
fn plugin_markdown_fifo_is_skipped_without_blocking() {
    let case = toctou_case("static-fifo");
    assert_eq!(case["fifo"]["timedOut"], true, "TS FIFO blocks");
    let scratch = Scratch::new("564-fifo");
    let base = scratch.path().join("p/commands");
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("a.md"), "safe").unwrap();
    let fifo = base.join("pipe.md");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let result = loader::load_plugin_commands("p", &base).unwrap();
    // Rust skips FIFO without blocking, reads safe sibling
    assert_eq!(result.len(), 1, "FIFO skipped without blocking");
    assert_eq!(result[0].content, "safe", "safe sibling read");
}

#[test]
#[cfg(unix)]
fn plugin_markdown_toctou_hardlink_has_same_identity() {
    let case = toctou_case("leaf-hardlink");
    assert_eq!(
        case["replaced"].as_array().unwrap().len(),
        2,
        "TS reads hardlink"
    );
    assert!(
        case["replaced"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["content"] == "SECRET"),
        "TS via hardlink"
    );
    let scratch = Scratch::new("564-hardlink");
    let base = scratch.path().join("p/commands");
    let sub = base.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir(scratch.path().join("outside")).unwrap();
    let safe = base.join("a.md");
    let leaf = sub.join("b.md");
    let outside = scratch.path().join("outside/secret.md");
    fs::write(&safe, "safe").unwrap();
    fs::write(&outside, "SECRET").unwrap();
    fs::hard_link(&outside, &leaf).unwrap();
    let result = loader::load_plugin_commands("p", &base).unwrap();
    // Hardlinks share (dev, ino), so the identity check passes: same file.
    assert_eq!(
        result.len(),
        2,
        "hardlink has same identity as checked path"
    );
    assert!(
        result.iter().any(|c| c.content == "SECRET"),
        "hardlink content read"
    );
}

#[test]
fn plugin_markdown_entries_match_ts() {
    let observed = case("entries-4097");
    let scratch = Scratch::new("484-entries");
    let root = scratch.path().join("p");
    let base = root.join("commands");
    fs::create_dir_all(&base).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"p"}"#).unwrap();
    for i in 0..4097 {
        fs::write(base.join(format!("{i:05}.md")), "").unwrap();
    }
    for loaded in [
        loader::load_plugin_commands("p", &base).expect("wide tree succeeds like TS"),
        loader::load_plugin_from_path(&root, PluginSource::Global)
            .unwrap()
            .commands,
    ] {
        assert_eq!(
            loaded.len() as u64,
            observed["result"]["commandCount"].as_u64().unwrap(),
            "all Markdown candidates survive like TS"
        );
    }
}
#[test]
fn plugin_markdown_deep_tree_matches_ts() {
    let scratch = Scratch::new("484-depth");
    let base = scratch.path().join("depth");
    let mut leaf = base.clone();
    for _ in 0..65 {
        leaf = leaf.join("d");
    }
    fs::create_dir_all(&leaf).unwrap();
    fs::write(leaf.join("a.md"), "a").unwrap();
    let loaded = loader::load_plugin_commands("p", &base).expect("deep tree succeeds like TS");
    assert_eq!(
        commands(&loaded, scratch.path()),
        case("depth-65")["result"],
        "deep acyclic tree matches complete TS contribution"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_non_markdown_assets() {
    let observed = budget_case("node-modules-assets");
    let scratch = Scratch::new("484-assets");
    let root = scratch.path().join("assets-plugin");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"assets-plugin"}"#).unwrap();
    let count = observed["input"]["skillCount"].as_u64().unwrap();
    let assets = observed["input"]["assetsPerSkill"].as_u64().unwrap();
    for i in 0..count {
        let asset_dir = root.join(format!("assets-{i}"));
        let skill = root.join(format!("skills/skill-{i}"));
        fs::create_dir(&asset_dir).unwrap();
        fs::create_dir_all(&skill).unwrap();
        for j in 0..assets {
            fs::write(asset_dir.join(format!("{j}.js")), "asset").unwrap();
        }
        fs::write(skill.join("SKILL.md"), format!("skill-{i}")).unwrap();
        std::os::unix::fs::symlink(format!("../../assets-{i}"), skill.join("node_modules"))
            .unwrap();
    }
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    assert_eq!(
        commands(&plugin.skills, scratch.path()),
        observed["result"],
        "non-Markdown assets match TS"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_configured_deep_node_modules() {
    let observed = unbounded_case("configured-deep");
    let scratch = Scratch::new("484-configured-deep");
    let root = scratch.path().join("configured-deep");
    let mut leaf = root.join("deep");
    for _ in 0..observed["input"]["depth"].as_u64().unwrap() {
        leaf = leaf.join("d");
    }
    fs::create_dir_all(&leaf).unwrap();
    let data = root.join("deep-data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("SKILL.md"), "deep").unwrap();
    std::os::unix::fs::symlink(&data, leaf.join("node_modules")).unwrap();
    fs::write(
        root.join("plugin.json"),
        r#"{"name":"p","commands":"deep","skills":"deep","outputStyles":"deep"}"#,
    )
    .unwrap();
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    let styles = loader::load_plugin_output_styles_from_path(&root, PluginSource::Global).unwrap();
    for (key, loaded) in [
        ("commands", &plugin.commands),
        ("skills", &plugin.skills),
        ("outputStyles", &plugin.output_styles),
        ("styleOnly", &styles.output_styles),
    ] {
        assert_eq!(
            commands(loaded, scratch.path()),
            observed["result"][key],
            "deep node_modules matches TS across configured loaders: {key}"
        );
    }
}

// The captured inventory preserves real dependency paths and Markdown bytes.
// See dependency-tree.py: no writes go through any node_modules path.
#[test]
#[cfg(unix)]
fn plugin_markdown_real_node_modules() {
    use sha2::{Digest, Sha256};
    let observed = unbounded_case("real-node-modules");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-loader");
    let snapshot = fixtures.join("dependency-tree.json.gz");
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&snapshot).unwrap())),
        observed["input"]["snapshotSha256"].as_str().unwrap(),
        "real dependency inventory hash"
    );
    let scratch = Scratch::new("484-real-dependencies");
    let root = scratch.path().join("real-plugin");
    let vendor = root.join("skills/s0/vendor");
    fs::create_dir_all(&vendor).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"real-plugin"}"#).unwrap();
    fs::write(root.join("skills/s0/SKILL.md"), "s0").unwrap();
    fs::create_dir(root.join("skills/s1")).unwrap();
    fs::write(root.join("skills/s1/SKILL.md"), "s1").unwrap();
    let output = Command::new("python3")
        .arg(fixtures.join("dependency-tree.py"))
        .arg("materialize")
        .arg(snapshot)
        .arg(root.join("dependency-data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "real tree materialization: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::os::unix::fs::symlink(
        String::from_utf8(output.stdout).unwrap().trim(),
        vendor.join("node_modules"),
    )
    .unwrap();
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    assert_eq!(
        commands(&plugin.skills, scratch.path()),
        observed["result"],
        "real node_modules skills match TS"
    );
}

#[test]
#[cfg(unix)]
fn plugin_markdown_readme_and_js() {
    let observed = unbounded_case("readme-and-js");
    let scratch = Scratch::new("484-readme-js");
    let root = scratch.path().join("mixed-plugin");
    let assets = root.join("dependency-data");
    fs::create_dir_all(&assets).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"mixed-plugin"}"#).unwrap();
    let input = &observed["input"];
    for i in 0..input["readmeCount"].as_u64().unwrap() {
        let package = assets.join(format!("p{i:05}"));
        fs::create_dir(&package).unwrap();
        fs::write(package.join("README.md"), "readme").unwrap();
    }
    for i in 0..input["jsCount"].as_u64().unwrap() {
        fs::write(assets.join(format!("asset-{i:05}.js")), "asset").unwrap();
    }
    for i in 0..input["skillCount"].as_u64().unwrap() {
        let skill = root.join(format!("skills/s{i}"));
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), format!("s{i}")).unwrap();
    }
    // Only one lexical copy: no alias semantics participate in this budget test.
    std::os::unix::fs::symlink("../../dependency-data", root.join("skills/s0/node_modules"))
        .unwrap();
    let plugin = loader::load_plugin_from_path(&root, PluginSource::Global).unwrap();
    assert_eq!(
        commands(&plugin.skills, scratch.path()),
        observed["result"],
        "5000 README and 54000 JS preserve TS skills"
    );
}

#[cfg(unix)]
fn alias_dag(base: &Path, levels: usize) {
    fs::create_dir(base).unwrap();
    for i in 0..=levels {
        let dir = base.join(format!("d{i:02}"));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("z.md"), format!("node-{i}")).unwrap();
        if i < levels {
            for name in ["a", "b"] {
                std::os::unix::fs::symlink(format!("../d{:02}", i + 1), dir.join(name)).unwrap();
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn plugin_markdown_alias_dag() {
    let observed = budget_case("alias-dag");
    let levels = observed["input"]["levels"].as_u64().unwrap() as usize;
    let scratch = Scratch::new("484-dag");
    let base = scratch.path().join("dag");
    alias_dag(&base, levels);
    let loaded = loader::load_plugin_commands("p", &base)
        .expect("alias DAG must use linear physical-directory work");
    assert!(observed["result"]["commandCount"].as_u64().unwrap() > 4096);
    assert_eq!(
        commands(&loaded, scratch.path()),
        observed["result"]["firstVisitCommands"],
        "alias DAG retains each physical contribution's first TS path"
    );
    // A larger DAG would have over 67 million TS paths. Run in a reaped
    // subprocess so removing de-duplication cannot wedge the test process.
    let base = scratch.path().join("deep-dag");
    alias_dag(&base, 24);
    let child = ReapedChild(
        Command::new(std::env::current_exe().expect("#273 DAG child path"))
            .args(["--exact", "plugin_markdown_alias_dag_child", "--ignored"])
            .env("PDRS_PLUGIN_DAG_BASE", &base)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("#273 DAG child spawn"),
    );
    wait_for_traversal(child, "plugin_markdown_alias_dag_child", "alias DAG");
}

#[test]
#[cfg(unix)]
#[ignore = "subprocess helper for bounded alias DAG test"]
fn plugin_markdown_alias_dag_child() {
    let base = std::env::var_os("PDRS_PLUGIN_DAG_BASE").expect("only invoked by parent");
    let commands = loader::load_plugin_commands("p", Path::new(&base)).unwrap();
    assert_directory_expansions(&commands, 25);
}

#[test]
#[cfg(unix)]
fn plugin_markdown_simple_cycle() {
    let observed = budget_case("simple-cycle");
    let scratch = Scratch::new("484-simple-cycle");
    let base = scratch.path().join("simple-cycle");
    fs::create_dir(&base).unwrap();
    for (name, target) in [("a", "b"), ("b", "a")] {
        let dir = base.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("z.md"), name).unwrap();
        std::os::unix::fs::symlink(format!("../{target}"), dir.join("link")).unwrap();
    }
    let loaded = loader::load_plugin_commands("p", &base).unwrap();
    let mut actual: Vec<_> = loaded.iter().map(|c| c.content.clone()).collect();
    let mut expected: Vec<_> = observed["result"]["firstVisitCommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["content"].as_str().unwrap().to_owned())
        .collect();
    actual.sort();
    expected.sort();
    assert!(observed["result"]["commandCount"].as_u64().unwrap() > expected.len() as u64);
    assert_eq!(
        actual, expected,
        "simple cycle removes TS ELOOP-truncated duplicates"
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
