//! External skills management port. Ported from PilotDeck cc7c4e6
//! `src/composition/domainPorts.ts:75-105` (`createSkillManagementPort`),
//! `src/composition/HttpModuleClient.ts:23-160` (manifest + module_call) and
//! `src/agent/modules/protocol.ts:264-358` (response validation).
//!
//! The port is deliberately not registered on the gateway: it is the external
//! branch of `createLocalGateway.ts:851-870`, ready for the A8 wiring package.
//! The native and disabled branches keep their previous behavior; only the
//! external branch that used to answer `SKILL_MODULE_HOST_REQUIRED` moves to
//! this real management port.
//!
//! `ExternalSkillsPort` is generic over [`ModuleHttpTransport`] so the golden
//! tests can replay the exact requests/responses recorded from the real TS
//! port; [`HyperModuleTransport`] is the production transport and reuses the
//! model HTTP stack (rustls, proxy env, redirects and body decoding).
use super::{Result, SkillError, SkillMethod};
use crate::js_value::number_string;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

/// `protocol.ts:3 MODULE_PROTOCOL_VERSION`.
pub const MODULE_PROTOCOL_VERSION: &str = "2.0";
/// `composition/types.ts:3 MODULE_HTTP_TRANSPORT`.
pub const MODULE_HTTP_TRANSPORT: &str = "module-http-v2";
/// `HttpModuleClient.ts:121 timeoutMs ?? 10_000`.
const DEFAULT_TIMEOUT_MS: f64 = 10_000.0;
const DEFAULT_MANIFEST_PATH: &str = "/module-manifest";
const DEFAULT_CALL_PATH: &str = "/v2/module/call";

/// The external binding fields the port reads. The registry snapshot projection
/// (`config/snapshot.rs::external_module`) already normalizes these fields and
/// config diagnostics reject malformed bindings, so this parse only re-reads
/// the resolved snapshot object.
#[derive(Clone, Debug)]
pub struct ExternalSkillBinding {
    pub implementation_id: String,
    pub contract: String,
    pub transport: String,
    pub endpoint: String,
    pub manifest_path: String,
    pub call_path: String,
    pub methods: Vec<String>,
    pub timeout_ms: f64,
    pub credential_env: Option<String>,
}

impl ExternalSkillBinding {
    /// `snapshot.config.modules?.skills` already resolved by the registry.
    /// `external_module` always writes implementationId/contract/transport/
    /// endpoint/manifestPath/callPath/methods, so the defaults below match the
    /// TS `??` defaults and never hide a valid snapshot.
    pub fn from_value(value: &JsValue) -> Result<Self> {
        let object = value.as_object().ok_or_else(|| {
            SkillError::type_error("", "External skill binding must be an object.")
        })?;
        let text = |key: &str| {
            object
                .get(key)
                .and_then(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()))
        };
        let required = |key: &str| {
            text(key).ok_or_else(|| {
                SkillError::protocol_failure(format!("External skill binding is missing {key}."))
            })
        };
        let methods = match object.get("methods") {
            Some(JsValue::Array(items)) => items
                .iter()
                .filter_map(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            implementation_id: required("implementationId")?,
            contract: required("contract")?,
            transport: required("transport")?,
            endpoint: required("endpoint")?,
            manifest_path: text("manifestPath").unwrap_or_else(|| DEFAULT_MANIFEST_PATH.into()),
            call_path: text("callPath").unwrap_or_else(|| DEFAULT_CALL_PATH.into()),
            methods,
            timeout_ms: object
                .get("timeoutMs")
                .and_then(JsValue::as_number)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(DEFAULT_TIMEOUT_MS),
            credential_env: text("credentialEnv"),
        })
    }
}

/// One outbound module HTTP exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleHttpRequest {
    pub method: &'static str,
    pub url: String,
    pub content_type: Option<&'static str>,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleHttpResponse {
    pub status: u16,
    /// Raw response bytes; `response.json()` parses them like TS does.
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleTransportError {
    /// `requestJson`'s AbortController timer fired.
    Timeout,
    /// Any other fetch/body failure (DNS, refused, TLS, truncated body).
    Unavailable,
}

pub type ModuleHttpFuture<'a> = Pin<
    Box<
        dyn Future<Output = std::result::Result<ModuleHttpResponse, ModuleTransportError>>
            + Send
            + 'a,
    >,
>;

/// The one HTTP exchange seam. Tests replay recorded TS exchanges; production
/// uses [`HyperModuleTransport`].
pub trait ModuleHttpTransport: Send + Sync {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a>;
}

/// `process.env` seam for `credentialEnv` (settable in tests without touching
/// the process environment).
pub trait ModuleEnvironment: Send + Sync {
    fn var(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnvironment;
impl ModuleEnvironment for ProcessEnvironment {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Production transport. `HyperModelTransport` is the existing rustls/proxy/
/// redirect/decoding stack; module hosts use the same Node fetch semantics as
/// providers, so reusing it avoids a second TLS/proxy implementation.
#[derive(Clone)]
pub struct HyperModuleTransport {
    inner: crate::runtime::model::http::HyperModelTransport,
}

impl HyperModuleTransport {
    pub fn new() -> Self {
        Self {
            inner: crate::runtime::model::http::HyperModelTransport::new(),
        }
    }
}

impl Default for HyperModuleTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleHttpTransport for HyperModuleTransport {
    fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
        Box::pin(async move {
            use crate::runtime::model::http::ModelHttpTransport as _;
            use bytes::Bytes;
            use http_body_util::BodyExt;
            use hyper::{HeaderMap, Method, header};
            use url::Url;

            let url = Url::parse(&request.url).map_err(|_| ModuleTransportError::Unavailable)?;
            let method = Method::from_bytes(request.method.as_bytes())
                .map_err(|_| ModuleTransportError::Unavailable)?;
            let mut headers = HeaderMap::new();
            if request.content_type.is_some() {
                headers.insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
            }
            if let Some(authorization) = &request.authorization {
                headers.insert(
                    header::AUTHORIZATION,
                    header::HeaderValue::from_str(authorization)
                        .map_err(|_| ModuleTransportError::Unavailable)?,
                );
            }
            let inner = &self.inner;
            // `follow` owns undici's redirect (and blocked-port) policy; the
            // `FollowRedirects` extension it installs also turns on decoding.
            let exchange = crate::fetch_redirect::follow(
                url,
                method,
                headers,
                Bytes::from(request.body),
                || crate::runtime::model::http::TransportFailure::fetch_failed(None),
                |request| inner.send(request),
            );
            let response = tokio::time::timeout(request.timeout, exchange)
                .await
                .map_err(|_| ModuleTransportError::Timeout)?
                .map_err(|_| ModuleTransportError::Unavailable)?;
            let status = response.status().as_u16();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|_| ModuleTransportError::Unavailable)?
                .to_bytes();
            Ok(ModuleHttpResponse {
                status,
                body: body.to_vec(),
            })
        })
    }
}

/// `domainPorts.ts:75-105`. One instance mirrors one `createSkillManagementPort`
/// call: the manifest cache, `requestId` counter and HTTP `messageId` counter
/// live on the port, exactly as they do on the TS closure.
pub struct ExternalSkillsPort<T> {
    binding: ExternalSkillBinding,
    transport: Arc<T>,
    env: Arc<dyn ModuleEnvironment>,
    request_sequence: AtomicU64,
    message_sequence: AtomicU64,
    manifest_loaded: Mutex<bool>,
}

impl<T: ModuleHttpTransport + 'static> ExternalSkillsPort<T> {
    pub fn new(binding: ExternalSkillBinding, transport: Arc<T>) -> Self {
        Self::with_environment(binding, transport, Arc::new(ProcessEnvironment))
    }

    pub fn with_environment(
        binding: ExternalSkillBinding,
        transport: Arc<T>,
        env: Arc<dyn ModuleEnvironment>,
    ) -> Self {
        Self {
            binding,
            transport,
            env,
            request_sequence: AtomicU64::new(0),
            message_sequence: AtomicU64::new(0),
            manifest_loaded: Mutex::new(false),
        }
    }

    /// One `SkillManagementPort` method. The nine-step order is the TS order:
    /// declared-method check, manifest, credential, request, envelope, failure
    /// projection, payload.result, record check.
    pub async fn call(&self, method: SkillMethod, input: &JsValue) -> Result<JsValue> {
        let operation = method.operation();
        if !self
            .binding
            .methods
            .iter()
            .any(|declared| declared == operation)
        {
            return Err(SkillError::protocol_failure(format!(
                "Skill operation '{operation}' is not declared."
            )));
        }
        let request_id = format!(
            "skills-management-{operation}-{}",
            self.next(&self.request_sequence)
        );
        let response = self.client_call(operation, &request_id, input).await?;
        let object = response.as_object().expect("validated module response");
        if object.get("ok").and_then(JsValue::as_bool) != Some(true) {
            return Err(module_failure(object));
        }
        let result = object
            .get("payload")
            .and_then(JsValue::as_object)
            .and_then(|payload| payload.get("result"))
            .cloned()
            .unwrap_or(JsValue::Undefined);
        if result.as_object().is_none() {
            return Err(SkillError::protocol_failure(format!(
                "Skill {operation} result must be an object."
            )));
        }
        Ok(result)
    }

    fn next(&self, counter: &AtomicU64) -> u64 {
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn client_call(
        &self,
        operation: &str,
        request_id: &str,
        input: &JsValue,
    ) -> Result<JsValue> {
        // TS awaits `client.manifest()` first; a missing credential still
        // performs that GET before failing, after the messageId counter
        // (`HttpModuleClient.ts:43-58`).
        self.load_manifest_cached().await?;
        let message_id = format!("module-http-{}", self.next(&self.message_sequence));
        let authorization = self.authorization()?;
        let request = request_body(&message_id, request_id, operation, input)?;
        let body = stringify_json(&request).ok().flatten().ok_or_else(|| {
            SkillError::protocol_failure("Module call request is not serializable.")
        })?;
        let response = self
            .request_json(
                &self.binding.call_path,
                "POST",
                Some(body.into_bytes()),
                true,
                authorization,
            )
            .await?;
        validate_response(&response, &message_id, request_id)?;
        Ok(response)
    }

    fn authorization(&self) -> Result<Option<String>> {
        let Some(name) = &self.binding.credential_env else {
            return Ok(None);
        };
        let credential = self
            .env
            .var(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        match credential {
            Some(value) => Ok(Some(format!("Bearer {value}"))),
            None => Err(SkillError::protocol_failure(
                "Knowledge module account credential is not configured.",
            )),
        }
    }

    async fn load_manifest_cached(&self) -> Result<()> {
        // TS shares one in-flight manifest promise and clears it on failure.
        // Holding this async mutex for the load gives concurrent callers the
        // same single GET; failures leave the flag false, so the next call
        // retries.
        let mut loaded = self.manifest_loaded.lock().await;
        if *loaded {
            return Ok(());
        }
        self.load_manifest().await?;
        *loaded = true;
        Ok(())
    }

    async fn load_manifest(&self) -> Result<()> {
        let value = self
            .request_json(&self.binding.manifest_path, "GET", None, false, None)
            .await?;
        let object = value
            .as_object()
            .ok_or_else(|| SkillError::protocol_failure("Module manifest must be an object."))?;
        let string = |key: &str| object.get(key).and_then(JsValue::as_string);
        let methods_valid = matches!(
            object.get("methods"),
            Some(JsValue::Array(items)) if items.iter().all(|item| item.as_string().is_some())
        );
        let identity = string("protocolVersion")
            .is_some_and(|v| v.equals_str(MODULE_PROTOCOL_VERSION))
            && string("implementationId")
                .is_some_and(|v| v.equals_str(&self.binding.implementation_id))
            && string("contract").is_some_and(|v| v.equals_str(&self.binding.contract))
            && string("transport").is_some_and(|v| v.equals_str(&self.binding.transport))
            && methods_valid;
        if !identity {
            return Err(SkillError::protocol_failure(
                "Module manifest identity, protocol, contract, transport, or methods are incompatible.",
            ));
        }
        let methods: Vec<String> = match object.get("methods") {
            Some(JsValue::Array(items)) => items
                .iter()
                .filter_map(JsValue::as_string)
                .map(|value| String::from_utf16_lossy(value.units()))
                .collect(),
            _ => unreachable!("methods_valid checked"),
        };
        let provided: HashSet<&str> = methods.iter().map(String::as_str).collect();
        if let Some(missing) = self
            .binding
            .methods
            .iter()
            .find(|method| !provided.contains(method.as_str()))
        {
            return Err(SkillError::protocol_failure(format!(
                "Module manifest does not provide required method '{missing}'."
            )));
        }
        Ok(())
    }

    async fn request_json(
        &self,
        path: &str,
        method: &'static str,
        body: Option<Vec<u8>>,
        accept_structured_error: bool,
        authorization: Option<String>,
    ) -> Result<JsValue> {
        let base = if self.binding.endpoint.ends_with('/') {
            self.binding.endpoint.clone()
        } else {
            format!("{}/", self.binding.endpoint)
        };
        let url = url::Url::parse(&base)
            .and_then(|base| base.join(path))
            .map_err(|_| {
                SkillError::type_error(
                    "ERR_INVALID_URL",
                    format!("Invalid URL: {}", self.binding.endpoint),
                )
            })?;
        let request = ModuleHttpRequest {
            method,
            url: url.to_string(),
            content_type: (method == "POST").then_some("application/json"),
            authorization,
            body: body.unwrap_or_default(),
            timeout: Duration::try_from_secs_f64(self.binding.timeout_ms / 1000.0)
                .unwrap_or(Duration::MAX),
        };
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|error| match error {
                ModuleTransportError::Timeout => SkillError::transport_failure(
                    "MODULE_TIMEOUT",
                    format!(
                        "Module HTTP request exceeded {}ms.",
                        number_string(self.binding.timeout_ms)
                    ),
                ),
                ModuleTransportError::Unavailable => SkillError::transport_failure(
                    "MODULE_TRANSPORT_UNAVAILABLE",
                    "Module HTTP request could not be completed.".into(),
                ),
            })?;
        let parsed = parse_json(&String::from_utf8_lossy(&response.body)).ok();
        let accepted_failure = accept_structured_error
            && parsed.as_ref().is_some_and(|value| {
                value
                    .get("kind")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("response"))
                    && value.get("ok") == Some(&JsValue::Bool(false))
            });
        if !(200..=299).contains(&response.status) && !accepted_failure {
            return Err(SkillError::transport_failure(
                "MODULE_TRANSPORT_UNAVAILABLE",
                "Module HTTP request could not be completed.".into(),
            ));
        }
        parsed
            .ok_or_else(|| SkillError::protocol_failure("Module HTTP response must contain JSON."))
    }
}

fn request_body(
    message_id: &str,
    request_id: &str,
    operation: &str,
    input: &JsValue,
) -> Result<JsValue> {
    let payload = object([("operation", operation.into()), ("input", input.clone())]);
    Ok(JsValue::Object(object([
        ("kind", "request".into()),
        ("method", "module_call".into()),
        ("messageId", message_id.into()),
        ("runId", "skills-management".into()),
        ("operationId", "skills-management".into()),
        ("requestId", request_id.into()),
        ("module", "skills".into()),
        ("payload", JsValue::Object(payload)),
    ])))
}

/// `protocol.ts:343-357` for the response branch plus
/// `HttpModuleClient.ts:75-92` semantics/correlation.
fn validate_response(value: &JsValue, message_id: &str, request_id: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| SkillError::protocol_failure("Module message must be an object."))?;
    let kind = object.get("kind").and_then(JsValue::as_string);
    if !kind.is_some_and(|kind| {
        ["request", "response", "event", "error"]
            .iter()
            .any(|candidate| kind.equals_str(candidate))
    }) {
        return Err(SkillError::protocol_failure(
            "Module message kind is invalid.",
        ));
    }
    if !kind.is_some_and(|kind| kind.equals_str("response")) {
        return Err(SkillError::protocol_failure(
            "Module call did not return a response envelope.",
        ));
    }
    nonempty(object.get("messageId"))
        .ok_or_else(|| SkillError::protocol_failure("Message messageId is required."))?;
    let in_reply_to = nonempty(object.get("inReplyTo"))
        .ok_or_else(|| SkillError::protocol_failure("Response requires inReplyTo and ok."))?;
    if object.get("ok").and_then(JsValue::as_bool).is_none() {
        return Err(SkillError::protocol_failure(
            "Response requires inReplyTo and ok.",
        ));
    }
    if object.get("final") == Some(&JsValue::Bool(true)) && !outcome(object.get("outcome")) {
        return Err(SkillError::protocol_failure(
            "Final response requires outcome.",
        ));
    }
    if object
        .get("outcome")
        .is_some_and(|value| !value.is_undefined())
        && !outcome(object.get("outcome"))
    {
        return Err(SkillError::protocol_failure("Response outcome is invalid."));
    }
    let ok = object.get("ok").and_then(JsValue::as_bool).unwrap_or(false);
    if ok {
        if object
            .get("code")
            .is_some_and(|value| !value.is_undefined())
            || object
                .get("error")
                .is_some_and(|value| !value.is_undefined())
            || object
                .get("outcome")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("failed"))
        {
            return Err(SkillError::protocol_failure(
                "Successful module response contains failure fields.",
            ));
        }
    } else {
        let code = nonempty(object.get("code")).ok_or_else(|| {
            SkillError::protocol_failure("Failed module response requires code and error details.")
        })?;
        let error = object
            .get("error")
            .and_then(JsValue::as_object)
            .ok_or_else(|| {
                SkillError::protocol_failure(
                    "Failed module response requires code and error details.",
                )
            })?;
        let consistent = error
            .get("code")
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.equals_str(&code))
            && nonempty(error.get("message")).is_some()
            && error
                .get("retryability")
                .and_then(JsValue::as_string)
                .is_some_and(|value| {
                    ["safe", "unsafe", "retry_after_status"]
                        .iter()
                        .any(|candidate| value.equals_str(candidate))
                });
        if !consistent {
            return Err(SkillError::protocol_failure(
                "Failed module response error fields are inconsistent.",
            ));
        }
    }
    let correlated = in_reply_to == message_id
        && match object.get("requestId") {
            None => true,
            Some(value) if value.is_undefined() => true,
            Some(value) => value
                .as_string()
                .is_some_and(|value| value.equals_str(request_id)),
        };
    if !correlated {
        return Err(SkillError::protocol_failure(
            "Module response correlation does not match the request.",
        ));
    }
    Ok(())
}

fn module_failure(response: &JsObject) -> SkillError {
    // `validate_response` already proved ok:false has code + consistent error.
    let code = nonempty(response.get("code")).unwrap_or_default();
    let error = response
        .get("error")
        .and_then(JsValue::as_object)
        .expect("validated error object");
    let message = nonempty(error.get("message")).unwrap_or_else(|| code.clone());
    let retryability = error
        .get("retryability")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()));
    let details = error
        .get("details")
        .filter(|value| !value.is_undefined())
        .cloned();
    SkillError::provider_failure(code, message, retryability, details)
}

fn nonempty(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .filter(|value| !value.units().is_empty())
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn outcome(value: Option<&JsValue>) -> bool {
    value.and_then(JsValue::as_string).is_some_and(|value| {
        ["completed", "failed", "cancelled", "result_unknown"]
            .iter()
            .any(|candidate| value.equals_str(candidate))
    })
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(JsString::from(key), value);
    }
    object
}
