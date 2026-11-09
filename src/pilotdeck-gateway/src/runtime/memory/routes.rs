//! Management entry validation from InProcessGateway.ts:2794-2826.
//! Dependencies are injected; this module does not create a native provider.
use crate::runtime::memory::{
    MemoryListInput, MemoryProjection, MemoryProjectionError, MemoryWipeInput, js_trim_units,
};
use crate::runtime::snapshot::{
    SnapshotProjectResolver, SnapshotProjection, SnapshotProjectionError,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::future::Future;

/// Projection errors retain the host boundary separately from their wire shape.
/// G0 must check `requires_host()` before serializing an ordinary RPC error.
#[derive(Debug)]
pub enum ManagementRouteError {
    Memory(MemoryProjectionError),
    Snapshot(SnapshotProjectionError),
}
impl ManagementRouteError {
    pub fn requires_host(&self) -> bool {
        match self {
            Self::Memory(error) => error.requires_host(),
            Self::Snapshot(error) => error.requires_host(),
        }
    }
    pub fn to_js(&self) -> JsValue {
        match self {
            Self::Memory(error) => error.to_js(),
            Self::Snapshot(error) => error.as_json(),
        }
    }
}
impl From<MemoryProjectionError> for ManagementRouteError {
    fn from(error: MemoryProjectionError) -> Self {
        Self::Memory(error)
    }
}
impl From<SnapshotProjectionError> for ManagementRouteError {
    fn from(error: SnapshotProjectionError) -> Self {
        Self::Snapshot(error)
    }
}

/// G0 supplies the same projections used by its project registry and storage.
pub struct ManagementRoutes<'a, R> {
    pub memory: &'a MemoryProjection,
    pub snapshots: &'a SnapshotProjection<R>,
}
impl<R: SnapshotProjectResolver> ManagementRoutes<'_, R> {
    /// Unknown methods fall through; every known method returns a future,
    /// including validation failures and explicit host-boundary failures.
    pub fn handle<'a>(
        &'a self,
        method: &str,
        params: &'a JsValue,
    ) -> Option<impl Future<Output = Result<JsValue, ManagementRouteError>> + Send + 'a> {
        let method = match method {
            "memory_list" => "memory_list",
            "memory_wipe" => "memory_wipe",
            "snapshot_list" => "snapshot_list",
            "snapshot_get" => "snapshot_get",
            "snapshot_restore" => "snapshot_restore",
            _ => return None,
        };
        Some(async move {
            let project_key = project_key(params)?;
            match method {
                "memory_list" => {
                    let session_key = optional_session_key(params)?;
                    Ok(self
                        .memory
                        .list(&MemoryListInput {
                            project_key: Some(project_key.clone()),
                            session_key: session_key.cloned(),
                        })?
                        .to_js())
                }
                "memory_wipe" => {
                    let session_key = optional_session_key(params)?;
                    let scope = super::field(params, "scope").as_string();
                    if scope.is_some_and(|scope| scope.equals_str("session"))
                        && session_key.is_none()
                    {
                        return Err(gateway_error(
                            "INVALID_MEMORY_SCOPE",
                            "sessionKey is required for a session-scoped wipe.",
                        ));
                    }
                    Ok(self
                        .memory
                        .wipe(&MemoryWipeInput {
                            project_key: Some(project_key.clone()),
                            session_key: session_key.cloned(),
                            scope: scope.cloned(),
                        })?
                        .to_js())
                }
                "snapshot_list" => {
                    optional_session_key(params)?;
                    Ok(self.snapshots.list(params)?)
                }
                "snapshot_get" => {
                    snapshot_id(params)?;
                    Ok(self.snapshots.get(params)?)
                }
                "snapshot_restore" => {
                    snapshot_id(params)?;
                    let target = super::field(params, "targetProjectKey");
                    if !target.is_undefined() {
                        nonempty_string(target).ok_or_else(|| {
                            gateway_error("PROJECT_NOT_FOUND", "projectKey is required.")
                        })?;
                    }
                    Ok(self.snapshots.restore(params)?)
                }
                _ => unreachable!("only admitted management methods reach dispatch"),
            }
        })
    }
}

fn gateway_error(code: &'static str, message: &'static str) -> ManagementRouteError {
    SnapshotProjectionError::gateway(code, message).into()
}
fn nonempty_string(value: &JsValue) -> Option<&JsString> {
    value
        .as_string()
        .filter(|text| !js_trim_units(text.units()).is_empty())
}
fn project_key(params: &JsValue) -> Result<&JsString, ManagementRouteError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        let kind = if matches!(params, JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(SnapshotProjectionError::plain(
            "TypeError",
            format!("Cannot read properties of {kind} (reading 'projectKey')"),
        )
        .into());
    }
    nonempty_string(super::field(params, "projectKey"))
        .ok_or_else(|| gateway_error("PROJECT_NOT_FOUND", "projectKey is required."))
}
fn optional_session_key(params: &JsValue) -> Result<Option<&JsString>, ManagementRouteError> {
    let value = super::field(params, "sessionKey");
    if value.is_undefined() {
        return Ok(None);
    }
    nonempty_string(value).map(Some).ok_or_else(|| {
        gateway_error(
            "INVALID_SESSION_KEY",
            "sessionKey must be non-empty when provided.",
        )
    })
}

fn snapshot_id(params: &JsValue) -> Result<&JsString, ManagementRouteError> {
    nonempty_string(super::field(params, "snapshotId"))
        .ok_or_else(|| gateway_error("INVALID_MANAGEMENT_IDENTIFIER", "snapshotId is required."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::memory::{
        MemoryManagementService, MemoryProviderError, ProjectMemoryRegistry,
    };
    use pilotdeck_protocol::{JsObject, parse_json};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Registry(
        Mutex<Vec<JsString>>,
        Option<Arc<dyn MemoryManagementService>>,
    );
    impl ProjectMemoryRegistry for Registry {
        fn resolve_project_key(&self, key: &JsString) -> Result<JsString, MemoryProviderError> {
            self.0.lock().unwrap().push(key.clone());
            if key.equals_str("host") {
                return Err(MemoryProviderError::host_required());
            }
            Ok(key.clone())
        }
        fn memory_management(
            &self,
            _: &JsString,
        ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
            Ok(self.1.clone())
        }
    }
    #[derive(Default)]
    struct Service {
        calls: Mutex<Vec<JsValue>>,
        error: Option<MemoryProviderError>,
    }
    impl MemoryManagementService for Service {
        fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
            assert_eq!(limit, 100);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(vec![
                json(r#"{"sourceSessionKey":" \ud800 ","text":"kept"}"#),
                json(r#"{"sourceSessionKey":"other"}"#),
            ])
        }
        fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
            self.calls.lock().unwrap().push(scope.into());
            match &self.error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn clear_session(&self, session: &JsString) -> Result<(), MemoryProviderError> {
            self.calls
                .lock()
                .unwrap()
                .push(JsValue::String(session.clone()));
            match &self.error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }
    struct Resolver;
    impl SnapshotProjectResolver for Resolver {
        fn resolve_project_key(&self, _: &[u16]) -> Result<String, SnapshotProjectionError> {
            panic!("unconfigured snapshot provider must fail before resolution")
        }
        fn workspace_id(&self, _: &str) -> String {
            unreachable!()
        }
    }
    fn json(text: &str) -> JsValue {
        parse_json(text).unwrap()
    }
    fn code(error: &ManagementRouteError) -> JsValue {
        error
            .to_js()
            .get("code")
            .cloned()
            .unwrap_or(JsValue::Undefined)
    }

    #[tokio::test]
    async fn memory_list_validates_before_provider_and_preserves_identity() {
        let registry = Arc::new(Registry::default());
        let memory = MemoryProjection::new(registry.clone(), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        assert!(routes.handle("unknown", &JsValue::Null).is_none());
        for params in [JsValue::Null, JsValue::Undefined] {
            let error = routes
                .handle("memory_list", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(error.to_js().get("name"), Some(&JsValue::from("TypeError")));
        }
        for params in [
            json("{}"),
            json(r#"{"projectKey":"\ufeff "}"#),
            json(r#"{"projectKey":42}"#),
        ] {
            let error = routes
                .handle("memory_list", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from("PROJECT_NOT_FOUND"));
        }
        for value in [
            JsValue::Null,
            JsValue::Bool(false),
            JsValue::from(""),
            JsValue::from("\u{feff}"),
        ] {
            let mut params = JsObject::new();
            params.insert("projectKey".into(), "p".into());
            params.insert("sessionKey".into(), value);
            let params = JsValue::Object(params);
            let error = routes
                .handle("memory_list", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from("INVALID_SESSION_KEY"));
        }
        assert!(registry.0.lock().unwrap().is_empty());
        let params = json(r#"{"projectKey":" \ud800 ","sessionKey":" s "}"#);
        let error = routes
            .handle("memory_list", &params)
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("CAPABILITY_UNAVAILABLE"));
        assert_eq!(registry.0.lock().unwrap()[0].units(), &[32, 0xd800, 32]);
        let params = json(r#"{"projectKey":"host"}"#);
        let error = routes
            .handle("memory_list", &params)
            .unwrap()
            .await
            .unwrap_err();
        assert!(error.requires_host());
        assert_eq!(code(&error), JsValue::from("JS_HOST_REQUIRED"));
    }

    #[tokio::test]
    async fn memory_wipe_validates_scope_before_provider() {
        let registry = Arc::new(Registry::default());
        let memory = MemoryProjection::new(registry.clone(), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        for (params, expected) in [
            (
                json(r#"{"projectKey":"p","scope":"session"}"#),
                "INVALID_MEMORY_SCOPE",
            ),
            (
                json(r#"{"projectKey":"p","scope":"session","sessionKey":null}"#),
                "INVALID_SESSION_KEY",
            ),
            (
                json(r#"{"projectKey":"p","scope":"session","sessionKey":" "}"#),
                "INVALID_SESSION_KEY",
            ),
            (
                json(r#"{"projectKey":null,"sessionKey":null}"#),
                "PROJECT_NOT_FOUND",
            ),
        ] {
            let error = routes
                .handle("memory_wipe", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from(expected));
        }
        assert!(registry.0.lock().unwrap().is_empty());
        let params = json(r#"{"projectKey":"p"}"#);
        let error = routes
            .handle("memory_wipe", &params)
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("CAPABILITY_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn injected_memory_list_and_wipe_are_offline_and_keep_utf16() {
        let service = Arc::new(Service::default());
        let registry = Arc::new(Registry(Mutex::default(), Some(service.clone())));
        let memory = MemoryProjection::new(registry.clone(), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        let params = json(r#"{"projectKey":" \ud800 ","sessionKey":" \ud800 "}"#);
        let listed = routes
            .handle("memory_list", &params)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            listed,
            json(r#"{"items":[{"sourceSessionKey":" \ud800 ","text":"kept"}]}"#)
        );
        let listed = routes
            .handle("memory_list", &json(r#"{"projectKey":"p"}"#))
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            listed,
            json(
                r#"{"items":[{"sourceSessionKey":" \ud800 ","text":"kept"},{"sourceSessionKey":"other"}]}"#
            )
        );
        let params = json(r#"{"projectKey":" \ud800 ","sessionKey":" \ud800 ","scope":"session"}"#);
        assert_eq!(
            routes
                .handle("memory_wipe", &params)
                .unwrap()
                .await
                .unwrap(),
            json(r#"{"wiped":true,"scope":"session"}"#)
        );
        for scope in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(true),
            JsValue::from("Session"),
            JsValue::from(" session "),
        ] {
            let mut params = JsObject::new();
            params.insert("projectKey".into(), "p".into());
            params.insert("scope".into(), scope);
            assert_eq!(
                routes
                    .handle("memory_wipe", &JsValue::Object(params))
                    .unwrap()
                    .await
                    .unwrap(),
                json(r#"{"wiped":true,"scope":"project"}"#)
            );
        }
        let calls = service.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            JsValue::String(JsString::from_utf16(vec![32, 0xd800, 32]))
        );
        assert_eq!(&calls[1..], &vec![JsValue::from("current_project"); 5]);
        assert_eq!(registry.0.lock().unwrap()[0].units(), &[32, 0xd800, 32]);
    }

    #[tokio::test]
    async fn memory_provider_errors_and_host_boundary_are_preserved() {
        for requires_host in [false, true] {
            let provider_error = MemoryProviderError {
                name: "CustomError".into(),
                code: Some("CUSTOM".into()),
                message: JsString::from_utf16(vec![0xd800]),
                requires_host,
            };
            let service = Arc::new(Service {
                calls: Mutex::default(),
                error: Some(provider_error),
            });
            let memory = MemoryProjection::new(
                Arc::new(Registry(Mutex::default(), Some(service))),
                "/unused",
            );
            let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
            let routes = ManagementRoutes {
                memory: &memory,
                snapshots: &snapshots,
            };
            for method in ["memory_list", "memory_wipe"] {
                let error = routes
                    .handle(method, &json(r#"{"projectKey":"p"}"#))
                    .unwrap()
                    .await
                    .unwrap_err();
                assert_eq!(error.requires_host(), requires_host);
                assert_eq!(
                    error.to_js(),
                    json(r#"{"name":"CustomError","code":"CUSTOM","message":"\ud800"}"#)
                );
            }
        }
    }

    struct LocalResolver {
        source: String,
        target: String,
    }
    impl SnapshotProjectResolver for LocalResolver {
        fn resolve_project_key(&self, key: &[u16]) -> Result<String, SnapshotProjectionError> {
            if key == JsString::from("p").units() {
                return Ok(self.source.clone());
            }
            if key == JsString::from("target").units() {
                return Ok(self.target.clone());
            }
            Err(SnapshotProjectionError::unknown_project_key(key))
        }
        fn workspace_id(&self, project: &str) -> String {
            if project == self.source {
                "w".into()
            } else {
                "foreign".into()
            }
        }
    }
    struct SnapshotFixture {
        root: std::path::PathBuf,
        projection: SnapshotProjection<LocalResolver>,
        ids: Vec<String>,
        manifests: Vec<String>,
    }
    impl SnapshotFixture {
        fn new() -> Self {
            use crate::storage::{
                ContentAddressedWorkspaceSnapshotRecorder, SnapshotState, WorkspaceSnapshotConfig,
                WorkspaceSnapshotInput, create_workspace_snapshot_provider,
            };
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../_tmp/20261027-g-mem")
                .join(uuid::Uuid::new_v4().to_string());
            let source = root.join("source");
            let target = root.join("target");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(source.join("file.txt"), "original").unwrap();
            let config = WorkspaceSnapshotConfig {
                root: root.join("store").to_str().unwrap().into(),
                snapshot_root: root.join("store").to_str().unwrap().into(),
            };
            let recorder = ContentAddressedWorkspaceSnapshotRecorder::new(config.clone());
            let mut ids = Vec::new();
            let mut manifests = Vec::new();
            for i in 0..4 {
                let result = recorder
                    .capture_pre_user(&WorkspaceSnapshotInput {
                        workspace_id: if i == 3 { "foreign" } else { "w" }.into(),
                        session_id: if i < 2 { " s " } else { "s" }.into(),
                        turn_id: format!("t{i}"),
                        run_id: "r".into(),
                        workspace_dir: source.to_str().unwrap().into(),
                        workspace_stable: None,
                        failure_kind: None,
                        failure_reason: None,
                    })
                    .unwrap();
                assert_eq!(result.state, SnapshotState::Committed);
                ids.push(result.snapshot_id);
                manifests.push(result.manifest_path.unwrap());
            }
            let projection = SnapshotProjection::new(
                Some(create_workspace_snapshot_provider(config)),
                LocalResolver {
                    source: source.to_str().unwrap().into(),
                    target: target.to_str().unwrap().into(),
                },
                "/unused",
            );
            Self {
                root,
                projection,
                ids,
                manifests,
            }
        }
    }
    impl Drop for SnapshotFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn array(value: &JsValue) -> &[JsValue] {
        match value {
            JsValue::Array(items) => items,
            _ => panic!("expected array"),
        }
    }

    #[tokio::test]
    async fn snapshot_list_validates_entry_before_capability() {
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        for (params, expected) in [
            (json("{}"), "PROJECT_NOT_FOUND"),
            (
                json(r#"{"projectKey":"p","sessionKey":null}"#),
                "INVALID_SESSION_KEY",
            ),
            (json(r#"{"projectKey":"p"}"#), "CAPABILITY_UNAVAILABLE"),
        ] {
            let error = routes
                .handle("snapshot_list", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from(expected));
            assert!(!error.requires_host());
        }
    }

    #[tokio::test]
    async fn injected_snapshot_list_filters_pages_and_preserves_raw_timestamp_sorting() {
        let fixture = SnapshotFixture::new();
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &fixture.projection,
        };
        let params = json(r#"{"projectKey":"p"}"#);
        let all = routes
            .handle("snapshot_list", &params)
            .unwrap()
            .await
            .unwrap();
        let all = array(all.get("items").unwrap());
        assert_eq!(all.len(), 3);
        assert!(
            all.iter()
                .all(|item| item.get("workspaceId") == Some(&JsValue::from("w")))
        );
        let params = json(r#"{"projectKey":"p","sessionKey":" s ","limit":1}"#);
        let page = routes
            .handle("snapshot_list", &params)
            .unwrap()
            .await
            .unwrap();
        let first = &array(page.get("items").unwrap())[0];
        assert_eq!(first.get("sessionId"), Some(&JsValue::from(" s ")));
        assert_eq!(page.get("nextCursor"), first.get("snapshotId"));
        let mut params = params.as_object().unwrap().clone();
        params.insert("cursor".into(), page.get("nextCursor").unwrap().clone());
        let page = routes
            .handle("snapshot_list", &JsValue::Object(params))
            .unwrap()
            .await
            .unwrap();
        assert_eq!(array(page.get("items").unwrap()).len(), 1);
        assert!(page.get("nextCursor").is_none());
        for (params, expected) in [
            (
                json(r#"{"projectKey":"p","limit":0}"#),
                "INVALID_SNAPSHOT_LIMIT",
            ),
            (
                json(r#"{"projectKey":"p","cursor":"missing"}"#),
                "INVALID_SNAPSHOT_CURSOR",
            ),
            (json(r#"{"projectKey":" \ud800 "}"#), "PROJECT_NOT_FOUND"),
        ] {
            let error = routes
                .handle("snapshot_list", &params)
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from(expected));
        }
        let path = &fixture.manifests[0];
        let mut manifest = json(&std::fs::read_to_string(path).unwrap())
            .as_object()
            .unwrap()
            .clone();
        manifest.insert(
            "createdAt".into(),
            JsValue::String(JsString::from_utf16(vec![0xd800])),
        );
        std::fs::write(
            path,
            pilotdeck_protocol::stringify_json(&JsValue::Object(manifest))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let listed = routes
            .handle("snapshot_list", &json(r#"{"projectKey":"p"}"#))
            .unwrap()
            .await
            .unwrap();
        let listed = array(listed.get("items").unwrap());
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().any(|item| {
            item.get("createdAt")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.units() == [0xd800])
        }));
        assert_eq!(fixture.ids.len(), 4);
    }

    #[tokio::test]
    async fn snapshot_get_validates_identifiers_before_capability() {
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        for value in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(true),
            JsValue::Number(1.0),
            JsValue::from(""),
            JsValue::from("\u{feff} "),
        ] {
            let mut params = JsObject::new();
            params.insert("projectKey".into(), "p".into());
            params.insert("snapshotId".into(), value);
            let error = routes
                .handle("snapshot_get", &JsValue::Object(params))
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from("INVALID_MANAGEMENT_IDENTIFIER"));
            assert_eq!(
                error.to_js().get("message"),
                Some(&JsValue::from("snapshotId is required."))
            );
        }
        let error = routes
            .handle("snapshot_get", &json(r#"{"snapshotId":null}"#))
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("PROJECT_NOT_FOUND"));
        let error = routes
            .handle(
                "snapshot_get",
                &json(r#"{"projectKey":"p","snapshotId":"id","sessionKey":null}"#),
            )
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("CAPABILITY_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn injected_snapshot_get_preserves_manifest_ownership_and_errors() {
        let fixture = SnapshotFixture::new();
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &fixture.projection,
        };
        let mut params = JsObject::new();
        params.insert("projectKey".into(), "p".into());
        params.insert("snapshotId".into(), fixture.ids[0].as_str().into());
        params.insert("sessionKey".into(), JsValue::Null);
        let result = routes
            .handle("snapshot_get", &JsValue::Object(params.clone()))
            .unwrap()
            .await
            .unwrap();
        let manifest = json(&std::fs::read_to_string(&fixture.manifests[0]).unwrap());
        assert_eq!(result.get("snapshot"), Some(&manifest));
        for id in [
            JsValue::from(fixture.ids[3].as_str()),
            JsValue::String(JsString::from(format!(" {} ", fixture.ids[0]))),
            JsValue::String(JsString::from_utf16(vec![0xd800])),
        ] {
            params.insert("snapshotId".into(), id.clone());
            let error = routes
                .handle("snapshot_get", &JsValue::Object(params.clone()))
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from("SNAPSHOT_NOT_FOUND"));
            let mut expected = "Snapshot ".encode_utf16().collect::<Vec<_>>();
            expected.extend_from_slice(id.as_string().unwrap().units());
            expected.extend(" was not found.".encode_utf16());
            assert_eq!(
                error.to_js().get("message"),
                Some(&JsValue::String(JsString::from_utf16(expected)))
            );
        }
        std::fs::write(&fixture.manifests[0], "{").unwrap();
        params.insert("snapshotId".into(), fixture.ids[0].as_str().into());
        let error = routes
            .handle("snapshot_get", &JsValue::Object(params))
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(
            error.to_js().get("name"),
            Some(&JsValue::from("SyntaxError"))
        );
        assert_eq!(code(&error), JsValue::Undefined);
        assert!(!error.requires_host());
    }

    #[tokio::test]
    async fn snapshot_restore_validates_target_and_restores() {
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let snapshots = SnapshotProjection::new(None, Resolver, "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &snapshots,
        };
        for target in [JsValue::Null, JsValue::from(""), JsValue::from("\u{feff} ")] {
            let mut params = JsObject::new();
            params.insert("projectKey".into(), "p".into());
            params.insert("snapshotId".into(), "id".into());
            params.insert("targetProjectKey".into(), target);
            let error = routes
                .handle("snapshot_restore", &JsValue::Object(params))
                .unwrap()
                .await
                .unwrap_err();
            assert_eq!(code(&error), JsValue::from("PROJECT_NOT_FOUND"));
        }
        let error = routes
            .handle("snapshot_restore", &json(r#"{"projectKey":"p"}"#))
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("INVALID_MANAGEMENT_IDENTIFIER"));
        let error = routes
            .handle(
                "snapshot_restore",
                &json(r#"{"projectKey":"p","snapshotId":"id"}"#),
            )
            .unwrap()
            .await
            .unwrap_err();
        assert_eq!(code(&error), JsValue::from("CAPABILITY_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn injected_snapshot_restore_defaults_and_honors_target() {
        let fixture = SnapshotFixture::new();
        let memory = MemoryProjection::new(Arc::new(Registry::default()), "/unused");
        let routes = ManagementRoutes {
            memory: &memory,
            snapshots: &fixture.projection,
        };
        for target in [JsValue::Undefined, "target".into()] {
            let mut params = JsObject::new();
            params.insert("projectKey".into(), "p".into());
            params.insert("snapshotId".into(), fixture.ids[0].as_str().into());
            params.insert("targetProjectKey".into(), target);
            let result = routes
                .handle("snapshot_restore", &JsValue::Object(params))
                .unwrap()
                .await
                .unwrap();
            assert_eq!(result.get("restored"), Some(&JsValue::Bool(true)));
            let key = result.get("workspaceKey").and_then(JsValue::as_string);
            assert!(key.is_some_and(|key| !key.is_empty()));
        }
    }
}
