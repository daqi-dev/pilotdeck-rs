//! In-process agent loop: no OS sidecar child; host module calls hit the gateway
//! [`HostHandler`] directly via [`pilotdeck_transport::direct_host::DirectHostCallback`].
use super::direct_host::DirectModuleHost;
use crate::host::{ExecuteRequest, ExecuteResult, HostError, HostErrorKind, HostHandler, Limits};
use pilotdeck_protocol::admission::SidecarBinding;
use pilotdeck_protocol::{JsObject, JsString, JsValue, MODULE_PROTOCOL_VERSION};
use pilotdeck_transport::default_factory::OriginalAgentFactory;
use pilotdeck_transport::server::{ExecutionClock, ExecutionServer};
use pilotdeck_transport::stdio::ConnectionOwner;
use pilotdeck_transport::system_clock::SystemExecutionClock;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
const QUEUE: usize = 256;

pub(crate) fn subprocess_sidecar_enabled() -> bool {
    std::env::var_os("PILOTDECK_AGENT_SUBPROCESS").is_some_and(|value| !value.is_empty())
}

struct EmbeddedRuntime {
    server: Arc<ExecutionServer>,
    owner: ConnectionOwner,
    worker: JoinHandle<()>,
}

impl EmbeddedRuntime {
    fn start(
        handler: Arc<dyn HostHandler>,
        cwd: JsString,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let instance = format!("pilotdeck-agent-loop-instance-{}", uuid::Uuid::new_v4());
        let binding = SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: instance.clone().into(),
            connection_generation: format!("{instance}-{}", uuid::Uuid::new_v4()).into(),
            capabilities_version: "1".into(),
        };
        let owner = ConnectionOwner::new(binding, QUEUE);
        owner.set_direct_host(Some(Arc::new(DirectModuleHost::new(
            handler,
            runtime.clone(),
        ))));
        let clock: Arc<dyn ExecutionClock> = Arc::new(SystemExecutionClock);
        let factory = Arc::new(OriginalAgentFactory {
            clock: clock.clone(),
            process_cwd: cwd,
        });
        let server = ExecutionServer::new(owner.clone(), factory, clock, QUEUE);
        let worker_server = server.clone();
        let worker = std::thread::spawn(move || worker_server.worker());
        Self {
            server,
            owner,
            worker,
        }
    }

    fn shutdown(self) {
        self.server.retire();
        let _ = self.worker.join();
    }
}

/// One embedded sidecar worker reused for every turn in a session.
pub struct SessionEmbeddedWorker {
    inner: Mutex<Option<EmbeddedRuntime>>,
    cwd: JsString,
}

impl SessionEmbeddedWorker {
    pub fn new(cwd: JsString) -> Self {
        Self {
            inner: Mutex::new(None),
            cwd,
        }
    }

    pub async fn execute(
        &self,
        request: ExecuteRequest,
        handler: Arc<dyn HostHandler>,
        limits: Limits,
        cancellation: crate::host::Cancellation,
    ) -> Result<ExecuteResult, HostError> {
        let tokio_handle = tokio::runtime::Handle::current();
        let runtime =
            {
                let mut slot = self.inner.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(EmbeddedRuntime::start(
                        handler.clone(),
                        self.cwd.clone(),
                        tokio_handle.clone(),
                    ));
                }
                // Refresh direct host each turn so the active ModuleHost is current.
                slot.as_ref().unwrap().owner.set_direct_host(Some(Arc::new(
                    DirectModuleHost::new(handler.clone(), tokio_handle.clone()),
                )));
                slot.as_ref().unwrap().server.clone()
            };
        let owner = {
            let slot = self.inner.lock().unwrap();
            slot.as_ref().unwrap().owner.clone()
        };

        let wire = wire_execute(&request);
        let terminal_slot = Arc::new(Mutex::new(None::<JsValue>));
        let event_handler = handler.clone();
        let drain_owner = owner.clone();
        let execution_timeout = limits.execution_timeout;
        let terminal_drain_timeout = limits.terminal_drain_timeout;
        let terminal_for_drain = terminal_slot.clone();
        let drain = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + execution_timeout;
            let mut post_final_deadline = None::<tokio::time::Instant>;
            loop {
                let now = tokio::time::Instant::now();
                if post_final_deadline.is_some_and(|end| now >= end) {
                    break;
                }
                if post_final_deadline.is_none() && now >= deadline {
                    break;
                }
                let line = tokio::task::spawn_blocking({
                    let drain_owner = drain_owner.clone();
                    move || drain_owner.try_pop_stream_line()
                })
                .await
                .ok()
                .flatten();
                let Some(line) = line else {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                };
                let Ok(value) = pilotdeck_protocol::parse_json(&line) else {
                    continue;
                };
                if value.get("final").and_then(JsValue::as_bool) == Some(true) {
                    *terminal_for_drain.lock().unwrap() = Some(value);
                    post_final_deadline =
                        Some(tokio::time::Instant::now() + terminal_drain_timeout);
                    continue;
                }
                if let Some(payload) = value.get("payload") {
                    // Match the subprocess client: the final frame owns completion.
                    if payload.get("type") == Some(&JsValue::from("turn_completed")) {
                        continue;
                    }
                    let _ = event_handler.event(payload.clone()).await;
                }
            }
        });

        runtime
            .receive(&wire)
            .map_err(|error| HostError::new(HostErrorKind::Io, error.to_string()))?;

        let terminal = tokio::time::timeout(execution_timeout, async {
            let mut cancelled = false;
            loop {
                if let Some(terminal) = terminal_slot.lock().unwrap().clone() {
                    return terminal;
                }
                if !cancelled && cancellation.is_cancelled() {
                    cancelled = true;
                    let mut cancel = JsObject::new();
                    cancel.insert("kind".into(), "request".into());
                    cancel.insert("messageId".into(), "cancel".into());
                    cancel.insert("method".into(), "cancel".into());
                    cancel.insert(
                        "runId".into(),
                        JsValue::String(request.identity.run_id.clone()),
                    );
                    cancel.insert(
                        "operationId".into(),
                        JsValue::String(request.identity.operation_id.clone()),
                    );
                    cancel.insert(
                        "requestId".into(),
                        JsValue::String(request.identity.request_id.clone()),
                    );
                    cancel.insert("reason".into(), "subagent_parent_aborted".into());
                    let _ = runtime.receive(&JsValue::Object(cancel));
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .map_err(|_| {
            HostError::new(
                HostErrorKind::Timeout,
                "Embedded agent turn timed out waiting for terminal.",
            )
        })?;
        let _ = tokio::time::timeout(terminal_drain_timeout, drain).await;

        if terminal
            .get("outcome")
            .and_then(JsValue::as_string)
            .is_some_and(|o| o.equals_str("failed") || o.equals_str("cancelled"))
        {
            return Ok(ExecuteResult::Rejected(terminal));
        }
        Ok(ExecuteResult::Terminal(terminal))
    }
}

impl Drop for SessionEmbeddedWorker {
    fn drop(&mut self) {
        if let Some(runtime) = self.inner.lock().unwrap().take() {
            runtime.shutdown();
        }
    }
}

fn wire_execute(request: &ExecuteRequest) -> JsValue {
    let mut envelope = JsObject::new();
    envelope.insert("kind".into(), "request".into());
    envelope.insert("messageId".into(), "execute".into());
    envelope.insert("method".into(), "execute".into());
    envelope.insert(
        "runId".into(),
        JsValue::String(request.identity.run_id.clone()),
    );
    envelope.insert(
        "operationId".into(),
        JsValue::String(request.identity.operation_id.clone()),
    );
    envelope.insert(
        "requestId".into(),
        JsValue::String(request.identity.request_id.clone()),
    );
    envelope.insert(
        "sessionId".into(),
        JsValue::String(request.identity.session_id.clone()),
    );
    envelope.insert(
        "turnId".into(),
        JsValue::String(request.identity.turn_id.clone()),
    );
    if let Some(key) = &request.idempotency_key {
        envelope.insert("idempotencyKey".into(), JsValue::String(key.clone()));
    }
    if let Some(deadline) = &request.operation_deadline {
        envelope.insert(
            "operationDeadline".into(),
            JsValue::String(deadline.clone()),
        );
    }
    envelope.insert("payload".into(), JsValue::Object(request.payload.clone()));
    JsValue::Object(envelope)
}

#[allow(dead_code)]
fn wire_hello() -> JsValue {
    JsValue::Object({
        let mut hello = JsObject::new();
        hello.insert("kind".into(), "request".into());
        hello.insert("messageId".into(), "hello".into());
        hello.insert("method".into(), "hello".into());
        hello.insert(
            "protocolVersion".into(),
            JsValue::from(MODULE_PROTOCOL_VERSION),
        );
        hello
    })
}
