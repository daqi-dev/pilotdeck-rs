//! Integration tests for PluginRuntime and GatewayCommandCatalogBundle.
//! Real oracle replay is in tests/plugin_runtime.rs (A6.3-pr7b scope).

use super::*;

#[test]
fn registry_duplicate_key_keeps_first_position_and_last_value() {
    // PluginRegistry.ts:73-76,92-93: Map.set replaces a value in place.
    let plugin = |name: &str, path: &str| {
        Arc::new(LoadedPlugin {
            name: name.into(),
            path: path.into(),
            source: PluginSource::Global,
            commands: Vec::new(),
            skills: Vec::new(),
            output_styles: Vec::new(),
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
        })
    };
    let first = plugin("z-plugin", "/first");
    let middle = plugin("a-plugin", "/middle");
    let replacement = plugin("z-plugin", "/replacement");
    let mut registry = PluginRegistry::new();

    registry.replace_all(vec![first, Arc::clone(&middle), Arc::clone(&replacement)]);

    let listed = registry.list();
    assert_eq!(listed.len(), 2, "each name@source key must appear once");
    assert!(
        Arc::ptr_eq(&listed[0], &replacement),
        "the first key must keep its position and use the last inserted instance"
    );
    assert!(Arc::ptr_eq(&listed[1], &middle));
}
