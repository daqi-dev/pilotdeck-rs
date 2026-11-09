use super::value::{deep_equal, error, field, nonempty, now, object, set, spread, text, truthy};
use crate::runtime::connection::RequestError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

type Locks = Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>;
static LOCKS: OnceLock<Locks> = OnceLock::new();

pub(crate) fn path(root: &Path, session: impl Into<JsString>) -> PathBuf {
    let session = session.into();
    root.join(format!(
        "{}.json",
        URL_SAFE_NO_PAD.encode(String::from_utf16_lossy(session.units()).as_bytes())
    ))
}
async fn lock(root: &Path, session: &JsString) -> OwnedMutexGuard<()> {
    let gate = {
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, gate| gate.strong_count() > 0);
        let key = path(root, session.clone());
        let gate = locks
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
        locks.insert(key, Arc::downgrade(&gate));
        gate
    };
    gate.lock_owned().await
}

pub(super) async fn status(
    root: &Path,
    session: impl Into<JsString>,
) -> Result<JsValue, RequestError> {
    let session = session.into();
    let session = &session;
    let _guard = lock(root, session).await;
    let Some(current) = read(root, session.clone())? else {
        return Ok(JsValue::Null);
    };
    let mut snapshot = object([
        ("sessionId", field(&current, "sessionId").clone()),
        ("revision", field(&current, "revision").clone()),
        ("state", field(&current, "state").clone()),
    ]);
    if current.get("wait").is_some() {
        set(&mut snapshot, "wait", field(&current, "wait").clone());
    }
    if text(field(&current, "wait"), "kind").as_deref() == Some("handoff") {
        match pinned_approval(&current) {
            Ok(approval) => set(&mut snapshot, "approval", approval),
            Err(e) => set(
                &mut snapshot,
                "approvalError",
                object([
                    ("code", e.code.as_str().into()),
                    ("message", JsValue::String(e.message)),
                ]),
            ),
        }
    }
    Ok(snapshot)
}

pub(super) async fn authority(
    root: &Path,
    session: impl Into<JsString>,
    expected: &JsValue,
) -> Result<JsValue, RequestError> {
    let session = session.into();
    let session = &session;
    let _guard = lock(root, session).await;
    let fail = |code| error(code, code, Some(409));
    let current =
        read(root, session.clone())?.ok_or_else(|| fail("SOP_AUTHORITY_STATE_MISSING"))?;
    if field(&current, "revision") != expected {
        return Err(fail("SOP_REVISION_CONFLICT"));
    }
    if truthy(field(&current, "wait"))
        || text(field(&current, "state"), "status").as_deref() != Some("active")
    {
        return Err(fail("SOP_AUTHORITY_STATE_INACTIVE"));
    }
    let state = field(&current, "state");
    let id = state
        .get("active_skill_id")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or_else(|| field(state, "selected_skill_id"));
    let JsValue::Array(sops) = field(field(&current, "bundle"), "sops") else {
        return Err(fail("SOP_AUTHORITY_PIN_INVALID"));
    };
    let matches: Vec<_> = sops
        .iter()
        .filter(|s| {
            s.get("skill_id")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .unwrap_or_else(|| field(s, "id"))
                == id
        })
        .collect();
    let sop = matches
        .first()
        .filter(|_| matches.len() == 1)
        .ok_or_else(|| fail("SOP_AUTHORITY_PIN_INVALID"))?;
    if field(sop, "version").as_string().is_none()
        || field(sop, "content").as_object().is_none()
        || field(state, "active_step_id").as_string().is_none()
    {
        return Err(fail("SOP_AUTHORITY_PIN_INVALID"));
    }
    Ok(object([
        ("sopId", id.clone()),
        ("sopVersion", field(sop, "version").clone()),
        ("nodeId", field(state, "active_step_id").clone()),
        ("content", field(sop, "content").clone()),
    ]))
}

pub(super) async fn resume(
    root: &Path,
    session: impl Into<JsString>,
    input: &JsValue,
    authority: Option<&JsValue>,
) -> Result<JsValue, RequestError> {
    let session = session.into();
    let session = &session;
    let _guard = lock(root, session).await;
    let Some(mut current) = read(root, session.clone())? else {
        return Err(error(
            "SOP_SESSION_NOT_FOUND",
            session_message("No StaffDeck SOP state exists for '", session, "'."),
            None,
        ));
    };
    let request_id = field(input, "requestId").as_string().ok_or_else(|| {
        error(
            "APPROVAL_INPUT_INVALID",
            "requestId must be a string.",
            Some(400),
        )
    })?;
    let human = text(input, "source").as_deref() == Some("human");
    let requests = field(&current, "resumeRequests");
    if let Some(duplicate) = requests.as_object().and_then(|o| o.get_key(request_id)) {
        if human {
            let authorization = field(duplicate, "authorization");
            if !truthy(authorization) {
                return Err(error(
                    "SOP_APPROVAL_LEGACY_RECEIPT",
                    "Old receipt has no authenticated approval facts.",
                    None,
                ));
            }
            validate_authority(authority, session, field(authorization, "approval"))?;
            if !deep_equal(
                field(authorization, "authority"),
                authority.unwrap_or(&super::value::UNDEFINED),
            ) || field(input, "expectedRevision") != field(authorization, "expectedRevision")
            {
                return Err(error(
                    "SOP_RESUME_REQUEST_CONFLICT",
                    "Approval request belongs to a different subject or original revision.",
                    None,
                ));
            }
        }
        let updates = if !matches!(
            field(input, "slotUpdates"),
            JsValue::Undefined | JsValue::Null
        ) {
            field(input, "slotUpdates").clone()
        } else {
            object([])
        };
        let saved_updates = if !matches!(
            field(duplicate, "slotUpdates"),
            JsValue::Undefined | JsValue::Null
        ) {
            field(duplicate, "slotUpdates").clone()
        } else {
            object([])
        };
        if field(duplicate, "message") != field(input, "message")
            || (!field(duplicate, "waitId").is_undefined()
                && field(duplicate, "waitId") != field(input, "waitId"))
            || (!field(duplicate, "source").is_undefined()
                && field(duplicate, "source") != field(input, "source"))
            || (!field(duplicate, "waitId").is_undefined() && !deep_equal(&saved_updates, &updates))
        {
            return Err(error(
                "SOP_RESUME_REQUEST_CONFLICT",
                "This request id already belongs to a different SOP resume payload.",
                None,
            ));
        }
        return Ok(receipt(duplicate, true));
    }
    let wait = field(&current, "wait");
    if !truthy(wait) {
        return Err(error(
            "SOP_NOT_WAITING",
            "The StaffDeck SOP session is not waiting for a resumable result.",
            None,
        ));
    }
    if field(wait, "id") != field(input, "waitId") {
        return Err(error(
            "SOP_WAIT_STALE",
            "The StaffDeck SOP wait identifier is no longer active.",
            None,
        ));
    }
    let expected_source = if text(wait, "kind").as_deref() == Some("handoff") {
        "human"
    } else {
        "external_task"
    };
    if text(input, "source").as_deref() != Some(expected_source) {
        return Err(error(
            "SOP_RESUME_SOURCE_INVALID",
            format!("This wait requires source '{expected_source}'."),
            None,
        ));
    }
    let revision = field(&current, "revision").as_number().unwrap();
    if !field(input, "expectedRevision").is_undefined()
        && field(input, "expectedRevision") != field(&current, "revision")
    {
        let expected =
            crate::js_value::display(field(input, "expectedRevision")).unwrap_or_default();
        return Err(error(
            "SOP_REVISION_CONFLICT",
            format!("Expected SOP revision {expected}, found {revision}."),
            None,
        ));
    }
    let authorization = if human {
        let approval = pinned_approval(&current)?;
        validate_authority(authority, session, &approval)?;
        if field(input, "expectedRevision").is_undefined() {
            return Err(error(
                "SOP_REVISION_REQUIRED",
                "Approval requires the original revision.",
                None,
            ));
        }
        Some(object([
            ("authority", authority.unwrap().clone()),
            ("approval", approval),
            ("expectedRevision", field(input, "expectedRevision").clone()),
        ]))
    } else {
        None
    };
    let result = object([
        ("accepted", JsValue::Bool(true)),
        ("duplicate", JsValue::Bool(false)),
        ("sessionId", JsValue::String(session.clone())),
        ("requestId", JsValue::String(request_id.clone())),
        ("revision", JsValue::Number(revision + 1.0)),
        ("message", field(input, "message").clone()),
    ]);
    let mut saved = result.clone();
    set(&mut saved, "waitId", field(input, "waitId").clone());
    set(&mut saved, "source", field(input, "source").clone());
    set(
        &mut saved,
        "slotUpdates",
        if matches!(
            field(input, "slotUpdates"),
            JsValue::Undefined | JsValue::Null
        ) {
            object([])
        } else {
            field(input, "slotUpdates").clone()
        },
    );
    if let Some(authorization) = authorization {
        set(&mut saved, "authorization", authorization);
    }
    let mut requests = spread(field(&current, "resumeRequests"));
    requests.insert(request_id.clone(), saved);
    let mut state = field(&current, "state").clone();
    let mut slots = if field(&state, "slots_json").as_object().is_some() {
        spread(field(&state, "slots_json"))
    } else {
        JsObject::new()
    };
    for (key, value) in spread(field(input, "slotUpdates")).entries() {
        slots.insert(key.clone(), value.clone());
    }
    set(&mut state, "status", "active".into());
    set(&mut state, "awaiting_input_json", JsValue::Null);
    set(&mut state, "slots_json", JsValue::Object(slots));
    set(&mut current, "state", state);
    set(&mut current, "revision", JsValue::Number(revision + 1.0));
    set(&mut current, "wait", JsValue::Undefined);
    set(&mut current, "resumeRequests", JsValue::Object(requests));
    set(&mut current, "updatedAt", now().as_str().into());
    write(root, session, &current)?;
    Ok(result)
}

fn receipt(value: &JsValue, duplicate: bool) -> JsValue {
    object([
        ("accepted", JsValue::Bool(true)),
        ("duplicate", JsValue::Bool(duplicate)),
        ("sessionId", field(value, "sessionId").clone()),
        ("requestId", field(value, "requestId").clone()),
        ("revision", field(value, "revision").clone()),
        ("message", field(value, "message").clone()),
    ])
}

pub(crate) fn read(
    root: &Path,
    session: impl Into<JsString>,
) -> Result<Option<JsValue>, RequestError> {
    let session = session.into();
    let session = &session;
    let bytes = match fs::read(path(root, session.clone())) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(error(
                "gateway_request_failed",
                session_message(
                    "Failed to read StaffDeck SOP state for '",
                    session,
                    &format!("': {e}"),
                ),
                None,
            ));
        }
    };
    let source = String::from_utf8_lossy(&bytes);
    let current = parse_json(&source).map_err(|e| {
        let message =
            pilotdeck_protocol::json_syntax::parse_error(&source).unwrap_or_else(|| e.to_string());
        error(
            "gateway_request_failed",
            session_message(
                "StaffDeck SOP state for '",
                session,
                &format!("' is invalid JSON: {message}"),
            ),
            None,
        )
    })?;
    if !matches!(
        field(&current, "schemaVersion").as_number(),
        Some(1.0 | 2.0 | 3.0)
    ) || field(&current, "sessionId").as_string() != Some(session)
        || field(&current, "bundle").as_object().is_none()
        || !matches!(field(field(&current, "bundle"), "sops"), JsValue::Array(_))
        || field(&current, "state").as_object().is_none()
    {
        return Err(error(
            "gateway_request_failed",
            session_message(
                "StaffDeck SOP state for '",
                session,
                "' has an invalid schema.",
            ),
            None,
        ));
    }
    let revision = field(&current, "revision")
        .as_number()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n > 0.0)
        .unwrap_or(1.0);
    let mut next = object([
        ("schemaVersion", JsValue::Number(3.0)),
        ("sessionId", JsValue::String(session.clone())),
        ("bundle", field(&current, "bundle").clone()),
        ("state", field(&current, "state").clone()),
        ("revision", JsValue::Number(revision)),
    ]);
    let wait = field(&current, "wait");
    if wait.as_object().is_some()
        && field(wait, "id").as_string().is_some()
        && matches!(
            text(wait, "kind").as_deref(),
            Some("handoff" | "external_task")
        )
        && field(wait, "createdAt").as_string().is_some()
    {
        set(&mut next, "wait", wait.clone());
    }
    let delivery = field(&current, "replyDelivery");
    let result = field(delivery, "result");
    if delivery.as_object().is_some()
        && field(delivery, "turnId").as_string().is_some()
        && matches!(
            text(delivery, "phase").as_deref(),
            Some("pending" | "durable")
        )
        && result.as_object().is_some()
        && field(result, "status").as_string().is_some()
        && field(result, "replyFragment").as_string().is_some()
        && field(result, "slotUpdates").as_object().is_some()
        && matches!(field(result, "events"), JsValue::Array(_))
        && field(delivery, "committedAt").as_string().is_some()
        && (field(delivery, "durableAt").is_undefined()
            || field(delivery, "durableAt").as_string().is_some())
    {
        set(&mut next, "replyDelivery", delivery.clone());
    }
    let mut requests = JsObject::new();
    if let Some(o) = field(&current, "resumeRequests").as_object() {
        for (key, r) in o.entries() {
            if r.as_object().is_some()
                && field(r, "accepted").as_bool() == Some(true)
                && ["sessionId", "requestId", "message"]
                    .iter()
                    .all(|k| field(r, k).as_string().is_some())
                && field(r, "revision").as_number().is_some()
            {
                requests.insert(key.clone(), r.clone());
            }
        }
    }
    set(&mut next, "resumeRequests", JsValue::Object(requests));
    set(
        &mut next,
        "updatedAt",
        if field(&current, "updatedAt").as_string().is_some() {
            field(&current, "updatedAt").clone()
        } else {
            "1970-01-01T00:00:00.000Z".into()
        },
    );
    Ok(Some(next))
}

fn session_message(prefix: &str, session: &JsString, suffix: &str) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(session.units());
    units.extend(suffix.encode_utf16());
    JsString::from_utf16(units)
}

fn write(root: &Path, session: &JsString, value: &JsValue) -> Result<(), RequestError> {
    let bytes = stringify_json(value)
        .map_err(|e| error("gateway_request_failed", e.to_string(), None))?
        .unwrap_or_default()
        + "\n";
    let target = path(root, session.clone());
    let temporary = target.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut owned = false;
    let persisted = (|| -> io::Result<()> {
        fs::create_dir_all(root)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        owned = true;
        file.write_all(bytes.as_bytes())?;
        drop(file);
        fs::rename(&temporary, &target)
    })();
    if let Err(e) = persisted {
        if owned {
            let _ = fs::remove_file(&temporary);
        }
        return Err(error(
            "gateway_request_failed",
            session_message(
                "Failed to persist StaffDeck SOP state for '",
                session,
                &format!("': {e}"),
            ),
            None,
        ));
    }
    Ok(())
}

fn pinned_approval(value: &JsValue) -> Result<JsValue, RequestError> {
    let wait = field(value, "wait");
    let state = field(value, "state");
    let invalid = |message| error("SOP_APPROVAL_PIN_INVALID", message, None);
    if text(wait, "kind").as_deref() != Some("handoff")
        || field(wait, "skillId") != field(state, "active_skill_id")
        || field(wait, "stepId") != field(state, "active_step_id")
    {
        return Err(invalid("Wait does not match the pinned active node."));
    }
    let JsValue::Array(sops) = field(field(value, "bundle"), "sops") else {
        return Err(invalid("Pinned SOP is missing or ambiguous."));
    };
    let matches: Vec<_> = sops
        .iter()
        .filter(|sop| {
            field(sop, "skill_id") == field(wait, "skillId")
                || field(sop, "id") == field(wait, "skillId")
        })
        .collect();
    if matches.len() != 1 {
        return Err(invalid("Pinned SOP is missing or ambiguous."));
    }
    let sop = matches[0];
    let content = if field(sop, "content").as_object().is_some() {
        field(sop, "content")
    } else {
        sop
    };
    let nodes: Vec<_> = match field(content, "nodes") {
        JsValue::Array(nodes) => nodes
            .iter()
            .filter(|n| n.as_object().is_some() && field(n, "node_id") == field(wait, "stepId"))
            .collect(),
        _ => vec![],
    };
    if nodes.len() != 1
        || field(sop, "version")
            .as_string()
            .is_none_or(|s| s.is_empty())
    {
        return Err(invalid("Pinned version or node is missing or ambiguous."));
    }
    if !nonempty(field(nodes[0], "assignee_user_id")) {
        return Err(error(
            "SOP_APPROVAL_ASSIGNEE_REQUIRED",
            "Unassigned handoff requires its original owner policy; no default approver is inferred.",
            None,
        ));
    }
    Ok(object([
        ("waitId", field(wait, "id").clone()),
        ("revision", field(value, "revision").clone()),
        ("skillId", field(wait, "skillId").clone()),
        ("version", field(sop, "version").clone()),
        ("nodeId", field(wait, "stepId").clone()),
        (
            "assigneeUserId",
            field(nodes[0], "assignee_user_id").clone(),
        ),
    ]))
}
fn validate_authority(
    authority: Option<&JsValue>,
    session: &JsString,
    approval: &JsValue,
) -> Result<(), RequestError> {
    let authority = authority.unwrap_or(&super::value::UNDEFINED);
    let subject = field(authority, "subject");
    if field(authority, "sessionId").as_string() != Some(session)
        || !truthy(field(authority, "tenantId"))
        || field(subject, "tenantId") != field(authority, "tenantId")
        || text(subject, "source").as_deref() != Some("web")
        || truthy(field(subject, "disabled"))
        || !truthy(field(subject, "userId"))
        || !matches!(text(subject, "role").as_deref(), Some("admin" | "member"))
    {
        return Err(error(
            "SOP_APPROVAL_AUTH_REQUIRED",
            "Authenticated native subject and original session access are required.",
            None,
        ));
    }
    if text(subject, "role").as_deref() != Some("admin")
        && field(subject, "userId") != field(approval, "assigneeUserId")
    {
        return Err(error(
            "SOP_APPROVAL_FORBIDDEN",
            "Handoff is not assigned to this member.",
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    #[tokio::test]
    async fn original_pin_read_checks_revision_and_active_state() {
        let root = std::env::temp_dir().join(format!("authority-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let v=parse_json(r#"{"schemaVersion":3,"sessionId":"s","revision":2,"bundle":{"sops":[{"id":"sop","version":"pin","content":{"nodes":[]}}]},"state":{"status":"active","active_skill_id":"sop","active_step_id":"node"}}"#).unwrap();
        std::fs::write(path(&root, "s"), stringify_json(&v).unwrap().unwrap()).unwrap();
        let result = authority(&root, "s", &JsValue::Number(2.0)).await.unwrap();
        assert_eq!(text(&result, "sopVersion").as_deref(), Some("pin"));
        assert_eq!(text(&result, "nodeId").as_deref(), Some("node"));
        assert_eq!(
            authority(&root, "s", &JsValue::Number(1.0))
                .await
                .unwrap_err()
                .code,
            "SOP_REVISION_CONFLICT"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
