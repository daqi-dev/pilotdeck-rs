//! Native SOP lifecycle control. Admission/busy checks belong to the shared host.
mod auth;
pub mod authority_port;
pub mod authority_validate;
pub mod control_tool_port;
pub mod knowledge;
pub mod prepare_step;
pub(crate) mod store;
mod value;

use crate::{config, runtime::connection::RequestError};
use pilotdeck_protocol::{JsString, JsValue};
use std::path::{Path, PathBuf};
use value::{error, field, nonempty, object, text};

pub async fn status(home: &Path, cwd: &Path, params: &JsValue) -> Result<JsValue, RequestError> {
    status_with_env(home, cwd, params, &std::env::vars().collect()).await
}

pub async fn resume(home: &Path, cwd: &Path, params: &JsValue) -> Result<JsValue, RequestError> {
    resume_with_env(home, cwd, params, &std::env::vars().collect()).await
}

pub(crate) async fn authority(
    home: &Path,
    cwd: &Path,
    params: &JsValue,
    principal: &JsValue,
) -> Result<JsValue, RequestError> {
    use value::{field, set};
    let deny = |code| error(code, code, Some(403));
    let allowed = [
        "sessionKey",
        "projectKey",
        "expectedRevision",
        "requestId",
        "admissionCredentialId",
    ];
    if !params.as_object().is_some_and(|o| {
        o.entries().iter().all(|(k, _)| {
            k.to_utf8()
                .ok()
                .is_some_and(|s| allowed.contains(&s.as_str()))
        })
    }) || ![
        "sessionKey",
        "projectKey",
        "requestId",
        "admissionCredentialId",
    ]
    .iter()
    .all(|k| nonempty(field(params, k)))
        || !value::safe_revision(field(params, "expectedRevision"))
    {
        return Err(deny("SOP_AUTHORITY_INPUT_INVALID"));
    }
    let env: config::Env = std::env::vars().collect();
    let project = text(params, "projectKey").unwrap();
    let session = session_key(params)?;
    let database = env
        .get("DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("auth.db"));
    if auth::installation_owner(&database).as_deref()
        != text(principal, "pilotDeckUserId").as_deref()
        || auth::projects(home, cwd)
            .iter()
            .filter(|p| *p == &project)
            .count()
            != 1
        || !crate::runtime::manager::contains_session_units(home, cwd, &project, &session)?
    {
        return Err(deny("SOP_AUTHORITY_SESSION_DENIED"));
    }
    let admission = auth::admission(home, cwd, &project, &session, &env)
        .await
        .ok_or_else(|| deny("SOP_AUTHORITY_ADMISSION_MISMATCH"))?;
    if !["pilotDeckUserId", "tenantId", "actorUserId", "agentId"]
        .iter()
        .all(|k| field(&admission, k) == field(principal, k))
        || field(&admission, "projectKey") != field(params, "projectKey")
        || field(&admission, "credentialId") != field(params, "admissionCredentialId")
    {
        return Err(deny("SOP_AUTHORITY_ADMISSION_MISMATCH"));
    }
    let root = state_root(home, cwd, Some(&project), &env)?;
    let mut result =
        store::authority(&root, session.clone(), field(params, "expectedRevision")).await?;
    for k in ["sessionKey", "projectKey", "expectedRevision", "requestId"] {
        set(&mut result, k, field(params, k).clone());
    }
    set(&mut result, "admission", admission);
    Ok(result)
}

async fn status_with_env(
    home: &Path,
    cwd: &Path,
    params: &JsValue,
    env: &config::Env,
) -> Result<JsValue, RequestError> {
    let session = session_key(params)?;
    if value::truthy(field(params, "approverAuthorization")) {
        let access = auth::access(home, cwd, params, env).await?;
        let root = state_root(home, cwd, Some(&access.project), env)?;
        let snapshot = store::status(&root, session.clone()).await?;
        if let Some(approval) = snapshot.get("approval")
            && text(&access.subject, "role").as_deref() != Some("admin")
            && field(approval, "assigneeUserId") != field(&access.subject, "userId")
        {
            return Err(error(
                "SOP_APPROVAL_FORBIDDEN",
                "SOP_APPROVAL_FORBIDDEN",
                Some(403),
            ));
        }
        return Ok(snapshot);
    }
    let root = state_root(home, cwd, project_key(params)?.as_deref(), env)?;
    let mut snapshot = store::status(&root, session).await?;
    if text(field(&snapshot, "wait"), "kind").as_deref() == Some("external_task") {
        value::set(&mut snapshot, "approval", JsValue::Undefined);
        value::set(&mut snapshot, "approvalError", JsValue::Undefined);
        return Ok(snapshot);
    }
    if !value::truthy(field(&snapshot, "wait")) {
        return Ok(JsValue::Null);
    }
    Err(error(
        "APPROVAL_AUTH_REQUIRED",
        "APPROVAL_AUTH_REQUIRED",
        Some(401),
    ))
}

async fn resume_with_env(
    home: &Path,
    cwd: &Path,
    params: &JsValue,
    env: &config::Env,
) -> Result<JsValue, RequestError> {
    resume_inner(home, cwd, params, env)
        .await
        .map_err(|mut err| {
            if err.code == "gateway_request_failed" {
                err.code = "SOP_RESUME_FAILED".into();
            }
            err
        })
}

async fn resume_inner(
    home: &Path,
    cwd: &Path,
    params: &JsValue,
    env: &config::Env,
) -> Result<JsValue, RequestError> {
    reject_override(params)?;
    let session = session_key(params)?;
    let human = text(params, "source").as_deref() == Some("human");
    if human
        && (!nonempty(field(params, "requestId"))
            || !nonempty(field(params, "waitId"))
            || !nonempty(field(params, "message"))
            || !value::safe_revision(field(params, "expectedRevision")))
    {
        return Err(error(
            "APPROVAL_INPUT_INVALID",
            "APPROVAL_INPUT_INVALID",
            Some(400),
        ));
    }
    let (project, authority) = if human {
        let access = auth::access(home, cwd, params, env).await?;
        (
            Some(access.project),
            Some(object([
                ("tenantId", access.tenant.as_str().into()),
                ("sessionId", JsValue::String(session.clone())),
                ("subject", access.subject),
            ])),
        )
    } else {
        (project_key(params)?, None)
    };
    let root = state_root(home, cwd, project.as_deref(), env)?;
    store::resume(&root, session, params, authority.as_ref()).await
}

fn reject_override(params: &JsValue) -> Result<(), RequestError> {
    if params
        .as_object()
        .is_some_and(|o| o.contains_key("authority") || o.contains_key("subject"))
    {
        return Err(error(
            "APPROVAL_AUTHORITY_OVERRIDE",
            "APPROVAL_AUTHORITY_OVERRIDE",
            Some(400),
        ));
    }
    Ok(())
}

fn session_key(params: &JsValue) -> Result<JsString, RequestError> {
    field(params, "sessionKey")
        .as_string()
        .cloned()
        .ok_or_else(|| {
            error(
                "APPROVAL_INPUT_INVALID",
                "sessionKey must be a string.",
                Some(400),
            )
        })
}

fn project_key(params: &JsValue) -> Result<Option<String>, RequestError> {
    match field(params, "projectKey") {
        JsValue::Undefined => Ok(None),
        JsValue::String(s) => s.to_utf8().map(Some).map_err(|_| {
            error(
                "APPROVAL_INPUT_INVALID",
                "projectKey must be valid UTF-8.",
                Some(400),
            )
        }),
        _ => Err(error(
            "APPROVAL_INPUT_INVALID",
            "projectKey must be a string.",
            Some(400),
        )),
    }
}

fn profile(
    home: &Path,
    cwd: &Path,
    project: Option<&str>,
    env: &config::Env,
) -> Result<JsValue, RequestError> {
    let snapshot =
        config::load_snapshot(home, cwd, env, project, 1.0, &value::now()).map_err(|e| {
            RequestError {
                code: e.code,
                message: e.message,
                details: None,
                validation: None,
            }
        })?;
    let sop = field(field(field(&snapshot, "config"), "modules"), "sop");
    if sop.as_object().is_none() {
        return Err(error(
            "SOP_MODULE_DISABLED",
            "The StaffDeck SOP module is not enabled for this project.",
            None,
        ));
    }
    Ok(sop.clone())
}

fn state_root(
    home: &Path,
    cwd: &Path,
    project: Option<&str>,
    env: &config::Env,
) -> Result<PathBuf, RequestError> {
    let profile = profile(home, cwd, project, env)?;
    let root = text(&profile, "stateRoot")
        .ok_or_else(|| error("SOP_MODULE_DISABLED", "SOP stateRoot is unavailable.", None))?;
    Ok(value::resolve(cwd, Path::new(&root)).join("sessions"))
}

pub async fn check_sop_authority_revision(
    sessions_root: &Path,
    session_key: &str,
    expected_revision: &JsValue,
) -> Result<(), RequestError> {
    store::authority(sessions_root, session_key, expected_revision).await?;
    Ok(())
}

pub fn sessions_root_from_sop_config(cwd: &str, sop: &JsValue) -> Option<PathBuf> {
    let root = text(sop, "stateRoot")?;
    Some(value::resolve(Path::new(cwd), Path::new(&root)).join("sessions"))
}

#[cfg(test)]
mod tests;
