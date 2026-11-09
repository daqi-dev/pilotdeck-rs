//! B dc123d18 web-tier memory scheduler, kept as its own module.
//!
//! B declared this inside memory/mod.rs (types at :242-251, impl at :253-477).
//! daqi already has the maintenance trait and the EdgeClaw runtime, so this
//! copies the same controller into one module and re-exports the two public
//! names. Nothing here is a stub: the cycle runs real scheduled maintenance on
//! real runtimes, and the no-op service exists only for the open-failure path.
use super::{MemoryFuture, MemoryMaintenanceService};
use crate::runtime::memory::edgeclaw::{
    NativeEdgeClawOptions, NativeEdgeClawRuntime, read_workspace_dir_from_data_dir,
};

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
/// Default no-op maintenance service for web-tier scheduler fallback.
#[derive(Default, Clone)]
pub struct DefaultWebMemoryMaintenanceService;
impl MemoryMaintenanceService for DefaultWebMemoryMaintenanceService {
    fn run_due_scheduled_maintenance(&self, _reason: &str) -> MemoryFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }
}

/// Web-tier process-level memory scheduler lifecycle controller.
/// Ports `ui/server/services/memoryService.js:324-483` and `pilotdeckConfigReloader.js:17-23`.
#[derive(Clone, Default)]
pub struct WebMemoryScheduler {
    workspace_root: Option<PathBuf>,
    pilot_home: Option<PathBuf>,
    services: Arc<Mutex<std::collections::HashMap<PathBuf, Arc<dyn MemoryMaintenanceService>>>>,
    cycle_running: Arc<std::sync::atomic::AtomicBool>,
    timer: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    llm: Arc<
        std::sync::RwLock<Option<Arc<dyn crate::runtime::memory::edgeclaw::NativeEdgeClawLlm>>>,
    >,
}

impl WebMemoryScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_workspace_root(workspace_root: PathBuf) -> Self {
        let pilot_home = workspace_root.parent().map(Path::to_path_buf);
        Self {
            workspace_root: Some(workspace_root),
            pilot_home,
            services: Arc::new(Mutex::new(std::collections::HashMap::new())),
            cycle_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            timer: Arc::new(Mutex::new(None)),
            llm: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    pub fn with_roots(workspace_root: PathBuf, pilot_home: PathBuf) -> Self {
        Self {
            workspace_root: Some(workspace_root),
            pilot_home: Some(pilot_home),
            services: Arc::new(Mutex::new(std::collections::HashMap::new())),
            cycle_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            timer: Arc::new(Mutex::new(None)),
            llm: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    pub fn set_llm(&self, llm: Arc<dyn crate::runtime::memory::edgeclaw::NativeEdgeClawLlm>) {
        let mut guard = self.llm.write().unwrap();
        *guard = Some(llm);
    }

    pub fn register_service(&self, path: PathBuf, service: Arc<dyn MemoryMaintenanceService>) {
        let mut services = self.services.lock().unwrap();
        services.insert(path, service);
    }

    /// Aligns with `ui/server/services/memoryService.js:324-340`: list dirs with both control.sqlite and memory.
    pub fn list_workspace_data_dirs(&self) -> Vec<PathBuf> {
        let mut search_roots = Vec::new();
        if let Some(root) = &self.workspace_root {
            search_roots.push(root.clone());
            if let Some(parent) = root.parent() {
                let memory_workspaces = parent.join("memory").join("workspaces");
                if memory_workspaces.exists() && !search_roots.contains(&memory_workspaces) {
                    search_roots.push(memory_workspaces);
                }
            }
        }
        if let Some(home) = &self.pilot_home {
            let memory_workspaces = home.join("memory").join("workspaces");
            if memory_workspaces.exists() && !search_roots.contains(&memory_workspaces) {
                search_roots.push(memory_workspaces);
            }
        }

        let mut dirs = Vec::new();
        for root in search_roots {
            let Ok(read_dir) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in read_dir.flatten() {
                if let Ok(file_type) = entry.file_type()
                    && file_type.is_dir()
                {
                    let path = entry.path();
                    let has_db = path.join("control.sqlite").exists();
                    let has_mem = path.join("memory").is_dir();
                    if has_db && has_mem && !dirs.contains(&path) {
                        dirs.push(path);
                    }
                }
            }
        }
        dirs.sort();
        dirs
    }

    /// Aligns with `ui/server/services/memoryService.js:343-345` (getOrCreateServiceForDataDir).
    /// Instantiates real NativeEdgeClawRuntime with persisted workspaceDir from control.sqlite.
    pub fn get_or_create_service_for_data_dir(
        &self,
        data_dir: &Path,
    ) -> Arc<dyn MemoryMaintenanceService> {
        {
            let services = self.services.lock().unwrap();
            if let Some(svc) = services.get(data_dir) {
                return Arc::clone(svc);
            }
        }

        let workspace_dir =
            crate::runtime::memory::edgeclaw::read_workspace_dir_from_data_dir(data_dir)
                .unwrap_or_else(|| data_dir.to_path_buf());
        let pilot_home = self.pilot_home.clone().unwrap_or_else(|| {
            self.workspace_root
                .as_ref()
                .and_then(|p| p.parent())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });
        let root_dir = pilot_home.join("memory");
        let llm = self.llm.read().unwrap().clone();

        let mut options =
            crate::runtime::memory::edgeclaw::NativeEdgeClawOptions::new(root_dir, &pilot_home);
        if let Some(llm) = llm {
            options = options.with_llm(llm);
        }

        match crate::runtime::memory::edgeclaw::NativeEdgeClawRuntime::open(&workspace_dir, options)
        {
            Ok(runtime) => {
                let service: Arc<dyn MemoryMaintenanceService> = Arc::new(runtime);
                let mut services = self.services.lock().unwrap();
                services.insert(data_dir.to_path_buf(), Arc::clone(&service));
                service
            }
            Err(e) => {
                eprintln!(
                    "[memory-scheduler] failed to open edgeclaw runtime for {}: {e:?}",
                    data_dir.display()
                );
                Arc::new(DefaultWebMemoryMaintenanceService)
            }
        }
    }

    /// Aligns with `ui/server/services/memoryService.js:393-421`: run one cycle, deduplicate in-flight, continue on single failure.
    pub async fn run_memory_scheduler_cycle(&self) {
        if self
            .cycle_running
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let dirs = self.list_workspace_data_dirs();
        for dir in dirs {
            let service = self.get_or_create_service_for_data_dir(&dir);
            if let Err(e) = service
                .run_due_scheduled_maintenance("scheduled:server_scheduler")
                .await
            {
                eprintln!(
                    "[memory-scheduler] scheduled maintenance failed for {}: {e:?}",
                    dir.display()
                );
            }
        }
        self.cycle_running
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub fn start_memory_scheduler(
        &self,
        service: impl Into<Option<Arc<dyn MemoryMaintenanceService>>>,
        enabled: bool,
        has_model: bool,
    ) {
        let service = service.into();
        if !enabled || !has_model {
            return;
        }
        let mut lock = self.timer.lock().unwrap();
        // If timer already exists, return directly without restarting (aligns with memoryService.js:433-435)
        if lock.is_some() {
            return;
        }

        // If an explicit service is passed (e.g. in tests or single service mount), run immediate cycle for it
        if let Some(svc) = &service {
            spawn_maintenance_cycle(Arc::clone(svc), "scheduled:web_immediate");
            if let Some(root) = &self.workspace_root {
                self.register_service(root.clone(), Arc::clone(svc));
            }
        }

        // Run one cycle immediately in background (aligns with memoryService.js:445)
        let this_immediate = self.clone();
        tokio::spawn(async move {
            this_immediate.run_memory_scheduler_cycle().await;
        });

        let this_interval = self.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                this_interval.run_memory_scheduler_cycle().await;
            }
        });
        *lock = Some(task);
    }

    pub fn stop_memory_scheduler(&self) {
        if let Some(timer) = self.timer.lock().unwrap().take() {
            timer.abort();
        }
    }

    pub fn close_memory_services<F: FnOnce()>(&self, close_fn: F) {
        self.stop_memory_scheduler();
        self.services.lock().unwrap().clear();
        close_fn();
    }

    pub fn on_config_reload<F: FnOnce()>(
        &self,
        close_fn: F,
        service: Option<Arc<dyn MemoryMaintenanceService>>,
        enabled: bool,
        has_model: bool,
    ) {
        self.close_memory_services(close_fn);
        if enabled && has_model {
            self.start_memory_scheduler(service, true, true);
        }
    }
}

fn spawn_maintenance_cycle(service: Arc<dyn MemoryMaintenanceService>, reason: &'static str) {
    tokio::spawn(async move {
        let _ = service.run_due_scheduled_maintenance(reason).await;
    });
}
