use super::*;
use pilotdeck_protocol::{JsValue, parse_json};
use std::{collections::VecDeque, fs, path::PathBuf};

struct Dialog {
    answers: VecDeque<String>,
    output: String,
}
impl Dialog {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|s| (*s).into()).collect(),
            output: String::new(),
        }
    }
}
impl Prompt for Dialog {
    fn line(&mut self, value: &str) -> SetupResult<()> {
        self.output.push_str(value);
        self.output.push('\n');
        Ok(())
    }
    fn question(&mut self, value: &str) -> SetupResult<String> {
        self.output.push_str(value);
        self.answers.pop_front().ok_or(SetupError::Eof)
    }
}
struct Fake {
    replies: VecDeque<Result<http::Response, String>>,
    requests: Vec<(String, String, Option<String>)>,
    time_ms: u64,
}
impl Fake {
    fn new(json: &str) -> Self {
        Self {
            replies: VecDeque::from([Ok(http::Response {
                status: 200,
                json: parse_json(json).unwrap(),
            })]),
            requests: Vec::new(),
            time_ms: 0,
        }
    }
}
impl http::Fetch for Fake {
    fn now_ms(&self) -> u64 {
        self.time_ms
    }
    fn sleep(&mut self, milliseconds: u64) {
        self.time_ms += milliseconds;
    }
    fn json(
        &mut self,
        method: &str,
        url: &str,
        body: Option<&str>,
    ) -> Result<http::Response, String> {
        self.requests
            .push((method.into(), url.into(), body.map(str::to_owned)));
        self.replies
            .pop_front()
            .expect("unexpected network request")
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-setup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn paths(&self) -> SetupPaths {
        SetupPaths {
            config: self.0.join("nested/pilotdeck.yaml"),
            credentials: self.0.join("creds.json"),
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn feishu_lark_success_preserves_other_fields_and_uses_selected_domain() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config,"models:\n  x: true\nadapters:\n  wecom:\n    token: keep\n  feishu:\n    appId: old-id\n    appSecret: old-secret\n    domainName: feishu\n").unwrap();
    let mut dialog = Dialog::new(&[" LARK ", "", ""]);
    let mut fake = Fake::new(r#"{"code":0,"tenant_access_token":"fake-token"}"#);
    run_with(&["lark".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert_eq!(fake.requests.len(), 1);
    assert_eq!(fake.requests[0].0, "POST");
    assert_eq!(
        fake.requests[0].1,
        "https://open.larksuite.com/open-apis/auth/v3/tenant_access_token/internal"
    );
    let body: serde_json::Value =
        serde_json::from_str(fake.requests[0].2.as_ref().unwrap()).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"app_id":"old-id","app_secret":"old-secret"})
    );
    assert_eq!(
        fs::read_to_string(&paths.config).unwrap(),
        "models:\n  x: true\nadapters:\n  wecom:\n    token: keep\n  feishu:\n    enabled: true\n    appId: old-id\n    appSecret: old-secret\n    connectionMode: stream\n    domainName: lark\n"
    );
    assert!(dialog.output.contains("✅ 凭据验证通过！"));
    assert!(!dialog.output.contains("是否尝试扫码"));
    assert!(dialog.answers.is_empty());
}
#[test]
fn failure_cancel_preserves_original_bytes_then_yes_can_save() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let original = "# comment\nkeep: 1\n";
    fs::write(&paths.config, original).unwrap();
    let mut dialog = Dialog::new(&["", "test-id", "fake-secret", "no"]);
    let mut fake = Fake::new(r#"{"code":999,"msg":"denied"}"#);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), original);
    assert!(dialog.output.contains("code=999 msg=denied"));
    assert!(dialog.output.ends_with("已取消。\n"));
    let mut dialog = Dialog::new(&["", "test-id", "fake-secret", "YES"]);
    let mut fake = Fake::new(r#"{"code":1}"#);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("code=1 msg=unknown"));
    assert_eq!(
        config::text(config::get(
            &config::load(&paths.config),
            &["adapters", "feishu", "appId"]
        )),
        "test-id"
    );
}
#[test]
fn missing_credentials_and_menu_cancel_never_network_or_write() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut fake = Fake::new("null");
    let mut dialog = Dialog::new(&["", "", ""]);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(fake.requests.is_empty());
    assert!(!paths.config.exists());
    let mut dialog = Dialog::new(&["q"]);
    run_with(&["unknown".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("请选择"));
    assert!(dialog.output.ends_with("未选择任何平台，退出。\n"));
}
#[test]
fn setup_paths_do_not_expand_pilot_home_tilde() {
    let env = Env::from_pairs(&[("HOME", "/h"), ("PILOT_HOME", "~/relative")]);
    let paths = SetupPaths::new(&env, Path::new("/work")).unwrap();
    assert_eq!(paths.config, Path::new("/work/~/relative/pilotdeck.yaml"));
    let env = Env::from_pairs(&[
        ("HOME", "/h"),
        ("PILOT_HOME", ""),
        ("PILOTDECK_CONFIG_PATH", "cfg.yaml"),
    ]);
    let paths = SetupPaths::new(&env, Path::new("/work")).unwrap();
    assert_eq!(paths.config, Path::new("/work/cfg.yaml"));
    assert_eq!(
        paths.credentials,
        Path::new("/h/.pilotdeck/weixin-credentials.json")
    );
}
#[test]
fn yaml_emit_matches_fixed_baseline_corpus() {
    let corpus = parse_json(include_str!(
        "../../tests/fixtures/gateway-setup/yaml-corpus.json"
    ))
    .unwrap();
    let JsValue::Array(cases) = config::get(&corpus, &["cases"]) else {
        panic!("cases")
    };
    for case in cases {
        assert_eq!(
            config::emit(config::get(case, &["value"])),
            config::text(config::get(case, &["yaml"])),
            "{}",
            config::text(config::get(case, &["id"]))
        );
    }
}
#[test]
fn network_errors_can_cancel_without_overwriting() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut fake = Fake::new("null");
    fake.replies = VecDeque::from([Err("fetch failed".into())]);
    let mut dialog = Dialog::new(&["", "id", "secret", "n"]);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("凭据验证失败: fetch failed"));
    assert!(!paths.config.exists());
}

#[test]
fn yaml_graph_write_preserves_aliases_cycles_and_primitive_error_bytes() {
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/graph-corpus.json"
    ))
    .unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        let original = case["yaml"].as_str().unwrap();
        fs::write(&paths.config, original).unwrap();
        let adapter = config::object([
            ("enabled", JsValue::Bool(true)),
            ("appId", config::string("fixture-app")),
            ("appSecret", config::string("fixture-secret")),
            ("connectionMode", config::string("stream")),
            ("domainName", config::string("feishu")),
        ]);
        let result = config::write_adapter(&paths.config, "feishu", adapter);
        if matches!(case["id"].as_str(), Some("root-array" | "array-adapters"))
            || case.get("error").is_some()
        {
            assert!(result.is_err(), "{}", case["id"]);
            assert_eq!(
                fs::read_to_string(&paths.config).unwrap(),
                original,
                "{}",
                case["id"]
            );
        } else if let Some(expected) = case["afterFeishuWrite"].as_str() {
            result.unwrap();
            assert_eq!(
                fs::read_to_string(&paths.config).unwrap(),
                expected,
                "{}",
                case["id"]
            );
        } else if case.get("writeError").is_some() {
            assert!(result.is_err(), "{}", case["id"]);
            assert_eq!(
                fs::read_to_string(&paths.config).unwrap(),
                original,
                "{}",
                case["id"]
            );
        }
    }
}

fn fake_http(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(e) => panic!("fake accept: {e}"),
                }
            };
            // BSD accept may inherit O_NONBLOCK from the readiness listener.
            // A deadline alone does not make a nonblocking stream wait.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let mut expected = None;
            loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0, "request closed");
                request.extend_from_slice(&buffer[..n]);
                if expected.is_none()
                    && let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let head = String::from_utf8_lossy(&request[..end]);
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    expected = Some(end + 4 + length);
                }
                if expected.is_some_and(|n| request.len() >= n) {
                    break;
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            socket.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (format!("http://{address}"), worker)
}
fn http_response(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    )
}

#[test]
fn production_fetch_follows_redirect_and_rewrites_post_like_node() {
    use super::http::Fetch;
    let (url, server) = fake_http(vec![
        http_response("302 Found", "Location: /final\r\n", "redirect"),
        http_response(
            "503 Service Unavailable",
            "",
            r#"{"code":0,"tenant_access_token":"fake"}"#,
        ),
    ]);
    let result = http::Network::new()
        .json(
            "POST",
            &format!("{url}/start"),
            Some(r#"{"app_id":"fixture-app","app_secret":"fixture-secret"}"#),
        )
        .unwrap();
    assert_eq!(result.status, 503);
    assert_eq!(
        result.json.get("code").and_then(JsValue::as_number),
        Some(0.0)
    );
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("POST /start HTTP/1.1\r\n"));
    assert!(
        requests[0]
            .to_lowercase()
            .contains("content-type: application/json; charset=utf-8")
    );
    assert!(requests[0].ends_with(r#"{"app_id":"fixture-app","app_secret":"fixture-secret"}"#));
    assert!(requests[1].starts_with("GET /final HTTP/1.1\r\n"));
    assert!(!requests[1].to_lowercase().contains("content-type:"));
    assert!(!requests[1].contains("app_id"));
}
#[test]
fn production_fetch_malformed_json_uses_node_syntax_message() {
    use super::http::Fetch;
    let (url, server) = fake_http(vec![http_response("200 OK", "", "{bad")]);
    let error = match http::Network::new().json("POST", &url, Some("{}")) {
        Ok(_) => panic!("malformed response accepted"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
    server.join().unwrap();
}
#[test]
fn numeric_zero_is_required_and_non_success_http_status_is_ignored() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    let mut fake = Fake::new(r#"{"code":"0","tenant_access_token":"fake"}"#);
    let mut dialog = Dialog::new(&["", "id", "secret", "n"]);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(dialog.output.contains("code=0 msg=unknown"));
    assert!(!paths.config.exists());
    let mut fake = Fake::new(r#"{"code":0,"tenant_access_token":"fake"}"#);
    fake.replies.front_mut().unwrap().as_mut().unwrap().status = 503;
    let mut dialog = Dialog::new(&["", "id", "secret"]);
    run_with(&["feishu".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert!(paths.config.exists());
}

#[test]
fn yaml_multiline_leading_space_keeps_explicit_indent_and_other_fields() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/leading-space.json"
    ))
    .unwrap();
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, fixture["input"].as_str().unwrap()).unwrap();
    let adapter = config::object([
        ("enabled", JsValue::Bool(true)),
        ("appId", config::string("fixture-app")),
        ("appSecret", config::string("fixture-secret")),
        ("connectionMode", config::string("stream")),
        ("domainName", config::string("feishu")),
    ]);
    config::write_adapter(&paths.config, "feishu", adapter).unwrap();
    assert_eq!(
        fs::read_to_string(&paths.config).unwrap(),
        fixture["expected"].as_str().unwrap()
    );
    assert_eq!(
        config::text(config::get(&config::load(&paths.config), &["item"])),
        " a\nb"
    );
}

fn fixture_adapter() -> JsValue {
    config::object([
        ("enabled", JsValue::Bool(true)),
        ("appId", config::string("fixture-app")),
        ("appSecret", config::string("fixture-secret")),
        ("connectionMode", config::string("stream")),
        ("domainName", config::string("feishu")),
    ])
}
fn failure_message(result: SetupResult<()>) -> String {
    match result {
        Err(SetupError::Failure(Failure::Error(message))) => message,
        other => panic!("expected error: {other:?}"),
    }
}
#[test]
fn yaml_literal_backslashes_and_control_chars_round_trip_separately() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/escapes.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::write(&paths.config, case["input"].as_str().unwrap()).unwrap();
        config::write_adapter(&paths.config, "feishu", fixture_adapter()).unwrap();
        assert_eq!(
            fs::read_to_string(&paths.config).unwrap(),
            case["expected"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(
            config::text(config::get(&config::load(&paths.config), &["note"])),
            case["value"].as_str().unwrap(),
            "{}",
            case["id"]
        );
    }
}
#[test]
fn strict_save_keeps_existing_invalid_yaml_and_unreadable_objects() {
    for original in ["keep: [\n", "a: 1\na: 2\n", "item: *missing\n"] {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::write(&paths.config, original).unwrap();
        assert_eq!(
            failure_message(config::write_adapter(
                &paths.config,
                "feishu",
                fixture_adapter()
            )),
            "Cannot save setup configuration: existing YAML is invalid."
        );
        assert_eq!(fs::read_to_string(&paths.config).unwrap(), original);
    }
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(&paths.config).unwrap();
    fs::write(paths.config.join("sentinel"), "keep").unwrap();
    let error = failure_message(config::write_adapter(
        &paths.config,
        "feishu",
        fixture_adapter(),
    ));
    assert!(error.contains("target must be a regular file"), "{error}");
    assert_eq!(
        fs::read_to_string(paths.config.join("sentinel")).unwrap(),
        "keep"
    );
}
#[test]
fn strict_save_refuses_array_shapes_without_claiming_success() {
    for (original, part) in [("- old\n", "root"), ("adapters: []\nkeep: 1\n", "adapters")] {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::write(&paths.config, original).unwrap();
        let mut fake = Fake::new(r#"{"code":0,"tenant_access_token":"fake"}"#);
        let mut dialog = Dialog::new(&["", "id", "secret"]);
        let message = failure_message(run_with(&["feishu".into()], &paths, &mut dialog, &mut fake));
        assert_eq!(
            message,
            format!("Cannot save setup configuration: {part} must be a YAML mapping.")
        );
        assert!(!dialog.output.contains("飞书配置已写入"));
        assert_eq!(fs::read_to_string(&paths.config).unwrap(), original);
    }
}
#[test]
fn production_fetch_keeps_same_origin_307_post_and_secret() {
    use super::http::Fetch;
    let (url, server) = fake_http(vec![
        http_response("307 Temporary Redirect", "Location: /final\r\n", ""),
        http_response("200 OK", "", r#"{"code":0,"tenant_access_token":"fake"}"#),
    ]);
    http::Network::new()
        .json(
            "POST",
            &format!("{url}/start"),
            Some(r#"{"app_secret":"fixture-secret"}"#),
        )
        .unwrap();
    let requests = server.join().unwrap();
    assert!(requests[1].starts_with("POST /final HTTP/1.1\r\n"));
    assert!(requests[1].ends_with(r#"{"app_secret":"fixture-secret"}"#));
}
#[test]
fn production_fetch_never_sends_307_secret_to_another_origin() {
    use super::http::Fetch;
    let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let target_url = format!("http://{}/capture", target.local_addr().unwrap());
    let (url, server) = fake_http(vec![http_response(
        "307 Temporary Redirect",
        &format!("Location: {target_url}\r\n"),
        "",
    )]);
    let result =
        http::Network::new().json("POST", &url, Some(r#"{"app_secret":"fixture-secret"}"#));
    assert_eq!(
        result.err().as_deref(),
        Some("Refusing unsafe credential redirect")
    );
    server.join().unwrap();
    assert!(matches!(target.accept(),Err(error)if error.kind()==std::io::ErrorKind::WouldBlock));
}
#[test]
fn credential_redirect_refuses_https_downgrade_even_without_body() {
    let secure = url::Url::parse("https://example.test/start").unwrap();
    let insecure = url::Url::parse("http://example.test/end").unwrap();
    for body in [false, true] {
        assert_eq!(
            http::credential_redirect(&secure, &insecure, body),
            Err("Refusing unsafe credential redirect".into())
        );
    }
}

#[cfg(unix)]
#[test]
#[ignore = "spawned by setup_write_reports_descriptor_stage_under_fsize_limit"]
fn setup_write_child_over_file_size_limit() {
    let path = PathBuf::from(
        std::env::var_os("PDRS_SETUP_WRITE_PATH")
            .expect("parent must provide isolated configuration path"),
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("write-stage runtime")
        .block_on(async { pilotdeck_gateway::ensure_sigxfsz_ignored() });
    let error = failure_message(config::write_adapter(&path, "feishu", fixture_adapter()));
    println!("WRITE_ERROR={error}");
}
#[cfg(unix)]
#[test]
fn setup_write_reports_descriptor_stage_under_fsize_limit() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let original = [107u8, 101, 101, 112, 58, 32, 111, 108, 100, 10];
    fs::write(&paths.config, original).unwrap();
    let child = concat!(module_path!(), "::setup_write_child_over_file_size_limit")
        .split_once("::")
        .expect("test module prefix")
        .1;
    // Same bounded-file child contract as skills.rs: a real resource limit,
    // ignored SIGXFSZ, and an explicit receipt so an empty filter cannot pass.
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("ulimit -f 0 || exit 1; exec \"$0\" --exact \"$1\" --ignored --nocapture --test-threads=2")
        .arg(std::env::current_exe().unwrap()).arg(child)
        .env("PDRS_SETUP_WRITE_PATH", &paths.config).output().expect("write-stage child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let message = stdout
        .lines()
        .find_map(|line| line.strip_prefix("WRITE_ERROR="))
        .unwrap_or_else(|| panic!("write-stage receipt absent: {stdout}{stderr}"));
    assert_eq!(message, "EFBIG: file too large, write");
    // The same real write failure that truncated the old implementation now
    // leaves the original file intact and removes its exclusive temporary.
    assert_eq!(fs::read(&paths.config).unwrap(), original);
    assert_no_setup_temporary(paths.config.parent().unwrap());
}

#[test]
fn strict_save_preserves_existing_invalid_utf8_bytes() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let original = [107u8, 101, 101, 112, 58, 32, 255, 10];
    fs::write(&paths.config, original).unwrap();
    assert_eq!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        )),
        "Cannot save setup configuration: existing YAML is invalid."
    );
    assert_eq!(fs::read(&paths.config).unwrap(), original);
}

fn assert_no_setup_temporary(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pilotdeck-setup-")
    }));
}
#[test]
fn atomic_save_preserves_existing_mode_owner_and_group() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, "keep: old").unwrap();
    fs::set_permissions(&paths.config, fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(&paths.config).unwrap();
    config::write_adapter(&paths.config, "feishu", fixture_adapter()).unwrap();
    let after = fs::metadata(&paths.config).unwrap();
    assert_eq!(
        (after.mode(), after.uid(), after.gid()),
        (before.mode(), before.uid(), before.gid())
    );
    assert_eq!(
        config::text(config::get(&config::load(&paths.config), &["keep"])),
        "old"
    );
    assert_no_setup_temporary(paths.config.parent().unwrap());
}
#[test]
fn atomic_new_config_is_private_and_complete() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let paths = scratch.paths();
    config::write_adapter(&paths.config, "feishu", fixture_adapter()).unwrap();
    assert_eq!(
        fs::metadata(&paths.config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        config::text(config::get(
            &config::load(&paths.config),
            &["adapters", "feishu", "appId"]
        )),
        "fixture-app"
    );
    assert_no_setup_temporary(paths.config.parent().unwrap());
}
#[test]
fn atomic_save_follows_symbolic_link_without_replacing_it() {
    use std::os::unix::fs::{MetadataExt, symlink};
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let target = scratch.0.join("real.yaml");
    fs::write(&target, "keep: old").unwrap();
    symlink(&target, &paths.config).unwrap();
    let before = fs::symlink_metadata(&paths.config).unwrap();
    config::write_adapter(&paths.config, "feishu", fixture_adapter()).unwrap();
    let after = fs::symlink_metadata(&paths.config).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    assert!(after.file_type().is_symlink());
    assert_eq!(fs::read_link(&paths.config).unwrap(), target);
    assert_eq!(
        config::text(config::get(
            &config::load(&target),
            &["adapters", "feishu", "appId"]
        )),
        "fixture-app"
    );
    assert_no_setup_temporary(&scratch.0);
}
#[test]
fn atomic_save_refuses_hard_links_and_dangling_symbolic_links() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let target = scratch.0.join("real.yaml");
    fs::write(&target, "keep: old").unwrap();
    fs::hard_link(&target, &paths.config).unwrap();
    assert!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        ))
        .contains("hard-linked")
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep: old");
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), "keep: old");
    fs::remove_file(&paths.config).unwrap();
    symlink("missing.yaml", &paths.config).unwrap();
    assert!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        ))
        .contains("dangling")
    );
    assert_eq!(
        fs::read_link(&paths.config).unwrap(),
        Path::new("missing.yaml")
    );
    assert!(!paths.config.parent().unwrap().join("missing.yaml").exists());
    assert_no_setup_temporary(paths.config.parent().unwrap());
}
#[test]
fn atomic_save_refuses_changed_target_and_preserves_new_writer_bytes() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, "keep: original").unwrap();
    let transaction = super::save::Target::prepare(&paths.config).unwrap();
    fs::write(&paths.config, "keep: another writer changed contents").unwrap();
    assert!(
        failure_message(transaction.publish(b"unexpected replacement")).contains("target changed")
    );
    assert_eq!(
        fs::read_to_string(&paths.config).unwrap(),
        "keep: another writer changed contents"
    );
    assert_no_setup_temporary(paths.config.parent().unwrap());
}
#[test]
fn atomic_save_refuses_extended_attributes_without_touching_old_file() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, "keep: original").unwrap();
    let file = fs::File::open(&paths.config).unwrap();
    rustix::fs::fsetxattr(
        &file,
        "user.pdrs_fixture",
        b"fixture",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    assert!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        ))
        .contains("extended attributes")
    );
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), "keep: original");
    let mut bytes = [0u8; 32];
    let count = rustix::fs::fgetxattr(&file, "user.pdrs_fixture", &mut bytes[..]).unwrap();
    assert_eq!(&bytes[..count], b"fixture");
    assert_no_setup_temporary(paths.config.parent().unwrap());
}
#[cfg(target_os = "macos")]
#[test]
fn atomic_save_refuses_macos_acl_without_touching_original() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, "keep: original").unwrap();
    let output = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg("everyone allow read")
        .arg(&paths.config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        ))
        .contains("ACL metadata")
    );
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), "keep: original");
    assert_no_setup_temporary(paths.config.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn atomic_save_refuses_bsd_flags_without_losing_them() {
    let scratch = Scratch::new();
    let paths = scratch.paths();
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    fs::write(&paths.config, "keep: original").unwrap();
    let output = std::process::Command::new("/usr/bin/chflags")
        .arg("hidden")
        .arg(&paths.config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = std::os::macos::fs::MetadataExt::st_flags(&fs::metadata(&paths.config).unwrap());
    assert_ne!(before, 0);
    assert!(
        failure_message(config::write_adapter(
            &paths.config,
            "feishu",
            fixture_adapter()
        ))
        .contains("BSD file flags")
    );
    let after = std::os::macos::fs::MetadataExt::st_flags(&fs::metadata(&paths.config).unwrap());
    assert_eq!(before, after);
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), "keep: original");
    assert_no_setup_temporary(paths.config.parent().unwrap());
    println!("BSD_FLAGS_PRESERVED={after}");
}

#[path = "tests_wecom.rs"]
mod wecom_tests;
