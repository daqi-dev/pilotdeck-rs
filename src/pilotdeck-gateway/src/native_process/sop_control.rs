//! StaffDeck control plane backed by the selected project generation and the
//! existing bridge, transcript catalog and SOP store owners.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    Options,
    methods::session_dir::{BoundedSessionMarkers, bounded_chat_dir, session_io_permit},
};
use crate::{
    config::Env,
    metadata::Metadata,
    module_host::approvals::{
        self, ApprovalAuthenticator, ApprovalBinding, ApprovalBindingResolver, ApprovalBridge,
        ApprovalBridgeImpl, ApprovalFailure, ApprovalPendingStore, ApprovalSubject,
        StoreResumeInput, StoreStatusInput,
        disk::DiskSopStore,
        native_auth::{
            AdmissionIdentity, FixedApprovalAuthenticator, NativeApprovalCatalog,
            NativeSessionResolver, authenticate_admission, installation_owner, read_admission,
        },
        native_http::{HyperJsonTransport, JsonTransport},
    },
    runtime::{
        connection::RequestError,
        manager::{SessionCatalogListInput, SessionCatalogPort, TranscriptSessionCatalog},
        project::{ProjectResources, ProjectRuntimeRegistry, RuntimeLease},
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
type Result<T> = std::result::Result<T, ApprovalFailure>;
type FutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
fn value<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn env(env: &Env, k: &str) -> String {
    env.get(k).cloned().unwrap_or_default()
}
fn fail(code: &str, message: impl Into<JsString>) -> ApprovalFailure {
    ApprovalFailure {
        status: None,
        code: Some(code.into()),
        message: Some(message.into()),
    }
}
fn plain(message: impl Into<JsString>) -> ApprovalFailure {
    ApprovalFailure {
        status: None,
        code: None,
        message: Some(message.into()),
    }
}
fn io_error(error: RequestError) -> ApprovalFailure {
    fail(&error.code, error.message)
}
pub(crate) fn request_error(error: ApprovalFailure) -> RequestError {
    let code = error
        .code
        .as_ref()
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_else(|| "gateway_request_failed".into());
    RequestError {
        message: error.message.unwrap_or_else(|| code.as_str().into()),
        code,
        details: None,
        validation: None,
    }
}
/// InProcessGateway.resumeSop wraps only callback failures, after the busy gate.
pub(crate) fn resume_error(error: ApprovalFailure) -> RequestError {
    let code = error
        .code
        .as_ref()
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_else(|| "SOP_RESUME_FAILED".into());
    let details = error
        .status
        .filter(|s| (400..=599).contains(s))
        .map(|status| {
            crate::metadata::object([("httpStatus", JsValue::Number(f64::from(status)))])
        });
    RequestError {
        message: error.message.unwrap_or_else(|| code.as_str().into()),
        code,
        details: details.map(Box::new),
        validation: None,
    }
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    let permit = session_io_permit().await.map_err(io_error)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|e| plain(e.to_string().as_str()))?
}
struct Profile {
    _lease: RuntimeLease,
    store: DiskSopStore,
}
#[derive(Clone)]
pub(crate) struct NativeSopControl {
    options: Options,
    env: Env,
    registry: Arc<ProjectRuntimeRegistry>,
    helper: PathBuf,
}
impl NativeSopControl {
    pub(crate) fn new(
        options: Options,
        env: Env,
        registry: Arc<ProjectRuntimeRegistry>,
    ) -> Result<Arc<Self>> {
        let helper = std::env::current_exe().map_err(|e| plain(e.to_string().as_str()))?;
        Ok(Arc::new(Self {
            options,
            env,
            registry,
            helper,
        }))
    }
    fn transport(&self, snapshot: &JsValue) -> Arc<dyn JsonTransport> {
        let proxy = snapshot
            .get("config")
            .and_then(|config| config.get("proxy"))
            .and_then(crate::runtime::model::http::ProxySettings::from_config);
        Arc::new(HyperJsonTransport::from_env(&self.env, proxy.as_ref()))
    }
    async fn snapshot(&self, project: Option<&JsValue>) -> Result<(RuntimeLease, Arc<JsValue>)> {
        let root = match project {
            None | Some(JsValue::Undefined | JsValue::Null) => self.options.project.clone(),
            Some(JsValue::String(key)) => super::project_scope::resolve(key).map_err(io_error)?,
            Some(other) => {
                let error = crate::session_model::path_argument(other);
                return Err(fail(&error.code, error.message));
            }
        };
        let registry = self.registry.clone();
        let lease = blocking(move || {
            registry
                .acquire(Some(&root))
                .map_err(|e| match e.js_error() {
                    Some(j) => fail(
                        j.code.as_deref().unwrap_or("gateway_request_failed"),
                        j.message.clone(),
                    ),
                    None => plain(e.message.as_str()),
                })
        })
        .await?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| plain("SOP project resources are unavailable."))?;
        let snapshot = resources.context().snapshot().clone();
        Ok((lease, snapshot))
    }
    async fn profile(&self, project: Option<&JsValue>) -> Result<Profile> {
        let (lease, snapshot) = self.snapshot(project).await?;
        let store = Self::store_for_snapshot(&snapshot)?;
        Ok(Profile {
            _lease: lease,
            store,
        })
    }
    fn store_for_snapshot(snapshot: &JsValue) -> Result<DiskSopStore> {
        let config = value(value(value(snapshot, "config"), "modules"), "sop").clone();
        if config.is_undefined() || matches!(config, JsValue::Null) {
            return Err(fail(
                "SOP_MODULE_DISABLED",
                "The StaffDeck SOP module is not enabled for this project.",
            ));
        }
        let state = value(&config, "stateRoot")
            .as_string()
            .ok_or_else(|| plain("SOP stateRoot is unavailable."))?
            .to_utf8()
            .map_err(|_| plain("SOP stateRoot cannot be represented by this host."))?;
        Ok(DiskSopStore::new(PathBuf::from(state).join("sessions")))
    }
    async fn original_admission(
        &self,
        project: &JsString,
        session: &JsString,
    ) -> Result<Option<JsValue>> {
        let home = self.options.home.clone();
        let p = project.clone();
        let id = session.clone();
        blocking(move || {
            let root = super::project_scope::resolve(&p).map_err(io_error)?;
            let path = bounded_chat_dir(&home, &root)
                .map_err(io_error)?
                .join(format!(
                    "{}.jsonl",
                    crate::storage::sanitize_session_id(&id)
                ));
            let read = crate::storage::read_transcript_bounded(&path)
                .map_err(|e| plain(e.to_string().as_str()))?;
            if !read.read.diagnostics.is_empty() {
                return Ok(None);
            }
            Ok(read_admission(&read.read.entries, &id))
        })
        .await
    }
    async fn same_snapshot_admission(
        &self,
        project: &JsString,
        snapshot: &JsValue,
        original: &JsValue,
    ) -> Result<bool> {
        let user = value(original, "pilotDeckUserId")
            .as_string()
            .cloned()
            .ok_or_else(|| plain("SOP admission owner is unavailable."))?;
        let current = self
            .admission_for_snapshot(project, snapshot, &user)
            .await?;
        Ok(current.as_ref().is_some_and(|v| {
            [
                "tenantId",
                "agentId",
                "pilotDeckUserId",
                "projectKey",
                "actorUserId",
                "credentialId",
                "staffDeckOrigin",
            ]
            .iter()
            .all(|k| v.get(k) == original.get(k))
        }))
    }
    /// Read original authority through the caller's retained project generation.
    /// Cross-project acquisition occurs only after owner/project/session gates.
    pub(crate) async fn authority(
        &self,
        input: &JsValue,
        principal: &crate::module_host::PublicHostPrincipal,
        generation: &RuntimeLease,
    ) -> Result<JsValue> {
        let input = authority_input(input)?;
        if installation_owner(&self.helper, &self.database())
            .await
            .as_ref()
            != Some(&principal.pilot_deck_user_id)
        {
            return Err(authority_failure("SOP_AUTHORITY_SESSION_DENIED"));
        }
        let projects = self.projects().await?;
        if projects
            .iter()
            .filter(|project| **project == input.project)
            .count()
            != 1
            || !self.has_session(&input.project, &input.session).await?
        {
            return Err(authority_failure("SOP_AUTHORITY_SESSION_DENIED"));
        }
        let root = super::project_scope::resolve(&input.project).map_err(io_error)?;
        // Authorize existence before acquiring any other project's configuration.
        let (_selected_lease, snapshot) = if generation.runtime().project_root() == root {
            let resources = generation
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| plain("SOP project resources are unavailable."))?;
            (None, resources.context().snapshot())
        } else {
            let (lease, snapshot) = self
                .snapshot(Some(&JsValue::String(input.project.clone())))
                .await?;
            (Some(lease), snapshot)
        };
        let original = self
            .original_admission(&input.project, &input.session)
            .await?
            .ok_or_else(|| authority_failure("SOP_AUTHORITY_ADMISSION_MISMATCH"))?;
        if !self
            .same_snapshot_admission(&input.project, &snapshot, &original)
            .await?
            || !authority_admission_matches(&input, principal, &original)
        {
            return Err(authority_failure("SOP_AUTHORITY_ADMISSION_MISMATCH"));
        }
        let state = Self::store_for_snapshot(&snapshot)?
            .authority(&input.session, input.revision)
            .await
            .map_err(|error| match error.code.as_str() {
                "SOP_REVISION_CONFLICT" | "SOP_AUTHORITY_STATE_INACTIVE" => {
                    authority_failure(&error.code)
                }
                _ => error.approval(),
            })?;
        Ok(authority_merge(input.context, original, state))
    }
    pub(crate) fn bridge(self: &Arc<Self>) -> Arc<dyn ApprovalBridge> {
        Arc::new(ApprovalBridgeImpl {
            binding: self.static_binding(),
            resolve_binding: (env(&self.env, "STAFFDECK_PROJECT_BINDING_ENABLED") == "true")
                .then(|| self.clone() as Arc<dyn ApprovalBindingResolver>),
            authenticate: self.clone(),
            resolve_session: Some(Arc::new(NativeSessionResolver {
                database: self.database(),
                helper: self.helper.clone(),
                catalog: self.clone(),
            })),
            store: self.clone(),
        })
    }
    fn static_binding(&self) -> ApprovalBinding {
        ApprovalBinding {
            tenant_id: env(&self.env, "STAFFDECK_COPY_TENANT_ID").into(),
            agent_id: env(&self.env, "STAFFDECK_COPY_TARGET_AGENT_ID").into(),
            pilot_deck_user_id: env(&self.env, "STAFFDECK_COPY_PILOTDECK_USER_ID").into(),
        }
    }
    fn database(&self) -> PathBuf {
        self.env
            .get("DATABASE_PATH")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.options.home.join("auth.db"))
    }
    pub(crate) async fn status(self: &Arc<Self>, input: &JsValue) -> Result<JsValue> {
        let authorization =
            super::methods::input_property(input, "approverAuthorization").map_err(io_error)?;
        if authorization
            .as_ref()
            .is_some_and(crate::runtime::turn::submit::truthy)
        {
            return self.bridge().status(input).await;
        }
        let profile = self.profile(input.get("projectKey")).await?;
        let session = string(input, "sessionKey")?;
        let Some(snapshot) = profile
            .store
            .status(&session)
            .await
            .map_err(|e| e.approval())?
        else {
            return Ok(JsValue::Null);
        };
        let wait = value(&snapshot, "wait");
        if value(wait, "kind") == &JsValue::from("external_task") {
            let mut output = pilotdeck_protocol::JsObject::new();
            if let Some(object) = snapshot.as_object() {
                for (k, v) in object.own_entries() {
                    if !k.equals_str("approval") && !k.equals_str("approvalError") {
                        output.insert(k.clone(), v.clone());
                    }
                }
            }
            return Ok(JsValue::Object(output));
        }
        if !crate::runtime::turn::submit::truthy(wait) {
            return Ok(JsValue::Null);
        }
        let mut error = fail("APPROVAL_AUTH_REQUIRED", "APPROVAL_AUTH_REQUIRED");
        error.status = Some(401);
        Err(error)
    }
    /// The caller retains InProcessGateway's active-turn guard before this callback.
    pub(crate) async fn resume(self: &Arc<Self>, input: &JsValue) -> Result<JsValue> {
        let _ = super::methods::input_property(input, "sessionKey").map_err(io_error)?;
        if input
            .as_object()
            .is_some_and(|o| o.contains_key("authority") || o.contains_key("subject"))
        {
            let mut e = fail("APPROVAL_AUTHORITY_OVERRIDE", "APPROVAL_AUTHORITY_OVERRIDE");
            e.status = Some(400);
            return Err(e);
        }
        if value(input, "source") == &JsValue::from("human") {
            return self.bridge().resume(input).await;
        }
        let profile = self.profile(input.get("projectKey")).await?;
        // Profile resolution precedes the store's state/receipt checks. Retain
        // wire values through that single locked transaction; do not reinterpret
        // an unknown source as an authenticated or external-task continuation.
        profile
            .store
            .resume_command(approvals::StoreResumeCommand::wire(
                string(input, "sessionKey")?,
                input,
            ))
            .await
            .map_err(|e| e.approval())
    }
    pub(crate) async fn creation_admission(
        &self,
        project: &JsString,
        snapshot: &JsValue,
    ) -> Option<JsValue> {
        let user: JsString = env(&self.env, "STAFFDECK_COPY_PILOTDECK_USER_ID").into();
        if user.is_empty()
            || installation_owner(&self.helper, &self.database())
                .await
                .as_ref()
                != Some(&user)
        {
            return None;
        }
        // Creation records an optional capability, not an approval response.
        // Keep its existing fail-closed optional contract. Approval reads below
        // propagate the same checked lookup failure instead of converting it to
        // an authenticated denial.
        self.admission_for_snapshot(project, snapshot, &user)
            .await
            .ok()
            .flatten()
    }
    /// Uses a supplied retained generation for creation admission; it never
    /// reacquires whatever registry generation happened to become current.
    pub(crate) async fn admission_for_snapshot(
        &self,
        project: &JsString,
        snapshot: &JsValue,
        pilot_user: &JsString,
    ) -> Result<Option<JsValue>> {
        let home = self.options.home.clone();
        let envs = self.env.clone();
        let Ok(root) = project.to_utf8() else {
            return Ok(None);
        };
        let agent = blocking(move || {
            crate::config::project_binding_target_bounded(&home, &envs, &root)
                .map_err(|e| fail(&e.code, e.message))
        })
        .await
        .map_err(|mut error| {
            // A temporary/unsafe binding read is unavailable, not proof that
            // this authenticated session lacks authorization. Preserve the
            // underlying code/message and make the HTTP failure retryable.
            error.status = Some(503);
            error
        })?;
        let Some(agent) = agent else {
            return Ok(None);
        };
        let sop = value(value(value(snapshot, "config"), "modules"), "sop");
        if !sop.is_undefined() && value(sop, "discoveryAgentId") != &JsValue::String(agent.clone())
        {
            return Ok(None);
        }
        let origin = env(&self.env, "STAFFDECK_FORMAL_API_ORIGIN");
        let identity = AdmissionIdentity {
            origin: origin.clone(),
            tenant: env(&self.env, "STAFFDECK_COPY_TENANT_ID").into(),
            agent: agent.clone(),
            actor: env(&self.env, "STAFFDECK_COPY_ACTOR_USER_ID").into(),
            credential: env(&self.env, "STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID").into(),
            actor_bearer: env(&self.env, "STAFFDECK_COPY_USER_TOKEN"),
            discovery_endpoint: value(sop, "discoveryEndpoint")
                .as_string()
                .map(|s| String::from_utf16_lossy(s.units()))
                .unwrap_or_else(|| format!("{origin}/api/v1")),
            discovery_agent: value(sop, "discoveryAgentId")
                .as_string()
                .cloned()
                .unwrap_or(agent),
            discovery_key: value(sop, "discoveryApiKey")
                .as_string()
                .cloned()
                .unwrap_or_else(|| env(&self.env, "STAFFDECK_SOP_MANAGEMENT_API_KEY").into()),
            pilot_user: pilot_user.clone(),
            expected_pilot_user: env(&self.env, "STAFFDECK_COPY_PILOTDECK_USER_ID").into(),
        };
        Ok(authenticate_admission(
            &identity,
            project,
            self.transport(snapshot).as_ref(),
            epoch_ms(),
        )
        .await)
    }
}
struct AuthorityInput {
    session: JsString,
    project: JsString,
    credential: JsString,
    revision: f64,
    context: JsValue,
}
fn authority_failure(code: &str) -> ApprovalFailure {
    ApprovalFailure {
        status: Some(if code.contains("REVISION") { 409 } else { 403 }),
        code: Some(code.into()),
        message: Some(code.into()),
    }
}
fn authority_input(input: &JsValue) -> Result<AuthorityInput> {
    let invalid = || authority_failure("SOP_AUTHORITY_INPUT_INVALID");
    let object = input.as_object().ok_or_else(invalid)?;
    if object.own_entries().iter().any(|(key, _)| {
        ![
            "sessionKey",
            "projectKey",
            "expectedRevision",
            "requestId",
            "admissionCredentialId",
        ]
        .iter()
        .any(|allowed| key.equals_str(allowed))
    }) {
        return Err(invalid());
    }
    let string = |key| {
        value(input, key)
            .as_string()
            .filter(|s| !crate::model_catalog::trim(s).is_empty())
            .cloned()
            .ok_or_else(invalid)
    };
    let session = string("sessionKey")?;
    let project = string("projectKey")?;
    let credential = string("admissionCredentialId")?;
    let _request = string("requestId")?;
    let revision = value(input, "expectedRevision")
        .as_number()
        .filter(|n| n.is_finite() && *n >= 1.0 && n.fract() == 0.0)
        .ok_or_else(invalid)?;
    let context = object.clone();
    let context = {
        let mut public = pilotdeck_protocol::JsObject::new();
        for (key, value) in context.entries() {
            if !key.equals_str("admissionCredentialId") {
                public.insert(key.clone(), value.clone());
            }
        }
        public
    };
    Ok(AuthorityInput {
        session,
        project,
        credential,
        revision,
        context: JsValue::Object(context),
    })
}
fn authority_admission_matches(
    input: &AuthorityInput,
    principal: &crate::module_host::PublicHostPrincipal,
    admission: &JsValue,
) -> bool {
    [
        ("projectKey", &input.project),
        ("pilotDeckUserId", &principal.pilot_deck_user_id),
        ("tenantId", &principal.tenant_id),
        ("actorUserId", &principal.actor_user_id),
        ("agentId", &principal.agent_id),
        ("credentialId", &input.credential),
    ]
    .iter()
    .all(|(key, expected)| value(admission, key) == &JsValue::String((*expected).clone()))
}
fn authority_merge(context: JsValue, admission: JsValue, state: JsValue) -> JsValue {
    let mut result = context
        .as_object()
        .expect("validated authority context")
        .clone();
    result.insert("admission".into(), admission);
    if let Some(state) = state.as_object() {
        for (key, value) in state.own_entries() {
            result.insert(key.clone(), value.clone());
        }
    }
    JsValue::Object(result)
}
fn string(input: &JsValue, key: &str) -> Result<JsString> {
    value(input, key).as_string().cloned().ok_or_else(|| {
        fail(
            "SOP_RESUME_INPUT_INVALID",
            format!("SOP {key} must be a string.").as_str(),
        )
    })
}
fn epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}
impl ApprovalPendingStore for NativeSopControl {
    fn status<'a>(&'a self, input: &'a StoreStatusInput) -> FutureResult<'a, Option<JsValue>> {
        Box::pin(async move {
            let profile = self.profile(input.project_key.as_ref()).await?;
            profile
                .store
                .status(&input.session_key)
                .await
                .map_err(|e| e.approval())
        })
    }
    fn resume<'a>(&'a self, input: &'a StoreResumeInput) -> FutureResult<'a, JsValue> {
        Box::pin(async move {
            let profile = self.profile(input.project_key.as_ref()).await?;
            let authority = input.authority.as_ref().map(|a| approvals::StoreAuthority {
                tenant_id: a.tenant_id.clone(),
                session_id: a.session_id.clone(),
                subject: ApprovalSubject {
                    tenant_id: a.subject.tenant_id.clone(),
                    source: a.subject.source.clone(),
                    disabled: a.subject.disabled,
                    user_id: a.subject.user_id.clone(),
                    role: a.subject.role.clone(),
                },
            });
            profile
                .store
                .resume(StoreResumeInput {
                    session_key: input.session_key.clone(),
                    project_key: input.project_key.clone(),
                    request_id: input.request_id.clone(),
                    wait_id: input.wait_id.clone(),
                    message: input.message.clone(),
                    expected_revision: input.expected_revision,
                    slot_updates: input.slot_updates.clone(),
                    source: input.source,
                    authority,
                })
                .await
                .map_err(|e| e.approval())
        })
    }
}
impl ApprovalAuthenticator for NativeSopControl {
    fn authenticate<'a>(&'a self, bearer: &'a JsString) -> FutureResult<'a, ApprovalSubject> {
        Box::pin(async move {
            let origin = env(&self.env, "STAFFDECK_FORMAL_API_ORIGIN");
            let approver = env(&self.env, "STAFFDECK_APPROVAL_USER_ID");
            if origin.is_empty() || approver.is_empty() {
                let mut e = fail(
                    "APPROVAL_BINDING_UNAVAILABLE",
                    "APPROVAL_BINDING_UNAVAILABLE",
                );
                e.status = Some(503);
                return Err(e);
            };
            // Approver identity is installation-wide, before project/session
            // authorization. Use the listener's project generation, as the TS
            // startup global dispatcher does; admission below uses its own
            // retained project snapshot instead.
            let (_lease, snapshot) = self.snapshot(None).await?;
            FixedApprovalAuthenticator::new(
                &origin,
                env(&self.env, "STAFFDECK_COPY_TENANT_ID").into(),
                approver.into(),
                self.transport(&snapshot),
            )?
            .authenticate(bearer)
            .await
        })
    }
}
impl NativeApprovalCatalog for NativeSopControl {
    fn projects(&self) -> FutureResult<'_, Vec<JsString>> {
        let home = self.options.home.clone();
        Box::pin(blocking(move || {
            let metadata = Metadata {
                home: home.clone(),
                cwd: PathBuf::new(),
                files: BoundedSessionMarkers::new(),
            };
            let listed = metadata
                .list_projects()
                .map_err(|e| plain(e.to_string().as_str()))?;
            if metadata.files.was_refused() {
                return Err(plain("Unsafe project marker."));
            }
            let mut output = vec![JsString::from(home.to_string_lossy().into_owned())];
            if let JsValue::Array(rows) = value(&listed, "projects") {
                for row in rows {
                    if let Some(root) = value(row, "projectKey").as_string()
                        && !output.contains(root)
                    {
                        output.push(root.clone());
                    }
                }
            }
            Ok(output)
        }))
    }
    fn has_session<'a>(
        &'a self,
        project: &'a JsString,
        session: &'a JsString,
    ) -> FutureResult<'a, bool> {
        let home = self.options.home.clone();
        let project = project.clone();
        let session = session.clone();
        Box::pin(blocking(move || {
            let root = super::project_scope::resolve(&project).map_err(io_error)?;
            let chat = bounded_chat_dir(&home, &root).map_err(io_error)?;
            let catalog = TranscriptSessionCatalog::new_bounded(move |_| Ok(chat.clone()));
            let mut input = SessionCatalogListInput::new(root, home);
            input.include_internal = true;
            let rows = catalog
                .list(&input)
                .map_err(|e| plain(e.message().clone()))?;
            Ok(rows
                .iter()
                .any(|v| value(v, "sessionId") == &JsValue::String(session.clone())))
        }))
    }
    fn admission<'a>(
        &'a self,
        project: &'a JsString,
        session: &'a JsString,
    ) -> FutureResult<'a, Option<JsValue>> {
        Box::pin(async move {
            let original = self.original_admission(project, session).await?;
            let Some(original) = original else {
                return Ok(None);
            };
            let (_lease, snapshot) = self
                .snapshot(Some(&JsValue::String(project.clone())))
                .await?;
            if !self
                .same_snapshot_admission(project, &snapshot, &original)
                .await?
            {
                return Ok(None);
            }
            Ok(Some(original))
        })
    }
}
impl ApprovalBindingResolver for NativeSopControl {
    fn resolve<'a>(&'a self, input: &'a JsValue) -> FutureResult<'a, ApprovalBinding> {
        Box::pin(async move {
            let projects = if crate::runtime::turn::submit::truthy(value(input, "projectKey")) {
                let raw = value(input, "projectKey").clone();
                let home = self.options.home.clone();
                let root = blocking(move || {
                    super::project_scope::registered(&home, &raw).map_err(io_error)
                })
                .await?;
                vec![root.to_string_lossy().into_owned().into()]
            } else {
                self.projects().await?
            };
            let session = string(input, "sessionKey")?;
            let mut found = None;
            for root in projects {
                if let Some(admission) = self.admission(&root, &session).await? {
                    if found.is_some() {
                        return Err(forbidden());
                    }
                    found = Some(admission)
                }
            }
            let admission = found.ok_or_else(forbidden)?;
            Ok(ApprovalBinding {
                tenant_id: value(&admission, "tenantId")
                    .as_string()
                    .expect("validated admission")
                    .clone(),
                agent_id: value(&admission, "agentId")
                    .as_string()
                    .expect("validated admission")
                    .clone(),
                pilot_deck_user_id: value(&admission, "pilotDeckUserId")
                    .as_string()
                    .expect("validated admission")
                    .clone(),
            })
        })
    }
}
fn forbidden() -> ApprovalFailure {
    ApprovalFailure {
        status: Some(403),
        code: None,
        message: Some("SOP_APPROVAL_SESSION_FORBIDDEN".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_native_control_uses_project_profile_and_existing_bridge_order() {
        let (_scratch, requests) = super::super::management_tests::setup();
        let control = NativeSopControl::new(
            requests.options.clone(),
            requests.env.clone(),
            requests.project.clone(),
        )
        .unwrap();
        let input = crate::metadata::object([("sessionKey", "s1".into())]);
        let error = control.status(&input).await.unwrap_err();
        assert_eq!(error.code, Some("SOP_MODULE_DISABLED".into()));
        assert_eq!(
            error.message,
            Some("The StaffDeck SOP module is not enabled for this project.".into())
        );
        // The fixed TS disabled-invalid oracle belongs to the native profile
        // boundary, before even an invalid non-human wire source reaches disk.
        let disabled_resume = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("source", "bogus".into()),
            ("requestId", JsValue::Number(3.0)),
        ]);
        let error = control.resume(&disabled_resume).await.unwrap_err();
        assert_eq!(error.code, Some("SOP_MODULE_DISABLED".into()));
        assert_eq!(
            error.message,
            Some("The StaffDeck SOP module is not enabled for this project.".into())
        );
        let override_input =
            crate::metadata::object([("sessionKey", "s1".into()), ("authority", JsValue::Null)]);
        let error = control.resume(&override_input).await.unwrap_err();
        assert_eq!(error.code, Some("APPROVAL_AUTHORITY_OVERRIDE".into()));
        let error = control.bridge().status(&input).await.unwrap_err();
        assert_eq!(error.code, Some("APPROVAL_BINDING_UNAVAILABLE".into()));
        assert_eq!(error.status, Some(503));
        let projects = control.projects().await.unwrap();
        assert!(projects.contains(&JsString::from(
            requests.options.home.to_string_lossy().into_owned()
        )));
        assert!(projects.contains(&JsString::from(
            requests.options.project.to_string_lossy().into_owned()
        )));
        assert!(
            !control
                .has_session(
                    &requests
                        .options
                        .project
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    &"sop-missing".into()
                )
                .await
                .unwrap()
        );
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn approver_authentication_uses_project_proxy_without_proxy_environment() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn headers(socket: &mut tokio::net::TcpStream) -> String {
            let mut bytes = Vec::new();
            loop {
                let byte = socket.read_u8().await.unwrap();
                bytes.push(byte);
                assert!(bytes.len() < 16384);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            String::from_utf8(bytes).unwrap()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(10), async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let connect = headers(&mut socket).await;
                assert!(connect.starts_with("CONNECT staffdeck-proxy-test.invalid:80 HTTP/1.1\r\n"), "{connect}");
                socket.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                let request = headers(&mut socket).await;
                assert!(request.starts_with("GET /api/auth/me HTTP/1.1\r\n"), "{request}");
                assert!(request.to_lowercase().contains("authorization: bearer isolated-approver\r\n"));
                let body = r#"{"tenant_id":"tenant","id":"approver","source":"web","role":"member"}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }).await.unwrap();
        });
        let (_scratch, requests) = super::super::management_tests::setup();
        let file = requests.options.home.join("pilotdeck.yaml");
        let mut config = std::fs::read_to_string(&file).unwrap();
        config.push_str(&format!("proxy:\n  url: http://{address}\n"));
        std::fs::write(file, config).unwrap();
        let mut environment = requests.env.clone();
        for (key, value) in [
            (
                "STAFFDECK_FORMAL_API_ORIGIN",
                "http://staffdeck-proxy-test.invalid",
            ),
            ("STAFFDECK_COPY_TENANT_ID", "tenant"),
            ("STAFFDECK_APPROVAL_USER_ID", "approver"),
        ] {
            environment.insert(key.into(), value.into());
        }
        assert!(
            !environment
                .keys()
                .any(|key| key.to_ascii_lowercase().ends_with("proxy"))
        );
        let control = NativeSopControl::new(
            requests.options.clone(),
            environment,
            requests.project.clone(),
        )
        .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            control.authenticate(&"isolated-approver".into()),
        )
        .await
        .unwrap();
        // Settle both owners even when a regression makes authentication fail.
        let server_result = server.await;
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
        server_result.unwrap();
        let subject = result.unwrap();
        assert!(subject.user_id.equals_str("approver"));
        assert!(subject.tenant_id.equals_str("tenant"));
    }

    #[test]
    fn admission_target_is_fresh_and_does_not_read_definitions_or_write_config() {
        let root = std::env::temp_dir().join(format!("pdrs-sop-binding-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("staffdeck")).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let file = root.join("staffdeck/project-bindings.json");
        let row = |status: &str| {
            crate::metadata::object([
                ("version", JsValue::Number(1.0)),
                ("installationId", "fixture-install".into()),
                (
                    "projects",
                    JsValue::Array(vec![crate::metadata::object([
                        ("projectRoot", project.to_string_lossy().as_ref().into()),
                        ("tenantId", "tenant".into()),
                        ("origin", "https://example.invalid".into()),
                        ("agentId", "target".into()),
                        ("status", status.into()),
                        ("bindingId", "missing-definitions".into()),
                    ])]),
                ),
            ])
        };
        let mut environment = Env::new();
        for (k, v) in [
            ("STAFFDECK_PROJECT_BINDING_ENABLED", "true"),
            ("STAFFDECK_COPY_TENANT_ID", "tenant"),
            ("STAFFDECK_FORMAL_API_ORIGIN", "https://example.invalid/"),
            ("STAFFDECK_COPY_TARGET_AGENT_ID", "stale-cached-target"),
        ] {
            environment.insert(k.into(), v.into());
        }
        std::fs::write(
            &file,
            pilotdeck_protocol::stringify_json(&row("bound"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            crate::config::project_binding_target_bounded(
                &root,
                &environment,
                &project.to_string_lossy()
            )
            .unwrap(),
            Some("target".into())
        );
        std::fs::write(
            &file,
            pilotdeck_protocol::stringify_json(&row("revoked"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let original = std::fs::read(&file).unwrap();
        assert_eq!(
            crate::config::project_binding_target_bounded(
                &root,
                &environment,
                &project.to_string_lossy()
            )
            .unwrap(),
            None
        );
        assert_eq!(std::fs::read(&file).unwrap(), original);
        assert!(!root.join("staffdeck/sop-bundles").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn native_resume_passes_raw_values_to_the_locked_store() {
        let (_scratch, requests) = super::super::management_tests::setup();
        let file = requests.options.home.join("pilotdeck.yaml");
        let mut config = std::fs::read_to_string(&file).unwrap();
        config.push_str("modules:\n  sop:\n    enabled: true\n    provider: staffdeck\n    endpoint: http://127.0.0.1:1\n    definitionsPath: fixture-sops.json\n    defaultSopId: fixture\n");
        std::fs::write(&file, &config).unwrap();
        let control = NativeSopControl::new(
            requests.options.clone(),
            requests.env.clone(),
            requests.project.clone(),
        )
        .unwrap();
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../tests/fixtures/sop-resume-wire/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(rows) = value(&oracle, "cases") else {
            panic!("oracle cases")
        };
        let find = |name: &str| {
            rows.iter()
                .find(|row| value(row, "name") == &JsValue::from(name))
                .unwrap()
        };
        let row = find("waiting-array-slots");
        let JsValue::Array(steps) = value(row, "steps") else {
            panic!("oracle steps")
        };
        let input = value(&steps[0], "input");
        // Before any state exists, a malformed source must still reach the
        // missing-state branch; no external source default is introduced.
        let missing = find("missing-unknown-source");
        let JsValue::Array(missing_steps) = value(missing, "steps") else {
            panic!("oracle steps")
        };
        let error = control
            .resume(value(&missing_steps[0], "input"))
            .await
            .unwrap_err();
        assert_eq!(error.code, Some("SOP_SESSION_NOT_FOUND".into()));
        let profile = control.profile(None).await.unwrap();
        let path = profile.store.path(&"s1".into());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                pilotdeck_protocol::stringify_json(value(row, "initial"))
                    .unwrap()
                    .unwrap()
            ),
        )
        .unwrap();
        let actual = control.resume(input).await.unwrap();
        assert_eq!(actual, value(value(&steps[0], "outcome"), "result").clone());
        let persisted =
            pilotdeck_protocol::parse_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value(value(&persisted, "state"), "slots_json"),
            value(value(value(&steps[0], "after"), "state"), "slots_json")
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), config);
        drop(profile);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn approval_read_distinguishes_unavailable_binding_from_missing_authority() {
        let (_scratch, requests) = super::super::management_tests::setup();
        // The project registry intentionally keeps the fixture's ordinary
        // config. This fresh authorization lookup uses the control env, so a
        // damaged binding is observed here after the retained snapshot exists.
        let mut environment = requests.env.clone();
        environment.insert("STAFFDECK_PROJECT_BINDING_ENABLED".into(), "true".into());
        let control = NativeSopControl::new(
            requests.options.clone(),
            environment,
            requests.project.clone(),
        )
        .unwrap();
        let project: JsString = requests
            .options
            .project
            .to_string_lossy()
            .into_owned()
            .into();
        let session: JsString = "sop-auth-binding".into();
        let admission = crate::metadata::object([
            ("tenantId", "tenant".into()),
            ("agentId", "agent".into()),
            ("pilotDeckUserId", "owner".into()),
            ("projectKey", JsValue::String(project.clone())),
            ("actorUserId", "actor".into()),
            ("credentialId", "credential".into()),
            ("staffDeckOrigin", "http://127.0.0.1:1".into()),
        ]);
        let entry = crate::metadata::object([
            ("type", "session_metadata".into()),
            ("sessionId", JsValue::String(session.clone())),
            ("turnId", "host-admission".into()),
            ("createdAt", "2026-01-02T03:04:05.000Z".into()),
            ("sequence", JsValue::Number(1.0)),
            (
                "metadata",
                crate::metadata::object([("staffDeckAdmission", admission)]),
            ),
        ]);
        let chat = bounded_chat_dir(&requests.options.home, &requests.options.project).unwrap();
        std::fs::create_dir_all(&chat).unwrap();
        let transcript = chat.join("sop-auth-binding.jsonl");
        let bytes = format!(
            "{}\n",
            pilotdeck_protocol::stringify_json(&entry).unwrap().unwrap()
        );
        std::fs::write(&transcript, &bytes).unwrap();
        let decoded = crate::storage::read_transcript_bounded(&transcript).unwrap();
        assert!(
            decoded.read.diagnostics.is_empty(),
            "{:?}",
            decoded.read.diagnostics
        );
        assert_eq!(decoded.read.entries.len(), 1);
        assert!(read_admission(&decoded.read.entries, &session).is_some());
        let input = crate::metadata::object([
            ("sessionKey", JsValue::String(session)),
            ("projectKey", JsValue::String(project)),
        ]);
        // Actual resolver consumes the transcript and same live profile; no
        // binding is a genuine 403, not an IO error.
        let missing = control.resolve(&input).await.err().unwrap();
        assert_eq!(missing.status, Some(403));
        assert_eq!(
            missing.message,
            Some("SOP_APPROVAL_SESSION_FORBIDDEN".into())
        );
        let binding = requests
            .options
            .home
            .join("staffdeck/project-bindings.json");
        std::fs::create_dir_all(binding.parent().unwrap()).unwrap();
        std::fs::write(&binding, b"{").unwrap();
        let invalid = control.resolve(&input).await.err().unwrap();
        assert_eq!(invalid.status, Some(503));
        assert_eq!(invalid.code, Some("PROJECT_BINDING_STORE_INVALID".into()));
        assert_eq!(
            invalid.message,
            Some("StaffDeck binding storage is invalid.".into())
        );
        let projected = resume_error(invalid);
        assert_eq!(projected.code, "PROJECT_BINDING_STORE_INVALID");
        assert_eq!(
            projected
                .details
                .as_deref()
                .and_then(|v| v.get("httpStatus")),
            Some(&JsValue::Number(503.0))
        );
        assert_eq!(std::fs::read(&binding).unwrap(), b"{");
        // Mode bits do not deny root. Keep the corrupted/directory cases
        // portable; report the permission subcase as unsupported under root.
        if !rustix::process::geteuid().is_root() {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(&binding).unwrap().permissions();
            std::fs::set_permissions(&binding, std::fs::Permissions::from_mode(0o0)).unwrap();
            assert_eq!(
                std::fs::read(&binding).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            let denied = control.resolve(&input).await.err().unwrap();
            std::fs::set_permissions(&binding, permissions).unwrap();
            assert_eq!(denied.status, Some(503));
            // Config's selected_binding maps a known lookup failure (including
            // PermissionDenied) to the original invalid-store code. The
            // approval boundary preserves that code while making it a 503.
            assert_eq!(denied.code, Some("PROJECT_BINDING_STORE_INVALID".into()));
            assert_eq!(
                denied.message,
                Some("StaffDeck binding storage is invalid.".into())
            );
            println!("SOP_BINDING_PERMISSION_CASE=checked");
        } else {
            println!("SOP_BINDING_PERMISSION_CASE=unsupported-root");
        }
        std::fs::remove_file(&binding).unwrap();
        std::fs::create_dir(&binding).unwrap();
        let directory = control.resolve(&input).await.err().unwrap();
        assert_eq!(directory.status, Some(503));
        assert_eq!(directory.code, Some("CONFIG_READ_NOT_REGULAR".into()));
        assert_eq!(
            directory.message,
            Some("Configuration input must be a regular file.".into())
        );
        assert!(binding.is_dir());
        std::fs::remove_dir(&binding).unwrap();
        let absent_again = control.resolve(&input).await.err().unwrap();
        assert_eq!(absent_again.status, Some(403));
        assert_eq!(std::fs::read_to_string(&transcript).unwrap(), bytes);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}

#[cfg(test)]
mod resume_error_tests {
    use super::*;
    #[test]
    fn resume_callback_preserves_plain_message_and_only_valid_http_status() {
        for status in [None, Some(399), Some(400), Some(599), Some(600)] {
            let error = resume_error(ApprovalFailure {
                status,
                code: None,
                message: Some("real owner failure".into()),
            });
            assert_eq!(error.code, "SOP_RESUME_FAILED");
            assert_eq!(error.message, JsString::from("real owner failure"));
            assert_eq!(
                error.details,
                status
                    .filter(|s| (400..=599).contains(s))
                    .map(|s| crate::metadata::object([(
                        "httpStatus",
                        JsValue::Number(f64::from(s))
                    )]))
                    .map(Box::new)
            );
        }
        let error = resume_error(ApprovalFailure {
            status: Some(409),
            code: Some("SOP_REVISION_CONFLICT".into()),
            message: Some("Expected SOP revision 2, found 3.".into()),
        });
        assert_eq!(error.code, "SOP_REVISION_CONFLICT");
        assert_eq!(
            error.message,
            JsString::from("Expected SOP revision 2, found 3.")
        );
        assert_eq!(
            error.details,
            Some(Box::new(crate::metadata::object([(
                "httpStatus",
                JsValue::Number(409.0)
            )])))
        );
    }
}

#[cfg(test)]
#[path = "sop_control_authority_tests.rs"]
mod authority_tests;
