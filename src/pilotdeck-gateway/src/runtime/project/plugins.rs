//! A6.9-pr3e: PluginRuntime family for ProjectRuntimeFactory.
//! ProjectRuntimeResourcesBundle.ts:218-225.
use super::*;
use crate::runtime::plugins::{PluginRuntime, PluginRuntimeOptions};
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

/// Native CLI restricted generation. Full plugin hooks/MCP/tools are never loaded.
pub struct NativeStylePluginFamilySource {
    pub options: PluginRuntimeOptions,
}
impl ResourceFamilySource for NativeStylePluginFamilySource {
    fn family(&self) -> Family {
        Family::PluginRuntime
    }
    fn stage(&self, _context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let runtime = Arc::new(PluginRuntime::new_native_style_only(PluginRuntimeOptions {
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

#[cfg(test)]
mod native_style_tests {
    use super::*;
    use std::{collections::HashMap, fs, path::PathBuf};
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn native_generation_accepts_only_inert_style_plugins() {
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
            builtin_plugins_enabled: HashMap::new(),
        });
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 1);
        assert_eq!(
            runtime.create_view(vec![]).await.list_output_styles().len(),
            1
        );
        fs::create_dir(plugin.join("commands")).unwrap();
        fs::write(plugin.join("commands/do.md"), "danger").unwrap();
        assert!(runtime.refresh().await.is_err());
        assert_eq!(runtime.generation().await, 1);
        assert!(runtime.all_commands().await.is_empty());
        fs::remove_dir_all(plugin.join("commands")).unwrap();
        fs::write(&manifest, r#"{"name":"p","hooks":{"PreToolUse":[]}}"#).unwrap();
        assert!(runtime.refresh().await.is_err());
        assert_eq!(runtime.generation().await, 1);
        fs::write(&manifest, r#"{"name":"p","outputStyles":"custom"}"#).unwrap();
        fs::rename(plugin.join("output-styles"), plugin.join("custom")).unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.generation().await, 2);
        assert_eq!(
            runtime.create_view(vec![]).await.list_output_styles().len(),
            1
        );
        runtime.dispose().await.unwrap();
    }
}
