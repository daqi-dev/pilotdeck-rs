//! In-process agent loop: no OS sidecar child; host module calls hit the gateway
//! [`HostHandler`] directly via [`pilotdeck_transport::direct_host::DirectHostCallback`].
use super::direct_host::DirectModuleHost;
use crate::host::{ExecuteRequest, ExecuteResult, HostError, HostErrorKind, HostHandler, Limits};
use pilotdeck_protocol::admission::SidecarBinding;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
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
    reusable: bool,
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
            reusable: true,
        }
    }

    fn shutdown(self) {
        self.server.retire();
        self.owner.set_direct_host(None);
        if self.worker.is_finished() {
            let _ = self.worker.join();
        } else {
            // A callback may still be unwinding on the Tokio runtime. Never join
            // it on that runtime's thread or hold up the next connection.
            std::thread::spawn(move || {
                let _ = self.worker.join();
            });
        }
    }
}

// An abandoned execute must cancel its worker even when no later turn arrives.
struct ExecuteGuard {
    server: Arc<ExecutionServer>,
    completed: bool,
}
impl Drop for ExecuteGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.server.retire();
        }
    }
}

/// One embedded sidecar worker reused for every turn in a session.
pub struct SessionEmbeddedWorker {
    inner: Mutex<Option<EmbeddedRuntime>>,
    cwd: JsString,
    execution: tokio::sync::Mutex<()>,
}

impl SessionEmbeddedWorker {
    pub fn new(cwd: JsString) -> Self {
        Self {
            inner: Mutex::new(None),
            cwd,
            execution: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn execute(
        &self,
        request: ExecuteRequest,
        handler: Arc<dyn HostHandler>,
        limits: Limits,
        cancellation: crate::host::Cancellation,
    ) -> Result<ExecuteResult, HostError> {
        let _execution = self.execution.lock().await;
        let tokio_handle = tokio::runtime::Handle::current();
        let retired = {
            let mut slot = self.inner.lock().unwrap();
            if slot
                .as_ref()
                .is_some_and(|runtime| !runtime.reusable || runtime.owner.is_closed())
            {
                slot.take()
            } else {
                None
            }
        };
        if let Some(runtime) = retired {
            runtime.shutdown();
        }
        let (runtime, owner) = {
            let mut slot = self.inner.lock().unwrap();
            let embedded = slot.get_or_insert_with(|| {
                EmbeddedRuntime::start(handler.clone(), self.cwd.clone(), tokio_handle.clone())
            });
            embedded
                .owner
                .set_direct_host(Some(Arc::new(DirectModuleHost::new(
                    handler.clone(),
                    tokio_handle,
                ))));
            // Only a consumed terminal makes this connection safe to reuse.
            // Dropping this future leaves it retired on the next execute.
            embedded.reusable = false;
            (embedded.server.clone(), embedded.owner.clone())
        };
        let mut execution_guard = ExecuteGuard {
            server: runtime.clone(),
            completed: false,
        };
        runtime
            .receive(&wire_execute(&request))
            .map_err(|error| HostError::new(HostErrorKind::Io, error.to_string()))?;
        let terminal = tokio::time::timeout(limits.execution_timeout, async {
            let cancel = async {
                while !cancellation.is_cancelled() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                {
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
                    runtime
                        .receive(&JsValue::Object(cancel))
                        .map_err(|error| HostError::new(HostErrorKind::Io, error.to_string()))?;
                }
                std::future::pending::<Result<JsValue, HostError>>().await
            };
            let events = async {
                loop {
                    while let Some(line) = owner.try_pop_control_line() {
                        let value = pilotdeck_protocol::parse_json(&line).map_err(|error| {
                            HostError::new(HostErrorKind::Io, error.to_string())
                        })?;
                        if value.get("kind") == Some(&JsValue::from("error")) {
                            return Err(HostError::new(
                                HostErrorKind::Io,
                                "Embedded agent rejected the execution envelope.",
                            ));
                        }
                        if value.get("inReplyTo") == Some(&JsValue::from("execute"))
                            && (value.get("final").and_then(JsValue::as_bool) == Some(true)
                                || value.get("ok").and_then(JsValue::as_bool) == Some(false))
                        {
                            return Ok(value);
                        }
                    }
                    if let Some(line) = owner.try_pop_stream_line() {
                        let value = pilotdeck_protocol::parse_json(&line).map_err(|error| {
                            HostError::new(HostErrorKind::Io, error.to_string())
                        })?;
                        if value.get("final").and_then(JsValue::as_bool) == Some(true) {
                            // The worker publishes final after every event in this FIFO.
                            return Ok(value);
                        }
                        if let Some(payload) = value.get("payload") {
                            if payload.get("type") != Some(&JsValue::from("turn_completed")) {
                                handler.event(payload.clone()).await?;
                            }
                        }
                        tokio::task::yield_now().await;
                        continue;
                    }
                    if owner.is_closed() {
                        return Err(HostError::new(
                            HostErrorKind::Io,
                            "Embedded agent connection closed.",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            };
            tokio::select! {
                terminal = events => terminal,
                failure = cancel => failure,
            }
        })
        .await
        .map_err(|_| {
            HostError::new(
                HostErrorKind::Timeout,
                "Embedded agent turn timed out waiting for terminal.",
            )
        })??;
        execution_guard.completed = true;
        {
            let mut slot = self.inner.lock().unwrap();
            if let Some(embedded) = slot.as_mut() {
                embedded.owner.set_direct_host(None);
                embedded.reusable = true;
            }
        }

        if terminal.get("ok").and_then(JsValue::as_bool) == Some(false)
            || terminal
                .get("outcome")
                .and_then(JsValue::as_string)
                .is_some_and(|o| o.equals_str("failed") || o.equals_str("cancelled"))
        {
            return Ok(ExecuteResult::Rejected(terminal));
        }
        Ok(ExecuteResult::Terminal(terminal))
    }

    pub async fn shutdown(&self) {
        let _execution = self.execution.lock().await;
        let runtime = self.inner.lock().unwrap().take();
        if let Some(runtime) = runtime {
            runtime.shutdown();
        }
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

#[cfg(test)]
#[path = "embedded_sidecar_tests.rs"]
mod tests;
