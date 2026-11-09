//! Inbound download and outbound upload for Weixin attachments (WeixinChannel.ts).
use super::http::{Http, failure};
use super::storage::text;
use super::media::{
    aes_ecb_padded_size, build_weixin_cdn_upload_url, decrypt_weixin_media_if_needed,
    encrypt_aes_128_ecb,
};
use crate::config::routes::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

const LIMIT: usize = 50 * 1024 * 1024;

#[cfg(test)]
static TEST_PERMIT_LOOPBACK_FETCH: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_test_permit_loopback_fetch(permit: bool) {
    TEST_PERMIT_LOOPBACK_FETCH.store(permit, Ordering::SeqCst);
}

fn permit_loopback_fetch() -> bool {
    #[cfg(test)]
    {
        return TEST_PERMIT_LOOPBACK_FETCH.load(Ordering::SeqCst);
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn download_client() -> Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Full<Bytes>,
> {
    let connector = crate::runtime::model::http::https_connector(
        crate::runtime::model::http::webpki_client_config(),
    );
    Client::builder(TokioExecutor::new()).build(connector)
}

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

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
        }
    }
}

fn host_is_local_name(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "localhost.localdomain"
    )
}

pub(crate) async fn validate_attachment_fetch_url(url: &str) -> Result<Url> {
    let parsed = Url::parse(url.trim()).map_err(|_| failure("Invalid attachment URL"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(failure("Attachment URL must be http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(failure("Attachment URL must not contain credentials"));
    }
    let Some(host) = parsed.host_str() else {
        return Err(failure("Attachment URL must include a host"));
    };
    if host_is_local_name(host) && !permit_loopback_fetch() {
        return Err(failure("Attachment URL host is not permitted"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) && !permit_loopback_fetch() {
            return Err(failure("Attachment URL host is not permitted"));
        }
        return Ok(parsed);
    }
    let mut resolved = tokio::net::lookup_host(format!("{host}:0"))
        .await
        .map_err(|_| failure("Attachment URL host resolution failed"))?
        .map(|socket| socket.ip())
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return Err(failure("Attachment URL host resolution failed"));
    }
    resolved.sort();
    resolved.dedup();
    if !permit_loopback_fetch() && resolved.iter().any(|ip| is_blocked_ip(*ip)) {
        return Err(failure("Attachment URL host is not permitted"));
    }
    Ok(parsed)
}

fn attachment_store_dir(home: &Path, chat: &str, message_id: &str) -> Result<PathBuf> {
    let chat = path_part(chat)?;
    let message_id = path_part(message_id)?;
    let root = home.join("im-attachments").join("weixin");
    std::fs::create_dir_all(&root).map_err(|error| failure(&error.to_string()))?;
    let root = std::fs::canonicalize(&root).map_err(|error| failure(&error.to_string()))?;
    let dir = root.join(&chat).join(&message_id);
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|error| failure(&error.to_string()))?;
    }
    let canonical = std::fs::canonicalize(&dir).map_err(|error| failure(&error.to_string()))?;
    if !canonical.starts_with(&root) {
        return Err(failure("Attachment path escapes storage root"));
    }
    Ok(canonical)
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
    attachments: &mut [Value],
) -> Result<()> {
    let store_dir = attachment_store_dir(home, chat, message_id)?;
    for attachment in attachments.iter_mut() {
        let Some(url) = attachment
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
        else {
            continue;
        };
        let kind = attachment
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
        let name = attachment
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or(default_name);
        let filename = sanitize_filename(&name);
        validate_attachment_fetch_url(&url).await?;
        let mut data = download_bytes(&url).await?;
        let media_owner = attachment
            .get("metadata")
            .and_then(|metadata| metadata.get("weixinMediaOwner"));
        data = decrypt_weixin_media_if_needed(&data, media_owner, image);
        let path = store_dir.join(&filename);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file.mode(0o600);
        }
        let mut file = file
            .open(&path)
            .map_err(|error| failure(&error.to_string()))?;
        file.write_all(&data)
            .map_err(|error| failure(&error.to_string()))?;
        let path = std::fs::canonicalize(path).map_err(|error| failure(&error.to_string()))?;
        if !path.starts_with(&store_dir) {
            return Err(failure("Attachment path escapes storage root"));
        }
        attachment["path"] = path.to_string_lossy().into();
        attachment["bytes"] = data.len().into();
        let mut metadata = attachment
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(object) = metadata.as_object_mut() {
            object.insert("channelKey".into(), "weixin".into());
            object.insert("chatId".into(), chat.into());
            object.insert("messageId".into(), message_id.into());
            object.insert("sourceUrl".into(), url.into());
        }
        attachment["metadata"] = metadata;
    }
    Ok(())
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let parsed = validate_attachment_fetch_url(url).await?;
    let client = download_client();
    let response = client
        .get(
            parsed
                .to_string()
                .parse::<hyper::Uri>()
                .map_err(|_| failure("Invalid attachment URL"))?,
        )
        .await
        .map_err(|_| failure("Attachment download transport failed"))?;
    if !response.status().is_success() {
        return Err(failure("Attachment download HTTP error"));
    }
    let mut data = Vec::new();
    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| failure("Attachment download body failed"))?;
        if let Ok(bytes) = frame.into_data() {
            if data.len() + bytes.len() > LIMIT {
                return Err(failure(&format!(
                    "Attachment is {} bytes (limit {LIMIT}).",
                    data.len() + bytes.len()
                )));
            }
            data.extend_from_slice(&bytes);
        }
    }
    Ok(data)
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
        "aes_key": base64::engine::general_purpose::STANDARD.encode(aeskey),
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
    validate_attachment_fetch_url(upload_url).await?;
    let ciphertext = encrypt_aes_128_ecb(data, aeskey);
    let parsed = Url::parse(upload_url).map_err(|_| failure("Invalid upload URL"))?;
    let client = download_client();
    let request = hyper::Request::builder()
        .method("POST")
        .uri(
            parsed
                .to_string()
                .parse::<hyper::Uri>()
                .map_err(|_| failure("Invalid upload URL"))?,
        )
        .header("Content-Type", "application/octet-stream")
        .body(Full::new(Bytes::copy_from_slice(&ciphertext)))
        .map_err(|_| failure("Invalid upload request"))?;
    let response = client
        .request(request)
        .await
        .map_err(|_| failure("Attachment upload transport failed"))?;
    if !response.status().is_success() {
        return Err(failure("Attachment upload HTTP error"));
    }
    let param = response
        .headers()
        .get("x-encrypted-param")
        .and_then(|value| value.to_str().ok())
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    param.ok_or_else(|| failure("Weixin CDN upload missing x-encrypted-param"))
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
        for url in [
            "http://127.0.0.1/evil",
            "http://10.0.0.1/evil",
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
    async fn inbound_download_writes_local_path() {
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
        set_test_permit_loopback_fetch(false);
        let _ = std::fs::remove_dir_all(home);
    }
}
