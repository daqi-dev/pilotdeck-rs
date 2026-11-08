//! The portable StaffDeck evaluator owns SOP transitions. This client validates
//! its manifest and envelopes; it never implements a second workflow engine.
use crate::module_host::approvals::native_http::{
    JsonFailure, JsonRequest, JsonResponse, JsonTransport,
};
use crate::runtime::network::Cancellation;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{sync::Arc, time::Duration};

#[derive(Debug)]
pub(crate) struct ClientError {
    pub code: JsString,
    pub message: JsString,
    pub details: Option<JsValue>,
    pub retryability: Option<JsString>,
}
impl ClientError {
    fn new(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            retryability: None,
        }
    }
    fn retry(mut self, value: &str) -> Self {
        self.retryability = Some(value.into());
        self
    }
}
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    v.get(key).unwrap_or(&UNDEFINED)
}
fn is(v: &JsValue, key: &str, text: &str) -> bool {
    field(v, key)
        .as_string()
        .is_some_and(|s| s.equals_str(text))
}
fn strings(v: &JsValue) -> bool {
    matches!(v,JsValue::Array(items) if items.iter().all(|v|v.as_string().is_some()))
}
fn optional(v: &JsValue, key: &str, test: impl FnOnce(&JsValue) -> bool) -> bool {
    let value = field(v, key);
    value.is_undefined() || test(value)
}
fn protocol(message: &str) -> ClientError {
    ClientError::new("SOP_RUNTIME_PROTOCOL", message)
}

pub(crate) struct SopClient {
    endpoint: String,
    manifest_path: String,
    expected: Option<JsValue>,
    timeout: Duration,
    http: Arc<dyn JsonTransport>,
    manifest: tokio::sync::Mutex<Option<JsValue>>,
}
impl SopClient {
    pub(crate) fn new(
        endpoint: String,
        manifest_path: Option<String>,
        expected: Option<JsValue>,
        timeout_ms: Option<f64>,
        http: Arc<dyn JsonTransport>,
    ) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            manifest_path: manifest_path.unwrap_or_else(|| "/healthz".into()),
            expected,
            timeout: timeout_ms
                .map(crate::runtime::network::node_timer_duration)
                .unwrap_or(Duration::from_secs(30)),
            http,
            manifest: tokio::sync::Mutex::new(None),
        }
    }
    async fn request(
        &self,
        path: &str,
        body: Option<JsValue>,
        signal: &Cancellation,
    ) -> Result<JsonResponse, ClientError> {
        let raw = body
            .as_ref()
            .map(stringify_json)
            .transpose()
            .map_err(|_| protocol("StaffDeck SOP runtime request is not serializable."))?
            .flatten();
        let url = url::Url::parse(&format!("{}{path}", self.endpoint)).map_err(|_| {
            ClientError::new(
                "SOP_RUNTIME_UNAVAILABLE",
                "StaffDeck SOP runtime request failed: fetch failed",
            )
            .retry("safe")
        })?;
        let request = JsonRequest {
            method: if raw.is_some() { "POST" } else { "GET" },
            url,
            bearer: None,
            body: raw,
            parse_error_body: true,
            redirect_error: false,
            timeout: self.timeout,
        };
        tokio::select! {
            biased;
            _=signal.cancelled()=>Err(ClientError::new("SOP_RUNTIME_CANCELLED","StaffDeck SOP runtime request was cancelled: This operation was aborted").retry("unsafe")),
            response=self.http.request(request)=>response.map_err(|error| {
                match error {
                    JsonFailure::Transport(error)=>{
                        let error=if error=="StaffDeck request timed out." {"StaffDeck SOP runtime request timed out.".to_owned()} else {error};
                        ClientError::new("SOP_RUNTIME_UNAVAILABLE",format!("StaffDeck SOP runtime request failed: {error}")).retry("safe")
                    }
                    JsonFailure::Body(error)=>protocol(&error).retry("unsafe"),
                }
            }),
        }
    }
    async fn ensure_manifest(&self, signal: &Cancellation) -> Result<(), ClientError> {
        let mut cached = tokio::select! {
            biased;
            _=signal.cancelled()=>return Err(ClientError::new("SOP_RUNTIME_CANCELLED","StaffDeck SOP runtime request was cancelled: This operation was aborted").retry("unsafe")),
            cached=self.manifest.lock()=>cached,
        };
        if cached.is_some() {
            return Ok(());
        }
        let response = self.request(&self.manifest_path, None, signal).await?;
        if !(200..300).contains(&response.status) {
            return Err(ClientError::new(
                "SOP_RUNTIME_UNAVAILABLE",
                format!(
                    "StaffDeck SOP runtime health check returned HTTP {}.",
                    response.status
                ),
            ));
        }
        validate_manifest(&response.body, self.expected.as_ref())?;
        *cached = Some(response.body);
        Ok(())
    }
    async fn post(
        &self,
        operation: &str,
        payload: JsValue,
        context: Option<&JsValue>,
        signal: &Cancellation,
    ) -> Result<JsValue, ClientError> {
        self.ensure_manifest(signal).await?;
        let request = envelope(operation, payload, context);
        let response = self
            .request(
                &format!("/v1/sop/{operation}"),
                Some(request.clone()),
                signal,
            )
            .await?;
        validate_envelope(&response.body, field(&request, "requestId"))?;
        if response.body.get("ok") == Some(&JsValue::Bool(false)) {
            let error = field(&response.body, "error");
            return Err(ClientError {
                code: field(error, "code")
                    .as_string()
                    .expect("validated error code")
                    .clone(),
                message: field(error, "message")
                    .as_string()
                    .expect("validated error message")
                    .clone(),
                details: error.get("details").filter(|v| !v.is_undefined()).cloned(),
                retryability: field(error, "retryability").as_string().cloned(),
            });
        }
        if !(200..300).contains(&response.status) {
            return Err(protocol(&format!(
                "StaffDeck SOP runtime returned HTTP {} with a successful envelope.",
                response.status
            )));
        }
        Ok(field(&response.body, "payload").clone())
    }
    pub(crate) async fn prepare(
        &self,
        bundle: JsValue,
        state: JsValue,
        context: Option<&JsValue>,
        signal: &Cancellation,
    ) -> Result<JsValue, ClientError> {
        let payload = self
            .post(
                "prepare",
                crate::metadata::object([("bundle", bundle), ("state", state)]),
                context,
                signal,
            )
            .await?;
        if payload.as_object().is_none()
            || field(&payload, "state").as_object().is_none()
            || !valid_step(field(&payload, "step"))
        {
            return Err(
                protocol("StaffDeck SOP runtime returned an invalid prepare payload.")
                    .retry("unsafe"),
            );
        }
        Ok(payload)
    }
    pub(crate) async fn submit(
        &self,
        bundle: JsValue,
        state: JsValue,
        proposal: JsValue,
        successful: Vec<JsString>,
        context: Option<&JsValue>,
        signal: &Cancellation,
    ) -> Result<JsValue, ClientError> {
        let payload = self
            .post(
                "submit",
                crate::metadata::object([
                    ("bundle", bundle),
                    ("state", state),
                    ("proposal", proposal),
                    (
                        "successfulToolNames",
                        JsValue::Array(successful.into_iter().map(JsValue::String).collect()),
                    ),
                ]),
                context,
                signal,
            )
            .await?;
        if payload.as_object().is_none()
            || field(&payload, "state").as_object().is_none()
            || !valid_submit(field(&payload, "result"))
        {
            return Err(
                protocol("StaffDeck SOP runtime returned an invalid submit payload.")
                    .retry("unsafe"),
            );
        }
        Ok(payload)
    }
}
fn envelope(operation: &str, payload: JsValue, context: Option<&JsValue>) -> JsValue {
    let fallback = crate::metadata::object([
        ("runId", format!("sop:{operation}").as_str().into()),
        ("operationId", format!("sop.{operation}").as_str().into()),
        ("requestId", format!("sop.{operation}").as_str().into()),
        ("sessionId", "unknown".into()),
        ("turnId", "unknown".into()),
    ]);
    let scope = context
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or(&fallback);
    let mut request = JsObject::new();
    request.insert("protocolVersion".into(), "2.0".into());
    for key in ["runId", "operationId", "requestId", "sessionId", "turnId"] {
        request.insert(key.into(), field(scope, key).clone());
    }
    for key in ["idempotencyKey", "deadlineAt"] {
        if scope
            .get(key)
            .is_some_and(crate::runtime::turn::submit::truthy)
        {
            request.insert(key.into(), field(scope, key).clone());
        }
    }
    if !field(scope, "expectedRevision").is_undefined() {
        request.insert(
            "expectedRevision".into(),
            field(scope, "expectedRevision").clone(),
        );
    }
    request.insert("payload".into(), payload);
    JsValue::Object(request)
}
fn validate_manifest(value: &JsValue, expected: Option<&JsValue>) -> Result<(), ClientError> {
    let operations = match field(value, "operations") {
        JsValue::Array(items) => Some(items),
        _ => None,
    };
    if value.as_object().is_none()
        || !is(value, "status", "ok")
        || !is(value, "protocolVersion", "2.0")
        || !is(value, "moduleId", "sop.runtime")
        || !is(value, "contract", "sop.lifecycle/v2")
        || !operations.is_some_and(|items| {
            ["prepare", "submit"]
                .iter()
                .all(|operation| items.contains(&JsValue::from(*operation)))
        })
    {
        return Err(ClientError::new(
            "SOP_PROTOCOL_INCOMPATIBLE",
            "StaffDeck SOP runtime does not advertise sop.runtime sop.lifecycle/v2 protocol 2.0 support.",
        ));
    }
    if let Some(expected) = expected
        && (!is(value, "descriptorVersion", "1.0")
            || ["implementationId", "contract", "transport"]
                .iter()
                .any(|key| field(value, key) != field(expected, key)))
    {
        return Err(ClientError::new(
            "SOP_PROTOCOL_INCOMPATIBLE",
            format!(
                "SOP runtime does not advertise the configured {} {}/{} binding.",
                display(field(expected, "implementationId")),
                display(field(expected, "contract")),
                display(field(expected, "transport"))
            ),
        ));
    }
    Ok(())
}
fn display(value: &JsValue) -> String {
    String::from_utf16_lossy(
        &crate::js_value::to_string_units_checked(value)
            .unwrap_or_else(|_| "undefined".encode_utf16().collect()),
    )
}
fn valid_runtime_error(value: &JsValue) -> bool {
    value.as_object().is_some()
        && field(value, "code").as_string().is_some()
        && field(value, "message").as_string().is_some()
        && ["safe", "unsafe", "retry_after_status"]
            .iter()
            .any(|s| is(value, "retryability", s))
        && optional(value, "details", |v| v.as_object().is_some())
}
fn validate_envelope(value: &JsValue, request: &JsValue) -> Result<(), ClientError> {
    if value.as_object().is_none()
        || !is(value, "protocolVersion", "2.0")
        || field(value, "requestId") != request
        || field(value, "ok").as_bool().is_none()
        || !(["completed", "failed"]
            .iter()
            .any(|s| is(value, "outcome", s)))
    {
        return Err(protocol(
            "StaffDeck SOP runtime returned an invalid protocol envelope.",
        ));
    }
    if field(value, "ok") == &JsValue::Bool(true) {
        if !is(value, "outcome", "completed") || field(value, "payload").as_object().is_none() {
            return Err(protocol(
                "StaffDeck SOP runtime returned an invalid success envelope.",
            ));
        }
    } else if !is(value, "outcome", "failed") || !valid_runtime_error(field(value, "error")) {
        return Err(protocol(
            "StaffDeck SOP runtime returned an invalid error envelope.",
        ));
    }
    Ok(())
}
fn valid_transition(value: &JsValue) -> bool {
    value.as_object().is_some()
        && field(value, "nextStepId").as_string().is_some()
        && field(value, "condition").as_string().is_some()
        && field(value, "priority")
            .as_number()
            .is_some_and(f64::is_finite)
        && optional(value, "label", |v| v.as_string().is_some())
        && field(value, "targetStep").as_object().is_some()
}
pub(crate) fn valid_step(value: &JsValue) -> bool {
    value.as_object().is_some()
        && ["skillId", "skillName", "version", "nodeId", "instruction"]
            .iter()
            .all(|key| field(value, key).as_string().is_some())
        && field(value, "node").as_object().is_some()
        && field(value, "knownSlots").as_object().is_some()
        && [
            "expectedUserInfo",
            "allowedNextStepIds",
            "requiredToolNames",
            "allowedActions",
        ]
        .iter()
        .all(|key| strings(field(value, key)))
        && ["isTerminal", "declaresHandoff"]
            .iter()
            .all(|key| field(value, key).as_bool().is_some())
        && optional(
            value,
            "transitions",
            |v| matches!(v,JsValue::Array(items) if items.iter().all(valid_transition)),
        )
        && optional(value, "contextMode", |v| {
            v.as_string()
                .is_some_and(|s| s.equals_str("new_session") || s.equals_str("inherit"))
        })
        && optional(value, "subSopId", |v| {
            matches!(v, JsValue::Null | JsValue::String(_))
        })
}
pub(crate) fn valid_submit(value: &JsValue) -> bool {
    value.as_object().is_some()
        && field(value, "status").as_string().is_some()
        && field(value, "replyFragment").as_string().is_some()
        && field(value, "slotUpdates").as_object().is_some()
        && ["taskSummary", "nextStepId"].iter().all(|key| {
            optional(value, key, |v| {
                matches!(v, JsValue::Null | JsValue::String(_))
            })
        })
        && matches!(field(value,"events"),JsValue::Array(items) if items.iter().all(|v|v.as_object().is_some()))
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
