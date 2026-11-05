use super::{
    Options, PersistenceResources, ProjectResources, ProjectRuntimeRegistry, ProxyError, Resource,
    RuntimeError,
};
use crate::storage::ContentAddressedWorkspaceSnapshotRecorder;
use std::sync::Arc;

// Validate real families before claiming readiness. Partial staging belongs
// to this registry and must drain even when construction returns an error.
pub(super) async fn snapshot_enabled(
    options: &Options,
    project: &Arc<ProjectRuntimeRegistry>,
) -> Result<bool, ProxyError> {
    let snapshot_enabled = match project.acquire(None).and_then(|lease| {
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
        let persistence = resources
            .context()
            .resource(Resource::Persistence)
            .ok_or_else(|| RuntimeError::new("Native persistence resource is missing."))?;
        let persistence = persistence
            .as_any()
            .downcast_ref::<PersistenceResources>()
            .ok_or_else(|| RuntimeError::new("Native persistence resource has the wrong type."))?;
        // Staging already resolved the immutable environment to build this
        // provider. Advertise that result instead of resolving it per request.
        Ok(persistence.snapshots.is_some())
    }) {
        Ok(enabled) => enabled,
        Err(error) => {
            let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
            return Err(error.into());
        }
    };
    Ok(snapshot_enabled)
}

// Extract the snapshot recorder once, so `Services` can hand the same
// provider to every submit turn without re-acquiring the project lease.
pub(super) async fn snapshot_recorder(
    options: &Options,
    project: &Arc<ProjectRuntimeRegistry>,
    snapshot_enabled: bool,
) -> Result<Option<Arc<ContentAddressedWorkspaceSnapshotRecorder>>, ProxyError> {
    let snapshot_recorder = if snapshot_enabled {
        match project.acquire(None).and_then(|lease| {
            let resources = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| RuntimeError::new("Native project resources are missing."))?;
            let persistence = resources
                .context()
                .resource(Resource::Persistence)
                .ok_or_else(|| RuntimeError::new("Native persistence resource is missing."))?;
            let persistence = persistence
                .as_any()
                .downcast_ref::<PersistenceResources>()
                .ok_or_else(|| {
                    RuntimeError::new("Native persistence resource has the wrong type.")
                })?;
            persistence
                .snapshots
                .as_ref()
                .map(|snapshots| snapshots.recorder.clone())
                .ok_or_else(|| RuntimeError::new("Native snapshot recorder is missing."))
        }) {
            Ok(recorder) => Some(recorder),
            Err(error) => {
                let _ = tokio::time::timeout(options.drain_budget, project.dispose()).await;
                return Err(error.into());
            }
        }
    } else {
        None
    };
    Ok(snapshot_recorder)
}
