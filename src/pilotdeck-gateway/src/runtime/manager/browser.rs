use super::{RequestError, items, resolve_scope, truthy};
use crate::runtime::mcp::client::{McpCallOptions, McpClientError};
use crate::runtime::mcp::registry::{SessionMcpRuntimeRegistration, SessionMcpRuntimeRegistry};
use crate::runtime::mcp::runtime::{McpFuture, McpRuntimePort, SessionMcpRuntime};
use crate::runtime::project::RuntimeError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Clone)]
struct Binding {
    id: u64,
    home: PathBuf,
    project: PathBuf,
    session: JsString,
    runtime: Arc<dyn McpRuntimePort>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    bindings: Vec<Binding>,
}

/// Register the existing per-session browser-use runtime, retaining the lease
/// for exactly its owning session lifetime. This manager never starts an MCP.
#[derive(Clone, Default)]
pub struct BrowserManager(Arc<Mutex<State>>);

pub struct BrowserRuntimeLease {
    state: Weak<Mutex<State>>,
    id: u64,
}

struct OwnedBrowserRuntime {
    runtime: Arc<dyn McpRuntimePort>,
    lease: Mutex<Option<BrowserRuntimeLease>>,
}

impl SessionMcpRuntime for OwnedBrowserRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        drop(
            self.lease
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        );
        self.runtime.stop()
    }
}

impl Drop for BrowserRuntimeLease {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .bindings
                .retain(|entry| entry.id != self.id);
        }
    }
}

impl BrowserManager {
    /// Use this instance explicitly in GatewayServices and its session MCP owner.
    /// The returned registration owns both MCP stop and browser deregistration.
    pub fn register_session_runtime(
        &self,
        registry: &SessionMcpRuntimeRegistry,
        home: &Path,
        project: &Path,
        session: JsString,
        runtime: Arc<dyn McpRuntimePort>,
    ) -> Result<SessionMcpRuntimeRegistration, RequestError> {
        let key = session
            .to_utf8()
            .map_err(|_| RequestError::new("MCP runtime registry requires a UTF-8 sessionKey."))?;
        let lease = self.register(home, project, session, runtime.clone())?;
        registry
            .register(
                &key,
                Arc::new(OwnedBrowserRuntime {
                    runtime,
                    lease: Mutex::new(Some(lease)),
                }),
            )
            .map_err(|error| RequestError::new(error.to_string()))
    }

    /// The three-argument gateway entry and LocalServer composition share this owner.
    pub fn production() -> &'static Self {
        static MANAGER: OnceLock<BrowserManager> = OnceLock::new();
        MANAGER.get_or_init(Self::default)
    }

    pub fn register(
        &self,
        home: &Path,
        project: &Path,
        session: JsString,
        runtime: Arc<dyn McpRuntimePort>,
    ) -> Result<BrowserRuntimeLease, RequestError> {
        if !home.is_absolute() || !project.is_absolute() {
            return Err(RequestError::new(
                "Browser runtime registration requires absolute home and project paths.",
            ));
        }
        let mut params = JsObject::new();
        params.insert(
            "projectKey".into(),
            project.to_string_lossy().as_ref().into(),
        );
        let scope = resolve_scope(home, project, &JsValue::Object(params))?;
        if session.is_empty() {
            return Err(RequestError::new(
                "Browser runtime registration requires a sessionKey.",
            ));
        }
        if runtime.server_info("browser-use").is_none() {
            return Err(RequestError::new(
                "Browser runtime does not own the browser-use MCP server.",
            ));
        }
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.next_id = state.next_id.checked_add(1).ok_or_else(|| {
            RequestError::new("Browser runtime registration generation exhausted.")
        })?;
        let id = state.next_id;
        state.bindings.retain(|entry| {
            !(entry.home == scope.metadata.home
                && entry.project == scope.project
                && entry.session == session)
        });
        state.bindings.push(Binding {
            id,
            home: scope.metadata.home,
            project: scope.project,
            session,
            runtime,
        });
        Ok(BrowserRuntimeLease {
            state: Arc::downgrade(&self.0),
            id,
        })
    }

    pub async fn browsers(
        &self,
        home: &Path,
        cwd: &Path,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        let (home, cwd, params) = (home.to_owned(), cwd.to_owned(), params.clone());
        let scope = tokio::task::spawn_blocking(move || resolve_scope(&home, &cwd, &params))
            .await
            .map_err(|error| RequestError::new(error.to_string()))??;
        let bindings: Vec<_> = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bindings
            .iter()
            .filter(|entry| {
                entry.home == scope.metadata.home
                    && entry.project == scope.project
                    && (!truthy(&scope.session)
                        || scope.session.as_string() == Some(&entry.session))
            })
            .cloned()
            .collect();
        let mut result = Vec::new();
        let name: JsString = "browser_tabs".into();
        let mut arguments = JsObject::new();
        arguments.insert("action".into(), "list".into());
        let arguments = JsValue::Object(arguments);
        for binding in bindings {
            // @playwright/mcp 0.0.75 uses ensureTab() for list: it can lazily open a blank tab.
            let listed = binding
                .runtime
                .call_tool("browser-use", &name, &arguments, McpCallOptions::default())
                .await
                .map_err(mcp_error)?;
            if listed.is_error == Some(true) || !matches!(&listed.content, JsValue::Array(_)) {
                let mut error = RequestError::new("Browser tab enumeration failed.");
                error.details = Some(Box::new(listed.to_value()));
                return Err(error);
            }
            let mut item = JsObject::new();
            item.insert(
                "projectKey".into(),
                binding.project.to_string_lossy().as_ref().into(),
            );
            item.insert("sessionKey".into(), JsValue::String(binding.session));
            item.insert("serverId".into(), "browser-use".into());
            // The host seam has unknown[] items; keep actual MCP content instead of parsing Markdown into guessed tabs.
            item.insert("content".into(), listed.content);
            result.push(JsValue::Object(item));
        }
        Ok(items(result))
    }
}

fn mcp_error(error: McpClientError) -> RequestError {
    let value = error.to_value();
    let mut result = RequestError::new(error.message());
    if let Some(code) = value
        .get("code")
        .and_then(JsValue::as_string)
        .and_then(|code| code.to_utf8().ok())
    {
        result.code = code;
    }
    result.details = Some(Box::new(value));
    result
}
