//! `McpRuntime` and its port, ported from the fixed TypeScript baseline's
//! `src/mcp/runtime/{McpRuntime,McpRuntimePort}.ts`: one [`McpClient`] per
//! server spec, started with bounded concurrency (M4, default 5), with the
//! aggregate tool list the tool bridge consumes.
//!
//! Like the TS `Map`, a later spec with an existing id replaces that
//! client but keeps its original position. `start()` reports statuses in
//! completion order (each worker appends when its client settles).

use super::client::{McpCallOptions, McpClient, McpClientError, McpClientOptions, McpErrorCode};
use super::protocol::{McpServerSpec, McpStatus, McpToolSpec, McpTransportSpec};
use crate::runtime::project::RuntimeError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// Boxed future used by the MCP runtime ports.
pub type McpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `PilotDeckMcpClientStatusEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpClientStatusEntry {
    pub server_id: String,
    pub status: McpStatus,
    pub error: Option<String>,
}
impl McpClientStatusEntry {
    /// `{ serverId, status, error? }`.
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "serverId".into(),
            JsValue::String(self.server_id.as_str().into()),
        );
        object.insert(
            "status".into(),
            JsValue::String(self.status.as_str().into()),
        );
        if let Some(error) = &self.error {
            object.insert("error".into(), JsValue::String(error.as_str().into()));
        }
        JsValue::Object(object)
    }
}

/// `PilotDeckMcpServerInstructions`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpServerInstructions {
    pub server_id: String,
    pub instructions: JsString,
}

/// `McpRuntimeServerInfo`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpRuntimeServerInfo {
    /// `"stdio"`, `"streamable_http"` or `"sse"`.
    pub transport: &'static str,
    /// Stdio working directory, used to resolve returned local media links.
    pub cwd: Option<String>,
}

pub use super::client::McpToolCallResult;

/// Provider-neutral MCP capability consumed by composition and the tool
/// bridge (`McpRuntimePort`). It deliberately does not expose `McpClient`.
/// Anything a per-session registry can stop (`SessionMcpRuntime`).
pub trait SessionMcpRuntime: Send + Sync {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>>;
}

/// Port rejections (`start()` / `listAllTools()` of an injected runtime) are
/// [`RuntimeError`]s; the native runtime never rejects.
pub trait McpRuntimePort: SessionMcpRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>>;
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>>;
    fn server_info(&self, server_id: &str) -> Option<McpRuntimeServerInfo>;
    fn call_tool<'a>(
        &'a self,
        server_id: &'a str,
        tool_name: &'a JsString,
        input: &'a JsValue,
        options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>>;
}

/// `McpRuntimeFactory`: composition-time provider selector.
pub type McpRuntimeFactory =
    Arc<dyn Fn(Vec<McpServerSpec>) -> Arc<dyn McpRuntimePort> + Send + Sync>;

/// `createNativeMcpRuntime`.
pub fn native_mcp_runtime_factory() -> McpRuntimeFactory {
    Arc::new(|servers| Arc::new(McpRuntime::new(servers, McpRuntimeOptions::default())))
}

/// `McpRuntimeOptions`.
#[derive(Clone, Debug, Default)]
pub struct McpRuntimeOptions {
    /// Max parallel connects during `start()` (M4). Default 5.
    pub connect_concurrency: Option<usize>,
    pub client_options: McpClientOptions,
}

/// `McpRuntime`.
pub struct McpRuntime {
    servers: Vec<McpServerSpec>,
    clients: Vec<(String, McpClient)>,
    connect_concurrency: usize,
}

impl McpRuntime {
    pub fn new(servers: Vec<McpServerSpec>, options: McpRuntimeOptions) -> Self {
        let mut clients: Vec<(String, McpClient)> = Vec::new();
        for spec in &servers {
            let client = McpClient::new(spec.clone(), options.client_options.clone());
            match clients.iter_mut().find(|(id, _)| *id == spec.id) {
                Some(entry) => entry.1 = client,
                None => clients.push((spec.id.clone(), client)),
            }
        }
        Self {
            servers,
            clients,
            connect_concurrency: options.connect_concurrency.unwrap_or(5),
        }
    }

    /// The specs the runtime was built from (`runtime.servers`).
    pub fn servers(&self) -> &[McpServerSpec] {
        &self.servers
    }

    pub fn client(&self, server_id: &str) -> Option<&McpClient> {
        self.clients
            .iter()
            .find(|(id, _)| id == server_id)
            .map(|(_, client)| client)
    }

    /// Start every client; per-server failures are recorded, not raised.
    pub async fn start(&self) -> Vec<McpClientStatusEntry> {
        let queue: Arc<Mutex<VecDeque<McpClient>>> = Arc::new(Mutex::new(
            self.clients
                .iter()
                .map(|(_, client)| client.clone())
                .collect(),
        ));
        let results = Arc::new(Mutex::new(Vec::new()));
        let workers = self.connect_concurrency.min(self.clients.len()).max(1);
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..workers {
            let queue = queue.clone();
            let results = results.clone();
            set.spawn(async move {
                loop {
                    let Some(client) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
                    else {
                        break;
                    };
                    let entry = match client.start().await {
                        Ok(()) => McpClientStatusEntry {
                            server_id: client.spec().id.clone(),
                            status: client.status(),
                            error: None,
                        },
                        Err(error) => McpClientStatusEntry {
                            server_id: client.spec().id.clone(),
                            status: McpStatus::Error,
                            error: Some(error.message().to_owned()),
                        },
                    };
                    results
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(entry);
                }
            });
        }
        while set.join_next().await.is_some() {}
        std::mem::take(&mut *results.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn server_info(&self, server_id: &str) -> Option<McpRuntimeServerInfo> {
        let spec = self.client(server_id)?.spec();
        Some(match &spec.transport {
            McpTransportSpec::Stdio { cwd, .. } => McpRuntimeServerInfo {
                transport: "stdio",
                cwd: cwd.clone().filter(|cwd| !cwd.is_empty()),
            },
            McpTransportSpec::StreamableHttp { .. } => McpRuntimeServerInfo {
                transport: "streamable_http",
                cwd: None,
            },
            McpTransportSpec::Sse { .. } => McpRuntimeServerInfo {
                transport: "sse",
                cwd: None,
            },
        })
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &JsString,
        input: &JsValue,
        options: McpCallOptions,
    ) -> Result<McpToolCallResult, McpClientError> {
        let Some(client) = self.client(server_id) else {
            return Err(McpClientError::with_code(
                "Error",
                McpErrorCode::Text("mcp_server_not_registered"),
                format!("MCP server {server_id} is not registered"),
            ));
        };
        client.call_tool(tool_name, input, options).await
    }

    /// Dispose every client concurrently, including retired cleanup tasks.
    pub async fn stop(&self) {
        let mut set = tokio::task::JoinSet::new();
        for (_, client) in &self.clients {
            let client = client.clone();
            set.spawn(async move { client.dispose().await });
        }
        while set.join_next().await.is_some() {}
    }

    /// Every advertised tool of every `ready` client; a failing list is
    /// skipped (start already recorded the error).
    pub async fn list_all_tools(&self) -> Vec<McpToolSpec> {
        let mut out = Vec::new();
        for (_, client) in &self.clients {
            if client.status() != McpStatus::Ready {
                continue;
            }
            if let Ok(tools) = client.list_tools().await {
                out.extend(tools);
            }
        }
        out
    }

    /// Non-blank server instructions of ready clients, by `localeCompare`
    /// of the server id. (The baseline client never surfaces instructions,
    /// so this is empty in practice; kept for the port's shape.)
    pub fn instructions(&self) -> Vec<McpServerInstructions> {
        let mut out: Vec<McpServerInstructions> = self
            .clients
            .iter()
            .filter(|(_, client)| client.status() == McpStatus::Ready)
            .filter_map(|(id, client)| {
                let instructions = client.instructions();
                let text = String::from_utf16_lossy(instructions.units());
                (!crate::metadata::js_trim(&text).is_empty()).then(|| McpServerInstructions {
                    server_id: id.clone(),
                    instructions,
                })
            })
            .collect();
        let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
        if let Ok(collator) = crate::project_files::node_collator(&locale) {
            out.sort_by(|a, b| {
                let left: Vec<u16> = a.server_id.encode_utf16().collect();
                let right: Vec<u16> = b.server_id.encode_utf16().collect();
                collator.compare_utf16(&left, &right)
            });
        }
        out
    }

    pub fn statuses(&self) -> Vec<McpClientStatusEntry> {
        self.clients
            .iter()
            .map(|(id, client)| McpClientStatusEntry {
                server_id: id.clone(),
                status: client.status(),
                error: None,
            })
            .collect()
    }
}

impl SessionMcpRuntime for McpRuntime {
    fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            McpRuntime::stop(self).await;
            Ok(())
        })
    }
}

impl McpRuntimePort for McpRuntime {
    fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
        Box::pin(async move { Ok(McpRuntime::start(self).await) })
    }
    fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
        Box::pin(async move { Ok(McpRuntime::list_all_tools(self).await) })
    }
    fn server_info(&self, server_id: &str) -> Option<McpRuntimeServerInfo> {
        McpRuntime::server_info(self, server_id)
    }
    fn call_tool<'a>(
        &'a self,
        server_id: &'a str,
        tool_name: &'a JsString,
        input: &'a JsValue,
        options: McpCallOptions,
    ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
        Box::pin(McpRuntime::call_tool(
            self, server_id, tool_name, input, options,
        ))
    }
}
