//! Native implementations of the existing approval bridge ports. The formal
//! approver credential is authenticated separately from the host bearer.
use super::native_http::{JsonRequest, JsonTransport};
use super::{
    ApprovalAuthenticator, ApprovalBinding, ApprovalFailure, ApprovalSession,
    ApprovalSessionResolver, ApprovalSubject, failure,
};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub(crate) struct FixedApprovalAuthenticator {
    origin: url::Url,
    tenant: JsString,
    approver: JsString,
    http: Arc<dyn JsonTransport>,
}
impl FixedApprovalAuthenticator {
    pub(crate) fn new(
        origin: &str,
        tenant: JsString,
        approver: JsString,
        http: Arc<dyn JsonTransport>,
    ) -> Result<Self, ApprovalFailure> {
        let origin = url::Url::parse(origin).map_err(|_| ApprovalFailure {
            status: None,
            code: Some("ERR_INVALID_URL".into()),
            message: Some("Invalid URL".into()),
        })?;
        if !matches!(origin.scheme(), "http" | "https")
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(authority_failure(
                400,
                "APPROVAL_CONFIG_INVALID",
                "A formal StaffDeck origin is required.",
            ));
        }
        if !nonempty(&tenant) || !nonempty(&approver) {
            return Err(authority_failure(
                400,
                "APPROVAL_CONFIG_INVALID",
                "Recorded tenant and approver IDs are required.",
            ));
        }
        Ok(Self {
            origin,
            tenant,
            approver,
            http,
        })
    }
}
fn authority_failure(status: u16, code: &str, message: &str) -> ApprovalFailure {
    ApprovalFailure {
        status: Some(status),
        code: Some(code.into()),
        message: Some(message.into()),
    }
}
fn authority_exception(message: impl Into<JsString>) -> ApprovalFailure {
    ApprovalFailure {
        status: None,
        code: None,
        message: Some(message.into()),
    }
}
fn nonempty(value: &JsString) -> bool {
    !crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()
}
impl ApprovalAuthenticator for FixedApprovalAuthenticator {
    fn authenticate<'a>(
        &'a self,
        bearer: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalSubject, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            if !nonempty(bearer) {
                return Err(authority_failure(
                    401,
                    "APPROVAL_AUTH_REQUIRED",
                    "The approver must sign in.",
                ));
            }
            let bearer = bearer
                .to_utf8()
                .map_err(|_| authority_exception("fetch failed"))?;
            let response = self
                .http
                .request(JsonRequest {
                    method: "GET",
                    url: self
                        .origin
                        .join("/api/auth/me")
                        .map_err(|_| failure(400, "APPROVAL_CONFIG_INVALID"))?,
                    bearer: Some(bearer),
                    body: None,
                    parse_error_body: false,
                    redirect_error: true,
                    timeout: Some(Duration::from_secs(5)),
                })
                .await
                .map_err(|error| match error {
                    super::native_http::JsonFailure::Transport(message)
                    | super::native_http::JsonFailure::Body(message) => {
                        authority_exception(message.as_str())
                    }
                })?;
            if !(200..300).contains(&response.status) {
                return Err(authority_failure(
                    response.status,
                    "APPROVAL_AUTH_REJECTED",
                    "StaffDeck rejected the approver identity.",
                ));
            }
            if let Some(message) = response.json_error {
                return Err(authority_exception(message));
            }
            let user = response.body;
            if matches!(user, JsValue::Null | JsValue::Undefined) {
                let value = if matches!(user, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                };
                return Err(authority_exception(
                    format!("Cannot read properties of {value} (reading 'tenant_id')").as_str(),
                ));
            }
            let role = user.get("role").and_then(JsValue::as_string);
            if user.get("tenant_id") != Some(&JsValue::String(self.tenant.clone()))
                || user.get("id") != Some(&JsValue::String(self.approver.clone()))
                || user.get("source") != Some(&JsValue::from("web"))
                || user.get("disabled") == Some(&JsValue::Bool(true))
                || !role.is_some_and(|v| v.equals_str("admin") || v.equals_str("member"))
            {
                return Err(authority_failure(
                    403,
                    "APPROVAL_SUBJECT_MISMATCH",
                    "This identity is not the configured native approver.",
                ));
            }
            Ok(ApprovalSubject {
                tenant_id: self.tenant.clone(),
                user_id: self.approver.clone(),
                source: "web".into(),
                disabled: false,
                role: role.expect("validated role").clone(),
            })
        })
    }
}

/// Actual catalog/admission IO stays with the native registry/storage owner.
/// Its production adapter supplies registered roots, durable sessions and the
/// authenticated original admission; it must not infer access from a pending wait.
pub(crate) trait NativeApprovalCatalog: Send + Sync {
    fn projects(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<JsString>, ApprovalFailure>> + Send + '_>>;
    fn has_session<'a>(
        &'a self,
        project: &'a JsString,
        session: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ApprovalFailure>> + Send + 'a>>;
    fn admission<'a>(
        &'a self,
        project: &'a JsString,
        session: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ApprovalFailure>> + Send + 'a>>;
}
pub(crate) struct NativeSessionResolver {
    pub database: PathBuf,
    pub helper: PathBuf,
    pub catalog: Arc<dyn NativeApprovalCatalog>,
}
impl ApprovalSessionResolver for NativeSessionResolver {
    fn resolve<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsValue>,
        binding: &'a ApprovalBinding,
        _subject: &'a ApprovalSubject,
    ) -> Pin<Box<dyn Future<Output = Option<ApprovalSession>> + Send + 'a>> {
        Box::pin(async move {
            if installation_owner(&self.helper, &self.database)
                .await
                .as_ref()
                != Some(&binding.pilot_deck_user_id)
            {
                return None;
            }
            let requested = match project {
                None | Some(JsValue::Undefined) => None,
                Some(JsValue::String(value)) => {
                    Some(crate::metadata::resolve_process_identity(Path::new(""), value).ok()??)
                }
                _ => return None,
            };
            let mut found = None;
            for root in self.catalog.projects().await.ok()? {
                let normalized =
                    crate::metadata::resolve_process_identity(Path::new(""), &root).ok()??;
                if requested.as_ref().is_some_and(|key| key != &normalized) {
                    continue;
                }
                if !self.catalog.has_session(&root, session).await.ok()? {
                    continue;
                }
                let Some(admission) = self.catalog.admission(&root, session).await.ok()? else {
                    continue;
                };
                if admission.get("tenantId") != Some(&JsValue::String(binding.tenant_id.clone()))
                    || admission.get("agentId") != Some(&JsValue::String(binding.agent_id.clone()))
                    || admission.get("pilotDeckUserId")
                        != Some(&JsValue::String(binding.pilot_deck_user_id.clone()))
                    || admission.get("projectKey") != Some(&JsValue::String(root.clone()))
                {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                found = Some(root);
            }
            Some(ApprovalSession {
                tenant_id: binding.tenant_id.clone(),
                agent_id: binding.agent_id.clone(),
                pilot_deck_user_id: binding.pilot_deck_user_id.clone(),
                session_key: session.clone(),
                project_key: Some(JsValue::String(found?)),
            })
        })
    }
}

/// Read only the original transcript's internally consistent admission. The
/// adapter must also reject diagnostics before invoking this projection.
pub(crate) fn read_admission(entries: &[JsValue], session: &JsString) -> Option<JsValue> {
    let mut admission: Option<JsValue> = None;
    for entry in entries {
        if entry.get("type") != Some(&JsValue::from("session_metadata")) {
            continue;
        }
        let Some(next) = entry
            .get("metadata")
            .and_then(|v| v.get("staffDeckAdmission"))
            .filter(|v| super::js_truthy(v))
        else {
            continue;
        };
        let valid_turn = entry.get("turnId") == Some(&JsValue::from("host-admission"))
            || entry
                .get("metadata")
                .and_then(|v| v.get("isSnapshot"))
                .is_some_and(super::js_truthy);
        if entry.get("sessionId") != Some(&JsValue::String(session.clone()))
            || !valid_turn
            || next.as_object().is_none()
        {
            return None;
        }
        for key in [
            "tenantId",
            "agentId",
            "pilotDeckUserId",
            "projectKey",
            "actorUserId",
            "credentialId",
            "staffDeckOrigin",
        ] {
            let value = next.get(key)?.as_string()?;
            if !nonempty(value)
                || admission
                    .as_ref()
                    .is_some_and(|old| old.get(key) != next.get(key))
            {
                return None;
            }
        }
        admission = Some(next.clone());
    }
    admission
}

/// Internal helper discriminator. All three bundle entrypoints dispatch this
/// before loading application configuration. The parent is the only process
/// that handles deadlines; this child never receives any credentials or HOME.
pub const OWNER_HELPER: &str = "--internal-native-installation-owner";
pub fn maybe_owner_helper(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<i32> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(OWNER_HELPER)) {
        return None;
    }
    let mut collected = vec![OWNER_HELPER.to_owned()];
    for value in args {
        match value.into_string() {
            Ok(value) => collected.push(value),
            Err(_) => return Some(2),
        }
        if collected.len() > 2 {
            return Some(2);
        }
    }
    owner_helper(&collected)
}

pub fn owner_helper(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some(OWNER_HELPER) {
        return None;
    }
    if args.len() != 2 || !Path::new(&args[1]).is_absolute() {
        return Some(2);
    }
    let owner = read_owner_sqlite(Path::new(&args[1]));
    let value =
        crate::metadata::object([("owner", owner.map(JsValue::String).unwrap_or(JsValue::Null))]);
    let Some(raw) = stringify_json(&value).ok().flatten() else {
        return Some(1);
    };
    use std::io::Write;
    Some(if writeln!(std::io::stdout().lock(), "{raw}").is_ok() {
        0
    } else {
        1
    })
}
fn read_owner_sqlite(path: &Path) -> Option<JsString> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.busy_timeout(Duration::from_millis(300)).ok()?;
    let mut stmt = db
        .prepare("SELECT id FROM users WHERE is_active = 1 LIMIT 2")
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    let first = rows.next().ok()??;
    let id = first.get_ref(0).ok()?;
    use rusqlite::types::ValueRef;
    let id = match id {
        ValueRef::Text(raw) => String::from_utf8_lossy(raw).into_owned(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(n) => crate::js_value::number_string(n),
        ValueRef::Null => "null".into(),
        ValueRef::Blob(raw) => raw.iter().map(u8::to_string).collect::<Vec<_>>().join(","),
    };
    if rows.next().ok()?.is_some() {
        return None;
    }
    Some(id.into())
}
pub(crate) async fn installation_owner(helper: &Path, database: &Path) -> Option<JsString> {
    static IO: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
        std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    let permit = IO.clone().acquire_owned().await.ok()?;
    let helper = helper.to_owned();
    let database = database.to_owned();
    // A cancelled caller must not release the process slot before its owned
    // helper has settled. This worker has an absolute 5s exchange + 2s reap
    // budget; dropping its child (including runtime shutdown) also sends kill.
    tokio::spawn(async move {
        let _permit = permit;
        installation_owner_inner(&helper, &database).await
    })
    .await
    .ok()
    .flatten()
}
async fn installation_owner_inner(helper: &Path, database: &Path) -> Option<JsString> {
    if !database.is_absolute() {
        return None;
    }
    let mut command = tokio::process::Command::new(helper);
    command
        .arg(OWNER_HELPER)
        .arg(database)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let exchange = async {
        let mut output = Vec::new();
        (&mut stdout)
            .take(4097)
            .read_to_end(&mut output)
            .await
            .ok()?;
        if output.len() > 4096 {
            return None;
        }
        let status = child.wait().await.ok()?;
        if !status.success() {
            return None;
        }
        let value = parse_json(std::str::from_utf8(&output).ok()?).ok()?;
        value.get("owner")?.as_string().cloned()
    };
    match tokio::time::timeout(Duration::from_secs(5), exchange).await {
        Ok(owner) => {
            // Oversized output/early parse rejection still owns a possibly live child.
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            }
            owner
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            None
        }
    }
}

/// Authenticate the original management actor, key and target before recording
/// or consuming transcript admission. This never returns credentials/HTTP bodies.
pub(crate) struct AdmissionIdentity {
    pub origin: String,
    pub tenant: JsString,
    pub agent: JsString,
    pub actor: JsString,
    pub credential: JsString,
    pub actor_bearer: String,
    pub discovery_endpoint: String,
    pub discovery_agent: JsString,
    pub discovery_key: JsString,
    pub pilot_user: JsString,
    pub expected_pilot_user: JsString,
}
pub(crate) async fn authenticate_admission(
    identity: &AdmissionIdentity,
    project: &JsString,
    http: &dyn JsonTransport,
    now_ms: f64,
) -> Option<JsValue> {
    async fn request(
        http: &dyn JsonTransport,
        origin: &url::Url,
        path: &str,
        bearer: String,
    ) -> Option<JsValue> {
        let response = http
            .request(JsonRequest {
                method: "GET",
                url: origin.join(path).ok()?,
                bearer: Some(bearer),
                body: None,
                parse_error_body: false,
                redirect_error: true,
                timeout: Some(Duration::from_secs(5)),
            })
            .await
            .ok()?;
        (200..300)
            .contains(&response.status)
            .then_some(response.body)
    }
    let operation = async {
        let id = identity;
        if [
            &id.tenant,
            &id.agent,
            &id.actor,
            &id.credential,
            &id.pilot_user,
            &id.discovery_key,
        ]
        .iter()
        .any(|s| s.is_empty())
            || id.actor_bearer.is_empty()
            || id.discovery_agent != id.agent
            || id.pilot_user != id.expected_pilot_user
        {
            return None;
        }
        let origin = url::Url::parse(&id.origin).ok()?;
        if !matches!(origin.scheme(), "http" | "https")
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return None;
        }
        let formal = origin.origin().ascii_serialization();
        // TS removes one trailing slash, not all of them.
        let discovery = id
            .discovery_endpoint
            .strip_suffix('/')
            .unwrap_or(&id.discovery_endpoint);
        if discovery != format!("{formal}/api/v1") {
            return None;
        }
        let actor = request(http, &origin, "/api/auth/me", id.actor_bearer.clone()).await?;
        if actor.get("id") != Some(&JsValue::String(id.actor.clone()))
            || actor.get("tenant_id") != Some(&JsValue::String(id.tenant.clone()))
            || actor.get("disabled") == Some(&JsValue::Bool(true))
            || actor.get("source") != Some(&JsValue::from("web"))
        {
            return None;
        }
        let credentials = request(
            http,
            &origin,
            "/api/auth/me/api-credentials",
            id.actor_bearer.clone(),
        )
        .await?;
        let JsValue::Array(rows) = &credentials else {
            return None;
        };
        let credential = rows
            .iter()
            .find(|row| row.get("id") == Some(&JsValue::String(id.credential.clone())))?;
        let prefix = credential.get("key_prefix")?.as_string()?;
        let units = prefix.units();
        if units.last() != Some(&0x2026)
            || units.len() != 21
            || !id.discovery_key.units().starts_with(&units[..20])
            || credential.get("user_id") != Some(&JsValue::String(id.actor.clone()))
            || credential.get("status") != Some(&JsValue::from("active"))
            || credential.get("revoked_at").is_some_and(super::js_truthy)
            || credential.get("access") != Some(&JsValue::from("user_full_access"))
        {
            return None;
        }
        if let Some(expires) = credential
            .get("expires_at")
            .filter(|value| super::js_truthy(value))
        {
            let expires =
                JsString::from_utf16(crate::js_value::to_string_units_checked(expires).ok()?);
            if !pilotdeck_transport::system_clock::parse_node22_date(&expires)
                .ok()
                .flatten()
                .is_some_and(|time| time > now_ms)
            {
                return None;
            }
        }
        let encoded = crate::channel::http_client::encode_uri_component(&id.agent.to_utf8().ok()?);
        let target = request(
            http,
            &origin,
            &format!("/api/v1/agents/{encoded}"),
            id.discovery_key.to_utf8().ok()?,
        )
        .await?;
        if target.get("id") != Some(&JsValue::String(id.agent.clone()))
            || target.get("status") != Some(&JsValue::from("active"))
            || target.get("is_overall") != Some(&JsValue::Bool(false))
        {
            return None;
        }
        Some(crate::metadata::object([
            ("tenantId", JsValue::String(id.tenant.clone())),
            ("agentId", JsValue::String(id.agent.clone())),
            ("pilotDeckUserId", JsValue::String(id.pilot_user.clone())),
            ("projectKey", JsValue::String(project.clone())),
            ("actorUserId", JsValue::String(id.actor.clone())),
            ("credentialId", JsValue::String(id.credential.clone())),
            ("staffDeckOrigin", formal.as_str().into()),
        ]))
    };
    tokio::time::timeout(Duration::from_secs(5), operation)
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
#[path = "native_auth_tests.rs"]
mod tests;
