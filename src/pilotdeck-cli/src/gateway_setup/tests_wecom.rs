use super::*;

fn reply(raw: &str) -> Result<http::Response, String> {
    Ok(http::Response {
        status: 200,
        json: parse_json(raw).unwrap(),
    })
}
#[test]
fn manual_wecom_saves_policies_and_handles_all_aliases() {
    for alias in ["wecom", "work-weixin", "workwechat"] {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        let mut dialog = Dialog::new(&[
            "n",
            "fixture-bot",
            "fixture-secret",
            "",
            "2",
            " a, b ,, ",
            "3",
            " grp-1, grp-2 ",
        ]);
        let mut fake = Fake::new("null");
        run_with(&[alias.into()], &paths, &mut dialog, &mut fake).unwrap();
        assert!(fake.requests.is_empty());
        let root = config::load(&paths.config);
        assert_eq!(
            config::text(config::get(&root, &["adapters", "wecom", "token"])),
            "fixture-bot"
        );
        assert_eq!(
            config::text(config::get(
                &root,
                &["adapters", "wecom", "extra", "dm_policy"]
            )),
            "allowlist"
        );
        assert_eq!(
            config::get(&root, &["adapters", "wecom", "extra", "allow_from"]),
            &JsValue::Array(vec![config::string("a"), config::string("b")])
        );
        assert_eq!(
            config::text(config::get(
                &root,
                &["adapters", "wecom", "extra", "group_policy"]
            )),
            "allowlist"
        );
        assert!(dialog.output.ends_with("重启 PilotDeck 服务后生效。\n\n"));
        assert!(dialog.answers.is_empty());
    }
}
#[test]
fn existing_wecom_credentials_keep_nullish_aliases_and_groups_snapshot() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config,"keep: &group\n  value: old\nadapters:\n  other:\n    enabled: true\n  wecom:\n    enabled: false\n    token: old-bot-id\n    extra:\n      secret: old-secret\n      websocketUrl: wss://old.example\n      dm_policy: ' ALLOWLIST '\n      allowFrom: [a, ' b ', '', 7]\n      group_policy: invalid\n      groupAllowFrom: 'g1, g2'\n      groups: *group\n").unwrap();
    let mut dialog = Dialog::new(&["no"]);
    let mut fake = Fake::new("null");
    run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(fake.requests.is_empty());
    let after = fs::read_to_string(&paths.config).unwrap();
    assert!(
        !after.contains("&a"),
        "groups is captured by the first parse, unlike fresh keep: {after}"
    );
    let root = config::load(&paths.config);
    assert_eq!(
        config::text(config::get(
            &root,
            &["adapters", "wecom", "extra", "groups", "value"]
        )),
        "old"
    );
    assert_eq!(
        config::text(config::get(
            &root,
            &["adapters", "wecom", "extra", "websocket_url"]
        )),
        "wss://old.example"
    );
    assert_eq!(
        config::text(config::get(
            &root,
            &["adapters", "wecom", "extra", "group_policy"]
        )),
        "disabled"
    );
    assert_eq!(
        config::get(&root, &["adapters", "other", "enabled"]),
        &JsValue::Bool(true)
    );
    assert!(
        dialog
            .output
            .ends_with("企业微信已启用（使用现有凭据）。\n\n")
    );
}
#[test]
fn empty_dm_allowlist_disables_dm_while_empty_group_allowlist_stays() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut dialog = Dialog::new(&["no", "bot", "secret", "", "2", " , ", "3", ""]);
    let mut fake = Fake::new("null");
    run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
    let root = config::load(&paths.config);
    assert_eq!(
        config::text(config::get(
            &root,
            &["adapters", "wecom", "extra", "dm_policy"]
        )),
        "disabled"
    );
    assert_eq!(
        config::text(config::get(
            &root,
            &["adapters", "wecom", "extra", "group_policy"]
        )),
        "allowlist"
    );
    assert!(matches!(
        config::get(&root, &["adapters", "wecom", "extra", "group_allow_from"]),
        JsValue::Undefined
    ));
}
#[test]
fn qr_wecom_encodes_scode_retries_query_error_and_saves_returned_credentials() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut dialog = Dialog::new(&["", "", "", ""]);
    let mut fake = Fake::new("null");
    fake.replies = VecDeque::from([
        reply(r#"{"data":{"scode":"a/b 中","auth_url":"https://fixture.invalid/scan"}}"#),
        Err("HTTP 503".into()),
        reply(
            r#"{"data":{"status":"SUCCESS","bot_info":{"bot_id":" qr-bot ","secret":" qr-secret "}}}"#,
        ),
    ]);
    run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert_eq!(fake.requests.len(), 3);
    assert!(fake.requests[0].1.ends_with("generate?source=hermes"));
    assert!(fake.requests[1].1.ends_with("scode=a%2Fb%20%E4%B8%AD"));
    assert_eq!(fake.time_ms, 3000);
    assert!(dialog.output.contains("https://fixture.invalid/scan"));
    assert!(!dialog.output.contains("手动配置步骤"));
    let root = config::load(&paths.config);
    assert_eq!(
        config::text(config::get(&root, &["adapters", "wecom", "token"])),
        "qr-bot"
    );
}
#[test]
fn malformed_qr_and_http_error_fall_back_to_manual_without_fake_success() {
    for response in [reply(r#"{"data":{}}"#), Err("HTTP 500".into())] {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        let mut dialog = Dialog::new(&["", "", ""]);
        let mut fake = Fake::new("null");
        fake.replies = VecDeque::from([response]);
        run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
        assert!(dialog.output.contains("将使用手动输入模式"));
        assert!(dialog.output.contains("未提供完整 Bot ID / Secret"));
        assert!(!dialog.output.contains("配置已写入"));
        assert!(!paths.config.exists());
    }
}
#[test]
fn qr_missing_credentials_falls_back_and_timeout_uses_mock_clock() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut dialog = Dialog::new(&["", "", ""]);
    let mut fake = Fake::new("null");
    fake.replies = VecDeque::from([
        reply(r#"{"data":{"scode":"code","auth_url":"https://fixture.invalid/scan"}}"#),
        reply(r#"{"data":{"status":"success","bot_info":{"botid":"only-id"}}}"#),
    ]);
    run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("扫码成功但未返回完整 Bot 凭据"));
    assert!(!paths.config.exists());
    let mut dialog = Dialog::new(&["", "", ""]);
    let mut fake = Fake::new("null");
    fake.replies = VecDeque::from([reply(
        r#"{"data":{"scode":"code","auth_url":"https://fixture.invalid/scan"}}"#,
    )]);
    for _ in 0..100 {
        fake.replies
            .push_back(reply(r#"{"data":{"status":"waiting"}}"#));
    }
    run_with(&["wecom".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert_eq!(fake.time_ms, 300000);
    assert_eq!(fake.requests.len(), 101);
    assert!(dialog.output.contains("企业微信扫码超时。"));
    assert!(!paths.config.exists());
}
#[test]
fn production_wecom_checks_http_before_parsing_invalid_json_and_sets_user_agent() {
    use super::super::http::Fetch;
    let (url, server) = fake_http(vec![http_response("503 Service Unavailable", "", "{bad")]);
    let error = http::Network::new().success_json(&url).unwrap_err();
    assert_eq!(error, "HTTP 503");
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET / HTTP/1.1"));
    assert!(
        requests[0]
            .to_lowercase()
            .contains("user-agent: pilotdeck/1.0")
    );
}
#[test]
fn menu_wecom_and_missing_manual_credentials_do_not_write() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut dialog = Dialog::new(&["3", "n", "", ""]);
    let mut fake = Fake::new("null");
    run_with(&[], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("企业微信 WeCom AI Bot 配置"));
    assert!(fake.requests.is_empty());
    assert!(!paths.config.exists());
}

#[test]
fn wecom_matches_fixed_ts_mock_oracle_stdout_requests_clock_and_files() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/wecom-oracle.json"
    ))
    .unwrap();
    check_oracle(&oracle);
}
#[test]
fn wecom_matches_ts_surrogate_and_deep_group_boundary_oracle() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/wecom-boundaries.json"
    ))
    .unwrap();
    check_oracle(&oracle);
}
fn check_oracle(oracle: &serde_json::Value) {
    for case in oracle["cases"].as_array().unwrap() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        if let Some(initial) = case["initial"].as_str() {
            fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
            fs::write(&paths.config, initial).unwrap();
        }
        let answers: Vec<_> = case["answers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|answer| answer[1].as_str().unwrap())
            .collect();
        let mut dialog = Dialog::new(&answers);
        let mut fake = Fake::new("null");
        fake.replies.clear();
        for response in case["responses"].as_array().into_iter().flatten() {
            fake.replies
                .push_back(if let Some(error) = response["error"].as_str() {
                    Err(error.into())
                } else {
                    Ok(http::Response {
                        status: response["status"].as_u64().unwrap_or(200) as u16,
                        json: if let Some(raw) = response["raw"].as_str() {
                            parse_json(raw).unwrap_or(JsValue::Undefined)
                        } else {
                            response
                                .get("json")
                                .map(|json| parse_json(&json.to_string()).unwrap())
                                .unwrap_or(JsValue::Undefined)
                        },
                    })
                });
        }
        let argv: Vec<_> = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect();
        run_with(&argv, &paths, &mut dialog, &mut fake).unwrap();
        assert_eq!(
            dialog.output,
            case["stdout"].as_str().unwrap(),
            "{} stdout",
            case["id"]
        );
        assert!(dialog.answers.is_empty(), "{} answers", case["id"]);
        assert_eq!(case["code"], 0);
        assert_eq!(case["stderr"], "");
        let after = match fs::read_to_string(&paths.config) {
            Ok(after) => Some(after),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("config read: {error}"),
        };
        assert_eq!(
            after.as_deref(),
            case["after"].as_str(),
            "{} file",
            case["id"]
        );
        assert_eq!(
            fake.time_ms,
            case["clock"].as_u64().unwrap(),
            "{} clock",
            case["id"]
        );
        let requests: Vec<_> = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["url"].as_str().unwrap().to_owned(),
                    event["body"].as_str().map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(fake.requests, requests, "{} requests", case["id"]);
    }
}

#[test]
fn wecom_invalid_existing_yaml_keeps_original_and_does_not_claim_success() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let original = [107u8, 101, 101, 112, 58, 32, 91, 10];
    fs::write(&paths.config, original).unwrap();
    let mut dialog = Dialog::new(&["n", "bot", "secret", "", "", ""]);
    let mut fake = Fake::new("null");
    assert_eq!(
        failure_message(run_with(&["wecom".into()], &paths, &mut dialog, &mut fake)),
        "Cannot save setup configuration: existing YAML is invalid."
    );
    assert_eq!(fs::read(&paths.config).unwrap(), original);
    assert!(!dialog.output.contains("企业微信配置已写入"));
    assert!(!dialog.output.contains("企业微信已启用"));
}
#[cfg(unix)]
#[test]
#[ignore = "spawned by wecom_reuse_and_qr_save_failure_preserve_old_files"]
fn wecom_write_failure_child() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { pilotdeck_gateway::ensure_sigxfsz_ignored() });
    for (name, qr) in [
        ("PDRS_WECOM_REUSE_PATH", false),
        ("PDRS_WECOM_QR_PATH", true),
    ] {
        let path = PathBuf::from(std::env::var_os(name).expect("parent supplies isolated path"));
        let paths = SetupPaths {
            config: path,
            credentials: PathBuf::from("unused-credentials"),
            credentials_label: PathBuf::from("unused-credentials"),
            home_pilot: None,
            home_pilot_label: None,
            home_input: crate::paths::HomeInput::capture(&Env::from_pairs(&[])),
            cwd: std::env::current_dir().unwrap(),
        };
        let mut dialog = if qr {
            Dialog::new(&["", "", "", ""])
        } else {
            Dialog::new(&["n"])
        };
        let mut fake = Fake::new("null");
        if qr {
            fake.replies = VecDeque::from([
                reply(r#"{"data":{"scode":"code","auth_url":"https://fixture.invalid"}}"#),
                reply(
                    r#"{"data":{"status":"success","bot_info":{"botid":"qr-bot","secret":"qr-secret"}}}"#,
                ),
            ]);
        }
        let error = failure_message(run_with(&["wecom".into()], &paths, &mut dialog, &mut fake));
        assert_eq!(error, "EFBIG: file too large, write");
        assert!(!dialog.output.contains("企业微信配置已写入"));
        assert!(!dialog.output.contains("企业微信已启用"));
        assert_eq!(fake.requests.len(), if qr { 2 } else { 0 });
    }
    println!("WECOM_SAVE_FAILURES=2");
}
#[cfg(unix)]
#[test]
fn wecom_reuse_and_qr_save_failure_preserve_old_files() {
    let scratch = Scratch::new();
    let reuse = scratch.0.join("reuse.yaml");
    let qr = scratch.0.join("qr.yaml");
    let original_reuse = "keep: original\nadapters:\n  wecom:\n    token: old-bot\n    extra:\n      secret: old-secret\n      groups: {nested: {policy: open}}\n";
    let original_qr = "keep: original";
    fs::write(&reuse, original_reuse).unwrap();
    fs::write(&qr, original_qr).unwrap();
    let child = concat!(module_path!(), "::wecom_write_failure_child")
        .split_once("::")
        .unwrap()
        .1;
    let output=std::process::Command::new("/bin/sh").arg("-c").arg("ulimit -f 0 || exit 1; exec \"$0\" --exact \"$1\" --ignored --nocapture --test-threads=2").arg(std::env::current_exe().unwrap()).arg(child).env("PDRS_WECOM_REUSE_PATH",&reuse).env("PDRS_WECOM_QR_PATH",&qr).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.lines().any(|line| line == "WECOM_SAVE_FAILURES=2"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&reuse).unwrap(), original_reuse);
    assert_eq!(fs::read_to_string(&qr).unwrap(), original_qr);
    assert_no_setup_temporary(&scratch.0);
}
