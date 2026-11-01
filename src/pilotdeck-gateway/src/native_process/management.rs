//! A8 management adapters. The A4 proxy dispatch table remains unchanged.
use super::requests::Requests;
use crate::{
    runtime::{
        connection::RequestError,
        manager::{ManagerGateway, ManagerProjects, TranscriptSessionCatalog},
        plugins::{
            CommandsListInput, GatewayCommandCatalogBundle, catalog::NativeCommandCatalogProvider,
        },
        project::{
            ProjectResources, Resource, persistence::PersistenceResources, plugins::PluginResources,
        },
    },
    skills::{
        SkillManager, SkillManagerOptions, SkillMethod, SkillsFacade, skill_binding_from_snapshot,
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

fn error(code: Option<&str>, message: JsString) -> RequestError {
    RequestError {
        code: code.unwrap_or("gateway_request_failed").into(),
        message,
        details: None,
        validation: None,
    }
}
fn unavailable() -> RequestError {
    error(
        Some("CAPABILITY_UNAVAILABLE"),
        "Native gateway cannot execute this host-dependent operation.".into(),
    )
}

impl Requests {
    pub(super) async fn management_request(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Option<Result<JsValue, RequestError>> {
        if !matches!(
            method,
            "snapshot_list"
                | "snapshot_get"
                | "snapshot_restore"
                | "manager_sessions"
                | "manager_browsers"
                | "commands_list"
        ) && SkillMethod::from_wire(method).is_none()
        {
            return None;
        }
        if method == "commands_list" {
            return Some(self.commands(params).await);
        }
        let requests = self.clone();
        let method = method.to_owned();
        let params = params.clone();
        let runtime = tokio::runtime::Handle::current();
        // Native adapters perform synchronous disk IO. Keep it off the serial
        // host so a pending file read cannot block abort, other RPCs or the
        // host's drain deadline. The worker owns its request and resource lease.
        Some(
            tokio::task::spawn_blocking(move || {
                runtime.block_on(requests.management(&method, &params))
            })
            .await
            .map_err(|e| RequestError::new(e.to_string()))
            .and_then(|result| result),
        )
    }

    fn admit_management_project(&self, params: &JsValue) -> Result<(), RequestError> {
        if let Some(JsValue::String(project)) = params.get("projectKey")
            && !project
                .to_utf8()
                .is_ok_and(|p| self.options.serves_project(&p))
        {
            return Err(error(
                Some("CAPABILITY_UNAVAILABLE"),
                "The isolated native process serves only its configured project.".into(),
            ));
        }
        Ok(())
    }

    async fn commands(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        self.admit_management_project(params)?;
        validate_command_project(params)?;
        // Acquire may read configuration synchronously. End that blocking job
        // before awaiting the catalog's own blocking loader/list jobs, so even
        // a single available blocking thread can make progress.
        let project = self.project.clone();
        let lease = tokio::task::spawn_blocking(move || project.acquire(None))
            .await
            .map_err(|e| RequestError::new(e.to_string()))?
            .map_err(|e| RequestError::new(e.to_string()))?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| RequestError::new("Native project resources are missing."))?;
        let context = resources.context();
        let plugins = context
            .resource(Resource::PluginRuntime)
            .ok_or_else(unavailable)?;
        let runtime = plugins
            .as_any()
            .downcast_ref::<PluginResources>()
            .ok_or_else(unavailable)?
            .runtime
            .clone();
        let project = self.options.project.to_string_lossy().into_owned();
        let bundle = GatewayCommandCatalogBundle::new(
            self.options.home.to_string_lossy().into_owned(),
            Arc::new(|key| Box::pin(async move { Ok(key) })),
            Arc::new(move |_| runtime.clone()),
            Arc::new(WireCommands(params.clone())),
            None,
        );
        bundle
            .commands_list(CommandsListInput {
                project_key: project,
                query: None,
                cursor: None,
                limit: None,
            })
            .await
            .map(|result| result.value)
            .map_err(|e| error(e.code, e.message.into()))
    }

    async fn management(&self, method: &str, params: &JsValue) -> Result<JsValue, RequestError> {
        self.admit_management_project(params)?;
        if matches!(method, "manager_sessions" | "manager_browsers") {
            let gateway = ManagerGateway::from_parts(
                Some(Arc::new(TranscriptSessionCatalog::new(Arc::new(
                    crate::storage::AsyncTranscriptAdapter::new(
                        Arc::new(crate::storage::JsonlTranscriptStore::new()),
                        Arc::new(|key| {
                            crate::session_chat_dir(
                                &key.pilot_home,
                                &key.project_root,
                                &key.project_root.to_string_lossy(),
                            )
                        }),
                    ),
                )))),
                None,
                ManagerProjects::new(&self.options.home, std::path::PathBuf::new()),
                &self.options.project,
                &self.options.home,
            );
            return (if method == "manager_sessions" {
                gateway.sessions(params)
            } else {
                gateway.browsers(params)
            })
            .map_err(|e| error(e.code(), e.message().clone()));
        }
        // Keep the generation leased through all awaits (catalog refresh/list/
        // release included); invalidation must not dispose an in-flight request.
        let lease = self
            .project
            .acquire(None)
            .map_err(|e| RequestError::new(e.to_string()))?;
        let resources = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| RequestError::new("Native project resources are missing."))?;
        let context = resources.context();
        if method.starts_with("snapshot_") {
            if method == "snapshot_restore"
                && let Some(target) = params.get("targetProjectKey").filter(|v| !v.is_undefined())
                && target
                    .as_string()
                    .is_some_and(|s| !s.to_utf8().is_ok_and(|p| self.options.serves_project(&p)))
            {
                return Err(error(
                    Some("CAPABILITY_UNAVAILABLE"),
                    "The isolated native process serves only its configured project.".into(),
                ));
            }
            let persistence = context
                .resource(Resource::Persistence)
                .ok_or_else(unavailable)?;
            let persistence = persistence
                .as_any()
                .downcast_ref::<PersistenceResources>()
                .ok_or_else(unavailable)?;
            let gateway = persistence.snapshot_gateway(std::path::PathBuf::new());
            return (match method {
                "snapshot_list" => gateway.list(params),
                "snapshot_get" => gateway.get(params),
                "snapshot_restore" => gateway.restore(params),
                _ => unreachable!(),
            })
            .map_err(|e| {
                if e.requires_host() {
                    unavailable()
                } else {
                    error(e.code(), e.message().clone())
                }
            });
        }
        let skill = SkillMethod::from_wire(method).ok_or_else(unavailable)?;
        let mut options =
            SkillManagerOptions::new(self.options.home.to_string_lossy().into_owned());
        options.env = Some(self.env.clone());
        options.home_dir = Some(self.options.home.to_string_lossy().into_owned());
        let snapshot = context.snapshot();
        let manager = SkillManager::new(options).map_err(skill_error)?;
        let facade = SkillsFacade::new(manager, move |_| skill_binding_from_snapshot(&snapshot));
        facade.call(skill, params).await.map_err(skill_error)
    }
}

fn skill_error(e: crate::skills::SkillError) -> RequestError {
    if e.requires_host {
        return unavailable();
    }
    let details = e.details().cloned().map(Box::new);
    let code = e.code.to_utf8().ok().filter(|s| !s.is_empty());
    RequestError {
        code: code.unwrap_or_else(|| "gateway_request_failed".into()),
        message: e.message,
        details,
        validation: e.validation,
    }
}

/// A wire-shaped outcome, also used by the request-route golden replay.
#[cfg(test)]
pub(super) fn outcome(result: Result<JsValue, RequestError>) -> JsValue {
    use crate::metadata::object;
    match result {
        Ok(value) => object([("ok", value)]),
        Err(e) => {
            let mut fields = vec![
                ("code", JsValue::String(e.code.into())),
                ("message", JsValue::String(e.message)),
            ];
            if let Some(validation) = e.validation {
                fields.push(("validation", *validation));
            }
            let mut value = pilotdeck_protocol::JsObject::new();
            for (key, field) in fields {
                value.insert(key.into(), field);
            }
            object([("error", JsValue::Object(value))])
        }
    }
}

fn validate_command_project(params: &JsValue) -> Result<(), RequestError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(error(
            None,
            format!("Cannot read properties of {kind} (reading 'projectKey')").into(),
        ));
    }
    let undefined = JsValue::Undefined;
    let project = params.get("projectKey").unwrap_or(&undefined);
    if !matches!(project, JsValue::String(_)) {
        let received = match crate::metadata::argument_received(project) {
            crate::metadata::ArgumentReceived::Text(text) => text,
            crate::metadata::ArgumentReceived::Throws { message, .. } => {
                return Err(error(None, message));
            }
        };
        let mut units = "The \"paths[0]\" argument must be of type string. Received "
            .encode_utf16()
            .collect::<Vec<_>>();
        units.extend_from_slice(received.units());
        return Err(error(
            Some("ERR_INVALID_ARG_TYPE"),
            JsString::from_utf16(units),
        ));
    }
    Ok(())
}

/// listCommands validation belongs inside the frozen catalog lease, after the
/// bundle's refresh/acquire and before its unconditional release.
struct WireCommands(JsValue);
impl crate::runtime::plugins::CommandCatalogListCommands for WireCommands {
    fn list(
        &self,
        mut input: CommandsListInput,
        home: String,
        commands: Vec<crate::runtime::plugins::PluginCommandContribution>,
    ) -> crate::runtime::plugins::BoxCommandsListFuture {
        let params = self.0.clone();
        Box::pin(async move {
            use crate::runtime::plugins::PluginRuntimeError as Error;
            let query = match params.get("query") {
                None | Some(JsValue::Undefined | JsValue::Null) => None,
                Some(JsValue::String(text)) => Some(text.clone()),
                _ => return Err(Error::new("input.query?.trim is not a function")),
            };
            if let Some(query) = &query {
                let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
                let trimmed = crate::model_catalog::trim(query);
                if crate::model_catalog::lower(&trimmed, &locale.id, &trimmed != query).len_utf16()
                    > 256
                {
                    return Err(Error::gateway(
                        "INVALID_QUERY",
                        "query must not exceed 256 characters.",
                    ));
                }
            }
            input.limit = match params.get("limit") {
                None | Some(JsValue::Undefined | JsValue::Null) => None,
                Some(JsValue::Number(n)) if n.fract() == 0.0 && (1.0..=100.0).contains(n) => {
                    Some(*n as u32)
                }
                _ => {
                    return Err(Error::gateway(
                        "INVALID_LIMIT",
                        "limit must be an integer between 1 and 100.",
                    ));
                }
            };
            input.cursor = match params.get("cursor") {
                None | Some(JsValue::Undefined | JsValue::Null | JsValue::Bool(false)) => None,
                Some(JsValue::Number(n)) if *n == 0.0 => None,
                Some(JsValue::String(s)) => {
                    Some(s.units().iter().map(|u| char::from(*u as u8)).collect())
                }
                Some(value) => Some(command_cursor_bytes(value)?),
            };
            NativeCommandCatalogProvider
                .list_with_query(input, home, commands, query.unwrap_or_default())
                .await
        })
    }
}

// Buffer.from(value, "base64url") accepts JSON byte arrays, Buffer JSON and
// numeric-length array-like objects. Preserve Node's ToUint8 element coercion.
fn command_cursor_bytes(
    value: &JsValue,
) -> Result<String, crate::runtime::plugins::PluginRuntimeError> {
    use crate::runtime::plugins::PluginRuntimeError as Error;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let invalid = || {
        Error::gateway(
            "INVALID_CURSOR",
            "cursor is invalid or does not match the query.",
        )
    };
    let byte = |v: &JsValue| {
        crate::js_value::to_number(v)
            .map(|n| {
                if !n.is_finite() {
                    0
                } else {
                    n.trunc().rem_euclid(256.0) as u8
                }
            })
            .map_err(|_| invalid())
    };
    let encode = |items: &[JsValue]| {
        items
            .iter()
            .map(byte)
            .collect::<Result<Vec<_>, _>>()
            .and_then(|b| {
                if b.is_empty() {
                    Err(invalid())
                } else {
                    Ok(URL_SAFE_NO_PAD.encode(b))
                }
            })
    };
    match value {
        JsValue::Array(items) => encode(items),
        JsValue::Object(_) => {
            if value.get("valueOf").is_some_and(|v| match v {
                JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => false,
                JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
                JsValue::String(s) => !s.units().is_empty(),
                _ => true,
            }) {
                return Err(invalid());
            }
            if let Some(JsValue::Number(length)) = value.get("length") {
                if !length.is_finite() {
                    return Err(invalid());
                }
                let mut bytes = Vec::new();
                for i in 0..(*length as usize) {
                    // A missing element becomes NUL, which JSON.parse rejects.
                    // Stop there instead of allocating an attacker-sized buffer.
                    let element = value.get(&i.to_string()).ok_or_else(invalid)?;
                    bytes.push(byte(element)?);
                }
                return if bytes.is_empty() {
                    Err(invalid())
                } else {
                    Ok(URL_SAFE_NO_PAD.encode(bytes))
                };
            }
            if value.get("length").is_some_and(|v| !v.is_undefined()) {
                return Err(invalid());
            }
            if value
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("Buffer"))
                && let Some(JsValue::Array(items)) = value.get("data")
            {
                return encode(items);
            }
            Err(invalid())
        }
        _ => Err(invalid()),
    }
}
