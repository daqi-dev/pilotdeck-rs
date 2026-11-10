//! Fixed createLocalGateway public capabilities over the production project owners.
use super::{Options, methods::session_dir::session_io_permit, sop_control::NativeSopControl};
use crate::{
    config::{self, Env},
    metadata::object,
    module_host::{
        HostBody, HostFailure, HostResponse, PublicHostPrincipal, PublicHostProvider,
        PublicHostResolver,
    },
    runtime::project::{ProjectResources, ProjectRuntimeRegistry, RuntimeLease},
    skills::{
        SkillManager, SkillManagerOptions, SkillMethod, SkillsFacade, skill_binding_from_snapshot,
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};
type Result<T> = std::result::Result<T, HostFailure>;
fn value<'a>(input: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    input.get(key).unwrap_or(&UNDEFINED)
}
fn failure(status: u16, code: &str, message: impl Into<String>) -> HostFailure {
    HostFailure {
        status: Some(status),
        code: Some(code.into()),
        message: Some(message.into()),
    }
}
fn internal(message: impl Into<String>) -> HostFailure {
    HostFailure {
        status: None,
        code: None,
        message: Some(message.into()),
    }
}
fn config_error(error: config::Error) -> HostFailure {
    HostFailure {
        status: None,
        code: Some(error.code),
        message: Some(String::from_utf16_lossy(error.message.units())),
    }
}
fn response(body: JsValue) -> HostResponse {
    HostResponse {
        status: 200,
        headers: vec![],
        body: HostBody::Value(body),
        raw_body: None,
    }
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = session_io_permit()
        .await
        .map_err(|e| internal(String::from_utf16_lossy(e.message.units())))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|e| internal(e.to_string()))?
}
#[derive(Clone)]
pub(super) struct Resolver {
    options: Options,
    env: Env,
    registry: Arc<ProjectRuntimeRegistry>,
    sop: Arc<NativeSopControl>,
}
impl Resolver {
    pub(super) fn new(
        options: Options,
        env: Env,
        registry: Arc<ProjectRuntimeRegistry>,
        sop: Arc<NativeSopControl>,
    ) -> Self {
        Self {
            options,
            env,
            registry,
            sop,
        }
    }
    async fn generation(&self, root: PathBuf) -> Result<Arc<RuntimeLease>> {
        let registry = self.registry.clone();
        blocking(move || {
            registry
                .acquire(Some(&root))
                .map(Arc::new)
                .map_err(|error| match error.js_error() {
                    Some(js) => HostFailure {
                        status: None,
                        code: js.code.clone(),
                        message: Some(String::from_utf16_lossy(js.message.units())),
                    },
                    None => internal(error.message),
                })
        })
        .await
    }
}
impl PublicHostResolver for Resolver {
    fn resolve(
        &self,
        principal: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn PublicHostProvider>>> + Send + '_>> {
        let principal = principal.clone();
        Box::pin(async move {
            let home = self.options.home.clone();
            let env = self.env.clone();
            let bound = blocking(move || bound_project(&home, &env, &principal)).await?;
            let generation = self
                .generation(
                    bound
                        .clone()
                        .unwrap_or_else(|| self.options.project.clone()),
                )
                .await?;
            let resources = generation
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| internal("Public host project resources are unavailable."))?;
            let snapshot = resources.context().snapshot();
            let skills = value(value(value(&snapshot, "config"), "modules"), "skills");
            let skills_enabled = value(skills, "enabled").as_bool() != Some(false);
            Ok(Arc::new(Provider {
                resolver: self.clone(),
                bound,
                generation,
                snapshot,
                skills_enabled,
            }) as Arc<dyn PublicHostProvider>)
        })
    }
}
struct Provider {
    resolver: Resolver,
    bound: Option<PathBuf>,
    generation: Arc<RuntimeLease>,
    snapshot: Arc<JsValue>,
    skills_enabled: bool,
}
impl Provider {
    async fn catalog(&self) -> Result<JsValue> {
        // Fixed TS NativeSessionModelSelectionPolicy reads global config afresh;
        // projectKey does not select a project override for this catalog port.
        // The selected project lease still owns the actual model execution.
        let home = self.resolver.options.home.clone();
        let env = self.resolver.env.clone();
        blocking(move || {
            let config = config::load_project_bounded_lazy(&home, &PathBuf::new(), &env, None)
                .map_err(config_error)?;
            crate::model_catalog::execute_loaded(
                config,
                &object([("includeAuto", JsValue::Bool(false))]),
                &env,
            )
            .map_err(config_error)
        })
        .await
    }
    async fn skills(&self) -> Result<HostResponse> {
        let options = self.resolver.options.clone();
        let env = self.resolver.env.clone();
        let root = self.generation.runtime().project_root().to_path_buf();
        // Retain the selected project until the blocking catalog read has ended.
        let generation = self.generation.clone();
        let snapshot = self.snapshot.clone();
        let runtime = tokio::runtime::Handle::current();
        blocking(move || {
            let _generation = generation;
            let mut settings =
                SkillManagerOptions::new(options.home.to_string_lossy().into_owned());
            settings.builtin_skills_root =
                super::assembly::explicit_builtin_skills_root(&options, &env)
                    .map(|root| root.to_string_lossy().into_owned());
            settings.env = Some(env.clone());
            settings.home_dir = Some(options.home.to_string_lossy().into_owned());
            let skill_error = |e: crate::skills::SkillError| HostFailure {
                status: Some(502),
                code: Some(String::from_utf16_lossy(e.code.units())),
                message: Some(String::from_utf16_lossy(e.message.units())),
            };
            let manager = SkillManager::new(settings).map_err(skill_error)?;
            let proxy = snapshot
                .get("config")
                .and_then(|config| config.get("proxy"))
                .and_then(crate::runtime::model::http::ProxySettings::from_config);
            let transport = Arc::new(
                crate::skills::HyperModuleTransport::skills_with_proxy_settings(
                    proxy.as_ref(),
                    &env,
                ),
            );
            let facade = SkillsFacade::with_transport(
                manager,
                move |_| skill_binding_from_snapshot(&snapshot),
                transport,
            );
            let mut result = runtime
                .block_on(facade.call(
                    SkillMethod::List,
                    &object([("projectKey", root.to_string_lossy().as_ref().into())]),
                ))
                .map_err(skill_error)?;
            if let JsValue::Object(result) = &mut result
                && let Some(JsValue::Array(items)) = result.get("items")
            {
                let items = items.clone();
                result.insert(
                    "items".into(),
                    JsValue::Array(
                        items
                            .into_iter()
                            .map(|mut item| {
                                let slug = value(&item, "slug").clone();
                                if let JsValue::Object(fields) = &mut item {
                                    fields.insert("id".into(), slug);
                                }
                                item
                            })
                            .collect(),
                    ),
                );
            }
            let mut body = JsObject::new();
            body.insert("data".into(), value(&result, "items").clone());
            body.insert(
                "next_cursor".into(),
                match value(&result, "nextCursor") {
                    JsValue::Undefined | JsValue::Null => JsValue::Null,
                    value => value.clone(),
                },
            );
            if let JsValue::Object(result) = &result {
                for (key, value) in result.entries() {
                    body.insert(key.clone(), value.clone());
                }
            }
            Ok(response(JsValue::Object(body)))
        })
        .await
    }
}
impl PublicHostProvider for Provider {
    fn operations(&self) -> Vec<String> {
        let mut operations = vec![];
        if self.skills_enabled {
            operations.push("list_general_skills".into());
        }
        operations.extend(
            [
                "list_model_catalog",
                "model_prepare",
                "model_stream",
                "file_parse",
                "read_sop_authority",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        operations
    }
    fn call(
        &self,
        operation: &str,
        input: &JsValue,
        principal: &PublicHostPrincipal,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse>> + Send + '_>> {
        self.invoke(operation, input, principal, None)
    }
    fn call_with_signal(
        &self,
        operation: &str,
        input: &JsValue,
        principal: &PublicHostPrincipal,
        signal: crate::runtime::model::router::AbortFlag,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse>> + Send + '_>> {
        self.invoke(operation, input, principal, Some(signal))
    }
}
impl Provider {
    fn invoke(
        &self,
        operation: &str,
        input: &JsValue,
        principal: &PublicHostPrincipal,
        signal: Option<crate::runtime::model::router::AbortFlag>,
    ) -> Pin<Box<dyn Future<Output = Result<HostResponse>> + Send + '_>> {
        let operation = operation.to_owned();
        let mut input = input.clone();
        let principal = principal.clone();
        Box::pin(async move {
            if signal
                .as_ref()
                .is_some_and(crate::runtime::model::router::AbortFlag::is_aborted)
            {
                let mut cancelled = response(object([("code", "PUBLIC_HOST_CANCELLED".into())]));
                cancelled.status = 499;
                return Ok(cancelled);
            }
            match operation.as_str() {
                "list_general_skills" if self.skills_enabled => self.skills().await,
                "list_model_catalog" => {
                    let result = self.catalog().await?;
                    let selected = value(&result, "defaultSelection");
                    let items = match value(&result, "items") {
                        JsValue::Array(items) => items,
                        _ => return Err(internal("Model catalog items are unavailable.")),
                    };
                    let data = items
                        .iter()
                        .cloned()
                        .map(|mut item| {
                            let name = value(&item, "displayName").clone();
                            let enabled = value(&item, "available").clone();
                            let is_default = value(&item, "provider")
                                == value(selected, "provider")
                                && value(&item, "model") == value(selected, "model");
                            if let JsValue::Object(fields) = &mut item {
                                fields.insert("name".into(), name);
                                fields.insert("enabled".into(), enabled);
                                fields.insert("is_default".into(), JsValue::Bool(is_default));
                            }
                            item
                        })
                        .collect();
                    Ok(response(object([
                        ("data", JsValue::Array(data)),
                        ("defaultSelection", selected.clone()),
                    ])))
                }
                "model_prepare" | "model_stream" => {
                    let catalog = self.catalog().await?;
                    crate::public_host_model::call(
                        &input,
                        &catalog,
                        self.generation.clone(),
                        operation == "model_stream",
                        signal,
                    )
                    .await
                }
                "file_parse" => {
                    blocking(move || {
                        Ok(super::public_host_file::parse_with_abort(
                            &input,
                            signal.as_ref(),
                        ))
                    })
                    .await
                }
                "read_sop_authority" => {
                    if let Some(root) = &self.bound {
                        let expected = JsValue::from(root.to_string_lossy().as_ref());
                        let requested = value(&input, "projectKey");
                        if truthy(requested) && requested != &expected {
                            return Err(failure(
                                403,
                                "PROJECT_BINDING_SCOPE_MISMATCH",
                                "The callback belongs to another project.",
                            ));
                        }
                        if let JsValue::Object(fields) = &mut input {
                            fields.insert("projectKey".into(), expected);
                        }
                    }
                    self.resolver
                        .sop
                        .authority(&input, &principal, &self.generation)
                        .await
                        .map(response)
                        .map_err(|error| HostFailure {
                            status: error.status,
                            code: error
                                .code
                                .as_ref()
                                .and_then(JsValue::as_string)
                                .map(|s| String::from_utf16_lossy(s.units())),
                            message: error
                                .message
                                .as_ref()
                                .map(|s| String::from_utf16_lossy(s.units())),
                        })
                }
                _ => Err(failure(
                    409,
                    "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
                    "PILOTDECK_HOST_CAPABILITY_UNAVAILABLE",
                )),
            }
        })
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::String(s) => !s.is_empty(),
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        _ => true,
    }
}
fn bound_project(
    home: &Path,
    env: &Env,
    principal: &PublicHostPrincipal,
) -> Result<Option<PathBuf>> {
    if env
        .get("STAFFDECK_PROJECT_BINDING_ENABLED")
        .map(String::as_str)
        != Some("true")
    {
        return Ok(None);
    }
    for (actual, key) in [
        (&principal.tenant_id, "STAFFDECK_COPY_TENANT_ID"),
        (&principal.actor_user_id, "STAFFDECK_COPY_ACTOR_USER_ID"),
        (
            &principal.pilot_deck_user_id,
            "STAFFDECK_COPY_PILOTDECK_USER_ID",
        ),
    ] {
        if !env
            .get(key)
            .is_some_and(|expected| actual.equals_str(expected))
        {
            return Err(failure(
                403,
                "PROJECT_BINDING_PRINCIPAL_MISMATCH",
                "The StaffDeck identity is not bound.",
            ));
        }
    }
    let invalid = || {
        failure(
            409,
            "PROJECT_BINDING_STORE_INVALID",
            "StaffDeck binding storage is invalid.",
        )
    };
    let bytes =
        match config::read_setting_source_bounded(&home.join("staffdeck/project-bindings.json")) {
            Ok(bytes) => bytes,
            Err(config::BoundedSourceReadError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(failure(
                    403,
                    "PROJECT_BINDING_NOT_FOUND",
                    "No unique bound project exists.",
                ));
            }
            Err(_) => return Err(invalid()),
        };
    let store =
        pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
    if value(&store, "version").as_number() != Some(1.0) || !truthy(value(&store, "installationId"))
    {
        return Err(invalid());
    }
    let JsValue::Array(rows) = value(&store, "projects") else {
        return Err(invalid());
    };
    let origin = env
        .get("STAFFDECK_FORMAL_API_ORIGIN")
        .map(|origin| origin.strip_suffix('/').unwrap_or(origin));
    let selected: Vec<_> = rows
        .iter()
        .filter(|row| {
            value(row, "status")
                .as_string()
                .is_some_and(|s| s.equals_str("bound"))
                && value(row, "agentId").as_string() == Some(&principal.agent_id)
                && value(row, "tenantId").as_string() == Some(&principal.tenant_id)
                && origin.is_some_and(|origin| {
                    value(row, "origin")
                        .as_string()
                        .is_some_and(|s| s.equals_str(origin))
                })
        })
        .collect();
    if selected.len() != 1 {
        return Err(failure(
            403,
            "PROJECT_BINDING_NOT_FOUND",
            "No unique bound project exists.",
        ));
    }
    let root = value(selected[0], "projectRoot")
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| internal("PROJECT_BINDING_PRINCIPAL_REQUIRED"))?;
    Ok(Some(PathBuf::from(root)))
}
