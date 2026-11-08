//! Native project-owned detached shell tasks. The execution-world family is
//! the existing rollback/disposal boundary; all sessions and SDK projections
//! clone handles to this one runtime, never create a per-turn task owner.
use crate::runtime::{
    background::BackgroundResources,
    project::{
        Families, Family, Resource, ResourceFamilySource, RuntimeResult, StageContext, StageSink,
    },
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

#[derive(Default)]
struct NativeExecutionWorldFamily {
    archives: Mutex<HashMap<PathBuf, Weak<super::background_archive::NativeArchive>>>,
}
impl ResourceFamilySource for NativeExecutionWorldFamily {
    fn family(&self) -> Family {
        Family::ExecutionWorld
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let env = context.env();
        let temporary_root = env
            .get("TMPDIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let sandbox = Arc::new(crate::runtime::tools::sandbox::NativeSandbox::new(
            context.profile().sandbox_mode,
            temporary_root,
        ));
        let project = context.project_root();
        let archive_project = std::fs::canonicalize(&project)
            .map_err(|error| crate::runtime::project::RuntimeError::new(error.to_string()))?;
        let archive = {
            let mut archives = self.archives.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(archive) = archives.get(&archive_project).and_then(Weak::upgrade) {
                archive
            } else {
                let archive = super::background_archive::NativeArchive::open(&archive_project)
                    .map_err(|error| {
                        crate::runtime::project::RuntimeError::new(error.to_string())
                    })?;
                archives.insert(archive_project, Arc::downgrade(&archive));
                archive
            }
        };
        let store = archive.handle();
        let resource = Arc::new(crate::runtime::background::BackgroundExecutionWorld {
            resources: BackgroundResources {
                tasks: crate::runtime::background::BackgroundTaskRuntime::new_native(
                    crate::runtime::background::BackgroundTaskRuntimeOptions {
                        disk_spill_dir: Some(project.join(".pilotdeck/background-tasks")),
                        snapshot_store: Some(store.clone()),
                        ..Default::default()
                    },
                    sandbox.clone(),
                    store,
                    crate::runtime::background::NativeTaskLimits::CLI,
                ),
                subagents: Default::default(),
            },
            sandbox,
        });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::ExecutionWorld, resource);
        Ok(())
    }
}
impl Families {
    pub(crate) fn with_native_background_tasks(mut self) -> Self {
        self.execution_world = Arc::new(NativeExecutionWorldFamily::default());
        self
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
