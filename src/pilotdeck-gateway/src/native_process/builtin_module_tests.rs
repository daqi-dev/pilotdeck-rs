use super::*;
use crate::runtime::project::ConfigSource;

#[tokio::test]
async fn real_ts_builtin_module_profiles_keep_default_owners() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-process/builtin-modules-oracle.json"
    ))
    .expect("builtin module oracle JSON");
    assert_eq!(
        oracle["metadata"]["sourceCommit"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    assert_eq!(oracle["rows"].as_array().unwrap().len(), 15);
    for row in oracle["rows"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let scratch = Scratch::new();
        let options = Options::parse(scratch.args()).unwrap();
        let env = options.environment(Env::new());
        std::fs::write(
            options.home.join("pilotdeck.yaml"),
            row["yaml"].as_str().unwrap(),
        )
        .unwrap();
        let loaded = crate::runtime::project::NativeConfigSource::new(
            options.project.clone(),
            env.clone(),
            Some(options.home.clone()),
            None,
        )
        .load(&options.project);
        if row["output"]["ok"] == false {
            assert!(
                loaded.is_err(),
                "{id}: invalid config must fail before profile admission"
            );
            continue;
        }
        let snapshot = loaded.unwrap_or_else(|error| panic!("{id}: {error}"));
        let null = JsValue::Null;
        let actual_modules = snapshot
            .get("config")
            .and_then(|v| v.get("modules"))
            .unwrap_or(&null);
        assert_eq!(
            json_value(actual_modules),
            row["output"]["modules"],
            "{id}: real loader module projection"
        );
        assert_eq!(
            json_value(snapshot.get("diagnostics").unwrap()),
            row["output"]["diagnostics"],
            "{id}: real loader diagnostics"
        );
        let admission = assembly::validate_profile(&options, &env).await;
        if id == "external-tools" {
            assert_eq!(
                admission.unwrap_err().to_string(),
                "Native isolated profile does not yet support configured module bindings.",
                "external owner must not be silently replaced"
            );
        } else {
            admission
                .unwrap_or_else(|error| panic!("{id}: builtin default owner rejected: {error}"));
            assert_eq!(row["output"]["selectors"]["runtimePortKeys"], json!([]));
            assert_eq!(row["output"]["selectors"]["agentLoopFactoryAbsent"], true);
            assert_eq!(row["output"]["selectors"]["skillsExternal"], false);
            assert_eq!(row["output"]["selectors"]["knowledgeExternal"], false);
        }
    }
}
