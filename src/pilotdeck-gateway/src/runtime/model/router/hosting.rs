//! Trusted production composition, matching ProjectRouterRuntimeBundle.ts:70-89
//! and ProjectRuntimeRegistry.ts:284-287,1625-1657.
use super::*;
use crate::runtime::sdk_session::{
    ModelRef, OrganizationPolicy, is_organization_model_allowed,
    organization_provider_policy_rejection,
};
use std::collections::HashMap;

pub type RouterModelPolicyAssertion =
    Arc<dyn Fn(&RouterModelRef) -> Result<(), RouterRuntimeError> + Send + Sync>;

/// Application-owned sticky state survives a project configuration reload.
/// Health, usage and contributed routers belong to the retiring generation.
#[derive(Default)]
pub struct RouterGenerationRegistry {
    stores: Mutex<HashMap<JsString, Arc<session::SessionRouterStore>>>,
}

impl RouterGenerationRegistry {
    pub fn generation(
        &self,
        project: &JsString,
        organization_policy: Option<OrganizationPolicy>,
    ) -> Arc<RouterSharedState> {
        let clock: Arc<dyn Fn() -> f64 + Send + Sync> = Arc::new(wall_clock);
        let store = self
            .stores
            .lock()
            .expect("project router state")
            .entry(project.clone())
            .or_insert_with(|| Arc::new(session::SessionRouterStore::new(clock.clone())))
            .clone();
        let mut generation = RouterSharedState::new(clock);
        generation.store = store;
        generation.owns_store = false;
        generation.organization_policy = organization_policy;
        Arc::new(generation)
    }
}

impl RouterSharedState {
    /// Called only with the host's loaded snapshot and trusted policy. Request
    /// JSON cannot inject or relax this policy.
    pub fn apply_model_policy(&self, snapshot: &JsValue, deps: &mut RouterDeps) {
        let Some(policy) = &self.organization_policy else {
            return;
        };
        let policy = policy.clone();
        let snapshot = snapshot.clone();
        // TS uses model selectors to filter fallback candidates, while its
        // assertion also checks the selected provider origin/credential source.
        let model_policy = policy.models.clone();
        let assert_allowed: RouterModelPolicyAssertion = Arc::new(move |model| {
            let reference = ModelRef {
                provider: model.provider.clone(),
                model: model.model.clone(),
            };
            if !is_organization_model_allowed(policy.models.as_ref(), &reference) {
                return Err(RouterRuntimeError::new(
                    "GATEWAY_ORGANIZATION_MODEL_DENIED",
                    join_message("Gateway organization policy denies model ", model, "."),
                ));
            }
            let provider = snapshot
                .get("config")
                .and_then(|config| config.get("model"))
                .and_then(|model| model.get("providers"))
                .and_then(|providers| {
                    providers.get(&String::from_utf16_lossy(model.provider.units()))
                });
            if let Some(error) = organization_provider_policy_rejection(
                policy.providers.as_ref(),
                &model.provider,
                provider,
            ) {
                let code = match error.code.as_str() {
                    "GATEWAY_ORGANIZATION_PROVIDER_DENIED" => {
                        "GATEWAY_ORGANIZATION_PROVIDER_DENIED"
                    }
                    "GATEWAY_ORGANIZATION_PROVIDER_ORIGIN_DENIED" => {
                        "GATEWAY_ORGANIZATION_PROVIDER_ORIGIN_DENIED"
                    }
                    _ => "GATEWAY_ORGANIZATION_CREDENTIAL_SOURCE_DENIED",
                };
                return Err(RouterRuntimeError::new(code, error.message));
            }
            Ok(())
        });
        let upstream = deps.is_model_allowed.clone();
        deps.is_model_allowed = Arc::new(move |model| {
            upstream(model)
                && is_organization_model_allowed(
                    model_policy.as_ref(),
                    &ModelRef {
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                    },
                )
        });
        deps.assert_model_allowed = Some(assert_allowed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::model::health::ProviderHealthInput;

    #[test]
    fn production_generations_retain_only_project_sticky_state_on_reload() {
        let registry = RouterGenerationRegistry::default();
        let first = registry.generation(&"project-a".into(), None);
        first.store.set(SessionRoutingState {
            session_id: "same-session".into(),
            sticky_provider: Some("proof".into()),
            sticky_model: Some("base".into()),
            ..Default::default()
        });
        first
            .usage
            .lock()
            .unwrap()
            .push(("same-session".into(), JsValue::Number(27.0)));
        let health = ProviderHealthInput {
            session_id: "same-session".into(),
            provider_id: "proof".into(),
            provider_generation: "original".into(),
        };
        for _ in 0..5 {
            first.health.record_failure(&health);
        }
        assert!(first.health.should_skip(&health));
        let reloaded = registry.generation(&"project-a".into(), None);
        assert!(reloaded.store.get(&"same-session".into(), false).is_some());
        assert!(reloaded.usage.lock().unwrap().is_empty());
        assert!(!reloaded.health.should_skip(&health));
        // A new generation may publish before the previous session drains.
        first.dispose();
        first.dispose();
        assert!(reloaded.store.get(&"same-session".into(), false).is_some());
        let other_project = registry.generation(&"project-b".into(), None);
        assert!(
            other_project
                .store
                .get(&"same-session".into(), false)
                .is_none()
        );
        // Each session occupies its own slot within a project.
        assert!(
            reloaded
                .store
                .get(&"different-session".into(), false)
                .is_none()
        );
    }
}
