//! Fixed TS public catalog reads the original global config on every call.
//! Original runtime probe: core-finish/public-host-audit/RESULT.json.
use super::{assembly, management_tests, public_host::Resolver, sop_control::NativeSopControl};
use crate::{
    metadata::object,
    module_host::{HostBody, PublicHostPrincipal, PublicHostResolver},
    runtime::project::ProjectResources,
};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&U)
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = value else {
        panic!("catalog array")
    };
    items
}
fn global_config(model: &str) -> String {
    let models = serde_json::Map::from_iter([(model.to_owned(), serde_json::json!({}))]);
    serde_json::json!({"schemaVersion":1,"agent":{"model":format!("fixture/{model}")},"model":{"providers":{"fixture":{"protocol":"openai","url":"http://127.0.0.1:9/v1","apiKey":"${KEY_A}","models":models}}}}).to_string()
}
#[tokio::test]
async fn production_public_catalog_uses_fresh_global_config_and_single_credential_resolution() {
    let (_scratch, requests) = management_tests::setup();
    let global = requests.options.home.join("pilotdeck.yaml");
    let original = global_config("global");
    std::fs::write(&global, &original).unwrap();
    let project = requests.options.project.join(".pilotdeck/pilotdeck.yaml");
    std::fs::create_dir_all(project.parent().unwrap()).unwrap();
    let project_bytes = r#"{"schemaVersion":1,"agent":{"model":"fixture/project-only"},"model":{"providers":{"fixture":{"models":{"project-only":{}}}}}}"#;
    std::fs::write(&project, project_bytes).unwrap();
    let mut environment = requests.env.clone();
    environment.insert("KEY_A".into(), "${ABSENT_B}".into());
    environment.remove("ABSENT_B");
    let registry = assembly::project_registry(&requests.options, &environment);
    let sop = NativeSopControl::new(
        requests.options.clone(),
        environment.clone(),
        registry.clone(),
    )
    .unwrap();
    let resolver = Resolver::new(requests.options.clone(), environment, registry.clone(), sop);
    let principal = PublicHostPrincipal {
        pilot_deck_user_id: "fixture-owner".into(),
        tenant_id: "fixture-tenant".into(),
        actor_user_id: "fixture-actor".into(),
        agent_id: "fixture-agent".into(),
    };
    // This is the production resolver/provider boundary, not a second catalog projection.
    let provider = resolver.resolve(&principal).await.unwrap();
    assert!(
        provider
            .operations()
            .contains(&"list_model_catalog".to_owned())
    );
    let generation = registry.acquire(Some(&requests.options.project)).unwrap();
    let snapshot = generation
        .runtime()
        .resources()
        .get::<ProjectResources>()
        .unwrap()
        .context()
        .snapshot();
    assert_eq!(
        field(field(field(&snapshot, "config"), "agent"), "model").get("model"),
        Some(&JsValue::from("project-only"))
    );
    let fixture = field(
        field(field(field(&snapshot, "config"), "model"), "providers"),
        "fixture",
    );
    assert_eq!(field(fixture, "apiKey"), &JsValue::from("${ABSENT_B}"));
    assert!(!field(field(fixture, "models"), "project-only").is_undefined());
    drop(generation);
    let input = object([]);
    let response = provider
        .call("list_model_catalog", &input, &principal)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let HostBody::Value(body) = response.body else {
        panic!("catalog JSON")
    };
    let items = array(field(&body, "data"));
    assert_eq!(
        items.len(),
        1,
        "project-only must not leak from the admitted generation into the global TS catalog"
    );
    assert_eq!(field(&items[0], "id"), &JsValue::from("fixture/global"));
    assert_eq!(field(&items[0], "available"), &JsValue::Bool(true));
    assert_eq!(field(&items[0], "enabled"), &JsValue::Bool(true));
    assert_eq!(field(&items[0], "is_default"), &JsValue::Bool(true));
    assert_eq!(
        field(field(&body, "defaultSelection"), "model"),
        &JsValue::from("global")
    );
    assert_eq!(std::fs::read_to_string(&global).unwrap(), original);
    assert_eq!(std::fs::read_to_string(&project).unwrap(), project_bytes);
    // Keep the same provider/runtime generation while replacing only the fixture's global file.
    let changed = global_config("changed-global");
    std::fs::write(&global, &changed).unwrap();
    let response = provider
        .call("list_model_catalog", &input, &principal)
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let HostBody::Value(changed_body) = response.body else {
        panic!("catalog JSON")
    };
    let changed_items = array(field(&changed_body, "data"));
    assert_eq!(changed_items.len(), 1);
    assert_eq!(
        field(&changed_items[0], "id"),
        &JsValue::from("fixture/changed-global")
    );
    assert_eq!(
        field(field(&changed_body, "defaultSelection"), "model"),
        &JsValue::from("changed-global")
    );
    assert_eq!(
        field(field(field(&snapshot, "config"), "agent"), "model").get("model"),
        Some(&JsValue::from("project-only")),
        "catalog reload must not mutate or replace the selected model runtime snapshot"
    );
    assert_eq!(std::fs::read_to_string(&global).unwrap(), changed);
    assert_eq!(std::fs::read_to_string(&project).unwrap(), project_bytes);
    // Ensure normal wire serialization carries the expected catalog values.
    let wire = stringify_json(&changed_body).unwrap().unwrap();
    assert_eq!(parse_json(&wire).unwrap(), changed_body);
    drop(provider);
    drop(resolver);
    registry.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
