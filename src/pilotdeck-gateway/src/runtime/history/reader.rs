//! Read-only JSONL history access. Reuses the native writer's path identity.
use crate::storage::{TranscriptRead, read_transcript_beneath, read_transcript_bounded};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;

pub(super) static READ_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
/// Marker IO for history and the native model-selection metadata adapter.
/// The latter may also read JSONL transcripts; configuration is deliberately
/// loaded by its separate bounded owner. Refusals cannot become lexical fallback.
pub(crate) struct HistoryMarkerFiles {
    refused: std::cell::Cell<bool>,
    allow_transcripts: bool,
}
impl HistoryMarkerFiles {
    pub(crate) fn new() -> Self {
        Self {
            refused: std::cell::Cell::new(false),
            allow_transcripts: false,
        }
    }
    pub(crate) fn for_model_selection() -> Self {
        Self {
            allow_transcripts: true,
            ..Self::new()
        }
    }
    pub(crate) fn was_refused(&self) -> bool {
        self.refused.get()
    }
}
impl crate::metadata::Files for HistoryMarkerFiles {
    fn entries(&self, path: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
        crate::metadata::Files::entries(&crate::metadata::NativeFiles, path)
    }
    fn stat(&self, path: &std::path::Path) -> std::io::Result<crate::metadata::Stat> {
        crate::metadata::Files::stat(&crate::metadata::NativeFiles, path)
    }
    fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let limit =
                if self.allow_transcripts && path.extension().is_some_and(|ext| ext == "jsonl") {
                    crate::storage::TRANSCRIPT_MAX_BYTES
                } else {
                    64 * 1024
                };
            let mut options = fs::OpenOptions::new();
            options.read(true).custom_flags(libc::O_NONBLOCK);
            let file = options.open(path)?;
            let meta = file.metadata()?;
            if !meta.is_file() || meta.len() > limit {
                self.refused.set(true);
                return Err(std::io::Error::other("Unsafe history project marker"));
            }
            let mut bytes = Vec::new();
            file.take(limit + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > limit {
                self.refused.set(true);
                return Err(std::io::Error::other("Unsafe history project marker"));
            }
            Ok(bytes)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            self.refused.set(true);
            Err(std::io::Error::other(
                "Bounded native history is unavailable on this platform",
            ))
        }
    }
}
pub(super) fn history_path(files: &HistoryFiles, session: &JsString) -> Result<PathBuf, String> {
    let lookup = crate::metadata::Metadata {
        home: files.home.clone(),
        cwd: PathBuf::new(),
        files: HistoryMarkerFiles::new(),
    };
    let chat = lookup
        .process_chat_dir(&files.project.to_string_lossy())
        .map_err(|error| error.to_string())?;
    if lookup.files.refused.get() {
        return Err("Unsafe history project marker".into());
    }
    Ok(chat.join(format!(
        "{}.jsonl",
        crate::storage::sanitize_session_id(session)
    )))
}

/// Resolve the TS background-task relative path without allowing traversal.
pub(super) fn background_path(
    input: &JsValue,
    chat: &std::path::Path,
) -> Result<Option<PathBuf>, String> {
    if !input
        .get("sessionKind")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str("background_task"))
    {
        return Ok(None);
    }
    let Some(value) = input
        .get("relativeTranscriptPath")
        .and_then(JsValue::as_string)
    else {
        return Ok(None);
    };
    if value.units().is_empty() {
        return Ok(None);
    }
    let text = value
        .to_utf8()
        .map_err(|_| "relativeTranscriptPath is not valid UTF-8".to_owned())?;
    let path = std::path::Path::new(&text);
    if path.is_absolute() {
        return Err("relativeTranscriptPath must be a relative path.".into());
    }
    let mut relative = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::Normal(name) => relative.push(name),
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir if relative.pop() => (),
            _ => {
                return Err(
                    "relativeTranscriptPath points outside the project transcript directory."
                        .into(),
                );
            }
        }
    }
    if relative.as_os_str().is_empty() || !relative.to_string_lossy().ends_with(".jsonl") {
        return Err(
            "relativeTranscriptPath points outside the project transcript directory.".into(),
        );
    }
    Ok(Some(chat.join(relative)))
}

#[derive(Clone)]
pub struct HistoryFiles {
    pub home: PathBuf,
    pub project: PathBuf,
}
pub struct HistorySnapshot {
    pub read: TranscriptRead,
    pub catalog: Option<JsValue>,
}
impl HistoryFiles {
    /// Read only the first/last 64 KiB from one nonblocking regular-file
    /// descriptor, matching the native TS prompt digest reader.
    pub(crate) async fn prompt_lite(
        &self,
        session: &JsString,
    ) -> Result<Option<(String, String)>, String> {
        let files = self.clone();
        let session = session.clone();
        let permit = READ_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = history_path(&files, &session)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut options = fs::OpenOptions::new();
                options.read(true).custom_flags(libc::O_NONBLOCK);
                let mut file = match options.open(path) {
                    Ok(file) => file,
                    Err(_) => return Ok(None),
                };
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                if !metadata.is_file() {
                    return Err("History transcript is not a regular file".into());
                }
                Ok(crate::runtime::manager::file_catalog::read_lite_chunks(
                    &mut file,
                    metadata.len(),
                )
                .ok()
                .flatten())
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err("Bounded native history is unavailable on this platform".into())
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub async fn read(&self, session: &JsString) -> Result<TranscriptRead, String> {
        Ok(self.snapshot(session).await?.read)
    }
    /// The optional catalog projection is derived from the same opened file's
    /// bounded bytes and metadata; no pathname is reopened to inspect headers.
    pub async fn snapshot(&self, session: &JsString) -> Result<HistorySnapshot, String> {
        let files = self.clone();
        let session = session.clone();
        let project = self.project.clone();
        let safe_id = JsString::from(crate::storage::sanitize_session_id(&session));
        let permit = READ_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = history_path(&files, &session)?;
            let value = read_transcript_bounded(&path).map_err(|e| e.to_string())?;
            let catalog = value.snapshot.as_ref().and_then(|(bytes, meta)| {
                crate::runtime::manager::file_catalog::history_session_info(
                    bytes, meta, &safe_id, &project,
                )
                .ok()
                .flatten()
            });
            Ok(HistorySnapshot {
                read: value.read,
                catalog,
            })
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_lite_reads_same_fd_windows_and_rejects_fifo() {
        let root = std::env::temp_dir().join(format!("pdrs-prompt-lite-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let md = crate::metadata::Metadata {
            home: home.clone(),
            cwd: PathBuf::new(),
            files: crate::metadata::NativeFiles,
        };
        let chat = md.process_chat_dir(project.to_str().unwrap()).unwrap();
        std::fs::create_dir_all(&chat).unwrap();
        let file = chat.join(format!(
            "{}.jsonl",
            crate::storage::sanitize_session_id(&"session".into())
        ));
        let files = HistoryFiles { home, project };
        std::fs::write(&file, b"short").unwrap();
        let (head, tail) = files.prompt_lite(&"session".into()).await.unwrap().unwrap();
        assert_eq!(head, "short");
        assert_eq!(tail, "short");
        let mut bytes = vec![b'x'; 200_000];
        bytes[..5].copy_from_slice(b"HEAD!");
        bytes[100_000..100_006].copy_from_slice(b"MIDDLE");
        let end = bytes.len();
        bytes[end - 5..].copy_from_slice(b"TAIL!");
        std::fs::write(&file, bytes).unwrap();
        let (head, tail) = files.prompt_lite(&"session".into()).await.unwrap().unwrap();
        assert_eq!(head.len(), 65_536);
        assert_eq!(tail.len(), 65_536);
        assert!(head.starts_with("HEAD!") && tail.ends_with("TAIL!"));
        assert!(!head.contains("MIDDLE") && !tail.contains("MIDDLE"));
        std::fs::remove_file(&file).unwrap();
        let result = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&file)
            .status()
            .unwrap();
        assert!(result.success());
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            files.prompt_lite(&"session".into()),
        )
        .await
        .expect("FIFO must not hang");
        assert!(refused.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_path_refuses_escape_and_ignores_normal_sessions() {
        use pilotdeck_protocol::parse_json;
        let root = std::path::Path::new("/tmp/chat");
        for raw in [
            r#"{"sessionKind":"background_task","relativeTranscriptPath":"../else.jsonl"}"#,
            r#"{"sessionKind":"background_task","relativeTranscriptPath":"/tmp/else.jsonl"}"#,
            r#"{"sessionKind":"background_task","relativeTranscriptPath":"x.txt"}"#,
        ] {
            assert!(background_path(&parse_json(raw).unwrap(), root).is_err());
        }
        let input = parse_json(
            r#"{"sessionKind":"background_task","relativeTranscriptPath":"sub/../task.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(
            background_path(&input, root).unwrap(),
            Some(root.join("task.jsonl"))
        );
        let input = parse_json(
            r#"{"sessionKind":"background_task","relativeTranscriptPath":"sub/task.jsonl/"}"#,
        )
        .unwrap();
        assert_eq!(
            background_path(&input, root).unwrap(),
            Some(root.join("sub/task.jsonl"))
        );
        let input = parse_json(r#"{"relativeTranscriptPath":"../else.jsonl"}"#).unwrap();
        assert_eq!(background_path(&input, root).unwrap(), None);
    }
    #[cfg(unix)]
    #[test]
    fn marker_fifo_and_oversize_are_refused() {
        let root =
            std::env::temp_dir().join(format!("pdrs-history-marker-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let project = root.join("project");
        let marker = home.join("projects").join("registered").join(".cwd");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::create_dir_all(&project).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&marker)
                .status()
                .unwrap()
                .success()
        );
        let files = HistoryFiles { home, project };
        assert!(
            history_path(&files, &JsString::from("session"))
                .unwrap_err()
                .contains("Unsafe history project marker")
        );
        fs::remove_file(&marker).unwrap();
        fs::write(&marker, vec![b'x'; 64 * 1024 + 1]).unwrap();
        assert!(
            history_path(&files, &JsString::from("session"))
                .unwrap_err()
                .contains("Unsafe history project marker")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn background_descriptor_reads_selected_file_and_refuses_symlink() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("pdrs-history-background-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("sub")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let path = root.join("sub/task.jsonl");
        fs::write(&path, b"{\"type\":\"unknown\",\"sessionId\":\"task\",\"turnId\":\"t\",\"createdAt\":\"2026-01-01T00:00:00Z\",\"sequence\":1}\n").unwrap();
        let selected = read_transcript_beneath(&root, &path).unwrap();
        assert_eq!(selected.read.entries.len(), 1);
        symlink(&path, root.join("link.jsonl")).unwrap();
        assert!(read_transcript_beneath(&root, &root.join("link.jsonl")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn bounded_read_uses_same_decoder_and_rejects_oversize() {
        let root = std::env::temp_dir().join(format!("pdrs-history-read-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let files = HistoryFiles {
            home: root.join("home"),
            project: root.join("project"),
        };
        let key: JsString = "session".into();
        let path = history_path(&files, &key).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path,b"{\"type\":\"unknown\",\"sessionId\":\"session\",\"turnId\":\"t\",\"createdAt\":\"2026-01-01T00:00:00Z\",\"sequence\":1}\n{bad}\n").unwrap();
        let old = crate::storage::read_transcript(&path).unwrap();
        let read = files.read(&key).await.unwrap();
        assert_eq!(read.entries, old.entries);
        assert_eq!(read.diagnostics, old.diagnostics);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(50 * 1024 * 1024 + 1)
            .unwrap();
        assert!(files.read(&key).await.unwrap_err().contains("50 MiB"));
        fs::remove_dir_all(root).unwrap();
    }
}

/// Native JSONL durable reader. A single permit covers read, catalog projection,
/// message projection and paging so expensive work never runs on the WS runtime.
#[derive(Clone)]
pub struct NativeHistoryReader {
    pub files: HistoryFiles,
    pub project_runtime: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
}
impl crate::runtime::turn::read::SessionHistoryReader for NativeHistoryReader {
    fn read(
        &self,
        input: &JsValue,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<JsValue, crate::runtime::turn::read::TurnReadError>,
                > + Send,
        >,
    > {
        let input = input.clone();
        let files = self.files.clone();
        let project_runtime = self.project_runtime.clone();
        Box::pin(async move {
            let map = |message: String| crate::runtime::turn::read::TurnReadError {
                code: None,
                name: "Error".into(),
                message: message.into(),
            };
            let key = input
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| map("History sessionKey must be a string".into()))?;
            let permit = READ_SLOTS
                .get_or_init(|| Arc::new(Semaphore::new(4)))
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| map(e.to_string()))?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let default_path = history_path(&files, &key).map_err(map)?;
                let chat = default_path
                    .parent()
                    .ok_or_else(|| map("Invalid history directory".into()))?;
                let background = background_path(&input, chat).map_err(map)?;
                let loaded = match &background {
                    Some(path) => read_transcript_beneath(chat, path),
                    None => read_transcript_bounded(&default_path),
                }
                .map_err(|e| map(e.to_string()))?;
                let safe_id = JsString::from(crate::storage::sanitize_session_id(&key));
                let catalog = loaded
                    .snapshot
                    .as_ref()
                    .filter(|_| background.is_none())
                    .and_then(|(bytes, meta)| {
                        crate::runtime::manager::file_catalog::history_session_info(
                            bytes,
                            meta,
                            &safe_id,
                            &files.project,
                        )
                        .ok()
                        .flatten()
                    });
                // Read the existing project generation; do not construct a separate
                // model or configuration snapshot for the history endpoint.
                let lease = project_runtime
                    .acquire(Some(&files.project))
                    .map_err(|e| map(e.to_string()))?;
                let resources = lease
                    .runtime()
                    .resources()
                    .get::<crate::runtime::project::ProjectResources>()
                    .ok_or_else(|| map("Native project resources are missing.".into()))?;
                let snapshot = resources.context().snapshot();
                let agent = snapshot.get("config").and_then(|v| v.get("agent"));
                let max_context = agent
                    .and_then(|v| v.get("maxContextTokens"))
                    .and_then(JsValue::as_number);
                let max_output = agent
                    .and_then(|v| v.get("maxOutputTokens"))
                    .and_then(JsValue::as_number);
                let project = JsValue::from(files.project.to_string_lossy().as_ref());
                let now = JsValue::from(
                    crate::storage::transcript_timestamp_for_history()
                        .map_err(|e| map(e.to_string()))?
                        .as_str(),
                );
                let opts = super::envelope::Options {
                    project_root: &project,
                    now: &now,
                    catalog: catalog.as_ref(),
                    include_artifacts: files.project != files.home,
                    max_context,
                    max_output,
                };
                super::envelope::project(&loaded.read.entries, &input, &opts)
                    .map_err(|e| map(e.to_string()))
            })
            .await
            .map_err(|e| map(e.to_string()))?
        })
    }
}
