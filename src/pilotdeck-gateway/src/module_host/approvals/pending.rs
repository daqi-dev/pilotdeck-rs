//! The pending SOP state the approvals bridge answers from — the A7.3
//! pending bus (`SopStateStore.ts` + the `StaffDeckSopControlPlane` front,
//! baseline cc7c4e6), ported as the one state source both this HTTP surface
//! and the WS `sop_status`/`sop_resume` family share. This module owns the
//! ported `resume`/`status` semantics cell by cell: receipt replay
//! (`isDeepStrictEqual` of the authority and payload), wait staleness,
//! revision conflict, the handoff pinning check, and the success mutation.
//!
//! Two deliberate scope lines, both wiring-lane follow-ups (#1305/#1313):
//! the TS store persists each state under
//! `<stateRoot>/sessions/<base64url(sessionId)>.json` with an atomic
//! temp-file rename, and its `read` can fail on a corrupted file (invalid
//! JSON, a wrong `sessionId`, a bad schema) with a plain `Error` the
//! transport maps to 502 `PUBLIC_APPROVAL_BRIDGE_FAILED`. This port keeps
//! the same states in memory — seeded as the already-migrated persisted
//! shape — because no wiring exists yet to give it a state root; the
//! corruption cells are therefore unreachable here and are not claimed as
//! covered. The TS per-session promise-chain lock serializes operations on
//! one session; a mutex over the whole map is strictly stronger and the
//! observable cells (the ones the transport answers) have no
//! interleaving-dependent branch.
//!
//! The store is a component seam, not a global: the tests construct one and
//! the later wiring lane will construct the real one.
#![cfg_attr(not(test), allow(dead_code))]

use super::{
    ApprovalFailure, ApprovalPendingStore, ResumeSource, StoreAuthority, StoreResumeInput,
    StoreStatusInput, js_truthy, state_error,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// The ported `sopStateError`: a code and the message the status snapshot's
/// `approvalError` projection exposes.
struct PendingFailure {
    code: &'static str,
    message: &'static str,
}

fn pending(code: &'static str, message: &'static str) -> PendingFailure {
    PendingFailure { code, message }
}

/// One migrated `PersistedSopState`: the raw JSON the TS store keeps on disk,
/// with the same defaults `migratePersistedState` applies (a positive integer
/// revision, a structurally valid wait, receipts filtered by
/// `isResumeResult`).
struct PendingState {
    session_id: JsString,
    bundle: JsValue,
    state: JsValue,
    revision: f64,
    wait: Option<JsValue>,
    resume_requests: JsObject,
}

/// The approvals-side pending SOP store: the `StaffDeckSopControlPlane`
/// profile gate (per project key, `SOP_MODULE_DISABLED` when absent) in front
/// of one store per resolved state root, holding the session states in
/// memory.
/// The `resolveProfile` port type: the project key's SOP runtime config, as
/// the state-root identity the control plane keys its stores by. `None` is
/// the missing profile.
pub(crate) type ProfileResolver = Arc<dyn Fn(Option<&JsValue>) -> Option<String> + Send + Sync>;

pub(crate) struct PendingSopStore {
    profiles: ProfileResolver,
    states: Mutex<HashMap<(String, JsString), PendingState>>,
}

impl PendingSopStore {
    pub(crate) fn new(profiles: ProfileResolver) -> Self {
        Self {
            profiles,
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Seed one session's persisted state, the way a state file would load:
    /// the raw persisted shape, run through the same migration the TS
    /// `read` applies (revision default, wait validation, receipt filtering).
    /// Test/fixture surface until the wiring lane owns a real state root.
    pub(crate) fn seed(&self, project_key: Option<&JsValue>, session_id: &JsString, raw: &JsValue) {
        let root = (self.profiles)(project_key).expect("#1350 seed under a known profile");
        let state = migrate_persisted_state(session_id, raw);
        self.states
            .lock()
            .expect("#1350 pending store lock")
            .insert((root, session_id.clone()), state);
    }

    fn status_locked(&self, input: &StoreStatusInput) -> Result<Option<JsValue>, ApprovalFailure> {
        // `StaffDeckSopControlPlane.store(projectKey)`: the profile gate
        // precedes every session lookup.
        let root = ((self.profiles)(input.project_key.as_ref()))
            .ok_or_else(|| state_error("SOP_MODULE_DISABLED"))?;
        let guard = self.states.lock().expect("#1350 pending store lock");
        let Some(current) = guard.get(&(root, input.session_key.clone())) else {
            return Ok(None);
        };
        Ok(Some(to_status_snapshot(current)))
    }

    fn resume_locked(&self, input: &StoreResumeInput) -> Result<JsValue, ApprovalFailure> {
        let root = ((self.profiles)(input.project_key.as_ref()))
            .ok_or_else(|| state_error("SOP_MODULE_DISABLED"))?;
        let mut guard = self.states.lock().expect("#1350 pending store lock");
        let Some(current) = guard.get_mut(&(root, input.session_key.clone())) else {
            return Err(state_error("SOP_SESSION_NOT_FOUND"));
        };
        // The duplicate receipt replay, `SopStateStore.resume`'s first branch.
        let duplicate = current
            .resume_requests
            .get_key(&input.request_id)
            .filter(|value| js_truthy(value))
            .cloned();
        if !crate::mutants::on("approvals-duplicate-off")
            && let Some(duplicate) = duplicate
        {
            return replay_receipt(input, &duplicate);
        }
        // `!current.wait` — after migration the wait is a valid object or
        // absent, so `Some` is the truthy case.
        let Some(wait) = current.wait.clone() else {
            return Err(state_error("SOP_NOT_WAITING"));
        };
        // `current.wait.id !== input.waitId` — the id is a string by `isWait`.
        let stale = !matches!(wait.get("id"), Some(JsValue::String(id)) if id == &input.wait_id);
        if stale {
            return Err(state_error("SOP_WAIT_STALE"));
        }
        let expected_source = match wait.get("kind") {
            Some(JsValue::String(kind)) if kind.equals_str("handoff") => ResumeSource::Human,
            _ => ResumeSource::ExternalTask,
        };
        if input.source != expected_source {
            return Err(state_error("SOP_RESUME_SOURCE_INVALID"));
        }
        if let Some(expected) = input.expected_revision
            && strict_not_equal(expected, current.revision)
        {
            return Err(state_error("SOP_REVISION_CONFLICT"));
        }
        let mut authorization: Option<JsValue> = None;
        if matches!(expected_source, ResumeSource::Human) {
            let approval = pinned_approval(current).map_err(|error| {
                // `toStatusSnapshot` projects this failure; resume propagates
                // the bare code to the transport.
                state_error(error.code)
            })?;
            validate_authority(input.authority.as_ref(), &input.session_key, &approval)
                .map_err(|error| state_error(error.code))?;
            let Some(expected) = input.expected_revision else {
                return Err(state_error("SOP_REVISION_REQUIRED"));
            };
            authorization = Some(crate::metadata::object([
                ("authority", authority_value(input.authority.as_ref())),
                ("approval", approval),
                ("expectedRevision", JsValue::Number(expected)),
            ]));
        }
        let revision = current.revision + 1.0;
        let result = resume_result(
            &input.session_key,
            &input.request_id,
            revision,
            &input.message,
        );
        // The receipt the persisted state stores: the result plus the
        // replay facts, `{...result, waitId, source, slotUpdates,
        // authorization}` — a separate object, so the returned result keeps
        // its own key set.
        let mut receipt = result.clone();
        receipt.insert("waitId".into(), JsValue::String(input.wait_id.clone()));
        receipt.insert("source".into(), JsValue::from(input.source.as_str()));
        receipt.insert(
            "slotUpdates".into(),
            input
                .slot_updates
                .clone()
                .unwrap_or(JsValue::Object(JsObject::new())),
        );
        if let Some(authorization) = authorization {
            receipt.insert("authorization".into(), authorization);
        }
        // The success mutation: `status: "active"`, `awaiting_input_json:
        // null`, `slots_json` merged, the wait cleared, the receipt stored.
        let state = {
            let mut next = current.state.clone();
            let JsValue::Object(object) = &mut next else {
                unreachable!("#1350 state is an object")
            };
            object.insert("status".into(), JsValue::from("active"));
            object.insert("awaiting_input_json".into(), JsValue::Null);
            let merged = {
                let mut slots = JsObject::new();
                if let Some(existing) = current
                    .state
                    .get("slots_json")
                    .filter(|value| value.as_object().is_some())
                {
                    spread_into(&mut slots, existing);
                }
                if let Some(updates) = &input.slot_updates {
                    spread_into(&mut slots, updates);
                }
                JsValue::Object(slots)
            };
            object.insert("slots_json".into(), merged);
            next
        };
        current.state = state;
        current.revision = revision;
        current.wait = None;
        current
            .resume_requests
            .insert(input.request_id.clone(), JsValue::Object(receipt));
        Ok(JsValue::Object(result))
    }
}

impl ApprovalPendingStore for PendingSopStore {
    fn status<'a>(
        &'a self,
        input: &'a StoreStatusInput,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move { self.status_locked(input) })
    }

    fn resume<'a>(
        &'a self,
        input: &'a StoreResumeInput,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move { self.resume_locked(input) })
    }
}

/// The duplicate-receipt branch of `SopStateStore.resume`: for the human
/// source the legacy-receipt, authority and original-revision checks, then
/// the payload identity checks, then the stored receipt's own result.
fn replay_receipt(
    input: &StoreResumeInput,
    duplicate: &JsValue,
) -> Result<JsValue, ApprovalFailure> {
    if matches!(input.source, ResumeSource::Human) {
        // `if (!duplicate.authorization)`.
        let authorization = duplicate
            .get("authorization")
            .filter(|value| js_truthy(value));
        let Some(authorization) = authorization else {
            return Err(state_error("SOP_APPROVAL_LEGACY_RECEIPT"));
        };
        // `validateAuthority(input.authority, input.sessionId,
        // duplicate.authorization.approval)`.
        let approval = authorization
            .get("approval")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        validate_authority(input.authority.as_ref(), &input.session_key, &approval)
            .map_err(|error| state_error(error.code))?;
        // `!isDeepStrictEqual(duplicate.authorization.authority,
        // input.authority)`.
        let stored_authority = authorization
            .get("authority")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        if !deep_strict_equal(
            &stored_authority,
            &authority_value(input.authority.as_ref()),
        ) {
            return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
        }
        // `input.expectedRevision !== duplicate.authorization.expectedRevision`.
        let stored_revision = match authorization.get("expectedRevision") {
            Some(JsValue::Number(number)) => Some(*number),
            _ => None,
        };
        if input.expected_revision != stored_revision {
            return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
        }
    }
    // `duplicate.message !== input.message` — a strict compare of whatever
    // the stored receipt carries.
    let message_equal =
        matches!(duplicate.get("message"), Some(JsValue::String(text)) if text == &input.message);
    if !message_equal {
        return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
    }
    // `(duplicate.waitId !== undefined && duplicate.waitId !== input.waitId)`.
    if let Some(wait_id) = duplicate
        .get("waitId")
        .filter(|value| !value.is_undefined())
        && !matches!(wait_id, JsValue::String(text) if text == &input.wait_id)
    {
        return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
    }
    // `(duplicate.source !== undefined && duplicate.source !== input.source)`.
    if let Some(source) = duplicate
        .get("source")
        .filter(|value| !value.is_undefined())
    {
        let equal =
            matches!(source, JsValue::String(text) if text.equals_str(input.source.as_str()));
        if !equal {
            return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
        }
    }
    // `(duplicate.waitId !== undefined && !isDeepStrictEqual(
    // duplicate.slotUpdates ?? {}, input.slotUpdates ?? {}))`.
    let has_wait_id = duplicate
        .get("waitId")
        .filter(|value| !value.is_undefined())
        .is_some();
    if has_wait_id {
        let stored = nullish_default(duplicate.get("slotUpdates"));
        let incoming = nullish_default(input.slot_updates.as_ref());
        if !deep_strict_equal(&stored, &incoming) {
            return Err(state_error("SOP_RESUME_REQUEST_CONFLICT"));
        }
    }
    // `{ accepted: true, duplicate: true, sessionId: duplicate.sessionId,
    // requestId: duplicate.requestId, revision: duplicate.revision,
    // message: duplicate.message }` — the stored receipt's own fields.
    Ok(crate::metadata::object([
        ("accepted", JsValue::Bool(true)),
        ("duplicate", JsValue::Bool(true)),
        (
            "sessionId",
            duplicate
                .get("sessionId")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "requestId",
            duplicate
                .get("requestId")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "revision",
            match duplicate.get("revision") {
                Some(JsValue::Number(number)) => JsValue::Number(*number),
                _ => JsValue::Undefined,
            },
        ),
        (
            "message",
            duplicate
                .get("message")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ),
    ]))
}

/// The fresh-resume result object, key order per `SopStateStore.resume`.
fn resume_result(
    session_id: &JsString,
    request_id: &JsString,
    revision: f64,
    message: &JsString,
) -> JsObject {
    let mut result = JsObject::new();
    result.insert("accepted".into(), JsValue::Bool(true));
    result.insert("duplicate".into(), JsValue::Bool(false));
    result.insert("sessionId".into(), JsValue::String(session_id.clone()));
    result.insert("requestId".into(), JsValue::String(request_id.clone()));
    result.insert("revision".into(), JsValue::Number(revision));
    result.insert("message".into(), JsValue::String(message.clone()));
    result
}

/// `toStatusSnapshot` (`SopStateStore.ts`): the status body in its key order,
/// with the handoff pinning projected as `approval` or `approvalError`.
fn to_status_snapshot(value: &PendingState) -> JsValue {
    let mut projection: Option<(&'static str, JsValue)> = None;
    let handoff = matches!(
        value.wait.as_ref().and_then(|wait| wait.get("kind")),
        Some(JsValue::String(kind)) if kind.equals_str("handoff")
    );
    if handoff {
        projection = match pinned_approval(value) {
            Ok(approval) => Some(("approval", approval)),
            Err(error) => Some((
                "approvalError",
                crate::metadata::object([
                    ("code", JsValue::from(error.code)),
                    ("message", JsValue::from(error.message)),
                ]),
            )),
        };
    }
    let mut snapshot = JsObject::new();
    snapshot.insert(
        "sessionId".into(),
        JsValue::String(value.session_id.clone()),
    );
    snapshot.insert("revision".into(), JsValue::Number(value.revision));
    snapshot.insert("state".into(), value.state.clone());
    if let Some(wait) = &value.wait {
        snapshot.insert("wait".into(), wait.clone());
    }
    if let Some((key, value)) = projection {
        snapshot.insert(key.into(), value);
    }
    JsValue::Object(snapshot)
}

/// `pinnedApproval` (`SopStateStore.ts`): the handoff wait pinned against the
/// bundle's SOP node and its assignee. Every failure here carries the code
/// the snapshot's `approvalError` (and resume's 502) answers with.
fn pinned_approval(value: &PendingState) -> Result<JsValue, PendingFailure> {
    let wait = value
        .wait
        .as_ref()
        .and_then(|wait| wait.as_object())
        .ok_or_else(|| {
            pending(
                "SOP_APPROVAL_PIN_INVALID",
                "Wait does not match the pinned active node.",
            )
        })?;
    // `wait.skillId !== value.state.active_skill_id || wait.stepId !==
    // value.state.active_step_id`.
    let skill_mismatch = wait.get("skillId") != value.state.get("active_skill_id");
    let step_mismatch = wait.get("stepId") != value.state.get("active_step_id");
    if !matches!(wait.get("kind"), Some(JsValue::String(kind)) if kind.equals_str("handoff"))
        || skill_mismatch
        || step_mismatch
    {
        return Err(pending(
            "SOP_APPROVAL_PIN_INVALID",
            "Wait does not match the pinned active node.",
        ));
    }
    let skill_id = wait.get("skillId");
    let matches = match value.bundle.get("sops") {
        Some(JsValue::Array(sops)) => sops.clone(),
        _ => Vec::new(),
    }
    .into_iter()
    // `sop.skill_id === wait.skillId || sop.id === wait.skillId`: a
    // missing field is `undefined`, which also equals an absent skillId.
    .filter(|sop| match skill_id {
        Some(id) => sop.get("skill_id") == Some(id) || sop.get("id") == Some(id),
        None => sop.get("skill_id").is_none() || sop.get("id").is_none(),
    })
    .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(pending(
            "SOP_APPROVAL_PIN_INVALID",
            "Pinned SOP is missing or ambiguous.",
        ));
    }
    let sop = matches.into_iter().next().expect("#1350 the single match");
    let content = match &sop {
        JsValue::Object(_) => sop
            .get("content")
            .filter(|content| content.as_object().is_some())
            .cloned()
            .unwrap_or_else(|| sop.clone()),
        _ => sop.clone(),
    };
    let step_id = wait.get("stepId");
    let nodes = match content.get("nodes") {
        Some(JsValue::Array(nodes)) => nodes.clone(),
        _ => Vec::new(),
    }
    .into_iter()
    // `node.node_id === wait.stepId`, same missing-field rule.
    .filter(|node| node.as_object().is_some() && node.get("node_id") == step_id)
    .collect::<Vec<_>>();
    let version_ok =
        matches!(sop.get("version"), Some(JsValue::String(version)) if version.len_utf16() > 0);
    if nodes.len() != 1 || !version_ok {
        return Err(pending(
            "SOP_APPROVAL_PIN_INVALID",
            "Pinned version or node is missing or ambiguous.",
        ));
    }
    let node = nodes.into_iter().next().expect("#1350 the single node");
    let assignee = node.get("assignee_user_id");
    let assignee_ok = matches!(assignee, Some(JsValue::String(text)) if crate::model_catalog::trim(text).len_utf16() > 0);
    if !assignee_ok {
        return Err(pending(
            "SOP_APPROVAL_ASSIGNEE_REQUIRED",
            "Unassigned handoff requires its original owner policy; no default approver is inferred.",
        ));
    }
    let mut approval = JsObject::new();
    if let Some(JsValue::String(id)) = wait.get("id") {
        approval.insert("waitId".into(), JsValue::String(id.clone()));
    }
    approval.insert("revision".into(), JsValue::Number(value.revision));
    if let Some(skill) = skill_id.cloned() {
        approval.insert("skillId".into(), skill);
    }
    if let Some(JsValue::String(version)) = sop.get("version") {
        approval.insert("version".into(), JsValue::String(version.clone()));
    }
    if let Some(step) = step_id.cloned() {
        approval.insert("nodeId".into(), step);
    }
    if let Some(assignee) = assignee {
        approval.insert("assigneeUserId".into(), assignee.clone());
    }
    Ok(JsValue::Object(approval))
}

/// `validateAuthority` (`SopStateStore.ts`): the authority must come from the
/// resolved session, and a member must be the pinned assignee.
fn validate_authority(
    authority: Option<&StoreAuthority>,
    session_id: &JsString,
    approval: &JsValue,
) -> Result<(), PendingFailure> {
    let Some(authority) = authority else {
        return Err(pending(
            "SOP_APPROVAL_AUTH_REQUIRED",
            "Authenticated native subject and original session access are required.",
        ));
    };
    let required = pending(
        "SOP_APPROVAL_AUTH_REQUIRED",
        "Authenticated native subject and original session access are required.",
    );
    if authority.session_id != *session_id
        || authority.tenant_id.is_empty()
        || authority.subject.tenant_id != authority.tenant_id
        || !authority.subject.source.equals_str("web")
        || authority.subject.disabled
        || authority.subject.user_id.is_empty()
        || !(authority.subject.role.equals_str("admin")
            || authority.subject.role.equals_str("member"))
    {
        return Err(required);
    }
    // Preserve SD handoff policy: admins may reply; members must be the
    // assignee.
    if !authority.subject.role.equals_str("admin") {
        let assignee = approval.get("assigneeUserId");
        if !matches!(assignee, Some(JsValue::String(id)) if id == &authority.subject.user_id) {
            return Err(pending(
                "SOP_APPROVAL_FORBIDDEN",
                "Handoff is not assigned to this member.",
            ));
        }
    }
    Ok(())
}

/// The authority as the bridge constructed it, for `isDeepStrictEqual`
/// against a stored receipt's copy. Key order is irrelevant to the deep
/// compare but is kept identical to the TS construction anyway.
fn authority_value(authority: Option<&StoreAuthority>) -> JsValue {
    let Some(authority) = authority else {
        return JsValue::Undefined;
    };
    crate::metadata::object([
        ("tenantId", JsValue::String(authority.tenant_id.clone())),
        ("sessionId", JsValue::String(authority.session_id.clone())),
        (
            "subject",
            crate::metadata::object([
                (
                    "tenantId",
                    JsValue::String(authority.subject.tenant_id.clone()),
                ),
                ("source", JsValue::String(authority.subject.source.clone())),
                ("disabled", JsValue::Bool(authority.subject.disabled)),
                ("userId", JsValue::String(authority.subject.user_id.clone())),
                ("role", JsValue::String(authority.subject.role.clone())),
            ]),
        ),
    ])
}

/// `migratePersistedState`: the defaults and validation a state load applies
/// before any operation sees it.
fn migrate_persisted_state(session_id: &JsString, raw: &JsValue) -> PendingState {
    let revision = match raw.get("revision") {
        Some(JsValue::Number(number)) if number.trunc() == *number && *number > 0.0 => *number,
        _ => 1.0,
    };
    let wait = raw.get("wait").filter(|wait| is_wait(wait)).cloned();
    let resume_requests = match raw.get("resumeRequests") {
        Some(JsValue::Object(object)) => {
            let mut filtered = JsObject::new();
            for (key, value) in object.own_entries() {
                if is_resume_result(value) {
                    filtered.insert(key.clone(), value.clone());
                }
            }
            filtered
        }
        _ => JsObject::new(),
    };
    PendingState {
        session_id: session_id.clone(),
        bundle: raw
            .get("bundle")
            .cloned()
            .unwrap_or(JsValue::Object(JsObject::new())),
        state: raw
            .get("state")
            .cloned()
            .unwrap_or(JsValue::Object(JsObject::new())),
        revision,
        wait,
        resume_requests,
    }
}

/// `isWait`: a record with a string id, a known kind and a string createdAt.
fn is_wait(value: &JsValue) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    matches!(object.get("id"), Some(JsValue::String(_)))
        && matches!(object.get("kind"), Some(JsValue::String(kind))
            if kind.equals_str("handoff") || kind.equals_str("external_task"))
        && matches!(object.get("createdAt"), Some(JsValue::String(_)))
}

/// `isResumeResult`: the shape a stored receipt must have to survive
/// migration.
fn is_resume_result(value: &JsValue) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("accepted") == Some(&JsValue::Bool(true))
        && matches!(object.get("sessionId"), Some(JsValue::String(_)))
        && matches!(object.get("requestId"), Some(JsValue::String(_)))
        && matches!(object.get("revision"), Some(JsValue::Number(_)))
        && matches!(object.get("message"), Some(JsValue::String(_)))
}

/// `nullish ?? {}`: `null` and `undefined` become the empty object, anything
/// else passes through.
fn nullish_default(value: Option<&JsValue>) -> JsValue {
    match value {
        Some(JsValue::Null | JsValue::Undefined) | None => JsValue::Object(JsObject::new()),
        Some(value) => value.clone(),
    }
}

/// Object spread of a JSON value into a target: an object's own entries, an
/// array's elements under their index keys, a string's code units under
/// theirs; primitives contribute nothing. The later value wins per key.
fn spread_into(target: &mut JsObject, source: &JsValue) {
    match source {
        JsValue::Object(object) => {
            for (key, value) in object.own_entries() {
                target.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(elements) => {
            for (index, value) in elements.iter().enumerate() {
                target.insert(JsString::from(index.to_string()), value.clone());
            }
        }
        JsValue::String(text) => {
            for (index, unit) in text.units().iter().enumerate() {
                target.insert(
                    JsString::from(index.to_string()),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
}

/// `node:util.isDeepStrictEqual`: structural equality with `SameValue`
/// primitives — `NaN` equals `NaN`, `+0` does not equal `-0` — and own
/// enumerable keys compared as a set, unlike [`JsValue`]'s order-sensitive
/// `PartialEq`.
pub(crate) fn deep_strict_equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a.to_bits() == b.to_bits(),
        (JsValue::String(a), JsValue::String(b)) => a == b,
        (JsValue::Array(a), JsValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| deep_strict_equal(a, b))
        }
        (JsValue::Object(a), JsValue::Object(b)) => {
            a.len() == b.len()
                && a.own_entries().iter().all(|(key, value)| {
                    b.get_key(key)
                        .is_some_and(|other| deep_strict_equal(value, other))
                })
        }
        _ => false,
    }
}

/// JavaScript strict `!==` for two revisions: numerically equal values are
/// strictly equal (`-0 === 0`), `NaN` equals nothing.
fn strict_not_equal(left: f64, right: f64) -> bool {
    !(left == right)
}
