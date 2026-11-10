use super::*;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn merge_inbound_and_materialize_into_one_diagnostics_block() {
    let inbound = format_attachment_diagnostics_block(&["缺 URL".into()]);
    let merged = merge_attachment_diagnostics(inbound, &["坏图".into()]);
    assert_eq!(merged.matches("[Attachment diagnostics]").count(), 1);
    assert!(merged.contains("- 缺 URL"));
    assert!(merged.contains("- 坏图"));
}

    struct Fixture {
        base: String,
        requests: Arc<StdMutex<Vec<(String, Value)>>>,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn fixture(payload: &'static [u8]) -> (Fixture, String) {
        set_test_permit_loopback_fetch(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let download_url = format!("{base}download.bin");
        let upload_base = base.clone();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header = String::from_utf8_lossy(&bytes);
                let path = header.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body_start = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap_or(bytes.len());
                let body = if bytes.len() > body_start {
                    String::from_utf8_lossy(&bytes[body_start..]).to_string()
                } else {
                    String::new()
                };
                if path.contains("getuploadurl") {
                    let value: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    recorded.lock().unwrap().push(("getuploadurl".into(), value));
                    let reply = json!({
                        "ret": 0,
                        "upload_full_url": format!("{upload_base}upload"),
                        "media": {"filekey": "k"}
                    });
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                                reply.to_string().len(),
                                reply
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    continue;
                }
                if path.contains("sendmessage") {
                    let value: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    recorded.lock().unwrap().push(("sendmessage".into(), value));
                    let reply = json!({"ret":0});
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                                reply.to_string().len(),
                                reply
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    continue;
                }
                if path.contains("download.bin") {
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                                payload.len()
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    socket.write_all(payload).await.unwrap();
                    continue;
                }
                if path.contains("upload") {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nx-encrypted-param: mock-cdn-param\r\n\r\n",
                        )
                        .await
                        .unwrap();
                    continue;
                }
                socket
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        (
            Fixture {
                base: base.clone(),
                requests,
                _task: task,
            },
            download_url,
        )
    }

    #[tokio::test]
    async fn rejects_private_and_metadata_attachment_urls() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(false);
        use super::super::attachment_net::{is_blocked_ip, set_test_dns_map};
        use std::collections::HashMap;
        use std::net::{IpAddr, Ipv4Addr};
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_blocked_ip("::ffff:10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(super::super::attachment_net::is_blocked_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(super::super::attachment_net::is_blocked_ipv4(Ipv4Addr::new(224, 0, 0, 1)));
        set_test_dns_map(Some(HashMap::from([(
            "rebind.test".into(),
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))],
        )])));
        assert!(
            validate_attachment_fetch_url("http://rebind.test/evil")
                .await
                .is_err()
        );
        set_test_dns_map(None);
        for url in [
            "http://127.0.0.1/evil",
            "http://10.0.0.1/evil",
            "http://[::ffff:127.0.0.1]/evil",
            "http://169.254.169.254/latest/meta-data",
            "ftp://example.com/file",
        ] {
            assert!(
                validate_attachment_fetch_url(url).await.is_err(),
                "expected reject: {url}"
            );
        }
    }

    #[test]
    fn rejects_path_traversal_in_storage_components() {
        let home = std::env::temp_dir().join(format!("weixin-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        for (chat, message) in [
            ("../etc", "m1"),
            ("user", "../../outside"),
            ("/tmp/abs", "m1"),
        ] {
            let dir = attachment_store_dir(&home, chat, message).unwrap();
            let canonical = std::fs::canonicalize(home.join("im-attachments/weixin")).unwrap();
            assert!(
                dir.starts_with(&canonical),
                "store path must stay under root: {}",
                dir.display()
            );
            assert!(!dir.starts_with("/etc"));
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn rejects_redirect_to_private_host() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let private = "http://10.0.0.1/evil";
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {private}\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let url = format!("{base}/start");
        assert!(attachment_net::pinned_get(&url, LIMIT).await.is_err());
        task.abort();
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn rejects_symlink_attachment_target() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let home = std::env::temp_dir().join(format!("weixin-symlink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (fixture, url) = fixture(b"payload").await;
        let _ = fixture;
        let mut attachments = vec![json!({"type":"file","name":"doc.bin","url":url})];
        materialize_inbound(&home, "user-1", "m1", &mut attachments)
            .await
            .unwrap();
        let store = attachment_store_dir(&home, "user-1", "m1").unwrap();
        let outside = home.join("outside-secret.bin");
        std::fs::write(&outside, b"secret").unwrap();
        let link = store.join("doc.bin");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let mut again = vec![json!({"type":"file","name":"doc.bin","url":url})];
        materialize_inbound(&home, "user-1", "m1", &mut again)
            .await
            .unwrap();
        assert!(again.is_empty());
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn skips_undecryptable_image_without_failing_batch() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let home = std::env::temp_dir().join(format!("weixin-bad-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (good_fixture, good_url) = fixture(b"hello-weixin").await;
        let (bad_fixture, bad_url) = fixture(b"not-a-valid-image").await;
        let _ = (good_fixture, bad_fixture);
        let mut attachments = vec![
            json!({
                "type": "image",
                "name": "bad.jpg",
                "url": bad_url,
                "metadata": { "weixinMediaOwner": { "aeskey": "00112233445566778899aabbccddeeff" } }
            }),
            json!({"type":"file","name":"doc.bin","url":good_url}),
        ];
        let diags = materialize_inbound(&home, "user-1", "m1", &mut attachments)
            .await
            .unwrap();
        let store = attachment_store_dir(&home, "user-1", "m1").unwrap();
        assert!(!store.join("bad.jpg").exists());
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0]["path"].as_str().is_some());
        assert!(
            diags.iter().any(|line| {
                line.starts_with("微信附件处理失败：Error: 微信图片解密后仍不是可识别图片（")
                    && line.contains("; at=")
                    && line.contains("candidates=")
                    && !line.contains("code=gateway_request_failed")
            }),
            "expected TS formatWeixinError diagnostic, got {diags:?}"
        );
        let merged = merge_attachment_diagnostics(String::new(), &diags);
        assert!(merged.contains("[Attachment diagnostics]"));
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
  #[cfg(unix)]
    async fn rejects_symlink_chat_directory_before_create() {
        let home = std::env::temp_dir().join(format!("weixin-chat-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let root = home.join("im-attachments").join("weixin");
        std::fs::create_dir_all(&root).unwrap();
        let outside = home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("chat-link")).unwrap();
        assert!(attachment_store_dir(&home, "chat-link", "m1").is_err());
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn post_upload_sets_octet_stream_content_type() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let captured = Arc::new(StdMutex::new(String::new()));
        let seen = captured.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let upload_url = format!("{base}/upload");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                *seen.lock().unwrap() = String::from_utf8_lossy(&bytes).into_owned();
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nx-encrypted-param: mock-cdn-param\r\n\r\n",
                    )
                    .await
                    .unwrap();
            }
        });
        let key = [0u8; 16];
        upload_encrypted_cdn(&upload_url, b"cipher", &key)
            .await
            .unwrap();
        let header = captured.lock().unwrap().clone();
        assert!(
            header
                .to_ascii_lowercase()
                .contains("content-type: application/octet-stream")
        );
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    async fn preserves_post_body_across_307_redirect() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let bodies = Arc::new(StdMutex::new(Vec::new()));
        let recorded = bodies.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let final_url = format!("{base}/final");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header = String::from_utf8_lossy(&bytes);
                let body_start = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap_or(bytes.len());
                let body = bytes[body_start..].to_vec();
                let path = header.split_whitespace().nth(1).unwrap_or("/");
                recorded.lock().unwrap().push(body);
                if path.contains("/final") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
                        .await
                        .unwrap();
                    continue;
                }
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {final_url}\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let payload = Bytes::from_static(b"payload-bytes");
        let response = attachment_net::pinned_post(&format!("{base}/start"), payload.clone(), 1024)
            .await
            .unwrap();
        assert_eq!(response.body, b"hello");
        let posted = bodies.lock().unwrap();
        assert_eq!(posted.len(), 2);
        assert_eq!(posted[1].as_slice(), payload.as_ref());
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    async fn get_after_302_does_not_replay_post_body_on_307() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let methods = Arc::new(StdMutex::new(Vec::new()));
        let bodies = Arc::new(StdMutex::new(Vec::new()));
        let recorded_methods = methods.clone();
        let recorded_bodies = bodies.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let mid = format!("{base}/mid");
        let final_url = format!("{base}/final");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header = String::from_utf8_lossy(&bytes);
                let method = header.split_whitespace().next().unwrap_or("").to_string();
                let path = header.split_whitespace().nth(1).unwrap_or("/");
                let body_start = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap_or(bytes.len());
                recorded_methods.lock().unwrap().push(method);
                recorded_bodies
                    .lock()
                    .unwrap()
                    .push(bytes[body_start..].to_vec());
                if path.contains("/final") {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                    continue;
                }
                if path.contains("/mid") {
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {final_url}\r\nContent-Length: 0\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    continue;
                }
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {mid}\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let payload = Bytes::from_static(b"payload-bytes");
        let payload_len = payload.len();
        attachment_net::pinned_post(&format!("{base}/start"), payload, 1024)
            .await
            .unwrap();
        let posted_methods = methods.lock().unwrap();
        let posted_bodies = bodies.lock().unwrap();
        assert_eq!(posted_methods.as_slice(), ["POST", "GET", "GET"]);
        assert_eq!(posted_bodies[0].len(), payload_len);
        assert!(posted_bodies[1].is_empty());
        assert!(posted_bodies[2].is_empty());
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    async fn upload_rejects_non_200_with_error_header() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upload_url = format!("http://{addr}/upload");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nx-error-message: denied\r\n\r\n",
                    )
                    .await
                    .unwrap();
            }
        });
        let key = [0u8; 16];
        let error = upload_encrypted_cdn(&upload_url, b"x", &key).await.unwrap_err();
        assert!(error.message.contains("weixin CDN upload HTTP 201"));
        assert!(error.message.contains("denied"));
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    async fn upload_non_200_preserves_empty_x_error_message() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upload_url = format!("http://{addr}/upload");
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 9\r\nx-error-message: \r\n\r\nbody-fail",
                    )
                    .await
                    .unwrap();
            }
        });
        let key = [0u8; 16];
        let error = upload_encrypted_cdn(&upload_url, b"x", &key).await.unwrap_err();
        assert_eq!(error.message, "weixin CDN upload HTTP 500: ");
        set_test_permit_loopback_fetch(false);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn rejects_hard_link_without_truncate() {
        let _lock = super::attachment_net::test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let home = std::env::temp_dir().join(format!("weixin-hardlink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (fixture, url) = fixture(b"payload").await;
        let _ = fixture;
        let mut attachments = vec![json!({"type":"file","name":"doc.bin","url":url})];
        materialize_inbound(&home, "user-1", "m1", &mut attachments)
            .await
            .unwrap();
        let store = attachment_store_dir(&home, "user-1", "m1").unwrap();
        let link = store.join("doc.bin");
        let outside = home.join("outside.bin");
        std::fs::write(&outside, b"victim").unwrap();
        std::fs::remove_file(&link).unwrap();
        std::fs::hard_link(&outside, &link).unwrap();
        let mut again = vec![json!({"type":"file","name":"doc.bin","url":url})];
        let diags = materialize_inbound(&home, "user-1", "m1", &mut again)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"victim");
        assert!(again.is_empty());
        assert!(!diags.is_empty());
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn inbound_download_writes_local_path() {
        let _lock = super::attachment_net::test_settings_lock();
        let home = std::env::temp_dir().join(format!("weixin-attach-in-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let (fixture, url) = fixture(b"hello-weixin").await;
        let _ = fixture;
        let mut attachments = vec![json!({"type":"file","name":"doc.bin","url":url})];
        materialize_inbound(&home, "user-1", "m1", &mut attachments)
            .await
            .unwrap();
        let path = PathBuf::from(attachments[0]["path"].as_str().unwrap());
        assert!(path.is_file());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello-weixin");
        assert_eq!(attachments[0]["bytes"].as_u64(), Some(12));
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn outbound_send_uploads_encrypted_payload() {
        let _lock = super::attachment_net::test_settings_lock();
        let home = std::env::temp_dir().join(format!("weixin-attach-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join("out.bin");
        std::fs::write(&file, b"payload").unwrap();
        let (fixture, _) = fixture(b"x").await;
        let http = Http::new();
        let credentials = json!({"baseUrl":fixture.base,"botToken":"bot-token"});
        let ok = send_outbound(
            &http,
            &credentials,
            "user-1",
            "ctx",
            &json!({"type":"file","name":"out.bin","path":file}),
        )
        .await
        .unwrap();
        assert!(ok);
        let requests = fixture.requests.lock().unwrap();
        assert!(requests.iter().any(|(name, _)| name == "getuploadurl"));
        assert!(requests.iter().any(|(name, _)| name == "sendmessage"));
        let send = requests
            .iter()
            .find(|(name, _)| name == "sendmessage")
            .map(|(_, body)| body.clone())
            .unwrap();
        assert_eq!(
            send["msg"]["item_list"][0]["type"].as_i64(),
            Some(super::super::item_types::MESSAGE_ITEM_FILE)
        );
        assert!(send["msg"]["item_list"][0]["file_item"]["media"]["encrypt_query_param"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        let aes_field = send["msg"]["item_list"][0]["file_item"]["media"]["aes_key"]
            .as_str()
            .unwrap();
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(aes_field)
            .unwrap();
        assert_eq!(decoded.len(), 32);
        assert!(decoded.iter().all(|byte| byte.is_ascii_hexdigit()));
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn send_outbound_rejects_local_path_without_grant() {
        let http = Http::new();
        let ok = send_outbound(
            &http,
            &json!({"baseUrl": "http://127.0.0.1"}),
            "user-1",
            "ctx-1",
            &json!({
                "type": "file",
                "name": "secret.pdf",
                "source": "local_path",
                "path": "/tmp/secret.pdf"
            }),
        )
        .await
        .unwrap();
        assert!(!ok, "must not upload unauthorized local_path attachment");
        assert_eq!(
            super::local_path_authorization_reply("/tmp/secret.pdf"),
            "附件 /tmp/secret.pdf 需要授权后才能发送。"
        );
    }

    #[test]
    fn attachment_store_dir_maps_whitespace_chat_to_safe_part() {
        let home = std::env::temp_dir().join(format!(
            "pdrs-weixin-attach-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let dir = attachment_store_dir(&home, "  ", "msg-1").unwrap();
        let path = dir.to_string_lossy();
        assert!(
            path.contains("weixin/_/msg-1"),
            "expected TS safePathPart single underscore for spaces, got {path}"
        );
        let _ = std::fs::remove_dir_all(home);
    }
