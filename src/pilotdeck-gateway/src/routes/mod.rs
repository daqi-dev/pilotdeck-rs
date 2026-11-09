//! Local method ownership and file requirements. Add domain modules here.
pub(crate) mod domains;
pub(crate) mod ws;
use crate::{
    dispatch::{self, Route},
    metadata::{Files, Metadata, text_field},
    runtime::mcp::{routes::McpRoutes, sdk::SdkMcpError},
};
use pilotdeck_protocol::JsValue;

enum FileRequirement {
    None,
    Required,
    ProjectKey,
}

struct Module {
    methods: &'static [(&'static str, FileRequirement)],
    handler: Handler,
}

enum Handler {
    Metadata,
    Config,
}

const MODULES: &[Module] = &[
    Module {
        handler: Handler::Config,
        methods: &[
            (
                "dispatch_im_inbound_slash_command",
                FileRequirement::Required,
            ),
            ("prepare_weixin_login", FileRequirement::Required),
            ("reload_extensions", FileRequirement::Required),
            ("reload_config", FileRequirement::Required),
            ("commands_list", FileRequirement::Required),
            ("reload_output_styles", FileRequirement::Required),
            ("set_output_style", FileRequirement::Required),
            ("output_styles_list", FileRequirement::Required),
            ("set_session_thinking", FileRequirement::Required),
            ("set_permission_mode", FileRequirement::Required),
            ("update_settings", FileRequirement::Required),
            ("resolve_settings", FileRequirement::Required),
            ("apply_flag_settings", FileRequirement::Required),
        ],
    },
    Module {
        handler: Handler::Metadata,
        methods: &[
            ("supported_agents", FileRequirement::None),
            ("list_projects", FileRequirement::Required),
            ("describe_project", FileRequirement::ProjectKey),
            ("project_files_list", FileRequirement::Required),
            ("project_file_read", FileRequirement::Required),
            ("model_catalog_list", FileRequirement::Required),
            ("session_model_get", FileRequirement::Required),
            ("native_archive_manifest", FileRequirement::Required),
            ("native_archive_entries", FileRequirement::Required),
            ("native_archive_artifact", FileRequirement::Required),
        ],
    },
];

fn registered(request: &JsValue) -> Option<(&'static Module, &'static FileRequirement)> {
    let method = text_field(request, "method")?;
    MODULES.iter().find_map(|module| {
        module
            .methods
            .iter()
            .find_map(|(name, files)| (*name == method).then_some((module, files)))
    })
}

impl Module {
    fn handle<F: Files>(&self, request: &JsValue, metadata: Option<&Metadata<F>>) -> Route {
        match self.handler {
            Handler::Metadata => dispatch::route_with_metadata(request, metadata),
            Handler::Config => crate::config::routes::native::response(request, metadata),
        }
    }
}

pub(crate) fn is_local(request: &JsValue) -> bool {
    registered(request).is_some()
        || text_field(request, "method").is_some_and(|m| domains::METHODS.contains(&m.as_str()))
        || is_mcp(request)
        || matches!(
            text_field(request, "method").as_deref(),
            Some(
                "permission_list"
                    | "grant_session_permission"
                    | "permission_decide"
                    | "elicitation_respond"
                    | "user_dialog_list"
                    | "user_dialog_claim"
                    | "user_dialog_release"
                    | "user_dialog_respond"
            )
        )
}

pub(crate) fn is_metadata_method(method: &str) -> bool {
    MODULES
        .iter()
        .any(|module| module.methods.iter().any(|(name, _)| *name == method))
}

pub(crate) async fn native_metadata(
    method: pilotdeck_protocol::JsString,
    params: JsValue,
    metadata: std::sync::Arc<Metadata<crate::metadata::NativeFiles>>,
) -> Result<JsValue, crate::runtime::connection::RequestError> {
    use crate::{metadata::object, runtime::connection::RequestError};
    tokio::task::spawn_blocking(move || {
        let request = object([
            ("type", "request".into()),
            ("id", "native".into()),
            ("method", JsValue::String(method)),
            ("params", params),
        ]);
        let Route::LocalResponse(bytes) = route(&request, Some(metadata.as_ref())) else {
            return Err(RequestError::new("Metadata capability is unavailable."));
        };
        let response = dispatch::parse(&bytes)
            .ok_or_else(|| RequestError::new("Invalid metadata response."))?;
        if response.get("ok") == Some(&JsValue::Bool(true)) {
            Ok(response
                .get("result")
                .cloned()
                .unwrap_or(JsValue::Undefined))
        } else {
            let error = response.get("error");
            let mut result = RequestError::new(
                error
                    .and_then(|e| e.get("message"))
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default(),
            );
            result.code = error
                .and_then(|e| e.get("code"))
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .unwrap_or_else(|| "gateway_request_failed".into());
            Err(result)
        }
    })
    .await
    .map_err(|error| RequestError::new(error.to_string()))?
}

pub(crate) fn is_mcp(request: &JsValue) -> bool {
    text_field(request, "method").is_some_and(|method| McpRoutes::handles(&method))
}

pub(crate) async fn route_mcp(request: &JsValue, mcp: Option<&McpRoutes>) -> Route {
    use crate::metadata::object;
    if !dispatch::is_request(request) || !is_mcp(request) {
        return Route::Forward;
    }
    let method = text_field(request, "method").expect("validated MCP method");
    let result = match mcp {
        Some(mcp) => {
            mcp.handle(
                &method,
                request.get("params").unwrap_or(&JsValue::Undefined),
            )
            .expect("registered MCP method")
            .await
        }
        None => Err(SdkMcpError::gateway(
            "CAPABILITY_UNAVAILABLE",
            format!("{method} is unavailable."),
        )),
    };
    let mut response = object([
        ("type", "response".into()),
        (
            "id",
            request.get("id").expect("validated request id").clone(),
        ),
        ("ok", JsValue::Bool(result.is_ok())),
    ]);
    let JsValue::Object(fields) = &mut response else {
        unreachable!("response object");
    };
    match result {
        Ok(value) => fields.insert("result".into(), value),
        Err(error) => fields.insert(
            "error".into(),
            object([
                (
                    "code",
                    error.code().unwrap_or("gateway_request_failed").into(),
                ),
                ("message", JsValue::String(error.message().clone())),
            ]),
        ),
    };
    Route::LocalResponse(
        pilotdeck_protocol::stringify_json(&response)
            .expect("MCP response is serializable")
            .expect("MCP response is an object")
            .into_bytes(),
    )
}

pub(crate) async fn user_dialog(
    request: &JsValue,
    state: &crate::user_dialog::GatewayUserDialogs,
) -> Option<Route> {
    let method = text_field(request, "method")?;
    crate::user_dialog::routes::handle(
        &method,
        request.get("params").unwrap_or(&JsValue::Undefined),
        state,
    )
    .await
    .map(|result| response(request, result))
}

/// Stateful domains use the server's coordinator in both transport paths.
pub(crate) fn route_with_interactions<F: Files>(
    request: &JsValue,
    metadata: Option<&Metadata<F>>,
    interactions: &crate::runtime::interaction::GatewayInteractionCoordinator,
) -> Route {
    if !dispatch::is_request(request) {
        return Route::Forward;
    }
    let method = text_field(request, "method").expect("validated request method");
    let Some(result) = crate::runtime::permission::routes::handle(
        &method,
        request.get("params").unwrap_or(&JsValue::Undefined),
        interactions,
    )
    .or_else(|| {
        crate::runtime::interaction::routes::handle(
            &method,
            request.get("params").unwrap_or(&JsValue::Undefined),
            interactions,
        )
    }) else {
        return dispatch::route_with_metadata(request, metadata);
    };
    response(request, result)
}

/// The online transport supplies params prepared with its authenticated binding.
#[cfg(test)]
pub(crate) fn bound_interaction(
    request: &JsValue,
    params: &JsValue,
    interactions: &crate::runtime::interaction::GatewayInteractionCoordinator,
) -> Vec<Route> {
    let mut output = Vec::new();
    let method = text_field(request, "method").expect("validated request method");
    let outcome = crate::runtime::permission::routes::handle(&method, params, interactions)
        .or_else(|| crate::runtime::interaction::routes::handle(&method, params, interactions))
        .expect("owned interaction method")
        .and_then(|result| {
            if method != "permission_decide" {
                return Ok(result);
            }
            if let Some(payload) = crate::runtime::connection::permission_notification(
                request.get("params").unwrap_or(&JsValue::Undefined),
            )? {
                let notification = crate::metadata::object([
                    ("type", "notification".into()),
                    ("name", "permission_changed".into()),
                    ("payload", payload),
                ]);
                output.push(Route::LocalResponse(
                    pilotdeck_protocol::stringify_json(&notification)
                        .unwrap()
                        .unwrap()
                        .into_bytes(),
                ));
            }
            Ok(result)
        });
    output.push(response(request, outcome));
    output
}

fn response(
    request: &JsValue,
    result: Result<JsValue, crate::runtime::connection::RequestError>,
) -> Route {
    let mut response = crate::metadata::object([
        ("type", "response".into()),
        (
            "id",
            request.get("id").expect("validated request id").clone(),
        ),
        ("ok", JsValue::Bool(result.is_ok())),
    ]);
    let JsValue::Object(ref mut fields) = response else {
        unreachable!()
    };
    match result {
        Ok(result) => {
            fields.insert("result".into(), result);
        }
        Err(error) => {
            fields.insert(
                "error".into(),
                crate::metadata::object([
                    ("code", error.code.as_str().into()),
                    ("message", JsValue::String(error.message)),
                ]),
            );
        }
    }
    Route::LocalResponse(
        pilotdeck_protocol::stringify_json(&response)
            .expect("permission response is JSON")
            .expect("response is defined")
            .into_bytes(),
    )
}

pub(crate) fn needs_files(request: &JsValue) -> bool {
    registered(request).is_some_and(|(_, files)| match files {
        FileRequirement::None => false,
        FileRequirement::Required => true,
        FileRequirement::ProjectKey => request
            .get("params")
            .and_then(|params| params.get("projectKey"))
            .and_then(JsValue::as_string)
            .is_some(),
    })
}

pub(crate) fn route<F: Files>(request: &JsValue, metadata: Option<&Metadata<F>>) -> Route {
    if !dispatch::is_request(request) {
        return Route::Forward;
    }
    registered(request).map_or(Route::Forward, |(module, _)| {
        module.handle(request, metadata)
    })
}

/// Shared native services, owned by the server rather than a socket.

pub struct GatewayServices {
    pub browsers: Option<crate::runtime::manager::BrowserManager>,
    pub home: std::path::PathBuf,
    pub cwd: std::path::PathBuf,
    pub admission: std::sync::Arc<crate::runtime::session::SessionAdmission>,
    pub async_hooks: crate::runtime::async_hooks::AsyncHookRegistry,
}
impl GatewayServices {
    pub fn new(
        home: std::path::PathBuf,
        cwd: std::path::PathBuf,
        admission: std::sync::Arc<crate::runtime::session::SessionAdmission>,
        async_hooks: crate::runtime::async_hooks::AsyncHookRegistry,
    ) -> Self {
        let cwd = crate::config::absolute_cwd(&cwd).unwrap_or(cwd);
        let home = if home.as_os_str().is_empty() {
            home
        } else {
            crate::metadata::resolve_path(&cwd, &home)
        };
        Self {
            browsers: None,
            home,
            cwd,
            admission,
            async_hooks,
        }
    }
    pub fn with_browser_manager(
        mut self,
        manager: crate::runtime::manager::BrowserManager,
    ) -> Self {
        self.browsers = Some(manager);
        self
    }
    pub(crate) fn from_env() -> Self {
        use crate::runtime::async_hooks::{AsyncHookRegistry, TokioHookClock};
        let metadata = crate::metadata::Metadata::from_env().ok().flatten();
        Self::new(
            metadata
                .as_ref()
                .map(|m| m.home.clone())
                .unwrap_or_default(),
            metadata
                .as_ref()
                .map(|m| m.cwd.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
            Default::default(),
            AsyncHookRegistry::new(std::sync::Arc::new(TokioHookClock), |_| None, |_, _| {}),
        )
    }
    pub async fn request(
        &self,
        method: &str,
        params: &JsValue,
    ) -> Option<Result<JsValue, crate::runtime::connection::RequestError>> {
        match method {
            "manager_browsers" => Some(match &self.browsers {
                Some(manager) => manager.browsers(&self.home, &self.cwd, params).await,
                None => Err(crate::runtime::manager::browser_provider_unavailable()),
            }),
            "manager_sessions" => {
                Some(crate::runtime::manager::sessions(&self.home, &self.cwd, params).await)
            }
            "sop_resume" => {
                use crate::runtime::connection::RequestError;
                if matches!(params, JsValue::Null | JsValue::Undefined) {
                    let kind = if params.is_undefined() {
                        "undefined"
                    } else {
                        "null"
                    };
                    return Some(Err(RequestError::new(format!(
                        "Cannot read properties of {kind} (reading 'sessionKey')"
                    ))));
                }
                if params
                    .get("sessionKey")
                    .and_then(JsValue::as_string)
                    .is_some_and(|session| self.admission.has_active_turn(session))
                {
                    let mut error =
                        RequestError::new("Cannot resume StaffDeck SOP while a turn is active.");
                    error.code = "SESSION_BUSY".into();
                    return Some(Err(error));
                }
                Some(crate::runtime::sop::resume(&self.home, &self.cwd, params).await)
            }
            "sop_status" => Some(crate::runtime::sop::status(&self.home, &self.cwd, params).await),
            "rewind_files" => Some(crate::runtime::rewind::handle(self, params).await),
            "hook_async_result" => {
                Some(crate::runtime::hooks::routes::handle(&self.async_hooks, params).await)
            }
            _ => None,
        }
    }
}

pub(crate) fn is_native_method(method: &str) -> bool {
    matches!(
        method,
        "hook_async_result"
            | "rewind_files"
            | "sop_status"
            | "sop_resume"
            | "manager_sessions"
            | "manager_browsers"
    )
}
pub(crate) fn native_request(request: &JsValue) -> bool {
    dispatch::is_request(request)
        && text_field(request, "method").is_some_and(|m| is_native_method(&m))
}
pub(crate) async fn native_route(services: &GatewayServices, request: &JsValue) -> Route {
    use crate::metadata::object;
    let method = text_field(request, "method").expect("native request method");
    let result = services
        .request(
            &method,
            request.get("params").unwrap_or(&JsValue::Undefined),
        )
        .await
        .expect("registered native method");
    let mut fields = vec![
        ("type", "response".into()),
        ("id", request.get("id").unwrap().clone()),
        ("ok", JsValue::Bool(result.is_ok())),
    ];
    match result {
        Ok(value) => fields.push(("result", value)),
        Err(error) => {
            let mut detail = pilotdeck_protocol::JsObject::new();
            detail.insert("code".into(), error.code.as_str().into());
            detail.insert("message".into(), JsValue::String(error.message));
            if let Some(validation) = error.validation {
                detail.insert("validation".into(), *validation);
            } else if let Some(details) = error.details {
                detail.insert("details".into(), *details);
            }
            fields.push(("error", JsValue::Object(detail)));
        }
    }
    Route::LocalResponse(
        pilotdeck_protocol::stringify_json(&object(fields))
            .unwrap()
            .unwrap()
            .into_bytes(),
    )
}

#[cfg(test)]
mod permission_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::NativeFiles;

    #[test]
    fn module_table_answers_supported_agents_and_leaves_unknown_unclaimed() {
        let request =
            dispatch::parse(br#"{"type":"request","id":"1","method":"supported_agents"}"#).unwrap();
        assert!(is_local(&request));
        assert!(!needs_files(&request));
        let Route::LocalResponse(bytes) = route::<NativeFiles>(&request, None) else {
            panic!("registered method must answer locally");
        };
        let reply = dispatch::parse(&bytes).unwrap();
        assert_eq!(reply.get("ok"), Some(&JsValue::Bool(true)));
        let unknown =
            dispatch::parse(br#"{"type":"request","id":"2","method":"unknown"}"#).unwrap();
        assert!(!is_local(&unknown));
        assert!(matches!(
            route::<NativeFiles>(&unknown, None),
            Route::Forward
        ));
    }
}
