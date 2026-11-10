use super::*;
use crate::runtime::context::PromptAssembler;
use std::future::Future;
#[test]
fn disabled_skills_do_not_register_execution() {
    let registry = ToolRegistry::default();
    NativeSkills::disabled().register(&registry);
    crate::negative_assert!(
        !registry.has(&"read_skill".into()),
        "#1231 disabled skills do not enable execution"
    );
    NativeSkills::default().register(&registry);
    assert!(
        registry.has(&"read_skill".into()),
        "enabled native skills retain their execution tool"
    );
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    static ABSENT: JsValue = JsValue::Undefined;
    value.get(name).unwrap_or(&ABSENT)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        &[]
    }
}
fn string(value: &JsValue) -> JsString {
    value.as_string().unwrap().clone()
}
fn skills(values: &JsValue) -> Vec<NativeSkill> {
    array(values)
        .iter()
        .map(|value| NativeSkill {
            name: string(field(value, "name")),
            path: string(field(value, "path")),
            description: field(value, "description")
                .as_string()
                .map(|s| s.to_utf8().unwrap()),
            content: "".into(),
        })
        .collect()
}
#[test]
fn actual_ts_read_render_and_available_skills_golden() {
    let golden = pilotdeck_protocol::parse_json(include_str!("skills_oracle.json")).unwrap();
    let mut count = 0;
    for case in array(field(&golden, "cases")) {
        let id = string(field(case, "id")).to_utf8().unwrap();
        if id.starts_with("read-") {
            let output = read_response(
                &string(field(field(case, "input"), "skillName")),
                field(case, "content").as_string(),
                &skills(field(case, "available")),
            );
            assert_eq!(
                &JsValue::Array(output.content),
                field(field(case, "result"), "content"),
                "{id}"
            );
        } else if id.starts_with("render-") {
            assert_eq!(
                &JsValue::String(render_skill_content(
                    &string(field(case, "content")),
                    &string(field(case, "path"))
                )),
                field(case, "result"),
                "{id}"
            );
        } else {
            let catalog = NativeSkills(skills(field(case, "skills")).into(), true);
            let assembler = PromptAssembler::with_environment(
                Arc::new(catalog),
                "en-US".parse().unwrap(),
                "darwin",
                "v22.22.0",
            );
            let extra = field(case, "extra");
            let custom = field(extra, "customSystemPrompt").as_string();
            let result = assembler
                .assemble(
                    "/fixture/project",
                    "fixture",
                    "none",
                    "default",
                    None,
                    &[],
                    &[],
                    custom,
                    custom.is_some(),
                    None,
                    field(extra, "includeExtensionsWithCustomSystemPrompt") == &JsValue::Bool(true),
                    true,
                    1_767_323_045_678.0,
                )
                .unwrap();
            let blocks: Vec<JsValue> = result
                .parts
                .iter()
                .filter(|part| {
                    part.units()
                        .starts_with(&"<available-skills>".encode_utf16().collect::<Vec<_>>())
                })
                .cloned()
                .map(JsValue::String)
                .collect();
            assert_eq!(
                &JsValue::Array(blocks),
                field(case, "expectedBlocks"),
                "{id}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 15);
}
#[test]
fn exact_skill_name_wins_before_suffix_and_both_follow_one_catalog() {
    let catalog = NativeSkills(
        vec![
            NativeSkill {
                name: "plugin:demo".into(),
                description: None,
                path: "/p/SKILL.md".into(),
                content: "qualified".into(),
            },
            NativeSkill {
                name: "demo".into(),
                description: None,
                path: "/d/SKILL.md".into(),
                content: "exact".into(),
            },
        ]
        .into(),
        true,
    );
    let result = catalog.read(&"demo".into());
    assert_eq!(
        result.content[0].get("text"),
        Some(&"<skill>\n<name>demo</name>\n<path>/d/SKILL.md</path>\nexact\n</skill>".into())
    );
    let filtered = ToolRegistry::new(
        None,
        Some(Arc::new(|tool| {
            tool.fields().get("name") != Some(&"read_skill".into())
        })),
    );
    catalog.register(&filtered);
    assert!(!filtered.has(&"read_skill".into()));
    assert!(!filtered.has(&"ReadSkill".into()));
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn skill_file(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: self-written fixture\n---\n{body}\n"),
    )
    .unwrap();
    path
}
#[tokio::test]
async fn standalone_global_project_builtin_priority_and_lease_survive_reload() {
    use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-native-skills-{}", uuid::Uuid::new_v4())));
    let home = scratch.0.join("home");
    let project = scratch.0.join("project");
    let builtin = scratch.0.join("builtin");
    skill_file(&builtin, "same", "BUILTIN");
    skill_file(&home.join("skills"), "same", "GLOBAL");
    let selected = skill_file(
        &project.join(".pilotdeck/skills"),
        "same",
        "PROJECT {{SKILL_ROOT_SHELL}}",
    );
    skill_file(&home.join("skills"), "global-only", "GLOBAL ONLY");
    skill_file(&builtin, "builtin-only", "BUILTIN ONLY");
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: Some(builtin),
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    });
    runtime.refresh().await.unwrap();
    let mut old_lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    let old = NativeSkills::from_contributions(&old_lease.contributions.skills);
    assert_eq!(old.list_skills().len(), 3);
    let skill = old
        .0
        .iter()
        .find(|skill| skill.name.equals_str("same"))
        .unwrap();
    assert_eq!(skill.path, selected.to_string_lossy().into_owned().into());
    assert_eq!(skill.content, "PROJECT {{SKILL_ROOT_SHELL}}\n".into());
    let before = old.read(&"same".into());
    std::fs::write(&selected, "RELOADED BODY\n").unwrap();
    runtime.refresh().await.unwrap();
    let new_lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    let new = NativeSkills::from_contributions(&new_lease.contributions.skills);
    assert_ne!(old_lease.generation, new_lease.generation);
    assert_eq!(old.read(&"same".into()).content, before.content);
    assert_eq!(
        new.0
            .iter()
            .find(|skill| skill.name.equals_str("same"))
            .unwrap()
            .content,
        "RELOADED BODY\n".into()
    );
    // The first pending dispose cannot retire leased selected files. Dropping
    // one lease uses the existing owned release path, as cancelled prepare does.
    let mut dispose = Box::pin(runtime.dispose());
    let pending =
        std::future::poll_fn(|cx| std::task::Poll::Ready(dispose.as_mut().poll(cx).is_pending()))
            .await;
    assert!(pending);
    old_lease.release().await.unwrap();
    drop(new_lease);
    tokio::time::timeout(std::time::Duration::from_secs(2), dispose)
        .await
        .unwrap()
        .unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn native_skills_refuse_escaped_leaf_and_special_file_without_loading_sibling_assets() {
    use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
    use std::os::unix::fs::symlink;
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "pdrs-native-skill-boundary-{}",
        uuid::Uuid::new_v4()
    )));
    let home = scratch.0.join("home");
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&home).unwrap();
    let root = project.join(".pilotdeck/skills");
    let good = skill_file(&root, "good", "SAFE");
    let escaped = skill_file(&root, "escaped", "discard");
    let secret = scratch.0.join("outside.md");
    std::fs::write(&secret, "MUST NOT LOAD").unwrap();
    std::fs::remove_file(&escaped).unwrap();
    symlink(&secret, &escaped).unwrap();
    let fifo = root.join("fifo");
    std::fs::create_dir_all(&fifo).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(fifo.join("SKILL.md"))
            .status()
            .unwrap()
            .success()
    );
    // An unread script outside discovery remains inert even if executable.
    std::fs::write(good.parent().unwrap().join("script.sh"), "exit 99").unwrap();
    // A skill-local sibling manifest must never contribute arbitrary MCP.
    std::fs::write(
        good.parent().unwrap().join(".mcp.json"),
        r#"{"mcpServers":{"untrusted-sibling":{"command":"must-not-execute"}}}"#,
    )
    .unwrap();
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    });
    let report = runtime.refresh_with_report().await.unwrap();
    assert_eq!(report.staged_failures.len(), 2);
    let mut lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    assert_eq!(lease.contributions.skills.len(), 1);
    assert_eq!(lease.contributions.skills[0].name, "good");
    assert!(lease.contributions.commands.is_empty());
    let servers = &lease.contributions.mcp_servers;
    assert_eq!(
        servers.own_entries().len(),
        2,
        "only controlled default builtin MCP"
    );
    for expected in [
        crate::runtime::mcp::native_builtins::browser_servers(),
        crate::runtime::mcp::native_builtins::funasr_servers(&std::env::current_exe().unwrap())
            .unwrap(),
    ] {
        for (name, spec) in expected.as_object().unwrap().own_entries() {
            assert_eq!(
                servers.get_property(&name.to_utf8().unwrap()),
                Some(spec),
                "builtin spec must stay exact"
            );
        }
    }
    assert!(servers.get_property("untrusted-sibling").is_none());
    lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
}

#[tokio::test]
async fn native_skill_source_budget_refuses_growth_before_catalog_publication() {
    use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
    let scratch = Scratch(
        std::env::temp_dir().join(format!("pdrs-native-skill-budget-{}", uuid::Uuid::new_v4())),
    );
    let home = scratch.0.join("home");
    let project = scratch.0.join("project");
    std::fs::create_dir_all(&home).unwrap();
    let selected = skill_file(&project.join(".pilotdeck/skills"), "bounded", "SAFE");
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    });
    runtime.refresh().await.unwrap();
    let generation = runtime.generation().await;
    std::fs::write(&selected, vec![b'x'; 8 * 1024 * 1024 + 1]).unwrap();
    let error = runtime.refresh().await.unwrap_err();
    assert_eq!(error.code, Some("NATIVE_SKILL_LIMIT"));
    assert_eq!(runtime.generation().await, generation);
    let mut lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    assert_eq!(lease.contributions.skills[0].content, "SAFE\n");
    lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
}

#[tokio::test]
async fn native_builtin_root_discovery_populates_real_skill_catalog() {
    use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-builtin-root-{}", uuid::Uuid::new_v4())));
    let project = scratch.0.join("project");
    let home = scratch.0.join("home");
    let bin = scratch.0.join("install/bin");
    let bundled = scratch.0.join("install/skills");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    skill_file(&bundled, "packaged-skill", "BUILTIN FIXTURE");
    let empty = crate::config::Env::new();
    let resolve = |env: &crate::config::Env, exe: Option<&std::path::Path>| {
        crate::native_process::assembly::builtin_skills_root_at(&project, env, exe)
    };
    assert_eq!(resolve(&empty, Some(&bin)), Some(bundled.clone()));
    let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
        project_root: project.clone(),
        pilot_home: home,
        builtin_skills_root: resolve(&empty, Some(&bin)),
        builtin_plugins: vec![],
        builtin_plugins_enabled: Default::default(),
    });
    runtime.refresh().await.unwrap();
    let lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    assert!(
        lease
            .contributions
            .skills
            .iter()
            .any(|skill| skill.name == "packaged-skill")
    );
    let mut override_env = crate::config::Env::new();
    override_env.insert("PILOTDECK_BUNDLED_SKILLS_DIR".into(), "chosen".into());
    assert_eq!(
        resolve(&override_env, Some(&bin)),
        Some(project.join("chosen"))
    );
    override_env.insert("PILOTDECK_BUNDLED_SKILLS_DIR".into(), "".into());
    assert_eq!(resolve(&override_env, Some(&bin)), Some(bundled.clone()));
    std::fs::remove_dir_all(&bundled).unwrap();
    let second = scratch.0.join("skills");
    std::fs::create_dir(&second).unwrap();
    assert_eq!(resolve(&empty, Some(&bin)), Some(second));
    std::fs::remove_dir_all(scratch.0.join("skills")).unwrap();
    assert_eq!(resolve(&empty, Some(&bin)), Some(project.join("skills")));
    assert_eq!(resolve(&empty, None), Some(project.join("skills")));
}
