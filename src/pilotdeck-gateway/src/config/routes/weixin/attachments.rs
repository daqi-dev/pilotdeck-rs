//! Inbound download and outbound upload for Weixin attachments (WeixinChannel.ts).
use super::attachment_net::{self, validate_attachment_fetch_url};
use super::http::{Http, failure};
use super::media::{
    aes_ecb_padded_size, build_weixin_cdn_upload_url, decrypt_weixin_media_if_needed,
    encrypt_aes_128_ecb, weixin_outbound_aes_key_field,
};
use super::storage::text;
use crate::config::routes::Result;
use bytes::Bytes;
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[cfg(test)]
pub(crate) use super::attachment_net::set_test_permit_loopback_fetch;

const LIMIT: usize = 50 * 1024 * 1024;

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn path_part(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err(failure("Invalid attachment path component"));
    }
    let replaced = regex::Regex::new("[^a-zA-Z0-9._-]+")
        .expect("valid regex")
        .replace_all(trimmed, "_")
        .chars()
        .take(120)
        .collect::<String>();
    if replaced.is_empty() || replaced == "." || replaced == ".." {
        return Err(failure("Invalid attachment path component"));
    }
    Ok(replaced)
}

fn reject_symlink_component(path: &Path) -> Result<()> {
    if path.exists() {
        let meta = std::fs::symlink_metadata(path).map_err(|error| failure(&error.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(failure("Attachment path escapes storage root"));
        }
    }
    Ok(())
}

fn ensure_dir_component(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    reject_symlink_component(&path)?;
    if !path.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|error| failure(&error.to_string()))?;
    }
    Ok(path)
}

fn attachment_store_dir(home: &Path, chat: &str, message_id: &str) -> Result<PathBuf> {
    let chat = path_part(chat)?;
    let message_id = path_part(message_id)?;
    let root = home.join("im-attachments").join("weixin");
    std::fs::create_dir_all(&root).map_err(|error| failure(&error.to_string()))?;
    let root = std::fs::canonicalize(&root).map_err(|error| failure(&error.to_string()))?;
    let chat_dir = ensure_dir_component(&root, &chat)?;
    let dir = ensure_dir_component(&chat_dir, &message_id)?;
    let canonical = std::fs::canonicalize(&dir).map_err(|error| failure(&error.to_string()))?;
    if !canonical.starts_with(&root) {
        return Err(failure("Attachment path escapes storage root"));
    }
    Ok(canonical)
}

fn write_attachment_bytes(store_dir: &Path, filename: &str, data: &[u8]) -> Result<PathBuf> {
    if store_dir
        .symlink_metadata()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(failure("Attachment path escapes storage root"));
    }
    let path = store_dir.join(filename);
    if path
        .symlink_metadata()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(failure("Attachment path escapes storage root"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(failure("Attachment path escapes storage root"));
        }
        Err(error) => return Err(failure(&error.to_string())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|error| failure(&error.to_string()))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(failure("Attachment path escapes storage root"));
        }
    }
    file.set_len(0)
        .map_err(|error| failure(&error.to_string()))?;
    use std::io::Write;
    file.write_all(data)
        .map_err(|error| failure(&error.to_string()))?;
    let path = std::fs::canonicalize(path).map_err(|error| failure(&error.to_string()))?;
    if !path.starts_with(store_dir) {
        return Err(failure("Attachment path escapes storage root"));
    }
    Ok(path)
}

pub(crate) fn format_attachment_fallback(attachment: &Value) -> String {
    let name = attachment
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            attachment
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| path.rsplit(['/', '\\']).next())
        })
        .unwrap_or("附件");
    let path_text = attachment
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(|path| format!("，可在本机查看：{path}"))
        .unwrap_or_default();
    format!("附件发送失败：{name}{path_text}")
}

pub(crate) async fn materialize_inbound(
    home: &Path,
    chat: &str,
    message_id: &str,
    attachments: &mut Vec<Value>,
) -> Result<()> {
    let store_dir = attachment_store_dir(home, chat, message_id)?;
    let mut index = 0usize;
    while index < attachments.len() {
        let Some(url) = attachments[index]
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
        else {
            index += 1;
            continue;
        };
        let kind = attachments[index]
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_owned();
        let image = kind == "image";
        let default_name = if image {
            format!("image-{message_id}.jpg")
        } else {
            format!("attachment-{message_id}.bin")
        };
        let name = attachments[index]
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or(default_name);
        let filename = sanitize_filename(&name);
        let media_owner = attachments[index]
            .get("metadata")
            .and_then(|metadata| metadata.get("weixinMediaOwner"))
            .cloned();
        let materialize_result = match download_bytes(&url).await {
            Err(error) => Err(error),
            Ok(mut data) => {
                let owner_ref = media_owner.as_ref();
                match decrypt_weixin_media_if_needed(&data, owner_ref, image) {
                    Err(message) => Err(failure(&message)),
                    Ok(decrypted) => {
                        data = decrypted;
                        write_attachment_bytes(&store_dir, &filename, &data)
                            .map(|path| (path, data.len()))
                    }
                }
            }
        };
        match materialize_result {
            Ok((path, bytes)) => {
                attachments[index]["path"] = path.to_string_lossy().into();
                attachments[index]["bytes"] = bytes.into();
                let mut metadata = attachments[index]
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if let Some(object) = metadata.as_object_mut() {
                    object.insert("channelKey".into(), "weixin".into());
                    object.insert("chatId".into(), chat.into());
                    object.insert("messageId".into(), message_id.into());
                    object.insert("sourceUrl".into(), url.into());
                }
                attachments[index]["metadata"] = metadata;
                index += 1;
            }
            Err(error) => {
                eprintln!("weixin: attachment materialize failed: {}", error.message);
                attachments.remove(index);
            }
        }
    }
    Ok(())
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    attachment_net::pinned_get(url, LIMIT).await
}

pub(crate) async fn send_outbound(
    http: &Http,
    credentials: &Value,
    chat: &str,
    context: &str,
    attachment: &Value,
) -> Result<bool> {
    let path = attachment
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty());
    let data = if let Some(path) = path {
        std::fs::read(path).map_err(|error| failure(&error.to_string()))?
    } else if let Some(content) = attachment.get("content").and_then(Value::as_str) {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|_| failure("Invalid attachment content"))?
    } else {
        return Ok(false);
    };
    if data.len() > LIMIT {
        return Err(failure("attachment exceeds 52428800 bytes"));
    }
    let kind = attachment
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("file");
    let image = kind == "image";
    let name = attachment
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| path.and_then(|p| p.rsplit(['/', '\\']).next()))
        .unwrap_or("attachment.bin");
    let base = text(credentials, "baseUrl").ok_or_else(|| failure("Weixin baseUrl missing"))?;
    let token = text(credentials, "botToken");
    let filekey = uuid::Uuid::new_v4().simple().to_string();
    let aeskey: [u8; 16] = uuid::Uuid::new_v4().into_bytes();
    let upload = http
        .json(
            &base,
            "ilink/bot/getuploadurl",
            token.as_deref(),
            Some(json!({
                "filekey": filekey,
                "media_type": if image { 1 } else { 3 },
                "to_user_id": chat,
                "rawsize": data.len(),
                "rawfilemd5": format!("{:x}", Md5::digest(&data)),
                "filesize": aes_ecb_padded_size(data.len()),
                "no_need_thumb": true,
                "aeskey": aeskey.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            })),
        )
        .await?;
    if upload.get("ret").and_then(Value::as_i64).is_some_and(|ret| ret != 0)
        || upload
            .get("errcode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
    {
        return Err(failure("Weixin getuploadurl rejected"));
    }
    let Some(upload_url) = build_weixin_cdn_upload_url(&upload, &filekey) else {
        return Err(failure("Weixin upload url response missing upload target"));
    };
    let download_param = upload_encrypted_cdn(&upload_url, &data, &aeskey).await?;
    use base64::Engine;
    let media = json!({
        "encrypt_query_param": download_param,
        "aes_key": weixin_outbound_aes_key_field(&aeskey),
        "encrypt_type": 1,
    });
    let item = if image {
        json!({
            "type": 2,
            "image_item": {
                "media": media,
                "mid_size": aes_ecb_padded_size(data.len()),
            }
        })
    } else {
        json!({
            "type": 3,
            "file_item": {
                "file_name": name,
                "len": data.len().to_string(),
                "media": media,
            }
        })
    };
    let response = http
        .json(
            &base,
            "ilink/bot/sendmessage",
            token.as_deref(),
            Some(json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": chat,
                    "client_id": uuid::Uuid::new_v4().to_string(),
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": context,
                    "item_list": [item],
                }
            })),
        )
        .await?;
    if response.get("ret").and_then(Value::as_i64).is_some_and(|ret| ret != 0)
        || response
            .get("errcode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
    {
        return Err(failure("Weixin sendmessage rejected attachment"));
    }
    Ok(true)
}

async fn upload_encrypted_cdn(upload_url: &str, data: &[u8], aeskey: &[u8; 16]) -> Result<String> {
    let ciphertext = encrypt_aes_128_ecb(data, aeskey);
    let response = attachment_net::pinned_post(
        upload_url,
        Bytes::copy_from_slice(&ciphertext),
        ciphertext.len().saturating_add(4096),
    )
    .await?;
    if !response.status.is_success() {
        return Err(failure("Attachment upload HTTP error"));
    }
    response
        .headers
        .get("x-encrypted-param")
        .and_then(|value| value.to_str().ok())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| failure("Weixin CDN upload missing x-encrypted-param"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

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
        assert!(attachment_store_dir(&home, "../etc", "m1").is_err());
        assert!(attachment_store_dir(&home, "user", "../../outside").is_err());
        assert!(attachment_store_dir(&home, "/tmp/abs", "m1").is_err());
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
        materialize_inbound(&home, "user-1", "m1", &mut attachments)
            .await
            .unwrap();
        let store = attachment_store_dir(&home, "user-1", "m1").unwrap();
        assert!(!store.join("bad.jpg").exists());
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0]["path"].as_str().is_some());
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
        assert_eq!(posted[1], payload.as_ref());
        set_test_permit_loopback_fetch(false);
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
}
