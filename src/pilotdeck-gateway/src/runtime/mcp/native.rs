//! MCP control shares the native turn owner and SDK project/plugin registry.
use super::{routes::McpRoutes, sdk::*};
use crate::{
    config::permission::GatewaySessionPermissionRuleSetRegistry,
    runtime::{
        interaction::GatewayInteractionCoordinator, sdk_session::SdkSessionRegistry,
        session::SessionOwner,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;
use tokio::sync::Mutex;

pub const METHODS: &[&str] = &[
    "mcp_server_status",
    "set_mcp_servers",
    "mcp_server_reconnect",
    "mcp_server_toggle",
    "set_mcp_permission_mode_override",
];

struct SdkProjects(Arc<Mutex<SdkSessionRegistry>>);
impl SdkMcpProjectResolver for SdkProjects {
    fn resolve<'a>(
        &'a self,
        key: Option<&'a JsString>,
    ) -> SdkMcpFuture<'a, Result<Arc<dyn SdkMcpProject>, SdkMcpError>> {
        Box::pin(async move {
            let resolver = self.0.lock().await.options().resolver.clone();
            let runtime = resolver
                .resolve(key)
                .map_err(|error| SdkMcpError::plain("Error", error.message))?;
            Ok(Arc::new(PluginProjectRuntime::new(
                JsString::from(runtime.project_root.to_string_lossy().into_owned()),
                runtime.plugin_runtime,
            )) as Arc<dyn SdkMcpProject>)
        })
    }
}
struct SessionPlugins(Arc<Mutex<SdkSessionRegistry>>);
impl SdkMcpSessionPlugins for SessionPlugins {
    fn plugins<'a>(
        &'a self,
        key: &'a JsValue,
    ) -> SdkMcpFuture<'a, Result<Vec<Arc<crate::runtime::plugins::LoadedPlugin>>, SdkMcpError>>
    {
        Box::pin(async move {
            let Some(key) = key.as_string() else {
                return Ok(Vec::new());
            };
            Ok(self.0.lock().await.session_plugins(key).to_vec())
        })
    }
}

pub fn routes(
    owner: SessionOwner,
    sdk: Arc<Mutex<SdkSessionRegistry>>,
    interactions: Arc<GatewayInteractionCoordinator>,
    permissions: Arc<GatewaySessionPermissionRuleSetRegistry>,
) -> McpRoutes {
    let closing = owner.clone();
    let projects = sdk
        .try_lock()
        .expect("MCP routes must be installed before SDK session configuration starts")
        .options()
        .projects
        .clone();
    let config = Arc::new(SdkMcpConfig::new(SdkMcpConfigOptions {
        resolver: Arc::new(SdkProjects(sdk.clone())),
        session_plugins: Arc::new(SessionPlugins(sdk.clone())),
        close_session: Arc::new(move |key, reason| {
            let owner = closing.clone();
            let interactions = interactions.clone();
            Box::pin(async move {
                let key = key.as_string().expect("MCP routes validate session keys");
                owner
                    .close(key)
                    .await
                    .map_err(|error| SdkMcpError::plain("Error", error.message))?;
                interactions.close_session(key, &reason.into());
                Ok(())
            })
        }),
        permission_rule_sets: permissions,
        environment: Arc::new(SystemEnvironment),
        projects,
    }));
    let resolver: Arc<dyn crate::runtime::sdk_session::SdkAgentMcpResolver> = config.clone();
    sdk.try_lock()
        .expect("MCP routes must be installed before SDK session configuration starts")
        .set_agent_mcp_resolver(Arc::downgrade(&resolver));
    McpRoutes::with_session_router(config, Arc::new(owner))
}

// B's native-adapter test builds its fixtures through `runtime::turn::wiring`,
// the channel composition root owned by the transferred channel track. The
// production path above is compiled and exercised by the MCP suites.
#[cfg(all(test, any()))]
mod tests {
    use super::*;
    use crate::runtime::{
        project::RuntimeError,
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwnerOptions,
        },
        turn::wiring,
    };
    struct TestSession(Arc<std::sync::atomic::AtomicBool>);
    impl LiveSession for TestSession {
        fn snapshot(&self) -> JsValue {
            JsValue::Null
        }
        fn abort(&self, _: &JsString) -> crate::runtime::project::RuntimeResult {
            Ok(())
        }
        fn dispose(&self) -> crate::runtime::project::DisposeFuture<'_> {
            Box::pin(async {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
    }
    struct TestFactory(Arc<std::sync::atomic::AtomicBool>);
    impl SessionFactory for TestFactory {
        fn create(
            &self,
            _: SessionContext,
            _: Option<LiveSessionHandle>,
        ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
            let session = self.0.clone();
            Box::pin(
                async move { Ok(LiveSessionHandle::new(Arc::new(TestSession(session)), None)) },
            )
        }
    }

    #[tokio::test]
    async fn native_adapter_waits_for_sdk_lock_and_observes_real_owner_admission() {
        let scratch = crate::scratch::Scratch::new("mcp-native");
        std::fs::write(
            scratch.path().join("pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: proof/base}\nmodel:\n  providers:\n    proof:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: public-test-only\n      models: {base: {}}\n",
        )
        .unwrap();
        let disposed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owner = SessionOwner::new(SessionOwnerOptions::new(Arc::new(TestFactory(
            disposed.clone(),
        ))));
        let providers =
            wiring::disk_providers(scratch.path().into(), scratch.path().into(), owner.clone())
                .unwrap();
        let sdk = providers.sdk.unwrap();
        let plugins = Arc::new(SessionPlugins(sdk.clone()));
        let guard = sdk.lock().await;
        let job = tokio::spawn(async move { plugins.plugins(&JsValue::from("s")).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), async {
                while !job.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
        );
        drop(guard);
        assert!(job.await.unwrap().unwrap().is_empty());
        let permissions = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
        let routes = routes(
            owner.clone(),
            sdk.clone(),
            Arc::new(GatewayInteractionCoordinator::default()),
            permissions.clone(),
        );
        let key = JsString::from("s");
        let run = JsString::from("r");
        let reservation = owner.begin_turn(&key, &run).unwrap();
        let input = crate::metadata::object([
            ("sessionKey", "s".into()),
            ("serverName", "a".into()),
            ("mode", "auto".into()),
        ]);
        let error = routes
            .handle("set_mcp_permission_mode_override", &input)
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some("SESSION_BUSY"));
        assert_eq!(permissions.size(), 0);
        owner.end_turn(&key, Some(&run), Some(&reservation));
        owner
            .get_or_create(SessionContext {
                session_key: key.clone(),
                project_key: None,
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            })
            .await
            .unwrap();
        assert!(
            routes
                .handle("set_mcp_permission_mode_override", &input)
                .unwrap()
                .await
                .is_ok()
        );
        assert!(disposed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(owner.current_handle(&key).is_none());

        let set_servers = crate::metadata::object([
            ("sessionKey", "s".into()),
            (
                "projectKey",
                scratch.path().to_string_lossy().as_ref().into(),
            ),
            (
                "servers",
                crate::metadata::object([(
                    "tickets",
                    crate::metadata::object([
                        ("type", "stdio".into()),
                        ("command", "ticket-server".into()),
                    ]),
                )]),
            ),
        ]);
        routes
            .handle("set_mcp_servers", &set_servers)
            .unwrap()
            .await
            .unwrap();
        let session_config = crate::metadata::object([(
            "agents",
            crate::metadata::object([(
                "reviewer",
                crate::metadata::object([("mcpServers", JsValue::Array(vec!["tickets".into()]))]),
            )]),
        )]);
        let mut registry = sdk.lock().await;
        registry
            .set_session_config(&key, &session_config, None, None)
            .await
            .unwrap();
        let resolved = registry
            .session_config(&key)
            .and_then(|config| config.get("agents"))
            .and_then(|agents| agents.get("reviewer"))
            .and_then(|agent| agent.get("mcpServers"))
            .and_then(|servers| servers.get("tickets"));
        assert_eq!(
            resolved.and_then(|server| server.get("command")),
            Some(&JsValue::from("ticket-server"))
        );
        drop(registry);
        owner.shutdown().await.unwrap();
    }
}
