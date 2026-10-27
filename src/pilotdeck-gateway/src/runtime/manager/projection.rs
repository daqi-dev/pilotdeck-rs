//! Manager projection and gateway-method adapter.
//!
//! TS:
//! - `createLocalGateway.ts:1259-1268` (`managerSessions`);
//! - `createLocalGateway.ts:1270` (`managerBrowsers: options.managerBrowsers`);
//! - `InProcessGateway.ts:2828-2834` (provider-unavailable guards);
//! - `GatewayWsConnection.ts:309-314` (wire method names).
//!
//! The projection is deliberately absent from `dispatch.rs::LOCAL_METHODS`.
//! It owns no HTTP/WS entry and no session write path: sessions are read from a
//! `SessionCatalogPort`, browsers from a host-injected port.

use super::catalog::{GatewaySessionCatalogConsumer, SessionCatalogPort};
use crate::metadata::{self, Metadata, NativeFiles};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

static UNDEFINED: JsValue = JsValue::Undefined;

/// TS-shaped manager error: `DialogGatewayError` carries a code, raw JS errors
/// keep their name but no code. The wire adapter (`GatewayWsConnection.ts:249-
/// 253`) is modeled by `as_json`, matching the snapshot projection fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerError {
    name: String,
    code: Option<String>,
    message: JsString,
}

impl ManagerError {
    pub fn new(name: impl Into<String>, code: Option<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            code,
            message: JsString::from(message.into()),
        }
    }

    pub fn with_message(name: impl Into<String>, code: Option<String>, message: JsString) -> Self {
        Self {
            name: name.into(),
            code,
            message,
        }
    }

    pub fn gateway(code: &'static str, message: impl Into<String>) -> Self {
        Self::new("DialogGatewayError", Some(code.to_owned()), message)
    }

    pub fn plain(name: &'static str, message: impl Into<String>) -> Self {
        Self::new(name, None, message)
    }

    /// TS `ProjectSessionCatalogUnavailableError` (`ProjectSessionDataPlane.ts:
    /// 36-41`): a named error with no wire code.
    pub fn catalog_unavailable() -> Self {
        Self::plain(
            "ProjectSessionCatalogUnavailableError",
            "The selected project session storage provider does not support session catalog reads.",
        )
    }

    /// `projects.resolveProjectKey` rejection keeps the caller's raw UTF-16
    /// units (`projectRegistry.ts:41-59`); a lone surrogate must not become
    /// U+FFFD before it reaches the wire.
    pub fn unknown_project_key(project_key: &[u16]) -> Self {
        let mut message = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
        message.extend_from_slice(project_key);
        Self {
            name: "DialogGatewayError".into(),
            code: Some("PROJECT_NOT_FOUND".into()),
            message: JsString::from_utf16(message),
        }
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn message(&self) -> &JsString {
        &self.message
    }

    /// Golden/wire shape: `{name, code?, message}`. The key order is the TS
    /// object literal order used by the real recorder (`name`, conditional
    /// `code`, `message`).
    pub fn as_json(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert("name".into(), self.name.as_str().into());
        if let Some(code) = &self.code {
            out.insert("code".into(), code.as_str().into());
        }
        out.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(out)
    }
}

impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.message.units()))
    }
}

impl std::error::Error for ManagerError {}

/// `projects.resolveProjectKey` seam. Production uses [`ManagerProjects`] over
/// the existing metadata catalog; fixture replay injects a fixed table.
pub trait ManagerProjectResolver: Send + Sync {
    fn resolve_project_key(&self, project_key: &[u16]) -> Result<PathBuf, ManagerError>;
}

/// Production resolver over the same `Metadata` catalog that snapshot uses.
pub struct ManagerProjects {
    metadata: Metadata<NativeFiles>,
}

impl ManagerProjects {
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            metadata: Metadata {
                home: home.into(),
                cwd: cwd.into(),
                files: NativeFiles,
            },
        }
    }
}

impl ManagerProjectResolver for ManagerProjects {
    fn resolve_project_key(&self, project_key: &[u16]) -> Result<PathBuf, ManagerError> {
        self.metadata
            .resolve_dialog_project(&JsString::from_utf16(project_key.to_vec()))
            .map(PathBuf::from)
            .ok_or_else(|| ManagerError::unknown_project_key(project_key))
    }
}

/// `managerSessions`: provider guard, catalog list, sessionKey filter, envelope.
#[derive(Clone, Default)]
pub struct ManagerSessions {
    consumer: Option<GatewaySessionCatalogConsumer>,
}

impl ManagerSessions {
    pub fn new(catalog: Arc<dyn SessionCatalogPort>) -> Self {
        Self {
            consumer: Some(GatewaySessionCatalogConsumer::new(catalog)),
        }
    }

    /// InProcessGateway's `managerSessions` with no host provider.
    pub fn unavailable() -> Self {
        Self { consumer: None }
    }

    pub fn consumer(&self) -> Option<&GatewaySessionCatalogConsumer> {
        self.consumer.as_ref()
    }

    fn require(&self) -> Result<&GatewaySessionCatalogConsumer, ManagerError> {
        self.consumer.as_ref().ok_or_else(|| {
            ManagerError::gateway(
                "CAPABILITY_UNAVAILABLE",
                "Gateway manager session provider is unavailable.",
            )
        })
    }

    /// `createLocalGateway.managerSessions`: `{items: filtered}`.
    pub fn list(
        &self,
        project_root: &Path,
        pilot_home: &Path,
        session_key: &JsValue,
    ) -> Result<JsValue, ManagerError> {
        let consumer = self.require()?;
        let sessions = consumer.list_manager_sessions(project_root, pilot_home)?;
        let items = if truthy(session_key) {
            sessions
                .into_iter()
                .filter(|session| matches_session_key(session, session_key))
                .collect()
        } else {
            sessions
        };
        let mut result = JsObject::new();
        result.insert("items".into(), JsValue::Array(items));
        Ok(JsValue::Object(result))
    }
}

/// `GatewayManagerResourceResult = { items: unknown[] }` provider.
///
/// The port returns the provider's complete result object because TS forwards
/// `options.managerBrowsers(input)` unchanged; it is not reconstructed here.
pub trait ManagerBrowserPort: Send + Sync {
    fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError>;
}

/// `managerBrowsers: options.managerBrowsers`.
#[derive(Clone, Default)]
pub struct ManagerBrowsers {
    provider: Option<Arc<dyn ManagerBrowserPort>>,
}

impl ManagerBrowsers {
    pub fn new(provider: Arc<dyn ManagerBrowserPort>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    /// InProcessGateway's `managerBrowsers` with no host provider.
    pub fn unavailable() -> Self {
        Self { provider: None }
    }

    pub fn provider(&self) -> Option<&Arc<dyn ManagerBrowserPort>> {
        self.provider.as_ref()
    }

    /// The success path returns the injected provider's value; it never
    /// synthesizes `CAPABILITY_UNAVAILABLE`.
    pub fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.provider
            .as_ref()
            .ok_or_else(|| {
                ManagerError::gateway(
                    "CAPABILITY_UNAVAILABLE",
                    "Gateway manager browser provider is unavailable.",
                )
            })?
            .list(input)
    }
}

/// `createLocalGateway.ts:1259-1270` projection.
pub struct ManagerProjection<R> {
    sessions: ManagerSessions,
    browsers: ManagerBrowsers,
    resolver: R,
    fallback_project_root: PathBuf,
    pilot_home: PathBuf,
}

impl<R: ManagerProjectResolver> ManagerProjection<R> {
    pub fn new(
        sessions: Option<Arc<dyn SessionCatalogPort>>,
        browsers: Option<Arc<dyn ManagerBrowserPort>>,
        resolver: R,
        fallback_project_root: impl Into<PathBuf>,
        pilot_home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sessions: sessions.map_or_else(ManagerSessions::unavailable, ManagerSessions::new),
            browsers: browsers.map_or_else(ManagerBrowsers::unavailable, ManagerBrowsers::new),
            resolver,
            fallback_project_root: fallback_project_root.into(),
            pilot_home: pilot_home.into(),
        }
    }

    pub fn sessions(&self) -> &ManagerSessions {
        &self.sessions
    }

    pub fn browsers(&self) -> &ManagerBrowsers {
        &self.browsers
    }

    /// `manager_sessions`: provider guard first (InProcessGateway), then
    /// `projectKey ?? fallbackProjectRoot` resolution, then catalog.
    pub fn manager_sessions(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.sessions.require()?;
        let project = self.resolve_source_project(input)?;
        self.sessions
            .list(&project, &self.pilot_home, field(input, "sessionKey"))
    }

    /// `manager_browsers`: the injected provider receives the raw input.
    pub fn manager_browsers(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
        self.browsers.list(input)
    }

    fn resolve_source_project(&self, input: &JsValue) -> Result<PathBuf, ManagerError> {
        // TS `managerSessions(input)` first reads `input.projectKey`; a nullish
        // input throws before `resolveProjectKey` and never reaches the catalog.
        if let Some(kind) = match input {
            JsValue::Undefined => Some("undefined"),
            JsValue::Null => Some("null"),
            _ => None,
        } {
            return Err(ManagerError::plain(
                "TypeError",
                format!("Cannot read properties of {kind} (reading 'projectKey')"),
            ));
        }
        let requested = field(input, "projectKey");
        let units = match requested {
            // `input.projectKey ?? fallbackProjectRoot`
            JsValue::Undefined | JsValue::Null => self
                .fallback_project_root
                .to_string_lossy()
                .encode_utf16()
                .collect(),
            // `path.resolve(projectKey)` accepts only strings; other values are
            // rejected by Node's validateString before the catalog is called.
            JsValue::String(text) => text.units().to_vec(),
            other => {
                // Reuse the gateway's existing Node `util.inspect` wording.
                let received = metadata::argument_received(other)
                    .unwrap_or_else(|| "an instance of Object".into());
                return Err(ManagerError::new(
                    "TypeError",
                    Some("ERR_INVALID_ARG_TYPE".into()),
                    format!(
                        "The \"paths[0]\" argument must be of type string. Received {received}"
                    ),
                ));
            }
        };
        self.resolver.resolve_project_key(&units)
    }
}

/// One native gateway-method outcome. There is no `Forward`: every error in
/// this component is a measured TS error, not a host boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ManagerDispatch {
    Response(JsValue),
}

/// `InProcessGateway.manager*` adapter. `dispatch` is the only wire-facing
/// method and is deliberately absent from `dispatch.rs::LOCAL_METHODS`.
pub struct ManagerGateway<R> {
    projection: ManagerProjection<R>,
}

impl<R: ManagerProjectResolver> ManagerGateway<R> {
    pub fn new(projection: ManagerProjection<R>) -> Self {
        Self { projection }
    }

    pub fn from_parts(
        sessions: Option<Arc<dyn SessionCatalogPort>>,
        browsers: Option<Arc<dyn ManagerBrowserPort>>,
        resolver: R,
        fallback_project_root: impl Into<PathBuf>,
        pilot_home: impl Into<PathBuf>,
    ) -> Self {
        Self {
            projection: ManagerProjection::new(
                sessions,
                browsers,
                resolver,
                fallback_project_root,
                pilot_home,
            ),
        }
    }

    pub fn projection(&self) -> &ManagerProjection<R> {
        &self.projection
    }

    pub fn sessions(&self, params: &JsValue) -> Result<JsValue, ManagerError> {
        self.projection.manager_sessions(params)
    }

    pub fn browsers(&self, params: &JsValue) -> Result<JsValue, ManagerError> {
        self.projection.manager_browsers(params)
    }

    /// WS method names. Unknown methods return `None` so default dispatch keeps
    /// forwarding; known errors become a TS-shaped response object.
    pub fn dispatch(&self, method: &str, params: &JsValue) -> Option<ManagerDispatch> {
        let outcome = match method {
            "manager_sessions" => self.sessions(params),
            "manager_browsers" => self.browsers(params),
            _ => return None,
        };
        Some(ManagerDispatch::Response(match outcome {
            Ok(value) => value,
            Err(error) => error.as_json(),
        }))
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Object(_) | JsValue::Array(_) => true,
    }
}

fn matches_session_key(session: &JsValue, key: &JsValue) -> bool {
    let Some(key) = key.as_string() else {
        // TS compares with `===`; a number/boolean/object never equals a string
        // `sessionId`/`sessionKey`.
        return false;
    };
    ["sessionId", "sessionKey"].iter().any(|name| {
        session
            .get(name)
            .and_then(JsValue::as_string)
            .is_some_and(|value| value == key)
    })
}
