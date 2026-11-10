use super::*;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    polls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(mode: &'static str) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let base = url.clone();
    let requests = Arc::new(Mutex::new(vec![]));
    let recorded = requests.clone();
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    let task = tokio::spawn(async move {
        let qr_count = Arc::new(AtomicUsize::new(0));
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let base = base.clone();
            let recorded = recorded.clone();
            let count = count.clone();
            let qr_count = qr_count.clone();
            tokio::spawn(async move {
                let mut buf = vec![0; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                recorded.lock().unwrap().push(path.clone());
                let (status, body) = if mode == "http-error" {
                    (503, json!({}))
                } else if path.contains("get_bot_qrcode") {
                    (
                        200,
                        json!({"qrcode":"a+b/&", "qrcode_img_content":"https://mock.invalid/qr"}),
                    )
                } else if path.contains("get_qrcode_status") {
                    let poll = qr_count.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        match mode {
                            "expired" => json!({"status":"expired"}),
                            "missing-token" => json!({"status":"confirmed"}),
                            _ if poll == 0 => json!({"status":"scaned"}),
                            _ => {
                                json!({"status":"confirmed","baseurl":base,"bot_token":"isolated-mock-only","ilink_bot_id":"mock-account"})
                            }
                        },
                    )
                } else {
                    assert!(request.contains("ilink_bot_token"));
                    count.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        if mode == "session-expired" {
                            json!({"errcode":-14})
                        } else if mode == "messages" {
                            json!({"ret":0,"get_updates_buf":"must-not-advance","msgs":[{"from_user_id":"mock-user"}]})
                        } else {
                            json!({"ret":0,"get_updates_buf":"persisted-cursor","msgs":[]})
                        },
                    )
                };
                let body = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    Mock {
        url,
        requests,
        polls,
        task,
    }
}
struct Home(PathBuf);
struct Manual {
    url: String,
    scanned: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Manual {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn manual(mode: &'static str) -> Manual {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let base = url.clone();
    let scanned = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let scanned_in = scanned.clone();
    let release_in = release.clone();
    let task = tokio::spawn(async move {
        let qr_count = Arc::new(AtomicUsize::new(0));
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let base = base.clone();
            let scanned = scanned_in.clone();
            let release = release_in.clone();
            let qr_count = qr_count.clone();
            tokio::spawn(async move {
                let mut buf = vec![0; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                let body = if path.contains("get_bot_qrcode") {
                    json!({"qrcode":"manual","qrcode_img_content":"https://mock.invalid/qr"})
                } else if path.contains("get_qrcode_status") {
                    let poll = qr_count.fetch_add(1, Ordering::SeqCst);
                    if poll == 0 {
                        // Hold the QR completion until the stale record is on disk.
                        scanned.notify_one();
                        release.notified().await;
                    }
                    json!({"status":"confirmed","baseurl":base,"bot_token":"isolated-mock-only","ilink_bot_id":"mock-account"})
                } else if mode == "session-expired" {
                    json!({"errcode":-14})
                } else {
                    json!({"ret":0,"get_updates_buf":"after-login","msgs":[]})
                };
                let body = body.to_string();
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    Manual { url, scanned, release, task }
}
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("config-weixin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn status(home: &Home) -> Value {
    serde_json::from_slice(&std::fs::read(home.0.join("channels/runtime-status.json")).unwrap())
        .unwrap()
}
async fn wait(home: &Home, state: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        loop {
            let s = status(home);
            if s["channels"]["weixin"]["state"] == state {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn native_qr_login_persists_credentials_mapper_status_and_starts_real_polling() {
    let home = Home::new();
    let server = mock("ok").await;
    std::fs::create_dir(home.0.join("channels")).unwrap();
    let mapper = json!({"activeByChatId":{"chat":"saved-session"},"projectByChatId":{"chat":"saved-project"}});
    std::fs::write(
        home.0.join("channels/weixin.state.json"),
        mapper.to_string(),
    )
    .unwrap();
    std::fs::write(
        home.0.join("channels/runtime-status.json"),
        r#"{"channels":{"other":{"state":"connected"}}}"#,
    )
    .unwrap();
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
    ));
    host.hot_start().await.unwrap();
    assert_eq!(
        status(&home)["channels"]["weixin"]["state"],
        "waiting_for_login"
    );
    wait(&home, "connected").await;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !std::fs::read_to_string(home.0.join("weixin-credentials.json"))
            .unwrap()
            .contains("persisted-cursor")
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(server.polls.load(Ordering::SeqCst) > 0);
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.ends_with("qrcode=a%2Bb%2F%26"))
    );
    assert_eq!(status(&home)["channels"]["other"]["state"], "connected");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(home.0.join("channels/weixin.state.json")).unwrap())
            .unwrap();
    assert_eq!(saved, mapper);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(home.0.join("weixin-credentials.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    host.hot_start().await.unwrap(); // Saved credentials bypass the QR flow.
    let requests = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.contains("get_bot_qrcode"))
        .count();
    assert_eq!(requests, 1);
    host.close().await.unwrap();
    assert_eq!(status(&home)["channels"]["weixin"]["state"], "stopped");
    let polls = server.polls.load(Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(polls, server.polls.load(Ordering::SeqCst));
}
#[tokio::test]
async fn native_login_errors_expiry_and_invalid_endpoints_are_truthful() {
    assert!(
        NativeWeixinFactory::new(
            PathBuf::from("/unused"),
            Some("http://public.invalid".into())
        )
        .is_err()
    );
    assert!(
        NativeWeixinFactory::new(
            PathBuf::from("/unused"),
            Some("https://user:secret@mock.invalid".into())
        )
        .is_err()
    );
    for (mode, expected) in [
        ("http-error", "HTTP status 503"),
        ("missing-token", "missing credentials"),
        ("expired", "expired too many times"),
    ] {
        let home = Home::new();
        let server = mock(mode).await;
        let host = WeixinChannelHost::new(Arc::new(
            NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
        ));
        host.hot_start().await.unwrap();
        assert!(
            wait(&home, "failed").await["channels"]["weixin"]["error"]
                .as_str()
                .unwrap()
                .contains(expected)
        );
        assert!(!home.0.join("weixin-credentials.json").exists());
        host.close().await.unwrap();
    }
}
#[tokio::test]
async fn native_saved_login_expiry_retains_credentials() {
    let home = Home::new();
    let server = mock("session-expired").await;
    let value = json!({"baseUrl":server.url,"botToken":"isolated-mock-only","accountId":"mock-account","cursor":"original"});
    std::fs::write(home.0.join("weixin-credentials.json"), value.to_string()).unwrap();
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
    ));
    host.hot_start().await.unwrap();
    wait(&home, "expired").await;
    host.close().await.unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(home.0.join("weixin-credentials.json")).unwrap())
            .unwrap();
    assert_eq!(saved, value);
}
#[tokio::test]
async fn mapper_parse_and_read_failures_preserve_original_and_do_not_start_network() {
    for bytes in [
        b"{invalid".as_slice(),
        b"{\"activeByChatId\":null}".as_slice(),
    ] {
        let home = Home::new();
        std::fs::create_dir(home.0.join("channels")).unwrap();
        let path = home.0.join("channels/weixin.state.json");
        std::fs::write(&path, bytes).unwrap();
        let server = mock("ok").await;
        let host = WeixinChannelHost::new(Arc::new(
            NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
        ));
        assert!(host.hot_start().await.is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(server.requests.lock().unwrap().is_empty());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        std::fs::create_dir(home.0.join("channels")).unwrap();
        let path = home.0.join("channels/weixin.state.json");
        let bytes =
            br#"{"activeByChatId":{"chat":"original"},"projectByChatId":{"chat":"project"}}"#;
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            std::fs::read(&path).is_err(),
            "unreadable regression requires an unprivileged test user"
        );
        let host = WeixinChannelHost::new(Arc::new(
            NativeWeixinFactory::new(home.0.clone(), Some("http://127.0.0.1:1".into())).unwrap(),
        ));
        assert!(host.hot_start().await.is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    let home = Home::new();
    let server = mock("http-error").await;
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
    ));
    host.hot_start().await.unwrap();
    assert!(
        !home.0.join("channels/weixin.state.json").exists(),
        "starting login must not write mapper"
    );
    host.close().await.unwrap();
}
#[tokio::test]
async fn pending_messages_are_durable_before_cursor_and_restart_waits_for_consumer_ack() {
    let home = Home::new();
    let server = mock("messages").await;
    std::fs::write(home.0.join("weixin-credentials.json"),json!({"baseUrl":server.url,"botToken":"isolated-mock-only","accountId":"mock-account","cursor":"original"}).to_string()).unwrap();
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
    ));
    host.hot_start().await.unwrap();
    let inbox = WeixinInbox::new(home.0.clone());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let pending = inbox.pending().unwrap();
            let saved: Value = serde_json::from_slice(
                &std::fs::read(home.0.join("weixin-credentials.json")).unwrap(),
            )
            .unwrap();
            if pending.len() == 1 && saved["cursor"] == "must-not-advance" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let batch = inbox.pending().unwrap().remove(0);
    assert_eq!(batch["messages"][0]["from_user_id"], "mock-user");
    assert_eq!(batch["cursorBefore"], "original");
    assert!(batch.get("botToken").is_none());
    host.hot_start().await.unwrap();
    let count = server.polls.load(Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(server.polls.load(Ordering::SeqCst), count);
    host.close().await.unwrap();
    assert!(inbox.acknowledge("../bad", None).is_err());
    let mapper = json!({"activeByChatId":{"mock-user":"accepted-session"},"projectByChatId":{}});
    inbox
        .acknowledge(batch["id"].as_str().unwrap(), Some(&mapper))
        .unwrap();
    assert!(inbox.pending().unwrap().is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(home.0.join("channels/weixin.state.json")).unwrap()
        )
        .unwrap(),
        mapper
    );
}
#[tokio::test]
async fn stale_poll_cannot_replace_new_account_or_its_cursor() {
    stale_poll_after_credential_update(true).await;
}
#[tokio::test]
async fn same_account_inflight_response_cannot_rewind_cursor_or_enqueue_stale_messages() {
    stale_poll_after_credential_update(false).await;
}
async fn stale_poll_after_credential_update(replace_account: bool) {
    let home = Home::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let path = home.0.join("weixin-credentials.json");
    let a = json!({"baseUrl":url,"botToken":"mock-A","accountId":"account-A","cursor":"a0"});
    let b = if replace_account {
        json!({"baseUrl":url,"botToken":"mock-B","accountId":"account-B","cursor":"b0"})
    } else {
        let mut value = a.clone();
        value["cursor"] = "a2".into();
        value
    };
    std::fs::write(&path, a.to_string()).unwrap();
    let (started, request) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let resume = release.clone();
    let serving = tokio::spawn(async move {
        let mut started = Some(started);
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 8192];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0);
                buffer.extend_from_slice(&chunk[..size]);
                if let Some(at) = buffer.windows(4).position(|p| p == [13, 10, 13, 10]) {
                    break at + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            assert!(headers.contains(if index == 1 && replace_account {
                "Bearer mock-B"
            } else {
                "Bearer mock-A"
            }));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while buffer.len() < header_end + length {
                let mut chunk = [0u8; 8192];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0);
                buffer.extend_from_slice(&chunk[..size]);
            }
            let body: Value =
                serde_json::from_slice(&buffer[header_end..header_end + length]).unwrap();
            assert_eq!(
                body["get_updates_buf"],
                if index == 0 {
                    "a0"
                } else if replace_account {
                    "b0"
                } else {
                    "a2"
                }
            );
            if index == 0 {
                let _ = started.take().unwrap().send(());
                resume.notified().await;
            }
            let body = if index == 0 {
                r#"{"ret":0,"msgs":[{"from_user_id":"stale-response"}],"get_updates_buf":"a1"}"#
            } else {
                r#"{"errcode":-14}"#
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    // The first accepted request is signalled independently of its response.
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(url)).unwrap(),
    ));
    host.hot_start().await.unwrap();
    // Wait for the request to be in flight before replacing the login file.
    request.await.unwrap();
    if replace_account {
        // A real new login lands before the old response returns.
        storage::Storage::new(home.0.clone())
            .save_login(b.clone())
            .unwrap();
    } else {
        let owner = storage::Storage::new(home.0.clone());
        assert_eq!(owner.advance_cursor(&a, "a2").unwrap().unwrap(), b);
    }
    release.notify_one();
    wait(&home, "expired").await;
    host.close().await.unwrap();
    serving.await.unwrap();
    let storage = storage::Storage::new(home.0.clone());
    // TS WeixinChannel.ts:389 saveCursor() reloads the current record, so the
    // retired generation can neither replace the account nor rewind its cursor.
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap(),
        b
    );
    assert_eq!(storage.credentials().unwrap().unwrap(), b);
    assert!(
        WeixinInbox::new(home.0.clone())
            .pending()
            .unwrap()
            .is_empty(),
        "stale HTTP responses must be discarded before message handoff"
    );
}

#[tokio::test]
async fn qr_login_confirmation_overwrites_existing_disk_credentials() {
    let home = Home::new();
    let server = manual("ok").await;
    // No credentials at start: the channel really runs run_qr_login.
    let host = WeixinChannelHost::new(Arc::new(
        NativeWeixinFactory::new(home.0.clone(), Some(server.url.clone())).unwrap(),
    ));
    host.hot_start().await.unwrap();
    // A stale login lands while QR sits on the "scaned" status.
    server.scanned.notified().await;
    let old = json!({
        "baseUrl": server.url,
        "botToken": "old-token",
        "accountId": "old-account",
        "cursor": "old-cursor"
    });
    std::fs::write(home.0.join("weixin-credentials.json"), old.to_string()).unwrap();
    server.release.notify_one();
    wait(&home, "connected").await;
    host.close().await.unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(home.0.join("weixin-credentials.json")).unwrap())
            .unwrap();
    // TS WeixinChannel.ts:321 confirms the new login after isStaleGeneration,
    // then :990-998 overwrites the older record instead of keeping it.
    assert_eq!(saved["accountId"], "mock-account");
    assert_eq!(saved["botToken"], "isolated-mock-only");
    // The new login drops the retired record's cursor; polling may then write its own.
    assert_ne!(saved.get("cursor"), Some(&json!("old-cursor")));
}

#[test]
fn duplicate_ack_does_not_rewrite_mapper_and_same_account_cursor_replacement_is_preserved() {
    let home = Home::new();
    let store = storage::Storage::new(home.0.clone());
    let a = json!({"baseUrl":"https://mock.invalid","accountId":"account","botToken":"mock-only","cursor":"a0"});
    let mut advanced = a.clone();
    advanced["cursor"] = "external-cursor".into();
    std::fs::write(home.0.join("weixin-credentials.json"), advanced.to_string()).unwrap();
    assert!(store.advance_cursor(&a, "stale-cursor").unwrap().is_none());
    assert_eq!(store.credentials().unwrap().unwrap(), advanced);
    let mapper = json!({"activeByChatId":{"chat":"current"},"projectByChatId":{}});
    store.save_mapper(&mapper).unwrap();
    let bytes = std::fs::read(home.0.join("channels/weixin.state.json")).unwrap();
    let stale = json!({"activeByChatId":{},"projectByChatId":{}});
    WeixinInbox::new(home.0.clone())
        .acknowledge(&"0".repeat(64), Some(&stale))
        .unwrap();
    assert_eq!(
        std::fs::read(home.0.join("channels/weixin.state.json")).unwrap(),
        bytes
    );
}

#[test]
fn native_credential_writers_share_one_owner_and_only_one_expected_cursor_wins() {
    let home = Home::new();
    let initial = json!({"baseUrl":"https://mock.invalid","botToken":"mock-only","accountId":"account","cursor":"c0"});
    let store = storage::Storage::new(home.0.clone());
    store.save_login(initial.clone()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let home = home.0.clone();
            let expected = initial.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let owner = storage::Storage::new(home);
                barrier.wait();
                owner
                    .advance_cursor(&expected, &format!("c{i}-won"))
                    .unwrap()
                    .is_some()
            })
        })
        .collect();
    let winners = threads
        .into_iter()
        .map(|thread| usize::from(thread.join().unwrap()))
        .sum::<usize>();
    assert_eq!(winners, 1);
    assert_ne!(store.credentials().unwrap().unwrap()["cursor"], "c0");
}
