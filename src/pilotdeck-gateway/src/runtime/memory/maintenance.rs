//! Best-effort maintenance keyed by the exact published generation (TS cc7c4e6).
use super::{MemoryFuture, MemoryMaintenanceService, MemoryProviderError};
use crate::runtime::{
    project::{
        ProjectResources, ProjectRuntime, Resource, RuntimeError, RuntimeResult,
        memory_bundle::MemoryMaintenanceResource,
    },
    refresh::MemoryMaintenance,
};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};
use tokio::runtime::Handle;

/// A published object, never a project-name key or a fresh per-request wrapper.
pub trait MaintenanceRuntime: Send + Sync + 'static {
    fn project_root(&self) -> JsString;
    fn memory_service(&self) -> Option<Arc<dyn MemoryMaintenanceService>>;
}
impl MaintenanceRuntime for ProjectRuntime {
    fn project_root(&self) -> JsString {
        self.project_root().to_string_lossy().as_ref().into()
    }
    fn memory_service(&self) -> Option<Arc<dyn MemoryMaintenanceService>> {
        let resources = self.resources().get::<ProjectResources>()?;
        let service = resources.context().resource(Resource::MemoryService)?;
        Some(
            service
                .as_any()
                .downcast_ref::<MemoryMaintenanceResource>()?
                .maintenance()
                .clone(),
        )
    }
}
pub trait MaintenanceTelemetry: Send + Sync {
    fn track_feature_loop_stage(&self, input: JsValue);
    fn track_error(&self, error: &MemoryProviderError, input: JsValue);
}
pub type ResolveRuntime<R> =
    Arc<dyn Fn(Option<&JsString>) -> Result<Arc<R>, RuntimeError> + Send + Sync>;
pub type Diagnostic = Arc<dyn Fn(&JsString, &MemoryProviderError) + Send + Sync>;
#[derive(Default)]
struct State {
    requested: bool,
    in_flight: bool,
}
type States<R> = Mutex<Vec<(Weak<R>, Arc<Mutex<State>>)>>;
struct Inner<R> {
    resolve: ResolveRuntime<R>,
    telemetry: Arc<dyn MaintenanceTelemetry>,
    diagnostic: Diagnostic,
    states: States<R>,
    executor: Handle,
    #[cfg(test)]
    settling: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    cancel_next: std::sync::atomic::AtomicBool,
}
/// Dropping a Tokio executor releases pending work; a later schedule may retry.
struct Flight(Option<Arc<Mutex<State>>>);
impl Drop for Flight {
    fn drop(&mut self) {
        if let Some(state) = &self.0 {
            state.lock().unwrap().in_flight = false;
        }
    }
}
pub struct ProjectMemoryMaintenanceController<R = ProjectRuntime>(Arc<Inner<R>>);
impl<R: MaintenanceRuntime> ProjectMemoryMaintenanceController<R> {
    /// Construct inside the executor that owns this application's background work.
    pub fn new(
        resolve: ResolveRuntime<R>,
        telemetry: Arc<dyn MaintenanceTelemetry>,
        diagnostic: Option<Diagnostic>,
    ) -> Self {
        Self(Arc::new(Inner {
            resolve,
            telemetry,
            diagnostic: diagnostic.unwrap_or_else(|| {
                Arc::new(|message, error| {
                    eprintln!(
                        "{} {}",
                        String::from_utf16_lossy(message.units()),
                        String::from_utf16_lossy(error.message.units())
                    );
                })
            }),
            states: Mutex::new(Vec::new()),
            executor: Handle::current(),
            #[cfg(test)]
            settling: Mutex::new(None),
            #[cfg(test)]
            cancel_next: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub fn schedule(&self, key: Option<&JsString>) -> RuntimeResult {
        let runtime = (self.0.resolve)(key)?;
        let Some(service) = runtime.memory_service() else {
            return Ok(());
        };
        let state = {
            let mut states = self.0.states.lock().unwrap();
            states.retain(|(runtime, _)| runtime.strong_count() > 0);
            if let Some((_, state)) = states
                .iter()
                .find(|(key, _)| key.ptr_eq(&Arc::downgrade(&runtime)))
            {
                state.clone()
            } else {
                let state = Arc::new(Mutex::new(State::default()));
                states.push((Arc::downgrade(&runtime), state.clone()));
                state
            }
        };
        {
            let mut state = state.lock().unwrap();
            state.requested = true;
            if state.in_flight {
                return Ok(());
            }
            state.in_flight = true;
            state.requested = false;
        }
        // TS invokes the service before schedule returns. The spawned task always
        // polls again, replacing the noop waker even if the first wake was lost.
        let flight = Flight(Some(state.clone()));
        let mut first = invocation(service.clone());
        let _entered = self.0.executor.enter();
        if let Poll::Ready(result) = first.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            first = Box::pin(std::future::ready(result));
        }
        let inner = self.0.clone();
        self.0.executor.spawn(async move {
            let mut flight = flight;
            let mut next = first;
            loop {
                // Each invocation is supervised separately: a panic must report
                // the failure and still consume any request queued during it.
                // Both tasks belong to the same executor; shutdown drops Flight.
                let task = inner.executor.spawn(next);
                #[cfg(test)]
                if inner.cancel_next.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    task.abort();
                }
                let result = match task.await {
                    Ok(result) => result,
                    Err(error) => match error.try_into_panic() {
                        Ok(payload) => Err(panic_error(payload)),
                        Err(_) => return, // Executor cancelled this invocation.
                    },
                };
                match result {
                    Ok(()) => inner.telemetry.track_feature_loop_stage(parse_json(r#"{"module":"memory","ownerModule":"memory","executionKind":"memory","phase":"maintenance","loopStage":"module_event","outcome":"success","metadata":{"phase":"maintenance_completed"}}"#).unwrap()),
                    Err(error) => {
                        let mut input = parse_json(r#"{"module":"memory","ownerModule":"memory","executionKind":"memory","phase":"maintenance","loopStage":"loop_end","errorCategory":"loop_error"}"#).unwrap().as_object().unwrap().clone();
                        input.insert("code".into(), JsValue::String(error.name.clone().into()));
                        inner.telemetry.track_error(&error, JsValue::Object(input));
                        let mut message: Vec<u16> = "[pilotdeck] memory maintenance failed for project ".encode_utf16().collect();
                        message.extend_from_slice(runtime.project_root().units()); message.push(b':' as u16);
                        (inner.diagnostic)(&JsString::from_utf16(message), &error);
                    }
                }
                if std::mem::take(&mut state.lock().unwrap().requested) { next = invocation(service.clone()); continue; }
                // Promise finally is a separate reaction. Requests during this
                // window remain attached to this runtime; never resolve again.
                #[cfg(test)]
                { let hook = inner.settling.lock().unwrap().take(); if let Some(hook) = hook { hook(); } }
                tokio::task::yield_now().await;
                let mut state = state.lock().unwrap();
                if !std::mem::take(&mut state.requested) {
                    state.in_flight = false;
                    // Disarm under the same lock: an old guard must not clear a new flight.
                    flight.0 = None;
                    break;
                }
                next = invocation(service.clone());
            }
        });
        Ok(())
    }
}
fn panic_error(payload: Box<dyn std::any::Any + Send>) -> MemoryProviderError {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("memory maintenance panicked");
    MemoryProviderError::new(message)
}
fn invocation(service: Arc<dyn MemoryMaintenanceService>) -> MemoryFuture<'static, ()> {
    Box::pin(async move { service.run_due_scheduled_maintenance("scheduled").await })
}
impl<R: MaintenanceRuntime> MemoryMaintenance for ProjectMemoryMaintenanceController<R> {
    fn schedule(&self, key: &JsString) -> RuntimeResult {
        self.schedule(Some(key))
    }
}
#[cfg(test)]
mod tests;
