use super::{
    profile, project_key, reject_override, session_key,
    value::{error, expiry_ms, field, nonempty, now_ms, object, resolve, text, truthy},
};
use crate::{
    config::Env,
    runtime::{
        connection::RequestError,
        model::http::{HyperModelTransport, ModelHttpTransport},
    },
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

pub(super) struct Access {
    pub project: String,
    pub tenant: String,
    pub subject: JsValue,
}

pub(super) async fn access(
    home: &Path,
    cwd: &Path,
    params: &JsValue,
    env: &Env,
) -> Result<Access, RequestError> {
    let session = session_key(params)?;
    let projects = projects(home, cwd);
    let requested = project_key(params)?;
    let selected: Vec<_> = projects
        .into_iter()
        .filter(|project| {
            requested
                .as_ref()
                .is_none_or(|r| resolve(cwd, Path::new(r)) == resolve(cwd, Path::new(project)))
        })
        .collect();
    let dynamic = env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .is_some_and(|s| s == "true");
    let mut admitted = Vec::new();
    let mut tenant = env
        .get("STAFFDECK_COPY_TENANT_ID")
        .cloned()
        .unwrap_or_default();
    let mut agent = env
        .get("STAFFDECK_COPY_TARGET_AGENT_ID")
        .cloned()
        .unwrap_or_default();
    let mut owner = env
        .get("STAFFDECK_COPY_PILOTDECK_USER_ID")
        .cloned()
        .unwrap_or_default();
    if dynamic {
        if requested.is_some() && selected.is_empty() {
            return Err(error(
                "PROJECT_NOT_FOUND",
                format!("Unknown projectKey: {}", requested.as_deref().unwrap()),
                None,
            ));
        }
        for project in &selected {
            if let Some(admission) = admission(home, cwd, project, &session, env).await {
                admitted.push((project.clone(), admission));
            }
        }
        if admitted.len() != 1 {
            return Err(error(
                "SOP_APPROVAL_SESSION_FORBIDDEN",
                "SOP_APPROVAL_SESSION_FORBIDDEN",
                Some(403),
            ));
        }
        tenant = text(&admitted[0].1, "tenantId").unwrap();
        agent = text(&admitted[0].1, "agentId").unwrap();
        owner = text(&admitted[0].1, "pilotDeckUserId").unwrap();
    }
    if !nonempty(field(params, "sessionKey")) {
        return Err(error(
            "APPROVAL_INPUT_INVALID",
            "APPROVAL_INPUT_INVALID",
            Some(400),
        ));
    }
    reject_override(params)?;
    if (!field(params, "tenantId").is_undefined()
        && text(params, "tenantId").as_deref() != Some(&tenant))
        || (!field(params, "agentId").is_undefined()
            && text(params, "agentId").as_deref() != Some(&agent))
    {
        return Err(error(
            "APPROVAL_SUBJECT_MISMATCH",
            "APPROVAL_SUBJECT_MISMATCH",
            Some(403),
        ));
    }
    if [&tenant, &agent, &owner]
        .iter()
        .any(|s| crate::metadata::js_trim(s).is_empty())
    {
        return Err(error(
            "APPROVAL_BINDING_UNAVAILABLE",
            "APPROVAL_BINDING_UNAVAILABLE",
            Some(503),
        ));
    }
    let authorization = text(params, "approverAuthorization").unwrap_or_default();
    if !authorization
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        || authorization
            .get(7..)
            .is_none_or(|b| crate::metadata::js_trim(b).is_empty())
    {
        return Err(error(
            "APPROVAL_AUTH_REQUIRED",
            "APPROVAL_AUTH_REQUIRED",
            Some(401),
        ));
    }
    let origin = env
        .get("STAFFDECK_FORMAL_API_ORIGIN")
        .filter(|s| !s.is_empty());
    let approver = env
        .get("STAFFDECK_APPROVAL_USER_ID")
        .filter(|s| !s.is_empty());
    let (Some(origin), Some(approver)) = (origin, approver) else {
        return Err(error(
            "APPROVAL_BINDING_UNAVAILABLE",
            "APPROVAL_BINDING_UNAVAILABLE",
            Some(503),
        ));
    };
    let origin = valid_origin(origin, false).ok_or_else(|| {
        error(
            "APPROVAL_CONFIG_INVALID",
            "A formal StaffDeck origin is required.",
            Some(400),
        )
    })?;
    let user = get(&origin, "/api/auth/me", &authorization[7..]).await?;
    if text(&user, "tenant_id").as_deref() != Some(&tenant)
        || text(&user, "id").as_deref() != Some(approver)
        || text(&user, "source").as_deref() != Some("web")
        || field(&user, "disabled").as_bool() == Some(true)
        || !matches!(text(&user, "role").as_deref(), Some("admin" | "member"))
    {
        return Err(error(
            "APPROVAL_SUBJECT_MISMATCH",
            "This identity is not the configured native approver.",
            Some(403),
        ));
    }
    let subject = object([
        ("tenantId", tenant.as_str().into()),
        ("userId", field(&user, "id").clone()),
        ("source", field(&user, "source").clone()),
        ("role", field(&user, "role").clone()),
        (
            "disabled",
            JsValue::Bool(field(&user, "disabled").as_bool() == Some(true)),
        ),
    ]);
    let database = env
        .get("DATABASE_PATH")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("auth.db"));
    if installation_owner(&database).as_deref() != Some(&owner) {
        return Err(error(
            "SOP_APPROVAL_SESSION_FORBIDDEN",
            "SOP_APPROVAL_SESSION_FORBIDDEN",
            Some(403),
        ));
    }
    // The native resolver revalidates admission even after dynamic binding resolution.
    admitted.clear();
    for project in &selected {
        if !crate::runtime::manager::contains_session_units(home, cwd, project, &session)? {
            continue;
        }
        if let Some(admission) = admission(home, cwd, project, &session, env).await
            && text(&admission, "tenantId").as_deref() == Some(&tenant)
            && text(&admission, "agentId").as_deref() == Some(&agent)
            && text(&admission, "pilotDeckUserId").as_deref() == Some(&owner)
            && text(&admission, "projectKey").as_deref() == Some(project)
        {
            admitted.push((project.clone(), admission));
        }
    }
    if admitted.len() != 1 || requested.as_ref().is_some_and(|r| r != &admitted[0].0) {
        return Err(error(
            "SOP_APPROVAL_SESSION_FORBIDDEN",
            "SOP_APPROVAL_SESSION_FORBIDDEN",
            Some(403),
        ));
    }
    Ok(Access {
        project: admitted.remove(0).0,
        tenant,
        subject,
    })
}

pub(super) fn installation_owner(database: &Path) -> Option<String> {
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let mut statement = db
        .prepare("SELECT id FROM users WHERE is_active = 1")
        .ok()?;
    let mut rows = statement.query([]).ok()?;
    let owner = match rows.next().ok()??.get_ref(0).ok()? {
        rusqlite::types::ValueRef::Text(s) => String::from_utf8(s.to_vec()).ok()?,
        rusqlite::types::ValueRef::Integer(i) => i.to_string(),
        _ => return None,
    };
    if rows.next().ok()?.is_some() {
        return None;
    }
    Some(owner)
}

fn valid_origin(value: &str, root_only: bool) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (root_only && url.path() != "/")
    {
        return None;
    }
    Some(url)
}

async fn get(origin: &Url, path: &str, bearer: &str) -> Result<JsValue, RequestError> {
    let url = origin.join(path).map_err(|_| {
        error(
            "APPROVAL_CONFIG_INVALID",
            "Invalid authority URL.",
            Some(400),
        )
    })?;
    let request = Request::builder()
        .method("GET")
        .uri(url.as_str())
        .header("authorization", format!("Bearer {bearer}"))
        .header("accept", "application/json")
        .body(Full::new(Bytes::new()))
        .map_err(|_| {
            error(
                "APPROVAL_AUTH_REQUIRED",
                "Invalid bearer header.",
                Some(401),
            )
        })?;
    let exchange = async {
        let response = HyperModelTransport::new()
            .send(request)
            .await
            .map_err(|_| {
                error(
                    "gateway_request_failed",
                    "StaffDeck identity request failed.",
                    None,
                )
            })?;
        if response.status().is_redirection() {
            return Err(error(
                "gateway_request_failed",
                "StaffDeck identity redirect was refused.",
                None,
            ));
        }
        if !response.status().is_success() {
            return Err(error(
                "APPROVAL_AUTH_REJECTED",
                "StaffDeck rejected the approver identity.",
                Some(response.status().as_u16()),
            ));
        }
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| {
                error(
                    "gateway_request_failed",
                    "StaffDeck identity response could not be read.",
                    None,
                )
            })?
            .to_bytes();
        parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| {
            error(
                "gateway_request_failed",
                "StaffDeck identity response is invalid JSON.",
                None,
            )
        })
    };
    tokio::time::timeout(crate::runtime::network::UNDICI_BODY_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            error(
                "gateway_request_failed",
                "StaffDeck identity request timed out.",
                None,
            )
        })?
}

fn original_admission(
    home: &Path,
    cwd: &Path,
    project: &str,
    session: &JsString,
) -> Option<JsValue> {
    let chats = crate::session_chat_dir(home, cwd, project);
    let file = chats.join(format!(
        "{}.jsonl",
        crate::storage::sanitize_session_id(session)
    ));
    let read = crate::storage::read_transcript(file).ok()?;
    if !read.diagnostics.is_empty() {
        return None;
    }
    let mut original = None;
    for entry in &read.entries {
        if text(entry, "type").as_deref() != Some("session_metadata") {
            continue;
        }
        let metadata = field(entry, "metadata");
        let next = field(metadata, "staffDeckAdmission");
        if !truthy(next) {
            continue;
        }
        if field(entry, "sessionId").as_string() != Some(session)
            || (text(entry, "turnId").as_deref() != Some("host-admission")
                && !truthy(field(metadata, "isSnapshot")))
        {
            return None;
        }
        if ![
            "tenantId",
            "agentId",
            "pilotDeckUserId",
            "projectKey",
            "actorUserId",
            "credentialId",
            "staffDeckOrigin",
        ]
        .iter()
        .all(|k| nonempty(field(next, k)))
        {
            return None;
        }
        if let Some(previous) = original.as_ref()
            && !same_admission(previous, next)
        {
            return None;
        }
        original = Some(next.clone());
    }
    original
}

fn same_admission(a: &JsValue, b: &JsValue) -> bool {
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
    .all(|k| field(a, k) == field(b, k))
}

pub(super) async fn admission(
    home: &Path,
    cwd: &Path,
    project: &str,
    session: &JsString,
    env: &Env,
) -> Option<JsValue> {
    let original = original_admission(home, cwd, project, session)?;
    let env = project_environment(home, cwd, project, env)?;
    let profile = profile(home, cwd, Some(project), &env).ok();
    let tenant = env.get("STAFFDECK_COPY_TENANT_ID")?;
    let agent = env.get("STAFFDECK_COPY_TARGET_AGENT_ID")?;
    let actor = env.get("STAFFDECK_COPY_ACTOR_USER_ID")?;
    let credential_id = env.get("STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID")?;
    let owner = env.get("STAFFDECK_COPY_PILOTDECK_USER_ID")?;
    let actor_bearer = env.get("STAFFDECK_COPY_USER_TOKEN")?;
    let origin = valid_origin(env.get("STAFFDECK_FORMAL_API_ORIGIN")?, true)?;
    let default_endpoint = format!("{}/api/v1", origin.origin().ascii_serialization());
    let endpoint = profile
        .as_ref()
        .and_then(|p| text(p, "discoveryEndpoint"))
        .unwrap_or(default_endpoint.clone());
    let discovery_agent = profile
        .as_ref()
        .and_then(|p| text(p, "discoveryAgentId"))
        .unwrap_or(agent.clone());
    let key = profile
        .as_ref()
        .and_then(|p| text(p, "discoveryApiKey"))
        .or_else(|| env.get("STAFFDECK_SOP_MANAGEMENT_API_KEY").cloned())?;
    if [
        tenant,
        agent,
        actor,
        credential_id,
        owner,
        actor_bearer,
        &key,
    ]
    .iter()
    .any(|s| s.is_empty())
        || discovery_agent != *agent
        || endpoint.trim_end_matches('/') != default_endpoint
        || text(&original, "pilotDeckUserId").as_deref() != Some(owner)
    {
        return None;
    }
    let verified = async {
        let user = get(&origin, "/api/auth/me", actor_bearer).await.ok()?;
        if text(&user, "id").as_deref() != Some(actor)
            || text(&user, "tenant_id").as_deref() != Some(tenant)
            || field(&user, "disabled").as_bool() == Some(true)
            || text(&user, "source").as_deref() != Some("web")
        {
            return None;
        }
        let credentials = get(&origin, "/api/auth/me/api-credentials", actor_bearer)
            .await
            .ok()?;
        let JsValue::Array(credentials) = &credentials else {
            return None;
        };
        let credential = credentials
            .iter()
            .find(|c| text(c, "id").as_deref() == Some(credential_id))?;
        let prefix = text(credential, "key_prefix")?;
        let prefix = prefix.strip_suffix('…')?;
        if text(credential, "user_id").as_deref() != Some(actor)
            || text(credential, "status").as_deref() != Some("active")
            || truthy(field(credential, "revoked_at"))
            || text(credential, "access").as_deref() != Some("user_full_access")
            || prefix.encode_utf16().count() != 20
            || !key.starts_with(prefix)
        {
            return None;
        }
        if truthy(field(credential, "expires_at"))
            && text(credential, "expires_at")
                .and_then(|s| expiry_ms(&s))
                .is_none_or(|date| date <= now_ms())
        {
            return None;
        }
        let mut target_url = origin.clone();
        target_url.path_segments_mut().ok()?.clear().extend([
            "api",
            "v1",
            "agents",
            agent.as_str(),
        ]);
        let target = get(&origin, target_url.path(), &key).await.ok()?;
        if text(&target, "id").as_deref() != Some(agent)
            || text(&target, "status").as_deref() != Some("active")
            || field(&target, "is_overall").as_bool() != Some(false)
        {
            return None;
        }
        Some(object([
            ("tenantId", tenant.as_str().into()),
            ("agentId", agent.as_str().into()),
            ("pilotDeckUserId", owner.as_str().into()),
            ("projectKey", project.into()),
            ("actorUserId", actor.as_str().into()),
            ("credentialId", credential_id.as_str().into()),
            (
                "staffDeckOrigin",
                origin.origin().ascii_serialization().as_str().into(),
            ),
        ]))
    };
    let current = tokio::time::timeout(Duration::from_secs(5), verified)
        .await
        .ok()??;
    if same_admission(&original, &current) {
        Some(original)
    } else {
        None
    }
}

fn project_environment(home: &Path, cwd: &Path, project: &str, env: &Env) -> Option<Env> {
    let mut env = env.clone();
    if env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .is_none_or(|s| s != "true")
    {
        return Some(env);
    }
    let bytes = fs::read(home.join("staffdeck/project-bindings.json")).ok()?;
    let store = parse_json(&String::from_utf8_lossy(&bytes)).ok()?;
    if field(&store, "version").as_number() != Some(1.0) || !truthy(field(&store, "installationId"))
    {
        return None;
    }
    let JsValue::Array(rows) = field(&store, "projects") else {
        return None;
    };
    let row = rows.iter().find(|row| {
        text(row, "projectRoot").is_some_and(|p| Path::new(&p) == resolve(cwd, Path::new(project)))
    })?;
    let live_binding = text(row, "status").as_deref() == Some("binding")
        && truthy(field(row, "runtimeReady"))
        && fs::read_to_string(home.join("staffdeck/project-bindings.json.lock"))
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
    if (text(row, "status").as_deref() != Some("bound") && !live_binding)
        || text(row, "tenantId").as_ref() != env.get("STAFFDECK_COPY_TENANT_ID")
        || text(row, "origin").as_deref()
            != env
                .get("STAFFDECK_FORMAL_API_ORIGIN")
                .map(|s| s.trim_end_matches('/'))
    {
        return None;
    }
    env.insert(
        "STAFFDECK_COPY_TARGET_AGENT_ID".into(),
        text(row, "agentId")?,
    );
    Some(env)
}

pub(super) fn projects(home: &Path, cwd: &Path) -> Vec<String> {
    let mut projects = vec![resolve(cwd, home).to_string_lossy().into_owned()];
    let Ok(entries) = crate::list_session_entries(&home.join("projects")) else {
        return projects;
    };
    for (id, _) in entries {
        let directory = home.join("projects").join(&id);
        if !directory.is_dir() {
            continue;
        }
        let marker = fs::read_to_string(directory.join(".cwd")).ok();
        let project = if let Some(marker) = marker {
            let marker = crate::metadata::js_trim(&marker);
            if marker.is_empty() {
                continue;
            }
            let path = resolve(cwd, Path::new(marker));
            if !path.is_dir() {
                continue;
            }
            Some(marker.to_owned())
        } else {
            let segments: Vec<_> = id.split('-').collect();
            (0..segments.len()).find_map(|first| {
                let path = format!("/{}", segments[first..].join("/"));
                (project_id(&path) == id && Path::new(&path).is_dir()).then_some(path)
            })
        };
        if let Some(project) = project
            && !projects
                .iter()
                .any(|p| resolve(cwd, Path::new(p)) == resolve(cwd, Path::new(&project)))
        {
            projects.push(project);
        }
    }
    projects
}
fn project_id(path: &str) -> String {
    let mut out = String::new();
    let mut replacing = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            replacing = false;
        } else if !replacing {
            out.push('-');
            replacing = true;
        }
    }
    let id = out.trim_matches('-');
    if id.is_empty() {
        "project".into()
    } else {
        id.into()
    }
}
