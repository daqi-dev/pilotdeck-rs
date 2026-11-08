//! Immutable request selection on one shared registry/session owner. Selection
//! is lexical identity only; Metadata and each handler keep their own project
//! registration and parameter-validation contracts.
use super::Options;
use crate::runtime::{
    connection::RequestError,
    session::{SessionOwner, SessionProjectScope},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::path::{Path, PathBuf};

pub(super) struct SelectedProject {
    pub options: Options,
    pub reservation: Option<SessionProjectScope>,
}

pub(super) fn resolve(key: &JsString) -> Result<PathBuf, RequestError> {
    crate::metadata::resolve_process_identity(Path::new(""), key)
        .map_err(|error| {
            let error = crate::config::cwd_error(error);
            RequestError {
                code: error.code,
                message: error.message,
                details: None,
                validation: None,
            }
        })?
        .map(PathBuf::from)
        .ok_or_else(|| {
            let mut units = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
            units.extend_from_slice(key.units());
            RequestError {
                code: "PROJECT_NOT_FOUND".into(),
                message: JsString::from_utf16(units),
                details: None,
                validation: None,
            }
        })
}
pub(super) fn select(
    owner: &SessionOwner,
    options: &Options,
    params: &JsValue,
    empty_is_default: bool,
) -> Result<SelectedProject, RequestError> {
    let requested = params
        .get("projectKey")
        .and_then(JsValue::as_string)
        .filter(|key| !(empty_is_default && key.is_empty()))
        .map(resolve)
        .transpose()?;
    let requested_key = requested
        .as_ref()
        .map(|root| JsString::from(root.to_string_lossy().into_owned()));
    let fallback = JsString::from(options.project.to_string_lossy().into_owned());
    let reservation = params
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .filter(|key| !key.is_empty())
        .map(|key| owner.reserve_project(key, requested_key.as_ref(), &fallback))
        .transpose()
        .map_err(|error| {
            let mut result = RequestError::new(error.message);
            if String::from_utf16_lossy(result.message.units())
                .starts_with("PROJECT_SESSION_MISMATCH:")
            {
                result.code = "PROJECT_SESSION_MISMATCH".into();
            }
            result
        })?;
    let mut scoped = options.clone();
    scoped.project = reservation
        .as_ref()
        .map(|scope| {
            scope
                .project_key()
                .to_utf8()
                .map(PathBuf::from)
                .map_err(|_| {
                    RequestError::new(
                        "Session project identity cannot be represented by this host.",
                    )
                })
        })
        .transpose()?
        .or(requested)
        .unwrap_or_else(|| options.project.clone());
    Ok(SelectedProject {
        options: scoped,
        reservation,
    })
}

pub(super) fn upload_store(home: PathBuf) -> std::sync::Arc<crate::upload::UploadStore> {
    let resolve_home = home.clone();
    std::sync::Arc::new(crate::upload::UploadStore::new(
        crate::upload::UploadStoreOptions::new(
            move |key| {
                let metadata = crate::metadata::Metadata {
                    home: resolve_home.clone(),
                    cwd: PathBuf::new(),
                    files: super::methods::session_dir::BoundedSessionMarkers::new(),
                };
                let resolved =
                    crate::session_model::resolve_project(&metadata, &JsValue::String(key.clone()));
                if metadata.files.was_refused() {
                    return Err(crate::upload::UploadError::dialog(
                        "gateway_request_failed",
                        "Unsafe project marker.",
                    ));
                }
                resolved
                    .map(PathBuf::from)
                    .map_err(|error| crate::upload::UploadError::dialog(&error.code, error.message))
            },
            move || {
                let metadata = crate::metadata::Metadata {
                    home: home.clone(),
                    cwd: PathBuf::new(),
                    files: super::methods::session_dir::BoundedSessionMarkers::new(),
                };
                let listed = metadata.list_projects().map_err(|error| {
                    crate::upload::UploadError::io(error, "scandir", Some(&home.join("projects")))
                })?;
                if metadata.files.was_refused() {
                    return Err(crate::upload::UploadError::dialog(
                        "gateway_request_failed",
                        "Unsafe project marker.",
                    ));
                }
                let Some(JsValue::Array(projects)) = listed.get("projects") else {
                    return Ok(Vec::new());
                };
                Ok(projects
                    .iter()
                    .filter_map(|project| {
                        project
                            .get("fullPath")
                            .and_then(JsValue::as_string)
                            .and_then(|path| path.to_utf8().ok())
                            .map(PathBuf::from)
                    })
                    .collect())
            },
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metadata::object, runtime::turn::submit_host::SubmitHostServices};
    #[tokio::test]
    async fn native_request_scope_preserves_raw_input_and_pins_session_default() {
        let (_scratch, requests) = super::super::management_tests::setup();
        let b = requests
            .options
            .project
            .parent()
            .unwrap()
            .join("unregistered-B");
        let raw = object([
            (
                "projectKey",
                JsValue::String(b.to_string_lossy().into_owned().into()),
            ),
            ("sessionKey", "same".into()),
        ]);
        let selected = select(&requests.owner, &requests.options, &raw, false).unwrap();
        assert_eq!(selected.options.project, b);
        assert_eq!(
            raw.get("projectKey").and_then(JsValue::as_string).unwrap(),
            &JsString::from(b.to_string_lossy().into_owned())
        );
        let fallback = select(
            &requests.owner,
            &requests.options,
            &object([("sessionKey", "same".into())]),
            false,
        )
        .unwrap();
        assert_eq!(fallback.options.project, b);
        let conflicting = object([
            (
                "projectKey",
                JsValue::String(
                    requests
                        .options
                        .project
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                ),
            ),
            ("sessionKey", "same".into()),
        ]);
        assert!(
            matches!(select(&requests.owner, &requests.options, &conflicting, false), Err(ref error) if error.code == "PROJECT_SESSION_MISMATCH")
        );
        assert!(
            !b.exists(),
            "selection must not create/register an unknown project"
        );
        drop(fallback);
        drop(selected);
        select(&requests.owner, &requests.options, &conflicting, false).unwrap();
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn native_scoped_services_validate_b_catalog_and_cwd_without_using_a() {
        let (_scratch, requests) = super::super::management_tests::setup();
        let b = requests
            .options
            .project
            .parent()
            .unwrap()
            .join("services-B");
        std::fs::create_dir_all(b.join(".pilotdeck")).unwrap();
        std::fs::write(b.join(".pilotdeck/pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: test/only-b}\nmodel: {providers: {test: {models: {only-b: {}}}}}\n".replace("\n", "
")).unwrap();
        let mut options = requests.options.clone();
        options.project = b.clone();
        super::super::assembly::register_project(&options).unwrap();
        let services = super::super::services::Services {
            options: requests.options.clone(),
            project: requests.project.clone(),
            env: requests.env.clone(),
            recorders: requests.recorders.clone(),
            owner: requests.owner.clone(),
            snapshot_recorder: None,
            upload_store: upload_store(requests.options.home.clone()),
        };
        let mut input = object([
            (
                "projectKey",
                JsValue::String(b.to_string_lossy().into_owned().into()),
            ),
            ("sessionKey", "services".into()),
            (
                "modelSelection",
                object([
                    ("mode", "model".into()),
                    ("provider", "test".into()),
                    ("model", "only-b".into()),
                ]),
            ),
        ]);
        let before = input.clone();
        let bound = services.bind_project(&mut input).await.unwrap().unwrap();
        assert_eq!(input, before, "binding must preserve request material");
        assert_eq!(
            bound.bound_project_key(),
            Some(b.to_string_lossy().into_owned().into())
        );
        bound
            .cwd()
            .unwrap()
            .set(&"services".into(), &b.to_string_lossy().into_owned().into())
            .unwrap();
        assert!(
            bound
                .cwd()
                .unwrap()
                .set(
                    &"services".into(),
                    &requests
                        .options
                        .project
                        .to_string_lossy()
                        .into_owned()
                        .into()
                )
                .is_err()
        );
        let selected = bound.resolve_model_selection(&input).await.unwrap();
        assert_eq!(
            selected
                .get("selection")
                .and_then(|value| value.get("model")),
            Some(&JsValue::from("only-b"))
        );
        assert!(
            services.resolve_model_selection(&input).await.is_err(),
            "unbound A must not validate B against A"
        );
        drop(bound);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn native_shared_upload_store_resolves_registered_a_and_b_but_not_unknown() {
        let (_scratch, requests) = super::super::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("upload-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut options = requests.options.clone();
        options.project = b.clone();
        super::super::assembly::register_project(&options).unwrap();
        let store = upload_store(requests.options.home.clone());
        let manifest = pilotdeck_protocol::parse_json(
            r#"[{"clientFileId":"f","name":"a.txt","relativePath":"a.txt","size":0}]"#,
        )
        .unwrap();
        let a_record = store.create(a.to_str().unwrap(), &manifest, None).unwrap();
        let b_record = store.create(b.to_str().unwrap(), &manifest, None).unwrap();
        assert_eq!(
            a_record.get("projectKey"),
            Some(&JsValue::from(a.to_str().unwrap()))
        );
        assert_eq!(
            b_record.get("projectKey"),
            Some(&JsValue::from(b.to_str().unwrap()))
        );
        let b_id = b_record
            .get("uploadId")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap();
        assert_eq!(
            store.get(&b_id).unwrap().get("projectKey"),
            b_record.get("projectKey")
        );
        let unknown = a.parent().unwrap().join("unknown-upload");
        assert!(
            matches!(store.create(unknown.to_str().unwrap(), &manifest, None), Err(ref error) if error.code == "PROJECT_NOT_FOUND")
        );
        assert!(!unknown.exists());
        drop(store);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn native_workspace_identity_folds_surrogate_segment_before_utf8_boundary() {
        use crate::runtime::turn::submit_host::SubmitHostServices;
        let (_scratch, requests) = super::super::management_tests::setup();
        let root = requests.options.project.clone();
        let mut units = root
            .parent()
            .unwrap()
            .to_string_lossy()
            .encode_utf16()
            .collect::<Vec<_>>();
        units.extend("/".encode_utf16());
        units.push(0xd800);
        units.extend("/../".encode_utf16());
        units.extend(root.file_name().unwrap().to_string_lossy().encode_utf16());
        let input = object([
            ("projectKey", JsValue::String(JsString::from_utf16(units))),
            ("sessionKey", "workspace-alias".into()),
        ]);
        let scoped = select(&requests.owner, &requests.options, &input, false).unwrap();
        assert_eq!(scoped.options.project, root);
        let services = super::super::services::Services {
            options: scoped.options,
            project: requests.project.clone(),
            env: requests.env.clone(),
            recorders: requests.recorders.clone(),
            owner: requests.owner.clone(),
            snapshot_recorder: Some(std::sync::Arc::new(
                crate::storage::ContentAddressedWorkspaceSnapshotRecorder::new(
                    crate::storage::resolve_workspace_snapshot_config(|key| {
                        (key == "PILOTDECK_STORAGE_ROOT").then(|| {
                            root.parent()
                                .unwrap()
                                .join("snapshots")
                                .to_string_lossy()
                                .into_owned()
                        })
                    })
                    .unwrap()
                    .unwrap(),
                ),
            )),
            upload_store: upload_store(requests.options.home.clone()),
        };
        let canonical = object([("projectKey", JsValue::from(root.to_string_lossy().as_ref()))]);
        assert_eq!(
            services.workspace_id(&canonical).unwrap(),
            Some("a8-main".into())
        );
        assert_eq!(
            services.workspace_id(&input).unwrap(),
            Some("a8-main".into())
        );
        let malformed = object([(
            "projectKey",
            JsValue::String(JsString::from_utf16(vec![0xd800])),
        )]);
        assert!(services.workspace_id(&malformed).is_err());
        drop(scoped.reservation);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
