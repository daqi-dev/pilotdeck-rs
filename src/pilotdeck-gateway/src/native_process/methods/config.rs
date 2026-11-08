//! S5 session configuration: the seven #1457 methods wired to the native
//! runtime. Gate orders mirror the TS chain — the InProcessGateway wire gates
//! first, then the createLocalGateway / ProjectRuntimeRegistry callback
//! behavior. `session_model_set` / `session_model_clear` wrap their callback
//! in the replacement coordinator's transcript-write reservation
//! (InProcessGateway.ts:2193,2203) and append the `model-selection` /
//! `model-selection-clear` transcript entries through the session storage
//! (NativeSessionModelSelectionPort.write/clear). `output_styles_list` reads
//! the project runtime's plugin styles. The three selection methods
//! (`set_permission_mode`, `set_session_thinking`, `set_output_style`)
//! store their selection on the per-gateway #1864 store
//! ([`crate::native_process::SessionOverrides`], the counterpart of the TS
//! `sessionOverrides` / `sdkThinkingOverrides` / `sdkSessionConfigs.outputStyle`
//! maps) that the session factory's prepare closure composes the next session
//! from: thinking and output style close the session first (the TS callbacks
//! close after storing), permission mode does not — TS re-reads it on every
//! run (InProcessGateway.ts:1443), so the live adapter's submit applies it
//! through its `permission_mode_source`. `seed_read_state` routes through the
//! owner's maintenance fence onto the live session's tool checkpoint.
use super::super::requests::Requests;
use crate::{
    config::{self, Env},
    metadata::{Metadata, object},
    runtime::{
        connection::{Call, RequestError},
        project::{ProjectResources, Resource, RuntimeError, plugins::PluginResources},
        session::{SessionContext, transcript_locks},
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
/// Session configuration is the only caller left (#1455 G7): the session
/// directory moved to the shared `require_text` guard in #1604, so this is no
/// longer a cross-family helper.
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

/// Preserve each callback's project contract: model mutations require dialog
/// registration; style callbacks resolve lexical roots without that registry.
fn lexical_project(requests: &Requests, params: &JsValue) -> Result<PathBuf, RequestError> {
    match super::input_property(params, "projectKey")?.as_ref() {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(requests.options.project.clone()),
        Some(JsValue::String(key)) => super::super::project_scope::resolve(key),
        Some(other) => Err(config_error(crate::session_model::path_argument(other))),
    }
}
async fn model_project(requests: &Requests, params: &JsValue) -> Result<String, RequestError> {
    let fallback = JsValue::from(requests.options.project.to_string_lossy().as_ref());
    let raw = super::input_property(params, "projectKey")?
        .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
        .unwrap_or(fallback);
    let home = requests.options.home.clone();
    let permit = super::session_dir::session_io_permit().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let root = super::super::project_scope::registered(&home, &raw)?;
        root.to_str().map(str::to_owned).ok_or_else(|| {
            RequestError::new("Project root is not valid UTF-8 for the config loader.")
        })
    })
    .await
    .map_err(|error| RequestError::new(error.to_string()))?
}

/// Hold the reservation through the owned async operation and any detached
/// blocking write. Dropping an RPC waiter never abandons an admitted write.
struct ModelWriteReservation {
    replacement: Arc<crate::runtime::replacement::ReplacementCoordinator>,
    key: JsString,
}
impl Drop for ModelWriteReservation {
    fn drop(&mut self) {
        self.replacement.release_transcript_write(&self.key);
    }
}
async fn bounded_model_config(
    home: PathBuf,
    env: Env,
    project: Option<String>,
) -> Result<config::Config, RequestError> {
    static SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
        std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    let permit = SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| RequestError::new("Model configuration reader is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        config::load_project_bounded_lazy(&home, &PathBuf::new(), &env, project.as_deref())
            .map_err(config_error)
    })
    .await
    .map_err(|_| RequestError::new("Model configuration reader stopped unexpectedly."))?
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
        let held = reserved.map(|key| {
            Arc::new(ModelWriteReservation {
                replacement: self.replacement.clone(),
                key: key.clone(),
            })
        });
        let requests = self.clone();
        let params = params.clone();
        tokio::spawn(async move { requests.session_model_set_reserved(&params, held).await })
            .await
            .map_err(|_| RequestError::new("Session model update stopped unexpectedly."))?
    }

    /// GatewaySessionModelBundle.set: resolveProjectKey → hasActiveTurn →
    /// selection shape → validateSelection → normalizeSelection → port write
    /// (sessionKey gate) → router.close → result.
    async fn session_model_set_reserved(
        &self,
        params: &JsValue,
        reservation: Option<Arc<ModelWriteReservation>>,
    ) -> Result<JsValue, RequestError> {
        let project_key = model_project(self, params).await?;
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
        validate_selection_shape(selection)?;
        // TS listModelCatalog intentionally reads the global catalog even
        // though project defaults and transcript ownership are per project.
        let config =
            bounded_model_config(self.options.home.clone(), self.env.clone(), None).await?;
        let catalog = crate::model_catalog::execute_loaded(config, &object([]), &self.env)
            .map_err(config_error)?;
        validate_selection_catalog(selection, &catalog)?;
        let selection = normalize_selection(selection);
        let session_key = require_session_key(params)?;
        let loaded = self
            .record_model_selection(&session_key, Some(&selection), &project_key, reservation)
            .await?;
        // #1457 fix-r3: the entry may have been appended through a session
        // registered under an alias spelling (`a/b` vs `a-b` share one
        // transcript). Closing the request spelling would leave that session
        // cached with the old selection, so close the key that actually owns
        // the writer.
        self.owner
            .close(loaded.as_ref().unwrap_or(&session_key))
            .await
            .map_err(runtime_error)?;
        let default_config = if is(selection.get("mode"), "model") {
            None
        } else {
            Some(
                bounded_model_config(
                    self.options.home.clone(),
                    self.env.clone(),
                    Some(project_key.clone()),
                )
                .await?,
            )
        };
        model_result(&project_key, &session_key, &selection, default_config)
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
        let held = reserved.map(|key| {
            Arc::new(ModelWriteReservation {
                replacement: self.replacement.clone(),
                key: key.clone(),
            })
        });
        let requests = self.clone();
        let params = params.clone();
        tokio::spawn(async move { requests.session_model_clear_reserved(&params, held).await })
            .await
            .map_err(|_| RequestError::new("Session model update stopped unexpectedly."))?
    }

    /// GatewaySessionModelBundle.clear: resolveProjectKey → requireSessionKey
    /// → hasActiveTurn → port clear → router.close → void.
    async fn session_model_clear_reserved(
        &self,
        params: &JsValue,
        reservation: Option<Arc<ModelWriteReservation>>,
    ) -> Result<JsValue, RequestError> {
        let project_key = model_project(self, params).await?;
        let session_key = require_session_key(params)?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot clear the model during an active turn.",
            ));
        }
        let loaded = self
            .record_model_selection(&session_key, None, &project_key, reservation)
            .await?;
        self.owner
            .close(loaded.as_ref().unwrap_or(&session_key))
            .await
            .map_err(runtime_error)?;
        Ok(JsValue::Undefined)
    }

    /// NativeSessionModelSelectionPort.write / clear: append the
    /// `model-selection` / `model-selection-clear` session_metadata entry,
    /// continuing the transcript's sequence like the rename/tag path. Returns
    /// the key the session is registered under when the entry went through a
    /// live recorder, so the caller closes *that* session.
    async fn record_model_selection(
        &self,
        session_key: &JsString,
        selection: Option<&JsValue>,
        project_root: &str,
        reservation: Option<Arc<ModelWriteReservation>>,
    ) -> Result<Option<JsString>, RequestError> {
        // #1457 fix-r1: one writer at a time per session — this path numbers its
        // append from the tail it just read, so two concurrent cold writers
        // would both claim `sequence + 1` and brick the transcript.
        // Resolve exactly once with bounded marker reads; the same resolved
        // directory must feed both the writer fence and the cold adapter.
        static WRITE_SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
        let permit = WRITE_SLOTS
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| RequestError::new("Session model writer is unavailable."))?;
        let lookup_home = self.options.home.clone();
        let lookup_project = project_root.to_owned();
        let (chat_dir, permit) = tokio::task::spawn_blocking(move || {
            let lookup = Metadata {
                home: lookup_home,
                cwd: PathBuf::new(),
                files: crate::runtime::history::HistoryMarkerFiles::new(),
            };
            let result = lookup
                .process_chat_dir(&lookup_project)
                .map_err(|error| RequestError::new(error.to_string()));
            if lookup.files.was_refused() {
                return Err(RequestError::new("Unsafe project marker."));
            }
            Ok((result?, permit))
        })
        .await
        .map_err(|_| RequestError::new("Session model path reader stopped unexpectedly."))??;
        let fence_path = chat_dir.join(format!(
            "{}.jsonl",
            crate::storage::sanitize_session_id(session_key)
        ));
        let write_fence = transcript_locks::acquire(&fence_path).await?;
        // #1457 fix-r2: when the session is loaded its recorder owns the writer
        // (and the writer lease) with an in-memory sequence, so the entry is
        // appended *through* it. Closing it first aborted an admitted turn and
        // left a second writer numbering from the same tail; routing through the
        // one writer does neither.
        // #1457 fix-r3: with no loaded session the cold append takes the same
        // writer lease a session holds, so it cannot interleave with the
        // factory's restore→publish window. A busy lease means a session owns
        // the transcript after all: use its writer, or report the retryable
        // busy error rather than writing a second chain.
        let recorders = self.recorders.clone();
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
        tokio::task::spawn_blocking(move || -> Result<Option<JsString>, RequestError> {
            // The fence outlives a cancelled request: dropping this future does
            // not stop a blocking closure that already started, so the guard
            // travels with the write instead of dropping at the await.
            let _permit = permit;
            let _reservation = reservation;
            let _write = write_fence;
            let turn = JsString::from(turn_id);
            let entry = JsValue::Object(metadata);
            if let Some((owner, recorder)) = super::super::assembly::live_recorder(&recorders, &key)
            {
                transcript_locks::ensure_recorder_matches(&fence_path, &recorder)?;
                recorder
                    .writer()
                    .record_session_metadata_checked(&key, &turn, entry)
                    .map_err(|e| RequestError::new(e.to_string()))?;
                return Ok(Some(owner));
            }
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new_bounded()),
                Arc::new(move |_: &ProjectKey| chat_dir.clone()),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_root.clone(),
                    pilot_home: home.clone(),
                    session_id: key.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path) else {
                // A session claimed the transcript between the lookup above and
                // here. Append through it when it is published; otherwise say
                // busy — never write a second chain beside it.
                if let Some((owner, recorder)) =
                    super::super::assembly::live_recorder(&recorders, &key)
                {
                    transcript_locks::ensure_recorder_matches(&fence_path, &recorder)?;
                    recorder
                        .writer()
                        .record_session_metadata_checked(&key, &turn, entry)
                        .map_err(|e| RequestError::new(e.to_string()))?;
                    return Ok(Some(owner));
                }
                return Err(transcript_locks::writer_busy_error());
            };
            if matches!(storage.transcript_exists(), Some(Ok(true))) {
                storage
                    .restore()
                    .map_err(|e| RequestError::new(e.to_string()))?;
            }
            storage
                .transcript
                .record_session_metadata(&key, &turn, entry)
                .map_err(|e| RequestError::new(e.to_string()))?;
            Ok(None)
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))?
    }

    /// InProcessGateway.ts:2321-2329 + createLocalGateway.ts:1371-1382: the
    /// mode gate, then the busy gate on the raw key (a blank key can hold a
    /// reserved turn), then the callback's sessionKey gate. The callback
    /// stores the selection on the #1864 store; the next run re-reads it
    /// (InProcessGateway.ts:1443), so — like the TS `sessionOverrides`
    /// spread — no close is needed.
    async fn set_permission_mode(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let mode = params.get("mode").unwrap_or(&UNDEFINED);
        let mode_ok = mode.as_string().is_some_and(|m| {
            crate::runtime::permission::PERMISSION_MODES
                .iter()
                .any(|allowed| m.equals_str(allowed))
        });
        if !mode_ok {
            // Keep JS String(mode) in UTF-16, including lone surrogates (#1485).
            let mut units: Vec<u16> = "Unsupported permission mode: ".encode_utf16().collect();
            units.extend(crate::js_value::to_string_units(mode).unwrap_or_default());
            return Err(gateway_error(
                "INVALID_PERMISSION_MODE",
                JsString::from_utf16(units),
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
        let session_key = require_session_key(params)?;
        let mode = match mode.as_string() {
            Some(mode) => mode.clone(),
            None => unreachable!("the mode gate validated a string"),
        };
        self.session_overrides
            .set_permission_mode(&session_key, mode);
        Ok(object([("applied", JsValue::Bool(true))]))
    }

    /// InProcessGateway.ts:2364-2372: sessionKey → busy →
    /// validateThinkingConfig. The callback deletes on a missing/undefined
    /// thinking, stores anything else (null included), then closes the
    /// session so the next one composes the change.
    async fn set_session_thinking(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = require_session_key(params)?;
        if self.owner.has_active_turn(&session_key) {
            return Err(gateway_error(
                "SESSION_BUSY",
                "Cannot change thinking while a turn is active.",
            ));
        }
        let thinking = params.get("thinking").unwrap_or(&UNDEFINED);
        validate_thinking(thinking)?;
        match thinking {
            JsValue::Undefined => self.session_overrides.clear_thinking(&session_key),
            thinking => self
                .session_overrides
                .set_thinking(&session_key, thinking.clone()),
        }
        self.owner
            .close(&session_key)
            .await
            .map_err(runtime_error)?;
        Ok(object([("applied", JsValue::Bool(true))]))
    }

    /// InProcessGateway.ts:2374-2377 has no params gate;
    /// ProjectRuntimeRegistry.outputStylesListForSdk refreshes the plugin
    /// runtime and lists the project's (plus session's) style contributions.
    async fn output_styles_list(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let project = lexical_project(self, params)?;
        let styles = self.plugin_styles(&project).await?;
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
        // ProjectRuntimeRegistry.outputStylesListForSdk reads the session's
        // stored selection from `sdkSessionConfigs`: the field appears only
        // when a sessionKey was given and a selection is stored.
        if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)
            && let Some(selected) = self.session_overrides.output_style(key)
        {
            result.insert("selected".into(), JsValue::String(selected));
        }
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
        let project = lexical_project(self, params)?;
        let styles = self.plugin_styles(&project).await?;
        if let Some(name) = &name {
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
        match &name {
            Some(name) => self
                .session_overrides
                .set_output_style(&session_key, name.clone()),
            None => self.session_overrides.clear_output_style(&session_key),
        }
        // The TS callback closes with `sdk_output_style_changed` so the next
        // session composes the style's appendSystemPrompt.
        self.owner
            .close(&session_key)
            .await
            .map_err(runtime_error)?;
        let mut result = pilotdeck_protocol::JsObject::new();
        result.insert("applied".into(), JsValue::Bool(true));
        if let Some(name) = &name {
            result.insert("selected".into(), JsValue::String(name.clone()));
        }
        Ok(JsValue::Object(result))
    }

    /// The plugin runtime's output styles, after the refresh the TS callback
    /// performs. The lease stays held across the await so a concurrent reload
    /// cannot retire the runtime mid-refresh.
    async fn plugin_styles(
        &self,
        root: &std::path::Path,
    ) -> Result<Vec<crate::runtime::plugins::PluginOutputStyleContribution>, RequestError> {
        // Acquire may read configuration synchronously (management.rs:118-125),
        // so the blocking job stays off the async executor.
        let project = self.project.clone();
        let root = root.to_path_buf();
        let lease = tokio::task::spawn_blocking(move || project.acquire(Some(&root)))
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
            project_key: Some(JsString::from(
                self.options.project.to_string_lossy().as_ref(),
            )),
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

/// Shape is validated before loading the catalog, as in TypeScript.
pub(in crate::native_process) fn validate_selection_shape(
    selection: &JsValue,
) -> Result<(), RequestError> {
    if is(selection.get("mode"), "auto")
        || (is(selection.get("mode"), "model")
            && selection
                .get("provider")
                .and_then(JsValue::as_string)
                .is_some()
            && selection
                .get("model")
                .and_then(JsValue::as_string)
                .is_some())
    {
        Ok(())
    } else {
        Err(gateway_error(
            "INVALID_MODEL_OVERRIDE",
            "A model selection requires mode, provider, and model.",
        ))
    }
}
/// Reuse session-model policy against a caller-supplied project catalog.
pub(in crate::native_process) fn validate_selection_catalog(
    selection: &JsValue,
    catalog: &JsValue,
) -> Result<(), RequestError> {
    if is(selection.get("mode"), "auto") {
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
pub(in crate::native_process) fn normalize_selection(selection: &JsValue) -> JsValue {
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
fn model_result(
    project_key: &str,
    session_key: &JsString,
    saved: &JsValue,
    default_config: Option<config::Config>,
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
        let config =
            default_config.ok_or_else(|| RequestError::new("Missing model configuration."))?;
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
    async fn permission_mode_error_preserves_utf16() {
        let (_scratch, r) =
            std::panic::catch_unwind(crate::native_process::management_tests::setup)
                .expect("#1485 permission mode fixture");
        for (label, mode, expected_json) in [
            (
                "high",
                JsValue::String(JsString::from_utf16(vec![0xd800])),
                r#""Unsupported permission mode: \ud800""#,
            ),
            (
                "low",
                JsValue::String(JsString::from_utf16(vec![0xdc00])),
                r#""Unsupported permission mode: \udc00""#,
            ),
            (
                "mixed array",
                JsValue::Array(vec![
                    JsValue::String(JsString::from_utf16(vec![0xd800])),
                    JsValue::String(JsString::from_utf16(vec![0xd83d, 0xde00, 0xdc00])),
                ]),
                r#""Unsupported permission mode: \ud800,😀\udc00""#,
            ),
        ] {
            let error = r
                .session_config_request(
                    "set_permission_mode".into(),
                    object([("sessionKey", "cfg-pm-utf16".into()), ("mode", mode)]),
                )
                .await
                .expect_err("#1485 invalid UTF-16 mode is rejected");
            negative_assert_eq!(error.code, "INVALID_PERMISSION_MODE", "#1485 mode code");
            let expected =
                pilotdeck_protocol::parse_json(expected_json).expect("#1485 expected message JSON");
            negative_assert_eq!(
                error.message.units(),
                expected.as_string().expect("#1485 expected string").units(),
                "#1485 mode UTF-16 preserved: {label}"
            );
            let wire = pilotdeck_protocol::stringify_json(&JsValue::String(error.message))
                .expect("#1485 message serializes")
                .expect("#1485 message is defined");
            negative_assert_eq!(wire, expected_json, "#1485 mode JSON preserved: {label}");
        }
        r.owner.shutdown().await.expect("#1485 shutdown");
        r.project.dispose().await.expect("#1485 dispose");
    }

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
    async fn set_permission_mode_applies_and_validates() {
        let (_scratch, r) = setup_fixture();
        // #1864: every TS mode passes the wire gates and is stored on the
        // per-gateway session-override store. Negative control: deleting the
        // `set_permission_mode` store write makes the read-back below fail
        // (`None` instead of `Some("plan")`).
        for mode in crate::runtime::permission::PERMISSION_MODES {
            let result = r
                .session_config_request(
                    "set_permission_mode".into(),
                    object([
                        ("sessionKey", "cfg-pm-modes".into()),
                        ("mode", (*mode).into()),
                    ]),
                )
                .await
                .expect("#1864 set_permission_mode applies the fixture mode");
            negative_assert_eq!(
                result.get("applied"),
                Some(&JsValue::Bool(true)),
                "#1864 set_permission_mode reports applied for {mode}"
            );
        }
        let result = r
            .session_config_request(
                "set_permission_mode".into(),
                object([("sessionKey", "cfg-pm-1".into()), ("mode", "plan".into())]),
            )
            .await
            .expect("#1864 set_permission_mode applies plan");
        negative_assert_eq!(
            result.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 set_permission_mode reports applied"
        );
        negative_assert_eq!(
            r.session_overrides
                .permission_mode(&"cfg-pm-1".into())
                .as_ref(),
            Some(&JsString::from("plan")),
            "#1864 the plan selection is stored for the next run to read"
        );
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
    async fn set_session_thinking_applies_and_validates() {
        let (_scratch, r) = setup_fixture();
        // #1864: valid payloads (including the explicit null) pass
        // validate_thinking and are stored. Negative control: deleting the
        // `set_thinking` call makes the stored-mode read-back below fail
        // (None instead of the mode object).
        let result = r
            .session_config_request(
                "set_session_thinking".into(),
                object([
                    ("sessionKey", "cfg-th-obj".into()),
                    (
                        "thinking",
                        object([("enabled", JsValue::Bool(true)), ("mode", "high".into())]),
                    ),
                ]),
            )
            .await
            .expect("#1864 set_session_thinking stores the mode object");
        negative_assert_eq!(
            result.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 set_session_thinking reports applied"
        );
        let stored = r
            .session_overrides
            .thinking(&"cfg-th-obj".into())
            .expect("#1864 the mode object is stored");
        negative_assert_eq!(
            stored.get("mode").and_then(JsValue::as_string),
            Some(&JsString::from("high")),
            "#1864 the stored thinking keeps its mode"
        );
        let result = r
            .session_config_request(
                "set_session_thinking".into(),
                object([
                    ("sessionKey", "cfg-th-null".into()),
                    ("thinking", JsValue::Null),
                ]),
            )
            .await
            .expect("#1864 set_session_thinking stores the explicit null");
        negative_assert_eq!(
            result.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 set_session_thinking reports applied for null"
        );
        // TS deletes only on undefined: a stored JS null is a value.
        negative_assert_eq!(
            r.session_overrides.thinking(&"cfg-th-null".into()),
            Some(JsValue::Null),
            "#1864 a stored null thinking stays a value"
        );
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
        // #1864: a style the project really contributes passes the name
        // gate, is stored and closes the session. Negative control: deleting
        // the `selected` insert in `output_styles_list` makes the final
        // assertion fail (None instead of Some("probe:compact")).
        let applied = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", JsValue::String(key.clone())),
                    ("name", "probe:compact".into()),
                ]),
            )
            .await
            .expect("#1864 set_output_style applies the probe style");
        negative_assert_eq!(
            applied.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 set_output_style reports applied"
        );
        negative_assert_eq!(
            applied.get("selected"),
            Some(&JsValue::String("probe:compact".into())),
            "#1864 set_output_style echoes the selected name"
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
            Some(&JsValue::String("probe:compact".into())),
            "#1864 the list echoes the stored selection for the session"
        );
        let bare = r
            .session_config_request("output_styles_list".into(), object([]))
            .await
            .expect("#1864 output_styles_list without a sessionKey");
        negative_assert_eq!(
            bare.get("selected"),
            None,
            "#1864 no sessionKey means no selected field"
        );
        let other = r
            .session_config_request(
                "output_styles_list".into(),
                object([("sessionKey", "cfg-style-other".into())]),
            )
            .await
            .expect("#1864 output_styles_list for an unselected session");
        negative_assert_eq!(
            other.get("selected"),
            None,
            "#1864 a session without a stored selection omits selected"
        );
        teardown(&r).await;
    }

    #[tokio::test]
    async fn set_output_style_validates_names_and_key() {
        let (_scratch, r) = setup_fixture();
        write_probe_plugin(&r);
        // #1864: the success path — a project style passes the name gate,
        // answers `applied: true` with the `selected` echo, and lands in the
        // store the next session composes from. Negative control: a stub
        // revert dies on the request expect, a dropped store write dies on
        // the read-back expect; both carry this anchor.
        let applied = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", "cfg-style-3".into()),
                    ("name", "probe:compact".into()),
                ]),
            )
            .await
            .expect("#1864 set_output_style applies the probe style");
        negative_assert_eq!(
            applied.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 set_output_style reports applied on the success path"
        );
        negative_assert_eq!(
            applied.get("selected"),
            Some(&JsValue::String("probe:compact".into())),
            "#1864 set_output_style echoes the selected name on the success path"
        );
        let stored = r
            .session_overrides
            .output_style(&"cfg-style-3".into())
            .expect("#1864 set_output_style applies the probe style");
        negative_assert_eq!(
            stored,
            JsString::from("probe:compact"),
            "#1864 the probe style selection is stored for the next compose"
        );
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
        // #1864: name null clears a stored selection. Negative control:
        // deleting the `clear_output_style` branch leaves the read-back as
        // `Some("probe:compact")`.
        let applied = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", "cfg-style-clear".into()),
                    ("name", "probe:compact".into()),
                ]),
            )
            .await
            .expect("#1864 set_output_style stores the probe style");
        negative_assert_eq!(
            applied.get("selected"),
            Some(&JsValue::String("probe:compact".into())),
            "#1864 set_output_style echoes the selected name"
        );
        let cleared = r
            .session_config_request(
                "set_output_style".into(),
                object([
                    ("sessionKey", "cfg-style-clear".into()),
                    ("name", JsValue::Null),
                ]),
            )
            .await
            .expect("#1864 a null name clears the selection");
        negative_assert_eq!(
            cleared.get("applied"),
            Some(&JsValue::Bool(true)),
            "#1864 clearing reports applied"
        );
        negative_assert_eq!(
            cleared.get("selected"),
            None,
            "#1864 clearing omits selected"
        );
        negative_assert_eq!(
            r.session_overrides.output_style(&"cfg-style-clear".into()),
            None,
            "#1864 the null name cleared the stored selection"
        );
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── #1864 next-turn effect

    /// #1864: the TS callbacks close the session after storing thinking and
    /// output style (so the next session composes the change); permission
    /// mode does not — InProcessGateway.ts:1443 re-reads the map on every
    /// run, so the live adapter applies it per turn. Negative control:
    /// deleting an `owner.close` call in `set_session_thinking` /
    /// `set_output_style` turns its `is_none` assertion red; adding one to
    /// `set_permission_mode` turns the `is_some` assertion red.
    #[tokio::test]
    async fn thinking_and_style_close_the_session_permission_mode_does_not() {
        let (_scratch, r) = setup_fixture();
        let live = |key: &str| SessionContext {
            session_key: key.into(),
            project_key: Some(r.options.project.to_string_lossy().into_owned().into()),
            channel_key: "test".into(),
            allowed_tools: None,
            disallowed_tools: None,
        };
        r.owner
            .get_or_create(live("cfg-close-pm"))
            .await
            .expect("#1864 the permission-mode session is created");
        r.session_config_request(
            "set_permission_mode".into(),
            object([
                ("sessionKey", "cfg-close-pm".into()),
                ("mode", "plan".into()),
            ]),
        )
        .await
        .expect("#1864 set_permission_mode applies plan");
        negative_assert!(
            r.owner.current_handle(&"cfg-close-pm".into()).is_some(),
            "#1864 permission mode leaves the live session in place"
        );
        r.owner
            .get_or_create(live("cfg-close-th"))
            .await
            .expect("#1864 the thinking session is created");
        r.session_config_request(
            "set_session_thinking".into(),
            object([
                ("sessionKey", "cfg-close-th".into()),
                (
                    "thinking",
                    object([("enabled", JsValue::Bool(true)), ("mode", "high".into())]),
                ),
            ]),
        )
        .await
        .expect("#1864 set_session_thinking applies high");
        negative_assert!(
            r.owner.current_handle(&"cfg-close-th".into()).is_none(),
            "#1864 thinking closes the session for the next compose"
        );
        write_probe_plugin(&r);
        r.owner
            .get_or_create(live("cfg-close-style"))
            .await
            .expect("#1864 the output-style session is created");
        r.session_config_request(
            "set_output_style".into(),
            object([
                ("sessionKey", "cfg-close-style".into()),
                ("name", "probe:compact".into()),
            ]),
        )
        .await
        .expect("#1864 set_output_style applies the probe style");
        negative_assert!(
            r.owner.current_handle(&"cfg-close-style".into()).is_none(),
            "#1864 output style closes the session for the next compose"
        );
        teardown(&r).await;
    }

    /// #1864 read-back: the compose step the prepare closure runs after
    /// reading the store, as a pure function. Negative control: deleting a
    /// branch of `apply_session_overrides` makes its matching assertion fail
    /// (permissionMode falls back to "default", thinking stays absent,
    /// appendSystemPrompt stays untouched or undefined).
    #[test]
    fn apply_session_overrides_composes_the_selections() {
        use crate::native_process::assembly::apply_session_overrides;
        let mode_of = |config: &pilotdeck_protocol::JsObject| {
            config
                .get("permissionMode")
                .and_then(JsValue::as_string)
                .cloned()
        };
        let mut config = pilotdeck_protocol::JsObject::new();
        apply_session_overrides(&mut config, None, None, None);
        negative_assert_eq!(
            mode_of(&config),
            Some(JsString::from("default")),
            "#1864 no stored selection composes the default mode"
        );
        apply_session_overrides(&mut config, Some(&JsString::from("plan")), None, None);
        negative_assert_eq!(
            mode_of(&config),
            Some(JsString::from("plan")),
            "#1864 the stored mode wins over the default"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        config.insert(
            "managedPermissions".into(),
            object([("defaultMode", "bypassPermissions".into())]),
        );
        apply_session_overrides(&mut config, Some(&JsString::from("plan")), None, None);
        negative_assert_eq!(
            mode_of(&config),
            Some(JsString::from("bypassPermissions")),
            "#1864 a non-empty managed defaultMode wins over the store"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        config.insert(
            "managedPermissions".into(),
            object([("defaultMode", "  ".into())]),
        );
        apply_session_overrides(&mut config, Some(&JsString::from("plan")), None, None);
        negative_assert_eq!(
            mode_of(&config),
            Some(JsString::from("plan")),
            "#1864 a blank managed defaultMode falls back to the store"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        config.insert("thinking".into(), JsValue::String("project-default".into()));
        apply_session_overrides(&mut config, None, None, None);
        negative_assert_eq!(
            config.get("thinking").and_then(JsValue::as_string).cloned(),
            Some(JsString::from("project-default")),
            "#1864 no stored thinking leaves the config's own value"
        );
        apply_session_overrides(&mut config, None, Some(&JsValue::Null), None);
        negative_assert_eq!(
            config.get("thinking"),
            Some(&JsValue::Null),
            "#1864 a stored null thinking is written, not skipped"
        );
        let thinking = object([("enabled", JsValue::Bool(true)), ("mode", "high".into())]);
        apply_session_overrides(&mut config, None, Some(&thinking), None);
        negative_assert_eq!(
            config
                .get("thinking")
                .and_then(|value| value.get("mode"))
                .and_then(JsValue::as_string)
                .cloned(),
            Some(JsString::from("high")),
            "#1864 the stored thinking object is written"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        apply_session_overrides(&mut config, None, None, None);
        negative_assert_eq!(
            config.get("appendSystemPrompt"),
            None,
            "#1864 no style leaves appendSystemPrompt untouched"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        config.insert(
            "appendSystemPrompt".into(),
            JsValue::String("keep this".into()),
        );
        apply_session_overrides(&mut config, None, None, Some("# Compact"));
        negative_assert_eq!(
            config
                .get("appendSystemPrompt")
                .and_then(JsValue::as_string)
                .cloned(),
            Some(JsString::from("keep this\n\n# Compact")),
            "#1864 the style content joins the existing prompt"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        apply_session_overrides(&mut config, None, None, Some("# Compact"));
        negative_assert_eq!(
            config
                .get("appendSystemPrompt")
                .and_then(JsValue::as_string)
                .cloned(),
            Some(JsString::from("# Compact")),
            "#1864 the style content alone when nothing existed"
        );
        let mut config = pilotdeck_protocol::JsObject::new();
        config.insert("appendSystemPrompt".into(), JsValue::String("   ".into()));
        apply_session_overrides(&mut config, None, None, Some("  "));
        negative_assert_eq!(
            config.get("appendSystemPrompt"),
            Some(&JsValue::Undefined),
            "#1864 both blank writes undefined"
        );
    }

    /// #1864 read-back end to end: the three selector methods write the very
    /// store the prepare closure reads, and the compose step turns the
    /// stored values into agent-config fields. Negative control: storing
    /// under a different key (or a store instance the factory does not
    /// share) makes the composed values revert to the defaults.
    #[tokio::test]
    async fn selector_writes_reach_the_compose_step() {
        use crate::native_process::assembly::apply_session_overrides;
        let (_scratch, r) = setup_fixture();
        r.session_config_request(
            "set_permission_mode".into(),
            object([("sessionKey", "cfg-rb".into()), ("mode", "plan".into())]),
        )
        .await
        .expect("#1864 set_permission_mode applies plan");
        r.session_config_request(
            "set_session_thinking".into(),
            object([
                ("sessionKey", "cfg-rb".into()),
                (
                    "thinking",
                    object([("enabled", JsValue::Bool(true)), ("mode", "high".into())]),
                ),
            ]),
        )
        .await
        .expect("#1864 set_session_thinking applies high");
        let mut config = pilotdeck_protocol::JsObject::new();
        let permission = r.session_overrides.permission_mode(&"cfg-rb".into());
        let thinking = r.session_overrides.thinking(&"cfg-rb".into());
        apply_session_overrides(&mut config, permission.as_ref(), thinking.as_ref(), None);
        negative_assert_eq!(
            config
                .get("permissionMode")
                .and_then(JsValue::as_string)
                .cloned(),
            Some(JsString::from("plan")),
            "#1864 the stored permission mode reaches the agent config"
        );
        negative_assert_eq!(
            config
                .get("thinking")
                .and_then(|value| value.get("mode"))
                .and_then(JsValue::as_string)
                .cloned(),
            Some(JsString::from("high")),
            "#1864 the stored thinking reaches the agent config"
        );
        teardown(&r).await;
    }

    // ─────────────────────────────────────────────── seed_read_state

    #[tokio::test]
    async fn seed_read_state_applies_and_validates() {
        let (_scratch, r) = setup_fixture();
        r.owner.enforce_project_binding();
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
        assert_eq!(
            r.owner.project_key(&"cfg-seed-1".into()),
            Some(JsString::from(r.options.project.to_string_lossy().as_ref())),
            "omitted project must pin the selected request view"
        );
        assert!(
            r.owner
                .reserve_project(&"cfg-seed-1".into(), Some(&"other".into()), &"other".into())
                .is_err(),
            "a different project must not reuse the seeded session"
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

    /// #1457 fix-r2 (P0): a loaded session owns the transcript writer, so the
    /// model-selection entry goes through its recorder instead of a second
    /// writer restored from the tail. The request spells the key as an alias
    /// (`b9/session` and `b9-session` are one file after
    /// `sanitize_session_id`), the case a raw-key lookup missed: the recorder
    /// would keep a stale sequence and its next append would duplicate.
    #[tokio::test]
    async fn model_set_appends_through_the_loaded_writer() {
        use crate::storage::recorder::Recorder;

        let (_scratch, r) = setup_fixture();
        let home = r.options.home.clone();
        let project = r.options.project.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |k: &ProjectKey| {
                crate::session_chat_dir(&home, &k.project_root, &k.project_root.to_string_lossy())
            }),
        );
        let open = |session_id: &str| {
            adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project.clone(),
                    pilot_home: r.options.home.clone(),
                    session_id: session_id.into(),
                    now: None,
                })
                .expect("#1457 the transcript storage opens")
        };
        let loaded = open("b9/session");
        loaded
            .restore()
            .expect("#1457 the seeded transcript restores");
        let writer = Arc::new(loaded.transcript);
        let recorder = Arc::new(Recorder::from_writer(writer.clone()));
        r.recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("b9/session".into(), Arc::downgrade(&recorder));

        let result = r
            .session_config_request(
                "session_model_set".into(),
                object([
                    ("sessionKey", "b9-session".into()),
                    ("selection", selection("model", "test", "test")),
                ]),
            )
            .await
            .expect("#1457 the model selection resolves over a loaded session");
        negative_assert_eq!(
            result
                .get("saved")
                .and_then(|saved| saved.get("model"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("test")),
            "#1457 the model selection is saved"
        );

        // The loaded writer still holds the only sequence: this append continues
        // the chain the selection entry extended. A second writer on the same
        // tail would have duplicated `sequence + 1` and this call would fail.
        writer
            .record_agent_status_message(
                &"b9/session".into(),
                &"after-selection".into(),
                &JsValue::Undefined,
            )
            .expect("#1457 the loaded writer keeps numbering the chain");
        // A second writer on the same tail leaves a duplicate `sequence` that
        // only the next reader rejects, so read the file back the same way the
        // resume path does.
        let reread = open("b9-session");
        reread.restore().expect("#1457 the transcript still loads");
        let entries = transcript_entries(&r, "b9-session");
        negative_assert_eq!(
            entries.iter().any(
                |entry| entry.get("turnId").and_then(|t| t.as_str()) == Some("model-selection")
            ),
            true,
            "#1457 the selection entry reached the loaded session's transcript"
        );
        negative_assert_eq!(
            entries.iter().any(
                |entry| entry.get("turnId").and_then(|t| t.as_str()) == Some("after-selection")
            ),
            true,
            "#1457 the loaded writer kept appending after the selection"
        );
        teardown(&r).await;
    }

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
    #[tokio::test]
    async fn selected_project_output_styles_do_not_read_fallback_generation() {
        let (_scratch, r) = setup_fixture();
        let a = r.options.project.clone();
        let b = a.parent().unwrap().join("styles-only-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut view = r.clone();
        view.options.project = b.clone();
        crate::native_process::assembly::register_project(&view.options).unwrap();
        write_probe_plugin(&view);
        let styles = view
            .session_config_request("output_styles_list".into(), object([]))
            .await
            .unwrap();
        assert!(
            array_field(&styles, "styles")
                .iter()
                .any(|item| item.get("name") == Some(&"probe:compact".into()))
        );
        let fallback = r
            .session_config_request("output_styles_list".into(), object([]))
            .await
            .unwrap();
        assert!(
            !array_field(&fallback, "styles")
                .iter()
                .any(|item| item.get("name") == Some(&"probe:compact".into()))
        );
        teardown(&r).await;
    }
}

#[cfg(all(test, unix))]
#[path = "config_model_bounded_tests.rs"]
mod bounded_native_write_tests;

#[cfg(test)]
#[path = "../multi_project_alias_tests.rs"]
mod multi_project_alias_tests;
