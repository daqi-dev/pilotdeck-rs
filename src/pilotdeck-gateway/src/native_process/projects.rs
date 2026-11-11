//! Per-root production resource construction. The registry remains the only
//! generation/lease owner; this is not a second project cache or admission gate.
use super::Options;
use crate::{
    config::Env,
    runtime::project::{
        ProjectRuntimeFactory, ResourceBundle, ResourceFamilySource, RuntimeResult,
    },
};
use std::{path::Path, sync::Arc};

pub(super) struct NativeProjectFactory {
    options: Options,
    env: Env,
    background: Option<Arc<dyn ResourceFamilySource>>,
    router_events: Arc<super::router_events::RouterEventJournal>,
}
impl NativeProjectFactory {
    pub(super) fn new(options: Options, env: Env) -> Self {
        Self {
            router_events: Arc::new(super::router_events::RouterEventJournal::new(&options.home)),
            options,
            env,
            background: {
                #[cfg(unix)]
                {
                    Some(super::background::native_background_family())
                }
                #[cfg(not(unix))]
                {
                    None
                }
            },
        }
    }
}
impl ProjectRuntimeFactory for NativeProjectFactory {
    fn stage(&self, root: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        super::assembly::project_resources_factory(
            &self.options,
            &self.env,
            root,
            self.background.clone(),
            self.router_events.clone(),
        )
        .stage(root, resources)
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::project::{
        ProjectResources, Resource, RuntimeLease, RuntimeState, plugins::PluginResources,
    };
    use pilotdeck_protocol::JsString;
    use std::sync::Arc;
    use std::time::Duration;

    fn model(lease: &RuntimeLease) -> JsString {
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .unwrap()
            .context()
            .snapshot()
            .get("config")
            .unwrap()
            .get("agent")
            .unwrap()
            .get("model")
            .unwrap()
            .get("model")
            .unwrap()
            .as_string()
            .unwrap()
            .clone()
    }
    #[tokio::test]
    async fn native_registry_stages_each_project_models_and_extensions_at_its_root() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("project-B");
        std::fs::create_dir_all(b.join(".pilotdeck/plugins/only-b/output-styles")).unwrap();
        std::fs::write(requests.options.home.join("pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: test/a}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fake-only\n      models: {a: {}, b: {}}\nmemory: {enabled: false}\n").unwrap();
        std::fs::write(
            b.join(".pilotdeck/pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: test/b}\n",
        )
        .unwrap();
        std::fs::write(
            b.join(".pilotdeck/plugins/only-b/plugin.json"),
            r#"{"name":"only-b","version":"1"}"#,
        )
        .unwrap();
        std::fs::write(
            b.join(".pilotdeck/plugins/only-b/output-styles/b.md"),
            "Only project B\n",
        )
        .unwrap();
        let registry = super::super::assembly::project_registry(&requests.options, &requests.env);
        let mut a_lease = registry.acquire(Some(&a)).unwrap();
        let mut b_lease = registry.acquire(Some(&b)).unwrap();
        assert_eq!(a_lease.runtime().project_root(), a);
        assert_eq!(b_lease.runtime().project_root(), b);
        assert_eq!(model(&a_lease), JsString::from("a"));
        assert_eq!(model(&b_lease), JsString::from("b"));
        for (lease, expected) in [(&a_lease, false), (&b_lease, true)] {
            let context = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .unwrap()
                .context();
            let resource = context.resource(Resource::PluginRuntime).unwrap();
            let plugin = resource.as_any().downcast_ref::<PluginResources>().unwrap();
            plugin.runtime.refresh().await.unwrap();
            assert_eq!(
                plugin
                    .runtime
                    .snapshot()
                    .await
                    .iter()
                    .any(|p| p.name == "only-b"),
                expected,
                "project-owned plugin catalog must use the same root as the model generation"
            );
        }
        // Invalidation of B must preserve both A's identity and B's live lease.
        registry.invalidate(Some(&b));
        assert_eq!(b_lease.runtime().state(), RuntimeState::Retired);
        assert_eq!(model(&b_lease), JsString::from("b"));
        assert!(Arc::ptr_eq(
            a_lease.runtime(),
            &registry.resolve(Some(&a)).unwrap()
        ));
        std::fs::write(
            b.join(".pilotdeck/pilotdeck.yaml"),
            "schemaVersion: 1\nagent: {model: test/a}\n",
        )
        .unwrap();
        let mut next_b = registry.acquire(Some(&b)).unwrap();
        assert!(!Arc::ptr_eq(b_lease.runtime(), next_b.runtime()));
        assert_eq!(model(&next_b), JsString::from("a"));
        assert_eq!(model(&b_lease), JsString::from("b"));
        tokio::time::timeout(Duration::from_secs(8), b_lease.release())
            .await
            .unwrap()
            .unwrap();
        a_lease.release().await.unwrap();
        next_b.release().await.unwrap();
        tokio::time::timeout(Duration::from_secs(8), registry.dispose())
            .await
            .unwrap()
            .unwrap();
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
