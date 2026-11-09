//! B publication identities feed daqi's existing maintenance supervisor.
use super::{
    ProjectMemoryMaintenanceController, ProjectMemoryMaintenanceRuntime,
    maintenance::{Diagnostic, OptionalMaintenanceTelemetry},
};
use crate::runtime::{project::RuntimeResult, refresh::MemoryMaintenance};
use pilotdeck_protocol::JsString;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

pub type DiagnosticCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct GenerationScopedMaintenanceCoordinator {
    controller: ProjectMemoryMaintenanceController<ProjectMemoryMaintenanceRuntime>,
}

impl GenerationScopedMaintenanceCoordinator {
    pub fn new(
        resolve_runtime: Arc<dyn Fn(&JsString) -> ProjectMemoryMaintenanceRuntime + Send + Sync>,
        on_diagnostic: Option<DiagnosticCallback>,
    ) -> Self {
        let publications = Mutex::new(HashMap::<
            (String, usize),
            Weak<ProjectMemoryMaintenanceRuntime>,
        >::new());
        let resolve = Arc::new(move |key: Option<&JsString>| {
            let publication = resolve_runtime(key.unwrap_or(&JsString::default()));
            let identity = (publication.project_root.clone(), publication.generation_id);
            let mut publications = publications.lock().unwrap();
            publications.retain(|_, runtime| runtime.strong_count() > 0);
            if let Some(runtime) = publications.get(&identity).and_then(Weak::upgrade) {
                return Ok(runtime);
            }
            let runtime = Arc::new(publication);
            publications.insert(identity, Arc::downgrade(&runtime));
            Ok(runtime)
        });
        let diagnostic: Diagnostic = Arc::new(move |message, error| {
            if let Some(callback) = &on_diagnostic {
                callback(
                    &String::from_utf16_lossy(message.units()),
                    &String::from_utf16_lossy(error.message.units()),
                );
            }
        });
        Self {
            controller: ProjectMemoryMaintenanceController::new(
                resolve,
                Arc::new(OptionalMaintenanceTelemetry),
                Some(diagnostic),
            ),
        }
    }
}

impl MemoryMaintenance for GenerationScopedMaintenanceCoordinator {
    fn schedule(&self, project_key: &JsString) -> RuntimeResult {
        self.controller.schedule(Some(project_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::memory::{MemoryFuture, MemoryMaintenanceService};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, Semaphore};

    struct Service {
        calls: AtomicUsize,
        started: Notify,
        release: Semaphore,
    }
    impl Default for Service {
        fn default() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                started: Notify::new(),
                release: Semaphore::new(0),
            }
        }
    }
    impl MemoryMaintenanceService for Service {
        fn run_due_scheduled_maintenance(&self, _: &str) -> MemoryFuture<'_, ()> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                self.release.acquire().await.unwrap().forget();
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn published_generation_merges_requests_without_aliasing_a_replacement() {
        let first = Arc::new(Service::default());
        let replacement = Arc::new(Service::default());
        let current = Arc::new(Mutex::new((1usize, first.clone())));
        let coordinator = GenerationScopedMaintenanceCoordinator::new(
            Arc::new({
                let current = current.clone();
                move |_| {
                    let current = current.lock().unwrap();
                    ProjectMemoryMaintenanceRuntime {
                        project_root: "project".into(),
                        generation_id: current.0,
                        memory_service: Some(current.1.clone()),
                    }
                }
            }),
            None,
        );
        coordinator.schedule(&"project".into()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), first.started.notified())
            .await
            .unwrap();
        coordinator.schedule(&"project".into()).unwrap();
        coordinator.schedule(&"project".into()).unwrap();
        assert_eq!(first.calls.load(Ordering::SeqCst), 1);
        *current.lock().unwrap() = (2, replacement.clone());
        coordinator.schedule(&"project".into()).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            replacement.started.notified(),
        )
        .await
        .unwrap();
        assert_eq!(replacement.calls.load(Ordering::SeqCst), 1);
        first.release.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), first.started.notified())
            .await
            .unwrap();
        assert_eq!(first.calls.load(Ordering::SeqCst), 2);
        first.release.add_permits(1);
        replacement.release.add_permits(1);
    }

    #[tokio::test]
    async fn timer_requires_a_model_and_stop_keeps_started_work_alive() {
        let service = Arc::new(Service::default());
        let controller =
            ProjectMemoryMaintenanceController::<ProjectMemoryMaintenanceRuntime>::default();
        controller.start(Some(service.clone()), true, false);
        assert_eq!(service.calls.load(Ordering::SeqCst), 0);
        controller.start(Some(service.clone()), true, true);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.started.notified(),
        )
        .await
        .unwrap();
        controller.stop();
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
        service.release.add_permits(1);
    }
}
