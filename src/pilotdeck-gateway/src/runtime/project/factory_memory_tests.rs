//! #639: YAML normalization -> registry -> context storage -> session prepare
//! -> actual resolver calls. Unrelated families are silent; memory and context
//! composition, including the deprecated native adapter, are production code.
use super::*;
use crate::host::{
    Cancellation,
    modules::context::{ContextInput, ContextRuntime},
};
use crate::runtime::context::memory::{
    MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture, MemoryResolver, MemoryRetrieveInput,
    MemoryRetrieveResult,
};
use crate::runtime::memory::{
    MemoryBundleError, MemoryProviderError, NativeMemoryService, NativeProjectMemoryProvider,
    ProjectMemoryProvider,
};
use crate::runtime::project::context_storage::{
    ContextStorageResources, SessionContextOptions, SessionContextStorage,
};
use crate::runtime::project::memory_bundle::MemoryBundleFamilySource;
use std::sync::atomic::AtomicBool;
use std::task::Poll;

#[derive(Clone, Copy)]
enum Behavior {
    Success(u64),
    Failure,
    Pending,
}

struct RecordingMemory {
    behavior: Behavior,
    inputs: Mutex<Vec<MemoryRetrieveInput>>,
    dropped: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    closes: AtomicUsize,
    close_gate: Option<Arc<CloseGate>>,
}
impl RecordingMemory {
    fn new(behavior: Behavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            inputs: Mutex::new(Vec::new()),
            dropped: Arc::new(AtomicBool::new(false)),
            completed: Arc::new(AtomicBool::new(false)),
            closes: AtomicUsize::new(0),
            close_gate: None,
        })
    }
    fn source(self: &Arc<Self>, native: bool) -> MemoryBundleFamilySource {
        let memory = self.clone();
        if native {
            MemoryBundleFamilySource::new().with_create_provider(Arc::new(move |_| {
                Ok(Some(NativeProjectMemoryProvider {
                    provider: memory.clone(),
                    service: memory.clone(),
                }))
            }))
        } else {
            MemoryBundleFamilySource::new().with_provider_factory(Arc::new(move |_| {
                Ok(Some(ProjectMemoryProvider {
                    memory: memory.clone(),
                    maintenance: None,
                    management: None,
                    dispose: None,
                }))
            }))
        }
    }
    fn cancelled(&self) -> bool {
        self.inputs
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .signal
            .is_cancelled()
    }
}
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl MemoryResolver for RecordingMemory {
    fn capture_turn(&self, _: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        Box::pin(async { Ok(()) })
    }
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        self.inputs.lock().unwrap().push(input);
        let behavior = self.behavior;
        let dropped = DropFlag(self.dropped.clone());
        let completed = self.completed.clone();
        Box::pin(async move {
            let _dropped = dropped;
            match behavior {
                Behavior::Pending => std::future::pending().await,
                Behavior::Failure => Err("probe failure".into()),
                Behavior::Success(delay) => {
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    completed.store(true, Ordering::SeqCst);
                    Ok(MemoryRetrieveResult {
                        system_context: Some("real provider context".into()),
                        diagnostics: vec![],
                    })
                }
            }
        })
    }
}
impl NativeMemoryService for RecordingMemory {
    fn run_due_scheduled_maintenance(
        &self,
        _: &str,
    ) -> crate::runtime::memory::MemoryFuture<'_, ()> {
        Box::pin(async { Err(MemoryProviderError::new("maintenance outside this fixture")) })
    }
    fn list(&self, _: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        Err(MemoryProviderError::new("management outside this fixture"))
    }
    fn clear(&self, _: &str) -> Result<(), MemoryProviderError> {
        Err(MemoryProviderError::new("management outside this fixture"))
    }
    fn clear_session(&self, _: &JsString) -> Result<(), MemoryProviderError> {
        Err(MemoryProviderError::new("management outside this fixture"))
    }
    fn close(&self) -> Result<(), MemoryProviderError> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.close_gate {
            gate.entered.notify_one();
            let (released, timeout) = gate
                .wake
                .wait_timeout_while(
                    gate.released.lock().unwrap(),
                    Duration::from_secs(10),
                    |released| !*released,
                )
                .unwrap();
            drop(released);
            gate.exited.store(true, Ordering::SeqCst);
            assert!(!timeout.timed_out(), "close gate was never released");
        }
        Ok(())
    }
}

#[derive(Default)]
struct CloseGate {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: std::sync::Condvar,
    exited: AtomicBool,
}
// Unblock cleanup on both normal completion and failed test assertions.
struct ReleaseClose(Arc<CloseGate>);
impl Drop for ReleaseClose {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.wake.notify_one();
    }
}

fn yaml(memory: Option<&str>) -> String {
    match memory {
        Some(fields) => format!("{CONFIG_YAML_NO_MEMORY}memory: {{{fields}}}\n"),
        None => CONFIG_YAML_NO_MEMORY.into(),
    }
}
fn storage(lease: &RuntimeLease) -> Arc<dyn FamilyResource> {
    lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context()
        .resource(Resource::InstructionStorage)
        .unwrap()
}
fn session(
    resource: &Arc<dyn FamilyResource>,
    dir: &Path,
) -> Result<SessionContextStorage, RuntimeError> {
    resource
        .as_any()
        .downcast_ref::<ContextStorageResources>()
        .unwrap()
        .create_session(dir)
}
fn build(
    scratch: &Scratch,
    config: Option<&str>,
    source: MemoryBundleFamilySource,
) -> (ProjectRuntimeRegistry, RuntimeLease, SessionContextStorage) {
    let registry = registry_for_yaml(
        scratch,
        &yaml(config),
        silent_families().with_context_memory().with_memory(source),
    );
    let lease = registry.acquire(Some(&scratch.0.join("project"))).unwrap();
    let context = session(&storage(&lease), &scratch.0.join("spill")).unwrap();
    (registry, lease, context)
}
fn input(scratch: &Scratch) -> ContextInput {
    ContextInput {
        fields: pilotdeck_protocol::parse_json(&json!({
            "sessionId": "probe-session", "turnId": "probe-turn",
            "cwd": scratch.0.join("different-cwd"), "provider": "test", "model": "mock",
            "permissionMode": "default", "messages": [{"role":"user", "content":[{"type":"text", "text":"retrieve me"}]}], "tools": []
        }).to_string()).unwrap().as_object().unwrap().clone(),
        abort_signal: None, budget_evaluator: None,
    }
}
fn text_field(value: &JsValue, field: &str) -> String {
    value
        .get(field)
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}
fn memory_diagnostics(result: &JsValue) -> Vec<&JsValue> {
    let JsValue::Array(diagnostics) = result.get("diagnostics").unwrap() else {
        panic!("diagnostics")
    };
    diagnostics
        .iter()
        .filter(|d| d.get("code") == Some(&JsValue::from("memory_provider_error")))
        .collect()
}
fn assert_no_memory(result: &JsValue) {
    assert!(!text_field(result, "systemPrompt").contains("<memory-context>"));
}
async fn poll_pending<F: Future>(mut future: std::pin::Pin<&mut F>) {
    assert!(std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test(start_paused = true)]
async fn context_memory_project_root_and_success_before_default_or_override_deadline() {
    for (fields, delay, native) in [("", 29_999, false), ("retrievalTimeoutMs: 17.9", 16, true)] {
        let scratch = Scratch::new();
        let memory = RecordingMemory::new(Behavior::Success(delay));
        let (registry, lease, composed) = build(&scratch, Some(fields), memory.source(native));
        let request = input(&scratch);
        let expected_messages = request.fields.get("messages").unwrap().clone();
        let mut prepared = Box::pin(composed.context_runtime.prepare_for_model(request));
        poll_pending(prepared.as_mut()).await;
        {
            let inputs = memory.inputs.lock().unwrap();
            assert_eq!(inputs.len(), 1);
            assert_eq!(
                inputs[0].project_root,
                scratch.0.join("project").to_str().unwrap()
            );
            assert_ne!(
                inputs[0].project_root,
                scratch.0.join("different-cwd").to_str().unwrap()
            );
            assert_eq!(inputs[0].query, "retrieve me");
            assert_eq!(inputs[0].session_id, "probe-session");
            assert_eq!(
                JsValue::Array(inputs[0].recent_messages.clone()),
                expected_messages
            );
        }
        tokio::time::advance(Duration::from_millis(delay)).await;
        let result = prepared.await.unwrap();
        assert!(
            text_field(&result, "systemPrompt")
                .contains("<memory-context>\nreal provider context\n</memory-context>")
        );
        assert!(memory_diagnostics(&result).is_empty());
        assert!(memory.completed.load(Ordering::SeqCst));
        assert!(!memory.cancelled());
        drop(composed);
        drop(lease);
        registry.dispose().await.unwrap();
        assert_eq!(memory.closes.load(Ordering::SeqCst), usize::from(native));
    }
}

#[tokio::test]
async fn context_memory_scoped_opt_out_skips_resolver_while_default_inherits() {
    for native in [false, true] {
        let scratch = Scratch::new();
        let memory = RecordingMemory::new(Behavior::Success(0));
        let (registry, lease, main) = build(&scratch, Some(""), memory.source(native));
        let resource = storage(&lease);
        let storage = resource
            .as_any()
            .downcast_ref::<ContextStorageResources>()
            .unwrap();
        for memory_disabled in [true, false] {
            let scoped = storage
                .create_session_with_options(
                    scratch.0.join("scoped-spill"),
                    SessionContextOptions {
                        memory_disabled,
                        ..SessionContextOptions::default()
                    },
                )
                .unwrap();
            let before = memory.inputs.lock().unwrap().len();
            let result = scoped
                .context_runtime
                .prepare_for_model(input(&scratch))
                .await
                .unwrap();
            assert_eq!(
                memory.inputs.lock().unwrap().len() - before,
                usize::from(!memory_disabled)
            );
            assert_eq!(
                text_field(&result, "systemPrompt").contains("real provider context"),
                !memory_disabled
            );
            assert!(memory_diagnostics(&result).is_empty());
        }
        // Opting out one scoped context never disables the shared provider.
        let result = main
            .context_runtime
            .prepare_for_model(input(&scratch))
            .await
            .unwrap();
        assert!(text_field(&result, "systemPrompt").contains("real provider context"));
        assert_eq!(memory.inputs.lock().unwrap().len(), 2);
        drop(main);
        drop(lease);
        registry.dispose().await.unwrap();
        assert_eq!(memory.closes.load(Ordering::SeqCst), usize::from(native));
    }
}

#[tokio::test]
async fn context_memory_host_workspace_root_drives_retrieval_and_instructions() {
    let scratch = Scratch::new();
    let memory = RecordingMemory::new(Behavior::Success(0));
    let (registry, lease, main) = build(&scratch, Some(""), memory.source(false));
    let generation_root = scratch.0.join("project");
    let host_root = scratch.0.join("host-workspace");
    std::fs::create_dir_all(&host_root).unwrap();
    std::fs::create_dir_all(scratch.0.join("pilot-home")).unwrap();
    std::fs::write(
        generation_root.join("PILOTDECK.md"),
        "GENERATION-INSTRUCTION",
    )
    .unwrap();
    std::fs::write(host_root.join("PILOTDECK.md"), "HOST-INSTRUCTION").unwrap();
    std::fs::write(
        scratch.0.join("pilot-home/PILOTDECK.md"),
        "USER-INSTRUCTION",
    )
    .unwrap();
    let resource = storage(&lease);
    let storage = resource
        .as_any()
        .downcast_ref::<ContextStorageResources>()
        .unwrap();
    for host_workspace_root in [Some(host_root.clone()), None] {
        let expected_root = host_workspace_root.as_ref().unwrap_or(&generation_root);
        let (expected_instruction, excluded_instruction) = if host_workspace_root.is_some() {
            ("HOST-INSTRUCTION", "GENERATION-INSTRUCTION")
        } else {
            ("GENERATION-INSTRUCTION", "HOST-INSTRUCTION")
        };
        let composed = storage
            .create_session_with_options(
                scratch.0.join("execution-spill"),
                SessionContextOptions {
                    host_workspace_root: host_workspace_root.clone(),
                    ..SessionContextOptions::default()
                },
            )
            .unwrap();
        let result = composed
            .context_runtime
            .prepare_for_model(input(&scratch))
            .await
            .unwrap();
        {
            let calls = memory.inputs.lock().unwrap();
            assert_eq!(
                calls.last().unwrap().project_root,
                expected_root.to_str().unwrap()
            );
            assert_ne!(
                calls.last().unwrap().project_root,
                scratch.0.join("different-cwd").to_str().unwrap()
            );
        }
        let prompt = text_field(&result, "systemPrompt");
        assert!(prompt.contains(expected_instruction), "{prompt}");
        assert!(!prompt.contains(excluded_instruction), "{prompt}");
        assert!(prompt.contains("USER-INSTRUCTION"), "{prompt}");
        assert_eq!(
            composed
                .tool_result_budget
                .tool_results_dir()
                .to_utf8()
                .unwrap(),
            scratch.0.join("execution-spill").to_str().unwrap()
        );
    }
    assert_eq!(memory.inputs.lock().unwrap().len(), 2);
    drop(main);
    drop(lease);
    registry.dispose().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn context_memory_default_and_normalized_override_timeout_cancel_and_drop_retrieve() {
    for (fields, deadline) in [("", 30_000), ("retrievalTimeoutMs: 17.9", 17)] {
        let scratch = Scratch::new();
        let memory = RecordingMemory::new(Behavior::Pending);
        let (registry, lease, composed) = build(&scratch, Some(fields), memory.source(false));
        let mut prepared = Box::pin(composed.context_runtime.prepare_for_model(input(&scratch)));
        poll_pending(prepared.as_mut()).await;
        assert_eq!(
            memory.inputs.lock().unwrap().len(),
            1,
            "must reach retrieve"
        );
        tokio::time::advance(Duration::from_millis(deadline - 1)).await;
        poll_pending(prepared.as_mut()).await;
        assert!(
            !memory.cancelled(),
            "must not cancel before configured deadline"
        );
        assert!(!memory.dropped.load(Ordering::SeqCst));
        tokio::time::advance(Duration::from_millis(1)).await;
        let result = prepared.await.unwrap();
        assert!(
            memory.cancelled(),
            "deadline must cancel the actual retrieve signal"
        );
        assert!(
            memory.dropped.load(Ordering::SeqCst),
            "timeout must drop the pending future"
        );
        assert!(!memory.completed.load(Ordering::SeqCst));
        assert_no_memory(&result);
        let diagnostics = memory_diagnostics(&result);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].get("severity"), Some(&"warning".into()));
        assert_eq!(
            diagnostics[0].get("message"),
            Some(&JsValue::String(
                format!("MemoryResolver.retrieve timed out after {deadline}ms.").into()
            ))
        );
        drop(composed);
        drop(lease);
        registry.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn context_memory_retrieve_error_is_diagnostic_without_success_attachment() {
    let scratch = Scratch::new();
    let memory = RecordingMemory::new(Behavior::Failure);
    let (registry, lease, composed) = build(&scratch, Some(""), memory.source(false));
    let result = composed
        .context_runtime
        .prepare_for_model(input(&scratch))
        .await
        .unwrap();
    assert_eq!(memory.inputs.lock().unwrap().len(), 1);
    assert_no_memory(&result);
    let diagnostics = memory_diagnostics(&result);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].get("severity"), Some(&"warning".into()));
    assert_eq!(
        diagnostics[0].get("message"),
        Some(&"MemoryResolver.retrieve failed: probe failure".into())
    );
    assert!(!memory.cancelled());
    drop(composed);
    drop(lease);
    registry.dispose().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn context_memory_parent_abort_cancels_pending_and_pre_abort_skips_retrieve() {
    for pre_abort in [false, true] {
        let scratch = Scratch::new();
        let memory = RecordingMemory::new(Behavior::Pending);
        let (registry, lease, composed) = build(&scratch, Some(""), memory.source(false));
        let cancellation = Cancellation::default();
        let mut request = input(&scratch);
        request.abort_signal = Some(Arc::new(cancellation.clone()));
        if pre_abort {
            cancellation.cancel(None);
        }
        let mut prepared = Box::pin(composed.context_runtime.prepare_for_model(request));
        if !pre_abort {
            poll_pending(prepared.as_mut()).await;
            assert!(!memory.cancelled());
            cancellation.cancel(None);
        }
        let result = prepared.await.unwrap();
        assert_eq!(memory.inputs.lock().unwrap().len(), usize::from(!pre_abort));
        if !pre_abort {
            assert!(memory.cancelled());
            assert!(memory.dropped.load(Ordering::SeqCst));
        }
        assert_no_memory(&result);
        assert!(
            memory_diagnostics(&result).is_empty(),
            "parent abort is not a provider failure"
        );
        drop(composed);
        drop(lease);
        registry.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn context_memory_absent_disabled_and_factory_none_prepare_without_memory() {
    for (config, source) in [
        (None, MemoryBundleFamilySource::new()),
        (Some("enabled: false"), MemoryBundleFamilySource::new()),
        (
            Some(""),
            MemoryBundleFamilySource::new().with_provider_factory(Arc::new(|_| Ok(None))),
        ),
        (
            Some(""),
            MemoryBundleFamilySource::new().with_create_provider(Arc::new(|_| Ok(None))),
        ),
    ] {
        let scratch = Scratch::new();
        let (registry, lease, composed) = build(&scratch, config, source);
        assert!(
            lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .unwrap()
                .context()
                .resource(Resource::Memory)
                .is_none()
        );
        let result = composed
            .context_runtime
            .prepare_for_model(input(&scratch))
            .await
            .unwrap();
        assert_no_memory(&result);
        assert!(memory_diagnostics(&result).is_empty());
        drop(composed);
        drop(lease);
        registry.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn context_memory_factory_failures_do_not_publish_generation() {
    for (source, expected) in [
        (
            MemoryBundleFamilySource::new().with_provider_factory(Arc::new(|_| {
                Err(MemoryBundleError::Factory(MemoryProviderError::new(
                    "factory failure",
                )))
            })),
            "factory failure",
        ),
        (
            MemoryBundleFamilySource::new().with_create_provider(Arc::new(|_| {
                Err(MemoryBundleError::Factory(MemoryProviderError::new(
                    "native failure",
                )))
            })),
            "native failure",
        ),
    ] {
        let scratch = Scratch::new();
        let registry = registry_for_yaml(
            &scratch,
            &yaml(Some("")),
            silent_families().with_context_memory().with_memory(source),
        );
        for _ in 0..2 {
            let error = registry
                .acquire(Some(&scratch.0.join("project")))
                .err()
                .expect("failed generation must not publish a context");
            assert_eq!(error.message, expected);
        }
        registry.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn context_memory_wrong_resource_type_fails_instead_of_silent_skip() {
    let scratch = Scratch::new();
    let (registry, lease, composed) = build(&scratch, None, MemoryBundleFamilySource::new());
    let resources = lease
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap();
    resources
        .sink()
        .register_resource(Resource::Memory, storage(&lease));
    let error = session(&storage(&lease), &scratch.0.join("second-spill"))
        .err()
        .expect("wrong resource type must fail");
    assert_eq!(
        error.message,
        "Project memory resource is not a memory resolver."
    );
    drop(composed);
    drop(lease);
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn context_memory_weak_lookup_does_not_keep_generation_alive() {
    let scratch = Scratch::new();
    let memory = RecordingMemory::new(Behavior::Success(0));
    let (registry, lease, composed) = build(&scratch, Some(""), memory.source(false));
    let retained_storage = storage(&lease);
    let weak_slots = Arc::downgrade(
        &lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .slots,
    );
    drop(lease);
    registry.dispose().await.unwrap();
    drop(registry);
    assert!(
        weak_slots.upgrade().is_none(),
        "context storage must not form a resource ownership cycle"
    );
    let error = session(&retained_storage, &scratch.0.join("late-spill"))
        .err()
        .expect("dropped generation must not become unconfigured memory");
    assert_eq!(
        error.message,
        "Context storage's project generation is no longer available."
    );
    // An already composed context owns the resolver Arc, not a late lookup.
    // This fixture's provider has no disposer; this is an ownership check,
    // not authorization to use a disposed native service after lease release.
    let result = composed
        .context_runtime
        .prepare_for_model(input(&scratch))
        .await
        .unwrap();
    assert!(text_field(&result, "systemPrompt").contains("real provider context"));
    assert_eq!(memory.inputs.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_memory_create_session_during_native_close_fails_closed() {
    let scratch = Scratch::new();
    let gate = Arc::new(CloseGate::default());
    let release = ReleaseClose(gate.clone());
    let mut memory = RecordingMemory::new(Behavior::Success(0));
    Arc::get_mut(&mut memory).unwrap().close_gate = Some(gate.clone());
    let (registry, lease, composed) = build(&scratch, Some(""), memory.source(true));
    let retained_storage = storage(&lease);
    let storage = retained_storage
        .as_any()
        .downcast_ref::<ContextStorageResources>()
        .unwrap();
    let weak_slots = Arc::downgrade(
        &lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .slots,
    );
    registry.invalidate(Some(&scratch.0.join("project")));
    // Retirement alone must not prevent a still-leased session lookup.
    let leased = storage
        .create_session(scratch.0.join("leased-spill"))
        .unwrap();
    let result = leased
        .context_runtime
        .prepare_for_model(input(&scratch))
        .await
        .unwrap();
    assert!(text_field(&result, "systemPrompt").contains("real provider context"));
    assert_eq!(memory.closes.load(Ordering::SeqCst), 0);
    drop(leased);
    drop(composed);
    drop(lease);
    bounded(gate.entered.notified()).await;
    assert!(
        weak_slots.upgrade().is_some(),
        "cleanup still owns the generation"
    );
    assert_eq!(memory.closes.load(Ordering::SeqCst), 1);
    assert!(
        !gate.exited.load(Ordering::SeqCst),
        "native close must still be in flight"
    );
    for memory_disabled in [false, true] {
        let error = storage
            .create_session_with_options(
                scratch.0.join("late-spill"),
                SessionContextOptions {
                    memory_disabled,
                    ..SessionContextOptions::default()
                },
            )
            .err()
            .expect("closing generation must reject new session composition");
        assert_eq!(
            error.message,
            "Context storage's project generation is no longer available."
        );
    }
    assert_eq!(memory.inputs.lock().unwrap().len(), 1);
    drop(release);
    bounded(registry.dispose()).await.unwrap();
    assert!(gate.exited.load(Ordering::SeqCst));
    assert_eq!(memory.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn context_memory_leased_sessions_keep_their_generation_resolver() {
    let scratch = Scratch::new();
    let providers = Arc::new(Mutex::new(Vec::new()));
    let created = providers.clone();
    let source = MemoryBundleFamilySource::new().with_provider_factory(Arc::new(move |_| {
        let memory = RecordingMemory::new(Behavior::Success(0));
        created.lock().unwrap().push(memory.clone());
        Ok(Some(ProjectMemoryProvider {
            memory,
            maintenance: None,
            management: None,
            dispose: None,
        }))
    }));
    let (registry, old_lease, old_session) = build(&scratch, Some(""), source);
    let root = scratch.0.join("project");
    registry.invalidate(Some(&root));
    let new_lease = registry.acquire(Some(&root)).unwrap();
    let new_session = session(&storage(&new_lease), &scratch.0.join("new-spill")).unwrap();
    let memories = providers.lock().unwrap().clone();
    assert_eq!(memories.len(), 2);
    old_session
        .context_runtime
        .prepare_for_model(input(&scratch))
        .await
        .unwrap();
    assert_eq!(memories[0].inputs.lock().unwrap().len(), 1);
    assert!(memories[1].inputs.lock().unwrap().is_empty());
    new_session
        .context_runtime
        .prepare_for_model(input(&scratch))
        .await
        .unwrap();
    assert_eq!(memories[0].inputs.lock().unwrap().len(), 1);
    assert_eq!(memories[1].inputs.lock().unwrap().len(), 1);
    drop(old_session);
    drop(new_session);
    drop(old_lease);
    drop(new_lease);
    registry.dispose().await.unwrap();
}
