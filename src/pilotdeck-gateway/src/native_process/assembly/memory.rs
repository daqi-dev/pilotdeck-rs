use super::{
    JsString, MemoryGateway, MemoryManagementResource, MemoryManagementService, MemoryProjection,
    MemoryProviderError, Options, ProjectMemoryRegistry, ProjectResources, ProjectRuntimeRegistry,
    ProxyError, Resource, RuntimeError,
};
use crate::metadata::{Metadata, NativeFiles};
use crate::runtime::project::RuntimeLease;
use pilotdeck_protocol::JsValue;
use std::sync::Arc;

/// `ProjectMemoryRegistry` adapter over the native `ProjectRuntimeRegistry`.
/// `resolveProjectKey` is the same port the snapshot route uses
/// (`metadata::Metadata::resolve_dialog_project`, `projectRegistry.ts:41-59`,
/// via `SnapshotProjects`): `path.resolve` folds a trailing slash, `//`, `.`
/// and `..` lexically and matches the registered project catalog, returning
/// the registered spelling. The earlier exact-code-unit comparison rejected
/// keys `snapshot_list` accepted for the same project (#1456 item 2).
/// `memory_management` acquires the current generation, reads
/// `Resource::MemoryManagement` from the staged context, and keeps that lease
/// alive for as long as the returned service can be called, returning `None`
/// when memory management was not staged (provider disabled or absent).
struct NativeMemoryRegistry {
    project: Arc<ProjectRuntimeRegistry>,
    project_root: std::path::PathBuf,
    metadata: Metadata,
}

impl NativeMemoryRegistry {
    fn new(project: Arc<ProjectRuntimeRegistry>, options: &Options) -> Self {
        Self {
            project,
            project_root: options.project.clone(),
            // Same base the management snapshot route builds
            // (`PersistenceResources::snapshot_gateway(PathBuf::new())`), so
            // both management families resolve a project key identically.
            metadata: Metadata {
                home: options.home.clone(),
                cwd: std::path::PathBuf::new(),
                files: NativeFiles,
            },
        }
    }
}

impl ProjectMemoryRegistry for NativeMemoryRegistry {
    fn resolve_project_key(&self, project_key: &JsString) -> Result<JsString, MemoryProviderError> {
        // A non-UTF-8 served root cannot be named by any JS string; keep the
        // R19 host boundary instead of lossily folding it onto another
        // project (unchanged from before #1456).
        self.project_root
            .to_str()
            .ok_or_else(MemoryProviderError::host_required)?;
        self.metadata
            .resolve_dialog_project(project_key)
            .map(JsString::from)
            .ok_or_else(|| MemoryProviderError::unknown_project_key(project_key.units()))
    }

    fn memory_management(
        &self,
        _project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        let lease = self
            .project
            .acquire(None)
            .map_err(MemoryProviderError::from)?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| MemoryProviderError::new("Native project resources are missing."))?;
        let context = resources.context();
        let Some(handle) = context.resource(Resource::MemoryManagement) else {
            return Ok(None);
        };
        let resource = handle
            .as_any()
            .downcast_ref::<MemoryManagementResource>()
            .ok_or_else(|| {
                MemoryProviderError::new("Native memory management resource has the wrong type.")
            })?;
        Ok(Some(Arc::new(LeasedMemoryManagement {
            management: resource.management().clone(),
            _lease: lease,
        })))
    }
}

/// Keeps the generation lease for the whole call, so `retire()` during a
/// reload/invalidate cannot dispose the provider under an in-flight
/// `memory_list` / `memory_wipe`. `management_request` already documents the
/// invariant for the snapshot and command paths ("Keep the generation leased
/// through all awaits …; invalidation must not dispose an in-flight request");
/// the memory route dropped the lease as soon as it cloned the service (#1456
/// item 3). `RuntimeLease` releases exactly once on drop, so the generation
/// stays leased until the projection drops this handle after the call.
struct LeasedMemoryManagement {
    management: Arc<dyn MemoryManagementService>,
    _lease: RuntimeLease,
}

impl MemoryManagementService for LeasedMemoryManagement {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        self.management.list(limit)
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        self.management.clear(scope)
    }
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        self.management.clear_session(session_key)
    }
}

/// Build the `MemoryGateway` by probing the current project generation. If
/// `Resource::MemoryManagement` is staged the gateway is enabled; otherwise
/// (`MemoryGateway::disabled()`) it advertises `CAPABILITY_UNAVAILABLE` after
/// parameter validation, matching the TS wrapper seam.
fn build_memory_gateway(
    options: &Options,
    project: &Arc<ProjectRuntimeRegistry>,
) -> Result<MemoryGateway, RuntimeError> {
    let registry = Arc::new(NativeMemoryRegistry::new(project.clone(), options));
    // Check if the current generation staged a management resource. If the
    // project has no memory provider the gateway is disabled but not an error.
    let lease = project.acquire(None)?;
    let resources = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
    let context = resources.context();
    if context.resource(Resource::MemoryManagement).is_some() {
        let projection = MemoryProjection::new(registry, options.project.clone());
        Ok(MemoryGateway::from_projection(projection))
    } else {
        Ok(MemoryGateway::disabled())
    }
}

pub(super) async fn build_memory(
    options: Options,
    project: Arc<ProjectRuntimeRegistry>,
) -> Result<Arc<MemoryGateway>, ProxyError> {
    let memory = match build_memory_gateway(&options, &project) {
        Ok(gateway) => Arc::new(gateway),
        Err(error) => {
            let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
            return Err(error.into());
        }
    };
    Ok(memory)
}

#[cfg(test)]
mod tests {
    //! #1456 devices for the two registry seams: the project-key port and the
    //! generation lease. Both stage a real `ProjectRuntimeRegistry` through
    //! `ProjectResourcesFactory` and inject one fake memory provider, so the
    //! assertions run on the same code path production uses; the negative
    //! controls in `tests/fixtures/native-management/negative_controls.py`
    //! prove each case goes red when the judged behavior is removed.
    use super::*;
    use crate::config::Env;
    use crate::runtime::context::memory::{
        MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture, MemoryResolver,
        MemoryRetrieveInput, MemoryRetrieveResult,
    };
    use crate::runtime::memory::gateway::MemoryDispatch;
    use crate::runtime::memory::{MemoryDisposeFn, MemoryProviderFactoryFn, ProjectMemoryProvider};
    use crate::runtime::plugins::PluginRuntimeOptions;
    use crate::runtime::project::memory_bundle::MemoryBundleFamilySource;
    use crate::runtime::project::{
        Families, Family, ProjectResourcesFactory, ProjectResourcesOptions, ResourceFamilySource,
        RuntimeProfileOverrides, RuntimeResult, StageContext, StageSink,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    /// Global and project config for the staged registry: one mock model
    /// provider and an enabled EdgeClaw memory section (the injected factory
    /// takes precedence over the native one).
    const CONFIG: &str = "schemaVersion: 1\nagent: {model: test/mock}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock\n      models:\n        mock: {}\nmemory:\n  enabled: true\n  provider: edgeclaw\n";

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pdrs-1456-memory-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .expect("#1456 clock")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("#1456 scratch directory");
            Self(path)
        }
        fn home(&self) -> std::path::PathBuf {
            self.0.join("home")
        }
        fn root(&self) -> std::path::PathBuf {
            self.0.join("project")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The staged provider: `list` returns one item and `dispose` counts, so
    /// the tests can tell "still serving" from "disposed under the call".
    #[derive(Default)]
    struct FakeMemory {
        list_calls: AtomicUsize,
        dispose_calls: AtomicUsize,
    }
    impl MemoryResolver for FakeMemory {
        fn capture_turn(&self, _: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
            Box::pin(async { Ok(()) })
        }
        fn retrieve(&self, _: MemoryRetrieveInput) -> MemoryFuture {
            Box::pin(async {
                Ok(MemoryRetrieveResult {
                    system_context: None,
                    diagnostics: Vec::new(),
                })
            })
        }
    }
    impl MemoryManagementService for FakeMemory {
        fn list(&self, _limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![JsValue::from("remembered")])
        }
        fn clear(&self, _scope: &str) -> Result<(), MemoryProviderError> {
            Ok(())
        }
        fn clear_session(&self, _session_key: &JsString) -> Result<(), MemoryProviderError> {
            Ok(())
        }
    }

    /// A family that stages nothing: the model, plugin, MCP, context and LSP
    /// families only have to be present so the generation reaches the memory
    /// family last (`ProjectResourcesFactory::stage`).
    struct NoopFamily(Family);
    impl ResourceFamilySource for NoopFamily {
        fn family(&self) -> Family {
            self.0
        }
        fn stage(&self, _: &StageContext, _: &mut StageSink<'_>) -> RuntimeResult {
            Ok(())
        }
    }

    /// One real registry over the scratch project, with `FakeMemory` staged as
    /// `Resource::MemoryManagement` and registered in `home/projects/a8-main`
    /// so `resolve_dialog_project` has a catalog entry to match.
    fn registry(scratch: &Scratch, provider: &Arc<FakeMemory>) -> Arc<ProjectRuntimeRegistry> {
        let root = scratch.root();
        let home = scratch.home();
        std::fs::create_dir_all(&root).expect("#1456 project directory");
        std::fs::create_dir_all(home.join(".pilotdeck")).expect("#1456 config directory");
        std::fs::write(home.join("pilotdeck.yaml"), CONFIG).expect("#1456 config");
        let projects = home.join("projects/a8-main");
        std::fs::create_dir_all(&projects).expect("#1456 project catalog entry");
        std::fs::write(projects.join(".cwd"), root.to_string_lossy().as_bytes())
            .expect("#1456 project marker");
        let mut env = Env::new();
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        let provider_in_factory = Arc::clone(provider);
        let dispose_provider = Arc::clone(provider);
        let provider_factory: MemoryProviderFactoryFn = Arc::new(move |_| {
            let memory: Arc<dyn MemoryResolver> = provider_in_factory.clone();
            let management: Arc<dyn MemoryManagementService> = provider_in_factory.clone();
            let dispose_provider = Arc::clone(&dispose_provider);
            let dispose: MemoryDisposeFn = Arc::new(move || {
                let provider = Arc::clone(&dispose_provider);
                Box::pin(async move {
                    provider.dispose_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            });
            Ok(Some(ProjectMemoryProvider {
                memory,
                maintenance: None,
                management: Some(management),
                dispose: Some(dispose),
            }))
        });
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
            .with_memory(
                MemoryBundleFamilySource::default().with_provider_factory(provider_factory),
            )
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

    fn options(scratch: &Scratch) -> Options {
        Options {
            home: scratch.home(),
            project: scratch.root(),
            listen: "127.0.0.1:0".parse().expect("#1456 listen address"),
            token_file: scratch.0.join("token"),
            sidecar: scratch.0.join("sidecar"),
            sidecar_args: Vec::new(),
            drain_budget: Duration::from_secs(5),
            storage_root: None,
        }
    }

    fn key(path: &std::path::Path) -> JsString {
        JsString::from(path.to_string_lossy().into_owned())
    }

    fn list_params(project_key: &str) -> JsValue {
        let mut params = pilotdeck_protocol::JsObject::new();
        params.insert("projectKey".into(), JsValue::from(project_key));
        JsValue::Object(params)
    }

    /// #1456 item 2. The TS baseline (`projectRegistry.ts:41-59`, measured by
    /// `_tmp/20261107-issue-1456/ts-baseline/probe.log` rows
    /// `A-resolveProjectKey`) resolves a key lexically and answers with the
    /// registered spelling, so a trailing slash, `//`, `/.` and `/sub/..` all
    /// name the same project. The earlier exact-code-unit comparison rejected
    /// every one of them with PROJECT_NOT_FOUND, while `snapshot_list` accepted
    /// them for that same project.
    #[tokio::test]
    async fn resolve_project_key_accepts_the_keys_the_snapshot_route_accepts() {
        let scratch = Scratch::new();
        let provider = Arc::new(FakeMemory::default());
        let project = registry(&scratch, &provider);
        let root = scratch.root().to_string_lossy().into_owned();
        let gateway = build_memory_gateway(&options(&scratch), &project).expect("#1456 gateway");
        assert!(
            gateway.is_enabled(),
            "#1456 the injected provider must stage Resource::MemoryManagement"
        );
        let registry = NativeMemoryRegistry::new(Arc::clone(&project), &options(&scratch));
        for spelling in [
            root.clone(),
            format!("{root}/"),
            format!("{root}//"),
            format!("{root}/."),
            format!("{root}/sub/.."),
            format!("{root}/../{}", root.rsplit('/').next().unwrap_or_default()),
        ] {
            let resolved = registry
                .resolve_project_key(&JsString::from(spelling.clone()))
                .unwrap_or_else(|error| {
                    panic!(
                        "#1456 {spelling} resolves to the registered project in the TS baseline, got {error:?}"
                    )
                });
            assert_eq!(
                resolved.to_utf8().expect("#1456 utf-8 project key"),
                root,
                "#1456 {spelling} must answer with the registered spelling"
            );
        }
        // The same key reaches the wire: the projection resolves it, stages the
        // management service and lists through it.
        let outcome = gateway
            .dispatch("memory_list", &list_params(&format!("{root}/")))
            .expect("#1456 memory_list is a memory method");
        let MemoryDispatch::Response(JsValue::Object(body)) = &outcome else {
            panic!("#1456 a resolvable projectKey must list, got {outcome:?}");
        };
        let Some(JsValue::Array(items)) = body.get("items") else {
            panic!("#1456 memory_list response must carry an items array");
        };
        assert_eq!(items.len(), 1, "#1456 memory_list through the resolved key");
        // Foreign keys keep the TS rejection, message included.
        for foreign in ["/definitely/not/a/project", "", "."] {
            let error = registry
                .resolve_project_key(&JsString::from(foreign))
                .expect_err("#1456 foreign projectKey must stay rejected");
            assert_eq!(
                error.code.as_deref(),
                Some("PROJECT_NOT_FOUND"),
                "#1456 {foreign:?} rejection code"
            );
            assert_eq!(error.name, "DialogGatewayError", "#1456 rejection name");
            assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                format!("Unknown projectKey: {foreign}"),
                "#1456 {foreign:?} rejection message"
            );
        }
        project.dispose().await.expect("#1456 registry disposal");
    }

    /// #1456 item 3. The sibling management route documents the invariant
    /// (`management.rs`: "Keep the generation leased through all awaits …
    /// invalidation must not dispose an in-flight request"); the memory route
    /// dropped the lease as soon as it cloned the service, so an invalidate
    /// during `memory_list` / `memory_wipe` could dispose the provider under
    /// the call.
    #[tokio::test]
    async fn memory_management_holds_the_generation_lease_through_the_call() {
        let scratch = Scratch::new();
        let provider = Arc::new(FakeMemory::default());
        let project = registry(&scratch, &provider);
        let baseline = project.acquire(None).expect("#1456 baseline lease");
        let runtime = Arc::clone(baseline.runtime());
        let leases = runtime.lease_count();
        let registry = NativeMemoryRegistry::new(Arc::clone(&project), &options(&scratch));
        let management = registry
            .memory_management(&key(&scratch.root()))
            .expect("#1456 memory_management resolves")
            .expect("#1456 the fake provider stages a management service");
        assert_eq!(
            runtime.lease_count(),
            leases + 1,
            "#1456 the returned service must own a generation lease"
        );
        // Retiring the generation mid-call must not dispose the provider.
        project.invalidate(None);
        assert_eq!(
            provider.dispose_calls.load(Ordering::SeqCst),
            0,
            "#1456 invalidation disposed the provider under an in-flight memory call"
        );
        assert_eq!(
            management
                .list(100)
                .expect("#1456 the leased service still serves the call")
                .len(),
            1,
            "#1456 in-flight memory_list result"
        );
        drop(management);
        drop(baseline);
        runtime
            .wait_disposed()
            .await
            .expect("#1456 cleanup after the last lease");
        assert_eq!(
            provider.dispose_calls.load(Ordering::SeqCst),
            1,
            "#1456 the provider is disposed exactly once, after the lease is released"
        );
    }
}
