//! Default project factory (`createEdgeClawMemoryProviderFromConfig.ts:45-111`).
use super::{
    llm::{
        config::resolve_memory_llm,
        transport::{LlmError, LlmRuntime},
    },
    messages::CaptureStrategy,
    provider::{EdgeClawMemoryProvider, EdgeClawMemoryProviderOptions},
    service::{
        ClearMemoryScope, EdgeClawMemoryService, ServiceDependencies, ServiceError, ServiceOptions,
    },
};
use crate::runtime::memory::{
    MemoryBundleError, MemoryCaptureStrategy, MemoryFuture, MemoryProviderError,
    NativeMemoryProviderOptions, NativeMemoryService, NativeProjectMemoryProvider,
    maintenance::MaintenanceTelemetry,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{path::PathBuf, sync::Arc, time::UNIX_EPOCH};

/// Callable memory telemetry carried by the project's opaque telemetry slot.
/// Hosts may opt in to callable native telemetry with this wrapper. Other
/// opaque host clients remain valid factory inputs and run without native
/// telemetry; injected factories still receive the original object unchanged.
pub struct MemoryTelemetry(pub Arc<dyn MaintenanceTelemetry>);
struct RuntimeTelemetry(Arc<dyn MaintenanceTelemetry>);
impl LlmRuntime for RuntimeTelemetry {
    fn track_feature_loop_stage(&self, event: JsValue) {
        self.0.track_feature_loop_stage(event);
    }
    fn track_error(&self, error: &LlmError, event: JsValue) {
        let mut provider = MemoryProviderError::new(error.message.clone());
        provider.name = error.name.into();
        self.0.track_error(&provider, event);
    }
}

pub struct CreatedMemoryProvider {
    pub provider: Arc<EdgeClawMemoryProvider>,
    pub service: Arc<EdgeClawMemoryService>,
}
impl CreatedMemoryProvider {
    pub fn into_native(self) -> NativeProjectMemoryProvider {
        NativeProjectMemoryProvider {
            provider: self.provider,
            service: self.service,
        }
    }
}

pub fn create_from_config(
    options: &NativeMemoryProviderOptions,
) -> Result<Option<CreatedMemoryProvider>, MemoryBundleError> {
    create_with_dependencies(options, ServiceDependencies::default())
}

/// Same factory with the service's existing deterministic host seams. The
/// selection, option mapping and provider assembly are shared with production.
pub fn create_with_dependencies(
    options: &NativeMemoryProviderOptions,
    mut dependencies: ServiceDependencies,
) -> Result<Option<CreatedMemoryProvider>, MemoryBundleError> {
    let Some(config) = options
        .config
        .as_ref()
        .filter(|c| c.enabled && c.provider.equals_str("edgeclaw"))
    else {
        return Ok(None);
    };
    let executor = tokio::runtime::Handle::try_current().map_err(|_| {
        MemoryBundleError::Factory(MemoryProviderError::new(
            "Native memory requires a Tokio runtime.",
        ))
    })?;
    let telemetry = options.telemetry.as_ref().and_then(|value| {
        value
            .downcast_ref::<MemoryTelemetry>()
            .map(|value| value.0.clone())
    });
    if crate::mutants::on("native-opaque-telemetry-rejected")
        && options.telemetry.is_some()
        && telemetry.is_none()
    {
        return Err(MemoryBundleError::Factory(MemoryProviderError::new(
            "Native memory telemetry adapter is unavailable.",
        )));
    }
    if let Some(telemetry) = &telemetry {
        dependencies.runtime = Some(Arc::new(RuntimeTelemetry(telemetry.clone())));
    }
    let mut service_options = ServiceOptions::new(options.project_root.clone());
    // Node filesystem paths use UTF-8 replacement encoding for lone surrogates;
    // this is a path boundary, not a session/key identity conversion (M21 config).
    service_options.root_dir = Some(if crate::mutants::on("native-root-ignored") {
        options.project_root.join("mutant-store")
    } else {
        PathBuf::from(String::from_utf16_lossy(config.root_dir.units()))
    });
    service_options.capture_strategy = match config.capture_strategy {
        MemoryCaptureStrategy::LastTurn => CaptureStrategy::LastTurn,
        MemoryCaptureStrategy::FullSession => CaptureStrategy::FullSession,
    };
    service_options.include_assistant = Some(config.include_assistant);
    service_options.max_message_chars = if crate::mutants::on("native-max-message-chars-drop") {
        None
    } else {
        config.max_message_chars
    };
    // Preserve the JS number through construction. SQL LIMIT and preferred-key
    // slice apply different number semantics when heartbeat actually runs.
    service_options.heartbeat_batch_size = config.heartbeat_batch_size;
    if crate::mutants::on("native-heartbeat-fraction-truncated") {
        service_options.heartbeat_batch_size = config.heartbeat_batch_size.map(f64::trunc);
    }
    if crate::mutants::on("native-heartbeat-batch") {
        service_options.heartbeat_batch_size = None;
    }
    service_options.default_indexing_settings = config.schedule.as_ref().map(|schedule| {
        let mut value = JsObject::new();
        if let Some(mode) = &schedule.reasoning_mode {
            value.insert("reasoningMode".into(), JsValue::String(mode.clone()));
        }
        if let Some(minutes) = schedule.auto_index_interval_minutes {
            value.insert("autoIndexIntervalMinutes".into(), JsValue::Number(minutes));
        }
        if let Some(minutes) = schedule.auto_dream_interval_minutes {
            value.insert("autoDreamIntervalMinutes".into(), JsValue::Number(minutes));
        }
        JsValue::Object(value)
    });
    service_options.source = Some("pilotdeck".into());
    service_options.llm = if crate::mutants::on("native-factory-ignore-llm") {
        None
    } else {
        resolve_memory_llm(
            config,
            options.model_config.as_ref(),
            options.agent_model.as_deref(),
        )
    };
    let service = Arc::new(
        EdgeClawMemoryService::with_dependencies(service_options, dependencies)
            .map_err(|e| MemoryBundleError::Factory(service_error(e)))?,
    );
    let mut provider_options = EdgeClawMemoryProviderOptions::new(service.clone());
    provider_options.source = Some(
        if crate::mutants::on("native-provider-source") {
            "mutant"
        } else {
            "pilotdeck"
        }
        .into(),
    );
    if crate::mutants::on("native-provider-retrieval-mode") {
        provider_options.retrieval_mode = Some("simple".into());
    }
    provider_options.telemetry = telemetry;
    // TS passes options.now only to the provider, never to the service clock.
    provider_options.now = options.now.clone().map(|now| {
        Arc::new(move || {
            let ms = match now().duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs_f64() * 1000.0,
                Err(e) => -e.duration().as_secs_f64() * 1000.0,
            };
            crate::runtime::context::prompt::iso_timestamp(ms)
        }) as super::file_memory::FileMemoryClock
    });
    let provider = Arc::new(EdgeClawMemoryProvider::new(provider_options, executor));
    Ok(Some(CreatedMemoryProvider { provider, service }))
}

fn service_error(error: ServiceError) -> MemoryProviderError {
    error.into()
}
impl NativeMemoryService for EdgeClawMemoryService {
    fn run_due_scheduled_maintenance(&self, reason: &str) -> MemoryFuture<'_, ()> {
        let reason = reason.to_owned();
        if crate::mutants::on("native-maintenance-noop") {
            return Box::pin(async { Ok(()) });
        }
        Box::pin(async move {
            EdgeClawMemoryService::run_due_scheduled_maintenance(self, &reason)
                .await
                .map(|_| ())
                .map_err(service_error)
        })
    }
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        let mut options = JsObject::new();
        options.insert("limit".into(), JsValue::Number(limit as f64));
        match &EdgeClawMemoryService::list(self, &options).map_err(service_error)? {
            JsValue::Array(items) => Ok(items.clone()),
            _ => Err(MemoryProviderError::new(
                "Native memory list did not return an array.",
            )),
        }
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        EdgeClawMemoryService::clear(
            self,
            if scope == "all_memory" {
                ClearMemoryScope::AllMemory
            } else {
                ClearMemoryScope::CurrentProject
            },
        )
        .map(|_| ())
        .map_err(service_error)
    }
    fn clear_session(&self, key: &JsString) -> Result<(), MemoryProviderError> {
        let key = key
            .to_utf8()
            .map_err(|_| MemoryProviderError::host_required())?;
        EdgeClawMemoryService::clear_session(self, &key)
            .map(|_| ())
            .map_err(service_error)
    }
    fn close(&self) -> Result<(), MemoryProviderError> {
        if crate::mutants::on("native-close-noop") {
            Ok(())
        } else {
            EdgeClawMemoryService::close(self).map_err(service_error)
        }
    }
}

#[cfg(test)]
#[path = "../../../../tests/edgeclaw_native_default.rs"]
mod tests;
