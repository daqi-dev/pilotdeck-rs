//! Module-host approvals transport: the `POST /api/module-host/approvals/status`
//! and `POST /api/module-host/approvals/resume` endpoints StaffDeck approvers
//! use over the public host port (TS `ui/server` callers reach the same cells
//! through `GatewayServer.ts:96`, which composes
//! `handlePublicApprovalHttpRequest` in front of the module-host transport
//! and every other route). This is the #1350 component: it replicates the TS
//! `publicApprovalHttpTransport.ts` (baseline cc7c4e6) transport cell by cell
//! and the `publicApprovalBridge.ts` access pipeline over injected ports, so
//! the approval state is answered from the Rust host itself. It is not wired
//! into `native_process`/`native_http` dispatch yet (the #1305 wiring lane
//! owns that); a caller composes it in front of its own routing and treats
//! `None` as "not handled" — TS `handlePublicApprovalHttpRequest` returning
//! `false`.
//!
//! The pending SOP state the bridge answers from lives in [`pending`]: one
//! shared store both this HTTP surface and the WS `sop_status`/`sop_resume`
//! family (`native_process/methods/sop.rs`) read and write — the A8.4
//! "WS 与 HTTP 写同一状态并互相可见" requirement. The WS side is still the
//! capability-gated stub (#1313 follow-ups own its wiring); the component
//! tests demonstrate the shared-source visibility through the same store the
//! transport reaches.
//!
//! Cells deliberately not modeled, same as #1349: the AbortController
//! `req`-aborted / `res`-close wiring (it is bound to the real socket
//! lifecycle the native listener owns; on the error path it only turns the
//! response into a zero-byte close, which a `res.destroy()` produces in TS
//! and the hyper body error here) and the per-chunk `for await` pause points
//! of the body read (the budget is a cumulative byte sum, so the chunk split
//! of either stack cannot change its verdict). Transport-level byte
//! divergences of the hyper codec are the ones A8.4-NATIVE-HTTP-001 already
//! registers (header name case, chunk-size case, the HTTP/1.0 status line)
//! and the premature-close residual A8.4-MODULE-HOST-001 registers; the
//! approvals replay normalizes or pins them the same way.
//!
//! Scope note: the bridge's `authenticate` port is the seam the external
//! StaffDeck authority lives behind (TS loads
//! `ui/server/staffdeck-approval-authority.mjs` and performs network calls);
//! it stays injected, never implemented here.
#![cfg_attr(not(test), allow(dead_code))]
mod pending;

use crate::{Body, model_catalog::trim};
use bytes::Bytes;
use http_body_util::BodyExt;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{future::Future, io, pin::Pin, sync::Arc, time::SystemTime};

/// The transport's request-body budget, `publicApprovalHttpTransport.ts`:
/// strictly more than 64 KiB total is over-budget, exactly 64 KiB is
/// served. The over-budget verdict never reaches the wire in TS as the
/// transport's 413 frame (see the `ReadError::OverBudget` branch).
const INPUT_BUDGET: usize = 64 * 1024;

/// The top-level input keys the transport's field allowlist accepts, TS
/// `allowed` (`publicApprovalHttpTransport.ts:27-28`): the four shared fields
/// plus the six resume-only ones.
const STATUS_FIELDS: [&str; 4] = ["tenantId", "agentId", "sessionKey", "projectKey"];
const RESUME_FIELDS: [&str; 6] = [
    "source",
    "requestId",
    "waitId",
    "expectedRevision",
    "message",
    "slotUpdates",
];

/// `PublicApprovalBinding`: the three strings a fixed installation binds a
/// request to. Empty strings are legal values here — the bridge's own access
/// gate rejects them with 503 `APPROVAL_BINDING_UNAVAILABLE`.
pub(crate) struct ApprovalBinding {
    pub tenant_id: JsString,
    pub agent_id: JsString,
    pub pilot_deck_user_id: JsString,
}

impl Clone for ApprovalBinding {
    fn clone(&self) -> Self {
        Self {
            tenant_id: self.tenant_id.clone(),
            agent_id: self.agent_id.clone(),
            pilot_deck_user_id: self.pilot_deck_user_id.clone(),
        }
    }
}

/// The `throw Object.assign(new Error(message), { status, code })` channel the
/// outer TS catch maps to `status ?? (SOP_RESUME_REQUEST_CONFLICT ? 409 : 502)`
/// and `code ?? PUBLIC_APPROVAL_BRIDGE_FAILED`. A failure with neither field
/// is a plain `Error`; the code stays a raw value because a scripted bridge
/// can throw any `{code}` (a number serializes as `{"code":123}`).
#[derive(Debug)]
pub(crate) struct ApprovalFailure {
    pub status: Option<u16>,
    pub code: Option<JsValue>,
}

/// The `failure(status, code)` helper of `publicApprovalBridge.ts`.
fn failure(status: u16, code: &str) -> ApprovalFailure {
    ApprovalFailure {
        status: Some(status),
        code: Some(JsValue::String(JsString::from(code))),
    }
}

/// The `sopStateError(code, message)` channel of `SopStateStore.ts`: a code,
/// never a status — the transport's `cause.status ?? …` falls through to the
/// 409-or-502 branch. The message never reaches the wire through this
/// transport (the catch answers `{code}` only), so only the
/// `approvalError` projection of a status snapshot needs one, in `pending`.
fn state_error(code: &str) -> ApprovalFailure {
    ApprovalFailure {
        status: None,
        code: Some(JsValue::String(JsString::from(code))),
    }
}

/// The `PublicApprovalSubject` (`StaffDeckApprovalAuthority['subject']`): the
/// authenticated approver facts the authenticate port returns.
pub(crate) struct ApprovalSubject {
    pub tenant_id: JsString,
    pub source: JsString,
    pub disabled: bool,
    pub user_id: JsString,
    pub role: JsString,
}

/// `PublicApprovalSession`: the binding plus the resolved session keys. The
/// project key is the raw value because a scripted TS resolver can return
/// any JSON value for it, not only a string.
pub(crate) struct ApprovalSession {
    pub tenant_id: JsString,
    pub agent_id: JsString,
    pub pilot_deck_user_id: JsString,
    pub session_key: JsString,
    pub project_key: Option<JsValue>,
}

/// The `authenticate` port of `createPublicApprovalBridge`: validates the
/// `x-staffdeck-approver-authorization` bearer against the formal source and
/// returns only the identifier facts.
pub(crate) trait ApprovalAuthenticator: Send + Sync {
    fn authenticate<'a>(
        &'a self,
        bearer: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalSubject, ApprovalFailure>> + Send + 'a>>;
}

/// The `resolveSession` port: joins the registered session catalog and the
/// session's original admission. `None` is the TS `undefined`. The project
/// key is the raw input value because the TS resolver sees whatever the
/// caller sent, not only strings.
pub(crate) trait ApprovalSessionResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        session_key: &'a JsString,
        project_key: Option<&'a JsValue>,
        binding: &'a ApprovalBinding,
        subject: &'a ApprovalSubject,
    ) -> Pin<Box<dyn Future<Output = Option<ApprovalSession>> + Send + 'a>>;
}

/// The `resolveBinding` port: present only under the project-binding mode
/// (`STAFFDECK_PROJECT_BINDING_ENABLED`), where the binding is re-resolved
/// per request (`dynamicBinding`).
pub(crate) trait ApprovalBindingResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalBinding, ApprovalFailure>> + Send + 'a>>;
}

/// The resume source the store distinguishes: the bridge only ever sends
/// `'human'`; `'external_task'` is the WS continuation lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResumeSource {
    Human,
    ExternalTask,
}

impl ResumeSource {
    fn as_str(self) -> &'static str {
        match self {
            ResumeSource::Human => "human",
            ResumeSource::ExternalTask => "external_task",
        }
    }
}

/// The status input the pending store receives, the TS `sopControl.status`
/// callback's shape.
pub(crate) struct StoreStatusInput {
    pub session_key: JsString,
    pub project_key: Option<JsValue>,
}

/// The resume input the pending store receives, the TS `sopControl.resume`
/// callback's shape: everything but the authority is caller input, and the
/// authority is what the bridge's access pipeline resolved.
pub(crate) struct StoreResumeInput {
    pub session_key: JsString,
    pub project_key: Option<JsValue>,
    pub request_id: JsString,
    pub wait_id: JsString,
    pub message: JsString,
    pub expected_revision: Option<f64>,
    pub slot_updates: Option<JsValue>,
    pub source: ResumeSource,
    pub authority: Option<StoreAuthority>,
}

/// `StaffDeckApprovalAuthority` as the bridge constructs it: the resolved
/// session's tenant and the authenticated subject.
pub(crate) struct StoreAuthority {
    pub tenant_id: JsString,
    pub session_id: JsString,
    pub subject: ApprovalSubject,
}

/// The pending SOP state store seam (`StaffDeckSopControlPlane.status/resume`
/// in TS): `status` answers the snapshot (`None` = `undefined`), `resume`
/// answers the receipt result. Store failures carry a code and no status —
/// the `sopStateError` channel the transport maps to 502 except
/// `SOP_RESUME_REQUEST_CONFLICT` (409).
pub(crate) trait ApprovalPendingStore: Send + Sync {
    fn status<'a>(
        &'a self,
        input: &'a StoreStatusInput,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ApprovalFailure>> + Send + 'a>>;
    fn resume<'a>(
        &'a self,
        input: &'a StoreResumeInput,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>>;
}

/// The bridge `createPublicApprovalBridge` returns — the surface the
/// transport consumes: the (static) binding, the dynamic flag, and the two
/// operations.
pub(crate) trait ApprovalBridge: Send + Sync {
    fn binding(&self) -> &ApprovalBinding;
    fn dynamic_binding(&self) -> bool;
    fn status<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>>;
    fn resume<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>>;
}

/// `createPublicApprovalBridge`: the access pipeline plus the two operations,
/// every collaborating capability an injected port.
pub(crate) struct ApprovalBridgeImpl {
    pub binding: ApprovalBinding,
    pub resolve_binding: Option<Arc<dyn ApprovalBindingResolver>>,
    pub authenticate: Arc<dyn ApprovalAuthenticator>,
    pub resolve_session: Option<Arc<dyn ApprovalSessionResolver>>,
    pub store: Arc<dyn ApprovalPendingStore>,
}

impl ApprovalBridge for ApprovalBridgeImpl {
    fn binding(&self) -> &ApprovalBinding {
        &self.binding
    }

    fn dynamic_binding(&self) -> bool {
        self.resolve_binding.is_some()
    }

    fn status<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            let (session, subject) = self.access(input).await?;
            let snapshot = self
                .store
                .status(&StoreStatusInput {
                    session_key: session.session_key.clone(),
                    project_key: session.project_key.clone(),
                })
                .await?;
            if let Some(snapshot) = &snapshot {
                // `snapshot?.sessionId !== undefined && snapshot.sessionId !==
                // session.sessionKey`: only a present, different value fails
                // (a number sessionId is also `!==` the string).
                if let Some(id) = snapshot
                    .get("sessionId")
                    .filter(|value| !value.is_undefined())
                    && !matches!(id, JsValue::String(text) if text == &session.session_key)
                {
                    return Err(failure(502, "SOP_APPROVAL_SESSION_MISMATCH"));
                }
                if let Some(approval) = snapshot.get("approval").filter(|value| js_truthy(value))
                    && !subject.role.equals_str("admin")
                {
                    let assignee = approval.get("assigneeUserId");
                    if !matches!(assignee, Some(JsValue::String(id)) if id == &subject.user_id) {
                        return Err(failure(403, "SOP_APPROVAL_FORBIDDEN"));
                    }
                }
            }
            // `snapshot ?? null`: the bridge result for an absent state.
            Ok(snapshot.unwrap_or(JsValue::Null))
        })
    }

    fn resume<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Pin<Box<dyn Future<Output = Result<JsValue, ApprovalFailure>> + Send + 'a>> {
        Box::pin(async move {
            // TS validates the resume shape before `access()` runs, so a bad
            // waitId wins over a bad sessionKey (publicApprovalBridge.ts:67-68).
            if !crate::mutants::on("approvals-resume-validate-off") && !resume_input_valid(input) {
                return Err(failure(400, "APPROVAL_INPUT_INVALID"));
            }
            let (session, subject) = self.access(input).await?;
            // No pending-status preflight: the original locked store
            // authorizes first use and receipt replay.
            let slot_updates = input
                .get("slotUpdates")
                .filter(|value| js_truthy(value))
                .cloned();
            let expected_revision = match input.get("expectedRevision") {
                Some(JsValue::Number(number)) => Some(*number),
                _ => None,
            };
            self.store
                .resume(&StoreResumeInput {
                    session_key: session.session_key.clone(),
                    project_key: session.project_key.clone(),
                    request_id: string_field(input, "requestId")?,
                    wait_id: string_field(input, "waitId")?,
                    message: string_field(input, "message")?,
                    expected_revision,
                    slot_updates,
                    source: ResumeSource::Human,
                    authority: Some(StoreAuthority {
                        tenant_id: session.tenant_id.clone(),
                        session_id: session.session_key.clone(),
                        subject,
                    }),
                })
                .await
        })
    }
}

impl ApprovalBridgeImpl {
    /// The `access()` pipeline of `publicApprovalBridge.ts:31-55`: binding
    /// resolution, input shape, scope, authorization, subject checks and
    /// session resolution, in the TS statement order.
    async fn access<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> Result<(ApprovalSession, ApprovalSubject), ApprovalFailure> {
        let binding = if let Some(resolver) = &self.resolve_binding {
            resolver.resolve(input).await?
        } else {
            self.binding.clone()
        };
        // `!nonempty(input.sessionKey)`
        if !nonempty(input.get("sessionKey")) {
            return Err(failure(400, "APPROVAL_INPUT_INVALID"));
        }
        // `Object.hasOwn(input, 'authority') || Object.hasOwn(input, 'subject')`
        // — unreachable through the transport (the field allowlist rejects
        // them first) but part of the bridge contract for direct callers.
        if let Some(object) = input.as_object()
            && (object.contains_key("authority") || object.contains_key("subject"))
        {
            return Err(failure(400, "APPROVAL_AUTHORITY_OVERRIDE"));
        }
        // Scope: a present tenantId/agentId must equal the binding's.
        for (field, binding_value) in [
            ("tenantId", &binding.tenant_id),
            ("agentId", &binding.agent_id),
        ] {
            if let Some(value) = input.get(field).filter(|value| !value.is_undefined()) {
                let matches = matches!(value, JsValue::String(text) if text == binding_value);
                if !matches {
                    return Err(failure(403, "APPROVAL_SUBJECT_MISMATCH"));
                }
            }
        }
        if !(nonempty_string(&binding.tenant_id)
            && nonempty_string(&binding.agent_id)
            && nonempty_string(&binding.pilot_deck_user_id))
        {
            return Err(failure(503, "APPROVAL_BINDING_UNAVAILABLE"));
        }
        // The approver authorization: `authorization.slice(0, 7).toLowerCase()
        // !== 'bearer '` — an ASCII-case-insensitive scheme with the space,
        // then `nonempty(authorization.slice(7))`. The only Unicode simple
        // case mapping into the ASCII letters of "bearer " is U+212A → 'k',
        // which this literal does not contain, so the ASCII fold is exact.
        let authorization = input.get("approverAuthorization");
        let bearer = match authorization {
            Some(JsValue::String(text)) => {
                let units = text.units();
                let scheme_ok = units.len() >= 7
                    && units[..7]
                        .iter()
                        .zip(b"bearer ".iter())
                        .all(|(unit, expected)| {
                            let lowered = if (0x41..=0x5A).contains(unit) {
                                *unit + 32
                            } else {
                                *unit
                            };
                            lowered == u16::from(*expected)
                        });
                if !scheme_ok {
                    return Err(failure(401, "APPROVAL_AUTH_REQUIRED"));
                }
                let rest = JsString::from_utf16(units[7..].to_vec());
                if !nonempty_string(&rest) {
                    return Err(failure(401, "APPROVAL_AUTH_REQUIRED"));
                }
                rest
            }
            _ => return Err(failure(401, "APPROVAL_AUTH_REQUIRED")),
        };
        let subject = self.authenticate.authenticate(&bearer).await?;
        if subject.tenant_id != binding.tenant_id
            || !subject.source.equals_str("web")
            || subject.disabled
            || !nonempty_string(&subject.user_id)
            || !(subject.role.equals_str("admin") || subject.role.equals_str("member"))
        {
            return Err(failure(403, "APPROVAL_SUBJECT_MISMATCH"));
        }
        let Some(resolver) = &self.resolve_session else {
            return Err(failure(503, "SOP_APPROVAL_SESSION_MAPPING_UNAVAILABLE"));
        };
        let session_key = match input.get("sessionKey") {
            Some(JsValue::String(text)) => text.clone(),
            _ => JsString::default(),
        };
        let input_project = input
            .get("projectKey")
            .filter(|value| !value.is_undefined())
            .cloned();
        let Some(session) = resolver
            .resolve(&session_key, input_project.as_ref(), &binding, &subject)
            .await
        else {
            return Err(failure(403, "SOP_APPROVAL_SESSION_FORBIDDEN"));
        };
        if session.session_key != session_key
            || session.tenant_id != binding.tenant_id
            || session.agent_id != binding.agent_id
            || session.pilot_deck_user_id != binding.pilot_deck_user_id
            || input_project.is_some_and(|key| !strict_equal(&key, session.project_key.as_ref()))
        {
            return Err(failure(403, "SOP_APPROVAL_SESSION_FORBIDDEN"));
        }
        Ok((session, subject))
    }
}

/// The resume input shape the bridge demands before `access()` runs
/// (`publicApprovalBridge.ts:67-68`): `source` exactly `'human'`, the three
/// strings non-empty (after the ECMAScript trim), and `expectedRevision` a
/// non-negative safe integer (`Number.isSafeInteger`, so `-0` passes).
fn resume_input_valid(input: &JsValue) -> bool {
    let source_ok =
        matches!(input.get("source"), Some(JsValue::String(text)) if text.equals_str("human"));
    source_ok
        && nonempty(input.get("waitId"))
        && nonempty(input.get("requestId"))
        && nonempty(input.get("message"))
        && match input.get("expectedRevision") {
            Some(JsValue::Number(number)) => is_safe_integer(*number) && *number >= 0.0,
            _ => false,
        }
}

/// `Number.isSafeInteger`: an integer within ±(2^53 − 1). NaN and infinities
/// fail.
fn is_safe_integer(number: f64) -> bool {
    number.is_finite() && number.trunc() == number && number.abs() <= 9_007_199_254_740_991.0
}

/// `nonempty` of `publicApprovalBridge.ts`: a string with a non-empty trim.
/// The `model_catalog` trim keeps the exact ECMAScript whitespace set.
fn nonempty(value: Option<&JsValue>) -> bool {
    match value {
        Some(JsValue::String(text)) => trim(text).len_utf16() > 0,
        _ => false,
    }
}

fn nonempty_string(text: &JsString) -> bool {
    trim(text).len_utf16() > 0
}

/// A required resume field, already validated by [`resume_input_valid`].
fn string_field(input: &JsValue, name: &str) -> Result<JsString, ApprovalFailure> {
    match input.get(name) {
        Some(JsValue::String(text)) => Ok(text.clone()),
        _ => Err(failure(400, "APPROVAL_INPUT_INVALID")),
    }
}

/// JavaScript truthiness for a JSON value: every object and array, a
/// non-empty string, a non-zero non-NaN number, and `true`.
fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(bool) => *bool,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => text.len_utf16() > 0,
        _ => true,
    }
}

/// JavaScript strict equality (`===`) for two JSON values: primitives of the
/// same type with the same value, never two distinct objects or arrays.
/// Numeric comparison is value equality (`-0 === 0`, `NaN !== NaN`).
fn strict_equal(left: &JsValue, right: Option<&JsValue>) -> bool {
    match (left, right) {
        (value, Some(other)) => strict_equal_values(value, other),
        _ => false,
    }
}

fn strict_equal_values(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b,
        (JsValue::String(a), JsValue::String(b)) => a == b,
        _ => false,
    }
}

/// The transport's routing inputs: the bearer token and the optional
/// approval bridge. `bridge: None` mirrors TS's absent `options.bridge`
/// (`options.publicApprovals` unset), the 503 cell.
#[derive(Clone, Default)]
pub(crate) struct Config {
    pub token: String,
    pub bridge: Option<Arc<dyn ApprovalBridge>>,
}

/// Route one request through the approvals transport. `None` means the
/// request is not for this surface and the caller keeps routing, like TS
/// `handlePublicApprovalHttpRequest` returning `false`. An `Err` is a
/// zero-response connection close (a body read error, the transport's
/// equivalent of TS's aborted request destroying the response).
pub(crate) async fn serve(
    request: &mut hyper::Request<hyper::body::Incoming>,
    config: &Config,
) -> Option<Result<hyper::Response<Body>, io::Error>> {
    let version = request.version();
    let path = whatwg_path(
        request
            .uri()
            .path_and_query()
            .map_or("", |value| value.as_str()),
    );
    if path != "/api/module-host/approvals/status" && path != "/api/module-host/approvals/resume" {
        return None;
    }
    let keep_alive = version == hyper::Version::HTTP_11 && !request_wants_close(request);
    // TS checks the bearer token before the method, and an empty configured
    // token is always the 401: `!options.token ||
    // req.headers.authorization !== `Bearer ${options.token}``. Node keeps
    // only the first value of a repeated `authorization` header (its
    // single-value header list), so a duplicated bearer compares its first
    // line alone — `first_header`, not the `", "` join.
    let authorized = !crate::mutants::on("approvals-auth-off")
        && !config.token.is_empty()
        && first_header(request.headers(), "authorization") == format!("Bearer {}", config.token);
    if !authorized {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::UNAUTHORIZED,
                "{\"code\":\"PUBLIC_HOST_TRANSPORT_UNAUTHORIZED\"}",
                keep_alive,
            ),
            version,
        )));
    }
    if !crate::mutants::on("approvals-method-off") && request.method() != hyper::Method::POST {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::METHOD_NOT_ALLOWED,
                "{\"code\":\"PUBLIC_HOST_METHOD_NOT_ALLOWED\"}",
                keep_alive,
            ),
            version,
        )));
    }
    // The absent bridge is the throw the catch answers with its own 503 —
    // and in TS it happens before the body is read.
    let Some(bridge) = config.bridge.clone() else {
        if crate::mutants::on("approvals-bridge-unavailable-off") {
            // The mutant answers as if a bridge were installed, which
            // diverges on the unconfigured 503 cell.
            return Some(Ok(finalize(
                send_json(hyper::StatusCode::OK, "{}", keep_alive),
                version,
            )));
        }
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                "{\"code\":\"APPROVAL_BINDING_UNAVAILABLE\"}",
                keep_alive,
            ),
            version,
        )));
    };
    let input = match read_body(request.body_mut()).await {
        Ok(bytes) => bytes,
        // Over-budget never reaches the wire in TS: the throw out of the
        // `for await` triggers the async iterator's return(), which destroys
        // the request stream; the 'aborted' event fires before the catch's
        // `send` (nextTick beats the promise microtask), and the catch then
        // sees `signal.aborted` and destroys the response — a zero-byte
        // connection close, measured deterministic 5/5 against baseline
        // cc7c4e6 (`_tmp/20261105-issue-1350/ts-probe/probe.mjs`, golden cell
        // `budget-over`: empty response, closed socket). The transport's own
        // 413 `APPROVAL_INPUT_TOO_LARGE` frame is dead code on this wire, so
        // the Rust branch matches the observable behavior instead.
        Err(ReadError::OverBudget) => {
            return Some(Err(io::Error::other(
                "module-host approvals request body exceeded the 64 KiB budget",
            )));
        }
        // A body that errors before its declared end is a premature close;
        // TS's aborted request destroys the response with zero bytes (the
        // A8.4-MODULE-HOST-001 residual).
        Err(ReadError::PrematureClose) => {
            return Some(Err(io::Error::other(
                "module-host approvals request body ended prematurely",
            )));
        }
    };
    let Ok(parsed) = pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&input)) else {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"APPROVAL_INPUT_INVALID\"}",
                keep_alive,
            ),
            version,
        )));
    };
    // `!body || typeof body !== 'object' || Array.isArray(body)`.
    let JsValue::Object(fields) = &parsed else {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"APPROVAL_INPUT_INVALID\"}",
                keep_alive,
            ),
            version,
        )));
    };
    let operation = if path == "/api/module-host/approvals/status" {
        Operation::Status
    } else {
        Operation::Resume
    };
    let allowed: Vec<&str> = match operation {
        Operation::Status => STATUS_FIELDS.to_vec(),
        Operation::Resume => STATUS_FIELDS
            .iter()
            .copied()
            .chain(RESUME_FIELDS.iter().copied())
            .collect(),
    };
    if !crate::mutants::on("approvals-whitelist-off")
        && fields
            .own_entries()
            .iter()
            .any(|(name, _)| !allowed.iter().any(|allowed| name.equals_str(allowed)))
    {
        return Some(Ok(finalize(
            send_json(
                hyper::StatusCode::BAD_REQUEST,
                "{\"code\":\"APPROVAL_AUTHORITY_OVERRIDE\"}",
                keep_alive,
            ),
            version,
        )));
    }
    // The transport-level scope gate: with a static binding the body's
    // tenantId/agentId must be present and equal (TS compares against
    // `undefined`, so a missing field is the mismatch); a dynamic binding
    // defers to the bridge's re-resolved scope.
    if !crate::mutants::on("approvals-scope-off") && !bridge.dynamic_binding() {
        let binding = bridge.binding();
        for (field, binding_value) in [
            ("tenantId", &binding.tenant_id),
            ("agentId", &binding.agent_id),
        ] {
            let matches =
                matches!(fields.get(field), Some(JsValue::String(text)) if text == binding_value);
            if !matches {
                return Some(Ok(finalize(
                    send_json(
                        hyper::StatusCode::FORBIDDEN,
                        "{\"code\":\"APPROVAL_SUBJECT_MISMATCH\"}",
                        keep_alive,
                    ),
                    version,
                )));
            }
        }
    }
    // `const input = { ...body, approverAuthorization:
    // req.headers['x-staffdeck-approver-authorization'] }`: the header value
    // replaces nothing (the allowlist kept it out of the body) and lands
    // after the body's own keys, `undefined` included when the header is
    // absent — the bridge's format gate reads it either way.
    let mut merged = parsed.clone();
    if let JsValue::Object(object) = &mut merged {
        let approver = joined_header(request.headers(), "x-staffdeck-approver-authorization");
        if !crate::mutants::on("approvals-approver-inject-off") && !approver.is_empty() {
            object.insert(
                "approverAuthorization".into(),
                JsValue::String(JsString::from(approver)),
            );
        }
    }
    let result = match operation {
        Operation::Status => bridge.status(&merged).await,
        Operation::Resume => bridge.resume(&merged).await,
    };
    match result {
        Ok(value) => {
            // `send(200, result)`: status wraps the bridge result in
            // `{ status: await options.bridge.status(…) }`; resume passes it
            // through. `JSON.stringify` of `undefined` writes nothing, so a
            // resume result of `undefined` carries an empty body.
            let body = match operation {
                Operation::Status => {
                    let mut wrapped = JsObject::new();
                    wrapped.insert("status".into(), value);
                    stringify_json(&JsValue::Object(wrapped))
                }
                Operation::Resume => stringify_json(&value),
            };
            let body = body
                .expect("#1350 a bridge result is serializable")
                .unwrap_or_default();
            Some(Ok(finalize(
                send_json(hyper::StatusCode::OK, &body, keep_alive),
                version,
            )))
        }
        Err(approval_failure) => {
            // `cause.status ?? (cause.code === 'SOP_RESUME_REQUEST_CONFLICT'
            // ? 409 : 502)` and `cause.code ?? 'PUBLIC_APPROVAL_BRIDGE_FAILED'`.
            let code = match approval_failure.code {
                Some(code) => code,
                None => {
                    if crate::mutants::on("approvals-failure-default-off") {
                        JsValue::Undefined
                    } else {
                        JsValue::String(JsString::from("PUBLIC_APPROVAL_BRIDGE_FAILED"))
                    }
                }
            };
            let status = match approval_failure.status {
                Some(status) => status,
                None => {
                    if !crate::mutants::on("approvals-conflict-409-off")
                        && matches!(&code, JsValue::String(text)
                            if text.equals_str("SOP_RESUME_REQUEST_CONFLICT"))
                    {
                        409
                    } else {
                        502
                    }
                }
            };
            let body = failure_body(&code);
            Some(Ok(finalize(
                send_json(
                    hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_GATEWAY),
                    &body,
                    keep_alive,
                ),
                version,
            )))
        }
    }
}

enum Operation {
    Status,
    Resume,
}

/// The catch branch's `send(status, { code })` body: `JSON.stringify` of the
/// already-defaulted code. Production TS always replaces a missing `code`
/// with the `?? 'PUBLIC_APPROVAL_BRIDGE_FAILED'` default, so the `undefined`
/// case only exists behind the `approvals-failure-default-off` switch — and
/// there it must still be a valid JSON frame (`JSON.stringify(undefined)`
/// writes nothing, which would produce the illegal `{"code":}`), so it
/// serializes as `null`.
fn failure_body(code: &JsValue) -> String {
    let serialized = stringify_json(code).expect("#1350 a failure code is serializable");
    format!("{{\"code\":{}}}", serialized.as_deref().unwrap_or("null"))
}

enum ReadError {
    OverBudget,
    PrematureClose,
}

/// Read the request body under the TS byte budget, a per-chunk sum: strictly
/// more than 64 KiB total is over-budget, exactly 64 KiB is served. The
/// chunk split is stack-specific but the cumulative verdict is not, which is
/// why the replay can normalize it away.
async fn read_body(body: &mut hyper::body::Incoming) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Err(ReadError::PrematureClose);
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        let over = if crate::mutants::on("approvals-input-budget-boundary") {
            bytes.len() + data.len() >= INPUT_BUDGET
        } else {
            bytes.len() + data.len() > INPUT_BUDGET
        };
        if !crate::mutants::on("approvals-input-budget-off") && over {
            return Err(ReadError::OverBudget);
        }
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

/// Node answers every HTTP/1.0 request with a `Connection: close` response
/// on a closed socket, even one asking to keep alive; the explicit 1.0
/// response version keeps hyper's `fix_keep_alive` in its HTTP/1.0 branch —
/// the #1209 trick (whose status line against Node's 1.1 is the registered
/// A8.4-NATIVE-HTTP-001 diff).
/// Sibling: keep in sync with `finalize` in `src/module_host.rs`.
fn finalize(mut response: hyper::Response<Body>, version: hyper::Version) -> hyper::Response<Body> {
    if version == hyper::Version::HTTP_10 {
        *response.version_mut() = hyper::Version::HTTP_10;
    }
    response
}

/// A TS `writeHead` + `end` JSON response: unknown-length body (chunked,
/// because `writeHead` commits the headers before `end` knows the size), the
/// literal content-type, Node's Date, and the keep-alive pair, in Node's
/// header order. Sibling: keep in sync with `send_json` in
/// `src/module_host.rs`.
fn send_json(status: hyper::StatusCode, body: &str, keep_alive: bool) -> hyper::Response<Body> {
    let mut builder = node_reason(hyper::Response::builder().status(status), status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    builder = connection_headers(builder, keep_alive);
    builder
        .body(streamed(Some(Bytes::from(body.to_owned()))))
        .expect("#1350 valid approvals response")
}

/// Node writes two reason phrases this transport can answer differently from
/// hyper's table: `418 I'm a Teapot` (its capital `T`) and `unknown` for a
/// status without a standard reason (measured `writeHead(499)`:
/// `HTTP/1.1 499 unknown`) — a scripted bridge can throw either. Sibling:
/// keep in sync with `node_reason` in `src/module_host.rs`.
fn node_reason(
    mut builder: hyper::http::response::Builder,
    status: hyper::StatusCode,
) -> hyper::http::response::Builder {
    let phrase = if status == hyper::StatusCode::IM_A_TEAPOT {
        Some(hyper::ext::ReasonPhrase::from_static(b"I'm a Teapot"))
    } else if status.canonical_reason().is_none() {
        Some(hyper::ext::ReasonPhrase::from_static(b"unknown"))
    } else {
        None
    };
    if let Some(phrase) = phrase {
        builder = builder.extension(phrase);
    }
    builder
}

/// Sibling: keep in sync with `connection_headers` in `src/module_host.rs`.
fn connection_headers(
    builder: hyper::http::response::Builder,
    keep_alive: bool,
) -> hyper::http::response::Builder {
    if keep_alive {
        builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5")
    } else {
        builder.header(hyper::header::CONNECTION, "close")
    }
}

/// The `authorization` request header the way Node exposes it to TS: Node
/// discards every value but the first of a repeated single-value header
/// (`authorization` among them), unlike the `", "` join of
/// [`joined_header`] — measured against Node v22.22.0 with a two-line
/// raw-socket probe: `req.headers.authorization` is the first value alone
/// while `x-staffdeck-approver-authorization` joins (oracle cells `auth-dup`
/// and `stub-echo-approver` pin both sides). The lossy byte fold for
/// non-UTF-8 values keeps an exact compare on the rejecting side.
fn first_header(headers: &hyper::HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// A request header the way Node exposes it to TS: lowercased name and, when
/// the client repeats the header, the values joined with `", "` (every
/// header except Node's single-value list, which `authorization` belongs to
/// — see [`first_header`]). The joined text only ever feeds an exact string
/// compare or the bearer prefix check, so a lossy byte fold for non-UTF-8
/// values stays on the rejecting side.
fn joined_header(headers: &hyper::HeaderMap, name: &str) -> String {
    headers
        .get_all(name)
        .iter()
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The WHATWG path of the request target, `new URL(request.url,
/// "http://localhost").pathname` in TS. Sibling: keep in sync with
/// `whatwg_path` in `src/module_host.rs`.
fn whatwg_path(target: &str) -> String {
    static BASE: std::sync::OnceLock<url::Url> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| url::Url::parse("http://127.0.0.1").expect("valid base URL"));
    url::Url::options()
        .base_url(Some(base))
        .parse(target)
        .map_or_else(|_| "/".to_owned(), |parsed| parsed.path().to_owned())
}

/// Node's `Connection: close` request detection. Sibling: keep in sync with
/// `request_wants_close` in `src/module_host.rs`.
fn request_wants_close(request: &hyper::Request<hyper::body::Incoming>) -> bool {
    request
        .headers()
        .get_all(hyper::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// Unknown-length single-frame body: the chunked framing a `writeHead` +
/// `end` response gets. Sibling: keep in sync with `StreamedBody` in
/// `src/module_host.rs`.
struct StreamedBody(Option<Bytes>);
impl hyper::body::Body for StreamedBody {
    type Data = Bytes;
    type Error = crate::ProxyError;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, crate::ProxyError>>> {
        std::task::Poll::Ready(
            self.0
                .take()
                .map(|bytes| Ok(hyper::body::Frame::data(bytes))),
        )
    }
}

fn streamed(body: Option<Bytes>) -> Body {
    StreamedBody(body).boxed()
}

#[cfg(test)]
mod tests;
