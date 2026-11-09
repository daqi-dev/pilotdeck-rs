//! Session ownership uses the same native factory as daqi project resources.
use super::factory::{CreatedMemoryProvider, create_from_config};
use crate::runtime::memory::{
    MemoryBundleError, MemoryDisposeFn, MemoryMaintenanceService, MemoryManagementService,
    MemoryResolver, NativeMemoryProviderOptions, wrap_native,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct NativeProjectMemoryOwner {
    pub resolver: Arc<dyn MemoryResolver>,
    pub maintenance: Arc<dyn MemoryMaintenanceService>,
    pub management: Arc<dyn MemoryManagementService>,
    pub disposer: MemoryDisposeFn,
    pub retrieval_timeout_ms: f64,
    pub has_llm: bool,
    pub enabled: bool,
}

impl CreatedMemoryProvider {
    pub fn into_owner(self, options: &NativeMemoryProviderOptions) -> NativeProjectMemoryOwner {
        let resources = wrap_native(self.into_native());
        // Lease clones share one disposal result without changing daqi service.close semantics.
        let dispose = resources.dispose.expect("native factory supplies disposal");
        let disposed = Arc::new(tokio::sync::OnceCell::new());
        let disposer: MemoryDisposeFn = Arc::new(move || {
            let dispose = dispose.clone();
            let disposed = disposed.clone();
            Box::pin(async move { disposed.get_or_init(|| dispose()).await.clone() })
        });
        NativeProjectMemoryOwner {
            resolver: resources.memory,
            maintenance: resources
                .maintenance
                .expect("native factory supplies maintenance"),
            management: resources
                .management
                .expect("native factory supplies management"),
            disposer,
            retrieval_timeout_ms: options
                .config
                .as_ref()
                .and_then(|config| config.retrieval_timeout_ms)
                .filter(|timeout| timeout.is_finite() && *timeout > 0.0)
                .unwrap_or(30_000.0),
            has_llm: options.config.as_ref().is_some_and(|config| {
                super::llm::config::resolve_memory_llm(
                    config,
                    options.model_config.as_ref(),
                    options.agent_model.as_deref(),
                )
                .is_some()
            }),
            enabled: true,
        }
    }
}

pub fn create_project_owner(
    options: &NativeMemoryProviderOptions,
) -> Result<Option<NativeProjectMemoryOwner>, MemoryBundleError> {
    Ok(create_from_config(options)?.map(|provider| provider.into_owner(options)))
}

#[cfg(test)]
mod tests {
    use super::super::{factory::create_with_dependencies, service::ServiceDependencies};
    use super::*;
    use crate::runtime::memory::{MemoryConfig, NativeMemoryProviderOptions};
    use pilotdeck_protocol::{JsString, JsValue};

    fn options(project_root: std::path::PathBuf) -> NativeMemoryProviderOptions {
        NativeMemoryProviderOptions {
            project_root,
            config: None,
            model_config: None,
            agent_model: None,
            now: None,
            telemetry: None,
        }
    }

    #[test]
    fn absent_owner_does_not_require_an_executor() {
        let options = options(std::path::PathBuf::from("/unused-owner-test"));
        assert!(create_project_owner(&options).unwrap().is_none());
    }

    #[tokio::test]
    async fn owner_uses_native_services_and_preserves_session_identity() {
        let scratch = crate::scratch::Scratch::new("memory-native-owner");
        let mut options = options(scratch.path().join("project"));
        let mut config = MemoryConfig::new(true, "edgeclaw");
        config.root_dir = scratch.path().join("store").to_str().unwrap().into();
        config.retrieval_timeout_ms = Some(1234.0);
        options.config = Some(config);
        let created = create_with_dependencies(
            &options,
            ServiceDependencies {
                home: Some(scratch.path().join("home")),
                llm_config: Some(JsValue::Undefined),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        let provider = created.provider.clone();
        let owner = created.into_owner(&options);
        assert!(Arc::ptr_eq(
            &owner.resolver,
            &(provider as Arc<dyn MemoryResolver>)
        ));
        assert_eq!(owner.retrieval_timeout_ms, 1234.0);
        assert!(owner.enabled);
        assert!(!owner.has_llm);
        assert!(owner.management.list(100).unwrap().is_empty());
        let invalid_key = JsString::from_utf16(vec![0xd800]);
        assert!(
            owner
                .management
                .clear_session(&invalid_key)
                .unwrap_err()
                .requires_host()
        );
        (owner.disposer)().await.unwrap();
        (owner.disposer)().await.unwrap();
    }
}
