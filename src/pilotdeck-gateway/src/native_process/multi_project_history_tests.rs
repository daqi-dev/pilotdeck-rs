//! The B history envelope must use B's retained project-generation config.
use super::{assembly, management_tests};
use crate::runtime::{
    history::{HistoryFiles, NativeHistoryReader},
    turn::read::SessionHistoryReader,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

#[tokio::test]
async fn native_history_uses_the_selected_project_generation_budget() {
    let (_scratch, requests) = management_tests::setup();
    let home = requests.options.home.clone();
    let project_a = requests.options.project.clone();
    let project_b = project_a.parent().unwrap().join("project-B");
    std::fs::create_dir_all(project_b.join(".pilotdeck")).unwrap();
    std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: test/test, maxContextTokens: 1000, maxOutputTokens: 100}\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fixture-only\n      models: {test: {}}\nmemory: {enabled: false}\n").unwrap();
    std::fs::write(project_b.join(".pilotdeck/pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: test/test, maxContextTokens: 3000, maxOutputTokens: 300}\n").unwrap();
    let marker = home.join("projects/b-history");
    std::fs::create_dir_all(&marker).unwrap();
    std::fs::write(marker.join(".cwd"), project_b.to_str().unwrap()).unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-history/typical-r1.json"
    ))
    .unwrap();
    let row = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "token-usage")
        .unwrap();
    let lines = row["spec"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let session = JsString::from("history:fixture");
    for project in [&project_a, &project_b] {
        let chat = crate::session_chat_dir(&home, project, &project.to_string_lossy());
        std::fs::create_dir_all(&chat).unwrap();
        std::fs::write(
            chat.join(format!(
                "{}.jsonl",
                crate::storage::sanitize_session_id(&session)
            )),
            &lines,
        )
        .unwrap();
    }
    // One registry; None is deliberately A. B must not reuse its config.
    let registry = assembly::project_registry(&requests.options, &requests.env);
    let input = pilotdeck_protocol::parse_json(r#"{"sessionKey":"history:fixture"}"#).unwrap();
    let mut results = Vec::new();
    for project in [project_a, project_b] {
        let reader = NativeHistoryReader {
            files: HistoryFiles {
                home: home.clone(),
                project,
            },
            project_runtime: Arc::clone(&registry),
        };
        results.push(reader.read(&input).await.unwrap());
    }
    for (result, total, reserved) in [(&results[0], 1000.0, 100.0), (&results[1], 3000.0, 300.0)] {
        let usage = result
            .get("tokenUsage")
            .expect("real transcript contains usage");
        assert_eq!(usage.get("total"), Some(&JsValue::Number(total)));
        assert_eq!(
            usage.get("reservedOutputTokens"),
            Some(&JsValue::Number(reserved))
        );
        assert_eq!(usage.get("used"), Some(&JsValue::Number(14.0)));
    }
}
