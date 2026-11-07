use super::super::http::{Fetch, IlinkFailure};
use super::*;
use std::collections::HashSet;

struct IlinkMock {
    base: Fake,
    aborts: HashSet<usize>,
    polls: Vec<bool>,
}
impl IlinkMock {
    fn new() -> Self {
        Self {
            base: Fake::new("null"),
            aborts: HashSet::new(),
            polls: Vec::new(),
        }
    }
}
impl Fetch for IlinkMock {
    fn json(
        &mut self,
        method: &str,
        url: &str,
        body: Option<&str>,
    ) -> Result<http::Response, String> {
        self.base.json(method, url, body)
    }
    fn ilink_json(&mut self, url: &str, poll: bool) -> Result<http::Response, IlinkFailure> {
        let index = self.base.requests.len();
        self.polls.push(poll);
        self.base.json("GET", url, None).map_err(|error| {
            if self.aborts.contains(&index) {
                IlinkFailure::Abort
            } else {
                IlinkFailure::Error(error)
            }
        })
    }
    fn now_ms(&self) -> u64 {
        self.base.time_ms
    }
    fn sleep(&mut self, milliseconds: u64) {
        self.base.time_ms += milliseconds;
    }
}
fn paths(scratch: &Scratch) -> SetupPaths {
    SetupPaths::new(
        &Env::from_pairs(&[
            ("HOME", "home"),
            ("PILOT_HOME", "pilot"),
            ("PILOTDECK_CONFIG_PATH", "pilot/pilotdeck.yaml"),
        ]),
        &scratch.0,
    )
    .unwrap()
}
fn responses(case: &serde_json::Value) -> IlinkMock {
    let mut fake = IlinkMock::new();
    fake.base.replies.clear();
    for (index, response) in case["responses"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if response["errorName"] == "AbortError" {
            fake.aborts.insert(index);
        }
        fake.base
            .replies
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
    fake
}
fn optional_file(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("fixture read failed: {error}"),
    }
}
#[test]
fn weixin_matches_fixed_ts_sdk_callback_requests_credentials_and_config_oracle() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/weixin-oracle.json"
    ))
    .unwrap();
    check_oracle(&oracle);
}
#[test]
fn all_platform_menu_matches_ts_order_and_complete_output() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/weixin-all.json"
    ))
    .unwrap();
    check_oracle(&oracle);
}
fn check_oracle(oracle: &serde_json::Value) {
    for case in oracle["cases"].as_array().unwrap() {
        let scratch = Scratch::new();
        let paths = paths(&scratch);
        if case["createPilot"] != false {
            fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        }
        if let Some(initial) = case["initial"].as_str() {
            fs::write(&paths.config, initial).unwrap();
        }
        if let Some(initial) = case["credentials"].as_str() {
            fs::write(&paths.credentials, initial).unwrap();
        }
        let answers: Vec<_> = case["answers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|answer| answer[1].as_str().unwrap())
            .collect();
        let mut dialog = Dialog::new(&answers);
        let mut fake = responses(case);
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
        assert_eq!(case["stderr"], "");
        assert_eq!(case["code"], 0);
        assert!(dialog.answers.is_empty(), "{} answers", case["id"]);
        assert_eq!(
            optional_file(&paths.config).as_deref(),
            case["after"].as_str(),
            "{} config",
            case["id"]
        );
        assert_eq!(
            optional_file(&paths.credentials).as_deref(),
            case["afterCredentials"].as_str(),
            "{} credentials",
            case["id"]
        );
        assert_eq!(
            paths.home_pilot.as_ref().unwrap().exists(),
            case["homePilotCreated"].as_bool().unwrap(),
            "{} homedir side effect",
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
        assert_eq!(fake.base.requests, requests, "{} requests", case["id"]);
        let polls: Vec<_> = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["headers"].is_object())
            .collect();
        assert_eq!(fake.polls, polls, "{} headers", case["id"]);
        assert_eq!(
            fake.base.time_ms,
            case["clock"].as_u64().unwrap(),
            "{} clock",
            case["id"]
        );
    }
}
#[test]
fn weixin_total_login_timeout_uses_mock_clock_and_never_saves() {
    let scratch = Scratch::new();
    let paths = paths(&scratch);
    fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
    let mut fake = IlinkMock::new();
    fake.base.replies.clear();
    fake.base.replies.push_back(Ok(http::Response {
        status: 200,
        json: parse_json(r#"{"qrcode":"code","qrcode_img_content":"https://fixture.invalid/qr"}"#)
            .unwrap(),
    }));
    for _ in 0..480 {
        fake.base.replies.push_back(Ok(http::Response {
            status: 200,
            json: parse_json(r#"{"status":"wait"}"#).unwrap(),
        }));
    }
    let mut dialog = Dialog::new(&[""]);
    run_with(&["weixin".into()], &paths, &mut dialog, &mut fake).unwrap();
    assert_eq!(fake.base.time_ms, 480000);
    assert_eq!(fake.base.requests.len(), 481);
    assert!(dialog.output.contains("微信登录失败: Login timed out"));
    assert!(!paths.credentials.exists());
    assert!(!paths.config.exists());
}
#[test]
fn weixin_native_poll_checks_status_and_sends_sdk_header() {
    let (url, server) = fake_http(vec![http_response("502 Bad Gateway", "", "{bad")]);
    let response = http::Network::new().ilink_json(&url, true).unwrap();
    assert_eq!(response.status, 502);
    assert!(matches!(response.json, JsValue::Undefined));
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .to_lowercase()
            .contains("ilink-app-clientversion: 1")
    );
}
fn delayed_server(headers_first: bool) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).unwrap();
        if headers_first {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        if headers_first {
            let _ = socket.write_all(b"{}");
        } else {
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    (format!("http://{address}"), worker)
}
#[test]
fn weixin_poll_abort_deadline_ends_at_headers_not_body() {
    let (url, server) = delayed_server(false);
    let result = http::Network::with_poll_timeout(std::time::Duration::from_millis(30))
        .ilink_json(&url, true);
    assert!(matches!(result, Err(IlinkFailure::Abort)));
    server.join().unwrap();
    let (url, server) = delayed_server(true);
    let result = http::Network::with_poll_timeout(std::time::Duration::from_millis(30))
        .ilink_json(&url, true)
        .unwrap();
    assert!(matches!(result.json, JsValue::Object(_)));
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "spawned by weixin_failed_credential_write_keeps_old_credentials_and_config"]
fn weixin_credential_write_failure_child() {
    let root = PathBuf::from(
        std::env::var_os("PDRS_WX_FAILURE_ROOT").expect("parent supplies isolated root"),
    );
    let paths = SetupPaths::new(
        &Env::from_pairs(&[
            ("HOME", "home"),
            ("PILOT_HOME", "pilot"),
            ("PILOTDECK_CONFIG_PATH", "pilot/pilotdeck.yaml"),
        ]),
        &root,
    )
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { pilotdeck_gateway::ensure_sigxfsz_ignored() });
    let case = serde_json::json!({"responses":[{"json":{"qrcode":"code","qrcode_img_content":"https://fixture.invalid/qr"}},{"json":{"status":"confirmed","ilink_bot_id":"new-account","bot_token":"new-token"}}]});
    let mut fake = responses(&case);
    let mut dialog = Dialog::new(&["yes", "yes"]);
    run_with(&["weixin".into()], &paths, &mut dialog, &mut fake).unwrap();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/gateway-setup/weixin-write-failure.json"
    ))
    .unwrap();
    let baseline_output = oracle["cases"][0]["stdout"]
        .as_str()
        .unwrap()
        .split_once("SETUP_EVENTS=")
        .expect("driver receipt")
        .0;
    assert_eq!(dialog.output, baseline_output);
    assert_eq!(oracle["cases"][0]["afterCredentials"], "");
    assert_eq!(oracle["cases"][0]["after"], "keep: original\n");
    assert!(!dialog.output.contains("微信登录成功"));
    assert_eq!(fake.base.requests.len(), 2);
    println!("WEIXIN_CREDENTIAL_FAILURE=EFBIG: file too large, write");
}
#[cfg(unix)]
#[test]
fn weixin_failed_credential_write_keeps_old_credentials_and_config() {
    let scratch = Scratch::new();
    let paths = paths(&scratch);
    fs::create_dir_all(paths.credentials.parent().unwrap()).unwrap();
    let credentials = r#"{"baseUrl":"https://fixture.invalid/old","botToken":"old-token","accountId":"old-account"}"#;
    let config = "keep: original";
    fs::write(&paths.credentials, credentials).unwrap();
    fs::write(&paths.config, config).unwrap();
    let child = concat!(module_path!(), "::weixin_credential_write_failure_child")
        .split_once("::")
        .unwrap()
        .1;
    let output=std::process::Command::new("/bin/sh").arg("-c").arg("ulimit -f 0 || exit 1; exec \"$0\" --exact \"$1\" --ignored --nocapture --test-threads=2").arg(std::env::current_exe().unwrap()).arg(child).env("PDRS_WX_FAILURE_ROOT",&scratch.0).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line == "WEIXIN_CREDENTIAL_FAILURE=EFBIG: file too large, write"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&paths.credentials).unwrap(), credentials);
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), config);
    assert_no_setup_temporary(paths.credentials.parent().unwrap());
}

#[cfg(unix)]
#[test]
#[ignore = "spawned and deadline-guarded by weixin_unsafe_credential_inputs_are_bounded_and_preserved"]
fn weixin_unsafe_credential_child() {
    let root = PathBuf::from(std::env::var_os("PDRS_WX_UNSAFE_ROOT").unwrap());
    let paths = SetupPaths::new(
        &Env::from_pairs(&[
            ("HOME", "home"),
            ("PILOT_HOME", "pilot"),
            ("PILOTDECK_CONFIG_PATH", "pilot/pilotdeck.yaml"),
        ]),
        &root,
    )
    .unwrap();
    let mut fake = IlinkMock::new();
    let mut dialog = Dialog::new(&[]);
    let error = run_with(&["weixin".into()], &paths, &mut dialog, &mut fake).unwrap_err();
    let SetupError::Failure(crate::Failure::Error(message)) = error else {
        panic!("unexpected error: {error:?}")
    };
    assert_eq!(message, std::env::var("PDRS_WX_UNSAFE_ERROR").unwrap());
    assert!(fake.base.requests.is_empty());
    assert!(!dialog.output.contains("微信登录成功"));
    println!("WEIXIN_UNSAFE_INPUT_REJECTED={message}");
}
#[cfg(unix)]
#[test]
fn weixin_unsafe_credential_inputs_are_bounded_and_preserved() {
    use std::{
        os::unix::fs::MetadataExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    for fifo in [true, false] {
        let scratch = Scratch::new();
        let paths = paths(&scratch);
        fs::create_dir_all(paths.credentials.parent().unwrap()).unwrap();
        let expected = if fifo {
            let status = Command::new("/usr/bin/mkfifo")
                .arg(&paths.credentials)
                .status()
                .unwrap();
            assert!(status.success());
            "Refusing non-regular Weixin credential file"
        } else {
            fs::File::create(&paths.credentials)
                .unwrap()
                .set_len(super::super::weixin::CREDENTIAL_LIMIT + 1)
                .unwrap();
            "Weixin credentials exceed 1048576-byte limit"
        };
        fs::write(&paths.config, b"keep: old\n").unwrap();
        let before = fs::symlink_metadata(&paths.credentials).unwrap();
        let name = concat!(module_path!(), "::weixin_unsafe_credential_child")
            .split_once("::")
            .unwrap()
            .1;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--ignored", "--nocapture"])
            .env("PDRS_WX_UNSAFE_ROOT", &scratch.0)
            .env("PDRS_WX_UNSAFE_ERROR", expected)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Weixin credential read did not terminate within three seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("WEIXIN_UNSAFE_INPUT_REJECTED={expected}"))
        );
        let after = fs::symlink_metadata(&paths.credentials).unwrap();
        assert_eq!(
            (before.ino(), before.mode(), before.len()),
            (after.ino(), after.mode(), after.len())
        );
        if !fifo {
            assert!(
                fs::read(&paths.credentials)
                    .unwrap()
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
        assert_eq!(fs::read(&paths.config).unwrap(), b"keep: old\n");
        assert_no_setup_temporary(paths.credentials.parent().unwrap());
    }
}

fn guarded_body_server(slow: bool) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).unwrap();
        if slow {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n")
                .unwrap();
            for _ in 0..50 {
                if socket.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else {
            let body = " ".repeat(65);
            let _ = socket.write_all(http_response("200 OK", "", &body).as_bytes());
        }
    });
    (format!("http://{address}"), worker)
}
struct LocalGuardedIlink {
    network: http::Network,
    url: String,
    calls: usize,
}
impl Fetch for LocalGuardedIlink {
    fn json(&mut self, _: &str, _: &str, _: Option<&str>) -> Result<http::Response, String> {
        panic!("unexpected generic request")
    }
    fn ilink_json(&mut self, _: &str, poll: bool) -> Result<http::Response, IlinkFailure> {
        self.calls += 1;
        if !poll {
            return Ok(http::Response {
                status: 200,
                json: parse_json(
                    r#"{"qrcode":"fixture","qrcode_img_content":"https://fixture.invalid/qr"}"#,
                )
                .unwrap(),
            });
        }
        self.network.ilink_json(&self.url, true)
    }
}
#[test]
fn weixin_native_body_limits_reject_oversize_and_continuous_slow_flow_without_saving() {
    use std::time::{Duration, Instant};
    for slow in [false, true] {
        let expected = if slow {
            "iLink response body exceeded time limit"
        } else {
            "iLink response exceeds 64-byte limit"
        };
        // Initial QR and poll responses share the same bounded body path.
        let (url, worker) = guarded_body_server(slow);
        let started = Instant::now();
        let error = http::Network::with_ilink_body_limits(64, Duration::from_millis(40))
            .ilink_json(&url, false)
            .err()
            .expect("body guard rejects response");
        assert!(
            matches!(error, IlinkFailure::Error(ref value) if value == expected),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        worker.join().unwrap();
        let scratch = Scratch::new();
        let paths = paths(&scratch);
        fs::create_dir_all(paths.credentials.parent().unwrap()).unwrap();
        let credentials = br#"{"baseUrl":"https://fixture.invalid/old","botToken":"old-token","accountId":"old-account"}"#;
        fs::write(&paths.credentials, credentials).unwrap();
        fs::write(&paths.config, b"keep: original\n").unwrap();
        let (url, worker) = guarded_body_server(slow);
        let mut network = LocalGuardedIlink {
            network: http::Network::with_ilink_body_limits(64, Duration::from_millis(40)),
            url,
            calls: 0,
        };
        let mut dialog = Dialog::new(&["yes", "yes"]);
        run_with(&["weixin".into()], &paths, &mut dialog, &mut network).unwrap();
        worker.join().unwrap();
        assert_eq!(network.calls, 2);
        assert!(dialog.output.contains(&format!("微信登录失败: {expected}")));
        assert!(!dialog.output.contains("微信登录成功"));
        assert_eq!(fs::read(&paths.credentials).unwrap(), credentials);
        assert_eq!(fs::read(&paths.config).unwrap(), b"keep: original\n");
        assert_no_setup_temporary(paths.credentials.parent().unwrap());
    }
}
