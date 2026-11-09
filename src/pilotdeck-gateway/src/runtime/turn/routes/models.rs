//! Session model preferences use the native transcript, not SDK configuration.
//! SDK configuration remains a separate, lower-priority model overlay.
use crate::{
    metadata::{Metadata, NativeFiles, object},
    runtime::{
        connection::RequestError,
        replacement::ReplacementCoordinator,
        sdk_session::{
            ModelField, ModelRef, ResolvedSdkRuntime, SdkSessionError, SdkSessionRegistry,
            resolve_configured_model,
        },
        session::SessionOwner,
    },
    storage::{AdapterError, AsyncTranscriptAdapter, SessionStorage, SessionStorageInput},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;

pub struct Models<'a> {
    pub sdk: &'a Mutex<SdkSessionRegistry>,
    pub owner: &'a SessionOwner,
    pub replacement: &'a ReplacementCoordinator,
    pub transcripts: &'a AsyncTranscriptAdapter,
    pub pilot_home: &'a Path,
}

pub async fn handle(
    models: &Models<'_>,
    method: &str,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    let operation = match method {
        "session_model_get" => None,
        "session_model_set" => Some("change the session model"),
        "session_model_clear" => Some("clear the session model"),
        _ => return Err(error("unknown_method", format!("Unknown method: {method}"))),
    };
    let session = session_key(params)?;
    // The reservation also fences fork/replace; Drop releases it on cancellation.
    let _write = if let Some(operation) = operation {
        models
            .replacement
            .reserve_transcript_write(&session, operation)
            .map_err(|failure| {
                error(
                    failure.code.unwrap_or("gateway_request_failed"),
                    failure.message,
                )
            })?;
        Some(TranscriptReservation {
            replacement: models.replacement,
            session: &session,
        })
    } else {
        None
    };
    let sdk = models.sdk.lock().await;
    let project = project_key(params)?;
    let runtime = resolve_project(models, &sdk, &project)?;
    let project = JsString::from(runtime.project_root.to_string_lossy().into_owned());
    let _maintenance = if operation.is_some() {
        Some(
            models
                .owner
                .begin_maintenance(&session)
                .map_err(|failure| RequestError::new(failure.message))?
                .ok_or_else(|| {
                    error(
                        "SESSION_BUSY",
                        if method == "session_model_set" {
                            "Cannot change the model during an active turn."
                        } else {
                            "Cannot clear the model during an active turn."
                        },
                    )
                })?,
        )
    } else {
        None
    };
    let storage = storage(models, &sdk, &runtime, &session)?;
    let saved = match method {
        "session_model_set" => {
            let requested = field(params, "selection");
            if !matches!(requested, JsValue::Object(_) | JsValue::Array(_)) {
                return Err(error("INVALID_MODEL_OVERRIDE", "selection is required."));
            }
            validate_selection(&catalog(models, &sdk)?, requested)?;
            let selection = normalize_selection(requested);
            // Enforce SDK/organization policy against the model actually selected.
            if is(field(&selection, "mode"), "model") {
                assert_allowed(&sdk, &session, &project, &selection)?;
            }
            persist(
                &storage,
                &session,
                "model-selection",
                selection.clone(),
                (sdk.options().clock)(),
            )?;
            selection
        }
        "session_model_clear" => {
            persist(
                &storage,
                &session,
                "model-selection-clear",
                JsValue::Null,
                (sdk.options().clock)(),
            )?;
            JsValue::Undefined
        }
        _ => {
            let saved = read_selection(&storage)?;
            let catalog = if is(field(&saved, "mode"), "model")
                && !field(&saved, "reasoning").is_undefined()
            {
                catalog(models, &sdk)?
            } else {
                object([])
            };
            restore_selection(&catalog, saved)
        }
    };
    let result = if method == "session_model_clear" {
        JsValue::Undefined
    } else {
        session_result(&project, &session, saved, &runtime)
    };
    drop(sdk);
    if operation.is_some() {
        // TS closes the cached session after a successful metadata append.
        models
            .owner
            .close(&session)
            .await
            .map_err(|failure| RequestError::new(failure.message))?;
    }
    Ok(result)
}

/// Every branch returns a concrete selection for AgentConfig. `source` still
/// distinguishes automatic routing and defaults from explicit preferences.
pub async fn resolve_for_turn(
    models: &Models<'_>,
    session: &JsString,
    project: &JsString,
    input: &JsValue,
) -> Result<JsValue, RequestError> {
    require_session_key(session)?;
    let sdk = models.sdk.lock().await;
    let runtime = resolve_project(models, &sdk, project)?;
    let project = JsString::from(runtime.project_root.to_string_lossy().into_owned());
    let requested = field(input, "modelSelection");
    let legacy = field(input, "modelOverride");
    if !requested.is_undefined() && !legacy.is_undefined() {
        return Err(error(
            "INVALID_MODEL_OVERRIDE",
            "Specify modelSelection or modelOverride, not both.",
        ));
    }
    let (selection, source) = if !requested.is_undefined() {
        if !matches!(requested, JsValue::Object(_) | JsValue::Array(_)) {
            return Err(error(
                "INVALID_MODEL_OVERRIDE",
                "modelSelection must be an object.",
            ));
        }
        validate_selection(&catalog(models, &sdk)?, requested)?;
        if is(field(requested, "mode"), "model") {
            (normalize_selection(requested), "turn")
        } else {
            (default_selection(&sdk, session, &runtime)?, "router")
        }
    } else if truthy(legacy) {
        validate_explicit(&catalog(models, &sdk)?, legacy)?;
        (normalize_selection(legacy), "turn")
    } else {
        let storage = storage(models, &sdk, &runtime, session)?;
        let saved = read_selection(&storage)?;
        let catalog = if is(field(&saved, "mode"), "model") {
            catalog(models, &sdk)?
        } else {
            object([])
        };
        let saved = restore_selection(&catalog, saved);
        if is(field(&saved, "mode"), "model") {
            validate_explicit(&catalog, &saved)?;
            (saved, "session")
        } else {
            (
                default_selection(&sdk, session, &runtime)?,
                default_source(&runtime),
            )
        }
    };
    assert_allowed(&sdk, session, &project, &selection)?;
    Ok(object([
        ("selection", selection),
        ("source", source.into()),
    ]))
}

fn default_selection(
    sdk: &SdkSessionRegistry,
    session: &JsString,
    runtime: &ResolvedSdkRuntime,
) -> Result<JsValue, RequestError> {
    let config = sdk
        .resolve_session_config(session, runtime)
        .map_err(sdk_error)?;
    let requested = config
        .as_ref()
        .and_then(|config| config.get("settings"))
        .and_then(|settings| settings.get("agent"))
        .and_then(|agent| agent.get("model"));
    let model = if let Some(requested) = requested.filter(|value| truthy(value)) {
        let requested = requested.as_string().ok_or_else(|| {
            error(
                "INVALID_SDK_AGENT_MODEL",
                "settings.agent.model must be a string.",
            )
        })?;
        let reference =
            resolve_configured_model(&runtime.snapshot, requested, ModelField::AgentModel)
                .map_err(sdk_error)?;
        object([
            ("provider", JsValue::String(reference.provider)),
            ("model", JsValue::String(reference.model)),
        ])
    } else {
        field(field(&runtime.snapshot, "config"), "agent")
            .get("model")
            .cloned()
            .unwrap_or(JsValue::Undefined)
    };
    if field(&model, "provider")
        .as_string()
        .is_none_or(JsString::is_empty)
        || field(&model, "model")
            .as_string()
            .is_none_or(JsString::is_empty)
    {
        return Err(error(
            "INVALID_MODEL_OVERRIDE",
            "Configured default model is invalid.",
        ));
    }
    Ok(object([
        ("mode", "model".into()),
        ("provider", field(&model, "provider").clone()),
        ("model", field(&model, "model").clone()),
    ]))
}

fn default_source(runtime: &ResolvedSdkRuntime) -> &'static str {
    if field(
        field(field(&runtime.snapshot, "config"), "router"),
        "enabled",
    )
    .as_bool()
        == Some(true)
    {
        "router"
    } else {
        "default"
    }
}

fn assert_allowed(
    sdk: &SdkSessionRegistry,
    session: &JsString,
    project: &JsString,
    selection: &JsValue,
) -> Result<(), RequestError> {
    let model = ModelRef {
        provider: string(selection, "provider")?,
        model: string(selection, "model")?,
    };
    sdk.assert_model_allowed(session, Some(&model), Some(project))
        .map_err(sdk_error)
}

fn metadata(models: &Models<'_>, sdk: &SdkSessionRegistry) -> Metadata {
    Metadata {
        home: models.pilot_home.to_path_buf(),
        cwd: sdk.options().cwd.clone(),
        files: NativeFiles,
    }
}

fn resolve_project(
    models: &Models<'_>,
    sdk: &SdkSessionRegistry,
    project: &JsString,
) -> Result<ResolvedSdkRuntime, RequestError> {
    let metadata = metadata(models, sdk);
    let root = crate::metadata::resolved_identity(&metadata.cwd, project)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| unknown_project(project))?;
    if root != metadata.home
        && !metadata.has_registered_project(&root)
            .map_err(|failure| RequestError::new(failure.to_string()))?
    {
        return Err(unknown_project(project));
    }
    sdk.options()
        .resolver
        .resolve(Some(&JsString::from(root.to_string_lossy().into_owned())))
        .map_err(sdk_error)
}

fn catalog(models: &Models<'_>, sdk: &SdkSessionRegistry) -> Result<JsValue, RequestError> {
    crate::model_catalog::execute(&metadata(models, sdk), &object([]), &sdk.options().env)
        .map_err(|failure| error(&failure.code, failure.message))
}

fn storage(
    models: &Models<'_>,
    sdk: &SdkSessionRegistry,
    runtime: &ResolvedSdkRuntime,
    session: &JsString,
) -> Result<SessionStorage, RequestError> {
    let now = iso_time((sdk.options().clock)())?;
    models
        .transcripts
        .create_session_storage(SessionStorageInput {
            project_root: runtime.project_root.clone(),
            pilot_home: models.pilot_home.to_path_buf(),
            session_id: session.clone(),
            now: Some(Arc::new(move || now.clone())),
        })
        .map_err(storage_error)
}

fn read_selection(storage: &SessionStorage) -> Result<JsValue, RequestError> {
    let read = storage.restore().map_err(storage_error)?;
    let mut saved = JsValue::Undefined;
    for entry in &read.entries {
        let selection = field(field(entry, "metadata"), "modelSelection");
        if (is(field(entry, "type"), "session_metadata") && !selection.is_undefined())
            || (is(field(entry, "type"), "accepted_input")
                && (is(field(selection, "mode"), "auto")
                    || (is(field(selection, "mode"), "model")
                        && field(selection, "provider").as_string().is_some()
                        && field(selection, "model").as_string().is_some())))
        {
            saved = selection.clone();
        }
    }
    Ok(saved)
}

fn persist(
    storage: &SessionStorage,
    session: &JsString,
    turn: &str,
    selection: JsValue,
    now_ms: f64,
) -> Result<(), RequestError> {
    let updated_at = iso_time(now_ms)?;
    // Restore sequence/parent before the short-lived metadata writer appends.
    let result = storage.restore().map_err(storage_error).and_then(|_| {
        storage
            .transcript
            .record_session_metadata(
                session,
                &turn.into(),
                object([
                    ("modelSelection", selection),
                    ("updatedAt", JsValue::String(updated_at.into())),
                ]),
            )
            .map_err(storage_error)
    });
    storage.transcript.close();
    result
}

fn session_result(
    project: &JsString,
    session: &JsString,
    saved: JsValue,
    runtime: &ResolvedSdkRuntime,
) -> JsValue {
    let explicit = is(field(&saved, "mode"), "model");
    let model = if explicit {
        &saved
    } else {
        field(field(field(&runtime.snapshot, "config"), "agent"), "model")
    };
    let effective = object([
        ("provider", field(model, "provider").clone()),
        ("model", field(model, "model").clone()),
        (
            "source",
            if explicit {
                "session"
            } else {
                default_source(runtime)
            }
            .into(),
        ),
        (
            "reasoning",
            if explicit {
                field(&saved, "reasoning").clone()
            } else {
                JsValue::Undefined
            },
        ),
        (
            "speed",
            if explicit {
                field(&saved, "speed").clone()
            } else {
                JsValue::Undefined
            },
        ),
    ]);
    object([
        ("projectKey", JsValue::String(project.clone())),
        ("sessionKey", JsValue::String(session.clone())),
        ("saved", saved),
        ("effective", effective),
    ])
}

fn validate_selection(catalog: &JsValue, selection: &JsValue) -> Result<(), RequestError> {
    if is(field(selection, "mode"), "auto") {
        if field(field(catalog, "router"), "autoAvailable").as_bool() != Some(true) {
            return Err(error(
                "ROUTER_AUTO_UNAVAILABLE",
                "Router auto is not available for this project.",
            ));
        }
        Ok(())
    } else {
        validate_explicit(catalog, selection)
    }
}

fn catalog_item<'a>(catalog: &'a JsValue, selection: &JsValue) -> Option<&'a JsValue> {
    let JsValue::Array(items) = field(catalog, "items") else {
        return None;
    };
    items.iter().find(|item| {
        field(item, "provider") == field(selection, "provider")
            && field(item, "model") == field(selection, "model")
    })
}

fn validate_explicit(catalog: &JsValue, selection: &JsValue) -> Result<(), RequestError> {
    if !is(field(selection, "mode"), "model")
        || field(selection, "provider").as_string().is_none()
        || field(selection, "model").as_string().is_none()
    {
        return Err(error(
            "INVALID_MODEL_OVERRIDE",
            "A model selection requires mode, provider, and model.",
        ));
    }
    let item = catalog_item(catalog, selection)
        .filter(|item| field(item, "available").as_bool() == Some(true))
        .ok_or_else(|| {
            let mut message = "Model is unavailable: ".encode_utf16().collect::<Vec<_>>();
            message.extend(field(selection, "provider").as_string().unwrap().units());
            message.push(u16::from(b'/'));
            message.extend(field(selection, "model").as_string().unwrap().units());
            error("INVALID_MODEL_OVERRIDE", JsString::from_utf16(message))
        })?;
    let speed = field(selection, "speed");
    if !speed.is_undefined() {
        if !speed
            .as_number()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
        {
            return Err(error(
                "UNSUPPORTED_MODEL_PARAMETER",
                "speed must be between 0 and 1.",
            ));
        }
        if field(field(item, "capabilities"), "speed").is_undefined() {
            return Err(error(
                "UNSUPPORTED_MODEL_PARAMETER",
                format!("speed is not supported by {}.", display(field(item, "id"))),
            ));
        }
    }
    let reasoning = field(selection, "reasoning");
    if !reasoning.is_undefined() && !supports_reasoning(item, reasoning) {
        return Err(error(
            "UNSUPPORTED_MODEL_PARAMETER",
            format!(
                "reasoning={} is not supported by {}.",
                display(reasoning),
                display(field(item, "id"))
            ),
        ));
    }
    Ok(())
}

fn supports_reasoning(item: &JsValue, reasoning: &JsValue) -> bool {
    matches!(field(field(field(item, "capabilities"), "reasoning"), "values"),
        JsValue::Array(values) if values.contains(reasoning))
}

fn normalize_selection(selection: &JsValue) -> JsValue {
    if is(field(selection, "mode"), "auto") {
        return object([("mode", "auto".into())]);
    }
    if !is(field(selection, "mode"), "model") {
        return selection.clone();
    }
    object([
        ("mode", "model".into()),
        ("provider", field(selection, "provider").clone()),
        ("model", field(selection, "model").clone()),
        ("reasoning", field(selection, "reasoning").clone()),
        ("speed", field(selection, "speed").clone()),
    ])
}

fn restore_selection(catalog: &JsValue, selection: JsValue) -> JsValue {
    if matches!(selection, JsValue::Undefined | JsValue::Null) {
        return JsValue::Undefined;
    }
    let mut selection = normalize_selection(&selection);
    if is(field(&selection, "mode"), "model")
        && !field(&selection, "reasoning").is_undefined()
        && !catalog_item(catalog, &selection)
            .is_some_and(|item| supports_reasoning(item, field(&selection, "reasoning")))
        && let JsValue::Object(value) = &mut selection
    {
        value.insert("reasoning".into(), JsValue::Undefined);
    }
    selection
}

struct TranscriptReservation<'a> {
    replacement: &'a ReplacementCoordinator,
    session: &'a JsString,
}
impl Drop for TranscriptReservation<'_> {
    fn drop(&mut self) {
        self.replacement.release_transcript_write(self.session);
    }
}

fn session_key(params: &JsValue) -> Result<JsString, RequestError> {
    let value = field(params, "sessionKey");
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));
    }
    let key = value
        .as_string()
        .ok_or_else(|| RequestError::new("input.sessionKey?.trim is not a function"))?;
    require_session_key(key)?;
    Ok(key.clone())
}
fn require_session_key(key: &JsString) -> Result<(), RequestError> {
    if key
        .units()
        .iter()
        .all(|unit| char::from_u32(u32::from(*unit)).is_some_and(crate::metadata::js_space))
    {
        Err(error("INVALID_SESSION_KEY", "sessionKey is required."))
    } else {
        Ok(())
    }
}
fn project_key(params: &JsValue) -> Result<JsString, RequestError> {
    string(params, "projectKey")
}
fn string(value: &JsValue, key: &str) -> Result<JsString, RequestError> {
    field(value, key)
        .as_string()
        .cloned()
        .ok_or_else(|| error("INVALID_REQUEST", format!("{key} must be a string.")))
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
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
fn display(value: &JsValue) -> String {
    crate::js_value::display(value).unwrap_or_default()
}
fn error(code: &str, message: impl Into<JsString>) -> RequestError {
    RequestError {
        code: code.into(),
        ..RequestError::new(message)
    }
}
fn sdk_error(failure: SdkSessionError) -> RequestError {
    error(
        if failure.code.is_empty() {
            "gateway_request_failed"
        } else {
            &failure.code
        },
        failure.message,
    )
}
fn storage_error(failure: AdapterError) -> RequestError {
    match failure {
        AdapterError::Validation { code, message } => error(code, message),
        failure => RequestError::new(failure.to_string()),
    }
}
fn unknown_project(project: &JsString) -> RequestError {
    let mut message = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
    message.extend(project.units());
    error("PROJECT_NOT_FOUND", JsString::from_utf16(message))
}

fn iso_time(milliseconds: f64) -> Result<String, RequestError> {
    if !milliseconds.is_finite() || milliseconds.abs() > 8_640_000_000_000_000.0 {
        return Err(RequestError::new("Invalid time value"));
    }
    Ok(crate::runtime::context::prompt::iso_timestamp(milliseconds))
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
