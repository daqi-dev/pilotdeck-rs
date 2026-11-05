use super::{Options, assembly::Recorders};
use crate::{
    config::Env,
    metadata::object,
    runtime::{
        project::{ProjectResources, ProjectRuntimeRegistry},
        snapshot::{SnapshotProjectResolver, SnapshotProjects},
        turn::{
            submit::{NativeSubmitSnapshots, SubmitError, SubmitFuture, SubmitSnapshots},
            submit_host::{SubmitAttachmentLease, SubmitCwdService, SubmitHostServices},
        },
    },
    storage::{ContentAddressedWorkspaceSnapshotRecorder, RunRef},
    upload::UploadStore,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pilotdeck_protocol::{JsString, JsValue};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub struct Services {
    pub options: Options,
    pub project: Arc<ProjectRuntimeRegistry>,
    pub env: Env,
    pub recorders: Recorders,
    pub owner: crate::runtime::session::SessionOwner,
    /// Extracted once at startup from `PersistenceResources`; `None` when
    /// `--storage-root` was not supplied and snapshots are disabled.
    pub snapshot_recorder: Option<Arc<ContentAddressedWorkspaceSnapshotRecorder>>,
    pub upload_store: Arc<UploadStore>,
}
fn base64_encode(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

/// Returns true only if `path` is an absolute path strictly under
/// `<project>/.tmp/chat-upload-leases/` with no `..` components.
/// Guards P1-1: only hard-linked lease files may be inlined or added
/// to allowedReadFiles; arbitrary client-supplied paths are rejected.
fn is_safe_lease_path(path: &str, project: &Path) -> bool {
    let p = Path::new(path);
    if p.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    let project_canon = fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let lease_root = project_canon.join(".tmp").join("chat-upload-leases");
    p.starts_with(&lease_root)
}

fn failure(message: impl Into<String>) -> SubmitError {
    SubmitError::new("gateway_submit_failed", message)
}
fn unavailable(message: &str) -> SubmitError {
    SubmitError::new("CAPABILITY_UNAVAILABLE", message)
}
struct Cwd(std::path::PathBuf);
impl SubmitCwdService for Cwd {
    fn set(&self, _: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        if cwd
            .to_utf8()
            .is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)
        {
            Ok(())
        } else {
            Err(unavailable(
                "The isolated native process cannot change its configured project.",
            ))
        }
    }
}
impl SubmitHostServices for Services {
    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError> {
        if let Some(project) = input.get("projectKey").filter(|v| !v.is_undefined()) {
            let project = project
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            if !self.options.serves_project(&project) {
                return Err(unavailable(
                    "The isolated native process serves only its configured project.",
                ));
            }
        }
        // The composition does not expose the SDK/model setter yet. Refuse
        // these before session admission instead of reporting a false selection.
        for key in ["modelSelection", "modelOverride", "sdkConfig"] {
            if input.get(key).is_some_and(|v| !v.is_undefined()) {
                return Err(unavailable(
                    "Per-turn model/SDK overrides are not installed in this isolated process.",
                ));
            }
        }
        // When snapshots are enabled (--storage-root was supplied), resolve the
        // id exactly as the management snapshot route does
        // (`PersistenceResources::snapshot_gateway`): TS `resolveProjectStorageId`
        // prefers a registered `.cwd` marker, else the legacy path slug. One
        // resolver keeps turn-captured snapshots visible to `snapshot_list`.
        if self.snapshot_recorder.is_some() {
            let projects = SnapshotProjects::new(self.options.home.clone(), PathBuf::new());
            let project = self.options.project.to_string_lossy();
            Ok(Some(JsString::from(
                projects.workspace_id(&project).as_str(),
            )))
        } else {
            Ok(None)
        }
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        self.snapshot_recorder.as_ref().map(|recorder| {
            Arc::new(NativeSubmitSnapshots(recorder.clone())) as Arc<dyn SubmitSnapshots>
        })
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        Some(Arc::new(Cwd(self.options.project.clone())))
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            let current = crate::runtime::project::NativeConfigSource::new(
                self.options.project.clone(),
                self.env.clone(),
                Some(self.options.home.clone()),
                None,
            );
            use crate::runtime::project::ConfigSource;
            let fresh = current
                .load(&self.options.project)
                .map_err(|e| failure(e.to_string()))?;
            let lease = self
                .project
                .acquire(None)
                .map_err(|e| failure(e.to_string()))?;
            let staged = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| failure("Native project resources are missing."))?
                .context()
                .snapshot();
            if fresh.get("config") != staged.get("config") {
                self.project
                    .reload()
                    .await
                    .map_err(|e| failure(e.to_string()))?;
                use crate::runtime::refresh::RefreshRouter;
                self.owner.mark_all_dirty("config_changed");
            }
            Ok(())
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        let lease = self.project.acquire(None).ok()?;
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context()
            .snapshot()
            .get("config")?
            .get("runPolicy")
            .cloned()
    }
    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        let store = self.upload_store.clone();
        let project = self.options.project.clone();
        Box::pin(async move {
            // `uploadedAttachments` is a non-empty array at this call site
            // (GatewaySubmitHost::prepare checks that before calling).
            let refs = match input.get("uploadedAttachments") {
                Some(JsValue::Array(refs)) => refs.clone(),
                _ => {
                    return Err(SubmitError::new(
                        "CAPABILITY_UNAVAILABLE",
                        "uploadedAttachments must be a non-empty array.",
                    ));
                }
            };
            let project_key = project.to_string_lossy().into_owned();
            // Acquire leases sequentially, mirroring TS GatewayUploadedAttachmentBundle.
            // Disk I/O runs on a blocking thread so the async executor is not blocked.
            let mut acquired: Vec<crate::upload::UploadArtifactLease> = Vec::new();
            let result: Result<Vec<crate::upload::UploadArtifactLease>, SubmitError> =
                tokio::task::spawn_blocking(move || {
                    // P1-2: On error mid-loop, release every lease acquired so far
                    // (reverse order, best-effort) before propagating. Without this
                    // the partially-acquired set is dropped without release.
                    let outcome: Result<(), SubmitError> = (|| {
                        for r in &refs {
                            let upload_id = r
                                .get("uploadId")
                                .and_then(JsValue::as_string)
                                .and_then(|s| s.to_utf8().ok())
                                .ok_or_else(|| {
                                    SubmitError::new(
                                        "CAPABILITY_UNAVAILABLE",
                                        "uploadedAttachments[].uploadId must be a string.",
                                    )
                                })?;
                            // attachmentIds: optional string array
                            let attachment_ids: Option<Vec<String>> = match r.get("attachmentIds") {
                                Some(JsValue::Array(ids)) => Some(
                                    ids.iter()
                                        .map(|v| {
                                            v.as_string().and_then(|s| s.to_utf8().ok()).ok_or_else(
                                                || {
                                                    SubmitError::new(
                                                        "CAPABILITY_UNAVAILABLE",
                                                        "attachmentIds must be strings.",
                                                    )
                                                },
                                            )
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                                _ => None,
                            };
                            let lease = store
                                .acquire_attachment_lease(
                                    &upload_id,
                                    &project_key,
                                    attachment_ids.as_deref(),
                                )
                                .map_err(|e| {
                                    SubmitError::new(
                                        e.code.as_str(),
                                        String::from_utf16_lossy(e.message.units()),
                                    )
                                })?;
                            acquired.push(lease);
                        }
                        Ok(())
                    })();
                    match outcome {
                        Ok(()) => Ok(acquired),
                        Err(e) => {
                            for lease in acquired.iter().rev() {
                                if let Err(rel) = lease.release() {
                                    eprintln!("native upload lease release on error: {rel}");
                                }
                            }
                            Err(e)
                        }
                    }
                })
                .await
                .map_err(|e| SubmitError::new("CAPABILITY_UNAVAILABLE", e.to_string()))?;
            let leases = result?;
            // Merge attachments from all leases (TS: flatMap over acquired).
            let attachments: Vec<JsValue> = leases
                .iter()
                .flat_map(|lease| lease.attachments.iter().cloned())
                .collect();
            // Release is idempotent on all acquired leases (reverse order like TS).
            let release: crate::runtime::turn::submit_host::SubmitLeaseRelease =
                Box::new(move || {
                    Box::pin(async move {
                        for lease in leases.iter().rev() {
                            // Best-effort: log failures but continue.
                            if let Err(e) = lease.release() {
                                eprintln!("native upload lease release: {e}");
                            }
                        }
                        Ok(())
                    })
                });
            Ok(SubmitAttachmentLease {
                attachments,
                release,
            })
        })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        _project: Option<&JsString>,
        _session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            super::assembly::validate_profile(&self.options, &self.env)
                .await
                .map_err(|error| unavailable(&error.to_string()))?;
            if !matches!(message, JsValue::String(_)) {
                return Err(failure("message must be a string."));
            }
            if attachments.is_empty() {
                return Ok(object([("type", "text".into()), ("text", message.clone())]));
            }
            // Build a blocks-type agent input mirroring TS GatewayAttachmentTurnComposer.
            // For each lease attachment we emit a `file` or `image` block using the
            // leased path (hard-linked, stable for the duration of the turn).
            let mut blocks: Vec<JsValue> = Vec::new();
            if let JsValue::String(text) = message
                && !text.is_empty()
            {
                blocks.push(object([("type", "text".into()), ("text", message.clone())]));
            }
            // Build the attachment path note so the agent can use read_file.
            // P1-1: Only paths under the project's lease directory are permitted.
            // Bare absolute paths and any path with '..' are rejected here before
            // they can reach allowedReadFiles or fs::read.
            let project_dir = self.options.project.clone();
            let mut allowed_paths: Vec<JsValue> = Vec::new();
            let mut note_lines: Vec<String> = Vec::new();
            // P1-3: Collect (path, mime, name) for lease-validated attachments,
            // then read files in a blocking task so the async executor is not blocked.
            struct AttachEntry {
                path: String,
                mime: String,
            }
            let mut entries: Vec<AttachEntry> = Vec::new();
            for attachment in &attachments {
                let path = attachment
                    .get("path")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                let name = attachment
                    .get("name")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_else(|| {
                        std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "attachment".into())
                    });
                let mime = attachment
                    .get("mimeType")
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default();
                if !path.is_empty() {
                    // P1-1: Reject any path that is not under the lease directory.
                    if !is_safe_lease_path(&path, &project_dir) {
                        return Err(failure(
                            "Attachment path is outside the permitted lease directory.",
                        ));
                    }
                    allowed_paths.push(JsValue::from(path.as_str()));
                    note_lines.push(format!("- {name}: {path}"));
                    entries.push(AttachEntry { path, mime });
                }
            }
            // P1-3: Read image bytes on a blocking thread (mirrors resolve_attachments).
            let image_blocks: Vec<JsValue> = tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                for entry in &entries {
                    if entry.mime.starts_with("image/")
                        && let Ok(bytes) = std::fs::read(&entry.path)
                    {
                        let encoded = base64_encode(&bytes);
                        out.push(object([
                            ("type", "image".into()),
                            (
                                "source",
                                object([
                                    ("type", "base64".into()),
                                    ("media_type", JsValue::from(entry.mime.as_str())),
                                    ("data", JsValue::from(encoded.as_str())),
                                ]),
                            ),
                        ]));
                    }
                }
                out
            })
            .await
            .map_err(|e| failure(e.to_string()))?;
            blocks.extend(image_blocks);
            if !note_lines.is_empty() {
                let header = "[Registered attachment files in this session:]";
                let note = format!(
                    "{header}\n{lines}\nThese are path references for reuse.",
                    lines = note_lines.join("\n")
                );
                blocks.push(object([
                    ("type", "text".into()),
                    ("text", JsValue::from(note.as_str())),
                ]));
            }
            let mut result = object([
                ("type", "blocks".into()),
                ("content", JsValue::Array(blocks)),
            ]);
            if !allowed_paths.is_empty()
                && let JsValue::Object(ref mut o) = result
            {
                o.insert("allowedReadFiles".into(), JsValue::Array(allowed_paths));
            }
            Ok(result)
        })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            let metadata = crate::metadata::Metadata {
                home: self.options.home.clone(),
                cwd: std::path::PathBuf::new(),
                files: crate::metadata::NativeFiles,
            };
            let params = object([
                (
                    "projectKey",
                    JsValue::String(self.options.project.to_string_lossy().into_owned().into()),
                ),
                (
                    "sessionKey",
                    input
                        .get("sessionKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            let state =
                crate::session_model::execute(&metadata, &params, &self.env).map_err(|e| {
                    SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                })?;
            let saved = state.get("saved").cloned().unwrap_or(JsValue::Undefined);
            if saved.get("mode") == Some(&"model".into()) {
                Ok(object([("source", "session".into()), ("selection", saved)]))
            } else {
                Ok(object([(
                    "source",
                    state
                        .get("effective")
                        .and_then(|v| v.get("source"))
                        .cloned()
                        .unwrap_or("default".into()),
                )]))
            }
        })
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let recorder = self
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run.session_key)
            .and_then(std::sync::Weak::upgrade);
        let run = run.clone();
        let status = status.clone();
        Box::pin(async move {
            let recorder =
                recorder.ok_or_else(|| failure("Native session recorder is unavailable."))?;
            tokio::task::spawn_blocking(move || {
                recorder.writer().record_agent_status_message(
                    &run.session_key,
                    &run.run_id,
                    &status,
                )
            })
            .await
            .map_err(|e| failure(e.to_string()))?
            .map_err(|e| failure(e.to_string()))
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        self.recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, recorder| recorder.strong_count() > 0);
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.options.drain_budget.as_millis() as u64
    }
}
