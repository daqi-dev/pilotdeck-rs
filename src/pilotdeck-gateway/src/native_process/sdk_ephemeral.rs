//! Gateway-owned SDK temporary session storage, matching ProjectRuntimeRegistry.
//! No extra session state machine: the normal RecordingPlan owns the writer.
use crate::runtime::project::RuntimeError;
use pilotdeck_protocol::JsString;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) struct TemporaryRoot {
    path: PathBuf,
    identity: File,
}
impl TemporaryRoot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    fn create() -> Result<Self, RuntimeError> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let path =
            std::env::temp_dir().join(format!("pilotdeck-sdk-ephemeral-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(io_error)?;
        let result = (|| {
            let identity = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(
                    rustix::fs::OFlags::NOFOLLOW.bits() as i32
                        | rustix::fs::OFlags::DIRECTORY.bits() as i32,
                )
                .open(&path)
                .map_err(io_error)?;
            let root = Self {
                path: std::fs::canonicalize(&path).map_err(io_error)?,
                identity,
            };
            root.verify()?;
            Ok(root)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir(&path);
        }
        result
    }
    pub(crate) fn verify(&self) -> Result<(), RuntimeError> {
        use std::os::unix::fs::MetadataExt;
        let path = std::fs::symlink_metadata(&self.path).map_err(io_error)?;
        let held = self.identity.metadata().map_err(io_error)?;
        if !path.is_dir()
            || path.file_type().is_symlink()
            || path.dev() != held.dev()
            || path.ino() != held.ino()
        {
            return Err(RuntimeError::new(
                "SDK temporary session directory identity changed.",
            ));
        }
        Ok(())
    }
    fn remove(&self) -> Result<(), RuntimeError> {
        self.verify()?;
        std::fs::remove_dir_all(&self.path).map_err(io_error)
    }
}
fn io_error(error: std::io::Error) -> RuntimeError {
    RuntimeError::new(error.to_string())
}
const MAX_IDENTITIES: usize = 4096;
const MAX_IDENTITY_UNITS: usize = 1024 * 1024;
#[derive(Default)]
struct State {
    closed: bool,
    roots: HashMap<JsString, Arc<TemporaryRoot>>,
    deleted: HashSet<JsString>,
}
type Shutdown = Arc<
    dyn Fn() -> crate::runtime::refresh::BoxFuture<crate::runtime::project::RuntimeResult>
        + Send
        + Sync,
>;
#[derive(Default)]
pub(crate) struct EphemeralSessions {
    state: Arc<Mutex<State>>,
    gate: Arc<tokio::sync::Mutex<()>>,
    shutdown: OnceLock<Shutdown>,
}
impl EphemeralSessions {
    pub(crate) fn bind_shutdown(&self, shutdown: Shutdown) -> Result<(), RuntimeError> {
        self.shutdown
            .set(shutdown)
            .map_err(|_| RuntimeError::new("SDK temporary storage shutdown was already bound."))
    }
    pub(crate) async fn acquire(
        &self,
        session: JsString,
    ) -> Result<Arc<TemporaryRoot>, RuntimeError> {
        let gate = self.gate.clone().lock_owned().await;
        let permit = super::methods::session_dir::session_io_permit()
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message.units())))?;
        let state = self.state.clone();
        // Admission and insertion finish in the owned blocking job even when
        // its async waiter disappears; host cleanup can still see the root.
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            let _permit = permit;
            {
                let state = state.lock().unwrap_or_else(|e| e.into_inner());
                if state.closed {
                    return Err(RuntimeError::new("SDK temporary storage is closed."));
                }
                if let Some(root) = state.roots.get(&session) {
                    return Ok(root.clone());
                }
                if !state.deleted.contains(&session) {
                    let units = state
                        .roots
                        .keys()
                        .chain(state.deleted.iter())
                        .fold(0usize, |sum, key| sum.saturating_add(key.units().len()));
                    if state.roots.len().saturating_add(state.deleted.len()) >= MAX_IDENTITIES
                        || units.saturating_add(session.units().len()) > MAX_IDENTITY_UNITS
                    {
                        return Err(RuntimeError::new(
                            "SDK temporary session identity budget exceeded.",
                        ));
                    }
                }
            }
            let root = Arc::new(TemporaryRoot::create()?);
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.deleted.remove(&session);
            state.roots.insert(session, root.clone());
            Ok(root)
        })
        .await
        .map_err(|e| RuntimeError::new(e.to_string()))?
    }
    /// Caller must have drained the real SessionOwner (including children).
    pub(crate) async fn delete(&self, session: JsString) -> Result<bool, RuntimeError> {
        let gate = self.gate.clone().lock_owned().await;
        let permit = super::methods::session_dir::session_io_permit()
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message.units())))?;
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            let _permit = permit;
            let root = state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .roots
                .get(&session)
                .cloned();
            let Some(root) = root else {
                return Ok(state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .deleted
                    .contains(&session));
            };
            root.remove()?;
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.roots.remove(&session);
            state.deleted.insert(session);
            Ok(true)
        })
        .await
        .map_err(|e| RuntimeError::new(e.to_string()))?
    }
    /// Only a successfully opened new durable recording retires a prior
    /// temporary deletion identity. This runs under the owner's same-key ticket.
    pub(crate) fn durable_storage(
        self: &Arc<Self>,
        storage: crate::runtime::session::factory::CreateStorage,
    ) -> crate::runtime::session::factory::CreateStorage {
        let owner = self.clone();
        Arc::new(move |context| {
            let opened = storage(context.clone());
            let owner = owner.clone();
            Box::pin(async move {
                let mut result = opened.await?;
                let crate::runtime::session::factory::SessionStorage::Recording(plan) = &mut result
                else {
                    return Err(RuntimeError::new(
                        "Native durable admission requires recording storage.",
                    ));
                };
                let open = plan.open.clone();
                let session = context.session_key;
                plan.open = Arc::new(move || {
                    let recording = open()?;
                    owner
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .deleted
                        .remove(&session);
                    Ok(recording)
                });
                Ok(result)
            })
        })
    }
    /// Owner shutdown precedes this barrier; a failed remove remains retryable.
    pub(crate) async fn close(&self) -> Result<(), RuntimeError> {
        if let Some(shutdown) = self.shutdown.get() {
            shutdown().await?;
        }
        let gate = self.gate.clone().lock_owned().await;
        let permit = super::methods::session_dir::session_io_permit()
            .await
            .map_err(|e| RuntimeError::new(String::from_utf16_lossy(e.message.units())))?;
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            let _permit = permit;
            let roots = {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.closed = true;
                state
                    .roots
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            };
            let mut errors = Vec::new();
            for (session, root) in roots {
                match root.remove() {
                    Ok(()) => {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .roots
                            .remove(&session);
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(RuntimeError::new(errors.join("; ")))
            }
        })
        .await
        .map_err(|e| RuntimeError::new(e.to_string()))?
    }
}

#[cfg(test)]
mod identity_budget_tests {
    use super::*;
    #[tokio::test]
    async fn deletion_identities_are_bounded_and_never_evicted_to_admit_new_keys() {
        let sessions = EphemeralSessions::default();
        for i in 0..MAX_IDENTITIES - 1 {
            sessions
                .state
                .lock()
                .unwrap()
                .deleted
                .insert(format!("old-{i}").as_str().into());
        }
        let last = sessions.acquire("last".into()).await.unwrap();
        assert!(
            sessions
                .acquire("overflow".into())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("identity budget")
        );
        sessions.delete("last".into()).await.unwrap();
        assert!(!last.path().exists());
        // A deleted identity is still owned; repeated deletes never fall back.
        assert!(sessions.delete("old-0".into()).await.unwrap());
        let reopened = sessions.acquire("old-0".into()).await.unwrap();
        assert!(reopened.path().exists());
        {
            let state = sessions.state.lock().unwrap();
            assert_eq!(state.roots.len() + state.deleted.len(), MAX_IDENTITIES);
        }
        sessions.close().await.unwrap();
    }
    #[tokio::test]
    async fn retained_identity_budget_counts_utf16_units() {
        let sessions = EphemeralSessions::default();
        sessions
            .state
            .lock()
            .unwrap()
            .deleted
            .insert(JsString::from_utf16(vec![0xd800; MAX_IDENTITY_UNITS - 1]));
        let root = sessions.acquire("x".into()).await.unwrap();
        assert!(
            sessions
                .acquire("y".into())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("identity budget")
        );
        sessions.close().await.unwrap();
        assert!(!root.path().exists());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn temporary_roots_are_private_retained_and_cleaned_only_after_shutdown() {
        use std::os::unix::fs::MetadataExt;
        let sessions = EphemeralSessions::default();
        let key: JsString = "same".into();
        let first = sessions.acquire(key.clone()).await.unwrap();
        let path = first.path().to_owned();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
        let second = sessions.acquire(key.clone()).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        std::fs::write(path.join("owned.jsonl"), "fixture").unwrap();
        let checked = path.clone();
        sessions
            .bind_shutdown(Arc::new(move || {
                let checked = checked.clone();
                Box::pin(async move {
                    assert!(checked.join("owned.jsonl").exists());
                    Ok(())
                })
            }))
            .unwrap();
        sessions.close().await.unwrap();
        assert!(!path.exists());
        assert!(sessions.acquire(key).await.is_err());
    }
    #[tokio::test]
    async fn failed_drain_or_replaced_root_is_preserved_and_reported() {
        let sessions = EphemeralSessions::default();
        let root = sessions.acquire("drain".into()).await.unwrap();
        let path = root.path().to_owned();
        sessions
            .bind_shutdown(Arc::new(|| {
                Box::pin(async { Err(RuntimeError::new("owned jobs not quiescent")) })
            }))
            .unwrap();
        assert!(sessions.close().await.is_err());
        assert!(path.exists());
        sessions.delete("drain".into()).await.unwrap();
        let sessions = EphemeralSessions::default();
        let root = sessions.acquire("replacement".into()).await.unwrap();
        let path = root.path().to_owned();
        let moved = path.with_extension("held");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("foreign"), "KEEP").unwrap();
        assert!(sessions.delete("replacement".into()).await.is_err());
        assert_eq!(std::fs::read(path.join("foreign")).unwrap(), b"KEEP");
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::rename(&moved, &path).unwrap();
        sessions.close().await.unwrap();
    }
}
