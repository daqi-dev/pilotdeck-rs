//! Native web tools share the leased project's router and captured config.
use crate::{
    host::{
        HostError, ModuleError,
        modules::{
            model::{ModelExecution, ModelFuture, ModelStream, PrepareInput, StreamInput},
            model_port::{self, RouterModelPortOptions},
        },
    },
    runtime::{
        model::{
            http::{HyperModelTransport, ModelHttpTransport},
            router::{AbortFlag, RouterRuntime},
        },
        tools::{
            ToolRegistry, ToolRunContext,
            builtin::web::{
                CustomProviderConfig, SecondaryModel, SecondaryModelEvent, SecondaryNext,
                SecondaryStream, UrlFetcher, WebFetchTool, WebFetchToolOptions, WebSearchOptions,
                WebSearchTool,
            },
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::{sync::Arc, time::Duration};

pub(super) struct NativeWebTools {
    router: Arc<RouterRuntime>,
    model_config: JsValue,
    project: JsString,
    search: Option<WebSearchOptions>,
    transport: Arc<dyn ModelHttpTransport>,
}
impl NativeWebTools {
    pub(super) fn search_enabled(&self) -> bool {
        self.search.is_some()
    }
    pub fn new(
        router: Arc<RouterRuntime>,
        model_config: JsValue,
        snapshot: &JsValue,
        project: JsString,
        env: &crate::config::Env,
    ) -> Self {
        let proxy = snapshot
            .get("config")
            .and_then(|v| v.get("proxy"))
            .and_then(crate::runtime::model::http::ProxySettings::from_config);
        let transport: Arc<dyn ModelHttpTransport> = Arc::new(
            HyperModelTransport::with_proxy_settings(proxy.as_ref(), env),
        );
        let search = search_options(snapshot, transport.clone());
        Self {
            router,
            model_config,
            project,
            search,
            transport,
        }
    }
    pub fn register(
        &self,
        registry: &ToolRegistry,
        identity: &ExecutionIdentity,
    ) -> Result<(), HostError> {
        if let Some(options) = &self.search {
            registry.register(Arc::new(WebSearchTool::new(options.clone())))?;
        }
        let ports = model_port::ports(
            self.router.clone(),
            self.model_config.clone(),
            None,
            RouterModelPortOptions {
                is_main_agent: false,
                project_path: Some(self.project.clone()),
                ..Default::default()
            },
            &identity.session_id,
        );
        let model = Arc::new(Auxiliary {
            execution: ports.execution,
            context: crate::metadata::object([
                ("sessionId", JsValue::String(identity.session_id.clone())),
                ("turnId", JsValue::String(identity.turn_id.clone())),
                ("projectPath", JsValue::String(self.project.clone())),
            ])
            .as_object()
            .expect("object")
            .clone(),
        });
        registry.register(Arc::new(WebFetchTool::new(WebFetchToolOptions {
            model: Some(model),
            provider: None,
            model_id: None,
            max_output_tokens: None,
            fetcher: UrlFetcher::new(self.transport.clone()),
        })))?;
        Ok(())
    }
}
fn text(value: &JsValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
}
fn search_options(
    snapshot: &JsValue,
    transport: Arc<dyn ModelHttpTransport>,
) -> Option<WebSearchOptions> {
    // ProjectExecutionWorldBundle.resolveWebSearchOptions: absence disables
    // this optional tool, even though the generic registry defaults enable it.
    let config = snapshot.get("config")?.get("tools")?.get("webSearch")?;
    if matches!(
        config,
        JsValue::Null | JsValue::Undefined | JsValue::Bool(false)
    ) || config.get("enabled").and_then(JsValue::as_bool) == Some(false)
    {
        return None;
    }
    let mut options = WebSearchOptions::new(transport);
    options.provider = text(config, "provider");
    options.api_key = text(config, "apiKey");
    options.endpoint = text(config, "endpoint");
    options.custom_provider = config.get("customProvider").map(|v| CustomProviderConfig {
        name: text(v, "name"),
        auth: text(v, "auth"),
        method: text(v, "method"),
        query_param: text(v, "queryParam"),
        api_key_param: text(v, "apiKeyParam"),
        results_path: text(v, "resultsPath"),
        title_field: text(v, "titleField"),
        url_field: text(v, "urlField"),
        snippet_field: text(v, "snippetField"),
        source_field: text(v, "sourceField"),
        published_at_field: text(v, "publishedAtField"),
    });
    Some(options)
}
struct Auxiliary {
    execution: Arc<dyn ModelExecution>,
    context: JsObject,
}
impl SecondaryModel for Auxiliary {
    fn stream(&self, request: JsValue, signal: Option<ToolRunContext>) -> Box<dyn SecondaryStream> {
        let abort = AbortFlag::new();
        let forward = signal.map(|context| {
            if context.is_aborted() {
                abort.abort();
            }
            let child = abort.clone();
            tokio::spawn(async move {
                context.cancelled().await;
                child.abort();
            })
        });
        let execution = self.execution.clone();
        let context = self.context.clone();
        let cancel = abort.clone();
        let start = Box::pin(async move {
            tokio::select! {
                _=cancel.cancelled()=>Err(ModuleError::new("Secondary model request was cancelled.")),
                result=async {
                    let prepared=execution.prepare(PrepareInput {request,context:context.clone()}).await?;
                    execution.stream(StreamInput {prepared,context,abort:Some(cancel.clone())}).await
                }=>result,
            }
        });
        Box::new(AuxiliaryStream {
            start: Some(start),
            stream: None,
            abort,
            forward,
            done: false,
        })
    }
}
struct AuxiliaryStream {
    start: Option<ModelFuture<Box<dyn ModelStream>>>,
    stream: Option<Box<dyn ModelStream>>,
    abort: AbortFlag,
    forward: Option<tokio::task::JoinHandle<()>>,
    done: bool,
}
impl Drop for AuxiliaryStream {
    fn drop(&mut self) {
        self.abort.abort();
        if let Some(task) = self.forward.take() {
            task.abort();
        }
    }
}
fn thrown(error: ModuleError) -> SecondaryModelEvent {
    let mut value = JsObject::new();
    value.insert("message".into(), JsValue::String(error.message));
    if let Some(code) = error.code {
        value.insert("code".into(), JsValue::String(code));
    }
    SecondaryModelEvent::Throw {
        is_error: true,
        value: JsValue::Object(value),
    }
}
impl SecondaryStream for AuxiliaryStream {
    fn next(&mut self) -> SecondaryNext<'_> {
        Box::pin(async move {
            if self.done {
                return None;
            }
            if let Some(start) = self.start.take() {
                match start.await {
                    Ok(stream) => self.stream = Some(stream),
                    Err(error) => {
                        self.done = true;
                        return Some(thrown(error));
                    }
                }
            }
            let stream = self.stream.as_mut()?;
            let result = tokio::select! {
                _=self.abort.cancelled()=>Err(ModuleError::new("Secondary model request was cancelled.")),
                result=stream.next()=>result,
            };
            match result {
                Ok(Some(event)) => Some(SecondaryModelEvent::Event(event)),
                result => {
                    self.done = true;
                    let _ = tokio::time::timeout(Duration::from_secs(2), stream.close()).await;
                    result.err().map(thrown)
                }
            }
        })
    }
}
