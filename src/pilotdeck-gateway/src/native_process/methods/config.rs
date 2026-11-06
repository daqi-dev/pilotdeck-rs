//! S5 session configuration: the seven #1457 methods wired to the native
//! runtime. Gate orders mirror the TS chain — the InProcessGateway wire gates
//! first, then the createLocalGateway / ProjectRuntimeRegistry callback
//! behavior. `session_model_set` / `session_model_clear` wrap their callback
//! in the replacement coordinator's transcript-write reservation
//! (InProcessGateway.ts:2193,2203) and append the `model-selection` /
//! `model-selection-clear` transcript entries through the session storage
//! (NativeSessionModelSelectionPort.write/clear). `output_styles_list` reads
//! the project runtime's plugin styles. The three selection methods
//! (`set_permission_mode`, `set_session_thinking`, `set_output_style`) keep
//! the TS gate order and payload validation verbatim, then answer
//! `CAPABILITY_UNAVAILABLE`: their TS callbacks write the per-gateway
//! `registry` (`sessionOverrides`, `sdkThinkingOverrides`,
//! `sdkSessionConfigs.outputStyle`), a registry the native process never
//! instantiates, so a local store here would be write-only and every
//! `applied: true` a false positive (#1457 fix-r1 P1; deviation registered as
//! A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R4). `seed_read_state` routes
//! through the owner's maintenance fence onto the live session's tool
//! checkpoint.
use super::super::requests::Requests;
use crate::{
    config::{self, Env},
    metadata::{Files, Metadata, object},
    runtime::{
        connection::{Call, RequestError},
        project::{ProjectResources, Resource, RuntimeError, plugins::PluginResources},
        session::SessionContext,
    },
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorageInput},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{path::PathBuf, sync::Arc};

// Wire names from the gateway frames registry. All 7 pass through admission
// as ordinary session requests.
static UNDEFINED: JsValue = JsValue::Undefined;
pub(super) const METHODS: &[&str] = &[
    "session_model_set",
    "session_model_clear",
    "set_permission_mode",
    "set_session_thinking",
    "output_styles_list",
    "set_output_style",
    "seed_read_state",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::session_config_request);

fn gateway_error(code: &str, message: impl Into<JsString>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

/// True when a JS string is non-empty after JS trim — the shared shape of the
/// TS `input.X?.trim()` required-string gates (IPG.ts:2191,2202,2366,2381,
/// 2427). Lone surrogates survive the TS check, so the conversion is lossy:
/// `to_utf8` would reject a key that TS accepts (#1455 probe4, PilotDeck
/// cc7c4e65: `{"label":"thinking-lone-surrogate-key","value":{"applied":"callback"}}`).
/// Session configuration and session directory share this one implementation
/// (#1455 G7); it lives here rather than in `methods/mod.rs` because the
/// sibling #1465/#1316 PRs are rewriting that file's declaration block.
pub(super) fn trimmed_string_is_non_empty(value: &JsString) -> bool {
    !crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()
}

/// The TS `input.sessionKey?.trim()` gate shared by five of the seven
/// methods: a present string that is non-empty after JS trim, else
/// INVALID_SESSION_KEY. A non-null non-string throws the bare TypeError the
/// TS optional-chain call produces on the wire
/// (A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2 ②).
fn require_session_key(params: &JsValue) -> Result<JsString, RequestError> {
    super::require_text(
        params,
        "sessionKey",
        "INVALID_SESSION_KEY",
        "sessionKey is required.",
    )
}

/// `truthy` over the protocol's value kinds, like the JS Boolean context.
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `is` from session_model: exact string-field comparison.
fn is(value: Option<&JsValue>, expected: &str) -> bool {
    value.is_some_and(|v| v.as_string().is_some_and(|s| s.equals_str(expected)))
}

/// JS `String(value)` rendering for interpolated error messages.
fn js_display(value: &JsValue) -> String {
    String::from_utf16_lossy(&crate::js_value::to_string_units(value).unwrap_or_default())
}

fn config_error(error: config::Error) -> RequestError {
    RequestError {
        code: error.code,
        message: error.message,
        details: None,
        validation: None,
    }
}

/// A RuntimeError keeps its JS error code when it carries one; the fallback
/// is the bare-Error wire shape.
fn runtime_error(error: RuntimeError) -> RequestError {
    match error.js_error() {
        Some(js_error) => RequestError {
            code: js_error
                .code
                .clone()
                .unwrap_or_else(|| "gateway_request_failed".into()),
            message: js_error.message.clone(),
            details: None,
            validation: None,
        },
        None => RequestError::new(error.message),
    }
}

fn replacement_error(error: crate::runtime::replacement::ReplacementError) -> RequestError {
    RequestError {
        code: error.code.unwrap_or("gateway_request_failed").into(),
        message: error.message.into(),
        details: None,
        validation: None,
    }
}

/// Resolves `projectKey` from params, falling back to the configured project.
/// The session_dir slice owns the family's copy of this resolution; a local
/// one keeps the config family inside its sanctioned file set.
fn resolve_project(requests: &Requests, params: &JsValue) -> Result<String, RequestError> {
    let project = params
        .get("projectKey")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| requests.options.project.to_string_lossy().into_owned());
    if !requests.options.serves_project(&project) {
        return Err(gateway_error(
            "CAPABILITY_UNAVAILABLE",
            "The isolated native process serves only its configured project.",
        ));
    }
    Ok(project)
}

/// The metadata face the model catalog validates against, like the services
/// slice's model-selection resolution.
fn native_metadata(home: &std::path::Path) -> Metadata<crate::metadata::NativeFiles> {
    Metadata {
        home: home.to_path_buf(),
        cwd: PathBuf::new(),
        files: crate::metadata::NativeFiles,
    }
}

/// #1457 fix-r1 (P1): the TS callbacks store their selection on the
/// per-gateway `registry` (`sessionOverrides` / `sdkThinkingOverrides` /
/// `sdkSessionConfigs.outputStyle`) and the next session reads it back. The
/// native process has no such registry instance — `SdkSessionRegistry` is a
/// ported module with no production binding — so a store here would be
/// written and never read. A validated request answers honestly instead of
/// claiming `applied: true`.
fn session_config_unavailable(method: &str) -> RequestError {
    gateway_error(
        "CAPABILITY_UNAVAILABLE",
        format!("Native gateway method {method} is not configured."),
    )
}

impl Requests {
    pub(in crate::native_process) fn session_config_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "session_model_set" => self.session_model_set(&params).await,
                "session_model_clear" => self.session_model_clear(&params).await,
                "set_permission_mode" => self.set_permission_mode(&params).await,
                "set_session_thinking" => self.set_session_thinking(&params).await,
                "output_styles_list" => self.output_styles_list(&params).await,
                "set_output_style" => self.set_output_style(&params).await,
                "seed_read_state" => self.seed_read_state(&params).await,
                _ => unreachable!("config family checked against METHODS"),
            }
        })
    }

    /// InProcessGateway.ts:2191-2199: the transcript-write reservation wraps
    /// the whole callback, released on every exit.
    async fn session_model_set(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let reserved = params.get("sessionKey").and_then(JsValue::as_string);
        if let Some(key) = &reserved
            && let Err(error) = self
                .replacement
                .reserve_transcript_write(key, "change the session model")
        {
            return Err(replacement_error(error));
        }
        let result = self.session_model_set_reserved(params).await;
        if let Some(key) = &reserved {
            self.replacement.release_transcript_write(key);
        }
        result
    }

    /// GatewaySessionModelBundle.set: resolveProjectKey → hasActiveTurn →
    /// selection shape → validateSelection → normalizeSelection → port write
    /// (sessionKey gate) → router.close → result.
    async fn session_model_set_reserved(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let project_key = resolve_project(self, params)?;
        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)
            && self.owner.has_active_turn(key)
        {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot change the model during an active turn.",
            ));
        }
        let selection = params.get("selection").unwrap_or(&UNDEFINED);
        if !matches!(selection, JsValue::Object(_) | JsValue::Array(_)) {
            return Err(gateway_error(
                "INVALID_MODEL_OVERRIDE",
                "selection is required.",
            ));
        }
        let metadata = native_metadata(&self.options.home);
        validate_selection(selection, &metadata, &self.env)?;
        let selection = normalize_selection(selection);
        let session_key = require_session_key(params)?;
        self.record_model_selection(&session_key, Some(&selection), &project_key)
            .await?;
        self.owner
            .close(&session_key)
            .await
            .map_err(runtime_error)?;
        model_result(&project_key, &session_key, &selection, &metadata, &self.env)
    }

    /// InProcessGateway.ts:2201-2209: the clear reservation mirrors set's.
    async fn session_model_clear(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let reserved = params.get("sessionKey").and_then(JsValue::as_string);
        if let Some(key) = &reserved
            && let Err(error) = self
                .replacement
                .reserve_transcript_write(key, "clear the session model")
        {
            return Err(replacement_error(error));
        }
        let result = self.session_model_clear_reserved(params).await;
        if let Some(key) = &reserved {
            self.replacement.release_transcript_write(key);
        }
        result
    }

    /// GatewaySessionModelBundle.clear: resolveProjectKey → requireSessionKey
    /// → hasActiveTurn → port clear → router.close → void.
    async fn session_model_clear_reserved(
        &self,
        params: &JsValue,
    ) -> Result<JsValue, RequestError> {
        let project_key = resolve_project(self, params)?;
        let session_key = require_session_key(params)?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot clear the model during an active turn.",
            ));
        }
        self.record_model_selection(&session_key, None, &project_key)
            .await?;
        self.owner
            .close(&session_key)
            .await
            .map_err(runtime_error)?;
        Ok(JsValue::Undefined)
    }

    /// NativeSessionModelSelectionPort.write / clear: append the
    /// `model-selection` / `model-selection-clear` session_metadata entry,
    /// continuing the transcript's sequence like the rename/tag path.
    async fn record_model_selection(
        &self,
        session_key: &JsString,
        selection: Option<&JsValue>,
        project_root: &str,
    ) -> Result<(), RequestError> {
        // #1457 fix-r1: one writer at a time per session — this path numbers its
        // append from the tail it just read, so two concurrent cold writers
        // would both claim `sequence + 1` and brick the transcript.
        let write_fence = super::transcript_locks::acquire(project_root, session_key).await;
        // #1457 fix-r2: when the session is loaded its recorder owns the writer
        // (and the writer lease) with an in-memory sequence, so the entry is
        // appended *through* it. Closing it first aborted an admitted turn and
        // left a second writer numbering from the same tail; routing through the
        // one writer does neither. With no loaded session the fence is the only
        // writer, and a session created while we hold it restores the tail after
        // this append.
        let live = super::transcript_locks::live_recorder(&self.recorders, session_key);
        let home = self.options.home.clone();
        let key = session_key.clone();
        let mut metadata = pilotdeck_protocol::JsObject::new();
        metadata.insert(
            "modelSelection".into(),
            selection.cloned().unwrap_or(JsValue::Null),
        );
        let updated_at = pilotdeck_transport::system_clock::format_epoch_iso(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        );
        metadata.insert("updatedAt".into(), JsValue::String(updated_at.into()));
        let turn_id = if selection.is_some() {
            "model-selection"
        } else {
            "model-selection-clear"
        };
        let project_root: PathBuf = project_root.into();
        tokio::task::spawn_blocking(move || {
            // The fence outlives a cancelled request: dropping this future does
            // not stop a blocking closure that already started, so the guard
            // travels with the write instead of dropping at the await.
            let _write = write_fence;
            let turn = JsString::from(turn_id);
            let entry = JsValue::Object(metadata);
            if let Some(recorder) = live {
                return recorder
                    .writer()
                    .record_session_metadata(&key, &turn, entry)
                    .map_err(|e| RequestError::new(e.to_string()));
            }
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_root.clone(),
                    pilot_home: home.clone(),
                    session_id: key.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            if matches!(storage.transcript_exists(), Some(Ok(true))) {
                storage
                    .restore()
                    .map_err(|e| RequestError::new(e.to_string()))?;
            }
            storage
                .transcript
                .record_session_metadata(&key, &turn, entry)
                .map_err(|e| RequestError::new(e.to_string()))
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(())
    }

    /// InProcessGateway.ts:2321-2329 + createLocalGateway.ts:1371-1382: the
    /// mode gate, then the busy gate on the raw key (a blank key can hold a
    /// reserved turn), then the callback's sessionKey gate. The
    /// sessionOverrides spread has no native counterpart
    /// ([`session_config_unavailable`]).
    async fn set_permission_mode(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let mode = params.get("mode").unwrap_or(&UNDEFINED);
        let mode_ok = mode.as_string().is_some_and(|m| {
            crate::runtime::permission::PERMISSION_MODES
                .iter()
                .any(|allowed| m.equals_str(allowed))
        });
        if !mode_ok {
            let display = js_display(mode);
            return Err(gateway_error(
                "INVALID_PERMISSION_MODE",
                format!("Unsupported permission mode: {display}"),
            ));
        }
        if let Some(JsValue::String(key)) = params.get("sessionKey")
            && self.owner.has_active_turn(key)
        {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot change permission mode during an active turn.",
            ));
        }
        require_session_key(params)?;
        Err(session_config_unavailable("set_permission_mode"))
    }

    /// InProcessGateway.ts:2364-2372: sessionKey → busy →
    /// validateThinkingConfig. The callback's thinking override lives on the
    /// native-absent registry ([`session_config_unavailable`]), so the store
    /// and the `close` stop here.
    async fn set_session_thinking(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = require_session_key(params)?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot change thinking while a turn is active.",
            ));
        }
        validate_thinking(params.get("thinking").unwrap_or(&UNDEFINED))?;
        Err(session_config_unavailable("set_session_thinking"))
    }

    /// InProcessGateway.ts:2374-2377 has no params gate;
    /// ProjectRuntimeRegistry.outputStylesListForSdk refreshes the plugin
    /// runtime and lists the project's (plus session's) style contributions.
    async fn output_styles_list(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let _project = resolve_project(self, params)?;
        let styles = self.plugin_styles().await?;
        let mut style_values = Vec::with_capacity(styles.len());
        for style in &styles {
            let mut entry = pilotdeck_protocol::JsObject::new();
            entry.insert("name".into(), JsValue::String(style.name.as_str().into()));
            if let Some(description) = &style.description {
                entry.insert(
                    "description".into(),
                    JsValue::String(description.as_str().into()),
                );
            }
            if !style.plugin.is_empty() {
                entry.insert(
                    "plugin".into(),
                    JsValue::String(style.plugin.as_str().into()),
                );
            }
            entry.insert("source".into(), style.source.as_str().into());
            style_values.push(JsValue::Object(entry));
        }
        let mut result = pilotdeck_protocol::JsObject::new();
        result.insert("styles".into(), JsValue::Array(style_values));
        // No `selected`: the only writer (`set_output_style`) answers
        // [`session_config_unavailable`], so the project view is the whole
        // answer here.
        Ok(JsValue::Object(result))
    }

    /// InProcessGateway.ts:2379-2385: sessionKey → busy → name (the TypeError
    /// shapes preserved, A7-W5-...-001-R2 ②), then the callback validates the
    /// style against the refreshed view, stores the selection and closes the
    /// session with `sdk_output_style_changed`.
    async fn set_output_style(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = require_session_key(params)?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot change output style during an active turn.",
            ));
        }
        let name = match params.get("name") {
            None | Some(JsValue::Undefined) => {
                return Err(gateway_error(
                    "gateway_request_failed",
                    "Cannot read properties of undefined (reading 'trim')",
                ));
            }
            Some(JsValue::Null) => None,
            Some(JsValue::String(name)) if trimmed_string_is_non_empty(name) => Some(name.clone()),
            Some(JsValue::String(_)) => {
                return Err(gateway_error(
                    "INVALID_OUTPUT_STYLE",
                    "name must be non-empty or null.",
                ));
            }
            Some(_) => {
                return Err(gateway_error(
                    "gateway_request_failed",
                    "input.name.trim is not a function",
                ));
            }
        };
        if let Some(name) = &name {
            let styles = self.plugin_styles().await?;
            if !styles
                .iter()
                .any(|style| JsString::from(style.name.as_str()) == *name)
            {
                let mut units: Vec<u16> = "Unknown output style: ".encode_utf16().collect();
                units.extend_from_slice(name.units());
                return Err(gateway_error(
                    "OUTPUT_STYLE_NOT_FOUND",
                    JsString::from_utf16(units),
                ));
            }
        }
        Err(session_config_unavailable("set_output_style"))
    }

    /// The plugin runtime's output styles, after the refresh the TS callback
    /// performs. The lease stays held across the await so a concurrent reload
    /// cannot retire the runtime mid-refresh.
    async fn plugin_styles(
        &self,
    ) -> Result<Vec<crate::runtime::plugins::PluginOutputStyleContribution>, RequestError> {
        // Acquire may read configuration synchronously (management.rs:118-125),
        // so the blocking job stays off the async executor.
        let project = self.project.clone();
        let lease = tokio::task::spawn_blocking(move || project.acquire(None))
            .await
            .map_err(|e| RequestError::new(e.to_string()))?
            .map_err(runtime_error)?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| RequestError::new("Native project resources are missing."))?;
        let plugin_resource = resources
            .context()
            .resource(Resource::PluginRuntime)
            .ok_or_else(|| RequestError::new("Native plugin runtime resource is missing."))?;
        let plugin_runtime = plugin_resource
            .as_any()
            .downcast_ref::<PluginResources>()
            .ok_or_else(|| RequestError::new("Native plugin runtime has the wrong type."))?
            .runtime
            .clone();
        plugin_runtime
            .refresh()
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        Ok(plugin_runtime
            .create_view(vec![])
            .await
            .list_output_styles())
    }

    /// InProcessGateway.ts:2423-2442: sessionKey → path → mtime → busy; the
    /// native process has no setSessionCwd callback (the `workspaceCwd`
    /// field is silently skipped) and no setSdkSessionConfig, so a truthy
    /// `sdkSessionConfig` answers the unavailable shape. The router fence
    /// (SessionRouter.ts:260-278) then seeds onto the live session.
    async fn seed_read_state(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = require_session_key(params)?;
        let path = super::require_text(params, "path", "INVALID_FILE_PATH", "path is required.")?;
        let mtime = params
            .get("mtime")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= 0.0)
            .ok_or_else(|| {
                gateway_error(
                    "INVALID_FILE_MTIME",
                    "mtime must be a non-negative integer in milliseconds.",
                )
            })?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot seed file read state while a turn is active.",
            ));
        }
        if params.get("sdkSessionConfig").is_some_and(truthy) {
            return Err(gateway_error(
                "CAPABILITY_UNAVAILABLE",
                "sdk_session_config is unavailable.",
            ));
        }
        let reservation = self
            .owner
            .begin_maintenance(&session_key)
            .map_err(runtime_error)?
            .ok_or_else(|| {
                RequestError::new(
                    "Cannot seed file read state while a turn or session control is active.",
                )
            })?;
        let context = SessionContext {
            session_key: session_key.clone(),
            project_key: params
                .get("projectKey")
                .and_then(JsValue::as_string)
                .cloned(),
            channel_key: params
                .get("channelKey")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "api_server".into()),
            allowed_tools: None,
            disallowed_tools: None,
        };
        let result = async {
            let handle = self
                .owner
                .get_or_create(context)
                .await
                .map_err(runtime_error)?;
            if self.owner.has_active_turn(&session_key) {
                return Err(RequestError::new(
                    "Cannot seed file read state while a turn is active.",
                ));
            }
            let applied = handle
                .seed_read_state(&path, mtime)
                .map_err(runtime_error)?;
            Ok(object([("applied", JsValue::Bool(applied))]))
        }
        .await;
        drop(reservation);
        result
    }
}

/// InProcessGateway.ts:3162-3177 `validateThinkingConfig`.
fn validate_thinking(thinking: &JsValue) -> Result<(), RequestError> {
    if matches!(thinking, JsValue::Null) {
        return Ok(());
    }
    let is_object = truthy(thinking) && matches!(thinking, JsValue::Object(_) | JsValue::Array(_));
    if !is_object || thinking.get("enabled").and_then(JsValue::as_bool).is_none() {
        return Err(gateway_error(
            "INVALID_THINKING_CONFIG",
            "thinking must be null or an object with an enabled boolean.",
        ));
    }
    const THINKING_MODES: [&str; 8] = [
        "default", "off", "minimal", "low", "medium", "high", "xhigh", "max",
    ];
    if let Some(mode) = thinking.get("mode")
        && !mode.is_undefined()
        && !mode
            .as_string()
            .is_some_and(|m| THINKING_MODES.iter().any(|allowed| m.equals_str(allowed)))
    {
        let display = js_display(mode);
        return Err(gateway_error(
            "INVALID_THINKING_CONFIG",
            format!("Unsupported thinking mode: {display}."),
        ));
    }
    if let Some(budget) = thinking.get("budgetTokens")
        && !budget.is_undefined()
        && !budget
            .as_number()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= 0.0)
    {
        return Err(gateway_error(
            "INVALID_THINKING_CONFIG",
            "budgetTokens must be a non-negative integer.",
        ));
    }
    Ok(())
}

/// modelCatalog.ts:58-84 `validateModelSelection` over the native catalog.
fn validate_selection<F: Files>(
    selection: &JsValue,
    metadata: &Metadata<F>,
    env: &Env,
) -> Result<(), RequestError> {
    if is(selection.get("mode"), "auto") {
        let catalog =
            crate::model_catalog::execute(metadata, &object([]), env).map_err(config_error)?;
        let available = catalog
            .get("router")
            .and_then(|router| router.get("autoAvailable"))
            == Some(&JsValue::Bool(true));
        if !available {
            return Err(gateway_error(
                "ROUTER_AUTO_UNAVAILABLE",
                "Router auto is not available for this project.",
            ));
        }
        return Ok(());
    }
    let provider = selection.get("provider").unwrap_or(&UNDEFINED);
    let model = selection.get("model").unwrap_or(&UNDEFINED);
    if !is(selection.get("mode"), "model")
        || provider.as_string().is_none()
        || model.as_string().is_none()
    {
        return Err(gateway_error(
            "INVALID_MODEL_OVERRIDE",
            "A model selection requires mode, provider, and model.",
        ));
    }
    let catalog =
        crate::model_catalog::execute(metadata, &object([]), env).map_err(config_error)?;
    let item = match catalog.get("items") {
        Some(JsValue::Array(items)) => items
            .iter()
            .find(|m| m.get("provider") == Some(provider) && m.get("model") == Some(model)),
        _ => None,
    };
    let Some(item) = item else {
        return Err(gateway_error(
            "INVALID_MODEL_OVERRIDE",
            unavailable_message(provider.as_string().unwrap(), model.as_string().unwrap()),
        ));
    };
    if item.get("available").and_then(JsValue::as_bool) != Some(true) {
        return Err(gateway_error(
            "INVALID_MODEL_OVERRIDE",
            unavailable_message(provider.as_string().unwrap(), model.as_string().unwrap()),
        ));
    }
    let id = item.get("id").and_then(JsValue::as_string);
    let speed = selection.get("speed").unwrap_or(&UNDEFINED);
    if !speed.is_undefined() {
        if !speed
            .as_number()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
        {
            return Err(gateway_error(
                "UNSUPPORTED_MODEL_PARAMETER",
                "speed must be between 0 and 1.",
            ));
        }
        if item
            .get("capabilities")
            .and_then(|c| c.get("speed"))
            .is_none()
        {
            return Err(gateway_error(
                "UNSUPPORTED_MODEL_PARAMETER",
                format!(
                    "speed is not supported by {}.",
                    id.map(|s| String::from_utf16_lossy(s.units()))
                        .unwrap_or_default()
                ),
            ));
        }
    }
    let reasoning = selection.get("reasoning").unwrap_or(&UNDEFINED);
    if !reasoning.is_undefined() {
        let supported = item
            .get("capabilities")
            .and_then(|c| c.get("reasoning"))
            .and_then(|r| r.get("values"))
            .is_some_and(|values| match values {
                JsValue::Array(list) => list.contains(reasoning),
                _ => false,
            });
        if !supported {
            return Err(gateway_error(
                "UNSUPPORTED_MODEL_PARAMETER",
                format!(
                    "reasoning={} is not supported by {}.",
                    js_display(reasoning),
                    id.map(|s| String::from_utf16_lossy(s.units()))
                        .unwrap_or_default()
                ),
            ));
        }
    }
    Ok(())
}

/// `Model is unavailable: ${provider}/${model}` in exact UTF-16.
fn unavailable_message(provider: &JsString, model: &JsString) -> JsString {
    let mut units: Vec<u16> = "Model is unavailable: ".encode_utf16().collect();
    units.extend_from_slice(provider.units());
    units.push('/' as u16);
    units.extend_from_slice(model.units());
    JsString::from_utf16(units)
}

/// modelCatalog.ts:92-100 `normalizeSessionModelSelection`.
fn normalize_selection(selection: &JsValue) -> JsValue {
    if is(selection.get("mode"), "auto") {
        return object([("mode", "auto".into())]);
    }
    if !is(selection.get("mode"), "model") {
        return selection.clone();
    }
    let mut normalized = pilotdeck_protocol::JsObject::new();
    normalized.insert("mode".into(), "model".into());
    normalized.insert(
        "provider".into(),
        selection
            .get("provider")
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    normalized.insert(
        "model".into(),
        selection
            .get("model")
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    if let Some(reasoning) = selection.get("reasoning") {
        normalized.insert("reasoning".into(), reasoning.clone());
    }
    if let Some(speed) = selection.get("speed") {
        normalized.insert("speed".into(), speed.clone());
    }
    JsValue::Object(normalized)
}

/// GatewaySessionModelBundle.result: `saved` plus the effective selection —
/// the session override for explicit selections, the project default
/// otherwise.
fn model_result<F: Files>(
    project_key: &str,
    session_key: &JsString,
    saved: &JsValue,
    metadata: &Metadata<F>,
    env: &Env,
) -> Result<JsValue, RequestError> {
    let effective = if is(saved.get("mode"), "model") {
        object([
            (
                "provider",
                saved.get("provider").cloned().unwrap_or(JsValue::Undefined),
            ),
            (
                "model",
                saved.get("model").cloned().unwrap_or(JsValue::Undefined),
            ),
            ("source", "session".into()),
            (
                "reasoning",
                saved
                    .get("reasoning")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            ),
            (
                "speed",
                saved.get("speed").cloned().unwrap_or(JsValue::Undefined),
            ),
        ])
    } else {
        let config = config::load_project_with_files(metadata, env, Some(project_key))
            .map_err(config_error)?;
        object([
            ("provider", JsValue::String(config.selection.0)),
            ("model", JsValue::String(config.selection.1)),
            (
                "source",
                if config.router_enabled {
                    "router"
                } else {
                    "default"
                }
                .into(),
            ),
        ])
    };
    Ok(object([
        ("projectKey", project_key.into()),
        ("sessionKey", JsValue::String(session_key.clone())),
        ("saved", saved.clone()),
        ("effective", effective),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{negative_assert, negative_assert_eq};

    fn setup_fixture() -> (crate::native_process::management_tests::Scratch, Requests) {
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1457 config fixture")
    }

    fn selection(mode: &str, provider: &str, model: &str) -> JsValue {
        object([
            ("mode", mode.into()),
            ("provider", provider.into()),
            ("model", model.into()),
        ])
    }

    fn error_text(error: &RequestError) -> String {
        String::from_utf16_lossy(error.message.units())
    }

    fn array_field(value: &JsValue, key: &str) -> Vec<JsValue> {
        match value.get(key) {
            Some(JsValue::Array(items)) => items.clone(),
            other => panic!("#1457 expected array field {key}, got {other:?}"),
        }
    }

    /// Parses the session transcript, one JSON value per non-empty line. The
    /// line split is `split("\n")`, not `splitlines()`: the JSONL store's own
    /// framing is LF and the shared convention keeps a stray CR visible.
    fn transcript_entries(r: &Requests, session: &str) -> Vec<serde_json::Value> {
        let chat = crate::session_chat_dir(
            &r.options.home,
            &r.options.project,
            &r.options.project.to_string_lossy(),
        );
        let text =
            std::fs::read_to_string(chat.join(format!("{session}.jsonl"))).unwrap_or_default();
        text.split("\n")
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("#1457 transcript JSON"))
            .collect()
    }

    async fn teardown(r: &Requests) {
        r.owner.shutdown().await.expect("#1457 shutdown");
        r.project.dispose().await.expect("#1457 dispose");
    }

    // ─────────────────────────────────────────────── session_model_set / clear

    #[tokio::test]
    async fn session_model_set_saves_selection_and_appends_transcript() {
        let (_scratch, r) = setup_fixture();
        let params = object([
            ("sessionKey", "cfg-set-1".into()),
            ("selection", selection("model", "test", "test")),
        ]);
        let result = r
            .session_config_request("session_model_set".into(), params)
            .await
            .expect("#1457 session_model_set accepts the fixture selection");
        negative_assert_eq!(
            result.get("sessionKey").and_then(JsValue::as_string),
            Some(&JsString::from("cfg-set-1")),
            "#1457 session_model_set echoes the sessionKey"
        );
        negative_assert_eq!(
            result
                .get("saved")
                .and_then(|saved| saved.get("mode"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("model")),
            "#1457 session_model_set saves the normalized selection"
        );
        let effective = result.get("effective").expect("#1457 effective");
        negative_assert_eq!(
            effective.get("provider").and_then(JsValue::as_string),
            Some(&JsString::from("test")),
            "#1457 effective provider is the session override"
        );
        negative_assert_eq!(
            effective.get("source").and_then(JsValue::as_string),
            Some(&JsString::from("session")),
            "#1457 effective source is the session override"
        );
        let entries = transcript_entries(&r, "cfg-set-1");
        let recorded = entries
            .iter()
            .find(|entry| entry.get("turnId").and_then(|t| t.as_str()) == Some("model-selection"))
            .expect("#1457 transcript model-selection entry");
        negative_assert_eq!(
            recorded["metadata"]["modelSelection"]["model"].as_str(),
            Some("test"),
            "#1457 transcript entry carries the model selection"
        );
        negative_assert_eq!(
            recorded["type"].as_str(),
            Some("session_metadata"),
            "#1457 transcript entry is session metadata"
        );
        teardown(&r).await;
    }

    #[tokio::test]
    async fn session_model_set_gate_order_matches_the_ts_chain() {
        let (_scratch, r) = setup_fixture();
        for (label, params, code, message) in [
            (
                "missing selection",
                object([("sessionKey", "cfg-set-2".into())]),
                "INVALID_MODEL_OVERRIDE",
                "selection is required.",
            ),
            (
                "selection without mode fields",
                object([
                    ("sessionKey", "cfg-set-2".into()),
                    ("selection", object([("mode", "model".into())])),
                ]),
                "INVALID_MODEL_OVERRIDE",
                "A model selection requires mode, provider, and model.",
            ),
            (
                "auto without router",
                object([
                    ("sessionKey", "cfg-set-2".into()),
                    ("selection", object([("mode", "auto".into())])),
                ]),
                "ROUTER_AUTO_UNAVAILABLE",
                "Router auto is not available for this project.",
            ),
            (
                "unknown provider",
                object([
                    ("sessionKey", "cfg-set-2".into()),
                    ("selection", selection("model", "nope", "x")),
                ]),
                "INVALID_MODEL_OVERRIDE",
                "Model is unavailable: nope/x",
            ),
            (
                "selection shape gate runs before the sessionKey gate",
                object([("selection", JsValue::Number(42.0))]),
                "INVALID_MODEL_OVERRIDE",
                "selection is required.",
            ),
            (
                "missing sessionKey",
                object([("selection", selection("model", "test", "test"))]),
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            ),
        ] {
            let error = r
                .session_config_request("session_model_set".into(), params)
                .await
                .expect_err("#1457 session_model_set rejects {label}");
            negative_assert_eq!(error.code.as_str(), code, "#1457 {label} code");
            negative_assert_eq!(error_text(&error), message, "#1457 {label} message");
        }
        teardown(&r).await;
    }

    #[tokio::test]
    async fn session_model_set_rejects_active_turn() {
        let (_scratch, r) = setup_fixture();
        let session = "cfg-set-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "session_model_set".into(),
                object([
                    ("sessionKey", session.as_str().into()),
                    ("selection", selection("model", "test", "test")),
                ]),
            )
            .await
            .expect_err("#1457 session_model_set is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot change the model during an active turn.",
            "#1457 busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    #[tokio::test]
    async fn session_model_clear_appends_transcript_and_returns_null() {
        let (_scratch, r) = setup_fixture();
        let set = object([
            ("sessionKey", "cfg-clear-1".into()),
            ("selection", selection("model", "test", "test")),
        ]);
        r.session_config_request("session_model_set".into(), set)
            .await
            .expect("#1457 seed the session selection first");
        let cleared = r
            .session_config_request(
                "session_model_clear".into(),
                object([("sessionKey", "cfg-clear-1".into())]),
            )
            .await
            .expect("#1457 session_model_clear succeeds");
        negative_assert!(
            matches!(cleared, JsValue::Undefined),
            "#1457 session_model_clear returns nothing on the wire"
        );
        let entries = transcript_entries(&r, "cfg-clear-1");
        let recorded = entries
            .iter()
            .find(|entry| {
                entry.get("turnId").and_then(|t| t.as_str()) == Some("model-selection-clear")
            })
            .expect("#1457 transcript model-selection-clear entry");
        negative_assert_eq!(
            recorded["metadata"].get("modelSelection"),
            Some(&serde_json::Value::Null),
            "#1457 clear entry nulls the model selection"
        );
        teardown(&r).await;
    }

    #[tokio::test]
    async fn session_model_clear_gates_key_and_busy() {
        let (_scratch, r) = setup_fixture();
        let error = r
            .session_config_request("session_model_clear".into(), object([]))
            .await
            .expect_err("#1457 session_model_clear needs a key");
        negative_assert_eq!(
            error.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1457 clear key gate"
        );
        let session = "cfg-clear-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "session_model_clear".into(),
                object([("sessionKey", session.as_str().into())]),
            )
            .await
            .expect_err("#1457 session_model_clear is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 clear busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot clear the model during an active turn.",
            "#1457 clear busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── set_permission_mode

    #[tokio::test]
    async fn set_permission_mode_validates_then_reports_unavailable() {
        let (_scratch, r) = setup_fixture();
        // fix-r1 P1: every TS mode passes the wire gates, then the request
        // answers honestly — nothing reads a local override (#1457-R4).
        for mode in crate::runtime::permission::PERMISSION_MODES {
            let error = r
                .session_config_request(
                    "set_permission_mode".into(),
                    object([
                        ("sessionKey", "cfg-pm-modes".into()),
                        ("mode", (*mode).into()),
                    ]),
                )
                .await
                .expect_err("#1457 set_permission_mode is not wired");
            negative_assert_eq!(
                error.code.as_str(),
                "CAPABILITY_UNAVAILABLE",
                "#1457 set_permission_mode unavailable code"
            );
            negative_assert_eq!(
                error_text(&error),
                "Native gateway method set_permission_mode is not configured.",
                "#1457 set_permission_mode unavailable message"
            );
        }
        for (label, params, code, message) in [
            (
                "unknown mode",
                object([("sessionKey", "cfg-pm-1".into()), ("mode", "admin".into())]),
                "INVALID_PERMISSION_MODE",
                "Unsupported permission mode: admin",
            ),
            (
                "absent mode renders like String(undefined)",
                object([("sessionKey", "cfg-pm-1".into())]),
                "INVALID_PERMISSION_MODE",
                "Unsupported permission mode: undefined",
            ),
            (
                "non-string mode renders like String(42)",
                object([
                    ("sessionKey", "cfg-pm-1".into()),
                    ("mode", JsValue::Number(42.0)),
                ]),
                "INVALID_PERMISSION_MODE",
                "Unsupported permission mode: 42",
            ),
            (
                // The real TS stack gates the key (createLocalGateway.ts:1375);
                // R3 corrects R2 ①'s injected-callback finding.
                "missing sessionKey",
                object([("mode", "plan".into())]),
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            ),
        ] {
            let error = r
                .session_config_request("set_permission_mode".into(), params)
                .await
                .expect_err("#1457 set_permission_mode rejects {label}");
            negative_assert_eq!(error.code.as_str(), code, "#1457 {label} code");
            negative_assert_eq!(error_text(&error), message, "#1457 {label} message");
        }
        let session = "cfg-pm-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "set_permission_mode".into(),
                object([
                    ("sessionKey", session.as_str().into()),
                    ("mode", "plan".into()),
                ]),
            )
            .await
            .expect_err("#1457 set_permission_mode is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 pm busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot change permission mode during an active turn.",
            "#1457 pm busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── set_session_thinking

    #[tokio::test]
    async fn set_session_thinking_validates_then_reports_unavailable() {
        let (_scratch, r) = setup_fixture();
        // fix-r1 P1: valid payloads (including the explicit null) pass
        // validate_thinking, then answer honestly (#1457-R4).
        for (label, thinking) in [
            (
                "mode object",
                object([("enabled", JsValue::Bool(true)), ("mode", "high".into())]),
            ),
            ("explicit null", JsValue::Null),
        ] {
            let error = r
                .session_config_request(
                    "set_session_thinking".into(),
                    object([("sessionKey", "cfg-th-1".into()), ("thinking", thinking)]),
                )
                .await
                .expect_err("#1457 set_session_thinking is not wired");
            negative_assert_eq!(
                error.code.as_str(),
                "CAPABILITY_UNAVAILABLE",
                "#1457 thinking unavailable code for {label}"
            );
            negative_assert_eq!(
                error_text(&error),
                "Native gateway method set_session_thinking is not configured.",
                "#1457 thinking unavailable message for {label}"
            );
        }
        for (label, params, code, message) in [
            (
                "non-object thinking",
                object([
                    ("sessionKey", "cfg-th-1".into()),
                    ("thinking", JsValue::Number(42.0)),
                ]),
                "INVALID_THINKING_CONFIG",
                "thinking must be null or an object with an enabled boolean.",
            ),
            (
                "unsupported mode",
                object([
                    ("sessionKey", "cfg-th-1".into()),
                    (
                        "thinking",
                        object([("enabled", JsValue::Bool(true)), ("mode", "turbo".into())]),
                    ),
                ]),
                "INVALID_THINKING_CONFIG",
                "Unsupported thinking mode: turbo.",
            ),
            (
                "negative budget",
                object([
                    ("sessionKey", "cfg-th-1".into()),
                    (
                        "thinking",
                        object([
                            ("enabled", JsValue::Bool(true)),
                            ("budgetTokens", JsValue::Number(-1.0)),
                        ]),
                    ),
                ]),
                "INVALID_THINKING_CONFIG",
                "budgetTokens must be a non-negative integer.",
            ),
            (
                "sessionKey gate runs before thinking validation",
                object([("thinking", JsValue::Number(42.0))]),
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            ),
        ] {
            let error = r
                .session_config_request("set_session_thinking".into(), params)
                .await
                .expect_err("#1457 set_session_thinking rejects {label}");
            negative_assert_eq!(error.code.as_str(), code, "#1457 {label} code");
            negative_assert_eq!(error_text(&error), message, "#1457 {label} message");
        }
        let session = "cfg-th-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "set_session_thinking".into(),
                object([
                    ("sessionKey", session.as_str().into()),
                    ("thinking", object([("enabled", JsValue::Bool(true))])),
                ]),
            )
            .await
            .expect_err("#1457 set_session_thinking is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 th busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot change thinking while a turn is active.",
            "#1457 th busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── output_styles_list / set

    fn write_probe_plugin(r: &Requests) {
        let plugin = r.options.project.join(".pilotdeck/plugins/probe");
        std::fs::create_dir_all(plugin.join("output-styles")).expect("#1457 plugin directory");
        std::fs::write(plugin.join("plugin.json"), "{\"name\":\"probe\"}")
            .expect("#1457 plugin manifest");
        std::fs::write(
            plugin.join("output-styles/compact.md"),
            "---\ndescription: Compact style\n---\n\n# Compact\n",
        )
        .expect("#1457 output style markdown");
    }

    #[tokio::test]
    async fn output_styles_list_reports_project_styles() {
        let (_scratch, r) = setup_fixture();
        let key: JsString = "cfg-style-1".into();
        let before = r
            .session_config_request("output_styles_list".into(), object([]))
            .await
            .expect("#1457 output_styles_list without a plugin");
        let styles = array_field(&before, "styles");
        negative_assert!(
            !styles
                .iter()
                .any(|s| s.get("name").and_then(JsValue::as_string)
                    == Some(&JsString::from("probe:compact"))),
            "#1457 the probe style is absent before the plugin exists"
        );
        write_probe_plugin(&r);
        // fix-r1 P1: a style the project really contributes passes the name
        // gate, then the request answers honestly (#1457-R4).
        let error = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", JsValue::String(key.clone())),
                    ("name", "probe:compact".into()),
                ]),
            )
            .await
            .expect_err("#1457 set_output_style is not wired");
        negative_assert_eq!(
            error.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1457 set_output_style unavailable code"
        );
        negative_assert_eq!(
            error_text(&error),
            "Native gateway method set_output_style is not configured.",
            "#1457 set_output_style unavailable message"
        );
        let listed = r
            .session_config_request(
                "output_styles_list".into(),
                object([("sessionKey", JsValue::String(key.clone()))]),
            )
            .await
            .expect("#1457 output_styles_list with the plugin");
        let style = array_field(&listed, "styles")
            .iter()
            .find(|s| {
                s.get("name").and_then(JsValue::as_string) == Some(&JsString::from("probe:compact"))
            })
            .expect("#1457 probe:compact is listed")
            .clone();
        negative_assert_eq!(
            style.get("description").and_then(JsValue::as_string),
            Some(&JsString::from("Compact style")),
            "#1457 style description comes from the frontmatter"
        );
        negative_assert_eq!(
            style.get("plugin").and_then(JsValue::as_string),
            Some(&JsString::from("probe")),
            "#1457 style plugin is the project plugin"
        );
        negative_assert_eq!(
            style.get("source").and_then(JsValue::as_string),
            Some(&JsString::from("project")),
            "#1457 style source is the project tree"
        );
        negative_assert_eq!(
            listed.get("selected"),
            None,
            "#1457 no selection can be stored, so the list omits selected"
        );
        teardown(&r).await;
    }

    #[tokio::test]
    async fn set_output_style_validates_names_and_key() {
        let (_scratch, r) = setup_fixture();
        write_probe_plugin(&r);
        for (label, params, code, message) in [
            (
                "unknown style",
                object([
                    ("sessionKey", "cfg-style-2".into()),
                    ("name", "nope".into()),
                ]),
                "OUTPUT_STYLE_NOT_FOUND",
                "Unknown output style: nope",
            ),
            (
                "blank name",
                object([("sessionKey", "cfg-style-2".into()), ("name", "  ".into())]),
                "INVALID_OUTPUT_STYLE",
                "name must be non-empty or null.",
            ),
            (
                "non-string name",
                object([
                    ("sessionKey", "cfg-style-2".into()),
                    ("name", JsValue::Number(42.0)),
                ]),
                "gateway_request_failed",
                "input.name.trim is not a function",
            ),
            (
                "absent name",
                object([("sessionKey", "cfg-style-2".into())]),
                "gateway_request_failed",
                "Cannot read properties of undefined (reading 'trim')",
            ),
            (
                "missing sessionKey",
                object([("name", "probe:compact".into())]),
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            ),
        ] {
            let error = r
                .session_config_request("set_output_style".into(), params)
                .await
                .expect_err("#1457 set_output_style rejects {label}");
            negative_assert_eq!(error.code.as_str(), code, "#1457 {label} code");
            negative_assert_eq!(error_text(&error), message, "#1457 {label} message");
        }
        let session = "cfg-style-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", session.as_str().into()),
                    ("name", "probe:compact".into()),
                ]),
            )
            .await
            .expect_err("#1457 set_output_style is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 style busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot change output style during an active turn.",
            "#1457 style busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── seed_read_state

    #[tokio::test]
    async fn seed_read_state_applies_and_validates() {
        let (_scratch, r) = setup_fixture();
        let file = r.options.project.join("seeded.txt");
        std::fs::write(&file, "seeded\n").expect("#1457 seeded file");
        let mtime_ms = std::fs::metadata(&file)
            .expect("#1457 seeded metadata")
            .modified()
            .expect("#1457 seeded mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("#1457 seeded mtime is after the epoch")
            .as_millis() as f64;
        let absolute = file.to_string_lossy().into_owned();
        let applied = r
            .session_config_request(
                "seed_read_state".into(),
                object([
                    ("sessionKey", "cfg-seed-1".into()),
                    ("path", absolute.as_str().into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
            )
            .await
            .expect("#1457 seed_read_state with the exact mtime");
        negative_assert_eq!(
            applied.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1457 exact mtime applies"
        );
        let applied = r
            .session_config_request(
                "seed_read_state".into(),
                object([
                    ("sessionKey", "cfg-seed-2".into()),
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
            )
            .await
            .expect("#1457 seed_read_state with a relative project path");
        negative_assert_eq!(
            applied.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1457 relative in-project path applies"
        );
        for (label, mtime, expected) in [
            ("stale mtime", mtime_ms - 1000.0, false),
            ("zero mtime", 0.0, false),
        ] {
            let result = r
                .session_config_request(
                    "seed_read_state".into(),
                    object([
                        ("sessionKey", "cfg-seed-3".into()),
                        ("path", "seeded.txt".into()),
                        ("mtime", JsValue::Number(mtime)),
                    ]),
                )
                .await
                .expect("#1457 seed_read_state returns the stale outcome");
            negative_assert_eq!(
                result.get("applied"),
                Some(&JsValue::Bool(expected)),
                "#1457 {label} outcome"
            );
        }
        // The TS probe's outside.txt exists on disk: an existing but
        // unregistered outside path is path_not_allowed, a missing one is
        // file_not_found (pathSafety.js allowRegisteredReadFiles branch).
        std::fs::write(
            r.options
                .project
                .parent()
                .expect("project parent")
                .join("outside.txt"),
            "outside\n",
        )
        .expect("#1457 outside file");
        for (label, params, code, message) in [
            (
                "missing file",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("path", "missing.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
                "file_not_found",
                "File missing.txt does not exist.",
            ),
            (
                "outside the workspace",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("path", "../outside.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
                "path_not_allowed",
                "Path ../outside.txt is outside the PilotDeck workspace.",
            ),
            (
                "negative mtime",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(-1.0)),
                ]),
                "INVALID_FILE_MTIME",
                "mtime must be a non-negative integer in milliseconds.",
            ),
            (
                "fractional mtime",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(1.5)),
                ]),
                "INVALID_FILE_MTIME",
                "mtime must be a non-negative integer in milliseconds.",
            ),
            (
                "absent path",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
                "INVALID_FILE_PATH",
                "path is required.",
            ),
            (
                "missing sessionKey",
                object([
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
                "INVALID_SESSION_KEY",
                "sessionKey is required.",
            ),
            (
                "sdkSessionConfig stays unavailable",
                object([
                    ("sessionKey", "cfg-seed-4".into()),
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                    ("sdkSessionConfig", object([("k", "v".into())])),
                ]),
                "CAPABILITY_UNAVAILABLE",
                "sdk_session_config is unavailable.",
            ),
        ] {
            let error = r
                .session_config_request("seed_read_state".into(), params)
                .await
                .expect_err("#1457 seed_read_state rejects {label}");
            negative_assert_eq!(error.code.as_str(), code, "#1457 {label} code");
            negative_assert_eq!(error_text(&error), message, "#1457 {label} message");
        }
        let session = "cfg-seed-busy".to_string();
        let run: JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&session.clone().into(), &run)
            .expect("#1457 reserve active turn");
        let error = r
            .session_config_request(
                "seed_read_state".into(),
                object([
                    ("sessionKey", session.as_str().into()),
                    ("path", "seeded.txt".into()),
                    ("mtime", JsValue::Number(mtime_ms)),
                ]),
            )
            .await
            .expect_err("#1457 seed_read_state is busy");
        negative_assert_eq!(error.code.as_str(), "SESSION_BUSY", "#1457 seed busy code");
        negative_assert_eq!(
            error_text(&error),
            "Cannot seed file read state while a turn is active.",
            "#1457 seed busy message"
        );
        r.owner
            .end_turn(&session.into(), Some(&run), Some(&reservation));
        teardown(&r).await;
    }

    // ─────────────────────────────────────── transcript write serialization

    /// #1457 fix-r1 (P0): `session_model_set` and `rename_session` each build a
    /// fresh `JsonlTranscriptStore`, restore the tail, and append
    /// `sequence + 1`. Racing them on one session must still leave a log the
    /// next `restore()` accepts: strictly increasing sequences, no duplicate.
    #[tokio::test]
    async fn concurrent_transcript_writers_keep_the_chain_loadable() {
        let (_scratch, r) = setup_fixture();
        let session = "cfg-race-1";
        // Eight rounds: a single unlucky ordering must not be able to make the
        // race invisible, and the fences must hold for the whole run.
        for round in 0..8 {
            let set_params = object([
                ("sessionKey", session.into()),
                ("selection", selection("model", "test", "test")),
            ]);
            let rename_params = object([
                ("sessionKey", session.into()),
                ("value", JsValue::String(format!("round-{round}").into())),
            ]);
            let (written, renamed) = tokio::join!(
                r.session_config_request("session_model_set".into(), set_params),
                r.session_dir_request("rename_session".into(), rename_params),
            );
            // The requests succeed only if each append claimed the next number
            // under the fence, so a lost fence shows up here as the store's
            // `sequence_not_increasing` rejection (#1457 fix-r2: the negative
            // controls bind to `#1457 transcript sequences strictly increase`,
            // and this is one of the sites where that invariant breaks first).
            written.expect("#1457 transcript sequences strictly increase (session_model_set)");
            renamed.expect("#1457 transcript sequences strictly increase (rename_session)");
        }
        let entries = transcript_entries(&r, session);
        let sequences: Vec<f64> = entries
            .iter()
            .map(|entry| {
                entry
                    .get("sequence")
                    .and_then(serde_json::Value::as_f64)
                    .expect("#1457 every transcript entry carries a sequence")
            })
            .collect();
        negative_assert!(
            sequences.windows(2).all(|pair| pair[0] < pair[1]),
            "#1457 transcript sequences strictly increase, got {sequences:?}"
        );
        negative_assert_eq!(
            sequences.len(),
            16,
            "#1457 every concurrent write landed exactly once"
        );
        // The real invariant: a fresh reader must still be able to load it.
        let home = r.options.home.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |k: &ProjectKey| {
                crate::session_chat_dir(&home, &k.project_root, &k.project_root.to_string_lossy())
            }),
        );
        let storage = adapter
            .create_session_storage(SessionStorageInput {
                project_root: r.options.project.clone(),
                pilot_home: r.options.home.clone(),
                session_id: session.into(),
                now: None,
            })
            .expect("#1457 storage");
        storage
            .restore()
            .expect("#1457 the concurrently written transcript still loads");
        teardown(&r).await;
    }
}
