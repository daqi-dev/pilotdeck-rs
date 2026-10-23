//! Process-local provider registrations and invocation leases. Baseline:
//! ModelInvocationProviderRegistry.ts:74-268. Each project generation owns
//! its own registry; replacement within that scope publishes before retiring.
#![allow(clippy::result_large_err)]
use super::{
    fallback::RouterStreamFuture,
    http::{self, ModelHttpTransport, ModelStreamError, StreamOptions, StreamOutput},
    router::{AbortFlag, RouterFuture, RouterInvokeError, RouterModelInvocation},
};
use crate::{
    host::ModuleError,
    runtime::project::{DisposeFuture, RuntimeError, RuntimeResult},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{runtime::Handle, sync::watch};

/// One stable provider route; a registration owns disposal, never the router.
pub trait ModelInvocationProvider: RouterModelInvocation {
    fn provider_id(&self) -> &JsString;
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
struct Entry {
    provider: Arc<dyn ModelInvocationProvider>,
    generation: u64,
    order: u64,
    state: Mutex<EntryState>,
    changed: watch::Sender<()>,
    completion: watch::Sender<Option<RuntimeResult>>,
    executor: Handle,
}
#[derive(Default)]
struct EntryState {
    leases: usize,
    retiring: bool,
}
impl Entry {
    fn retire(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.retiring {
            return;
        }
        state.retiring = true;
        let entry = self.clone();
        self.executor.spawn(async move {
            entry.drained().await;
            // Isolate a provider panic so every waiter still completes.
            let cleanup = entry.clone();
            let result = entry
                .executor
                .spawn(async move { cleanup.provider.dispose().await })
                .await
                .unwrap_or_else(|error| {
                    Err(RuntimeError::new(format!(
                        "Provider cleanup task failed: {error}"
                    )))
                });
            entry.completion.send_replace(Some(result));
        });
    }
    async fn drained(&self) {
        let mut changed = self.changed.subscribe();
        loop {
            changed.borrow_and_update();
            if self.state.lock().unwrap().leases == 0 {
                return;
            }
            if changed.changed().await.is_err() {
                return;
            }
        }
    }
    async fn disposed(&self) -> RuntimeResult {
        let mut done = self.completion.subscribe();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result;
            }
            done.changed().await.expect("entry owns completion sender");
        }
    }
}
/// `registration.dispose` removes only that registration, even after replace.
#[derive(Clone)]
pub struct ProviderRegistration {
    registry: Arc<Mutex<State>>,
    entry: Arc<Entry>,
}
impl ProviderRegistration {
    pub fn generation(&self) -> u64 {
        self.entry.generation
    }
    pub async fn when_drained(&self) {
        self.entry.drained().await;
    }
    pub async fn dispose(&self) -> RuntimeResult {
        self.retire();
        self.entry.disposed().await
    }
    fn retire(&self) {
        let mut registry = self.registry.lock().unwrap();
        let id = self.entry.provider.provider_id();
        if registry
            .entries
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.entry))
        {
            registry.entries.remove(id);
        }
        self.entry.retire();
    }
}
pub struct ProviderReplacement {
    pub registration: ProviderRegistration,
    pub retired: Option<ProviderRegistration>,
}
/// Dropping a pending stream/complete future releases its lease as well.
pub struct ProviderLease {
    entry: Arc<Entry>,
    released: bool,
}
impl ProviderLease {
    pub fn generation(&self) -> u64 {
        self.entry.generation
    }
    pub fn provider(&self) -> &Arc<dyn ModelInvocationProvider> {
        &self.entry.provider
    }
    pub fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.entry.state.lock().unwrap().leases -= 1;
            self.entry.changed.send_replace(());
        }
    }
}
impl Drop for ProviderLease {
    fn drop(&mut self) {
        self.release();
    }
}
struct State {
    entries: BTreeMap<JsString, Arc<Entry>>,
    next_generation: u64,
    disposed: bool,
    disposal: Option<Vec<Arc<Entry>>>,
}
#[derive(Clone)]
pub struct ModelInvocationProviderRegistry {
    state: Arc<Mutex<State>>,
    executor: Handle,
}
impl ModelInvocationProviderRegistry {
    pub fn new(executor: Handle) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                entries: BTreeMap::new(),
                next_generation: 1,
                disposed: false,
                disposal: None,
            })),
            executor,
        }
    }
    pub fn register(
        &self,
        provider: Arc<dyn ModelInvocationProvider>,
    ) -> Result<ProviderRegistration, ModuleError> {
        self.publish(provider, false)
            .map(|replacement| replacement.registration)
    }
    pub fn replace(
        &self,
        provider: Arc<dyn ModelInvocationProvider>,
    ) -> Result<ProviderReplacement, ModuleError> {
        self.publish(provider, true)
    }
    fn publish(
        &self,
        provider: Arc<dyn ModelInvocationProvider>,
        replace: bool,
    ) -> Result<ProviderReplacement, ModuleError> {
        let mut state = self.state.lock().unwrap();
        if state.disposed {
            return Err(coded(
                "registry_disposed",
                "Model invocation provider registry is disposed.",
            ));
        }
        let id = provider.provider_id().clone();
        if id.units().is_empty() || id.units().iter().all(|unit| matches!(*unit,
                0x0009..=0x000D | 0x0020 | 0x00A0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
            )) {
            return Err(coded(
                "invalid_provider",
                "Model invocation provider requires a non-empty providerId.",
            ));
        }
        if !replace && state.entries.contains_key(&id) {
            return Err(coded(
                "duplicate_provider",
                format!(
                    "Provider {} is already registered. Use replace() to publish a new generation.",
                    String::from_utf16_lossy(id.units())
                ),
            ));
        }
        let order = state
            .entries
            .get(&id)
            .map(|entry| entry.order)
            .unwrap_or(state.next_generation);
        let entry = Arc::new(Entry {
            provider,
            generation: state.next_generation,
            order,
            state: Mutex::default(),
            changed: watch::channel(()).0,
            completion: watch::channel(None).0,
            executor: self.executor.clone(),
        });
        state.next_generation += 1;
        let previous = state.entries.insert(id, entry.clone());
        if let Some(previous) = &previous {
            previous.retire();
        }
        Ok(ProviderReplacement {
            registration: ProviderRegistration {
                registry: self.state.clone(),
                entry,
            },
            retired: previous.map(|entry| ProviderRegistration {
                registry: self.state.clone(),
                entry,
            }),
        })
    }
    pub fn acquire(&self, id: &JsString) -> Result<ProviderLease, ModuleError> {
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(id).ok_or_else(|| missing(id))?.clone();
        entry.state.lock().unwrap().leases += 1;
        Ok(ProviderLease {
            entry,
            released: false,
        })
    }
    fn current(&self, id: &JsString) -> Result<Arc<Entry>, ModuleError> {
        self.state
            .lock()
            .unwrap()
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| missing(id))
    }
    pub async fn unregister(&self, id: &JsString) -> Result<bool, RuntimeError> {
        let entry = self.state.lock().unwrap().entries.remove(id);
        match entry {
            Some(entry) => {
                entry.retire();
                entry.disposed().await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
    pub async fn dispose(&self) -> RuntimeResult {
        let entries = {
            let mut state = self.state.lock().unwrap();
            if let Some(entries) = &state.disposal {
                entries.clone()
            } else {
                state.disposed = true;
                // JS Map replacement retains the original route insertion position.
                let mut entries: Vec<_> =
                    std::mem::take(&mut state.entries).into_values().collect();
                entries.sort_by_key(|entry| entry.order);
                for entry in &entries {
                    entry.retire();
                }
                state.disposal = Some(entries.clone());
                entries
            }
        };
        let mut failures = Vec::new();
        for entry in entries {
            if let Err(error) = entry.disposed().await {
                failures.push(error);
            }
        }
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.remove(0)),
            _ => Err(RuntimeError {
                message: "Failed to dispose model invocation providers.".into(),
                causes: failures,
            }),
        }
    }
}
fn missing(id: &JsString) -> ModuleError {
    coded(
        "provider_not_found",
        format!(
            "Provider {} does not exist.",
            String::from_utf16_lossy(id.units())
        ),
    )
}
impl RouterModelInvocation for ModelInvocationProviderRegistry {
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.current(provider)?
            .provider
            .get_capabilities(provider, model)
    }
    fn get_multimodal(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError> {
        self.current(provider)?
            .provider
            .get_multimodal(provider, model)
    }
    fn get_provider_generation(&self, provider: &JsString) -> JsValue {
        self.current(provider)
            .map(|entry| JsValue::Number(entry.generation as f64))
            .unwrap_or(JsValue::Undefined)
    }
    fn get_provider_protocol(&self, provider: &JsString) -> JsString {
        self.current(provider)
            .map(|entry| entry.provider.get_provider_protocol(provider))
            .unwrap_or_default()
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        // Acquire when stream() is called, before the body is first polled.
        let lease = request
            .get("provider")
            .and_then(JsValue::as_string)
            .ok_or_else(|| missing(&JsString::from("undefined")))
            .and_then(|id| self.acquire(id));
        Box::pin(async move {
            let lease = lease.map_err(|error| ModelStreamError::Request {
                code: error.code.unwrap_or_default(),
                message: error.message,
                details: None,
            })?;
            lease.provider().stream(request, options, sink).await
        })
    }
    fn supports_complete(&self) -> bool {
        // RouterModelInvocationPort.ts:40-48 always exposes complete, even
        // for an empty registry. Dispatch (and any unsupported implementation)
        // is resolved against the requested provider when complete is called.
        true
    }
    fn complete(&self, request: JsValue, abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        let lease = request
            .get("provider")
            .and_then(JsValue::as_string)
            .ok_or_else(|| missing(&JsString::from("undefined")))
            .and_then(|id| self.acquire(id));
        Box::pin(async move {
            let lease = lease.map_err(|error| {
                RouterInvokeError::request(error.code.unwrap_or_default(), error.message)
            })?;
            lease.provider().complete(request, abort).await
        })
    }
}

/// Native provider adapter with an immutable single-provider HTTP config.
/// Non-streaming complete() is not yet implemented by the existing HTTP layer;
/// its direct capability remains false; the registry dispatches calls to it
/// at invocation time, where the inherited complete() reports the missing port.
pub struct NativeModelInvocationProvider {
    id: JsString,
    config: JsValue,
    provider: JsValue,
    transport: Arc<dyn ModelHttpTransport>,
}
impl NativeModelInvocationProvider {
    pub fn new(provider: JsValue, transport: Arc<dyn ModelHttpTransport>) -> Self {
        let id = provider
            .get("id")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let mut providers = JsObject::new();
        providers.insert(id.clone(), provider.clone());
        let mut config = JsObject::new();
        config.insert("providers".into(), JsValue::Object(providers));
        Self {
            id,
            config: JsValue::Object(config),
            provider,
            transport,
        }
    }
    fn metadata(&self, model: &JsString, key: &str) -> Result<JsValue, ModuleError> {
        let model_config = self
            .provider
            .get("models")
            .and_then(JsValue::as_object)
            .and_then(|models| models.get_key(model))
            .ok_or_else(|| {
                coded(
                    "model_not_found",
                    format!(
                        "Model {} does not exist in provider {}.",
                        String::from_utf16_lossy(model.units()),
                        String::from_utf16_lossy(self.id.units())
                    ),
                )
            })?;
        Ok(model_config.get(key).cloned().unwrap_or(JsValue::Undefined))
    }
}
impl ModelInvocationProvider for NativeModelInvocationProvider {
    fn provider_id(&self) -> &JsString {
        &self.id
    }
}
impl RouterModelInvocation for NativeModelInvocationProvider {
    fn get_capabilities(&self, _: &JsString, model: &JsString) -> Result<JsValue, ModuleError> {
        self.metadata(model, "capabilities")
    }
    fn get_multimodal(&self, _: &JsString, model: &JsString) -> Result<JsValue, ModuleError> {
        self.metadata(model, "multimodal")
    }
    fn get_provider_protocol(&self, _: &JsString) -> JsString {
        self.provider
            .get("protocol")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default()
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(http::stream_model(
            self.transport.as_ref(),
            request,
            &self.config,
            options,
            sink,
        ))
    }
}

fn coded(code: &str, message: impl Into<JsString>) -> ModuleError {
    let mut error = ModuleError::new(message);
    error.code = Some(code.into());
    error
}
