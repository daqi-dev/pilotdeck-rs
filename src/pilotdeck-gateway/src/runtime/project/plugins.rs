//! A6.9-pr3e: PluginRuntime family for ProjectRuntimeFactory.
//! ProjectRuntimeResourcesBundle.ts:218-225.
use super::*;
use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
use pilotdeck_protocol::JsValue;
use std::{any::Any, sync::Arc};

pub struct PluginFamilySource {
    pub options: PluginRuntimeOptions,
}

pub struct PluginResources {
    pub runtime: Arc<PluginRuntime>,
}

impl FamilyResource for PluginResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.runtime
                .dispose()
                .await
                .map_err(|error| RuntimeError::new(error.to_string()))
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ResourceFamilySource for PluginFamilySource {
    fn family(&self) -> Family {
        Family::PluginRuntime
    }

    fn stage(&self, _context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: self.options.project_root.clone(),
            pilot_home: self.options.pilot_home.clone(),
            builtin_skills_root: self.options.builtin_skills_root.clone(),
            builtin_plugins: self.options.builtin_plugins.clone(),
            builtin_plugins_enabled: self.options.builtin_plugins_enabled.clone(),
        }));
        let resource = Arc::new(PluginResources { runtime });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::PluginRuntime, resource);
        Ok(())
    }
}

/// Native CLI bounded disk generation retains Markdown and server declarations.
/// Programmatic builtin plugins are handled by a separate, unsupported profile.
pub struct NativeStylePluginFamilySource {
    pub options: PluginRuntimeOptions,
}
impl ResourceFamilySource for NativeStylePluginFamilySource {
    fn family(&self) -> Family {
        Family::PluginRuntime
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let mut enabled = self.options.builtin_plugins_enabled.clone();
        let snapshot = context.snapshot();
        if let Some(configured) = snapshot
            .get("config")
            .and_then(|config| config.get("extension"))
            .and_then(|extension| extension.get("builtinPluginsEnabled"))
            .and_then(JsValue::as_object)
        {
            for (name, value) in configured.entries() {
                if let Some(value) = value.as_bool()
                    && let Ok(name) = name.to_utf8()
                {
                    enabled.insert(name, value);
                }
            }
        }
        let runtime = Arc::new(PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: self.options.project_root.clone(),
            pilot_home: self.options.pilot_home.clone(),
            builtin_skills_root: self.options.builtin_skills_root.clone(),
            builtin_plugins: self.options.builtin_plugins.clone(),
            builtin_plugins_enabled: enabled,
        }));
        let resource = Arc::new(PluginResources { runtime });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::PluginRuntime, resource);
        Ok(())
    }
}

#[cfg(test)]
mod native_style_tests {
    use super::*;
    use std::{collections::HashMap, fs, path::PathBuf};
    // Keep disk-plugin generation assertions isolated from real first-party
    // builtins; builtin priority and default loading have dedicated tests.
    fn isolated_builtin_switches() -> HashMap<String, bool> {
        [
            ("browser-use".into(), false),
            ("funasr".into(), false),
            ("windows-skills".into(), false),
        ]
        .into_iter()
        .collect()
    }
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn native_generation_loads_declared_styles_commands_hooks_and_mcp() {
        let root =
            Scratch(std::env::temp_dir().join(format!("pdrs-style-gate-{}", uuid::Uuid::new_v4())));
        let project = root.0.join("project");
        let home = root.0.join("home");
        let plugin = project.join(".pilotdeck/plugins/p");
        fs::create_dir_all(plugin.join("output-styles")).unwrap();
        fs::create_dir_all(&home).unwrap();
        let manifest = plugin.join("plugin.json");
        fs::write(&manifest, r#"{"name":"p","version":"1.0.0"}"#).unwrap();
        fs::write(plugin.join("output-styles/a.md"), "first").unwrap();
        let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: project,
            pilot_home: home,
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: isolated_builtin_switches(),
        });
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 1);
        assert_eq!(
            runtime.create_view(vec![]).await.list_output_styles().len(),
            1
        );
        fs::create_dir(plugin.join("commands")).unwrap();
        fs::write(plugin.join("commands/do.md"), "danger").unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 2);
        let commands = runtime.all_commands().await;
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "p:do");
        assert_eq!(commands[0].content, "danger");
        fs::remove_dir_all(plugin.join("commands")).unwrap();
        fs::write(
            &manifest,
            r#"{"name":"p","hooks":{"PreToolUse":[{"matcher":"write_file","hooks":[{"type":"command","command":"echo fixture-hook"}]}]}}"#,
        )
        .unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 3);
        let snapshot = runtime.create_view(vec![]).await.snapshot_contributions();
        let (event, matchers) = snapshot
            .hooks
            .iter()
            .find(|(event, _)| event.equals_str("PreToolUse"))
            .expect("PreToolUse hook is loaded");
        assert!(event.equals_str("PreToolUse"));
        assert_eq!(matchers.len(), 1);
        assert!(
            matchers[0]
                .matcher
                .as_ref()
                .is_some_and(|matcher| matcher.equals_str("write_file"))
        );
        assert!(
            matches!(matchers[0].hooks.as_slice(), [crate::hooks::protocol::HookCommand::Command { command, .. }] if command.equals_str("echo fixture-hook"))
        );
        fs::write(&manifest, r#"{"name":"p","mcpServers":{}}"#).unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 4);
        assert!(runtime.snapshot().await[0].mcp_servers.is_some());
        fs::write(&manifest, r#"{"name":"p","outputStyles":"custom"}"#).unwrap();
        fs::rename(plugin.join("output-styles"), plugin.join("custom")).unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 5);
        assert_eq!(
            runtime.create_view(vec![]).await.list_output_styles().len(),
            1
        );
        runtime.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn native_generation_accepts_empty_hook_objects_and_matchers() {
        let root = Scratch(
            std::env::temp_dir().join(format!("pdrs-empty-hooks-{}", uuid::Uuid::new_v4())),
        );
        let project = root.0.join("project");
        let home = root.0.join("home");
        let plugin = project.join(".pilotdeck/plugins/p");
        fs::create_dir_all(&plugin).unwrap();
        fs::create_dir_all(&home).unwrap();
        let manifest = plugin.join("plugin.json");
        let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: project,
            pilot_home: home,
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: isolated_builtin_switches(),
        });
        for (generation, raw) in [
            (1_u64, r#"{"name":"p","hooks":{}}"#),
            (2_u64, r#"{"name":"p","hooks":{"PreToolUse":[]}}"#),
        ] {
            fs::write(&manifest, raw).unwrap();
            assert!(!plugin.join("output-styles").exists());
            runtime.refresh().await.unwrap();
            assert_eq!(runtime.generation().await, generation);
            let loaded = runtime.snapshot().await;
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].name, "p");
            assert!(
                loaded[0]
                    .hooks_config
                    .as_ref()
                    .is_some_and(|hooks| hooks.is_empty())
            );
            let view = runtime.create_view(vec![]).await;
            assert!(view.list_output_styles().is_empty());
            assert!(view.snapshot_contributions().hooks.is_empty());

            fs::write(&manifest, r#"{"name":false}"#).unwrap();
            let report = runtime.refresh_with_report().await.unwrap();
            assert_eq!(report.staged_failures.len(), 1);
            assert_eq!(report.generation, generation);
            assert_eq!(runtime.generation().await, generation);
            assert_eq!(runtime.snapshot().await.len(), 1);
        }
        runtime.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn native_declarations_share_generation_and_keep_the_retained_catalog() {
        let root = Scratch(
            std::env::temp_dir().join(format!("pdrs-plugin-data-{}", uuid::Uuid::new_v4())),
        );
        let project = root.0.join("project");
        let home = root.0.join("home");
        let plugin = project.join(".pilotdeck/plugins/directory-name");
        fs::create_dir_all(plugin.join("commands")).unwrap();
        fs::create_dir_all(plugin.join("skills/research")).unwrap();
        fs::create_dir_all(plugin.join("agents")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(plugin.join("commands/inspect.md"), "first command").unwrap();
        fs::write(plugin.join("skills/research/SKILL.md"), "research skill").unwrap();
        fs::write(plugin.join("skills/notes.md"), "not an additional skill").unwrap();
        fs::write(
            plugin.join("agents/unused.md"),
            "ignored by the TS disk loader",
        )
        .unwrap();
        fs::write(
            plugin.join("unknown.js"),
            "throw new Error('must not execute');",
        )
        .unwrap();
        let manifest = plugin.join("plugin.json");
        fs::write(
            &manifest,
            r#"{
            "name":"Visible name",
            "outputStyles":[],
            "hooks":{},
            "agents":"agents",
            "settings":{"keptAsManifestMetadata":true},
            "unknown":{"module":"unknown.js"},
            "mcpServers":{"bridge":{"command":"fixture-mcp","args":[]}},
            "lspServers":{"rust":{"command":"fixture-lsp","args":[]}}
        }"#,
        )
        .unwrap();
        let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: project,
            pilot_home: home,
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: isolated_builtin_switches(),
        });
        runtime.refresh().await.unwrap();
        let mut old = runtime.acquire_contribution_snapshot().await.unwrap();
        assert_eq!(old.generation, 1);
        assert_eq!(old.contributions.commands.len(), 1);
        assert_eq!(old.contributions.commands[0].name, "directory-name:inspect");
        assert_eq!(old.contributions.commands[0].content, "first command");
        assert_eq!(old.contributions.skills.len(), 1);
        assert_eq!(old.contributions.skills[0].name, "directory-name:research");
        assert!(
            old.contributions
                .mcp_servers
                .get("bridge")
                .and_then(|value| value.get("command"))
                .and_then(|value| value.as_string())
                .is_some_and(|value| value.equals_str("fixture-mcp"))
        );
        assert!(
            old.contributions
                .lsp_servers
                .get("rust")
                .and_then(|value| value.get("command"))
                .and_then(|value| value.as_string())
                .is_some_and(|value| value.equals_str("fixture-lsp"))
        );

        fs::write(plugin.join("commands/inspect.md"), "second command").unwrap();
        runtime.refresh().await.unwrap();
        let mut next = runtime.acquire_contribution_snapshot().await.unwrap();
        assert_eq!(next.generation, 2);
        assert_eq!(next.contributions.commands[0].content, "second command");
        assert_eq!(old.contributions.commands[0].content, "first command");
        fs::write(&manifest, r#"{"name":false}"#).unwrap();
        let report = runtime.refresh_with_report().await.unwrap();
        assert_eq!(report.staged_failures.len(), 1);
        assert_eq!(report.generation, 2);
        assert_eq!(runtime.generation().await, 2);
        assert_eq!(runtime.snapshot().await[0].name, "Visible name");
        old.release().await.unwrap();
        next.release().await.unwrap();
        runtime.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn native_declarations_keep_programmatic_builtins_explicitly_unavailable() {
        let builtin = Arc::new(crate::runtime::plugins::loader::LoadedPlugin {
            name: "programmatic".into(),
            path: PathBuf::new(),
            source: crate::runtime::plugins::loader::PluginSource::Builtin,
            commands: vec![],
            skills: vec![],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
        });
        let runtime = PluginRuntime::new_native_style_only(PluginRuntimeOptions {
            project_root: PathBuf::new(),
            pilot_home: PathBuf::new(),
            builtin_skills_root: None,
            builtin_plugins: vec![builtin],
            builtin_plugins_enabled: HashMap::new(),
        });
        assert_eq!(
            runtime.refresh().await.unwrap_err().message,
            "Native restricted plugin family cannot load programmatic builtins."
        );
        assert_eq!(runtime.generation().await, 0);
        assert!(runtime.snapshot().await.is_empty());
        runtime.dispose().await.unwrap();
    }
}
