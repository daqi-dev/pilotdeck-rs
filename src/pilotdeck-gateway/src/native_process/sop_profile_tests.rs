use super::*;
use crate::runtime::project::ConfigSource;

#[tokio::test]
async fn production_profile_accepts_both_real_normalized_sop_shapes_and_rejects_unrelated_external_tools()
 {
    let root = std::env::temp_dir().join(format!(
        "pdrs-sop-normalized-profile-{}",
        uuid::Uuid::new_v4()
    ));
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let options = Options {
        home: home.clone(),
        project: project.clone(),
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: home.join("token"),
        sidecar: home.join("sidecar"),
        sidecar_args: vec![],
        drain_budget: Duration::from_secs(1),
        storage_root: None,
    };
    let env = options.environment(Env::new());
    let base = serde_json::json!({"schemaVersion":1,"agent":{"model":"fixture/mock"},"model":{"providers":{"fixture":{"protocol":"openai","url":"http://127.0.0.1:1/v1","apiKey":"fake","models":{"mock":{}}}}}});
    for protocol in [false, true] {
        let mut config = base.clone();
        let mut sop = serde_json::json!({"enabled":true,"endpoint":"http://127.0.0.1:1","definitionsPath":"definitions.json","defaultSopId":"fixture"});
        if protocol {
            sop["implementationId"] = "fixture.sop".into();
            sop["contract"] = "sop.lifecycle/v2".into();
            sop["transport"] = "sop-http-v2".into();
        } else {
            sop["provider"] = "staffdeck".into();
        }
        config["modules"] = serde_json::json!({"sop":sop});
        std::fs::write(
            home.join("pilotdeck.yaml"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let snapshot = crate::runtime::project::NativeConfigSource::new(
            project.clone(),
            env.clone(),
            Some(home.clone()),
            None,
        )
        .bounded()
        .load(&project)
        .unwrap();
        let normalized = snapshot
            .get("config")
            .unwrap()
            .get("modules")
            .unwrap()
            .get("sop")
            .unwrap();
        assert!(
            normalized.get("enabled").is_none(),
            "SOP snapshot intentionally omits enabled"
        );
        assembly::validate_profile(&options, &env)
            .await
            .expect("normalized SOP must pass production profile admission");
    }
    let mut config = base;
    config["modules"] = serde_json::json!({"tools":{"enabled":true,"implementationId":"fixture.tools","contract":"pilotdeck.tools/v1","transport":"module-http-v2","endpoint":"http://127.0.0.1:1","catalog":[{"name":"fixture_echo","description":"fixture","inputSchema":{"type":"object"}}]}});
    std::fs::write(
        home.join("pilotdeck.yaml"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "unimplemented external tools must remain rejected"
    );
    std::fs::remove_dir_all(root).unwrap();
}
