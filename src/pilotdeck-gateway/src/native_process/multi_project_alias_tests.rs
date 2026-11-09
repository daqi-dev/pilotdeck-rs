use crate::{
    metadata::object,
    native_process::{assembly, management_tests},
    runtime::turn::gateway::GatewayRequests,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::path::{Path, PathBuf};
fn b_project(requests: &crate::native_process::requests::Requests, register: bool) -> PathBuf {
    let b = requests.options.project.parent().unwrap().join("alias-B");
    std::fs::create_dir_all(b.join(".pilotdeck/plugins/style-b/output-styles")).unwrap();
    std::fs::write(b.join(".pilotdeck/pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: test/only-b}\nmodel: {providers: {test: {models: {only-b: {}}}}}\n").unwrap();
    std::fs::write(
        b.join(".pilotdeck/plugins/style-b/plugin.json"),
        r#"{"name":"style-b","version":"1"}"#,
    )
    .unwrap();
    std::fs::write(
        b.join(".pilotdeck/plugins/style-b/output-styles/only-b.md"),
        "B only style\n",
    )
    .unwrap();
    if register {
        let mut options = requests.options.clone();
        options.project = b.clone();
        assembly::register_project(&options).unwrap();
    }
    b
}
fn alias(b: &Path) -> JsString {
    let mut units = b
        .parent()
        .unwrap()
        .to_string_lossy()
        .encode_utf16()
        .collect::<Vec<_>>();
    units.extend("/".encode_utf16());
    units.push(0xd800);
    units.extend("/../".encode_utf16());
    units.extend(b.file_name().unwrap().to_string_lossy().encode_utf16());
    JsString::from_utf16(units)
}
async fn close(requests: &crate::native_process::requests::Requests) {
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
#[tokio::test]
async fn model_set_and_clear_preserve_global_catalog_with_canonical_b_transcript() {
    let (_scratch, requests) = management_tests::setup();
    let b = b_project(&requests, true);
    let key = "model-B-only";
    let refused = requests
        .request(
            "session_model_set".into(),
            object([
                ("projectKey", JsValue::String(alias(&b))),
                ("sessionKey", key.into()),
                (
                    "selection",
                    object([
                        ("mode", "model".into()),
                        ("provider", "test".into()),
                        ("model", "only-b".into()),
                    ]),
                ),
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, "INVALID_MODEL_OVERRIDE");
    assert_eq!(
        refused.message,
        JsString::from("Model is unavailable: test/only-b")
    );
    let result = requests
        .request(
            "session_model_set".into(),
            object([
                ("projectKey", JsValue::String(alias(&b))),
                ("sessionKey", key.into()),
                (
                    "selection",
                    object([
                        ("mode", "model".into()),
                        ("provider", "test".into()),
                        ("model", "test".into()),
                    ]),
                ),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(
        result.get("projectKey"),
        Some(&JsValue::from(b.to_str().unwrap()))
    );
    assert_eq!(
        result.get("saved").and_then(|s| s.get("model")),
        Some(&JsValue::from("test"))
    );
    let b_chat =
        crate::native_process::methods::session_dir::bounded_chat_dir(&requests.options.home, &b)
            .unwrap();
    let a_chat = crate::native_process::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    let transcript = b_chat.join(format!("{key}.jsonl"));
    let bytes = std::fs::read(&transcript).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("modelSelection"));
    assert!(!a_chat.join(format!("{key}.jsonl")).exists());
    requests
        .request(
            "session_model_clear".into(),
            object([
                ("projectKey", JsValue::String(alias(&b))),
                ("sessionKey", key.into()),
            ]),
        )
        .await
        .unwrap();
    let cleared = std::fs::read(&transcript).unwrap();
    assert!(cleared.starts_with(&bytes));
    assert!(String::from_utf8_lossy(&cleared).contains("model-selection-clear"));
    close(&requests).await;
}
#[tokio::test]
async fn model_mutations_reject_unknown_project_before_missing_key_or_selection() {
    let (_scratch, requests) = management_tests::setup();
    let b = b_project(&requests, false);
    let before = std::fs::read(b.join(".pilotdeck/pilotdeck.yaml")).unwrap();
    for method in ["session_model_set", "session_model_clear"] {
        let error = requests
            .request(
                method.into(),
                object([("projectKey", b.to_str().unwrap().into())]),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "PROJECT_NOT_FOUND");
        assert_eq!(
            error.message,
            JsString::from(format!("Unknown projectKey: {}", b.display()))
        );
    }
    assert_eq!(
        std::fs::read(b.join(".pilotdeck/pilotdeck.yaml")).unwrap(),
        before
    );
    assert!(
        !requests
            .options
            .home
            .join("projects")
            .join(crate::metadata::project_id(&b))
            .exists()
    );
    close(&requests).await;
}
#[tokio::test]
async fn styles_allow_unregistered_b_but_do_not_ignore_bad_project_type() {
    let (_scratch, requests) = management_tests::setup();
    let b = b_project(&requests, false);
    let result = requests
        .request(
            "output_styles_list".into(),
            object([("projectKey", JsValue::String(alias(&b)))]),
        )
        .await
        .unwrap();
    let Some(JsValue::Array(styles)) = result.get("styles") else {
        panic!("native output styles");
    };
    assert!(
        styles
            .iter()
            .any(|style| style.get("name") == Some(&JsValue::from("style-b:only-b")))
    );
    let error = requests
        .request(
            "output_styles_list".into(),
            object([("projectKey", JsValue::Number(7.0))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "ERR_INVALID_ARG_TYPE");
    let error = requests
        .request(
            "set_output_style".into(),
            object([
                ("projectKey", JsValue::Number(7.0)),
                ("sessionKey", "style-type".into()),
                ("name", JsValue::Null),
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "ERR_INVALID_ARG_TYPE");
    close(&requests).await;
}
#[test]
fn history_empty_key_fallback_is_used_by_actual_request_consumer() {
    crate::test_process::run_cases(
        "native_process::methods::config::multi_project_alias_tests::history_empty_key_worker",
        vec![("empty-history".into(), vec![])],
    );
}
#[tokio::test]
#[ignore = "owned subprocess changes cwd"]
async fn history_empty_key_worker() {
    let (_scratch, requests) = management_tests::setup();
    let b = b_project(&requests, true);
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(&b).unwrap();
    let key = JsString::from("history-empty");
    let project = JsString::from(requests.options.project.to_string_lossy().into_owned());
    let _sink = requests
        .coordinator
        .start_for_project(&key, &"run-A".into(), &project, |_| {});
    requests.coordinator.record(
        &key,
        &object([
            ("type", "assistant_text_delta".into()),
            ("delta", "A_REPLAY_ONLY".into()),
        ]),
    );
    let empty = requests
        .request(
            "read_session_messages".into(),
            object([
                ("projectKey", "".into()),
                ("sessionKey", JsValue::String(key.clone())),
            ]),
        )
        .await;
    let missing = requests
        .request(
            "read_session_messages".into(),
            object([("sessionKey", JsValue::String(key.clone()))]),
        )
        .await;
    let aliased = requests
        .request(
            "read_session_messages".into(),
            object([
                (
                    "projectKey",
                    JsValue::String(alias(&requests.options.project)),
                ),
                ("sessionKey", JsValue::String(key.clone())),
            ]),
        )
        .await;
    let foreign = requests
        .request(
            "read_session_messages".into(),
            object([
                ("projectKey", b.to_str().unwrap().into()),
                ("sessionKey", JsValue::String(key.clone())),
            ]),
        )
        .await;
    std::env::set_current_dir(original).unwrap();
    let empty = empty.unwrap();
    let missing = missing.unwrap();
    let aliased = aliased.unwrap();
    let foreign = foreign.unwrap();
    assert_eq!(empty.get("stream"), missing.get("stream"));
    assert_eq!(empty.get("stream"), aliased.get("stream"));
    let text = |value: &JsValue| pilotdeck_protocol::stringify_json(value).unwrap().unwrap();
    assert!(text(&empty).contains("A_REPLAY_ONLY"));
    assert!(!text(&foreign).contains("A_REPLAY_ONLY"));
    requests.coordinator.retain_terminal(&key, &"run-A".into());
    close(&requests).await;
    crate::test_process::receipt("empty-history");
}
