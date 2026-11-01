//! The five U7 gateway methods. See the [module](super) docs for the layer
//! map; every comment below cites the TS line it mirrors.
use crate::config::{
    Error as ConfigError,
    store::{ConfigStore, ReloadEvent, Subscription},
};
use crate::js_value;
use crate::runtime::refresh::{ExtensionsReload, RefreshBundle, ReloadOutcome};
use crate::runtime::session::SessionOwner;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

/// Boxed delegate future; the injected ports own their state.
pub type SettingsFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A `DialogGatewayError` / raw JS error / `PilotConfigError` /
/// `PilotLocalSettingsError` in the WS error frame's `{code, message}`
/// projection. `code: None` is a raw throw (the WS layer answers
/// `gateway_request_failed` for it); the name is kept for the adapter-level
/// contract like the MCP slice's `SdkMcpError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    code: Option<JsString>,
    name: &'static str,
    message: JsString,
}

impl SettingsError {
    pub fn gateway(code: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            code: Some(code.into()),
            name: "DialogGatewayError",
            message: message.into(),
        }
    }
    /// A JS throw without a structured code (`TypeError`, `RangeError`, ...).
    pub fn plain(name: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            code: None,
            name,
            message: message.into(),
        }
    }
    fn type_error(message: impl Into<JsString>) -> Self {
        Self::plain("TypeError", message)
    }
    /// A `PilotConfigError` from the store reload (code + message + name).
    fn config(error: &ConfigError) -> Self {
        Self {
            code: Some(JsString::from(error.code.clone())),
            name: error.name,
            message: error.message.clone(),
        }
    }
    /// A `PilotLocalSettingsError` from the writer port.
    pub fn local_settings(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            code: Some(JsString::from(code)),
            name: "PilotLocalSettingsError",
            message: message.into(),
        }
    }
    pub fn code(&self) -> Option<&JsString> {
        self.code.as_ref()
    }
    pub fn name(&self) -> &str {
        self.name
    }
    pub fn message(&self) -> &JsString {
        &self.message
    }
}

// ------------------------------------------------------------- delegate ports

/// `options.applyFlagSettings` (createLocalGateway binds
/// `registry.applyFlagSettingsForSdk`). The real registry method is
/// synchronous state mutation, so the port is too.
pub trait ApplyFlagSettings: Send + Sync {
    fn apply(&self, input: &JsValue) -> Result<FlagSettings, SettingsError>;
}

/// `GatewayApplyFlagSettingsResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlagSettings {
    pub applied: Vec<JsString>,
    pub cleared: Vec<JsString>,
}

/// `updatePilotLocalSettings({ settings, env, projectRoot })`: the allowlist
/// validation, the YAML document edit and the atomic write. The production
/// writer is a wiring-layer concern; the adapter composes it with the real
/// store reload below.
pub trait LocalSettingsWriter: Send + Sync {
    fn update(&self, settings: &JsValue) -> Result<LocalSettingsUpdate, SettingsError>;
}

/// `PilotLocalSettingsUpdateResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalSettingsUpdate {
    pub applied: Vec<JsString>,
    pub cleared: Vec<JsString>,
    pub changed_paths: Vec<JsString>,
}

/// createLocalGateway's `updateSettings` closure state.
pub struct UpdateSettings {
    /// `organizationPolicy?.settings?.canUpdateLocalSettings === false`.
    pub organization_denied: bool,
    pub writer: Arc<dyn LocalSettingsWriter>,
    /// The same store the refresh bundle listens to: the write, the reload
    /// and the broadcasts stay one chain (gap U7).
    pub store: ConfigStore,
}

/// `options.reloadOutputStyles` (createLocalGateway binds
/// `registry.reloadOutputStylesForSdk`): resolve, the style-only plugin
/// reload, and the per-session `closeSession("sdk_output_styles_reloaded")`.
pub trait ReloadOutputStyles: Send + Sync {
    fn reload(&self, params: &JsValue)
    -> SettingsFuture<Result<OutputStylesReload, SettingsError>>;
}

/// `GatewayReloadOutputStylesResult`'s `changed` list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputStylesReload {
    pub changed: Vec<JsString>,
}

/// `options.reloadConfig` (createLocalGateway binds
/// `runtimeRefresh.reloadConfig()`).
pub trait ReloadConfigSource: Send + Sync {
    fn reload_config(&self) -> SettingsFuture<Result<ReloadOutcome, SettingsError>>;
}

impl ReloadConfigSource for RefreshBundle {
    fn reload_config(&self) -> SettingsFuture<Result<ReloadOutcome, SettingsError>> {
        let reload = RefreshBundle::reload_config(self);
        Box::pin(async move { reload.await.map_err(|error| SettingsError::config(&error)) })
    }
}

/// `options.reloadExtensions` (createLocalGateway binds
/// `runtimeRefresh.reloadExtensions(input)`).
pub trait ReloadExtensionsSource: Send + Sync {
    fn reload_extensions(&self, input: ExtensionsReload) -> ReloadOutcome;
}

impl ReloadExtensionsSource for RefreshBundle {
    fn reload_extensions(&self, input: ExtensionsReload) -> ReloadOutcome {
        RefreshBundle::reload_extensions(self, input)
    }
}

// ---------------------------------------------------------------- the adapter

pub struct SettingsGatewayOptions {
    /// The #1011 router-facing admission/close port: `hasActiveTurn` drives
    /// the SESSION_BUSY gate and `close` is the post-success session close.
    pub owner: SessionOwner,
    pub apply_flag_settings: Option<Arc<dyn ApplyFlagSettings>>,
    pub update_settings: Option<UpdateSettings>,
    pub reload_output_styles: Option<Arc<dyn ReloadOutputStyles>>,
    pub reload_config: Option<Arc<dyn ReloadConfigSource>>,
    pub reload_extensions: Option<Arc<dyn ReloadExtensionsSource>>,
}

/// The five U7 methods with `InProcessGateway`'s method-level contract.
///
/// Not part of the default routing: callers assemble this component when
/// they want the methods; nothing in `dispatch.rs` references it yet (#1320).
pub struct SettingsGateway {
    options: SettingsGatewayOptions,
}

static UNDEFINED: JsValue = JsValue::Undefined;

impl SettingsGateway {
    pub fn new(options: SettingsGatewayOptions) -> Self {
        Self { options }
    }

    /// `InProcessGateway.applyFlagSettings` (client/InProcessGateway.ts:2337).
    pub async fn apply_flag_settings(&self, input: &JsValue) -> Result<JsValue, SettingsError> {
        let port = self
            .options
            .apply_flag_settings
            .as_ref()
            .ok_or_else(|| unavailable("apply_flag_settings"))?;
        let session_key = required_session_key(input)?;
        let settings = field(input, "settings");
        // `!input.settings || typeof input.settings !== "object" ||
        // Array.isArray(input.settings)`.
        if !matches!(settings, JsValue::Object(_)) {
            return Err(SettingsError::gateway(
                "INVALID_FLAG_SETTINGS",
                "settings must be an object.",
            ));
        }
        if self.options.owner.has_active_turn(&session_key) {
            return Err(SettingsError::gateway(
                "SESSION_BUSY",
                "Cannot apply flag settings during an active turn.",
            ));
        }
        validate_sdk_flag_settings(settings)?;
        let result = port.apply(input)?;
        // `if (result.applied.length > 0 || result.cleared.length > 0)
        // await this.router.close(input.sessionKey)` — a close failure
        // propagates instead of being swallowed.
        if !result.applied.is_empty() || !result.cleared.is_empty() {
            self.close(&session_key).await?;
        }
        let mut object = JsObject::new();
        object.insert("applied".into(), strings(&result.applied));
        object.insert("cleared".into(), strings(&result.cleared));
        Ok(JsValue::Object(object))
    }

    /// `InProcessGateway.updateSettings` (client/InProcessGateway.ts:2350):
    /// capability, then the `source` and `settings` field checks, then
    /// createLocalGateway's closure.
    pub async fn update_settings(&self, input: &JsValue) -> Result<JsValue, SettingsError> {
        let chain = self
            .options
            .update_settings
            .as_ref()
            .ok_or_else(|| unavailable("update_settings"))?;
        // `input.source` on a nullish input throws the property-read
        // TypeError before any field check.
        require_input(input, "source")?;
        if !matches!(field(input, "source"), JsValue::String(source) if source.equals_str("localSettings"))
        {
            return Err(SettingsError::gateway(
                "INVALID_SETTINGS_SOURCE",
                "Only localSettings is supported.",
            ));
        }
        let settings = field(input, "settings");
        if !matches!(settings, JsValue::Object(_)) {
            return Err(SettingsError::gateway(
                "INVALID_LOCAL_SETTINGS",
                "settings must be an object.",
            ));
        }
        // createLocalGateway.ts:1384-1404.
        if chain.organization_denied {
            return Err(SettingsError::gateway(
                "GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED",
                "Gateway organization policy denies SDK localSettings updates.",
            ));
        }
        let updated = chain.writer.update(settings)?;
        if updated.changed_paths.is_empty() {
            return Ok(update_result(
                &updated.applied,
                &updated.cleared,
                &updated.changed_paths,
            ));
        }
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let subscription = chain.store.subscribe(Arc::new(move |event: &ReloadEvent| {
            *lock(&sink) = event.changed_paths.clone()
        }));
        // `try { await configStore.reload("sdk-update-settings") } finally
        // { unsubscribe() }`: the listener is detached when the reload
        // settles, when it fails, and when the future is dropped at the
        // await point (the WS layer drops in-flight requests on disconnect;
        // `Subscription` has no `Drop` of its own). Same guard shape as
        // `RefreshBundle::reload_config`'s capture.
        struct Capture(Option<Subscription>);
        impl Drop for Capture {
            fn drop(&mut self) {
                if let Some(subscription) = self.0.take() {
                    subscription.unsubscribe();
                }
            }
        }
        let capture = Capture(Some(subscription));
        let reload = chain.store.reload("sdk-update-settings");
        let result = reload.await;
        drop(capture);
        result.map_err(|error| SettingsError::config(&error))?;
        let changed_paths = std::mem::take(&mut *lock(&captured));
        Ok(update_result(
            &updated.applied,
            &updated.cleared,
            &changed_paths,
        ))
    }

    /// `InProcessGateway.reloadOutputStyles`
    /// (client/InProcessGateway.ts:2387): the capability check is the whole
    /// wrapper; the registry method owns the rest.
    pub async fn reload_output_styles(&self, params: &JsValue) -> Result<JsValue, SettingsError> {
        let port = self
            .options
            .reload_output_styles
            .as_ref()
            .ok_or_else(|| unavailable("reload_output_styles"))?;
        let outcome = port.reload(params).await?;
        let mut object = JsObject::new();
        object.insert("reloaded".into(), JsValue::Bool(true));
        object.insert("changed".into(), strings(&outcome.changed));
        Ok(JsValue::Object(object))
    }

    /// `InProcessGateway.reloadConfig` (client/InProcessGateway.ts:2905):
    /// the absent option is the `{reloaded: false, reason: "unsupported"}`
    /// projection, exactly like the WS dispatch of the same method.
    pub async fn reload_config(&self) -> Result<JsValue, SettingsError> {
        let Some(port) = &self.options.reload_config else {
            return Ok(unsupported());
        };
        let outcome = port.reload_config().await?;
        Ok(reload_outcome(&outcome))
    }

    /// `InProcessGateway.reloadExtensions`
    /// (client/InProcessGateway.ts:2923). `params` is `frame.params`
    /// verbatim: `undefined`/`null` and every non-object shape take the
    /// bundle's default input, like `input?.projectKey` in TS.
    pub async fn reload_extensions(&self, params: &JsValue) -> Result<JsValue, SettingsError> {
        let Some(port) = &self.options.reload_extensions else {
            return Ok(unsupported());
        };
        let input = extensions_reload(params)?;
        Ok(reload_outcome(&port.reload_extensions(input)))
    }

    /// The `GatewayWsConnection` dispatch entry for the five methods
    /// (server/GatewayWsConnection.ts:405-425, 523-543): `None` means the
    /// method is not one of the five and belongs to the caller's wider
    /// dispatch.
    pub async fn dispatch(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Option<Result<JsValue, SettingsError>> {
        Some(match method {
            "apply_flag_settings" => self.apply_flag_settings(params).await,
            "update_settings" => self.update_settings(params).await,
            "reload_output_styles" => self.reload_output_styles(params).await,
            "reload_config" => self.reload_config().await,
            "reload_extensions" => self.reload_extensions(params).await,
            _ => return None,
        })
    }

    /// `await this.router.close(input.sessionKey)`. A close failure
    /// propagates; the WS error frame carries only code+message, so the TS
    /// `AggregateError` name of a dispose failure is flattened to `"Error"`
    /// like the MCP slice's adapter.
    async fn close(&self, session_key: &JsString) -> Result<(), SettingsError> {
        self.options
            .owner
            .close(session_key)
            .await
            .map_err(|error| SettingsError::plain("Error", JsString::from(error.message)))
    }
}

// ------------------------------------------------------------------- helpers

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `if (!this.options.<method>) throw new DialogGatewayError(
/// "CAPABILITY_UNAVAILABLE", "<method> is unavailable.")` — the WS dispatch
/// of the same methods rejects with the same code and message text.
fn unavailable(method: &'static str) -> SettingsError {
    let mut units = method.encode_utf16().collect::<Vec<_>>();
    units.extend(" is unavailable.".encode_utf16());
    SettingsError::gateway("CAPABILITY_UNAVAILABLE", JsString::from_utf16(units))
}

/// `{ reloaded: false, reason: "unsupported" }`.
fn unsupported() -> JsValue {
    let mut object = JsObject::new();
    object.insert("reloaded".into(), JsValue::Bool(false));
    object.insert("reason".into(), "unsupported".into());
    JsValue::Object(object)
}

/// `{ reloaded, changedPaths }` (the bundle always returns both).
fn reload_outcome(outcome: &ReloadOutcome) -> JsValue {
    let mut object = JsObject::new();
    object.insert("reloaded".into(), JsValue::Bool(outcome.reloaded));
    object.insert("changedPaths".into(), strings(&outcome.changed_paths));
    JsValue::Object(object)
}

/// `{ applied, cleared, changedPaths }` with the TS key order
/// (`{ ...updated, changedPaths }`).
fn update_result(
    applied: &[JsString],
    cleared: &[JsString],
    changed_paths: &[JsString],
) -> JsValue {
    let mut object = JsObject::new();
    object.insert("applied".into(), strings(applied));
    object.insert("cleared".into(), strings(cleared));
    object.insert("changedPaths".into(), strings(changed_paths));
    JsValue::Object(object)
}

fn strings(values: &[JsString]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}

/// `input.<field>`: a nullish input throws the property-read TypeError;
/// every other shape simply has no own property (reading a primitive in JS
/// yields `undefined`).
fn field<'a>(input: &'a JsValue, name: &str) -> &'a JsValue {
    input.get(name).unwrap_or(&UNDEFINED)
}

/// `!input.sessionKey?.trim()`: a nullish input throws the property-read
/// TypeError, a missing/nullish/blank key is rejected as blank (the optional
/// chain short-circuits on a nullish key value, including the `undefined`
/// a primitive params' property read yields), and a non-string non-nullish
/// key has no `trim` method, so the call itself throws
/// `input.sessionKey?.trim is not a function`. Returns the raw key, trimmed
/// only for the emptiness check.
fn required_session_key(input: &JsValue) -> Result<JsString, SettingsError> {
    require_input(input, "sessionKey")?;
    match field(input, "sessionKey") {
        JsValue::String(key) => {
            let text = String::from_utf16_lossy(key.units());
            if crate::metadata::js_trim(&text).is_empty() {
                Err(SettingsError::gateway(
                    "INVALID_SESSION_KEY",
                    "sessionKey is required.",
                ))
            } else {
                Ok(key.clone())
            }
        }
        JsValue::Undefined | JsValue::Null => Err(SettingsError::gateway(
            "INVALID_SESSION_KEY",
            "sessionKey is required.",
        )),
        _ => Err(SettingsError::type_error(
            "input.sessionKey?.trim is not a function",
        )),
    }
}

/// `input.<field>` on a nullish input: the raw JS property read throws.
fn require_input<'a>(input: &'a JsValue, field: &str) -> Result<&'a JsValue, SettingsError> {
    match input {
        JsValue::Undefined | JsValue::Null => {
            let kind = if input.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            Err(SettingsError::type_error(format!(
                "Cannot read properties of {kind} (reading '{field}')"
            )))
        }
        _ => Ok(input),
    }
}

/// `Object.entries(settings)` with JS own-key order: array indices first
/// in ascending numeric order, then other keys in insertion order.
fn own_entries(object: &JsObject) -> Vec<(JsString, JsValue)> {
    let mut indexed = Vec::new();
    let mut ordinary = Vec::new();
    for (name, value) in object.entries() {
        match name.array_index() {
            Some(index) => indexed.push((index, name.clone(), value.clone())),
            None => ordinary.push((name.clone(), value.clone())),
        }
    }
    indexed.sort_by_key(|(index, _, _)| *index);
    indexed
        .into_iter()
        .map(|(_, name, value)| (name, value))
        .chain(ordinary)
        .collect()
}

/// `validateSdkFlagSettings` (InProcessGateway.ts:3179-3214). The entries are
/// walked in JS own-key order, so a settings object mixing an array-index
/// key with `effortLevel` fails on the index key first, as in TS.
fn validate_sdk_flag_settings(settings: &JsValue) -> Result<(), SettingsError> {
    let object = settings
        .as_object()
        .expect("the wrapper checked settings is an object");
    for (key, value) in own_entries(object) {
        if key.equals_str("effortLevel") {
            let ok = matches!(&value, JsValue::Null)
                || value.as_string().is_some_and(|mode| {
                    mode.equals_str("low") || mode.equals_str("medium") || mode.equals_str("high")
                });
            if !ok {
                return Err(SettingsError::gateway(
                    "INVALID_FLAG_SETTINGS",
                    "effortLevel must be low, medium, high, or null.",
                ));
            }
            continue;
        }
        if key.equals_str("permissions") {
            // `if (value === null) continue; if (!value || typeof value !==
            // "object" || Array.isArray(value)) throw ...`.
            if matches!(&value, JsValue::Null) {
                continue;
            }
            let entries = match &value {
                JsValue::Object(object) => own_entries(object),
                _ => {
                    return Err(SettingsError::gateway(
                        "INVALID_FLAG_SETTINGS",
                        "permissions must be an object or null.",
                    ));
                }
            };
            for (permission_key, permission_value) in entries {
                if !permission_key.equals_str("defaultMode") {
                    let mut units = "permissions.".encode_utf16().collect::<Vec<_>>();
                    units.extend(permission_key.units().iter().copied());
                    units.extend(" has no PilotDeck session equivalent.".encode_utf16());
                    return Err(SettingsError::gateway(
                        "UNSUPPORTED_FLAG_SETTING",
                        JsString::from_utf16(units),
                    ));
                }
                let mode_ok = matches!(&permission_value, JsValue::Null)
                    || permission_value.as_string().is_some_and(|mode| {
                        mode.equals_str("default")
                            || mode.equals_str("plan")
                            || mode.equals_str("bypassPermissions")
                    });
                if !mode_ok {
                    let text = js_value::to_string_units_checked(&permission_value)
                        .map_err(to_string_throw)?;
                    let mut units = "permissions.defaultMode="
                        .encode_utf16()
                        .collect::<Vec<_>>();
                    units.extend(text);
                    units.extend(" is not supported.".encode_utf16());
                    return Err(SettingsError::gateway(
                        "UNSUPPORTED_FLAG_SETTING",
                        JsString::from_utf16(units),
                    ));
                }
            }
            continue;
        }
        let mut units = key.units().to_vec();
        units.extend(" has no PilotDeck session equivalent.".encode_utf16());
        return Err(SettingsError::gateway(
            "UNSUPPORTED_FLAG_SETTING",
            JsString::from_utf16(units),
        ));
    }
    Ok(())
}

/// The `String(value)` throws inside the validation message.
fn to_string_throw(error: js_value::ToStringError) -> SettingsError {
    SettingsError::plain(error.name(), error.message())
}

/// `frame.params` → `ExtensionsReload` for the bundle delegate. Only an
/// object params has own properties in TS (`input?.projectKey`); a non-string
/// truthy `projectKey` or a non-string `changedPaths` entry is coerced with
/// `String(value)` (the registered divergence
/// `U7-EXTENSIONS-RELOAD-STRING-COERCION-001`, to converge with #1320).
fn extensions_reload(params: &JsValue) -> Result<ExtensionsReload, SettingsError> {
    let object = match params {
        JsValue::Object(object) => object,
        _ => return Ok(ExtensionsReload::default()),
    };
    let project_key = match object.get("projectKey") {
        None | Some(JsValue::Undefined | JsValue::Null) => None,
        Some(JsValue::String(key)) => Some(key.clone()),
        Some(value) => Some(JsString::from_utf16(
            js_value::to_string_units_checked(value).map_err(to_string_throw)?,
        )),
    };
    let changed_paths = match object.get("changedPaths") {
        None | Some(JsValue::Undefined | JsValue::Null) => None,
        Some(JsValue::Array(items)) => Some(
            items
                .iter()
                .map(|item| {
                    Ok(match item {
                        JsValue::Null | JsValue::Undefined => JsString::from(""),
                        JsValue::String(text) => text.clone(),
                        value => JsString::from_utf16(
                            js_value::to_string_units_checked(value).map_err(to_string_throw)?,
                        ),
                    })
                })
                .collect::<Result<Vec<_>, SettingsError>>()?,
        ),
        // A non-array `changedPaths` has no `join`: the bundle method throws
        // the property-access TypeError, exactly like
        // `changedPaths.join(", ")` in TS.
        Some(_) => {
            return Err(SettingsError::type_error(
                "changedPaths.join is not a function",
            ));
        }
    };
    Ok(ExtensionsReload {
        project_key,
        changed_paths,
    })
}
