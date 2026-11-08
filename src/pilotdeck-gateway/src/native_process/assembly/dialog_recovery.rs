//! Native callback for createLocalGateway's clearRecoveredUserDialogs. It runs
//! in the existing admitted-preflight window before any refresh/session stage.
mod recovery;
use super::sdk_adapter::NativeSdkPort;
use crate::runtime::turn::{
    admission::SubmitDialogRecovery,
    submit::{SubmitError, SubmitFuture},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

pub(crate) struct NativeDialogRecovery {
    cache: Arc<std::sync::Mutex<std::collections::HashMap<(PathBuf, JsString), Vec<JsValue>>>>,
    flights: std::sync::Mutex<
        std::collections::HashMap<(PathBuf, JsString, JsString), recovery::RecoveryFlight>,
    >,
    home: PathBuf,
    fallback: PathBuf,
    sdk: Arc<NativeSdkPort>,
}
impl NativeDialogRecovery {
    pub(super) fn new(home: PathBuf, fallback: PathBuf, sdk: Arc<NativeSdkPort>) -> Self {
        Self {
            cache: Arc::default(),
            flights: Default::default(),
            home,
            fallback,
            sdk,
        }
    }
    fn clear<'a>(
        &'a self,
        session: &'a JsString,
        project: JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        Box::pin(async move {
            let home = self.home.clone();
            let root = blocking(move || resolve_registered(&home, &project)).await?;
            // TS resolves the dialog project before the ephemeral-session check.
            if self.sdk.registry.lock().await.is_ephemeral_session(session) {
                return Ok(());
            }
            let home = self.home.clone();
            let session = session.clone();
            let cache = self.cache.clone();
            let resolve_home = home.clone();
            let resolve_root = root.clone();
            let directory = blocking(move || {
                super::super::methods::session_dir::bounded_chat_dir(&resolve_home, &resolve_root)
                    .map_err(|error| {
                        SubmitError::new(
                            &error.code,
                            String::from_utf16_lossy(error.message.units()),
                        )
                    })
            })
            .await?;
            let transcript = directory.join(format!(
                "{}.jsonl",
                crate::storage::sanitize_session_id(&session)
            ));
            let fence = crate::runtime::session::transcript_locks::acquire(&transcript)
                .await
                .map_err(|error| {
                    SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
                })?;
            blocking(move || {
                // A recovered answer keeps this same fence until both durable
                // appends and journal removal complete, even after disconnect.
                let _fence = fence;
                clear_journal_at(&home, &directory, &session)?;
                cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(root, session));
                Ok(())
            })
            .await
        })
    }
}
impl SubmitDialogRecovery for NativeDialogRecovery {
    fn clear_recovered<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let project = project
            .cloned()
            .unwrap_or_else(|| self.fallback.to_string_lossy().into_owned().into());
        self.clear(session, JsValue::String(project))
    }
    fn clear_recovered_input<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        match input
            .get("projectKey")
            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
        {
            Some(raw) => self.clear(session, raw.clone()),
            None => self.clear_recovered(session, project),
        }
    }
}
fn failure(message: impl Into<String>) -> SubmitError {
    SubmitError::new("gateway_request_failed", message)
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SubmitError> + Send + 'static,
) -> Result<T, SubmitError> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| failure(error.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|error| failure(error.to_string()))?
}
fn resolve_registered(home: &Path, project: &JsValue) -> Result<PathBuf, SubmitError> {
    super::super::project_scope::registered(home, project).map_err(|error| {
        SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
    })
}

fn io_failure(error: std::io::Error, operation: &str, path: &Path) -> SubmitError {
    let message =
        crate::node_io::diagnostic(&error, operation, Some(path.to_string_lossy().as_ref()))
            .map(|(_, message)| message)
            .unwrap_or_else(|| error.to_string());
    failure(message)
}
#[cfg(test)]
fn clear_journal(home: &Path, project: &Path, session: &JsString) -> Result<(), SubmitError> {
    let directory =
        super::super::methods::session_dir::bounded_chat_dir(home, project).map_err(|error| {
            SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
        })?;
    clear_journal_at(home, &directory, session)
}
fn clear_journal_at(home: &Path, directory: &Path, session: &JsString) -> Result<(), SubmitError> {
    let name = format!(
        "{}.jsonl.dialogs.json",
        crate::storage::sanitize_session_id(session)
    );
    let path = directory.join(&name);
    #[cfg(unix)]
    {
        // Journal.clear is unlink-only, never reads possibly hostile content.
        // Pin its existing parent under the isolated home; missing parents are
        // the same no-op as unlink(ENOENT), without making a new chat directory.
        let parent = match crate::project_files::open_directory_beneath(home, &directory) {
            Ok(parent) => parent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_failure(error, "open", &directory)),
        };
        match rustix::fs::unlinkat(&parent, name.as_str(), rustix::fs::AtFlags::empty()) {
            Ok(()) => Ok(()),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
            Err(error) => Err(io_failure(error.into(), "unlink", &path)),
        }
    }
    #[cfg(not(unix))]
    {
        // Preserve the existing journal implementation on other platforms;
        // the descriptor-relative native guarantee here is Unix-only.
        crate::user_dialog::GatewayUserDialogJournal::new(
            path,
            crate::user_dialog::JournalOptions::default(),
        )
        .clear()
        .map_err(|error| failure(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn new_submit_cannot_clear_a_recovery_journal_while_its_writer_is_fenced() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let port = recovery(&requests);
        let session: JsString = "fenced-recovery".into();
        let project: JsString = requests
            .options
            .project
            .to_string_lossy()
            .into_owned()
            .into();
        let directory = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &requests.options.project,
        )
        .unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let journal = directory.join("fenced-recovery.jsonl.dialogs.json");
        std::fs::write(&journal, "pending recovery").unwrap();
        let fence = crate::runtime::session::transcript_locks::acquire(
            &directory.join("fenced-recovery.jsonl"),
        )
        .await
        .unwrap();
        let mut clear = port.clear_recovered(&session, Some(&project));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut clear)
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&journal).unwrap(),
            "pending recovery"
        );
        drop(fence);
        tokio::time::timeout(std::time::Duration::from_secs(2), clear)
            .await
            .unwrap()
            .unwrap();
        assert!(!journal.exists());
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    pub(super) fn recovery(
        requests: &crate::native_process::requests::Requests,
    ) -> NativeDialogRecovery {
        let options = crate::runtime::sdk_session::SdkSessionOptions::new(
            requests.options.project.clone(),
            requests.options.project.clone(),
            requests.env.clone(),
            Arc::new(super::super::sdk_adapter::NativeSdkResolver {
                project: requests.project.clone(),
            }),
        );
        let sdk = Arc::new(NativeSdkPort {
            registry: tokio::sync::Mutex::new(
                crate::runtime::sdk_session::SdkSessionRegistry::new(options),
            ),
            handlers: Arc::default(),
            async_hooks: Arc::default(),
            mcp_config: Arc::default(),
        });
        NativeDialogRecovery::new(
            requests.options.home.clone(),
            requests.options.project.clone(),
            sdk,
        )
    }
    #[tokio::test]
    async fn native_recovery_port_preserves_raw_unknown_and_argument_errors_before_factory() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let port = recovery(&requests);
        let canonical = JsString::from(requests.options.project.to_string_lossy().into_owned());
        let session = JsString::from("raw-input");
        let raw = crate::metadata::object([("projectKey", "../unknown-project-recovery".into())]);
        let error = port
            .clear_recovered_input(&session, Some(&canonical), &raw)
            .await
            .unwrap_err();
        assert_eq!(error.code, "PROJECT_NOT_FOUND");
        assert_eq!(
            error.message,
            "Unknown projectKey: ../unknown-project-recovery"
        );
        let invalid = crate::metadata::object([("projectKey", JsValue::Number(7.0))]);
        let error = port
            .clear_recovered_input(&session, Some(&canonical), &invalid)
            .await
            .unwrap_err();
        assert_eq!(error.code, "ERR_INVALID_ARG_TYPE");
        assert_eq!(
            error.message,
            "The \"paths[0]\" argument must be of type string. Received type number (7)"
        );
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn submit_registration_refusal_does_not_stage_or_create_unknown_project_files() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let b = requests
            .options
            .project
            .parent()
            .unwrap()
            .join("recovery-unregistered");
        std::fs::create_dir_all(&b).unwrap();
        let port = recovery(&requests);
        let key = JsString::from(b.to_string_lossy().into_owned());
        let error = port
            .clear_recovered(&"cold-unknown".into(), Some(&key))
            .await
            .unwrap_err();
        assert_eq!(error.code, "PROJECT_NOT_FOUND");
        assert_eq!(
            error.message,
            format!("Unknown projectKey: {}", b.display())
        );
        assert!(!b.join(".pilotdeck").exists());
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn recovered_dialog_cleanup_deletes_only_selected_journal_and_keeps_transcripts() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("recovery-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut options = requests.options.clone();
        options.project = b.clone();
        super::super::register_project(&options).unwrap();
        let selected =
            resolve_registered(&options.home, &JsValue::from(b.to_str().unwrap())).unwrap();
        assert_eq!(selected, b);
        let a_dir =
            crate::native_process::methods::session_dir::bounded_chat_dir(&options.home, &a)
                .unwrap();
        let b_dir =
            crate::native_process::methods::session_dir::bounded_chat_dir(&options.home, &b)
                .unwrap();
        for (dir, label) in [(&a_dir, "A"), (&b_dir, "B")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("same.jsonl"), label).unwrap();
            std::fs::write(
                dir.join("same.jsonl.dialogs.json"),
                "invalid journal is removed without parsing",
            )
            .unwrap();
        }
        clear_journal(&options.home, &selected, &"same".into()).unwrap();
        assert!(!b_dir.join("same.jsonl.dialogs.json").exists());
        assert!(a_dir.join("same.jsonl.dialogs.json").exists());
        assert_eq!(std::fs::read(b_dir.join("same.jsonl")).unwrap(), b"B");
        assert_eq!(std::fs::read(a_dir.join("same.jsonl")).unwrap(), b"A");
        clear_journal(&options.home, &selected, &"missing".into()).unwrap();
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn recovered_dialog_cleanup_unlinks_journal_symlink_without_touching_target() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let project = requests.options.project.clone();
        let dir = crate::native_process::methods::session_dir::bounded_chat_dir(
            &requests.options.home,
            &project,
        )
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let victim = project.join("victim");
        std::fs::write(&victim, "KEEP").unwrap();
        let journal = dir.join("same.jsonl.dialogs.json");
        std::os::unix::fs::symlink(&victim, &journal).unwrap();
        clear_journal(&requests.options.home, &project, &"same".into()).unwrap();
        assert!(std::fs::symlink_metadata(journal).is_err());
        assert_eq!(std::fs::read(victim).unwrap(), b"KEEP");
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
