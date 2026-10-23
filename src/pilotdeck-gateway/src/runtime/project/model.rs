//! A6.9-pr3b: concrete model and router families for ProjectResourcesFactory.
//! ProjectModelRuntimeBundle.ts:35-69; ProjectRouterRuntimeBundle.ts:56-100.
use super::*;
use crate::{
    host::modules::{
        model::TurnContext,
        model_port::{self, ComposedPorts, DurableRecorder, RouterModelPortOptions},
    },
    runtime::model::{
        http::{HyperModelTransport, ModelHttpTransport},
        provider_registry::{
            ModelInvocationProvider, ModelInvocationProviderRegistry, NativeModelInvocationProvider,
        },
        router::{CustomRouterRegistry, RouterConfig, RouterDeps, RouterEventSink, RouterRuntime},
        token_accounting::TokenAccountingRuntime,
    },
};
use pilotdeck_protocol::JsValue;
use std::{any::Any, sync::Arc, time::UNIX_EPOCH};

/// Provider injection retains provider-owned cleanup (unlike a legacy runtime
/// injection). Called once per project generation with that snapshot.
pub type ProviderFactory = Arc<
    dyn Fn(&JsValue) -> Result<Vec<Arc<dyn ModelInvocationProvider>>, RuntimeError> + Send + Sync,
>;

pub struct ModelFamilySource {
    pub transport: Arc<dyn ModelHttpTransport>,
    pub providers: Option<ProviderFactory>,
}
impl Default for ModelFamilySource {
    fn default() -> Self {
        Self {
            transport: Arc::new(HyperModelTransport::new()),
            providers: None,
        }
    }
}
pub struct ModelResources {
    pub providers: Arc<ModelInvocationProviderRegistry>,
    pub config: JsValue,
}
impl FamilyResource for ModelResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(self.providers.dispose())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl ResourceFamilySource for ModelFamilySource {
    fn family(&self) -> Family {
        Family::ModelRuntime
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let snapshot = context.snapshot();
        let config = snapshot
            .get("config")
            .and_then(|config| config.get("model"))
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let resource = Arc::new(ModelResources {
            providers: Arc::new(ModelInvocationProviderRegistry::new(
                tokio::runtime::Handle::current(),
            )),
            config,
        });
        // Own the registry before injected construction/registration can fail.
        sink.register_owner(self.family(), resource.clone());
        let providers = match &self.providers {
            Some(factory) => factory(&snapshot)?,
            None => resource
                .config
                .get("providers")
                .and_then(JsValue::as_object)
                .map(|providers| {
                    providers
                        .entries()
                        .iter()
                        .map(|(_, provider)| {
                            Arc::new(NativeModelInvocationProvider::new(
                                provider.clone(),
                                self.transport.clone(),
                            )) as Arc<dyn ModelInvocationProvider>
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        for provider in providers {
            resource
                .providers
                .register(provider)
                .map_err(module_error)?;
        }
        sink.register_resource(Resource::Model, resource.clone());
        sink.register_resource(Resource::ModelProviders, resource);
        Ok(())
    }
}

/// #588 supplies the project-owned custom router registry. This constructor
/// never looks up a mutable live plugin generation.
#[derive(Default)]
pub struct RouterFamilySource {
    pub custom_routers: Option<Arc<dyn CustomRouterRegistry>>,
    pub events: Option<Arc<dyn RouterEventSink>>,
}
pub struct RouterResources {
    pub router: Arc<RouterRuntime>,
    pub model_config: JsValue,
}
impl RouterResources {
    /// One call per admitted turn, using resources read through its runtime
    /// lease. Metadata and durable recording use this same frozen config.
    pub fn ports(
        &self,
        recorder: Option<DurableRecorder>,
        options: RouterModelPortOptions,
        turn: &TurnContext,
    ) -> ComposedPorts {
        model_port::ports(
            self.router.clone(),
            self.model_config.clone(),
            recorder,
            options,
            &turn.session_id,
        )
    }
}
impl FamilyResource for RouterResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.router.shutdown();
            Ok(())
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
pub struct TokenAccountingResources {
    pub accounting: Arc<TokenAccountingRuntime>,
}
impl FamilyResource for TokenAccountingResources {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl ResourceFamilySource for RouterFamilySource {
    fn family(&self) -> Family {
        Family::RouterRuntime
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let model = context
            .resource(Resource::Model)
            .ok_or_else(|| RuntimeError::new("Model resource is not staged."))?;
        let model = model
            .as_any()
            .downcast_ref::<ModelResources>()
            .ok_or_else(|| RuntimeError::new("Model resource is not a native model family."))?;
        let config = model
            .config
            .as_object()
            .cloned()
            .ok_or_else(|| RuntimeError::new("Model config must be an object."))?;
        let accounting = Arc::new(TokenAccountingResources {
            accounting: Arc::new(TokenAccountingRuntime::new(config)),
        });
        let mut deps = RouterDeps::new(model.providers.clone());
        if let Some(custom) = &self.custom_routers {
            deps.custom_routers = custom.clone();
        }
        if let Some(events) = &self.events {
            deps.events = events.clone();
        }
        let snapshot = context.snapshot();
        let now = context.clock();
        deps.clock = Arc::new(move || match now().duration_since(UNIX_EPOCH) {
            Ok(time) => time.as_millis() as f64,
            Err(error) => -(error.duration().as_millis() as f64),
        });
        let router = Arc::new(
            RouterRuntime::with_deps(project_router_config(&snapshot), deps)
                .map_err(|error| RuntimeError::new(error.to_string()))?,
        );
        let resource = Arc::new(RouterResources {
            router,
            model_config: model.config.clone(),
        });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::Router, resource);
        sink.register_resource(Resource::TokenAccounting, accounting);
        Ok(())
    }
}
fn module_error(error: crate::host::ModuleError) -> RuntimeError {
    RuntimeError::new(String::from_utf16_lossy(error.message.units()))
}

/// ProjectRuntimeRegistry.ensureRouterConfig (1755-1805). Config snapshots
/// preserve optional router absence; an absent router must stay disabled.
fn project_router_config(snapshot: &JsValue) -> RouterConfig {
    use pilotdeck_protocol::{JsObject, parse_json};
    let undefined = JsValue::Undefined;
    let config = snapshot.get("config").unwrap_or(&undefined);
    let router = config.get("router").unwrap_or(&undefined);
    if router.is_undefined()
        || matches!(router, JsValue::Null)
        || router.get("enabled") == Some(&JsValue::Bool(false))
    {
        return RouterConfig {
            enabled: false,
            ..Default::default()
        };
    }
    let agent = config
        .get("agent")
        .and_then(|agent| agent.get("model"))
        .cloned()
        .unwrap_or(JsValue::Undefined);
    let mut route = JsObject::new();
    for key in ["id", "provider", "model"] {
        route.insert(
            key.into(),
            agent.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    let route = JsValue::Object(route);
    let mut router = router.as_object().cloned().unwrap_or_default();
    let nullish = |key: &str, router: &JsObject| {
        router
            .get(key)
            .is_none_or(|value| value.is_undefined() || matches!(value, JsValue::Null))
    };
    if nullish("scenarios", &router) {
        let mut scenarios = JsObject::new();
        scenarios.insert("default".into(), route.clone());
        router.insert("scenarios".into(), JsValue::Object(scenarios));
    }
    if nullish("fallback", &router) {
        let mut fallback = JsObject::new();
        fallback.insert("default".into(), JsValue::Array(vec![route.clone()]));
        router.insert("fallback".into(), JsValue::Object(fallback));
    }
    if nullish("tokenSaver", &router) {
        let mut saver =
            parse_json(r#"{"enabled":true,"defaultTier":"medium","judgeTimeoutMs":15000}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();
        saver.insert("judge".into(), route.clone());
        let mut tiers = JsObject::new();
        for tier in ["simple", "medium", "complex", "reasoning"] {
            let mut entry = JsObject::new();
            entry.insert("model".into(), route.clone());
            tiers.insert(tier.into(), JsValue::Object(entry));
        }
        saver.insert("tiers".into(), JsValue::Object(tiers));
        router.insert("tokenSaver".into(), JsValue::Object(saver));
    }
    if nullish("autoOrchestrate", &router) {
        router.insert("autoOrchestrate".into(), parse_json(r#"{"enabled":true,"triggerTiers":["complex"],"slimSystemPrompt":true,"allowedTools":["agent","read_file","grep","glob","read_skill"]}"#).unwrap());
    }
    let mut stats = JsObject::new();
    stats.insert("enabled".into(), JsValue::Bool(true));
    stats.insert("baselineModel".into(), route);
    if let Some(configured) = router.get("stats").and_then(JsValue::as_object) {
        for (key, value) in configured.entries() {
            stats.insert(key.clone(), value.clone());
        }
    }
    router.insert("stats".into(), JsValue::Object(stats));
    let mut config = config.as_object().cloned().unwrap_or_default();
    config.insert("router".into(), JsValue::Object(router));
    let mut snapshot = JsObject::new();
    snapshot.insert("config".into(), JsValue::Object(config));
    RouterConfig::from_snapshot(&JsValue::Object(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_router_defaults_match_real_ts() {
        use pilotdeck_protocol::JsObject;
        let oracle = crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/model-factory/oracle.json"
        ));
        let JsValue::Array(cases) = oracle.get("routerConfigs").unwrap() else {
            panic!("router configs")
        };
        for case in cases {
            let actual = project_router_config(case.get("input").unwrap());
            let mut config = JsObject::new();
            config.insert("router".into(), case.get("result").unwrap().clone());
            let mut snapshot = JsObject::new();
            snapshot.insert("config".into(), JsValue::Object(config));
            let expected = RouterConfig::from_snapshot(&JsValue::Object(snapshot));
            assert_eq!(actual, expected);
        }
    }
}
