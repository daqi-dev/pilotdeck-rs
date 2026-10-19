//! Router decision runtime. Ported from PilotDeck `cc7c4e65`:
//! `src/router/RouterRuntime.ts` (decision half), `router/scenario/*`,
//! `router/tokenSaver/classifyAndRoute.ts`, `router/policy/*` and
//! `router/session/SessionRouterStore.ts`.
//!
//! No provider HTTP lives here. Judge/model calls go through
//! [`RouterModelInvocation`], which A6.4-pr5 implements against real providers.
//! Decisions are plain canonical JS objects so they can be logged and replayed
//! without an intermediate projection that could drift from the baseline.
//!
//! The router never reads `pilotdeck.yaml`. Configuration arrives as the
//! already-written `config.snapshot` JSON, projected by `config/snapshot.rs`.
#![allow(clippy::result_large_err)]

use crate::host::ModuleError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

mod classify;
mod config;
mod policy;
mod scenario;
mod session;
pub use classify::is_short_continuation;
pub use config::{
    AutoOrchestrateConfig, CacheAwareSwitching, FallbackConfig, LITELLM_DEFAULT_MAX_RETRIES,
    LITELLM_INITIAL_RETRY_DELAY_MS, LITELLM_MAX_RETRY_DELAY_MS, LITELLM_RETRY_JITTER, RouterConfig,
    RouterModelRef, StatsConfig, SubagentPolicy, TierConfig, TokenSaverConfig,
};
pub use policy::{RetryDecision, retry_decide};
pub use scenario::{ScenarioResolution, SubagentDetection, detect_subagent};
pub use session::SessionRoutingState;

static UNDEFINED: JsValue = JsValue::Undefined;

/// Future returned by a router model/judge port.
pub type RouterFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, RouterInvokeError>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeErrorKind {
    /// `ModelProviderError`: carries a normalized code and retryability.
    Provider,
    /// `ModelRequestError`: local request validation; never retried.
    Request,
    /// Any other error; retried by the token-saver judge.
    Other,
}

/// Judge/model invocation failure. Mirrors the three error classes
/// `classifyAndRoute` distinguishes without importing provider specifics.
#[derive(Clone, Debug)]
pub struct RouterInvokeError {
    pub kind: InvokeErrorKind,
    pub code: Option<JsString>,
    pub message: JsString,
    pub retryable: bool,
}
impl RouterInvokeError {
    pub fn provider(
        code: impl Into<JsString>,
        message: impl Into<JsString>,
        retryable: bool,
    ) -> Self {
        Self {
            kind: InvokeErrorKind::Provider,
            code: Some(code.into()),
            message: message.into(),
            retryable,
        }
    }
    pub fn request(code: impl Into<JsString>, message: impl Into<JsString>) -> Self {
        Self {
            kind: InvokeErrorKind::Request,
            code: Some(code.into()),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn other(message: impl Into<JsString>) -> Self {
        Self {
            kind: InvokeErrorKind::Other,
            code: None,
            message: message.into(),
            retryable: true,
        }
    }
}
impl From<ModuleError> for RouterInvokeError {
    fn from(error: ModuleError) -> Self {
        Self {
            kind: InvokeErrorKind::Other,
            code: error.code,
            message: error.message,
            retryable: error.retryable.unwrap_or(true),
        }
    }
}

/// Cooperative cancellation shared with a judge port. The decision path never
/// fabricates a provider abort reason; it only observes cancellation.
///
/// A judge attempt gets its own child flag so that timing the judge out never
/// marks the caller's turn-level signal as aborted (TS keeps a separate
/// `judgeAbortController`).
#[derive(Debug, Default)]
struct AbortState {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

#[derive(Clone, Debug, Default)]
pub struct AbortFlag(Arc<AbortState>);
impl AbortFlag {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn abort(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }
    pub fn is_aborted(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }
    /// Resolves once the flag is aborted. Registration happens before the flag
    /// check so a concurrent `abort()` cannot be missed.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// Model capability + multimodal + judge port. A6.4-pr5 supplies the provider
/// implementation; tests inject deterministic fakes.
pub trait RouterModelInvocation: Send + Sync {
    /// Throws when the provider/model is unknown, like `ModelRuntime`.
    fn get_capabilities(
        &self,
        provider: &JsString,
        model: &JsString,
    ) -> Result<JsValue, ModuleError>;
    fn get_multimodal(&self, provider: &JsString, model: &JsString)
    -> Result<JsValue, ModuleError>;
    fn get_provider_generation(&self, _provider: &JsString) -> JsValue {
        JsValue::Undefined
    }
    /// Whether [`complete`](Self::complete) is implemented.
    fn supports_complete(&self) -> bool {
        false
    }
    fn complete(&self, _request: JsValue, _abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        Box::pin(async {
            Err(RouterInvokeError::other(
                "Router judge invocation provider does not implement complete().",
            ))
        })
    }
}

/// Router event sink. Events are diagnostic only; decisions do not depend on
/// them, and a no-op sink is valid.
pub trait RouterEventSink: Send + Sync {
    fn emit(&self, event: JsValue);
}
impl<F> RouterEventSink for F
where
    F: Fn(JsValue) + Send + Sync,
{
    fn emit(&self, event: JsValue) {
        self(event)
    }
}
pub struct NoopEvents;
impl RouterEventSink for NoopEvents {
    fn emit(&self, _event: JsValue) {}
}

pub trait CustomRouter: Send + Sync {
    fn decide(&self, input: JsValue) -> RouterFuture<JsValue>;
}
pub trait CustomRouterRegistry: Send + Sync {
    fn lookup_router(
        &self,
        extension_id: &JsString,
        session_id: &JsString,
    ) -> Option<Arc<dyn CustomRouter>>;
}
pub struct NoopCustomRouters;
impl CustomRouterRegistry for NoopCustomRouters {
    fn lookup_router(
        &self,
        _extension_id: &JsString,
        _session_id: &JsString,
    ) -> Option<Arc<dyn CustomRouter>> {
        None
    }
}

/// Gateway/SDK model policy, evaluated by `validateModelSelection` and later by
/// `execute`. The decision path itself does not reject, matching TS.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManagedModelPolicy {
    pub allow: Vec<JsString>,
    pub deny: Vec<JsString>,
}

#[derive(Clone)]
pub struct RouterDeps {
    pub model_invoker: Arc<dyn RouterModelInvocation>,
    pub custom_routers: Arc<dyn CustomRouterRegistry>,
    pub events: Arc<dyn RouterEventSink>,
    pub clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub is_model_allowed: Arc<dyn Fn(&RouterModelRef) -> bool + Send + Sync>,
}
impl RouterDeps {
    pub fn new(model_invoker: Arc<dyn RouterModelInvocation>) -> Self {
        Self {
            model_invoker,
            custom_routers: Arc::new(NoopCustomRouters),
            events: Arc::new(NoopEvents),
            // `RouterRuntime.ts` defaults to `() => new Date()` (wall clock).
            clock: Arc::new(wall_clock),
            is_model_allowed: Arc::new(|_| true),
        }
    }
}

/// Milliseconds since the Unix epoch, matching `Date.now()`.
pub fn wall_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterRuntimeError {
    pub code: &'static str,
    pub message: JsString,
}
impl RouterRuntimeError {
    pub fn new(code: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for RouterRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.code,
            String::from_utf16_lossy(self.message.units())
        )
    }
}
impl std::error::Error for RouterRuntimeError {}

#[derive(Clone, Debug)]
pub struct RouterDecisionInput {
    pub request: JsValue,
    pub session_id: JsString,
    pub is_main_agent: bool,
    pub abort: Option<AbortFlag>,
    pub metadata: Option<JsObject>,
}

pub struct RouterRuntime {
    config: RouterConfig,
    enabled: bool,
    invoker: Arc<dyn RouterModelInvocation>,
    custom_routers: Arc<dyn CustomRouterRegistry>,
    events: Arc<dyn RouterEventSink>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    is_model_allowed: Arc<dyn Fn(&RouterModelRef) -> bool + Send + Sync>,
    store: session::SessionRouterStore,
    usage: Mutex<Vec<(JsString, JsValue)>>,
}

impl RouterRuntime {
    /// `createRouterRuntime`: token saving needs a `complete()`-capable judge
    /// provider even before the first decision.
    pub fn new(
        config: RouterConfig,
        invoker: Arc<dyn RouterModelInvocation>,
    ) -> Result<Self, RouterRuntimeError> {
        Self::with_deps(config, RouterDeps::new(invoker))
    }

    pub fn with_deps(config: RouterConfig, deps: RouterDeps) -> Result<Self, RouterRuntimeError> {
        if config
            .token_saver
            .as_ref()
            .is_some_and(|saver| saver.enabled)
            && !deps.model_invoker.supports_complete()
        {
            return Err(RouterRuntimeError::new(
                "judge_invoker_missing",
                "Token-saver routing requires a judge invocation provider with complete().",
            ));
        }
        let enabled = config.enabled;
        let clock = deps.clock.clone();
        Ok(Self {
            config,
            enabled,
            invoker: deps.model_invoker,
            custom_routers: deps.custom_routers,
            events: deps.events,
            clock: clock.clone(),
            is_model_allowed: deps.is_model_allowed,
            store: session::SessionRouterStore::new(clock),
            usage: Mutex::new(Vec::new()),
        })
    }

    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    fn assert_model_allowed(&self, model: &RouterModelRef) -> Result<(), RouterRuntimeError> {
        if (self.is_model_allowed)(model) {
            return Ok(());
        }
        Err(RouterRuntimeError::new(
            "MODEL_POLICY_DENIED",
            join_message("Gateway model policy denies ", model, "."),
        ))
    }

    /// `validateModelSelection`: host policy, then SDK managed policy, then a
    /// capability lookup whose error is surfaced unchanged.
    pub fn validate_model_selection(
        &self,
        provider: &JsString,
        model: &JsString,
        policy: Option<&ManagedModelPolicy>,
    ) -> Result<(), RouterRuntimeError> {
        let reference = RouterModelRef::new(provider.clone(), model.clone());
        self.assert_model_allowed(&reference)?;
        if let Some(policy) = policy
            && !managed_model_allowed(policy, &reference)
        {
            return Err(RouterRuntimeError::new(
                "SDK_MANAGED_MODEL_DENIED",
                join_message("SDK managedSettings.models denies model ", &reference, "."),
            ));
        }
        self.invoker
            .get_capabilities(provider, model)
            .map(|_| ())
            .map_err(|error| RouterRuntimeError::new("model_capabilities_failed", error.message))
    }

    pub async fn decide(&self, input: &RouterDecisionInput) -> Result<JsValue, RouterRuntimeError> {
        if !self.enabled {
            return Ok(disabled_decision(input));
        }

        let sticky = self.store.get(&input.session_id, !input.is_main_agent);
        let base_usage = self.get_session_usage(&input.session_id);
        let input_with_usage = self.with_usage_hint(input, base_usage.as_ref());

        let custom = self.resolve_custom(&input_with_usage).await;
        let scenario_outcome = scenario::decide_scenario(&input_with_usage, &self.config);

        let mut scenario_type = scenario_outcome.scenario_type;
        let previous_sticky_selection = input
            .metadata
            .as_ref()
            .and_then(|metadata| {
                let provider = metadata
                    .get("previousProvider")
                    .and_then(JsValue::as_string)?;
                let model = metadata.get("previousModel").and_then(JsValue::as_string)?;
                (truthy(&JsValue::String(provider.clone()))
                    && truthy(&JsValue::String(model.clone())))
                .then(|| RouterModelRef::new(provider.clone(), model.clone()))
            })
            .or_else(|| {
                let provider = sticky.as_ref()?.sticky_provider.clone()?;
                let model = sticky.as_ref()?.sticky_model.clone()?;
                Some(RouterModelRef::new(provider, model))
            });

        let custom_selection = custom.as_ref().and_then(|decision| {
            let provider = decision.get("provider").and_then(JsValue::as_string)?;
            let model = decision.get("model").and_then(JsValue::as_string)?;
            (truthy(&JsValue::String(provider.clone())) && truthy(&JsValue::String(model.clone())))
                .then(|| RouterModelRef::new(provider.clone(), model.clone()))
        });
        let mut selection = custom_selection.or_else(|| scenario_outcome.selection.clone());

        let mut resolved_from: &'static str = if custom
            .as_ref()
            .and_then(|decision| decision.get("provider"))
            .and_then(JsValue::as_string)
            .is_some_and(|provider| truthy(&JsValue::String(provider.clone())))
        {
            "custom"
        } else if scenario_type == "explicit" {
            "explicit"
        } else {
            "scenario"
        };

        let mut token_saver_tier: Option<JsString> = None;
        let mut cache_aware_switch: Option<JsValue> = None;
        let subagent_policy = self
            .config
            .token_saver
            .as_ref()
            .map(|saver| saver.subagent_policy)
            .unwrap_or(SubagentPolicy::Judge);
        let token_saver_enabled = self
            .config
            .token_saver
            .as_ref()
            .is_some_and(|saver| saver.enabled);
        let custom_has_provider = custom
            .as_ref()
            .and_then(|decision| decision.get("provider"))
            .and_then(JsValue::as_string)
            .is_some_and(|provider| truthy(&JsValue::String(provider.clone())));

        if !custom_has_provider
            && scenario_type != "explicit"
            && token_saver_enabled
            && (input.is_main_agent || subagent_policy != SubagentPolicy::Skip)
        {
            let messages = request_messages(&input_with_usage.request);
            let mut sticky_hit = false;

            if input.is_main_agent
                && messages.len() > 1
                && let Some(main_sticky) = self.store.get(&input.session_id, false)
                && let (Some(provider), Some(model)) = (
                    main_sticky.sticky_provider.clone(),
                    main_sticky.sticky_model.clone(),
                )
            {
                selection = Some(RouterModelRef::new(provider, model));
                resolved_from = "tokenSaver";
                token_saver_tier = main_sticky.token_saver_tier.clone();
                sticky_hit = true;
            }

            if !input.is_main_agent
                && subagent_policy == SubagentPolicy::Judge
                && messages.len() > 1
                && let Some(sub_sticky) = self.store.get(&input.session_id, true)
                && let (Some(provider), Some(model)) = (
                    sub_sticky.sticky_provider.clone(),
                    sub_sticky.sticky_model.clone(),
                )
            {
                selection = Some(RouterModelRef::new(provider, model));
                resolved_from = "tokenSaver";
                token_saver_tier = sub_sticky.token_saver_tier.clone();
                sticky_hit = true;
            }

            if !sticky_hit {
                let saver = self
                    .config
                    .token_saver
                    .clone()
                    .expect("enabled token saver");
                let previous_tier = input_with_usage
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("previousTier"))
                    .and_then(JsValue::as_string)
                    .cloned();
                let outcome = classify::classify_and_route(
                    &saver,
                    &messages,
                    self.invoker.as_ref(),
                    input.abort.clone(),
                    previous_tier.as_ref(),
                    &input.session_id,
                )
                .await
                .map_err(|error| {
                    RouterRuntimeError::new("router_decision_failed", error.message)
                })?;
                if let Some(outcome) = outcome {
                    if let Some(reason) = outcome.failure_reason {
                        let mut event = JsObject::new();
                        event.insert("type".into(), "pilotdeck_router_token_saver_failed".into());
                        event.insert(
                            "sessionId".into(),
                            JsValue::String(input.session_id.clone()),
                        );
                        event.insert("reason".into(), JsValue::String(reason.into()));
                        event.insert("fallbackTier".into(), JsValue::String(outcome.tier.clone()));
                        event.insert(
                            "judgeProvider".into(),
                            JsValue::String(saver.judge.provider.clone()),
                        );
                        event.insert(
                            "judgeModel".into(),
                            JsValue::String(saver.judge.model.clone()),
                        );
                        event.insert(
                            "attempts".into(),
                            JsValue::Number(outcome.failure.as_ref().map_or(1.0, |f| f.attempts)),
                        );
                        if let Some(failure) = &outcome.failure {
                            if let Some(code) = &failure.code {
                                event.insert("errorCode".into(), JsValue::String(code.clone()));
                            }
                            if let Some(message) = &failure.message {
                                event.insert(
                                    "errorMessage".into(),
                                    JsValue::String(message.clone()),
                                );
                            }
                        }
                        self.events.emit(JsValue::Object(event));
                    }
                    if let Some(next) = outcome.selection.clone() {
                        resolved_from = "tokenSaver";
                        let cache_aware = policy::cache_policy_select(
                            &self.config,
                            previous_sticky_selection.as_ref(),
                            &next,
                            &messages,
                            base_usage.as_ref(),
                        )
                        .await
                        .map_err(|error| {
                            RouterRuntimeError::new("router_decision_failed", error.message)
                        })?;
                        selection = Some(cache_aware.selection);
                        cache_aware_switch = cache_aware.mutation;
                        token_saver_tier = if cache_aware_switch
                            .as_ref()
                            .and_then(|mutation| mutation.get("action"))
                            .and_then(JsValue::as_string)
                            .is_some_and(|action| action.equals_str("kept_sticky"))
                        {
                            sticky
                                .as_ref()
                                .and_then(|state| state.token_saver_tier.clone())
                                .or_else(|| {
                                    input_with_usage
                                        .metadata
                                        .as_ref()
                                        .and_then(|metadata| metadata.get("previousTier"))
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                })
                                .or_else(|| Some(outcome.tier.clone()))
                        } else {
                            Some(outcome.tier.clone())
                        };
                    }
                }
            }
        }

        if selection.is_none()
            && let Some(hint) = &scenario_outcome.subagent_model_hint
            && let Some(slash) = hint.units().iter().position(|unit| *unit == 47)
        {
            let provider = JsString::from_utf16(hint.units()[..slash].to_vec());
            let model = JsString::from_utf16(hint.units()[slash + 1..].to_vec());
            if truthy(&JsValue::String(provider.clone())) && truthy(&JsValue::String(model.clone()))
            {
                selection = Some(RouterModelRef::new(provider, model));
                resolved_from = "explicit";
            }
        }

        if selection.is_none() {
            selection = self.config.scenarios_default.clone();
            if scenario_type != "explicit" {
                scenario_type = "default";
            }
        }

        let Some(selection) = selection else {
            return Err(RouterRuntimeError::new(
                "router_no_default",
                "Router: no default scenario configured and no model could be resolved",
            ));
        };

        let mut decision = JsObject::new();
        decision.insert(
            "provider".into(),
            JsValue::String(selection.provider.clone()),
        );
        decision.insert("model".into(), JsValue::String(selection.model.clone()));
        decision.insert("scenarioType".into(), JsValue::String(scenario_type.into()));
        if let Some(tier) = &token_saver_tier {
            decision.insert("tokenSaverTier".into(), JsValue::String(tier.clone()));
        }
        decision.insert(
            "isSubagent".into(),
            JsValue::Bool(scenario_outcome.is_subagent),
        );
        decision.insert("orchestrating".into(), JsValue::Bool(false));
        decision.insert("resolvedFrom".into(), JsValue::String(resolved_from.into()));

        let already_orchestrating = sticky.as_ref().is_some_and(|state| state.orchestrating);
        let token_saver_active = token_saver_enabled && token_saver_tier.is_some();
        let orch_gate = token_saver_active || already_orchestrating;

        let mut mutations = JsObject::new();
        if let Some(cache_aware) = cache_aware_switch {
            mutations.insert("cacheAwareSwitch".into(), cache_aware);
        }
        if orch_gate {
            let orchestrated = policy::orchestration_decide(
                self.config.auto_orchestrate.as_ref(),
                input.is_main_agent,
                token_saver_tier.as_ref(),
                already_orchestrating,
            );
            if orchestrated.applied {
                if let Some(activated) = orchestrated.mutations.get("orchestrationActivated") {
                    mutations.insert("orchestrationActivated".into(), activated.clone());
                }
                decision.insert("orchestrating".into(), JsValue::Bool(true));
            }
        }

        if scenario_outcome.subagent_model_hint.is_some() || scenario_outcome.is_subagent {
            mutations.insert("subagentTagStripped".into(), JsValue::Bool(true));
        }

        let media_messages = request_messages(&input_with_usage.request);
        scenario::reroute_decision_for_media(
            &self.config,
            self.invoker.as_ref(),
            &mut decision,
            &media_messages,
            &mut mutations,
        );

        decision.insert("mutations".into(), JsValue::Object(mutations));

        let mut state = SessionRoutingState {
            session_id: input.session_id.clone(),
            is_subagent: !input.is_main_agent,
            token_saver_tier: token_saver_tier.clone(),
            sticky_provider: Some(
                decision
                    .get("provider")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .expect("decision provider"),
            ),
            sticky_model: Some(
                decision
                    .get("model")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .expect("decision model"),
            ),
            orchestrating: decision
                .get("orchestrating")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            last_usage: sticky.as_ref().and_then(|state| state.last_usage.clone()),
            updated_at: (self.clock)(),
        };
        // A decision always writes the caller's state slot.
        state.is_subagent = !input.is_main_agent;
        self.store.set(state);

        let mut event = JsObject::new();
        event.insert("type".into(), "pilotdeck_router_decision".into());
        event.insert(
            "sessionId".into(),
            JsValue::String(input.session_id.clone()),
        );
        event.insert("decision".into(), JsValue::Object(decision.clone()));
        self.events.emit(JsValue::Object(event));

        Ok(JsValue::Object(decision))
    }

    fn with_usage_hint(
        &self,
        input: &RouterDecisionInput,
        base: Option<&JsValue>,
    ) -> RouterDecisionInput {
        let mut metadata = input.metadata.clone().unwrap_or_default();
        if metadata.get("lastUsage").is_none() {
            let mut hint = JsObject::new();
            for key in ["inputTokens", "outputTokens", "totalTokens"] {
                hint.insert(
                    key.into(),
                    base.and_then(|usage| usage.get(key))
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
            }
            metadata.insert("lastUsage".into(), JsValue::Object(hint));
        }
        RouterDecisionInput {
            request: input.request.clone(),
            session_id: input.session_id.clone(),
            is_main_agent: input.is_main_agent,
            abort: input.abort.clone(),
            metadata: Some(metadata),
        }
    }

    async fn resolve_custom(&self, input: &RouterDecisionInput) -> Option<JsValue> {
        let configured = self.config.custom_router.as_ref()?;
        let router = self
            .custom_routers
            .lookup_router(configured, &input.session_id)?;
        let mut context = JsObject::new();
        context.insert(
            "sessionId".into(),
            JsValue::String(input.session_id.clone()),
        );
        context.insert("isMainAgent".into(), JsValue::Bool(input.is_main_agent));
        let scenarios = self
            .config
            .scenario_names()
            .into_iter()
            .map(JsValue::String)
            .collect();
        context.insert("scenarios".into(), JsValue::Array(scenarios));
        let mut call = JsObject::new();
        call.insert("request".into(), input.request.clone());
        call.insert(
            "sessionId".into(),
            JsValue::String(input.session_id.clone()),
        );
        call.insert("isMainAgent".into(), JsValue::Bool(input.is_main_agent));
        if let Some(metadata) = &input.metadata {
            call.insert("metadata".into(), JsValue::Object(metadata.clone()));
        }
        call.insert("context".into(), JsValue::Object(context));
        match router.decide(JsValue::Object(call)).await {
            Ok(value) => Some(value),
            Err(error) => {
                let mut event = JsObject::new();
                event.insert("type".into(), "pilotdeck_router_custom_failed".into());
                event.insert(
                    "sessionId".into(),
                    JsValue::String(input.session_id.clone()),
                );
                event.insert("extensionId".into(), JsValue::String(configured.clone()));
                event.insert("reason".into(), JsValue::String(error.message));
                self.events.emit(JsValue::Object(event));
                None
            }
        }
    }

    /// Apply a decision to a canonical request without invoking a provider.
    pub fn materialize_request(&self, decision: &JsValue, request: &JsValue) -> JsValue {
        policy::materialize_request(self.invoker.as_ref(), decision, request)
    }

    /// Clear sticky provider/model/tier for a session while preserving
    /// orchestration, like `RouterRuntime.invalidateSticky`.
    pub fn invalidate_sticky(&self, session_id: &JsString) -> JsValue {
        if !self.enabled {
            let mut result = JsObject::new();
            result.insert("orchestrating".into(), JsValue::Bool(false));
            return JsValue::Object(result);
        }
        // TS `invalidateSticky` is synchronous, so `get` + `set` cannot be
        // interleaved by another turn. Rust needs one lock-held read/modify/write.
        let current = self.store.update(session_id, |current| {
            let previous_tier = current
                .as_ref()
                .and_then(|state| state.token_saver_tier.clone());
            let orchestrating = current.as_ref().is_some_and(|state| state.orchestrating);
            let preserve = orchestrating
                && previous_tier
                    .as_ref()
                    .is_some_and(|tier| !tier.units().is_empty());
            SessionRoutingState {
                session_id: session_id.clone(),
                is_subagent: false,
                orchestrating,
                token_saver_tier: if preserve {
                    previous_tier.clone()
                } else {
                    None
                },
                sticky_provider: if preserve {
                    current
                        .as_ref()
                        .and_then(|state| state.sticky_provider.clone())
                } else {
                    None
                },
                sticky_model: if preserve {
                    current
                        .as_ref()
                        .and_then(|state| state.sticky_model.clone())
                } else {
                    None
                },
                last_usage: None,
                updated_at: (self.clock)(),
            }
        });
        let previous_tier = current
            .as_ref()
            .and_then(|state| state.token_saver_tier.clone());
        let previous_provider = current
            .as_ref()
            .and_then(|state| state.sticky_provider.clone());
        let previous_model = current
            .as_ref()
            .and_then(|state| state.sticky_model.clone());
        let orchestrating = current.as_ref().is_some_and(|state| state.orchestrating);
        let mut result = JsObject::new();
        if let Some(tier) = previous_tier {
            result.insert("previousTier".into(), JsValue::String(tier));
        }
        if let Some(provider) = previous_provider {
            result.insert("previousProvider".into(), JsValue::String(provider));
        }
        if let Some(model) = previous_model {
            result.insert("previousModel".into(), JsValue::String(model));
        }
        result.insert("orchestrating".into(), JsValue::Bool(orchestrating));
        JsValue::Object(result)
    }

    pub fn observe_usage(&self, session_id: &JsString, usage: Option<&JsValue>) {
        if !self.enabled {
            return;
        }
        let Some(usage) = usage.filter(|usage| !usage.is_undefined()) else {
            return;
        };
        let mut cache = self.usage.lock().expect("router usage cache");
        if let Some(index) = cache.iter().position(|(key, _)| key == session_id) {
            cache.remove(index);
        } else if cache.len() >= 500 {
            cache.remove(0);
        }
        cache.push((session_id.clone(), usage.clone()));
    }

    pub fn get_session_usage(&self, session_id: &JsString) -> Option<JsValue> {
        self.usage
            .lock()
            .expect("router usage cache")
            .iter()
            .find(|(key, _)| key == session_id)
            .map(|(_, usage)| usage.clone())
    }

    pub fn estimate_usage_cost(
        &self,
        usage: Option<&JsValue>,
        provider: &JsString,
        model: &JsString,
    ) -> Option<f64> {
        let usage = usage.and_then(usage_from_js);
        let provider = provider.to_utf8().unwrap_or_default();
        let model = model.to_utf8().unwrap_or_default();
        let configured = self.config.stats.model_pricing.as_ref();
        Some(crate::model_cost::estimate_cost(
            usage.as_ref(),
            &provider,
            &model,
            configured,
        ))
    }
}

fn disabled_decision(input: &RouterDecisionInput) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert(
        "provider".into(),
        request_field(&input.request, "provider")
            .as_string()
            .cloned()
            .map(JsValue::String)
            .unwrap_or(JsValue::Undefined),
    );
    decision.insert(
        "model".into(),
        request_field(&input.request, "model")
            .as_string()
            .cloned()
            .map(JsValue::String)
            .unwrap_or(JsValue::Undefined),
    );
    decision.insert("scenarioType".into(), "default".into());
    decision.insert("isSubagent".into(), JsValue::Bool(!input.is_main_agent));
    decision.insert("orchestrating".into(), JsValue::Bool(false));
    decision.insert("resolvedFrom".into(), "scenario".into());
    decision.insert("mutations".into(), JsValue::Object(JsObject::new()));
    JsValue::Object(decision)
}

fn managed_model_allowed(policy: &ManagedModelPolicy, model: &RouterModelRef) -> bool {
    let provider = model.provider.to_utf8().unwrap_or_default();
    let name = model.model.to_utf8().unwrap_or_default();
    let matches = |selector: &JsString| {
        let selector = selector.to_utf8().unwrap_or_default();
        selector == "*"
            || selector == format!("{provider}/*")
            || selector == format!("{provider}/{name}")
    };
    if policy.deny.iter().any(matches) {
        return false;
    }
    policy.allow.is_empty() || policy.allow.iter().any(matches)
}

fn join_message(prefix: &str, model: &RouterModelRef, suffix: &str) -> JsString {
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(model.provider.units());
    units.push(47);
    units.extend_from_slice(model.model.units());
    units.extend(suffix.encode_utf16());
    JsString::from_utf16(units)
}

fn request_field<'a>(request: &'a JsValue, key: &str) -> &'a JsValue {
    request.get(key).unwrap_or(&UNDEFINED)
}

pub(super) fn request_messages(request: &JsValue) -> Vec<JsValue> {
    match request_field(request, "messages") {
        JsValue::Array(items) => items.clone(),
        _ => Vec::new(),
    }
}

pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
pub(super) fn object_field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsObject> {
    field(value, key).as_object()
}
pub(super) fn js_text(value: &JsValue) -> Option<&JsString> {
    value.as_string()
}
pub(super) fn number(value: &JsValue) -> Option<f64> {
    value.as_number()
}
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}

/// JS `String.prototype.trim` over UTF-16 units (keeps lone surrogates).
pub(super) fn trim_units(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|unit| !is_js_whitespace(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !is_js_whitespace(*unit))
        .map_or(start, |index| index + 1);
    &units[start..end]
}
fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        9 | 10
            | 11
            | 12
            | 13
            | 32
            | 0xa0
            | 0x1680
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    ) || (0x2000..=0x200a).contains(&unit)
}

/// JS `trimEnd`: only the right edge is stripped.
pub(super) fn trim_end_units(units: &[u16]) -> &[u16] {
    let end = units
        .iter()
        .rposition(|unit| !is_js_whitespace(*unit))
        .map_or(0, |index| index + 1);
    &units[..end]
}

pub(super) fn trim_js(value: &JsString) -> JsString {
    JsString::from_utf16(trim_units(value.units()).to_vec())
}

pub(super) fn usage_from_js(value: &JsValue) -> Option<pilotdeck_protocol::model::Usage> {
    let object = value.as_object()?;
    let number = |key: &str| object.get(key).and_then(JsValue::as_number);
    Some(pilotdeck_protocol::model::Usage {
        input_tokens: number("inputTokens"),
        output_tokens: number("outputTokens"),
        cache_read_tokens: number("cacheReadTokens"),
        cache_write_tokens: number("cacheWriteTokens"),
        total_tokens: number("totalTokens"),
        native_cost: number("nativeCost"),
    })
}

#[cfg(test)]
#[path = "router/tests.rs"]
mod tests;
