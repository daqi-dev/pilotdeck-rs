//! Verified upload leases for a single admitted turn (GatewayUploadedAttachmentBundle.ts).
use super::submit::SubmitError;
use crate::{
    metadata::{self, Metadata, NativeFiles, object},
    upload::{UploadArtifactLease, UploadError, UploadStore, UploadStoreOptions},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) struct UploadedTurn {
    pub input: JsValue,
    leases: Vec<UploadArtifactLease>,
}

impl UploadedTurn {
    pub(super) fn release(mut self) -> Result<(), SubmitError> {
        self.release_leases()
    }

    fn release_leases(&mut self) -> Result<(), SubmitError> {
        let failures = release_all(std::mem::take(&mut self.leases));
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.into_iter().next().unwrap()),
            _ => Err(SubmitError::new(
                "UPLOAD_LEASE_RELEASE_FAILED",
                format!(
                    "Failed to release uploaded attachment leases: {}",
                    failures
                        .iter()
                        .map(|error| format!("{}: {}", error.code, error.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )),
        }
    }
}

impl Drop for UploadedTurn {
    fn drop(&mut self) {
        if let Err(error) = self.release_leases() {
            eprintln!(
                "Upload lease cleanup failed [{}]: {}",
                error.code, error.message
            );
        }
    }
}

fn release_all(leases: Vec<UploadArtifactLease>) -> Vec<SubmitError> {
    leases
        .into_iter()
        .rev()
        .filter_map(|lease| lease.release().err().map(upload_error))
        .collect()
}

pub(super) async fn resolve(
    input: &JsValue,
    pilot_home: &Path,
    cwd: &JsString,
) -> Result<UploadedTurn, SubmitError> {
    let uploads = match input.get("uploadedAttachments") {
        None | Some(JsValue::Undefined | JsValue::Null) => &[][..],
        Some(JsValue::Array(uploads)) => uploads.as_slice(),
        Some(_) => return Err(invalid("uploadedAttachments must be an array.")),
    };
    if uploads.is_empty() {
        return Ok(UploadedTurn {
            input: input.clone(),
            leases: Vec::new(),
        });
    }
    let input = input.clone();
    let pilot_home = pilot_home.to_path_buf();
    let cwd = cwd.clone();
    tokio::task::spawn_blocking(move || resolve_blocking(input, &pilot_home, &cwd))
        .await
        .map_err(|error| SubmitError::new("UPLOAD_RESOLVE_FAILED", error.to_string()))?
}

fn resolve_blocking(
    input: JsValue,
    pilot_home: &Path,
    cwd: &JsString,
) -> Result<UploadedTurn, SubmitError> {
    let mut turn = UploadedTurn {
        input,
        leases: Vec::new(),
    };
    let result = (|| {
        let cwd = cwd
            .to_utf8()
            .map_err(|_| invalid("cwd must be a valid filesystem path."))?;
        // daqi keeps resolve_process_path fallible (#879: an unresolvable process
        // cwd must surface, not silently become /). B returns a bare PathBuf; map
        // the error into the submit error code instead.
        let home = metadata::resolve_process_path(Path::new(&cwd), pilot_home)
            .map_err(|error| SubmitError::new("UPLOAD_RESOLVE_FAILED", error.to_string()))?;
        let cwd_resolved = metadata::resolve_process_path(Path::new(""), Path::new(&cwd))
            .map_err(|error| SubmitError::new("UPLOAD_RESOLVE_FAILED", error.to_string()))?;
        let metadata = Arc::new(Metadata {
            home,
            cwd: cwd_resolved,
            files: NativeFiles,
        });
        let project = turn
            .input
            .get("projectKey")
            .and_then(JsValue::as_string)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                SubmitError::new(
                    "PROJECT_NOT_FOUND",
                    "projectKey is required for uploaded attachments.",
                )
            })?;
        let root = metadata::resolved_identity(&metadata.cwd, project)
            .ok_or_else(|| SubmitError::new("PROJECT_NOT_FOUND", "Unknown projectKey."))?;
        if Path::new(&root) != metadata.home
            && !metadata
                .has_registered_project(Path::new(&root))
                .map_err(|error| SubmitError::new("UPLOAD_RESOLVE_FAILED", error.to_string()))?
        {
            return Err(SubmitError::new("PROJECT_NOT_FOUND", "Unknown projectKey."));
        }
        let store = upload_store(metadata);
        let uploads = match turn.input.get("uploadedAttachments") {
            Some(JsValue::Array(uploads)) => uploads.clone(),
            _ => unreachable!("resolve admits nonempty upload arrays"),
        };
        let mut attachments = match turn.input.get("attachments") {
            None | Some(JsValue::Undefined | JsValue::Null) => Vec::new(),
            Some(JsValue::Array(attachments)) => attachments.clone(),
            Some(_) => return Err(invalid("attachments must be an array.")),
        };
        for upload in uploads {
            let upload_id = required_text(&upload, "uploadId")?;
            let attachment_ids = match upload.get("attachmentIds") {
                None | Some(JsValue::Undefined) => None,
                Some(JsValue::Array(ids)) => Some(
                    ids.iter()
                        .map(|id| {
                            id.as_string()
                                .and_then(|id| id.to_utf8().ok())
                                .ok_or_else(|| invalid("attachmentIds must contain strings."))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                Some(_) => return Err(invalid("attachmentIds must be an array.")),
            };
            let lease = store
                .acquire_attachment_lease(&upload_id, &root, attachment_ids.as_deref())
                .map_err(upload_error)?;
            turn.leases.push(lease);
            for attachment in &turn.leases.last().unwrap().attachments {
                let field = |name| attachment.get(name).cloned().unwrap_or(JsValue::Undefined);
                let image = attachment
                    .get("mimeType")
                    .and_then(JsValue::as_string)
                    .and_then(|mime| mime.to_utf8().ok())
                    .is_some_and(|mime| mime.starts_with("image/"));
                attachments.push(object([
                    ("type", if image { "image" } else { "file" }.into()),
                    ("name", field("name")),
                    ("path", field("path")),
                    ("mimeType", field("mimeType")),
                    ("bytes", field("bytes")),
                    (
                        "metadata",
                        object([
                            ("uploadId", upload_id.as_str().into()),
                            ("attachmentId", field("attachmentId")),
                            ("relativePath", field("relativePath")),
                            ("sha256", field("sha256")),
                        ]),
                    ),
                ]));
            }
        }
        let JsValue::Object(fields) = &mut turn.input else {
            return Err(invalid("Submit input must be an object."));
        };
        fields.insert("attachments".into(), JsValue::Array(attachments));
        Ok(())
    })();
    if let Err(error) = result {
        // TS releases every acquired lease before throwing; cleanup errors win.
        turn.release_leases()?;
        return Err(error);
    }
    Ok(turn)
}

pub(crate) fn upload_store(metadata: Arc<Metadata>) -> Arc<UploadStore> {
    type Stores = std::collections::HashMap<(PathBuf, PathBuf), std::sync::Weak<UploadStore>>;
    static STORES: std::sync::OnceLock<std::sync::Mutex<Stores>> = std::sync::OnceLock::new();
    let mut stores = STORES.get_or_init(Default::default).lock().unwrap();
    let key = (metadata.home.clone(), metadata.cwd.clone());
    if let Some(store) = stores.get(&key).and_then(std::sync::Weak::upgrade) {
        return store;
    }
    let registry = metadata.clone();
    let store = Arc::new(UploadStore::new(UploadStoreOptions::new(
        move |key: &JsString| {
            // daqi keeps the project key as raw UTF-16 all the way down;
            // resolve_dialog_project does the same resolve + registry check
            // and returns None when the key names no registered project.
            let root = registry
                .resolve_dialog_project(key)
                .ok_or_else(|| {
                    UploadError::dialog(
                        "PROJECT_NOT_FOUND",
                        format!(
                            "Unknown projectKey: {}",
                            String::from_utf16_lossy(key.units())
                        ),
                    )
                })?;
            Ok(PathBuf::from(root))
        },
        move || {
            let projects = metadata
                .list_projects()
                .map_err(|error| UploadError::dialog("PROJECT_NOT_FOUND", error.to_string()))?;
            let Some(JsValue::Array(projects)) = projects.get("projects") else {
                return Err(UploadError::dialog(
                    "PROJECT_NOT_FOUND",
                    "Project registry is unavailable.",
                ));
            };
            let mut roots: Vec<PathBuf> = projects
                .iter()
                .map(|project| {
                    let key = project
                        .get("projectKey")
                        .and_then(JsValue::as_string)
                        .and_then(|key| metadata::resolved_identity(&metadata.cwd, key))
                        .ok_or_else(|| {
                            UploadError::dialog("PROJECT_NOT_FOUND", "Invalid registered project.")
                        })?;
                    Ok(key.into())
                })
                .collect::<Result<_, _>>()?;
            if !roots.contains(&metadata.home) {
                roots.insert(0, metadata.home.clone());
            }
            Ok(roots)
        },
    )));
    stores.insert(key, Arc::downgrade(&store));
    store
}

fn required_text(value: &JsValue, field: &str) -> Result<String, SubmitError> {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .ok_or_else(|| invalid(format!("{field} must be a string.")))
}
fn invalid(message: impl Into<String>) -> SubmitError {
    SubmitError::new("INVALID_UPLOADED_ATTACHMENT", message)
}
fn upload_error(error: UploadError) -> SubmitError {
    SubmitError::new(
        if error.code.is_empty() {
            "gateway_submit_failed"
        } else {
            &error.code
        },
        error.message_string(),
    )
}

