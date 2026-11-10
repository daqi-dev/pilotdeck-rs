use super::*;
use crate::module_host::PublicHostPrincipal;
fn json(s: &str) -> JsValue {
    pilotdeck_protocol::parse_json(s).unwrap()
}
fn principal() -> PublicHostPrincipal {
    PublicHostPrincipal {
        pilot_deck_user_id: "owner".into(),
        tenant_id: "tenant".into(),
        actor_user_id: "actor".into(),
        agent_id: "target".into(),
    }
}
fn input() -> JsValue {
    json(
        "{\"sessionKey\":\"s\",\"projectKey\":\"/project\",\"requestId\":\" req \",\"expectedRevision\":3,\"admissionCredentialId\":\"credential\"}",
    )
}
#[test]
fn public_authority_checks_exact_input_and_full_principal_without_leaking_credential() {
    let raw = input();
    let parsed = authority_input(&raw).unwrap();
    let admission = json(
        "{\"projectKey\":\"/project\",\"pilotDeckUserId\":\"owner\",\"tenantId\":\"tenant\",\"actorUserId\":\"actor\",\"agentId\":\"target\",\"credentialId\":\"credential\"}",
    );
    assert!(authority_admission_matches(
        &parsed,
        &principal(),
        &admission
    ));
    for key in [
        "projectKey",
        "pilotDeckUserId",
        "tenantId",
        "actorUserId",
        "agentId",
        "credentialId",
    ] {
        let mut changed = admission.as_object().unwrap().clone();
        changed.insert(key.into(), "other".into());
        assert!(
            !authority_admission_matches(&parsed, &principal(), &JsValue::Object(changed)),
            "{key}"
        );
    }
    let merged = authority_merge(
        parsed.context,
        admission.clone(),
        json("{\"sopId\":\"fixture\",\"sopVersion\":\"v1\",\"nodeId\":\"node\",\"content\":{}}"),
    );
    assert_eq!(value(&merged, "requestId"), &JsValue::from(" req "));
    assert_eq!(value(&merged, "admission"), &admission);
    assert!(value(&merged, "admissionCredentialId").is_undefined());
    assert_eq!(value(&merged, "sopVersion"), &JsValue::from("v1"));
    for bad in [JsValue::Null, json("[]"), json("{}")] {
        assert_eq!(authority_input(&bad).err().unwrap().status, Some(403));
    }
    for (key, bad) in [
        ("requestId", JsValue::from(" \u{feff}")),
        ("projectKey", JsValue::Null),
        ("sessionKey", JsValue::Number(3.0)),
        ("admissionCredentialId", JsValue::from("")),
        ("expectedRevision", JsValue::Number(0.0)),
        ("expectedRevision", JsValue::Number(0.5)),
        ("authority", JsValue::Null),
    ] {
        let mut v = raw.as_object().unwrap().clone();
        v.insert(key.into(), bad);
        let e = authority_input(&JsValue::Object(v)).err().unwrap();
        assert_eq!(e.code, Some("SOP_AUTHORITY_INPUT_INVALID".into()));
        assert_eq!(
            e.message.as_ref(),
            e.code.as_ref().and_then(JsValue::as_string)
        );
    }
    // Fixed TS uses Number.isInteger, not Number.isSafeInteger, for revisions.
    let mut v = raw.as_object().unwrap().clone();
    v.insert(
        "expectedRevision".into(),
        JsValue::Number(9_007_199_254_740_992.0),
    );
    assert!(authority_input(&JsValue::Object(v)).is_ok());
}
#[cfg(unix)]
#[tokio::test]
async fn configured_native_authority_reads_real_admission_and_disk_state_from_target_generation() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_scratch, requests) = super::super::management_tests::setup();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    // Fixture owner helper and local formal HTTP responses; no real identity/key.
    let helper = requests.options.home.join("fixture-owner-helper");
    std::fs::write(&helper, "#!/bin/sh\nprintf '{\"owner\":\"owner\"}'\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let http = tokio::spawn(async move {
        let mut paths = vec![];
        for _ in 0..9 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = vec![];
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
                assert!(head.len() < 16384);
            }
            let head = String::from_utf8(head).unwrap();
            let path = head.split_whitespace().nth(1).unwrap();
            paths.push(path.to_owned());
            let body = match path {
                "/api/auth/me" => {
                    "{\"id\":\"actor\",\"tenant_id\":\"tenant\",\"disabled\":false,\"source\":\"web\"}"
                }
                "/api/auth/me/api-credentials" => {
                    "[{\"id\":\"credential\",\"user_id\":\"actor\",\"status\":\"active\",\"access\":\"user_full_access\",\"key_prefix\":\"12345678901234567890…\"}]"
                }
                "/api/v1/agents/target" => {
                    "{\"id\":\"target\",\"status\":\"active\",\"is_overall\":false}"
                }
                _ => panic!("unexpected authority HTTP path {path}"),
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
        paths
    });
    let config = requests.options.home.join("pilotdeck.yaml");
    let mut bytes = std::fs::read_to_string(&config).unwrap();
    bytes.push_str(&format!("modules:\n  sop:\n    enabled: true\n    provider: staffdeck\n    endpoint: {origin}\n    discoveryEndpoint: {origin}/api/v1\n    discoveryAgentId: target\n    discoveryApiKey: 12345678901234567890-fixture-key\n    definitionsPath: fixture-sops.json\n    defaultSopId: fixture\n"));
    std::fs::write(config, bytes).unwrap();
    let mut environment = requests.env.clone();
    for (k, v) in [
        ("STAFFDECK_FORMAL_API_ORIGIN", origin.as_str()),
        ("STAFFDECK_COPY_TARGET_AGENT_ID", "target"),
        ("STAFFDECK_COPY_TENANT_ID", "tenant"),
        ("STAFFDECK_COPY_PILOTDECK_USER_ID", "owner"),
        ("STAFFDECK_COPY_ACTOR_USER_ID", "actor"),
        ("STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID", "credential"),
        ("STAFFDECK_COPY_USER_TOKEN", "fixture-actor-bearer"),
        (
            "STAFFDECK_SOP_MANAGEMENT_API_KEY",
            "12345678901234567890-fixture-key",
        ),
    ] {
        environment.insert(k.into(), v.into());
    }
    let registry = super::super::assembly::project_registry(&requests.options, &environment);
    let mut control =
        NativeSopControl::new(requests.options.clone(), environment, registry.clone()).unwrap();
    Arc::get_mut(&mut control).unwrap().helper = helper;
    let project = JsString::from(requests.options.project.to_string_lossy().into_owned());
    let session = JsString::from("a8-session");
    let admission = crate::metadata::object([
        ("tenantId", "tenant".into()),
        ("agentId", "target".into()),
        ("pilotDeckUserId", "owner".into()),
        ("projectKey", JsValue::String(project.clone())),
        ("actorUserId", "actor".into()),
        ("credentialId", "credential".into()),
        ("staffDeckOrigin", origin.as_str().into()),
    ]);
    let entry = crate::metadata::object([
        ("type", "session_metadata".into()),
        ("sessionId", JsValue::String(session.clone())),
        ("turnId", "host-admission".into()),
        ("createdAt", "2026-11-11T00:00:00.000Z".into()),
        ("sequence", JsValue::Number(2.0)),
        (
            "metadata",
            crate::metadata::object([("staffDeckAdmission", admission.clone())]),
        ),
    ]);
    let chat = bounded_chat_dir(&requests.options.home, &requests.options.project).unwrap();
    let transcript = chat.join("a8-session.jsonl");
    let mut bytes = std::fs::read_to_string(&transcript).unwrap();
    bytes.push_str(&format!(
        "{}\n",
        pilotdeck_protocol::stringify_json(&entry).unwrap().unwrap()
    ));
    std::fs::write(transcript, bytes).unwrap();
    let (lease, snapshot) = control
        .snapshot(Some(&JsValue::String(project.clone())))
        .await
        .unwrap();
    let store = NativeSopControl::store_for_snapshot(&snapshot).unwrap();
    let oracle = json(include_str!("../../tests/fixtures/native-sop/oracle.json"));
    let JsValue::Array(cases) = value(&oracle, "cases") else {
        panic!("oracle cases must be an array")
    };
    let row = cases
        .iter()
        .find(|row| value(row, "name") == &JsValue::from("authority"))
        .unwrap();
    let mut initial = value(row, "initial").as_object().unwrap().clone();
    initial.insert("sessionId".into(), JsValue::String(session.clone()));
    let path = store.path(&session);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        pilotdeck_protocol::stringify_json(&JsValue::Object(initial))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let input = crate::metadata::object([
        ("sessionKey", JsValue::String(session)),
        ("projectKey", JsValue::String(project)),
        ("requestId", "authority-request".into()),
        ("expectedRevision", JsValue::Number(3.0)),
        ("admissionCredentialId", "credential".into()),
    ]);
    // A default home generation may differ from input's registered project.
    let default = registry.acquire(Some(&requests.options.home)).unwrap();
    let actual = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        control.authority(&input, &principal(), &default),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(value(&actual, "admission"), &admission);
    assert_eq!(value(&actual, "sopId"), &JsValue::from("fixture"));
    assert_eq!(
        value(&actual, "requestId"),
        &JsValue::from("authority-request")
    );
    assert!(value(&actual, "admissionCredentialId").is_undefined());
    let mut stale = input.as_object().unwrap().clone();
    stale.insert("expectedRevision".into(), JsValue::Number(2.0));
    let e = control
        .authority(&JsValue::Object(stale), &principal(), &lease)
        .await
        .unwrap_err();
    assert_eq!(e.status, Some(409));
    assert_eq!(e.message, Some("SOP_REVISION_CONFLICT".into()));
    let mut wrong = principal();
    wrong.actor_user_id = "other".into();
    let e = control.authority(&input, &wrong, &lease).await.unwrap_err();
    assert_eq!(e.status, Some(403));
    assert_eq!(e.code, Some("SOP_AUTHORITY_ADMISSION_MISMATCH".into()));
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), http)
            .await
            .unwrap()
            .unwrap()
            .len(),
        9
    );
    drop(default);
    drop(lease);
    drop(control);
    registry.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
