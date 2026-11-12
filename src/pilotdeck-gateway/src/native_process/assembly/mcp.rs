use super::{
    GatewaySessionPermissionRuleSetRegistry, JsString, JsValue, McpGateway, McpGatewayOptions,
    PluginProjectRuntime, PluginResources, ProjectResources, ProjectRuntimeRegistry, Resource,
    SdkMcpCloseSession, SdkMcpConfig, SdkMcpConfigOptions, SdkMcpError, SdkMcpFuture,
    SdkMcpProject, SdkMcpProjectResolver, SdkMcpSessionPlugins, SdkSessionProjects, SessionOwner,
    SystemEnvironment,
};
use crate::runtime::plugins::LoadedPlugin;
use crate::runtime::project::RuntimeLease;
use std::sync::Arc;

// ── U6 MCP resolver / session-plugins for the native process (#1319) ────────

/// The native process serves exactly one project. The SDK MCP resolver resolves
/// any `projectKey` (or no key) to that project's plugin runtime. This mirrors
/// `ProjectRuntimeRegistry.resolve` without exposing the full SDK session layer.
struct NativeMcpResolver {
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
}
impl SdkMcpProjectResolver for NativeMcpResolver {
    fn resolve<'a>(
        &'a self,
        _project_key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        let project = self.project.clone();
        Box::pin(async move {
            let lease = project
                .acquire(None)
                .map_err(|e| SdkMcpError::plain("Error", JsString::from(e.to_string())))?;
            let resources = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native project resources are missing.")
                })?;
            let plugin_res = resources
                .context()
                .resource(Resource::PluginRuntime)
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native plugin runtime resource is missing.")
                })?;
            let plugin_runtime = plugin_res
                .as_any()
                .downcast_ref::<PluginResources>()
                .ok_or_else(|| {
                    SdkMcpError::plain("Error", "Native plugin runtime has the wrong type.")
                })?
                .runtime
                .clone();
            let resolved: Arc<dyn SdkMcpProject> = Arc::new(LeasedMcpProject {
                project: PluginProjectRuntime::from_generation(lease.runtime(), plugin_runtime),
                // Struct-literal fields evaluate in written order: `project`
                // borrows `lease` first, this field moves it.
                _lease: if crate::mutants::on("mcp-resolver-lease") {
                    None
                } else {
                    Some(lease)
                },
            });
            Ok(resolved)
        })
    }
}

/// Keeps the resolved project generation leased for as long as the SDK MCP
/// state machine can call `refresh` / `snapshot`. `resolve` runs before
/// `set_mcp_servers` refreshes the plugin runtime, so releasing the lease when
/// `resolve` returns would let an `invalidate` / `reload` retire the
/// generation and `PluginResources::dispose` shut the runtime down under the
/// caller (#2157; the memory route documents and fixes the same invariant).
/// `RuntimeLease` releases exactly once on drop, so the generation stays
/// leased until the gateway drops this handle. The `mcp-resolver-lease`
/// fixture switch (`crate::mutants::on`, feature `pdrs-mutants`) selects the
/// registered mutation that resolves without holding the lease; with the
/// feature off the switch is a constant `false` and the lease is always held.
struct LeasedMcpProject {
    project: PluginProjectRuntime,
    _lease: Option<RuntimeLease>,
}

impl SdkMcpProject for LeasedMcpProject {
    fn project_root(&self) -> JsString {
        self.project.project_root()
    }
    fn refresh<'a>(&'a self) -> SdkMcpFuture<'a, Result<(), SdkMcpError>> {
        self.project.refresh()
    }
    fn snapshot<'a>(&'a self) -> SdkMcpFuture<'a, Vec<Arc<LoadedPlugin>>> {
        self.project.snapshot()
    }
}

/// The native process has no SDK session plugin sources (only project-level
/// plugins apply). This satisfies the `SdkMcpSessionPlugins` contract with an
/// empty list, matching the TS path where `sdkSessionPlugins` is never set.
struct NoSessionPlugins;
impl SdkMcpSessionPlugins for NoSessionPlugins {
    fn plugins(&self, _session_key: &JsValue) -> Vec<Arc<crate::runtime::plugins::LoadedPlugin>> {
        Vec::new()
    }
}

pub(super) fn build_mcp_gateway(
    owner: &SessionOwner,
    build_project: &Arc<ProjectRuntimeRegistry>,
    permission_rule_sets: &Arc<GatewaySessionPermissionRuleSetRegistry>,
    build_mcp_config: &Arc<std::sync::OnceLock<Arc<SdkMcpConfig>>>,
) -> Arc<McpGateway> {
    // U6 MCP four-control gateway (#1319): wire the #1215 component
    // into the native request router. The SdkMcpConfig owns the
    // registry state; the McpGateway owns validation, the busy gate,
    // and the post-success close. The close_session callback mirrors
    // the TS `closeSession` that the registry calls after its commit.
    let close_owner = owner.clone();
    let close_session: SdkMcpCloseSession = Arc::new(move |session_key, _reason| {
        let key = match &session_key {
            JsValue::String(key) => key.clone(),
            _ => JsString::from(""),
        };
        let owner = close_owner.clone();
        Box::pin(async move {
            owner
                .close(&key)
                .await
                .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))
        })
    });
    let mcp_config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(NativeMcpResolver {
            project: build_project.clone(),
        }),
        close_session,
        session_plugins: Arc::new(NoSessionPlugins),
        permission_rule_sets: permission_rule_sets.clone(),
        environment: Arc::new(SystemEnvironment),
        projects: SdkSessionProjects::shared(),
    }));
    // Publish the config so the prepare closure's sync_permission_rules
    // call (on each session lease acquire) can reach it.  The set
    // succeeds once; subsequent calls from a second start_with_cleanup
    // invocation are silently ignored because the reference is stable.
    let _ = build_mcp_config.set(mcp_config.clone());
    let mcp_gateway = Arc::new(McpGateway::new(McpGatewayOptions {
        owner: owner.clone(),
        config: mcp_config,
    }));
    let _ = &mcp_gateway;
    mcp_gateway
}

#[cfg(test)]
mod tests {
    //! #2157 device for the resolver lease: stage a real
    //! `ProjectRuntimeRegistry` through `ProjectResourcesFactory`, resolve the
    //! MCP project, retire the generation before the caller refreshes, and
    //! require the refresh to keep running. The old shape (lease dropped when
    //! `resolve` returns) lets cleanup dispose the plugin runtime and the
    //! refresh fails with "plugin runtime is draining".
    use super::*;
    use crate::config::Env;
    use crate::runtime::plugins::PluginRuntimeOptions;
    use crate::runtime::project::{
        Families, Family, ProjectResourcesFactory, ProjectResourcesOptions, ResourceFamilySource,
        RuntimeProfileOverrides, RuntimeResult, StageContext, StageSink,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};
    use tokio::time::timeout;

    const CONFIG: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\n";

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pdrs-2157-mcp-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("#2157 clock")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("#2157 scratch directory");
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The model, MCP, context and LSP families only have to be present so the
    /// plugin family stages on the production path
    /// (`ProjectResourcesFactory::stage`); they contribute nothing here.
    struct NoopFamily(Family);
    impl ResourceFamilySource for NoopFamily {
        fn family(&self) -> Family {
            self.0
        }
        fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
            Ok(())
        }
    }

    fn config(home: &Path) {
        std::fs::create_dir_all(home.join(".pilotdeck")).expect("#2157 config directory");
        std::fs::write(home.join("pilotdeck.yaml"), CONFIG).expect("#2157 config");
    }

    /// One real registry over a scratch project with the plugin family wired,
    /// so `Resource::PluginRuntime` holds a live `PluginRuntime`.
    fn registry(scratch: &Scratch) -> Arc<ProjectRuntimeRegistry> {
        let root = scratch.0.join("project");
        let home = scratch.0.join("home");
        std::fs::create_dir(&root).expect("#2157 project directory");
        std::fs::create_dir(&home).expect("#2157 home directory");
        config(&home);
        let mut env = Env::new();
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let factory = ProjectResourcesFactory::new(ProjectResourcesOptions {
            pilot_home: home.clone(),
            cwd: root.clone(),
            env,
            homedir: Some(home.clone()),
            builtin_skills_root: None,
            now: Arc::new(|| SystemTime::UNIX_EPOCH),
            telemetry: None,
            config_version: None,
            runtime_profile_overrides: RuntimeProfileOverrides::default(),
            families: Families {
                lsp: Some(Arc::new(NoopFamily(Family::LspService))),
                session_title: Some(Arc::new(NoopFamily(Family::SessionTitleProvider))),
                ..Families::unwired()
            }
            .with_native_models()
            .with_context_memory()
            .with_tools_mcp()
            .with_plugins(PluginRuntimeOptions {
                project_root: root.clone(),
                pilot_home: home.clone(),
                builtin_skills_root: None,
                builtin_plugins: Vec::new(),
                builtin_plugins_enabled: std::collections::HashMap::new(),
            })
            .with_interaction(),
        });
        Arc::new(ProjectRuntimeRegistry::new(root, Arc::new(factory)))
    }

    /// #2157. The caller of `resolve` only refreshes *after* it returns
    /// (`set_mcp_servers`), so the lease must travel with the resolved
    /// project: an invalidate between the two must not dispose the plugin
    /// runtime. The deterministic claim runs first: right after the
    /// invalidate, `lease_count` still reports the resolver's lease under
    /// both cleanup timings, before the 500 ms window can make the outcome
    /// depend on how far the disposal task got. The behavioural assertions
    /// follow: the refresh either still works or names the disposed runtime,
    /// and retirement must still be blocked. The claim that the resolver
    /// must hold the lease is registered as the `mcp-resolver-lease`
    /// executable mutation
    /// (`tests/fixtures/mcp-resolver-lease/negative_controls.py`), which must
    /// stay red at the `lease_count` assertion; this prose no longer carries
    /// the proof on its own.
    #[tokio::test]
    async fn resolved_project_holds_the_generation_lease_through_refresh() {
        let scratch = Scratch::new();
        let project = registry(&scratch);
        let resolver = NativeMcpResolver {
            project: Arc::clone(&project),
        };
        let resolved = resolver.resolve(None).await.expect("#2157 resolve");
        let runtime = project.resolve(None).expect("#2157 current generation");
        // Retire the generation in the window between `resolve` and the
        // caller's `refresh` (`set_mcp_servers`).
        project.invalidate(None);
        // Deterministic under both cleanup timings: the count only tracks the
        // lease the resolver returned with, and neither "the disposal task
        // has not been scheduled yet" nor "it already finished" changes it.
        crate::negative_assert_eq!(
            runtime.lease_count(),
            1,
            "#2157 the resolved project must own the generation lease"
        );
        // 500 ms only bounds "cleanup had its chance": with the lease held the
        // disposal cannot finish at any duration, and without it the
        // deterministic `lease_count` assertion above has already caught the
        // early drop, whatever a slow machine does to this window.
        let retired = timeout(Duration::from_millis(500), runtime.wait_disposed()).await;
        crate::test_negative_receipt::expect_ok(
            resolved.refresh().await,
            "#2157 refresh must run on the generation the resolver leased",
        );
        // The lease must also survive the refresh itself: `refresh` runs on
        // the leased generation and returns with the handle still held, so
        // the count stays deterministic here too (r4 DS concurrency P2:
        // the early claim above proves the window entry, this one re-proves
        // the exit the original #2157 assertion carried).
        crate::negative_assert_eq!(
            runtime.lease_count(),
            1,
            "#2157 the resolved project must own the generation lease"
        );
        crate::negative_assert!(
            retired.is_err(),
            "#2157 retirement must wait for the resolver's lease; the returned project dropped it"
        );
        drop(resolved);
        runtime
            .wait_disposed()
            .await
            .expect("#2157 cleanup after the last lease");
    }
}
