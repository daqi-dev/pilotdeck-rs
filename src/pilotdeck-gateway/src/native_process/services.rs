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

/// TS AttachmentResolver.ts:36 — `DEFAULT_MAX_FILE_BYTES = 1_000_000` (1 MB):
/// larger file attachments are skipped with an `Attachment … skipped`
/// diagnostic instead of being resolved inline.
const MAX_INLINE_FILE_BYTES: u64 = 1_000_000;
/// TS AttachmentResolver.ts:37 — `DEFAULT_MAX_IMAGE_BYTES = 5_242_880` (5 MiB):
/// larger images are skipped with an `Image … skipped` diagnostic instead of
/// being base64-inlined (verified against the TS source by the
/// `_tmp/20261105-issue-1321-fix-r1/ts-probe/` probe).
const MAX_INLINE_IMAGE_BYTES: u64 = 5_242_880;

/// Per-attachment inline byte cap, mirroring TS AttachmentResolver
/// (AttachmentResolver.ts:36-37): 5 MiB for images, 1 MB for other files.
fn inline_byte_limit(mime: &str) -> u64 {
    if mime.starts_with("image/") {
        MAX_INLINE_IMAGE_BYTES
    } else {
        MAX_INLINE_FILE_BYTES
    }
}

/// Returns true only if `path` is an absolute path strictly under
/// `lease_root` (the canonical `<project>/.tmp/chat-upload-leases/`) with no
/// `..` components, and `path` itself canonicalizes — symlinks followed — to
/// a location still under `lease_root`. Guards P1-1: only hard-linked lease
/// files may be inlined or added to allowedReadFiles; arbitrary
/// client-supplied paths, including a lease-directory symlink that escapes
/// the project, are rejected here before they can reach allowedReadFiles or
/// fs::read (same shape as `read_at`'s PROJECT_PATH_FORBIDDEN symlink check
/// in upload/mod.rs). Paths that cannot be resolved at all (missing file,
/// dangling symlink) are rejected too: the guard must not pass a path it
/// could not verify.
fn is_safe_lease_path(path: &str, lease_root: &Path) -> bool {
    let p = Path::new(path);
    if p.components().any(|c| c == Component::ParentDir) {
        return false;
    }
    fs::canonicalize(p).is_ok_and(|resolved| resolved.starts_with(lease_root))
}

/// `Image attachment not found: ${absolute} (${error}).` — the missing-file
/// diagnostic of TS AttachmentResolver (AttachmentResolver.ts:189-199). A
/// read failure here (lease TTL cleanup, permissions, a directory swap)
/// uses the same channel so the attachment is never silently dropped.
fn image_not_found(path: &str, error: &std::io::Error) -> String {
    format!("Image attachment not found: {path} ({error}).")
}

/// `Attachment not found: ${absolute} (${error}).` — the missing-file
/// diagnostic of TS AttachmentResolver for non-image files
/// (AttachmentResolver.ts:131-143).
fn file_not_found(path: &str, error: &std::io::Error) -> String {
    format!("Attachment not found: {path} ({error}).")
}

/// Composes the blocks-type agent input for a turn with attachments,
/// mirroring TS GatewayAttachmentTurnComposer: a text block for the message,
/// base64 image blocks for inlined images, an `[Attachment diagnostics]`
/// text block for skipped or unreadable attachments
/// (GatewayAttachmentTurnComposer.ts:270-274, verified by the
/// `_tmp/20261105-issue-1321-fix-r1/ts-probe/` probe), and the
/// registered-path note. Extracted from `prepare_attachment_turn` so the
/// lease guard and the size caps are unit-testable without the full native
/// assembly; runs entirely on a blocking thread.
pub(super) fn compose_attachment_turn(
    project_dir: &Path,
    message: &JsValue,
    attachments: &[JsValue],
) -> Result<JsValue, SubmitError> {
    let mut blocks: Vec<JsValue> = Vec::new();
    if let JsValue::String(text) = message
        && !text.is_empty()
    {
        blocks.push(object([("type", "text".into()), ("text", message.clone())]));
    }
    struct AttachEntry {
        path: String,
        name: String,
        mime: String,
        declared_bytes: Option<f64>,
    }
    let mut entries: Vec<AttachEntry> = Vec::new();
    for attachment in attachments {
        // TS drops attachments without a usable path silently instead of
        // rejecting them (GatewayAttachmentTurnComposer.ts:102
        // `if (!attachment.path) continue;` and :229): missing, empty, and
        // non-UTF-8 paths are skipped the same way here.
        let path = attachment
            .get("path")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let name = attachment
            .get("name")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "attachment".into())
            });
        let mime = attachment
            .get("mimeType")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap_or_default();
        // Lease attachments carry the upload manifest's verified `bytes`;
        // client-supplied attachments may declare any number.
        let declared_bytes = attachment.get("bytes").and_then(JsValue::as_number);
        entries.push(AttachEntry {
            path,
            name,
            mime,
            declared_bytes,
        });
    }
    // P1-1: the canonical lease root is computed once per prepare (simplify#2)
    // and every attachment path must canonicalize inside it before it can
    // reach allowedReadFiles or fs::read.
    let project_canon = fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let lease_root = project_canon.join(".tmp").join("chat-upload-leases");
    let mut allowed_paths: Vec<JsValue> = Vec::new();
    let mut note_lines: Vec<String> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    let mut image_blocks: Vec<JsValue> = Vec::new();
    for entry in &entries {
        if !is_safe_lease_path(&entry.path, &lease_root) {
            return Err(failure(
                "Attachment path is outside the permitted lease directory.",
            ));
        }
        allowed_paths.push(JsValue::from(entry.path.as_str()));
        note_lines.push(format!("- {}: {}", entry.name, entry.path));
        let image = entry.mime.starts_with("image/");
        let limit = inline_byte_limit(&entry.mime);
        // P1-2: check the byte count before reading so an oversized
        // attachment is never pulled into memory (TS stat cap,
        // AttachmentResolver.ts:164-175 and :202-212). Prefer the
        // manifest-declared `bytes`; `fs::metadata` is authoritative whenever
        // it disagrees, so a small declared size cannot smuggle an oversized
        // file past the stat.
        let oversize = if let Some(bytes) =
            entry.declared_bytes.filter(|b| b.is_finite() && *b >= 0.0)
            && bytes > limit as f64
        {
            Some(bytes)
        } else {
            match fs::metadata(&entry.path) {
                Ok(info) if info.len() > limit => Some(info.len() as f64),
                Ok(_) => None,
                Err(error) => {
                    diagnostics.push(if image {
                        image_not_found(&entry.path, &error)
                    } else {
                        file_not_found(&entry.path, &error)
                    });
                    continue;
                }
            }
        };
        if let Some(bytes) = oversize {
            diagnostics.push(if image {
                // AttachmentResolver.ts:202-212
                format!(
                    "Image {path} is {bytes} bytes (limit {limit}); skipped \
                     (PilotDeck does not resize, intentional_difference §4.5).",
                    path = entry.path
                )
            } else {
                // AttachmentResolver.ts:164-175
                format!(
                    "Attachment {path} is {bytes} bytes (limit {limit}); skipped.",
                    path = entry.path
                )
            });
            continue;
        }
        if !image {
            // Non-image attachments stay registered path references (the note
            // above); only images are read for inlining.
            continue;
        }
        // P2a-1: a read failure must not silently drop the attachment —
        // lease TTL cleanup, permissions, or a directory in place of the
        // file surfaces as a TS-shaped diagnostic instead.
        match fs::read(&entry.path) {
            Ok(bytes) => {
                let encoded = base64_encode(&bytes);
                image_blocks.push(object([
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
            Err(error) => diagnostics.push(image_not_found(&entry.path, &error)),
        }
    }
    blocks.extend(image_blocks);
    if !diagnostics.is_empty() {
        // GatewayAttachmentTurnComposer.ts:270-274
        let note = format!(
            "[Attachment diagnostics]\n{}",
            diagnostics
                .iter()
                .map(|message| format!("- {message}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        blocks.push(object([
            ("type", "text".into()),
            ("text", JsValue::from(note.as_str())),
        ]));
    }
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
            let project_dir = self.options.project.clone();
            let message = message.clone();
            // P1-3/concurrency#5: the whole composition — the once-per-prepare
            // lease-root canonicalize (simplify#2), the per-attachment stat and
            // read — runs on a blocking thread so the async executor is not
            // blocked.
            tokio::task::spawn_blocking(move || {
                compose_attachment_turn(&project_dir, &message, &attachments)
            })
            .await
            .map_err(|e| failure(e.to_string()))?
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
