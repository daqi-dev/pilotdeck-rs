mod http_module_client;
pub use http_module_client::HttpModuleClient;
pub mod knowledge;
pub use knowledge::register_knowledge_query_tool;

pub mod knowledge_authority;
pub fn is_external_module_binding(binding: &pilotdeck_protocol::JsValue) -> bool {
    binding.get("implementationId").is_some()
        && binding
            .get("transport")
            .and_then(pilotdeck_protocol::JsValue::as_string)
            .is_some_and(|value| value.equals_str("module-http-v2"))
}
