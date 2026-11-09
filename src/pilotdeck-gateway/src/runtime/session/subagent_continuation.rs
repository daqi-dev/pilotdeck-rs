//! Durable continuable-subagent ownership and admission.
//!
//! The manager owns child handles after provider preparation. A tool call only
//! succeeds after the child descriptor and input admission are durable. Abort
//! owns admission, not already-enqueued work.

use crate::{
    runtime::{network::Cancellation, session::LiveSessionHandle},
    storage::TranscriptWriter,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot};

pub const SUBAGENT_DESCRIPTOR_VERSION: f64 = 1.0;
pub const SUBAGENT_CONTINUABLE_DESCRIPTOR_VERSION: f64 = 2.0;
pub const SUBAGENT_DESCRIPTOR_METADATA_KEY: &str = "pilotdeck.subagentDescriptor";

pub type ContinuationFuture<T> = Pin<Box<dyn Future<Output = Result<T, ContinuationError>> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationError {
    pub code: &'static str,
    pub message: String,
    /// Populated for multi-step dispose failures (TS `AggregateError.errors`).
    pub causes: Vec<ContinuationError>,
}

impl ContinuationError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            causes: Vec::new(),
        }
    }

    pub fn aggregate(
        code: &'static str,
        message: impl Into<String>,
        causes: Vec<Self>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            causes,
        }
    }
}

impl fmt::Display for ContinuationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ContinuationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneShotSubagentDescriptor {
    pub provider: JsString,
    pub definition_id: JsString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuableSubagentDescriptor {
    pub provider: JsString,
    pub definition_id: JsString,
    pub parent_session_id: JsString,
    pub label: JsString,
    pub agent_provider: JsString,
    pub agent_model: JsString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentDescriptor {
    OneShot(OneShotSubagentDescriptor),
    Continuable(ContinuableSubagentDescriptor),
}

impl SubagentDescriptor {
    pub fn to_js(&self) -> JsValue {
        let mut value = JsObject::new();
        match self {
            Self::OneShot(descriptor) => {
                value.insert(
                    "version".into(),
                    JsValue::Number(SUBAGENT_DESCRIPTOR_VERSION),
                );
                value.insert("mode".into(), "one-shot".into());
                value.insert(
                    "provider".into(),
                    JsValue::String(descriptor.provider.clone()),
                );
                value.insert(
                    "definitionId".into(),
                    JsValue::String(descriptor.definition_id.clone()),
                );
            }
            Self::Continuable(descriptor) => {
                value.insert(
                    "version".into(),
                    JsValue::Number(SUBAGENT_CONTINUABLE_DESCRIPTOR_VERSION),
                );
                value.insert("mode".into(), "continuable".into());
                value.insert(
                    "provider".into(),
                    JsValue::String(descriptor.provider.clone()),
                );
                value.insert(
                    "definitionId".into(),
                    JsValue::String(descriptor.definition_id.clone()),
                );
                value.insert(
                    "parentSessionId".into(),
                    JsValue::String(descriptor.parent_session_id.clone()),
                );
                value.insert("label".into(), JsValue::String(descriptor.label.clone()));
                value.insert(
                    "agentProvider".into(),
                    JsValue::String(descriptor.agent_provider.clone()),
                );
                value.insert(
                    "agentModel".into(),
                    JsValue::String(descriptor.agent_model.clone()),
                );
            }
        }
        JsValue::Object(value)
    }
}

pub fn snapshot_continuable_descriptor(
    provider: JsString,
    definition_id: JsString,
    parent_session_id: JsString,
    label: JsString,
    agent_provider: JsString,
    agent_model: JsString,
) -> Result<ContinuableSubagentDescriptor, ContinuationError> {
    Ok(ContinuableSubagentDescriptor {
        provider: require_non_empty(provider, "provider")?,
        definition_id: require_non_empty(definition_id, "definitionId")?,
        parent_session_id: require_non_empty(parent_session_id, "parentSessionId")?,
        label: require_non_empty(label, "label")?,
        agent_provider: require_non_empty(agent_provider, "agentProvider")?,
        agent_model: require_non_empty(agent_model, "agentModel")?,
    })
}

/// Strictly parse supported v1/v2 descriptors. Unknown versions are ignored,
/// while malformed supported versions fail the cold-resume transaction.
pub fn parse_subagent_descriptor(
    value: &JsValue,
) -> Result<Option<SubagentDescriptor>, ContinuationError> {
    let object = value.as_object().ok_or_else(|| {
        ContinuationError::new(
            "invalid_subagent_descriptor",
            "Persisted subagent descriptor must be an object.",
        )
    })?;
    let version = object
        .get("version")
        .and_then(JsValue::as_number)
        .ok_or_else(|| {
            ContinuationError::new(
                "invalid_subagent_descriptor",
                "Persisted subagent descriptor version must be a number.",
            )
        })?;
    let (mode, allowed): (&str, &[&str]) = if version == SUBAGENT_DESCRIPTOR_VERSION {
        ("one-shot", &["version", "mode", "provider", "definitionId"])
    } else if version == SUBAGENT_CONTINUABLE_DESCRIPTOR_VERSION {
        (
            "continuable",
            &[
                "version",
                "mode",
                "provider",
                "definitionId",
                "parentSessionId",
                "label",
                "agentProvider",
                "agentModel",
            ],
        )
    } else {
        return Ok(None);
    };
    if let Some((unknown, _)) = object
        .entries()
        .iter()
        .find(|(key, _)| !allowed.iter().any(|allowed| key.equals_str(allowed)))
    {
        return Err(ContinuationError::new(
            "invalid_subagent_descriptor",
            format!(
                "Persisted subagent descriptor has unknown field \"{}\".",
                text(unknown)
            ),
        ));
    }
    if object.get("mode") != Some(&JsValue::from(mode)) {
        return Err(ContinuationError::new(
            "invalid_subagent_descriptor",
            format!("Persisted subagent descriptor mode must be \"{mode}\"."),
        ));
    }
    let provider = descriptor_string(object, "provider")?;
    let definition_id = descriptor_string(object, "definitionId")?;
    if version == SUBAGENT_DESCRIPTOR_VERSION {
        return Ok(Some(SubagentDescriptor::OneShot(
            OneShotSubagentDescriptor {
                provider,
                definition_id,
            },
        )));
    }
    Ok(Some(SubagentDescriptor::Continuable(
        ContinuableSubagentDescriptor {
            provider,
            definition_id,
            parent_session_id: descriptor_string(object, "parentSessionId")?,
            label: descriptor_string(object, "label")?,
            agent_provider: descriptor_string(object, "agentProvider")?,
            agent_model: descriptor_string(object, "agentModel")?,
        },
    )))
}

/// The first descriptor is authoritative, including an unsupported version.
pub fn fold_subagent_descriptor(
    entries: &[JsValue],
) -> Result<Option<SubagentDescriptor>, ContinuationError> {
    let Some(entry) = entries.iter().find(|entry| {
        entry.get("type").and_then(JsValue::as_string)
            == Some(&JsString::from("subagent_descriptor"))
    }) else {
        return Ok(None);
    };
    let undefined = JsValue::Undefined;
    let descriptor = entry.get("descriptor").unwrap_or(&undefined);
    parse_subagent_descriptor(descriptor)
}

fn descriptor_string(
    object: &JsObject,
    field: &'static str,
) -> Result<JsString, ContinuationError> {
    let value = object
        .get(field)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    require_non_empty(value, field)
}

fn require_non_empty(value: JsString, field: &'static str) -> Result<JsString, ContinuationError> {
    if text(&value).trim().is_empty() {
        return Err(ContinuationError::new(
            "invalid_subagent_descriptor",
            format!("Subagent descriptor {field} must be a non-empty string."),
        ));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContinuationParent {
    pub session_id: JsString,
    /// Object identity assigned by the owning agent directory. Session id alone
    /// is not sufficient authorization after replacement.
    pub identity: u64,
}

#[derive(Clone, Debug)]
pub struct ContinuationAdmission {
    pub child_session_id: JsString,
    pub item_id: JsString,
    pub turn_id: JsString,
}

#[derive(Clone)]
pub struct ChildAdmissionRequest {
    pub message: JsString,
    pub item_id: JsString,
    pub turn_id: JsString,
    pub cancellation: Option<Cancellation>,
    /// The child must call this immediately before durable enqueue.
    pub authorize_admission: Arc<dyn Fn() -> Result<(), ContinuationError> + Send + Sync>,
}

struct AdmissionRequestInput {
    child_session_id: JsString,
    parent: ContinuationParent,
    expected_child: ContinuationParent,
    message: JsString,
    item_id: JsString,
    turn_id: JsString,
    cancellation: Option<Cancellation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationTerminalState {
    Completed,
    Aborted,
    Failed,
    Unknown,
}

impl ContinuationTerminalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContinuationTerminal {
    pub state: ContinuationTerminalState,
    pub detail: Option<JsString>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuationChildSnapshot {
    pub active: bool,
    pub in_flight: usize,
    pub pending_turns: usize,
}

pub trait ContinuationChild: Send + Sync {
    fn reference(&self) -> ContinuationParent;
    fn record_descriptor(
        &self,
        turn_id: JsString,
        descriptor: ContinuableSubagentDescriptor,
    ) -> ContinuationFuture<()>;
    fn followup(&self, request: ChildAdmissionRequest) -> ContinuationFuture<()>;
    fn snapshot(&self) -> ContinuationChildSnapshot;
    fn wait_until_idle(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    fn terminal(&self) -> ContinuationTerminal;
    fn dispose(&self, reason: JsString) -> ContinuationFuture<()>;
}

#[derive(Clone, Debug)]
pub struct ContinuationPrepareRequest {
    pub child_session_id: JsString,
    pub parent_session_id: JsString,
    pub definition_id: JsString,
}

#[derive(Clone, Debug)]
pub struct PreparedContinuation {
    pub provider: JsString,
    pub generation: u64,
    pub spec: JsValue,
}

pub trait ContinuationProviderRegistry: Send + Sync {
    fn prepare(
        &self,
        provider: JsString,
        request: ContinuationPrepareRequest,
        cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<PreparedContinuation>;
}

/// One named provider generation. The provider returns detached creation data;
/// live child ownership starts at [`SubagentContinuationManager`].
pub trait NamedContinuationProvider: Send + Sync {
    fn supports_continuation(&self) -> bool {
        true
    }
    fn prepare_continuable(
        &self,
        request: ContinuationPrepareRequest,
        cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<JsValue>;
    fn dispose(&self) -> ContinuationFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRegistryState {
    Active,
    Draining,
    Disposed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderEntryState {
    Active,
    Draining,
    Disposed,
}

struct ProviderEntryStatus {
    state: ProviderEntryState,
    in_flight: usize,
    dispose_started: bool,
    dispose_result: Option<Result<(), ContinuationError>>,
}

struct ProviderEntry {
    name: String,
    generation: u64,
    provider: Arc<dyn NamedContinuationProvider>,
    status: Mutex<ProviderEntryStatus>,
    drained: Notify,
    disposed: Notify,
}

impl ProviderEntry {
    fn stop(&self) {
        let mut status = self.status.lock().unwrap();
        if status.state == ProviderEntryState::Active {
            status.state = ProviderEntryState::Draining;
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<ProviderLease, ContinuationError> {
        let mut status = self.status.lock().unwrap();
        if status.state != ProviderEntryState::Active {
            return Err(ContinuationError::new(
                "subagent_provider_inactive",
                format!("Subagent provider is not active: {}", self.name),
            ));
        }
        status.in_flight += 1;
        Ok(ProviderLease {
            entry: self.clone(),
            active: true,
        })
    }
}

/// An active-run lease. Replacement does not invalidate accepted work; it only
/// prevents new leases on the old generation and waits for these guards.
pub struct ProviderLease {
    entry: Arc<ProviderEntry>,
    active: bool,
}

impl ProviderLease {
    pub fn name(&self) -> &str {
        &self.entry.name
    }
    pub fn generation(&self) -> u64 {
        self.entry.generation
    }
    pub fn release(mut self) {
        self.release_inner();
    }
    fn release_inner(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let drained = {
            let mut status = self.entry.status.lock().unwrap();
            status.in_flight -= 1;
            status.in_flight == 0
        };
        if drained {
            self.entry.drained.notify_waiters();
        }
    }
}

impl Drop for ProviderLease {
    fn drop(&mut self) {
        self.release_inner();
    }
}

#[derive(Clone)]
pub struct ProviderRegistration(Arc<ProviderEntry>);

impl ProviderRegistration {
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
    pub fn active(&self) -> bool {
        self.0.status.lock().unwrap().state == ProviderEntryState::Active
    }
    pub fn in_flight(&self) -> usize {
        self.0.status.lock().unwrap().in_flight
    }
    pub fn stop(&self) {
        self.0.stop();
    }
    pub fn lease(&self) -> Result<ProviderLease, ContinuationError> {
        self.0.acquire()
    }
    pub async fn when_drained(&self) {
        loop {
            let notified = self.0.drained.notified();
            if self.0.status.lock().unwrap().in_flight == 0 {
                return;
            }
            notified.await;
        }
    }
    pub async fn dispose(&self) -> Result<(), ContinuationError> {
        dispose_provider_entry(self.0.clone()).await
    }
}

pub struct ProviderReplacement {
    pub registration: ProviderRegistration,
    pub previous_disposed: ContinuationFuture<()>,
}

struct ProviderRegistryInner {
    state: ProviderRegistryState,
    next_generation: u64,
    current: HashMap<String, Arc<ProviderEntry>>,
    order: Vec<(String, u64)>,
    dispose_result: Option<Result<(), ContinuationError>>,
}

#[derive(Clone)]
pub struct NamedContinuationProviderRegistry {
    inner: Arc<Mutex<ProviderRegistryInner>>,
    dispose_lock: Arc<AsyncMutex<()>>,
}

impl Default for NamedContinuationProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl NamedContinuationProviderRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ProviderRegistryInner {
                state: ProviderRegistryState::Active,
                next_generation: 0,
                current: HashMap::new(),
                order: Vec::new(),
                dispose_result: None,
            })),
            dispose_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    pub fn state(&self) -> ProviderRegistryState {
        self.inner.lock().unwrap().state
    }

    pub fn size(&self) -> usize {
        self.inner.lock().unwrap().current.len()
    }

    pub fn register(
        &self,
        name: &str,
        provider: Arc<dyn NamedContinuationProvider>,
    ) -> Result<ProviderRegistration, ContinuationError> {
        let normalized = normalize_provider_name(name)?;
        let mut inner = self.inner.lock().unwrap();
        assert_registry_active(inner.state, "register a subagent provider")?;
        if inner.current.contains_key(&normalized) {
            return Err(ContinuationError::new(
                "duplicate_subagent_provider",
                format!("Subagent provider is already registered: {normalized}"),
            ));
        }
        let entry = create_provider_entry(&mut inner, normalized.clone(), provider);
        inner.current.insert(normalized, entry.clone());
        Ok(ProviderRegistration(entry))
    }

    pub fn replace(
        &self,
        name: &str,
        provider: Arc<dyn NamedContinuationProvider>,
    ) -> Result<ProviderReplacement, ContinuationError> {
        let normalized = normalize_provider_name(name)?;
        let (entry, previous) = {
            let mut inner = self.inner.lock().unwrap();
            assert_registry_active(inner.state, "replace a subagent provider")?;
            let previous = inner.current.get(&normalized).cloned().ok_or_else(|| {
                ContinuationError::new(
                    "unknown_subagent_provider",
                    format!("Subagent provider is not registered: {normalized}"),
                )
            })?;
            let entry = create_provider_entry(&mut inner, normalized.clone(), provider);
            // Publish the new generation before stopping the old one.
            inner.current.insert(normalized, entry.clone());
            (entry, previous)
        };
        previous.stop();
        let disposal = tokio::spawn(dispose_provider_entry(previous));
        Ok(ProviderReplacement {
            registration: ProviderRegistration(entry),
            previous_disposed: Box::pin(async move {
                disposal.await.map_err(|error| {
                    ContinuationError::new(
                        "subagent_provider_dispose_failed",
                        format!("Subagent provider disposal task failed: {error}"),
                    )
                })?
            }),
        })
    }

    pub async fn unregister(&self, name: &str) -> Result<(), ContinuationError> {
        let normalized = normalize_provider_name(name)?;
        let entry = {
            let mut inner = self.inner.lock().unwrap();
            assert_registry_active(inner.state, "unregister a subagent provider")?;
            inner.current.remove(&normalized).ok_or_else(|| {
                ContinuationError::new(
                    "unknown_subagent_provider",
                    format!("Subagent provider is not registered: {normalized}"),
                )
            })?
        };
        entry.stop();
        dispose_provider_entry(entry).await
    }

    pub fn lease(&self, name: &str) -> Result<ProviderLease, ContinuationError> {
        let normalized = normalize_provider_name(name)?;
        let entry = {
            let inner = self.inner.lock().unwrap();
            assert_registry_active(inner.state, "start a subagent run")?;
            inner.current.get(&normalized).cloned().ok_or_else(|| {
                ContinuationError::new(
                    "unavailable_subagent_provider",
                    format!("Subagent provider is not available: {normalized}"),
                )
            })?
        };
        entry.acquire()
    }

    pub fn lease_generation(
        &self,
        name: &str,
        generation: u64,
    ) -> Result<ProviderLease, ContinuationError> {
        let lease = self.lease(name)?;
        if lease.generation() != generation {
            drop(lease);
            return Err(ContinuationError::new(
                "stale_subagent_provider_generation",
                format!("Subagent provider generation is stale: {name}@{generation}"),
            ));
        }
        Ok(lease)
    }

    pub async fn prepare_continuable(
        &self,
        provider: JsString,
        request: ContinuationPrepareRequest,
        cancellation: Option<Cancellation>,
    ) -> Result<PreparedContinuation, ContinuationError> {
        let name = text(&provider);
        let normalized = normalize_provider_name(&name)?;
        let (entry, lease) = {
            let inner = self.inner.lock().unwrap();
            assert_registry_active(inner.state, "prepare a continuable subagent")?;
            let entry = inner.current.get(&normalized).cloned().ok_or_else(|| {
                ContinuationError::new(
                    "unavailable_subagent_provider",
                    format!("Subagent provider is not available: {name}"),
                )
            })?;
            let lease = entry.acquire()?;
            (entry, lease)
        };
        if !entry.provider.supports_continuation() {
            return Err(ContinuationError::new(
                "unsupported_subagent_provider",
                format!(
                    "Subagent provider \"{}\" does not support continuable children.",
                    entry.name
                ),
            ));
        }
        if cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            return Err(ContinuationError::new(
                "subagent_admission_aborted",
                "Continuable subagent preparation aborted.",
            ));
        }
        let spec = entry
            .provider
            .prepare_continuable(request, cancellation.clone())
            .await?;
        if cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            return Err(ContinuationError::new(
                "subagent_admission_aborted",
                "Continuable subagent preparation aborted.",
            ));
        }
        let spec = snapshot_continuation_spec(&spec)?;
        let prepared = PreparedContinuation {
            provider: entry.name.as_str().into(),
            generation: lease.generation(),
            spec,
        };
        drop(lease);
        Ok(prepared)
    }

    pub async fn dispose(&self) -> Result<(), ContinuationError> {
        let _dispose = self.dispose_lock.lock().await;
        let registrations = {
            let mut inner = self.inner.lock().unwrap();
            if inner.state == ProviderRegistryState::Disposed {
                return inner.dispose_result.clone().unwrap_or(Ok(()));
            }
            inner.state = ProviderRegistryState::Draining;
            let mut registrations = Vec::new();
            for (name, generation) in inner.order.iter().rev() {
                if let Some(entry) = inner.current.get(name)
                    && entry.generation == *generation
                {
                    entry.stop();
                    registrations.push(entry.clone());
                }
            }
            registrations
        };
        let mut failures = Vec::new();
        for entry in registrations {
            if let Err(error) = dispose_provider_entry(entry).await {
                failures.push(error);
            }
        }
        let result = continuation_provider_registry_dispose_failures(failures);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.current.clear();
            inner.state = ProviderRegistryState::Disposed;
            inner.dispose_result = Some(result.clone());
        }
        result
    }
}

impl ContinuationProviderRegistry for NamedContinuationProviderRegistry {
    fn prepare(
        &self,
        provider: JsString,
        request: ContinuationPrepareRequest,
        cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<PreparedContinuation> {
        let registry = self.clone();
        Box::pin(async move {
            registry
                .prepare_continuable(provider, request, cancellation)
                .await
        })
    }
}

fn create_provider_entry(
    inner: &mut ProviderRegistryInner,
    name: String,
    provider: Arc<dyn NamedContinuationProvider>,
) -> Arc<ProviderEntry> {
    inner.next_generation += 1;
    let generation = inner.next_generation;
    inner.order.push((name.clone(), generation));
    Arc::new(ProviderEntry {
        name,
        generation,
        provider,
        status: Mutex::new(ProviderEntryStatus {
            state: ProviderEntryState::Active,
            in_flight: 0,
            dispose_started: false,
            dispose_result: None,
        }),
        drained: Notify::new(),
        disposed: Notify::new(),
    })
}

async fn dispose_provider_entry(entry: Arc<ProviderEntry>) -> Result<(), ContinuationError> {
    loop {
        let notified = entry.disposed.notified();
        let first = {
            let mut status = entry.status.lock().unwrap();
            if status.state == ProviderEntryState::Disposed {
                return status.dispose_result.clone().unwrap_or(Ok(()));
            }
            status.state = ProviderEntryState::Draining;
            if status.dispose_started {
                false
            } else {
                status.dispose_started = true;
                true
            }
        };
        if first {
            break;
        }
        notified.await;
    }
    loop {
        let notified = entry.drained.notified();
        if entry.status.lock().unwrap().in_flight == 0 {
            break;
        }
        notified.await;
    }
    let result = entry.provider.dispose().await;
    {
        let mut status = entry.status.lock().unwrap();
        status.state = ProviderEntryState::Disposed;
        status.dispose_result = Some(result.clone());
    }
    entry.disposed.notify_waiters();
    result
}

fn normalize_provider_name(name: &str) -> Result<String, ContinuationError> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return Err(ContinuationError::new(
            "invalid_subagent_provider",
            "Subagent provider name must not be empty.",
        ));
    }
    Ok(normalized.to_owned())
}

fn assert_registry_active(
    state: ProviderRegistryState,
    action: &str,
) -> Result<(), ContinuationError> {
    if state != ProviderRegistryState::Active {
        return Err(ContinuationError::new(
            "subagent_provider_registry_inactive",
            format!("Cannot {action}; subagent provider registry is inactive."),
        ));
    }
    Ok(())
}

fn snapshot_continuation_spec(value: &JsValue) -> Result<JsValue, ContinuationError> {
    let object = value.as_object().ok_or_else(|| {
        ContinuationError::new(
            "invalid_subagent_provider_spec",
            "Continuable subagent creation spec must be an object.",
        )
    })?;
    if let Some((unknown, _)) = object
        .entries()
        .iter()
        .find(|(key, _)| !key.equals_str("seedEntries"))
    {
        return Err(ContinuationError::new(
            "invalid_subagent_provider_spec",
            format!(
                "Continuable subagent creation spec has unknown field \"{}\".",
                text(unknown)
            ),
        ));
    }
    if let Some(seed_entries) = object.get("seedEntries")
        && !seed_entries.is_undefined()
        && !matches!(seed_entries, JsValue::Array(_))
    {
        return Err(ContinuationError::new(
            "invalid_subagent_provider_spec",
            "Continuable subagent creation spec seedEntries must be an array.",
        ));
    }
    validate_json_value(value, "continuable subagent creation spec")?;
    let serialized = stringify_json(value)
        .map_err(|error| {
            ContinuationError::new("invalid_subagent_provider_spec", error.to_string())
        })?
        .ok_or_else(|| {
            ContinuationError::new(
                "invalid_subagent_provider_spec",
                "Continuable subagent creation spec must be JSON serializable.",
            )
        })?;
    parse_json(&serialized).map_err(|error| {
        ContinuationError::new("invalid_subagent_provider_spec", error.to_string())
    })
}

fn validate_json_value(value: &JsValue, path: &str) -> Result<(), ContinuationError> {
    match value {
        JsValue::Null | JsValue::Bool(_) | JsValue::String(_) => Ok(()),
        JsValue::Number(number) if number.is_finite() => Ok(()),
        JsValue::Number(_) => Err(ContinuationError::new(
            "invalid_subagent_provider_spec",
            format!("{path} contains a non-finite number."),
        )),
        JsValue::Undefined => Err(ContinuationError::new(
            "invalid_subagent_provider_spec",
            format!("{path} contains a non-JSON value."),
        )),
        JsValue::Array(values) => values
            .iter()
            .enumerate()
            .try_for_each(|(index, value)| validate_json_value(value, &format!("{path}[{index}]"))),
        JsValue::Object(object) => {
            if object.native_tag().is_some() || object.prototype().is_some() {
                return Err(ContinuationError::new(
                    "invalid_subagent_provider_spec",
                    format!("{path} must contain only plain JSON objects."),
                ));
            }
            object.entries().iter().try_for_each(|(key, value)| {
                validate_json_value(value, &format!("{path}.{}", text(key)))
            })
        }
    }
}

#[derive(Clone)]
pub struct ContinuationCreateRequest {
    pub child_session_id: JsString,
    pub parent: ContinuationParent,
    pub descriptor: ContinuableSubagentDescriptor,
    pub prepared: PreparedContinuation,
    pub cancellation: Option<Cancellation>,
}

#[derive(Clone)]
pub struct ContinuationResumeRequest {
    pub child_session_id: JsString,
    pub parent: ContinuationParent,
    pub descriptor: ContinuableSubagentDescriptor,
    pub cancellation: Option<Cancellation>,
}

pub trait ContinuationHost: Send + Sync {
    fn create(
        &self,
        request: ContinuationCreateRequest,
    ) -> ContinuationFuture<Arc<dyn ContinuationChild>>;
    fn inspect(
        &self,
        child_session_id: JsString,
        parent: ContinuationParent,
        cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<Vec<JsValue>>;
    fn resume(
        &self,
        request: ContinuationResumeRequest,
    ) -> ContinuationFuture<Arc<dyn ContinuationChild>>;
}

pub trait ContinuationParentDirectory: Send + Sync {
    fn is_live(&self, parent: &ContinuationParent) -> bool;
    fn notify(&self, parent: ContinuationParent, message: JsString) -> ContinuationFuture<()>;
}

#[derive(Clone)]
pub struct StartContinuationRequest {
    pub provider: JsString,
    pub label: JsString,
    pub child_session_id: Option<JsString>,
    pub parent: ContinuationParent,
    pub definition_id: JsString,
    pub agent_provider: JsString,
    pub agent_model: JsString,
    pub prompt: JsString,
    pub cancellation: Option<Cancellation>,
}

#[derive(Clone)]
pub struct FollowupContinuationRequest {
    pub child_session_id: JsString,
    pub parent: ContinuationParent,
    pub message: JsString,
    pub cancellation: Option<Cancellation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationManagerState {
    Active,
    Draining,
    Disposed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationResidencyState {
    Running,
    Waiting,
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationActivationSnapshot {
    pub child_session_id: JsString,
    pub parent_session_id: JsString,
    pub provider: JsString,
    pub provider_generation: Option<u64>,
    pub draining: bool,
}

#[derive(Clone)]
struct Activation {
    child_session_id: JsString,
    parent: ContinuationParent,
    provider: JsString,
    provider_generation: Option<u64>,
    child: Arc<dyn ContinuationChild>,
    owned_children: HashSet<JsString>,
    parent_activation: Option<JsString>,
    announced: bool,
    draining: bool,
}

struct State {
    lifecycle: ContinuationManagerState,
    activations: HashMap<JsString, Activation>,
}

struct ManagerInner {
    state: Mutex<State>,
    locks: Mutex<HashMap<JsString, Arc<AsyncMutex<()>>>>,
    providers: Arc<dyn ContinuationProviderRegistry>,
    host: Arc<dyn ContinuationHost>,
    parents: Arc<dyn ContinuationParentDirectory>,
    transactions: AtomicUsize,
    transactions_changed: Notify,
    residency_changed: Notify,
}

#[derive(Clone)]
pub struct SubagentContinuationManager(Arc<ManagerInner>);

impl SubagentContinuationManager {
    pub fn new(
        providers: Arc<dyn ContinuationProviderRegistry>,
        host: Arc<dyn ContinuationHost>,
        parents: Arc<dyn ContinuationParentDirectory>,
    ) -> Self {
        Self(Arc::new(ManagerInner {
            state: Mutex::new(State {
                lifecycle: ContinuationManagerState::Active,
                activations: HashMap::new(),
            }),
            locks: Mutex::new(HashMap::new()),
            providers,
            host,
            parents,
            transactions: AtomicUsize::new(0),
            transactions_changed: Notify::new(),
            residency_changed: Notify::new(),
        }))
    }

    pub fn state(&self) -> ContinuationManagerState {
        self.0.state.lock().unwrap().lifecycle
    }

    pub fn size(&self) -> usize {
        self.0.state.lock().unwrap().activations.len()
    }

    pub fn has(&self, child_session_id: &JsString) -> bool {
        self.0
            .state
            .lock()
            .unwrap()
            .activations
            .contains_key(child_session_id)
    }

    pub fn snapshot(&self) -> Vec<ContinuationActivationSnapshot> {
        self.0
            .state
            .lock()
            .unwrap()
            .activations
            .values()
            .map(|activation| ContinuationActivationSnapshot {
                child_session_id: activation.child_session_id.clone(),
                parent_session_id: activation.parent.session_id.clone(),
                provider: activation.provider.clone(),
                provider_generation: activation.provider_generation,
                draining: activation.draining,
            })
            .collect()
    }

    pub fn activation_state(
        &self,
        child_session_id: &JsString,
    ) -> Option<ContinuationResidencyState> {
        let state = self.0.state.lock().unwrap();
        let activation = state.activations.get(child_session_id)?;
        Some(residency(activation))
    }

    pub async fn start(
        &self,
        request: StartContinuationRequest,
    ) -> Result<ContinuationAdmission, ContinuationError> {
        let _transaction = self.begin_transaction("start a continuable subagent")?;
        let child_session_id = request.child_session_id.clone().unwrap_or_else(next_id);
        require_id(&child_session_id, "childSessionId")?;
        self.authorize_live_parent(&request.parent)?;
        if child_session_id == request.parent.session_id {
            return Err(ContinuationError::new(
                "invalid_subagent_id",
                "A continuable subagent cannot use its parent session id.",
            ));
        }
        let provider = require_id(&request.provider, "provider")?;
        let descriptor = snapshot_continuable_descriptor(
            provider,
            request.definition_id.clone(),
            request.parent.session_id.clone(),
            request.label.clone(),
            request.agent_provider.clone(),
            request.agent_model.clone(),
        )?;
        let item_id = next_id();
        let turn_id = next_id();
        let operation_lock = self.operation_lock(&child_session_id);
        let _operation = operation_lock.lock().await;
        self.ensure_active("start a continuable subagent")?;
        self.authorize_live_parent(&request.parent)?;
        self.throw_if_cancelled(request.cancellation.as_ref(), "continuable subagent start")?;
        if self.has(&child_session_id) {
            return Err(ContinuationError::new(
                "duplicate_subagent",
                format!(
                    "Continuable subagent already exists: {}",
                    text(&child_session_id)
                ),
            ));
        }
        let prepared = self
            .0
            .providers
            .prepare(
                request.provider.clone(),
                ContinuationPrepareRequest {
                    child_session_id: child_session_id.clone(),
                    parent_session_id: request.parent.session_id.clone(),
                    definition_id: request.definition_id.clone(),
                },
                request.cancellation.clone(),
            )
            .await?;
        self.ensure_active("materialize a continuable subagent")?;
        self.authorize_live_parent(&request.parent)?;
        self.throw_if_cancelled(request.cancellation.as_ref(), "continuable subagent start")?;
        let child = self
            .0
            .host
            .create(ContinuationCreateRequest {
                child_session_id: child_session_id.clone(),
                parent: request.parent.clone(),
                descriptor: descriptor.clone(),
                prepared: prepared.clone(),
                cancellation: request.cancellation.clone(),
            })
            .await?;
        if let Err(error) = self
            .ensure_active("publish a continuable subagent")
            .and_then(|_| self.authorize_live_parent(&request.parent).map(|_| ()))
            .and_then(|_| {
                self.throw_if_cancelled(request.cancellation.as_ref(), "continuable subagent start")
            })
            .and_then(|_| {
                self.insert_activation(
                    child_session_id.clone(),
                    request.parent.clone(),
                    prepared.provider.clone(),
                    Some(prepared.generation),
                    child.clone(),
                )
            })
        {
            let _ = child
                .dispose("continuable_subagent_admission_rollback".into())
                .await;
            return Err(error);
        }
        let admission_result = async {
            child.record_descriptor(turn_id.clone(), descriptor).await?;
            self.authorize_activation_parent(
                &child_session_id,
                &request.parent,
                child.reference(),
            )?;
            child
                .followup(self.admission_request(AdmissionRequestInput {
                    child_session_id: child_session_id.clone(),
                    parent: request.parent.clone(),
                    expected_child: child.reference(),
                    message: request.prompt,
                    item_id: item_id.clone(),
                    turn_id: turn_id.clone(),
                    cancellation: request.cancellation,
                }))
                .await?;
            Ok::<_, ContinuationError>(())
        }
        .await;
        if let Err(error) = admission_result {
            let rollback = self
                .finish_one_locked(
                    &child_session_id,
                    "continuable_subagent_admission_rollback".into(),
                )
                .await;
            return Err(rollback.err().unwrap_or(error));
        }
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(activation) = state.activations.get_mut(&child_session_id) {
                activation.announced = true;
            }
        }
        self.watch_settlement(child_session_id.clone());
        Ok(ContinuationAdmission {
            child_session_id,
            item_id,
            turn_id,
        })
    }

    pub async fn followup(
        &self,
        request: FollowupContinuationRequest,
    ) -> Result<ContinuationAdmission, ContinuationError> {
        let _transaction = self.begin_transaction("follow up a continuable subagent")?;
        require_id(&request.child_session_id, "childSessionId")?;
        self.authorize_live_parent(&request.parent)?;
        let operation_lock = self.operation_lock(&request.child_session_id);
        let _operation = operation_lock.lock().await;
        self.ensure_active("follow up a continuable subagent")?;
        self.authorize_live_parent(&request.parent)?;
        self.throw_if_cancelled(
            request.cancellation.as_ref(),
            "continuable subagent follow-up",
        )?;
        let child = self
            .activation_child(&request.child_session_id)
            .map(|activation| {
                self.authorize_activation_parent(
                    &request.child_session_id,
                    &request.parent,
                    activation.reference(),
                )?;
                Ok(activation)
            })
            .transpose()?;
        match child {
            Some(child) => self.admit(child, request).await,
            None => self.cold_resume(request).await,
        }
    }

    pub async fn dispose_child(
        &self,
        child_session_id: &JsString,
        reason: JsString,
    ) -> Result<bool, ContinuationError> {
        self.ensure_active("dispose a continuable subagent")?;
        require_id(child_session_id, "childSessionId")?;
        if !self.has(child_session_id) {
            return Ok(false);
        }
        self.dispose_tree(child_session_id.clone(), reason).await?;
        Ok(true)
    }

    pub async fn drain_descendants(
        &self,
        parent: &ContinuationParent,
        reason: JsString,
    ) -> Result<(), ContinuationError> {
        let roots = {
            let state = self.0.state.lock().unwrap();
            state
                .activations
                .values()
                .filter(|activation| {
                    activation.parent == *parent
                        || ancestor_matches(&state.activations, activation, parent)
                })
                .filter(|activation| {
                    activation
                        .parent_activation
                        .as_ref()
                        .is_none_or(|parent_id| {
                            !state.activations.get(parent_id).is_some_and(|candidate| {
                                candidate.parent == *parent
                                    || ancestor_matches(&state.activations, candidate, parent)
                            })
                        })
                })
                .map(|activation| activation.child_session_id.clone())
                .collect::<Vec<_>>()
        };
        let mut errors = Vec::new();
        for root in roots {
            if let Err(error) = self.dispose_tree(root, reason.clone()).await {
                errors.push(error.message);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ContinuationError::new(
                "subagent_dispose_failed",
                format!(
                    "Failed to dispose continuable descendants: {}",
                    errors.join("; ")
                ),
            ))
        }
    }

    pub async fn dispose(&self, reason: JsString) -> Result<(), ContinuationError> {
        {
            let mut state = self.0.state.lock().unwrap();
            if state.lifecycle == ContinuationManagerState::Disposed {
                return Ok(());
            }
            state.lifecycle = ContinuationManagerState::Draining;
        }
        loop {
            let notified = self.0.transactions_changed.notified();
            if self.0.transactions.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
        let roots = {
            let state = self.0.state.lock().unwrap();
            state
                .activations
                .values()
                .filter(|activation| activation.parent_activation.is_none())
                .map(|activation| activation.child_session_id.clone())
                .collect::<Vec<_>>()
        };
        let mut failures = Vec::new();
        for root in roots.into_iter().rev() {
            if let Err(error) = self.dispose_tree(root, reason.clone()).await {
                failures.push(error);
            }
        }
        self.0.state.lock().unwrap().lifecycle = ContinuationManagerState::Disposed;
        continuation_dispose_failures(
            failures,
            "subagent_dispose_failed",
            |count| format!("Failed to dispose continuable subagents: {count} failures."),
        )
    }

    async fn admit(
        &self,
        child: Arc<dyn ContinuationChild>,
        request: FollowupContinuationRequest,
    ) -> Result<ContinuationAdmission, ContinuationError> {
        let item_id = next_id();
        let turn_id = next_id();
        child
            .followup(self.admission_request(AdmissionRequestInput {
                child_session_id: request.child_session_id.clone(),
                parent: request.parent,
                expected_child: child.reference(),
                message: request.message,
                item_id: item_id.clone(),
                turn_id: turn_id.clone(),
                cancellation: request.cancellation,
            }))
            .await?;
        self.0.residency_changed.notify_waiters();
        Ok(ContinuationAdmission {
            child_session_id: request.child_session_id,
            item_id,
            turn_id,
        })
    }

    async fn cold_resume(
        &self,
        request: FollowupContinuationRequest,
    ) -> Result<ContinuationAdmission, ContinuationError> {
        let entries = self
            .0
            .host
            .inspect(
                request.child_session_id.clone(),
                request.parent.clone(),
                request.cancellation.clone(),
            )
            .await
            .map_err(|error| {
                ContinuationError::new(
                    "subagent_not_resumable",
                    format!(
                        "Continuable subagent is not resumable: {} ({})",
                        text(&request.child_session_id),
                        error.message
                    ),
                )
            })?;
        self.ensure_active("cold resume a continuable subagent")?;
        self.throw_if_cancelled(
            request.cancellation.as_ref(),
            "continuable subagent cold resume",
        )?;
        self.authorize_live_parent(&request.parent)?;
        let owned_entries = entries
            .into_iter()
            .filter(|entry| {
                entry.get("sessionId").and_then(JsValue::as_string)
                    == Some(&request.child_session_id)
            })
            .collect::<Vec<_>>();
        let descriptor = match fold_subagent_descriptor(&owned_entries).map_err(|error| {
            ContinuationError::new(
                "invalid_subagent_state",
                format!(
                    "Continuable subagent has invalid durable state: {} ({})",
                    text(&request.child_session_id),
                    error.message
                ),
            )
        })? {
            Some(SubagentDescriptor::Continuable(descriptor)) => descriptor,
            _ => {
                return Err(ContinuationError::new(
                    "subagent_not_resumable",
                    format!(
                        "Continuable subagent is not resumable: {}",
                        text(&request.child_session_id)
                    ),
                ));
            }
        };
        if descriptor.parent_session_id != request.parent.session_id {
            return Err(ContinuationError::new(
                "wrong_subagent_parent",
                format!(
                    "Continuable subagent {} belongs to another parent session.",
                    text(&request.child_session_id)
                ),
            ));
        }
        validate_persisted_subagent_definition_id(&descriptor.definition_id)?;
        let child = self
            .0
            .host
            .resume(ContinuationResumeRequest {
                child_session_id: request.child_session_id.clone(),
                parent: request.parent.clone(),
                descriptor: descriptor.clone(),
                cancellation: request.cancellation.clone(),
            })
            .await?;
        self.ensure_active("publish a cold-resumed continuable subagent")?;
        self.throw_if_cancelled(
            request.cancellation.as_ref(),
            "continuable subagent cold resume",
        )?;
        self.authorize_live_parent(&request.parent)?;
        self.insert_activation(
            request.child_session_id.clone(),
            request.parent.clone(),
            descriptor.provider,
            None,
            child.clone(),
        )?;
        let result = match self.admit(child, request.clone()).await {
            Ok(result) => result,
            Err(error) => {
                let rollback = self
                    .finish_one_locked(
                        &request.child_session_id,
                        "continuable_subagent_admission_rollback".into(),
                    )
                    .await;
                return Err(rollback.err().unwrap_or(error));
            }
        };
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(activation) = state.activations.get_mut(&request.child_session_id) {
                activation.announced = true;
            }
        }
        self.watch_settlement(request.child_session_id);
        Ok(result)
    }

    fn admission_request(&self, input: AdmissionRequestInput) -> ChildAdmissionRequest {
        let manager = self.clone();
        ChildAdmissionRequest {
            message: input.message,
            item_id: input.item_id,
            turn_id: input.turn_id,
            cancellation: input.cancellation,
            authorize_admission: Arc::new(move || {
                manager.authorize_activation_parent(
                    &input.child_session_id,
                    &input.parent,
                    input.expected_child.clone(),
                )
            }),
        }
    }

    fn insert_activation(
        &self,
        child_session_id: JsString,
        parent: ContinuationParent,
        provider: JsString,
        provider_generation: Option<u64>,
        child: Arc<dyn ContinuationChild>,
    ) -> Result<(), ContinuationError> {
        let mut state = self.0.state.lock().unwrap();
        if state.activations.contains_key(&child_session_id) {
            return Err(ContinuationError::new(
                "duplicate_subagent",
                format!(
                    "Continuable subagent already exists: {}",
                    text(&child_session_id)
                ),
            ));
        }
        let parent_activation = state
            .activations
            .iter()
            .find(|(_, candidate)| candidate.child.reference() == parent)
            .map(|(id, _)| id.clone());
        if let Some(parent_id) = &parent_activation {
            let parent_entry = state.activations.get_mut(parent_id).unwrap();
            if parent_entry.draining {
                return Err(ContinuationError::new(
                    "subagent_parent_disposing",
                    format!(
                        "Continuable parent is already disposing: {}",
                        text(&parent.session_id)
                    ),
                ));
            }
            parent_entry.owned_children.insert(child_session_id.clone());
        }
        state.activations.insert(
            child_session_id.clone(),
            Activation {
                child_session_id,
                parent,
                provider,
                provider_generation,
                child,
                owned_children: HashSet::new(),
                parent_activation,
                announced: false,
                draining: false,
            },
        );
        Ok(())
    }

    fn activation_child(&self, child_session_id: &JsString) -> Option<Arc<dyn ContinuationChild>> {
        self.0
            .state
            .lock()
            .unwrap()
            .activations
            .get(child_session_id)
            .filter(|activation| !activation.draining)
            .map(|activation| activation.child.clone())
    }

    fn authorize_activation_parent(
        &self,
        child_session_id: &JsString,
        parent: &ContinuationParent,
        expected_child: ContinuationParent,
    ) -> Result<(), ContinuationError> {
        self.authorize_live_parent(parent)?;
        let state = self.0.state.lock().unwrap();
        let activation = state.activations.get(child_session_id).ok_or_else(|| {
            ContinuationError::new(
                "inactive_subagent",
                format!(
                    "Continuable subagent is not active: {}",
                    text(child_session_id)
                ),
            )
        })?;
        if activation.parent != *parent || activation.child.reference() != expected_child {
            return Err(ContinuationError::new(
                "wrong_subagent_parent",
                format!(
                    "Continuable subagent {} belongs to another live parent agent.",
                    text(child_session_id)
                ),
            ));
        }
        Ok(())
    }

    fn authorize_live_parent(
        &self,
        parent: &ContinuationParent,
    ) -> Result<JsString, ContinuationError> {
        require_id(&parent.session_id, "parent.sessionId")?;
        let nested = self
            .0
            .state
            .lock()
            .unwrap()
            .activations
            .values()
            .any(|activation| !activation.draining && activation.child.reference() == *parent);
        if !nested && !self.0.parents.is_live(parent) {
            return Err(ContinuationError::new(
                "invalid_subagent_parent",
                format!(
                    "Continuable subagent operation requires the exact live parent agent: {}",
                    text(&parent.session_id)
                ),
            ));
        }
        Ok(parent.session_id.clone())
    }

    fn ensure_active(&self, action: &str) -> Result<(), ContinuationError> {
        let lifecycle = self.state();
        if lifecycle != ContinuationManagerState::Active {
            return Err(ContinuationError::new(
                "subagent_manager_inactive",
                format!(
                    "Cannot {action}; subagent continuation manager is {}.",
                    match lifecycle {
                        ContinuationManagerState::Active => "active",
                        ContinuationManagerState::Draining => "draining",
                        ContinuationManagerState::Disposed => "disposed",
                    }
                ),
            ));
        }
        Ok(())
    }

    fn throw_if_cancelled(
        &self,
        cancellation: Option<&Cancellation>,
        action: &str,
    ) -> Result<(), ContinuationError> {
        if cancellation.is_some_and(Cancellation::is_cancelled) {
            return Err(ContinuationError::new(
                "subagent_admission_aborted",
                format!("{action} aborted: aborted"),
            ));
        }
        Ok(())
    }

    fn begin_transaction(&self, action: &str) -> Result<TransactionGuard, ContinuationError> {
        let state = self.0.state.lock().unwrap();
        if state.lifecycle != ContinuationManagerState::Active {
            return Err(ContinuationError::new(
                "subagent_manager_inactive",
                format!("Cannot {action}; subagent continuation manager is inactive."),
            ));
        }
        self.0.transactions.fetch_add(1, Ordering::SeqCst);
        drop(state);
        Ok(TransactionGuard(self.0.clone()))
    }

    fn operation_lock(&self, child_session_id: &JsString) -> Arc<AsyncMutex<()>> {
        self.0
            .locks
            .lock()
            .unwrap()
            .entry(child_session_id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn watch_settlement(&self, child_session_id: JsString) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                let notified = manager.0.residency_changed.notified();
                let Some(child) = manager.activation_child(&child_session_id) else {
                    return;
                };
                match manager.activation_state(&child_session_id) {
                    Some(ContinuationResidencyState::Running) => child.wait_until_idle().await,
                    Some(ContinuationResidencyState::Waiting) => notified.await,
                    Some(ContinuationResidencyState::Settled) => {
                        let _ = manager
                            .dispose_tree(
                                child_session_id.clone(),
                                "continuable_subagent_settled".into(),
                            )
                            .await;
                        return;
                    }
                    None => return,
                }
            }
        });
    }

    async fn dispose_tree(
        &self,
        root: JsString,
        reason: JsString,
    ) -> Result<(), ContinuationError> {
        let order = {
            let state = self.0.state.lock().unwrap();
            let mut order = Vec::new();
            collect_tree(&state.activations, &root, &mut order);
            order
        };
        let mut errors = Vec::new();
        for child_session_id in order.into_iter().rev() {
            let operation_lock = self.operation_lock(&child_session_id);
            let _operation = operation_lock.lock().await;
            if let Err(error) = self
                .finish_one_locked(&child_session_id, reason.clone())
                .await
            {
                errors.push(error.message);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ContinuationError::new(
                "subagent_dispose_failed",
                errors.join("; "),
            ))
        }
    }

    async fn finish_one_locked(
        &self,
        child_session_id: &JsString,
        reason: JsString,
    ) -> Result<(), ContinuationError> {
        let activation = {
            let mut state = self.0.state.lock().unwrap();
            let Some(activation) = state.activations.get_mut(child_session_id) else {
                return Ok(());
            };
            activation.draining = true;
            activation.clone()
        };
        let terminal = activation.child.terminal();
        let disposal = activation.child.dispose(reason).await;
        {
            let mut state = self.0.state.lock().unwrap();
            if state
                .activations
                .get(child_session_id)
                .is_some_and(|current| Arc::ptr_eq(&current.child, &activation.child))
            {
                state.activations.remove(child_session_id);
            }
        }
        if activation.announced {
            self.notify_settlement(&activation, terminal).await;
        }
        {
            let mut state = self.0.state.lock().unwrap();
            if let Some(parent_id) = &activation.parent_activation
                && let Some(parent) = state.activations.get_mut(parent_id)
            {
                parent.owned_children.remove(child_session_id);
            }
        }
        self.0.residency_changed.notify_waiters();
        disposal
    }

    async fn notify_settlement(&self, activation: &Activation, terminal: ContinuationTerminal) {
        let detail = terminal
            .detail
            .as_ref()
            .map(|detail| format!(" ({})", text(detail)))
            .unwrap_or_default();
        let message: JsString = format!(
            "Continuable subagent {} settled: {}{}.",
            text(&activation.child_session_id),
            terminal.state.as_str(),
            detail
        )
        .into();
        let nested = {
            let state = self.0.state.lock().unwrap();
            state
                .activations
                .values()
                .find(|candidate| candidate.child.reference() == activation.parent)
                .map(|candidate| candidate.child.clone())
        };
        if let Some(parent) = nested {
            let item_id = next_id();
            let turn_id = next_id();
            let _ = parent
                .followup(ChildAdmissionRequest {
                    message,
                    item_id,
                    turn_id,
                    cancellation: None,
                    authorize_admission: Arc::new(|| Ok(())),
                })
                .await;
        } else if self.0.parents.is_live(&activation.parent) {
            let _ = self
                .0
                .parents
                .notify(activation.parent.clone(), message)
                .await;
        }
    }
}

struct TransactionGuard(Arc<ManagerInner>);

impl Drop for TransactionGuard {
    fn drop(&mut self) {
        self.0.transactions.fetch_sub(1, Ordering::SeqCst);
        self.0.transactions_changed.notify_waiters();
    }
}

fn residency(activation: &Activation) -> ContinuationResidencyState {
    if activation.draining {
        return ContinuationResidencyState::Settled;
    }
    let child = activation.child.snapshot();
    if !child.active || child.in_flight > 0 || child.pending_turns > 0 {
        ContinuationResidencyState::Running
    } else if !activation.owned_children.is_empty() {
        ContinuationResidencyState::Waiting
    } else {
        ContinuationResidencyState::Settled
    }
}

fn collect_tree(
    activations: &HashMap<JsString, Activation>,
    root: &JsString,
    order: &mut Vec<JsString>,
) {
    let Some(activation) = activations.get(root) else {
        return;
    };
    order.push(root.clone());
    for child in &activation.owned_children {
        collect_tree(activations, child, order);
    }
}

fn ancestor_matches(
    activations: &HashMap<JsString, Activation>,
    activation: &Activation,
    parent: &ContinuationParent,
) -> bool {
    let mut cursor = activation.parent_activation.as_ref();
    while let Some(id) = cursor {
        let Some(candidate) = activations.get(id) else {
            return false;
        };
        if candidate.child.reference() == *parent {
            return true;
        }
        cursor = candidate.parent_activation.as_ref();
    }
    false
}

fn require_id(value: &JsString, field: &'static str) -> Result<JsString, ContinuationError> {
    if text(value).trim().is_empty() {
        return Err(ContinuationError::new(
            "invalid_subagent_id",
            format!("{field} must not be empty."),
        ));
    }
    Ok(value.clone())
}

fn next_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// The production continuation host does not recreate Gateway session wiring.
/// Its factory port is implemented by the existing session composition and
/// returns one fully configured live handle plus its authoritative child
/// transcript writer.
pub trait NativeContinuationSessionFactory: Send + Sync {
    fn materialize(
        &self,
        request: NativeContinuationMaterializeRequest,
    ) -> ContinuationFuture<NativeContinuationMaterializedChild>;

    fn inspect(
        &self,
        request: NativeContinuationInspectRequest,
    ) -> ContinuationFuture<Vec<JsValue>>;
}

#[derive(Clone)]
pub struct NativeContinuationMaterializeRequest {
    pub child_session_id: JsString,
    pub parent: ContinuationParent,
    pub descriptor: ContinuableSubagentDescriptor,
    pub prepared: Option<PreparedContinuation>,
    pub resume: bool,
    pub cancellation: Option<Cancellation>,
}

#[derive(Clone)]
pub struct NativeContinuationInspectRequest {
    pub child_session_id: JsString,
    pub parent: ContinuationParent,
    pub cancellation: Option<Cancellation>,
}

/// A factory result owns the already-composed session. `input_template` is the
/// child runtime configuration produced by the normal subagent composition;
/// the continuation driver only replaces the per-turn message fields.
pub trait NativeContinuationChildConfiguration: Send + Sync {
    fn dispose(&self, reason: JsString) -> ContinuationFuture<()>;
}

/// Runs after the host assigns the child's exact continuation identity but
/// before the manager can publish the activation. Application wiring uses this
/// hook to install session-local continuation tools for nested children.
pub trait NativeContinuationChildConfigurator: Send + Sync {
    fn configure(
        &self,
        child: ContinuationParent,
        handle: LiveSessionHandle,
    ) -> ContinuationFuture<Arc<dyn NativeContinuationChildConfiguration>>;
}

pub struct NativeContinuationMaterializedChild {
    pub handle: LiveSessionHandle,
    pub transcript: Arc<TranscriptWriter>,
    pub input_template: JsObject,
    pub configurator: Option<Arc<dyn NativeContinuationChildConfigurator>>,
}

pub trait NativeContinuationParentNotifier: Send + Sync {
    /// Admit the settlement notice through the exact live parent. Completion
    /// of this future is the durable admission boundary.
    fn notify(&self, message: JsString) -> ContinuationFuture<()>;
}

const CONTINUABLE_BUILTIN_DEFINITION_IDS: &[&str] =
    &["general-purpose", "explore", "plan", "verify"];

fn validate_persisted_subagent_definition_id(
    definition_id: &JsString,
) -> Result<(), ContinuationError> {
    let id = text(definition_id);
    if CONTINUABLE_BUILTIN_DEFINITION_IDS
        .iter()
        .any(|known| *known == id.as_str())
    {
        Ok(())
    } else {
        Err(ContinuationError::new(
            "unknown_subagent_type",
            format!("Unknown persisted subagent type: {id}"),
        ))
    }
}

fn continuation_dispose_failures(
    failures: Vec<ContinuationError>,
    aggregate_code: &'static str,
    aggregate_message: impl FnOnce(usize) -> String,
) -> Result<(), ContinuationError> {
    match failures.len() {
        0 => Ok(()),
        1 => Err(failures.into_iter().next().unwrap()),
        count => Err(ContinuationError::aggregate(
            aggregate_code,
            aggregate_message(count),
            failures,
        )),
    }
}

fn continuation_provider_registry_dispose_failures(
    failures: Vec<ContinuationError>,
) -> Result<(), ContinuationError> {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ContinuationError::aggregate(
            "subagent_provider_dispose_failed",
            format!(
                "Failed to dispose subagent providers: {} failures.",
                failures.len()
            ),
            failures,
        ))
    }
}

struct NativeContinuationAgentRegistry {
    handles: Mutex<HashMap<JsString, LiveSessionHandle>>,
}

impl NativeContinuationAgentRegistry {
    fn new() -> Self {
        Self {
            handles: Mutex::new(HashMap::new()),
        }
    }

    fn register(&self, session_id: JsString, handle: LiveSessionHandle) {
        self.handles.lock().unwrap().insert(session_id, handle);
    }

    fn unregister(&self, session_id: &JsString) {
        self.handles.lock().unwrap().remove(session_id);
    }

    async fn dispose(&self, reason: JsString) -> Result<(), ContinuationError> {
        let handles = std::mem::take(&mut *self.handles.lock().unwrap());
        let mut failures = Vec::new();
        for handle in handles.into_values() {
            if let Err(error) = handle.dispose(reason.clone()).await {
                failures.push(ContinuationError::new(
                    "agent_dispose_failed",
                    error.to_string(),
                ));
            }
        }
        continuation_dispose_failures(
            failures,
            "agent_dispose_failed",
            |count| format!("Failed to dispose registered agent handles: {count} failures."),
        )
    }
}

#[derive(Clone)]
pub struct NativeContinuationParentBinding {
    pub parent: ContinuationParent,
    pub factory: Arc<dyn NativeContinuationSessionFactory>,
    pub notifier: Arc<dyn NativeContinuationParentNotifier>,
}

struct NativeContinuationHost {
    parents: Mutex<HashMap<JsString, NativeContinuationParentBinding>>,
    next_identity: AtomicU64,
    agents: Arc<NativeContinuationAgentRegistry>,
}

impl NativeContinuationHost {
    fn new(agents: Arc<NativeContinuationAgentRegistry>) -> Self {
        Self {
            parents: Mutex::new(HashMap::new()),
            next_identity: AtomicU64::new(1),
            agents,
        }
    }

    fn bind(
        self: &Arc<Self>,
        binding: NativeContinuationParentBinding,
    ) -> Result<NativeContinuationParentLease, ContinuationError> {
        require_id(&binding.parent.session_id, "parent.sessionId")?;
        let parent = binding.parent.clone();
        self.parents
            .lock()
            .unwrap()
            .insert(parent.session_id.clone(), binding);
        Ok(NativeContinuationParentLease {
            inner: Arc::new(NativeContinuationParentLeaseInner {
                host: self.clone(),
                parent,
                active: AtomicBool::new(true),
            }),
        })
    }

    fn binding(
        &self,
        parent: &ContinuationParent,
    ) -> Result<NativeContinuationParentBinding, ContinuationError> {
        self.parents
            .lock()
            .unwrap()
            .get(&parent.session_id)
            .filter(|binding| binding.parent == *parent)
            .cloned()
            .ok_or_else(|| {
                ContinuationError::new(
                    "invalid_subagent_parent",
                    format!(
                        "Continuable subagent parent is not bound: {}",
                        text(&parent.session_id)
                    ),
                )
            })
    }

    fn unbind(&self, parent: &ContinuationParent) {
        let mut parents = self.parents.lock().unwrap();
        if parents
            .get(&parent.session_id)
            .is_some_and(|binding| binding.parent == *parent)
        {
            parents.remove(&parent.session_id);
        }
    }

    fn clear(&self) {
        self.parents.lock().unwrap().clear();
    }
}

impl ContinuationHost for NativeContinuationHost {
    fn create(
        &self,
        request: ContinuationCreateRequest,
    ) -> ContinuationFuture<Arc<dyn ContinuationChild>> {
        let binding = self.binding(&request.parent);
        let identity = self.next_identity.fetch_add(1, Ordering::SeqCst);
        let agents = self.agents.clone();
        let definition_id = request.descriptor.definition_id.clone();
        Box::pin(async move {
            let binding = binding?;
            validate_persisted_subagent_definition_id(&definition_id)?;
            let child_session_id = request.child_session_id.clone();
            let materialized = binding
                .factory
                .materialize(NativeContinuationMaterializeRequest {
                    child_session_id: request.child_session_id,
                    parent: request.parent.clone(),
                    descriptor: request.descriptor,
                    prepared: Some(request.prepared),
                    resume: false,
                    cancellation: request.cancellation,
                })
                .await?;
            let child_reference = ContinuationParent {
                session_id: child_session_id.clone(),
                identity,
            };
            let configuration =
                configure_materialized_child(&materialized, &child_reference).await?;
            agents.register(child_session_id.clone(), materialized.handle.clone());
            materialized.handle.resume_queued_turns();
            let child: Arc<dyn ContinuationChild> = NativeContinuationChild::new_with_identity(
                child_reference,
                request.parent,
                materialized,
                configuration,
                agents,
            );
            Ok(child)
        })
    }

    fn inspect(
        &self,
        child_session_id: JsString,
        parent: ContinuationParent,
        cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<Vec<JsValue>> {
        let binding = self.binding(&parent);
        Box::pin(async move {
            binding?
                .factory
                .inspect(NativeContinuationInspectRequest {
                    child_session_id,
                    parent,
                    cancellation,
                })
                .await
        })
    }

    fn resume(
        &self,
        request: ContinuationResumeRequest,
    ) -> ContinuationFuture<Arc<dyn ContinuationChild>> {
        let binding = self.binding(&request.parent);
        let identity = self.next_identity.fetch_add(1, Ordering::SeqCst);
        let agents = self.agents.clone();
        let definition_id = request.descriptor.definition_id.clone();
        Box::pin(async move {
            let binding = binding?;
            validate_persisted_subagent_definition_id(&definition_id)?;
            let child_session_id = request.child_session_id.clone();
            let materialized = binding
                .factory
                .materialize(NativeContinuationMaterializeRequest {
                    child_session_id: request.child_session_id,
                    parent: request.parent.clone(),
                    descriptor: request.descriptor,
                    prepared: None,
                    resume: true,
                    cancellation: request.cancellation,
                })
                .await?;
            let child_reference = ContinuationParent {
                session_id: child_session_id.clone(),
                identity,
            };
            let configuration =
                configure_materialized_child(&materialized, &child_reference).await?;
            agents.register(child_session_id.clone(), materialized.handle.clone());
            materialized.handle.resume_queued_turns();
            let child: Arc<dyn ContinuationChild> = NativeContinuationChild::new_with_identity(
                child_reference,
                request.parent,
                materialized,
                configuration,
                agents,
            );
            Ok(child)
        })
    }
}

async fn configure_materialized_child(
    materialized: &NativeContinuationMaterializedChild,
    child: &ContinuationParent,
) -> Result<Option<Arc<dyn NativeContinuationChildConfiguration>>, ContinuationError> {
    let Some(configurator) = &materialized.configurator else {
        return Ok(None);
    };
    match configurator
        .configure(child.clone(), materialized.handle.clone())
        .await
    {
        Ok(configuration) => Ok(Some(configuration)),
        Err(error) => {
            let rollback = materialized
                .handle
                .dispose("continuable_subagent_configuration_rollback".into())
                .await;
            match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(ContinuationError::new(
                    "subagent_materialize_failed",
                    format!(
                        "Failed to configure continuable child: {}; rollback failed: {}",
                        error.message, rollback.message
                    ),
                )),
            }
        }
    }
}

impl ContinuationParentDirectory for NativeContinuationHost {
    fn is_live(&self, parent: &ContinuationParent) -> bool {
        self.parents
            .lock()
            .unwrap()
            .get(&parent.session_id)
            .is_some_and(|binding| binding.parent == *parent)
    }

    fn notify(&self, parent: ContinuationParent, message: JsString) -> ContinuationFuture<()> {
        let binding = self.binding(&parent);
        Box::pin(async move { binding?.notifier.notify(message).await })
    }
}

#[derive(Clone)]
pub struct NativeContinuationParentLease {
    inner: Arc<NativeContinuationParentLeaseInner>,
}

struct NativeContinuationParentLeaseInner {
    host: Arc<NativeContinuationHost>,
    parent: ContinuationParent,
    active: AtomicBool,
}

impl NativeContinuationParentLease {
    pub fn parent(&self) -> &ContinuationParent {
        &self.inner.parent
    }

    pub fn is_active(&self) -> bool {
        self.inner.active.load(Ordering::Acquire)
    }

    fn unbind(&self) {
        if self.inner.active.swap(false, Ordering::AcqRel) {
            self.inner.host.unbind(&self.inner.parent);
        }
    }
}

impl Drop for NativeContinuationParentLease {
    fn drop(&mut self) {
        // The owning session must call NativeContinuationRuntime::release_parent
        // to drain descendants. Drop only revokes authority as a final guard.
        if Arc::strong_count(&self.inner) == 1 {
            self.unbind();
        }
    }
}

pub(crate) struct NativeContinuationProvider;

impl NamedContinuationProvider for NativeContinuationProvider {
    fn prepare_continuable(
        &self,
        _request: ContinuationPrepareRequest,
        _cancellation: Option<Cancellation>,
    ) -> ContinuationFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
    }
}

struct NativeContinuationRuntimeInner {
    providers: Arc<NamedContinuationProviderRegistry>,
    host: Arc<NativeContinuationHost>,
    manager: SubagentContinuationManager,
    agents: Arc<NativeContinuationAgentRegistry>,
    dispose_lock: AsyncMutex<()>,
    dispose_result: Mutex<Option<Result<(), ContinuationError>>>,
}

/// Cross-session owner matching TS `GatewaySubagentRuntimeBundle`: manager,
/// providers, then parent directory are released in that order.
#[derive(Clone)]
pub struct NativeContinuationRuntime(Arc<NativeContinuationRuntimeInner>);

impl NativeContinuationRuntime {
    pub fn new() -> Result<Self, ContinuationError> {
        let providers = Arc::new(NamedContinuationProviderRegistry::new());
        providers.register("pilotdeck-native", Arc::new(NativeContinuationProvider))?;
        let agents = Arc::new(NativeContinuationAgentRegistry::new());
        let host = Arc::new(NativeContinuationHost::new(agents.clone()));
        let manager =
            SubagentContinuationManager::new(providers.clone(), host.clone(), host.clone());
        Ok(Self(Arc::new(NativeContinuationRuntimeInner {
            providers,
            host,
            manager,
            agents,
            dispose_lock: AsyncMutex::new(()),
            dispose_result: Mutex::new(None),
        })))
    }

    pub fn manager(&self) -> SubagentContinuationManager {
        self.0.manager.clone()
    }

    pub fn providers(&self) -> Arc<NamedContinuationProviderRegistry> {
        self.0.providers.clone()
    }

    pub fn bind_parent(
        &self,
        binding: NativeContinuationParentBinding,
    ) -> Result<NativeContinuationParentLease, ContinuationError> {
        self.0.host.bind(binding)
    }

    pub async fn release_parent(
        &self,
        lease: &NativeContinuationParentLease,
        reason: JsString,
    ) -> Result<(), ContinuationError> {
        if !lease.is_active() {
            return Ok(());
        }
        let result = self
            .0
            .manager
            .drain_descendants(lease.parent(), reason)
            .await;
        lease.unbind();
        result
    }

    pub async fn dispose(&self, reason: JsString) -> Result<(), ContinuationError> {
        let _dispose = self.0.dispose_lock.lock().await;
        if let Some(result) = self.0.dispose_result.lock().unwrap().clone() {
            return result;
        }
        let mut failures = Vec::new();
        if let Err(error) = self.0.manager.dispose(reason.clone()).await {
            failures.push(error);
        }
        if let Err(error) = self.0.providers.dispose().await {
            failures.push(error);
        }
        if let Err(error) = self.0.agents.dispose(reason.clone()).await {
            failures.push(error);
        }
        self.0.host.clear();
        let result = continuation_dispose_failures(
            failures,
            "subagent_runtime_dispose_failed",
            |count| format!("Failed to dispose native subagent runtime: {count} failures."),
        );
        *self.0.dispose_result.lock().unwrap() = Some(result.clone());
        result
    }

    pub fn register_agent(&self, session_id: JsString, handle: LiveSessionHandle) {
        self.0.agents.register(session_id, handle);
    }
}

struct NativeContinuationChildState {
    accepting: bool,
    in_flight: usize,
    pending_turns: usize,
    terminal: ContinuationTerminal,
}

struct NativeQueuedAdmission {
    request: ChildAdmissionRequest,
    admitted: oneshot::Sender<Result<(), ContinuationError>>,
}

struct NativeContinuationChild {
    reference: ContinuationParent,
    handle: LiveSessionHandle,
    transcript: Arc<TranscriptWriter>,
    sender: Mutex<Option<mpsc::UnboundedSender<NativeQueuedAdmission>>>,
    state: Arc<Mutex<NativeContinuationChildState>>,
    changed: Arc<Notify>,
    configuration: Mutex<Option<Arc<dyn NativeContinuationChildConfiguration>>>,
    agents: Arc<NativeContinuationAgentRegistry>,
}

impl NativeContinuationChild {
    fn new_with_identity(
        reference: ContinuationParent,
        _parent: ContinuationParent,
        materialized: NativeContinuationMaterializedChild,
        configuration: Option<Arc<dyn NativeContinuationChildConfiguration>>,
        agents: Arc<NativeContinuationAgentRegistry>,
    ) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(NativeContinuationChildState {
            accepting: true,
            in_flight: 0,
            pending_turns: 0,
            terminal: ContinuationTerminal {
                state: ContinuationTerminalState::Unknown,
                detail: None,
            },
        }));
        let changed = Arc::new(Notify::new());
        tokio::spawn(run_native_child_queue(
            materialized.handle.clone(),
            materialized.input_template,
            state.clone(),
            changed.clone(),
            receiver,
        ));
        Arc::new(Self {
            reference,
            handle: materialized.handle,
            transcript: materialized.transcript,
            sender: Mutex::new(Some(sender)),
            state,
            changed,
            configuration: Mutex::new(configuration),
            agents,
        })
    }
}

impl ContinuationChild for NativeContinuationChild {
    fn reference(&self) -> ContinuationParent {
        self.reference.clone()
    }

    fn record_descriptor(
        &self,
        turn_id: JsString,
        descriptor: ContinuableSubagentDescriptor,
    ) -> ContinuationFuture<()> {
        let result = if self.state.lock().unwrap().accepting {
            let mut draft = JsObject::new();
            draft.insert("type".into(), "subagent_descriptor".into());
            draft.insert(
                "descriptor".into(),
                SubagentDescriptor::Continuable(descriptor).to_js(),
            );
            self.transcript
                .record_session_event(&self.reference.session_id, &turn_id, draft)
                .map_err(|error| {
                    ContinuationError::new("subagent_descriptor_persist_failed", error.to_string())
                })
        } else {
            Err(ContinuationError::new(
                "inactive_subagent",
                "Continuable subagent is not active.",
            ))
        };
        Box::pin(async move { result })
    }

    fn followup(&self, request: ChildAdmissionRequest) -> ContinuationFuture<()> {
        let authorization = (request.authorize_admission)();
        if let Err(error) = authorization {
            return Box::pin(async move { Err(error) });
        }
        if request
            .cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            return Box::pin(async {
                Err(ContinuationError::new(
                    "subagent_admission_aborted",
                    "Continuable subagent admission aborted: aborted",
                ))
            });
        }
        let sender = {
            let mut state = self.state.lock().unwrap();
            if !state.accepting {
                return Box::pin(async {
                    Err(ContinuationError::new(
                        "inactive_subagent",
                        "Continuable subagent is not active.",
                    ))
                });
            }
            state.pending_turns += 1;
            self.sender.lock().unwrap().clone()
        };
        let (admitted, result) = oneshot::channel();
        let Some(sender) = sender else {
            self.state.lock().unwrap().pending_turns -= 1;
            return Box::pin(async {
                Err(ContinuationError::new(
                    "inactive_subagent",
                    "Continuable subagent is not active.",
                ))
            });
        };
        if sender
            .send(NativeQueuedAdmission { request, admitted })
            .is_err()
        {
            self.state.lock().unwrap().pending_turns -= 1;
            return Box::pin(async {
                Err(ContinuationError::new(
                    "subagent_admission_failed",
                    "Continuable subagent admission queue is closed.",
                ))
            });
        }
        Box::pin(async move {
            result.await.unwrap_or_else(|_| {
                Err(ContinuationError::new(
                    "subagent_admission_failed",
                    "Continuable subagent admission did not complete.",
                ))
            })
        })
    }

    fn snapshot(&self) -> ContinuationChildSnapshot {
        let state = self.state.lock().unwrap();
        ContinuationChildSnapshot {
            active: state.accepting,
            in_flight: state.in_flight,
            pending_turns: state.pending_turns,
        }
    }

    fn wait_until_idle(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.changed.notified();
                let idle = {
                    let state = self.state.lock().unwrap();
                    state.in_flight == 0 && state.pending_turns == 0
                };
                if idle {
                    return;
                }
                notified.await;
            }
        })
    }

    fn terminal(&self) -> ContinuationTerminal {
        self.state.lock().unwrap().terminal.clone()
    }

    fn dispose(&self, reason: JsString) -> ContinuationFuture<()> {
        {
            let mut state = self.state.lock().unwrap();
            state.accepting = false;
        }
        self.sender.lock().unwrap().take();
        self.changed.notify_waiters();
        let handle = self.handle.clone();
        let configuration = self.configuration.lock().unwrap().take();
        let session_id = self.reference.session_id.clone();
        let agents = self.agents.clone();
        Box::pin(async move {
            let mut failures = Vec::new();
            if let Some(configuration) = configuration
                && let Err(error) = configuration.dispose(reason.clone()).await
            {
                failures.push(error);
            }
            if let Err(error) = handle.dispose(reason).await {
                failures.push(ContinuationError::new(
                    "subagent_dispose_failed",
                    error.to_string(),
                ));
            }
            agents.unregister(&session_id);
            continuation_dispose_failures(
                failures,
                "subagent_dispose_failed",
                |count| format!("Failed to dispose continuable subagent: {count} failures."),
            )
        })
    }
}

async fn run_native_child_queue(
    handle: LiveSessionHandle,
    input_template: JsObject,
    state: Arc<Mutex<NativeContinuationChildState>>,
    changed: Arc<Notify>,
    mut receiver: mpsc::UnboundedReceiver<NativeQueuedAdmission>,
) {
    while let Some(queued) = receiver.recv().await {
        {
            let mut progress = state.lock().unwrap();
            progress.pending_turns = progress.pending_turns.saturating_sub(1);
            if !progress.accepting {
                let _ = queued.admitted.send(Err(ContinuationError::new(
                    "inactive_subagent",
                    "Continuable subagent is not active.",
                )));
                changed.notify_waiters();
                continue;
            }
        }
        if queued
            .request
            .cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            let _ = queued.admitted.send(Err(ContinuationError::new(
                "subagent_admission_aborted",
                "Continuable subagent admission aborted: aborted",
            )));
            changed.notify_waiters();
            continue;
        }
        if let Err(error) = (queued.request.authorize_admission)() {
            let _ = queued.admitted.send(Err(error));
            changed.notify_waiters();
            continue;
        }
        state.lock().unwrap().in_flight += 1;
        changed.notify_waiters();
        let outcome = run_native_child_turn(&handle, &input_template, queued).await;
        {
            let mut progress = state.lock().unwrap();
            progress.in_flight = progress.in_flight.saturating_sub(1);
            progress.terminal = outcome;
        }
        changed.notify_waiters();
    }
}

async fn run_native_child_turn(
    handle: &LiveSessionHandle,
    input_template: &JsObject,
    queued: NativeQueuedAdmission,
) -> ContinuationTerminal {
    let message = continuation_message(queued.request.message.clone());
    let mut input = input_template.clone();
    input.insert("messages".into(), JsValue::Array(vec![message.clone()]));
    input.insert("acceptedMessages".into(), JsValue::Array(vec![message]));
    let mut metadata = input
        .get("metadata")
        .and_then(JsValue::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert(
        "queueItemId".into(),
        JsValue::String(queued.request.item_id.clone()),
    );
    input.insert("metadata".into(), JsValue::Object(metadata));
    let options = JsValue::Object({
        let mut options = JsObject::new();
        options.insert(
            "turnId".into(),
            JsValue::String(queued.request.turn_id.clone()),
        );
        options
    });
    let admitted = queued.admitted;
    let mut submission = match handle.submit(JsValue::Object(input), options) {
        Ok(submission) => submission,
        Err(error) => {
            let _ = admitted.send(Err(ContinuationError::new(
                "subagent_admission_failed",
                error.to_string(),
            )));
            return ContinuationTerminal {
                state: ContinuationTerminalState::Failed,
                detail: Some(error.to_string().into()),
            };
        }
    };
    // Durable admission is recorded before the turn stream starts (see
    // ContinuationChildSession::submit_inner). Match TS enqueue success: cancel
    // after this point must not roll back admission.
    let _ = admitted.send(Ok(()));
    let mut terminal = ContinuationTerminal {
        state: ContinuationTerminalState::Unknown,
        detail: None,
    };
    loop {
        let next = if let Some(cancellation) = queued.request.cancellation.as_ref() {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    let _ = handle.abort(&"continuable_subagent_admission_aborted".into());
                    submission.next().await
                }
                next = submission.next() => next,
            }
        } else {
            submission.next().await
        };
        match next {
            Ok(Some(event)) => {
                if event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("turn_completed"))
                {
                    terminal = continuation_terminal(event.get("result"));
                }
            }
            Ok(None) => {
                return terminal;
            }
            Err(error) => {
                return ContinuationTerminal {
                    state: ContinuationTerminalState::Failed,
                    detail: Some(error.to_string().into()),
                };
            }
        }
    }
}

fn continuation_message(text: JsString) -> JsValue {
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert("text".into(), JsValue::String(text));
    let mut message = JsObject::new();
    message.insert("role".into(), "user".into());
    message.insert(
        "content".into(),
        JsValue::Array(vec![JsValue::Object(block)]),
    );
    JsValue::Object(message)
}

fn continuation_terminal(result: Option<&JsValue>) -> ContinuationTerminal {
    let Some(result) = result else {
        return ContinuationTerminal {
            state: ContinuationTerminalState::Unknown,
            detail: None,
        };
    };
    let kind = result
        .get("type")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default();
    let state = match kind.as_str() {
        "success" | "completed" => ContinuationTerminalState::Completed,
        "aborted" => ContinuationTerminalState::Aborted,
        "error" | "failed" => ContinuationTerminalState::Failed,
        _ => ContinuationTerminalState::Unknown,
    };
    let detail = result
        .get("stopReason")
        .and_then(JsValue::as_string)
        .cloned()
        .or_else(|| result.get("error").and_then(JsValue::as_string).cloned());
    ContinuationTerminal { state, detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{
            project::{DisposeFuture, RuntimeResult},
            session::{LiveSession, LiveTurn, SessionNext},
        },
        storage::{
            AsyncTranscriptAdapter, ProjectKey, SessionStorageInput, StoreResult, TranscriptKey,
            TranscriptRead, TranscriptStore,
        },
    };
    use std::{
        collections::VecDeque,
        path::PathBuf,
        sync::atomic::{AtomicBool, AtomicU64},
    };

    fn object(entries: &[(&str, JsValue)]) -> JsValue {
        let mut object = JsObject::new();
        for (key, value) in entries {
            object.insert((*key).into(), value.clone());
        }
        JsValue::Object(object)
    }

    fn descriptor(parent: &str) -> ContinuableSubagentDescriptor {
        snapshot_continuable_descriptor(
            "native".into(),
            "general-purpose".into(),
            parent.into(),
            "proof".into(),
            "proof".into(),
            "model".into(),
        )
        .unwrap()
    }

    #[test]
    fn descriptor_v1_v2_are_strict_and_first_entry_is_authoritative() {
        let v2 = SubagentDescriptor::Continuable(descriptor("parent")).to_js();
        assert_eq!(
            parse_subagent_descriptor(&v2).unwrap(),
            Some(SubagentDescriptor::Continuable(descriptor("parent")))
        );
        let mut invalid = v2.as_object().unwrap().clone();
        invalid.insert("agentModel".into(), "rewritten".into());
        invalid.insert("extra".into(), JsValue::Bool(true));
        assert_eq!(
            parse_subagent_descriptor(&JsValue::Object(invalid))
                .unwrap_err()
                .message,
            "Persisted subagent descriptor has unknown field \"extra\"."
        );
        let unsupported = object(&[
            ("type", "subagent_descriptor".into()),
            ("descriptor", object(&[("version", JsValue::Number(99.0))])),
        ]);
        let valid = object(&[("type", "subagent_descriptor".into()), ("descriptor", v2)]);
        assert_eq!(
            fold_subagent_descriptor(&[unsupported, valid]).unwrap(),
            None
        );
    }

    struct Provider;
    impl ContinuationProviderRegistry for Provider {
        fn prepare(
            &self,
            provider: JsString,
            _request: ContinuationPrepareRequest,
            _cancellation: Option<Cancellation>,
        ) -> ContinuationFuture<PreparedContinuation> {
            Box::pin(async move {
                Ok(PreparedContinuation {
                    provider,
                    generation: 7,
                    spec: JsValue::Null,
                })
            })
        }
    }

    struct Directory {
        live: Mutex<HashMap<JsString, u64>>,
        notices: Mutex<Vec<String>>,
    }
    impl ContinuationParentDirectory for Directory {
        fn is_live(&self, parent: &ContinuationParent) -> bool {
            self.live.lock().unwrap().get(&parent.session_id) == Some(&parent.identity)
        }
        fn notify(&self, _parent: ContinuationParent, message: JsString) -> ContinuationFuture<()> {
            self.notices.lock().unwrap().push(text(&message));
            Box::pin(async { Ok(()) })
        }
    }

    struct Child {
        reference: ContinuationParent,
        descriptors: Mutex<Vec<ContinuableSubagentDescriptor>>,
        admissions: Mutex<Vec<(String, String, String)>>,
        active: AtomicBool,
        idle: Notify,
        disposed: AtomicBool,
    }
    impl Child {
        fn new(session_id: JsString, identity: u64) -> Self {
            Self {
                reference: ContinuationParent {
                    session_id,
                    identity,
                },
                descriptors: Mutex::new(Vec::new()),
                admissions: Mutex::new(Vec::new()),
                active: AtomicBool::new(true),
                idle: Notify::new(),
                disposed: AtomicBool::new(false),
            }
        }
    }
    impl ContinuationChild for Child {
        fn reference(&self) -> ContinuationParent {
            self.reference.clone()
        }
        fn record_descriptor(
            &self,
            _turn_id: JsString,
            descriptor: ContinuableSubagentDescriptor,
        ) -> ContinuationFuture<()> {
            self.descriptors.lock().unwrap().push(descriptor);
            Box::pin(async { Ok(()) })
        }
        fn followup(&self, request: ChildAdmissionRequest) -> ContinuationFuture<()> {
            let authorization = (request.authorize_admission)();
            if let Err(error) = authorization {
                return Box::pin(async move { Err(error) });
            }
            if request
                .cancellation
                .as_ref()
                .is_some_and(Cancellation::is_cancelled)
            {
                return Box::pin(async {
                    Err(ContinuationError::new(
                        "subagent_admission_aborted",
                        "admission aborted",
                    ))
                });
            }
            self.admissions.lock().unwrap().push((
                text(&request.message),
                text(&request.item_id),
                text(&request.turn_id),
            ));
            Box::pin(async { Ok(()) })
        }
        fn snapshot(&self) -> ContinuationChildSnapshot {
            ContinuationChildSnapshot {
                active: self.active.load(Ordering::SeqCst),
                in_flight: if self.active.load(Ordering::SeqCst) {
                    1
                } else {
                    0
                },
                pending_turns: 0,
            }
        }
        fn wait_until_idle(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let notified = self.idle.notified();
            Box::pin(notified)
        }
        fn terminal(&self) -> ContinuationTerminal {
            ContinuationTerminal {
                state: ContinuationTerminalState::Completed,
                detail: None,
            }
        }
        fn dispose(&self, _reason: JsString) -> ContinuationFuture<()> {
            self.disposed.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct Host {
        next_identity: AtomicU64,
        children: Mutex<HashMap<JsString, Arc<Child>>>,
        entries: Mutex<HashMap<JsString, Vec<JsValue>>>,
    }
    impl ContinuationHost for Host {
        fn create(
            &self,
            request: ContinuationCreateRequest,
        ) -> ContinuationFuture<Arc<dyn ContinuationChild>> {
            let child = Arc::new(Child::new(
                request.child_session_id.clone(),
                self.next_identity.fetch_add(1, Ordering::SeqCst),
            ));
            self.children
                .lock()
                .unwrap()
                .insert(request.child_session_id, child.clone());
            let child: Arc<dyn ContinuationChild> = child;
            Box::pin(async move { Ok(child) })
        }
        fn inspect(
            &self,
            child_session_id: JsString,
            _parent: ContinuationParent,
            _cancellation: Option<Cancellation>,
        ) -> ContinuationFuture<Vec<JsValue>> {
            let entries = self
                .entries
                .lock()
                .unwrap()
                .get(&child_session_id)
                .cloned()
                .unwrap_or_default();
            Box::pin(async move { Ok(entries) })
        }
        fn resume(
            &self,
            request: ContinuationResumeRequest,
        ) -> ContinuationFuture<Arc<dyn ContinuationChild>> {
            let child = Arc::new(Child::new(
                request.child_session_id.clone(),
                self.next_identity.fetch_add(1, Ordering::SeqCst),
            ));
            self.children
                .lock()
                .unwrap()
                .insert(request.child_session_id, child.clone());
            let child: Arc<dyn ContinuationChild> = child;
            Box::pin(async move { Ok(child) })
        }
    }

    fn fixture() -> (
        SubagentContinuationManager,
        Arc<Directory>,
        Arc<Host>,
        ContinuationParent,
    ) {
        let parent = ContinuationParent {
            session_id: "parent".into(),
            identity: 1,
        };
        let directory = Arc::new(Directory {
            live: Mutex::new(HashMap::from([(parent.session_id.clone(), 1)])),
            notices: Mutex::new(Vec::new()),
        });
        let host = Arc::new(Host {
            next_identity: AtomicU64::new(10),
            children: Mutex::new(HashMap::new()),
            entries: Mutex::new(HashMap::new()),
        });
        (
            SubagentContinuationManager::new(Arc::new(Provider), host.clone(), directory.clone()),
            directory,
            host,
            parent,
        )
    }

    fn start(parent: ContinuationParent, child: &str) -> StartContinuationRequest {
        StartContinuationRequest {
            provider: "native".into(),
            label: "proof".into(),
            child_session_id: Some(child.into()),
            parent,
            definition_id: "general-purpose".into(),
            agent_provider: "proof".into(),
            agent_model: "model".into(),
            prompt: "initial".into(),
            cancellation: None,
        }
    }

    #[derive(Default)]
    struct MemoryTranscriptStore(Mutex<HashMap<PathBuf, Vec<JsValue>>>);

    impl TranscriptStore for MemoryTranscriptStore {
        fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
            self.0
                .lock()
                .unwrap()
                .entry(key.transcript_path.clone())
                .or_default()
                .push(entry.clone());
            Ok(())
        }

        fn read(
            &self,
            key: &TranscriptKey,
            _max_bytes: Option<u64>,
        ) -> StoreResult<TranscriptRead> {
            Ok(TranscriptRead {
                entries: self
                    .0
                    .lock()
                    .unwrap()
                    .get(&key.transcript_path)
                    .cloned()
                    .unwrap_or_default(),
                diagnostics: vec![],
            })
        }
    }

    struct NativeTestTurn(VecDeque<JsValue>);

    impl LiveTurn for NativeTestTurn {
        fn next(&mut self) -> SessionNext<'_> {
            Box::pin(async move { Ok(self.0.pop_front()) })
        }
    }

    struct NativeTestSession {
        session_id: JsString,
        writer: Arc<TranscriptWriter>,
    }

    impl LiveSession for NativeTestSession {
        fn submit(
            &self,
            input: JsValue,
            options: JsValue,
        ) -> Result<Box<dyn LiveTurn>, crate::runtime::project::RuntimeError> {
            let turn_id = options
                .get("turnId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap();
            let messages = input
                .get("acceptedMessages")
                .and_then(|value| match value {
                    JsValue::Array(messages) => Some(messages.clone()),
                    _ => None,
                })
                .unwrap();
            self.writer
                .record_accepted_input(&self.session_id, &turn_id, messages, None)
                .unwrap();
            let mut started = JsObject::new();
            started.insert("type".into(), "turn_started".into());
            self.writer
                .record_native_session_event(&self.session_id, &turn_id, started)
                .unwrap();
            let result = object(&[
                ("type", "success".into()),
                ("sessionId", JsValue::String(self.session_id.clone())),
                ("turnId", JsValue::String(turn_id.clone())),
                ("usage", object(&[])),
                ("permissionDenials", JsValue::Array(vec![])),
            ]);
            let mut completed = JsObject::new();
            completed.insert("type".into(), "turn_result".into());
            completed.insert("result".into(), result.clone());
            self.writer
                .record_native_session_event(&self.session_id, &turn_id, completed)
                .unwrap();
            Ok(Box::new(NativeTestTurn(VecDeque::from([
                object(&[
                    ("type", "input_accepted".into()),
                    ("sessionId", JsValue::String(self.session_id.clone())),
                    ("turnId", JsValue::String(turn_id.clone())),
                ]),
                object(&[
                    ("type", "turn_completed".into()),
                    ("sessionId", JsValue::String(self.session_id.clone())),
                    ("turnId", JsValue::String(turn_id)),
                    ("result", result),
                ]),
            ]))))
        }

        fn snapshot(&self) -> JsValue {
            object(&[("sessionKey", JsValue::String(self.session_id.clone()))])
        }

        fn abort(&self, _reason: &JsString) -> RuntimeResult {
            Ok(())
        }

        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    struct NativeTestFactory {
        adapter: Arc<AsyncTranscriptAdapter>,
        project_root: PathBuf,
        pilot_home: PathBuf,
        configurator: Option<Arc<dyn NativeContinuationChildConfigurator>>,
    }

    impl NativeTestFactory {
        fn storage(&self, session_id: JsString) -> crate::storage::SessionStorage {
            self.adapter.create_session_storage(SessionStorageInput {
                project_root: self.project_root.clone(),
                pilot_home: self.pilot_home.clone(),
                session_id,
                now: None,
            })
        }
    }

    impl NativeContinuationSessionFactory for NativeTestFactory {
        fn materialize(
            &self,
            request: NativeContinuationMaterializeRequest,
        ) -> ContinuationFuture<NativeContinuationMaterializedChild> {
            let storage = self.storage(request.child_session_id.clone());
            if request.resume {
                storage.restore().unwrap();
            }
            let writer = Arc::new(storage.transcript);
            let handle = LiveSessionHandle::new(
                Arc::new(NativeTestSession {
                    session_id: request.child_session_id,
                    writer: writer.clone(),
                }),
                None,
            );
            let configurator = self.configurator.clone();
            Box::pin(async move {
                Ok(NativeContinuationMaterializedChild {
                    handle,
                    transcript: writer,
                    input_template: JsObject::new(),
                    configurator,
                })
            })
        }

        fn inspect(
            &self,
            request: NativeContinuationInspectRequest,
        ) -> ContinuationFuture<Vec<JsValue>> {
            let result = self
                .storage(request.child_session_id)
                .read_transcript(None)
                .map(|read| read.entries)
                .map_err(|error| {
                    ContinuationError::new("subagent_inspect_failed", error.to_string())
                });
            Box::pin(async move { result })
        }
    }

    #[derive(Default)]
    struct NativeTestNotifier(Mutex<Vec<JsString>>);

    impl NativeContinuationParentNotifier for NativeTestNotifier {
        fn notify(&self, message: JsString) -> ContinuationFuture<()> {
            self.0.lock().unwrap().push(message);
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct NativeTestChildConfiguration(AtomicBool);

    impl NativeContinuationChildConfiguration for NativeTestChildConfiguration {
        fn dispose(&self, _reason: JsString) -> ContinuationFuture<()> {
            self.0.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    struct NativeTestChildConfigurator {
        configured: Mutex<Vec<ContinuationParent>>,
        configuration: Arc<NativeTestChildConfiguration>,
    }

    impl NativeContinuationChildConfigurator for NativeTestChildConfigurator {
        fn configure(
            &self,
            child: ContinuationParent,
            handle: LiveSessionHandle,
        ) -> ContinuationFuture<Arc<dyn NativeContinuationChildConfiguration>> {
            assert_eq!(
                handle.snapshot().get("sessionKey"),
                Some(&JsValue::String(child.session_id.clone()))
            );
            self.configured.lock().unwrap().push(child);
            let configuration: Arc<dyn NativeContinuationChildConfiguration> =
                self.configuration.clone();
            Box::pin(async move { Ok(configuration) })
        }
    }

    #[tokio::test]
    async fn child_configuration_receives_assigned_identity_and_disposes_with_child() {
        let store = Arc::new(MemoryTranscriptStore::default());
        let chat_dir = PathBuf::from("/virtual/configured-chats");
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |_: &ProjectKey| chat_dir.clone()),
        ));
        let configuration = Arc::new(NativeTestChildConfiguration::default());
        let configurator = Arc::new(NativeTestChildConfigurator {
            configured: Mutex::new(Vec::new()),
            configuration: configuration.clone(),
        });
        let factory = Arc::new(NativeTestFactory {
            adapter,
            project_root: PathBuf::from("/virtual/configured-project"),
            pilot_home: PathBuf::from("/virtual/configured-home"),
            configurator: Some(configurator.clone()),
        });
        let parent = ContinuationParent {
            session_id: "configured-parent".into(),
            identity: 77,
        };
        let runtime = NativeContinuationRuntime::new().unwrap();
        let lease = runtime
            .bind_parent(NativeContinuationParentBinding {
                parent: parent.clone(),
                factory,
                notifier: Arc::new(NativeTestNotifier::default()),
            })
            .unwrap();

        runtime
            .manager()
            .start(StartContinuationRequest {
                provider: "pilotdeck-native".into(),
                label: "configured".into(),
                child_session_id: Some("configured-child".into()),
                parent,
                definition_id: "general-purpose".into(),
                agent_provider: "provider".into(),
                agent_model: "model".into(),
                prompt: "inspect".into(),
                cancellation: None,
            })
            .await
            .unwrap();
        let configured = configurator.configured.lock().unwrap().clone();
        assert_eq!(configured.len(), 1);
        assert_eq!(configured[0].session_id, JsString::from("configured-child"));
        assert_eq!(configured[0].identity, 1);

        runtime
            .release_parent(&lease, "test_done".into())
            .await
            .unwrap();
        assert!(configuration.0.load(Ordering::SeqCst));
        runtime.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn native_runtime_persists_descriptor_before_admission_and_revokes_parent() {
        let store = Arc::new(MemoryTranscriptStore::default());
        let chat_dir = PathBuf::from("/virtual/chats");
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |_: &ProjectKey| chat_dir.clone()),
        ));
        let factory = Arc::new(NativeTestFactory {
            adapter: adapter.clone(),
            project_root: PathBuf::from("/virtual/project"),
            pilot_home: PathBuf::from("/virtual/home"),
            configurator: None,
        });
        let notifier = Arc::new(NativeTestNotifier::default());
        let parent = ContinuationParent {
            session_id: "parent".into(),
            identity: 41,
        };
        let runtime = NativeContinuationRuntime::new().unwrap();
        let lease = runtime
            .bind_parent(NativeContinuationParentBinding {
                parent: parent.clone(),
                factory: factory.clone(),
                notifier: notifier.clone(),
            })
            .unwrap();
        runtime
            .manager()
            .start(StartContinuationRequest {
                provider: "pilotdeck-native".into(),
                label: "proof".into(),
                child_session_id: Some("child".into()),
                parent: parent.clone(),
                definition_id: "general-purpose".into(),
                agent_provider: "provider".into(),
                agent_model: "model".into(),
                prompt: "inspect".into(),
                cancellation: None,
            })
            .await
            .unwrap();
        let entries = factory
            .storage("child".into())
            .read_transcript(None)
            .unwrap()
            .entries;
        assert_eq!(
            entries
                .iter()
                .take(2)
                .map(|entry| text(entry.get("type").unwrap().as_string().unwrap()))
                .collect::<Vec<_>>(),
            vec!["subagent_descriptor", "accepted_input"]
        );

        runtime
            .release_parent(&lease, "parent_disposed".into())
            .await
            .unwrap();
        assert!(!lease.is_active());
        let error = runtime
            .manager()
            .start(start(parent, "after-release"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_subagent_parent");
        runtime.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn native_child_pumps_durable_followups_in_fifo_order() {
        let store = Arc::new(MemoryTranscriptStore::default());
        let chat_dir = PathBuf::from("/virtual/pump-chats");
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |_: &ProjectKey| chat_dir.clone()),
        ));
        let factory = NativeTestFactory {
            adapter,
            project_root: PathBuf::from("/virtual/pump-project"),
            pilot_home: PathBuf::from("/virtual/pump-home"),
            configurator: None,
        };
        let parent = ContinuationParent {
            session_id: "parent".into(),
            identity: 1,
        };
        let child_session_id: JsString = "pumped-child".into();
        let materialized = factory
            .materialize(NativeContinuationMaterializeRequest {
                child_session_id: child_session_id.clone(),
                parent: parent.clone(),
                descriptor: descriptor("parent"),
                prepared: None,
                resume: false,
                cancellation: None,
            })
            .await
            .unwrap();
        let child = NativeContinuationChild::new_with_identity(
            ContinuationParent {
                session_id: child_session_id.clone(),
                identity: 2,
            },
            parent,
            materialized,
            None,
            Arc::new(NativeContinuationAgentRegistry::new()),
        );
        let request = |message: &str, item: &str, turn: &str| ChildAdmissionRequest {
            message: message.into(),
            item_id: item.into(),
            turn_id: turn.into(),
            cancellation: None,
            authorize_admission: Arc::new(|| Ok(())),
        };

        let first = child.followup(request("first", "item-1", "turn-1"));
        let second = child.followup(request("second", "item-2", "turn-2"));
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        child.wait_until_idle().await;

        let accepted_turns = factory
            .storage(child_session_id)
            .read_transcript(None)
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| entry.get("type") == Some(&"accepted_input".into()))
            .map(|entry| entry.get("turnId").unwrap().as_string().unwrap().clone())
            .collect::<Vec<_>>();
        assert_eq!(
            accepted_turns,
            vec![JsString::from("turn-1"), JsString::from("turn-2")]
        );
        child.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn start_persists_descriptor_before_admission_and_rejects_other_parent() {
        let (manager, directory, host, parent) = fixture();
        let admission = manager.start(start(parent.clone(), "child")).await.unwrap();
        assert_eq!(admission.child_session_id, JsString::from("child"));
        let child = host
            .children
            .lock()
            .unwrap()
            .get(&"child".into())
            .unwrap()
            .clone();
        assert_eq!(child.descriptors.lock().unwrap().len(), 1);
        assert_eq!(child.admissions.lock().unwrap()[0].0, "initial");

        directory.live.lock().unwrap().insert("parent".into(), 2);
        let error = manager
            .followup(FollowupContinuationRequest {
                child_session_id: "child".into(),
                parent,
                message: "late".into(),
                cancellation: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_subagent_parent");
    }

    #[tokio::test]
    async fn cancellation_before_admission_rolls_back_but_not_after_return() {
        let (manager, _directory, host, parent) = fixture();
        let cancellation = Cancellation::new();
        cancellation.cancel_synchronously();
        let mut request = start(parent.clone(), "cancelled");
        request.cancellation = Some(cancellation);
        assert_eq!(
            manager.start(request).await.unwrap_err().code,
            "subagent_admission_aborted"
        );
        assert!(!manager.has(&"cancelled".into()));

        let accepted = manager.start(start(parent, "accepted")).await.unwrap();
        assert_eq!(accepted.child_session_id, JsString::from("accepted"));
        assert_eq!(
            host.children
                .lock()
                .unwrap()
                .get(&"accepted".into())
                .unwrap()
                .admissions
                .lock()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn cold_resume_uses_owned_first_descriptor_and_rejects_cross_parent() {
        let (manager, _directory, host, parent) = fixture();
        let entry = object(&[
            ("sessionId", "cold".into()),
            ("type", "subagent_descriptor".into()),
            (
                "descriptor",
                SubagentDescriptor::Continuable(descriptor("parent")).to_js(),
            ),
        ]);
        host.entries
            .lock()
            .unwrap()
            .insert("cold".into(), vec![entry]);
        let admission = manager
            .followup(FollowupContinuationRequest {
                child_session_id: "cold".into(),
                parent: parent.clone(),
                message: "resume".into(),
                cancellation: None,
            })
            .await
            .unwrap();
        assert_eq!(admission.child_session_id, JsString::from("cold"));
        assert!(manager.has(&"cold".into()));

        manager
            .dispose_child(&"cold".into(), "test".into())
            .await
            .unwrap();
        let wrong = object(&[
            ("sessionId", "wrong".into()),
            ("type", "subagent_descriptor".into()),
            (
                "descriptor",
                SubagentDescriptor::Continuable(descriptor("someone-else")).to_js(),
            ),
        ]);
        host.entries
            .lock()
            .unwrap()
            .insert("wrong".into(), vec![wrong]);
        assert_eq!(
            manager
                .followup(FollowupContinuationRequest {
                    child_session_id: "wrong".into(),
                    parent,
                    message: "forbidden".into(),
                    cancellation: None,
                })
                .await
                .unwrap_err()
                .code,
            "wrong_subagent_parent"
        );
    }

    #[tokio::test]
    async fn production_bound_port_rejects_cross_parent_followup() {
        use crate::runtime::tools::builtin::subagent_continuation::{
            BoundSubagentContinuationPort, SubagentContinuationPort, SubagentFollowupPortRequest,
            SubagentStartPortRequest,
        };
        let (manager, directory, _host, parent_a) = fixture();
        let parent_b = ContinuationParent {
            session_id: "parent-b".into(),
            identity: 2,
        };
        directory
            .live
            .lock()
            .unwrap()
            .insert(parent_b.session_id.clone(), parent_b.identity);
        let port_a = BoundSubagentContinuationPort::new(
            manager.clone(),
            parent_a.clone(),
            "native".into(),
            "parity".into(),
            "deterministic".into(),
            vec!["general-purpose".into()],
        );
        let admission = port_a
            .start(SubagentStartPortRequest {
                label: "cross-parent proof".into(),
                definition_id: "general-purpose".into(),
                prompt: "hello".into(),
                cancellation: None,
            })
            .await
            .unwrap();
        let port_b = BoundSubagentContinuationPort::new(
            manager,
            parent_b,
            "native".into(),
            "parity".into(),
            "deterministic".into(),
            vec!["general-purpose".into()],
        );
        assert_eq!(
            port_b
                .followup(SubagentFollowupPortRequest {
                    child_session_id: admission.child_session_id,
                    message: "forbidden".into(),
                    cancellation: None,
                })
                .await
                .unwrap_err()
                .code,
            "wrong_subagent_parent"
        );
    }

    #[tokio::test]
    async fn recursive_disposal_is_children_first_and_notifies_parent_after_admission() {
        let (manager, directory, host, parent) = fixture();
        manager
            .start(start(parent.clone(), "root-child"))
            .await
            .unwrap();
        let nested_parent = host
            .children
            .lock()
            .unwrap()
            .get(&"root-child".into())
            .unwrap()
            .reference();
        manager
            .start(start(nested_parent, "grandchild"))
            .await
            .unwrap();
        assert_eq!(
            manager.activation_state(&"root-child".into()),
            Some(ContinuationResidencyState::Running)
        );
        manager
            .dispose_child(&"root-child".into(), "parent_agent_disposed".into())
            .await
            .unwrap();
        assert_eq!(manager.size(), 0);
        assert_eq!(directory.notices.lock().unwrap().len(), 1);
        assert!(directory.notices.lock().unwrap()[0].contains("root-child settled: completed"));
    }

    struct NamedProvider {
        label: &'static str,
        disposed: AtomicBool,
        disposal_order: Arc<Mutex<Vec<&'static str>>>,
        fail_dispose: bool,
    }

    impl NamedProvider {
        fn new(
            label: &'static str,
            disposal_order: Arc<Mutex<Vec<&'static str>>>,
            fail_dispose: bool,
        ) -> Arc<Self> {
            Arc::new(Self {
                label,
                disposed: AtomicBool::new(false),
                disposal_order,
                fail_dispose,
            })
        }
    }

    impl NamedContinuationProvider for NamedProvider {
        fn prepare_continuable(
            &self,
            _request: ContinuationPrepareRequest,
            _cancellation: Option<Cancellation>,
        ) -> ContinuationFuture<JsValue> {
            Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
        }

        fn dispose(&self) -> ContinuationFuture<()> {
            self.disposal_order.lock().unwrap().push(self.label);
            self.disposed.store(true, Ordering::SeqCst);
            let fail = self.fail_dispose;
            let label = self.label;
            Box::pin(async move {
                if fail {
                    Err(ContinuationError::new(
                        "provider_dispose_proof",
                        format!("{label} dispose failed"),
                    ))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn provider_replacement_publishes_new_generation_and_drains_old_lease() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let old = NamedProvider::new("old", order.clone(), false);
        let new = NamedProvider::new("new", order, false);
        let registry = NamedContinuationProviderRegistry::new();
        let old_registration = registry.register(" native ", old.clone()).unwrap();
        assert_eq!(old_registration.name(), "native");
        assert_eq!(old_registration.generation(), 1);
        let lease = registry.lease_generation("native", 1).unwrap();
        assert_eq!(old_registration.in_flight(), 1);

        let replacement = registry.replace("native", new).unwrap();
        assert_eq!(replacement.registration.generation(), 2);
        assert!(!old_registration.active());
        assert_eq!(
            old_registration.lease().err().unwrap().code,
            "subagent_provider_inactive"
        );
        assert_eq!(
            registry.lease_generation("native", 1).err().unwrap().code,
            "stale_subagent_provider_generation"
        );
        assert_eq!(registry.lease("native").unwrap().generation(), 2);
        assert!(!old.disposed.load(Ordering::SeqCst));
        drop(lease);
        replacement.previous_disposed.await.unwrap();
        assert!(old.disposed.load(Ordering::SeqCst));
        // Repeated drain/dispose is idempotent.
        old_registration.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn provider_registry_prepares_detached_spec_and_disposes_reverse_with_errors() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let registry = NamedContinuationProviderRegistry::new();
        registry
            .register("first", NamedProvider::new("first", order.clone(), true))
            .unwrap();
        registry
            .register("second", NamedProvider::new("second", order.clone(), false))
            .unwrap();
        let prepared = registry
            .prepare_continuable(
                "second".into(),
                ContinuationPrepareRequest {
                    child_session_id: "child".into(),
                    parent_session_id: "parent".into(),
                    definition_id: "general-purpose".into(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(prepared.provider, JsString::from("second"));
        assert_eq!(prepared.generation, 2);
        assert!(prepared.spec.as_object().unwrap().is_empty());

        let error = registry.dispose().await.unwrap_err();
        assert_eq!(error.code, "subagent_provider_dispose_failed");
        assert_eq!(error.causes.len(), 1);
        assert_eq!(error.causes[0].code, "provider_dispose_proof");
        assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);
        assert_eq!(registry.state(), ProviderRegistryState::Disposed);
        assert_eq!(
            registry.dispose().await.unwrap_err().code,
            "subagent_provider_dispose_failed"
        );
    }

    #[tokio::test]
    async fn durable_submit_admits_even_if_cancel_arrives_before_turn_stream_events() {
        let store = Arc::new(MemoryTranscriptStore::default());
        let chat_dir = PathBuf::from("/virtual/cancel-admit-chats");
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |_: &ProjectKey| chat_dir.clone()),
        ));
        let factory = NativeTestFactory {
            adapter,
            project_root: PathBuf::from("/virtual/cancel-admit-project"),
            pilot_home: PathBuf::from("/virtual/cancel-admit-home"),
            configurator: None,
        };
        let parent = ContinuationParent {
            session_id: "parent".into(),
            identity: 1,
        };
        let child_session_id: JsString = "cancel-admit".into();
        let materialized = factory
            .materialize(NativeContinuationMaterializeRequest {
                child_session_id: child_session_id.clone(),
                parent: parent.clone(),
                descriptor: descriptor("parent"),
                prepared: None,
                resume: false,
                cancellation: None,
            })
            .await
            .unwrap();
        let child = NativeContinuationChild::new_with_identity(
            ContinuationParent {
                session_id: child_session_id.clone(),
                identity: 2,
            },
            parent,
            materialized,
            None,
            Arc::new(NativeContinuationAgentRegistry::new()),
        );
        let cancellation = Cancellation::new();
        let request = ChildAdmissionRequest {
            message: "follow".into(),
            item_id: "item".into(),
            turn_id: "turn".into(),
            cancellation: Some(cancellation.clone()),
            authorize_admission: Arc::new(|| Ok(())),
        };
        let followup = child.followup(request);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            cancellation.cancel();
        });
        followup.await.unwrap();
        child.wait_until_idle().await;
        child.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn cold_resume_rejects_unknown_persisted_definition_id() {
        let (manager, _directory, host, parent) = fixture();
        let mut unknown = descriptor("parent");
        unknown.definition_id = "mystery-agent".into();
        let entry = object(&[
            ("sessionId", "unknown-def".into()),
            ("type", "subagent_descriptor".into()),
            (
                "descriptor",
                SubagentDescriptor::Continuable(unknown).to_js(),
            ),
        ]);
        host.entries
            .lock()
            .unwrap()
            .insert("unknown-def".into(), vec![entry]);
        let error = manager
            .followup(FollowupContinuationRequest {
                child_session_id: "unknown-def".into(),
                parent: parent.clone(),
                message: "resume".into(),
                cancellation: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "unknown_subagent_type");
        assert!(error.message.contains("Unknown persisted subagent type: mystery-agent"));
    }

    #[tokio::test]
    async fn provider_registry_dispose_preserves_multiple_failure_codes() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let registry = NamedContinuationProviderRegistry::new();
        registry
            .register("first", NamedProvider::new("first", order.clone(), true))
            .unwrap();
        registry
            .register("second", NamedProvider::new("second", order.clone(), true))
            .unwrap();
        let error = registry.dispose().await.unwrap_err();
        assert_eq!(error.code, "subagent_provider_dispose_failed");
        assert_eq!(error.causes.len(), 2);
        assert_eq!(error.causes[0].code, "provider_dispose_proof");
        assert_eq!(error.causes[1].code, "provider_dispose_proof");
    }

    #[tokio::test]
    async fn runtime_dispose_drains_registered_parent_agent_handles() {
        let runtime = NativeContinuationRuntime::new().unwrap();
        let store = Arc::new(MemoryTranscriptStore::default());
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |_: &ProjectKey| PathBuf::from("/virtual/agent-parent-chats")),
        ));
        let storage = adapter.create_session_storage(SessionStorageInput {
            project_root: PathBuf::from("/virtual/agent-parent-project"),
            pilot_home: PathBuf::from("/virtual/agent-parent-home"),
            session_id: "parent-agent".into(),
            now: None,
        });
        let handle = LiveSessionHandle::new(
            Arc::new(NativeTestSession {
                session_id: "parent-agent".into(),
                writer: Arc::new(storage.transcript),
            }),
            None,
        );
        runtime.register_agent("parent-agent".into(), handle);
        runtime.dispose("test_done".into()).await.unwrap();
    }
}
