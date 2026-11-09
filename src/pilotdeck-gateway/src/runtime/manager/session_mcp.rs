//! Session-bound MCP resources shared with browser reads and tool execution.
use crate::config::permission::{GatewaySessionPermissionRuleSetRegistry, Lease, RuleSetHandle};
use crate::runtime::{
    mcp::{
        bridge::{CreateToolDefinitionsOptions, create_mcp_tool_definitions_from_runtime},
        registry::{
            SessionMcpRuntimeRegistration, SessionMcpRuntimeRegistry, parse_plugin_mcp_servers,
        },
        runtime::{McpRuntime, McpRuntimeOptions, McpRuntimePort},
        sdk::{SdkMcpConfig, SdkMcpConfigOptions},
    },
    project::RuntimeError,
    tools::{ToolRegistration, ToolRegistry},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{path::Path, sync::Arc};

pub(crate) fn spawn_drop_cleanup(
    executor: tokio::runtime::Handle,
    resource: &'static str,
    cleanup: impl std::future::Future<Output = Result<(), RuntimeError>> + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<Result<(), RuntimeError>>> {
    // Like WriterCloseGuard, the blocking owner survives the dropping thread's
    // runtime context. It drives cleanup and observes its result itself.
    let thread = std::thread::Builder::new()
        .name(format!("{resource}-cleanup"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                executor.block_on(cleanup)
            }))
            .unwrap_or_else(|panic| {
                Err(RuntimeError::new(
                    crate::runtime::cleanup_task::panic_message(panic.as_ref())
                        .unwrap_or("cleanup panicked"),
                ))
            });
            if let Err(error) = &result {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "{resource} Drop cleanup failed: {error}"
                );
            }
            result
        });
    if let Err(error) = &thread {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "{resource} Drop cleanup thread failed: {error}"
        );
    }
    thread
}

pub struct SessionMcpOwner {
    registration: Arc<SessionMcpRuntimeRegistration>,
    permission: Lease,
    tools: Vec<Arc<dyn crate::runtime::tools::ExecutableTool>>,
    // Captured during construction, also usable from Drop without a current runtime.
    executor: tokio::runtime::Handle,
}
impl SessionMcpOwner {
    pub async fn open(
        config: &SdkMcpConfig,
        // Supply the same collaborators used to construct this SDK config.
        options: &SdkMcpConfigOptions,
        permissions: &GatewaySessionPermissionRuleSetRegistry,
        home: &Path,
        project: &Path,
        session: &JsString,
    ) -> Result<Option<Arc<Self>>, RuntimeError> {
        let project_key = JsString::from(project.to_string_lossy().into_owned());
        let resolved = options
            .resolver
            .resolve(Some(&project_key))
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message().units())))?;
        resolved
            .refresh()
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message().units())))?;
        let session_key = JsValue::String(session.clone());
        let plugins = resolved.snapshot().await;
        let session_plugins = options
            .session_plugins
            .plugins(&session_key)
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message().units())))?;
        let mut raw = JsObject::new();
        for plugin in plugins.into_iter().chain(session_plugins) {
            match &plugin.mcp_servers {
                Some(JsValue::Object(servers)) => {
                    for (name, spec) in servers.own_entries() {
                        raw.insert(name.clone(), spec.clone());
                    }
                }
                Some(JsValue::Array(servers)) => {
                    for (index, spec) in servers.iter().enumerate() {
                        raw.insert(index.to_string().into(), spec.clone());
                    }
                }
                _ => {}
            }
        }
        // Plugin names remain reserved, including invalid plugin specs.
        for (name, entry) in config.session_mcp_servers(&session_key) {
            if raw.get_key(&name).is_none() && entry.get("enabled").is_some_and(super::truthy) {
                let spec = entry
                    .get("config")
                    .ok_or_else(|| RuntimeError::new("SDK MCP entry is missing its config"))?;
                raw.insert(name, spec.clone());
            }
        }
        let parsed = parse_plugin_mcp_servers(Some(&raw), options.environment.as_ref());
        if let Some(error) = parsed.diagnostics.first() {
            return Err(RuntimeError::new(format!(
                "MCP {}: {}",
                error.id, error.message
            )));
        }
        let specs = parsed.servers;
        if specs.is_empty() {
            return Ok(None);
        }
        let runtime: Arc<dyn McpRuntimePort> =
            Arc::new(McpRuntime::new(specs, McpRuntimeOptions::default()));
        let registry = SessionMcpRuntimeRegistry::new();
        let registration = if runtime.server_info("browser-use").is_some() {
            super::BrowserManager::production()
                .register_session_runtime(
                    &registry,
                    home,
                    project,
                    session.clone(),
                    runtime.clone(),
                )
                .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message.units())))?
        } else {
            registry.register(
                &session
                    .to_utf8()
                    .map_err(|_| RuntimeError::new("MCP session requires UTF-8"))?,
                runtime.clone(),
            )?
        };
        let registration = Arc::new(registration);
        // Session creation can be cancelled while MCP initializes; retain cleanup then too.
        struct StartupCleanup {
            registration: Option<Arc<SessionMcpRuntimeRegistration>>,
            executor: tokio::runtime::Handle,
        }
        impl Drop for StartupCleanup {
            fn drop(&mut self) {
                if let Some(registration) = self.registration.take() {
                    drop(spawn_drop_cleanup(
                        self.executor.clone(),
                        "SessionMcpStartup",
                        async move { registration.dispose().await },
                    ));
                }
            }
        }
        let executor = tokio::runtime::Handle::current();
        let mut cleanup = StartupCleanup {
            registration: Some(registration.clone()),
            executor: executor.clone(),
        };
        let started = runtime.start().await;
        let statuses = match started {
            Ok(statuses) => statuses,
            Err(error) => {
                registration.dispose().await?;
                return Err(error);
            }
        };
        if let Some(failed) = statuses
            .iter()
            .find(|s| s.status != crate::runtime::mcp::protocol::McpStatus::Ready)
        {
            let error = RuntimeError::new(format!(
                "MCP {} startup failed: {}",
                failed.server_id,
                failed.error.as_deref().unwrap_or(failed.status.as_str())
            ));
            registration.dispose().await?;
            return Err(error);
        }
        let tools = match create_mcp_tool_definitions_from_runtime(
            runtime.clone(),
            CreateToolDefinitionsOptions::default(),
        )
        .await
        {
            Ok(tools) => tools
                .into_iter()
                .map(|tool| tool as Arc<dyn crate::runtime::tools::ExecutableTool>)
                .collect(),
            Err(error) => {
                registration.dispose().await?;
                return Err(error);
            }
        };
        let permission = permissions.acquire(session, None);
        cleanup.registration.take();
        Ok(Some(Arc::new(Self {
            registration,
            permission,
            tools,
            executor,
        })))
    }
    pub fn rules(&self) -> RuleSetHandle {
        self.permission.rules()
    }
    pub fn install(&self, registry: &ToolRegistry) -> Result<Vec<ToolRegistration>, RuntimeError> {
        self.tools
            .iter()
            .map(|tool| {
                registry
                    .register(tool.clone())
                    .map_err(|e| RuntimeError::new(e.message))
            })
            .collect()
    }
    pub async fn dispose(&self) -> Result<(), RuntimeError> {
        let result = self.registration.dispose().await;
        self.permission.release();
        result
    }
}

impl Drop for SessionMcpOwner {
    fn drop(&mut self) {
        self.permission.release();
        let registration = self.registration.clone();
        drop(spawn_drop_cleanup(
            self.executor.clone(),
            "SessionMcpOwner",
            async move { registration.dispose().await },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::mcp::runtime::{McpFuture, SessionMcpRuntime};
    use std::sync::mpsc;
    use std::time::Duration;

    struct StopProbe(mpsc::Sender<()>);
    impl SessionMcpRuntime for StopProbe {
        fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
            Box::pin(async move {
                self.0.send(()).unwrap();
                Ok(())
            })
        }
    }

    #[test]
    fn drop_without_current_runtime_disposes_registration() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (stopped, observed) = mpsc::channel();
        let registry = SessionMcpRuntimeRegistry::new();
        let registration = Arc::new(
            registry
                .register("drop-test", Arc::new(StopProbe(stopped)))
                .unwrap(),
        );
        let permissions = GatewaySessionPermissionRuleSetRegistry::default();
        let owner = SessionMcpOwner {
            registration,
            permission: permissions.acquire(&"drop-test".into(), None),
            tools: Vec::new(),
            executor: runtime.handle().clone(),
        };
        std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            drop(owner);
        })
        .join()
        .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn drop_cleanup_reports_error_and_panic_to_joiner() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        for panic in [false, true] {
            let task = spawn_drop_cleanup(runtime.handle().clone(), "test", async move {
                if panic {
                    panic!("cleanup probe panic");
                }
                Err(RuntimeError::new("cleanup probe error"))
            })
            .unwrap();
            let result = task.join().unwrap().unwrap_err();
            assert_eq!(
                result.message,
                if panic {
                    "cleanup probe panic"
                } else {
                    "cleanup probe error"
                }
            );
        }
    }
}
