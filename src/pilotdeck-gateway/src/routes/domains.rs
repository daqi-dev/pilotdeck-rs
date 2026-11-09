//! Management adapters share the gateway's explicit project and provider boundary.
use crate::{
    metadata::Metadata,
    runtime::{
        connection::RequestError,
        memory::{
            MemoryManagementService, MemoryProjection, MemoryProviderError, ProjectMemoryRegistry,
        },
        snapshot::{SnapshotProjectResolver, SnapshotProjection, SnapshotProjectionError},
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{path::PathBuf, sync::Arc};

pub(crate) const METHODS: &[&str] = &[
    "memory_list",
    "memory_wipe",
    "snapshot_list",
    "snapshot_get",
    "snapshot_restore",
    "upload_create",
    "upload_get",
    "upload_part",
    "upload_complete",
    "upload_cancel",
    "skill_list",
    "skill_read",
    "skill_write",
    "skill_create",
    "skill_delete",
    "skill_import",
    "skill_validate",
    "skill_scan",
];
struct Projects(Arc<Metadata>);
impl Projects {
    fn resolve(&self, key: &JsString) -> Option<PathBuf> {
        let path = crate::metadata::resolved_identity(&self.0.cwd, key)?;
        let path = PathBuf::from(path);
        self.0.has_registered_project(&path).then_some(path)
    }
}
impl ProjectMemoryRegistry for Projects {
    fn resolve_project_key(&self, key: &JsString) -> Result<JsString, MemoryProviderError> {
        self.resolve(key)
            .map(|p| p.to_string_lossy().as_ref().into())
            .ok_or_else(|| MemoryProviderError::unknown_project_key(key.units()))
    }
    fn memory_management(
        &self,
        key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        let project = self
            .resolve(key)
            .ok_or_else(|| MemoryProviderError::unknown_project_key(key.units()))?;
        let snapshot = crate::config::load_snapshot(
            &self.0.home,
            &self.0.cwd,
            &std::env::vars().collect(),
            Some(&project.to_string_lossy()),
            1.0,
            "",
        )
        .map_err(|error| {
            let mut failure = MemoryProviderError::new(error.message);
            failure.code = Some(error.code);
            failure
        })?;
        let memory = snapshot.get("config").and_then(|c| c.get("memory"));
        let Some(memory) = memory.filter(|m| {
            m.get("enabled") == Some(&JsValue::Bool(true))
                && m.get("provider")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("edgeclaw"))
        }) else {
            return Ok(None);
        };
        let root = memory
            .get("rootDir")
            .and_then(JsValue::as_string)
            .map(JsString::to_utf8)
            .transpose()
            .map_err(|_| MemoryProviderError::host_required())?
            .map(PathBuf::from)
            .unwrap_or_else(|| self.0.home.join("memory"));
        let root = crate::metadata::resolve_process_path(&self.0.cwd, &root);
        Ok(Some(Arc::new(
            crate::runtime::memory::edgeclaw::DiskMemoryManagement::open(
                &project,
                &root,
                &self.0.home,
            )?,
        )))
    }
}
impl SnapshotProjectResolver for Projects {
    fn resolve_project_key(&self, key: &[u16]) -> Result<String, SnapshotProjectionError> {
        let raw = JsString::from_utf16(key.to_vec());
        let general = crate::metadata::resolved_identity(&self.0.cwd, &raw)
            .filter(|path| std::path::Path::new(path) == self.0.home)
            .map(PathBuf::from);
        general
            .or_else(|| self.resolve(&raw))
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| SnapshotProjectionError::unknown_project_key(key))
    }
    fn workspace_id(&self, project: &str) -> String {
        crate::metadata::project_id(std::path::Path::new(project))
    }
}
fn error(value: JsValue) -> RequestError {
    let mut error = RequestError::new(
        value
            .get("message")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default(),
    );
    error.code = value
        .get("code")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| "gateway_request_failed".into());
    error.validation = value.get("validation").cloned().map(Box::new);
    error
}
pub(crate) async fn request(
    metadata: Arc<Metadata>,
    method: String,
    params: JsValue,
) -> Result<JsValue, RequestError> {
    if method.starts_with("upload_") {
        let params = if method == "upload_create" {
            upload_project_params(&metadata, params)?
        } else {
            params
        };
        let store = crate::runtime::turn::submit_uploads::upload_store(metadata);
        return crate::upload::routes::handle(&store, &method, &params)
            .expect("upload method")
            .await
            .map_err(|e| {
                let mut error = RequestError::new(e.message);
                error.code = e.code;
                error.details = e.details;
                error
            });
    }
    tokio::task::spawn_blocking(move || {
        if method.starts_with("skill_") {
            let mut options =
                crate::skills::SkillManagerOptions::new(metadata.home.to_string_lossy());
            options.builtin_skills_root = std::env::var("PILOTDECK_BUNDLED_SKILLS_DIR").ok();
            let manager = crate::skills::SkillManager::new(options)
                .map_err(|e| RequestError::new(e.message))?;
            let facade = crate::skills::SkillsFacade::new(manager, |project: Option<&JsValue>| {
                let project = project
                    .and_then(JsValue::as_string)
                    .map(JsString::to_utf8)
                    .transpose()
                    .map_err(|e| {
                        crate::skills::SkillError::manager("INVALID_PROJECT_KEY", e.to_string())
                    })?;
                let snapshot = crate::config::load_snapshot(
                    &metadata.home,
                    &metadata.cwd,
                    &std::env::vars().collect(),
                    project.as_deref(),
                    1.0,
                    "",
                )
                .map_err(|e| {
                    crate::skills::SkillError::manager(
                        &e.code,
                        String::from_utf16_lossy(e.message.units()),
                    )
                })?;
                Ok(snapshot
                    .get("config")
                    .and_then(|c| c.get("modules"))
                    .and_then(|m| m.get("skills"))
                    .cloned()
                    .unwrap_or(JsValue::Undefined))
            });
            let body =
                crate::skills::routes::handle(&method, &params, &facade).expect("skill method");
            return if body.get("ok") == Some(&JsValue::Bool(true)) {
                Ok(body.get("result").cloned().unwrap_or(JsValue::Undefined))
            } else {
                Err(error(body.get("error").cloned().unwrap_or(JsValue::Null)))
            };
        }
        let config =
            crate::storage::resolve_workspace_snapshot_config(|key| std::env::var(key).ok())
                .map_err(|e| RequestError::new(e.to_string()))?;
        let memory =
            MemoryProjection::new(Arc::new(Projects(metadata.clone())), metadata.cwd.clone());
        let snapshots = SnapshotProjection::new(
            config.map(crate::storage::create_workspace_snapshot_provider),
            Projects(metadata.clone()),
            metadata.cwd.to_string_lossy(),
        );
        let routes = crate::runtime::snapshot::memory_routes::ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        // These projection futures perform synchronous provider work; execute on a dedicated worker.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| RequestError::new(e.to_string()))?;
        runtime
            .block_on(routes.handle(&method, &params).expect("management method"))
            .map_err(|e| error(e.to_js()))
    })
    .await
    .map_err(|e| RequestError::new(e.to_string()))?
}

fn upload_project_params(
    metadata: &Metadata,
    mut params: JsValue,
) -> Result<JsValue, RequestError> {
    // InProcessGateway validates the key and files array before UploadStore
    // resolves the project. Leave those errors to the shared upload adapter.
    let Some(key) = params.get("projectKey").and_then(JsValue::as_string) else {
        return Ok(params);
    };
    if crate::metadata::js_trim(&String::from_utf16_lossy(key.units())).is_empty()
        || !matches!(params.get("files"), Some(JsValue::Array(_)))
    {
        return Ok(params);
    }
    let identity = crate::metadata::resolved_identity(&metadata.cwd, key);
    if !identity.as_ref().is_some_and(|root| {
        std::path::Path::new(root) == metadata.home
            || metadata.has_registered_project(std::path::Path::new(root))
    }) {
        let mut message: Vec<u16> = "Unknown projectKey: ".encode_utf16().collect();
        message.extend_from_slice(key.units());
        let mut error = RequestError::new(JsString::from_utf16(message));
        error.code = "PROJECT_NOT_FOUND".into();
        return Err(error);
    }
    // `path.resolve` can fold away a surrogate-bearing segment before the
    // registry compares identity; only then can the disk store use UTF-8.
    if key.to_utf8().is_err()
        && let JsValue::Object(fields) = &mut params
    {
        fields.insert(
            "projectKey".into(),
            JsValue::String(identity.unwrap().into()),
        );
    }
    Ok(params)
}
pub(crate) async fn response(metadata: Arc<Metadata>, request: &JsValue) -> super::Route {
    let method = crate::metadata::text_field(request, "method").expect("registered domain method");
    let result = self::request(
        metadata,
        method,
        request.get("params").cloned().unwrap_or(JsValue::Undefined),
    )
    .await;
    super::response(request, result)
}

#[cfg(test)]
mod upload_wiring_tests {
    use super::*;
    use crate::metadata::object;

    #[tokio::test]
    async fn upload_create_preserves_unknown_project_units_and_validation_order() {
        let scratch = crate::test_scratch::Scratch::new("upload-project-wiring");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let metadata = Arc::new(Metadata {
            home,
            cwd: scratch.path().to_path_buf(),
            files: crate::metadata::NativeFiles,
        });
        for key in [
            JsString::from("missing"),
            JsString::from_utf16(vec![0xd800]),
            JsString::from_utf16(vec![
                0xd800, 47, 46, 46, 47, 109, 105, 115, 115, 105, 110, 103,
            ]),
        ] {
            let params = object([
                ("projectKey", JsValue::String(key.clone())),
                ("files", JsValue::Array(vec![])),
            ]);
            let error = request(metadata.clone(), "upload_create".into(), params)
                .await
                .unwrap_err();
            assert_eq!(error.code, "PROJECT_NOT_FOUND");
            let mut expected: Vec<u16> = "Unknown projectKey: ".encode_utf16().collect();
            expected.extend_from_slice(key.units());
            assert_eq!(error.message, JsString::from_utf16(expected));
        }
        let params = object([("projectKey", "missing".into()), ("files", JsValue::Null)]);
        assert_eq!(
            request(metadata.clone(), "upload_create".into(), params)
                .await
                .unwrap_err()
                .code,
            "UPLOAD_MANIFEST_INVALID"
        );
        // A surrogate segment discarded by path.resolve still identifies General.
        let mut key: Vec<u16> = metadata.home.to_string_lossy().encode_utf16().collect();
        key.extend_from_slice(&[47, 0xd800, 47, 46, 46]);
        let params = object([
            ("projectKey", JsValue::String(JsString::from_utf16(key))),
            ("files", JsValue::Array(vec![])),
        ]);
        let normalized = upload_project_params(&metadata, params).unwrap();
        assert_eq!(
            normalized.get("projectKey"),
            Some(&JsValue::from(metadata.home.to_string_lossy().as_ref()))
        );
    }
}

#[cfg(test)]
mod snapshot_general_tests {
    use super::*;
    #[test]
    fn snapshot_general_project_resolves_without_a_registration_marker() {
        let scratch = crate::scratch::Scratch::new("snapshot-general");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let projects = Projects(Arc::new(Metadata {
            home: home.clone(),
            cwd: scratch.path().into(),
            files: crate::metadata::NativeFiles,
        }));
        let mut key = home.to_string_lossy().encode_utf16().collect::<Vec<_>>();
        key.extend_from_slice(&[47, 0xd800, 47, 46, 46]);
        assert_eq!(
            SnapshotProjectResolver::resolve_project_key(&projects, &key).unwrap(),
            home.to_string_lossy()
        );
        assert!(
            SnapshotProjectResolver::resolve_project_key(
                &projects,
                &"missing".encode_utf16().collect::<Vec<_>>()
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod memory_wiring_tests {
    use super::*;

    #[test]
    fn registered_enabled_memory_opens_native_repository_disabled_stays_unavailable() {
        let scratch = crate::scratch::Scratch::new("memory-domain-wiring");
        let home = scratch.path().join("home");
        let project = scratch.path().join("project");
        std::fs::create_dir_all(home.join("projects/proof")).unwrap();
        std::fs::create_dir_all(project.join(".pilotdeck")).unwrap();
        std::fs::write(
            home.join("projects/proof/.cwd"),
            project.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::fs::write(
            home.join("pilotdeck.yaml"),
            "schemaVersion: 1
agent: {model: proof/base}
model:
  providers:
    proof:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: public-fixture-only
      models: {base: {}}
",
        )
        .unwrap();
        let config_path = project.join(".pilotdeck/pilotdeck.yaml");
        let memory_root = scratch.path().join("memory-root");
        std::fs::write(&config_path, serde_json::json!({"schemaVersion":1,"memory":{"enabled":true,"provider":"edgeclaw","rootDir":memory_root}}).to_string()).unwrap();
        let projects = Projects(Arc::new(Metadata {
            home,
            cwd: project.clone(),
            files: crate::metadata::NativeFiles,
        }));
        let key = JsString::from(project.to_string_lossy().as_ref());
        let service = projects
            .memory_management(&key)
            .unwrap()
            .expect("native provider");
        assert!(service.list(100).unwrap().is_empty());
        service.clear_session(&"session".into()).unwrap();
        assert!(memory_root.join("workspaces").exists());
        assert!(
            projects
                .memory_management(&"unregistered-project".into())
                .is_err()
        );
        std::fs::write(
            &config_path,
            r#"{"schemaVersion":1,"memory":{"enabled":false}}"#,
        )
        .unwrap();
        assert!(projects.memory_management(&key).unwrap().is_none());
    }
}
