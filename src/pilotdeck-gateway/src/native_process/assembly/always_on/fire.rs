//! Native automation dependencies are staged on the bundle's owned blocking
//! worker. Binding transfers each project lease exactly once to its fire owner.
use std::{collections::HashMap, io, path::PathBuf, sync::{Arc, Mutex}};
use crate::runtime::{
    always_on::{AlwaysOnConfig, DiscoveryFirePort, DiscoveryReportStore, FireFactory,
        FireInputs, NativeFire, NativeWorkspace, event_sink::PhaseEventSink},
    history::HistoryFiles,
    manager::{ManagerError, SessionCatalogListInput, SessionCatalogPort, TranscriptSessionCatalog},
    project::ProjectRuntimeRegistry,
    tools::builtin::bash::runner::NativeOwnedShellCommandRunner,
};

pub(super) struct NativeFireFactory {
    home: PathBuf,
    workspaces: Mutex<HashMap<String, Arc<NativeWorkspace>>>,
    catalog: Arc<dyn SessionCatalogPort>,
    max_frame_bytes: usize,
}
impl NativeFireFactory {
    pub(super) fn stage(home: PathBuf, config: &AlwaysOnConfig,
        registry: &ProjectRuntimeRegistry, env: &crate::config::Env,
        max_frame_bytes: usize) -> io::Result<Arc<Self>> {
        let mut workspaces = HashMap::new();
        for (key, enabled) in &config.projects {
            if !enabled { continue; }
            let paths = super::bounded_paths(&home, key)?;
            let lease = registry.acquire(Some(std::path::Path::new(key)))
                .map_err(|error| io::Error::other(error.to_string()))?;
            let workspace = NativeWorkspace::new(paths, Arc::new(lease),
                Arc::new(NativeOwnedShellCommandRunner),
                Some(env.iter().map(|(k,v)| (k.as_str().into(),v.as_str().into())).collect()))?;
            workspaces.insert(key.clone(), Arc::new(workspace));
        }
        let catalog = Arc::new(TranscriptSessionCatalog::new_bounded(|project| {
            crate::native_process::methods::session_dir::bounded_chat_dir(
                &project.pilot_home, &project.project_root).map_err(|error|
                    ManagerError::with_message("Error", Some(error.code), error.message))
        }));
        Ok(Arc::new(Self { home, workspaces: Mutex::new(workspaces), catalog, max_frame_bytes }))
    }
}
impl FireFactory for NativeFireFactory {
    fn create(&self, input: FireInputs) -> io::Result<Arc<dyn DiscoveryFirePort>> {
        // Leaving a second lease in the factory would keep the registry alive
        // after engine.close(). Retain only the fire owner's lease after bind.
        let workspace = self.workspaces.lock().unwrap_or_else(|e| e.into_inner())
            .remove(&input.project_key).ok_or_else(|| io::Error::other(
                "Always-On project generation was not staged or was already bound."))?;
        let project = PathBuf::from(&input.project_key);
        let sink = Arc::new(PhaseEventSink::new(input.project_key.as_str().into(),
            DiscoveryReportStore::new(input.paths.clone()), None,
            Arc::new(|message| eprintln!("{message}"))));
        let history = HistoryFiles { home: self.home.clone(), project: project.clone() };
        let catalog_input = SessionCatalogListInput::new(project, self.home.clone());
        NativeFire::new(input, workspace, sink, history, self.catalog.clone(),
            catalog_input, self.max_frame_bytes)
    }
}
