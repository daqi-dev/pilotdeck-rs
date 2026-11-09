//! Inbound download and outbound upload for Weixin attachments (WeixinChannel.ts).
use super::http::{Http, failure};
use super::storage::text;
use crate::config::routes::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use md5::{Digest, Md5};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const LIMIT: usize = 50 * 1024 * 1024;

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

pub(crate) async fn materialize_inbound(
    home: &Path,
    chat: &str,
    message_id: &str,
    attachments: &mut [Value],
) -> Result<()> {
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
        let default_name = if kind == "image" {
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
        let data = download_bytes(&url).await?;
        let root = home.join("im-attachments").join("weixin").join(chat).join(message_id);
        {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&root)
                .map_err(|error| failure(&error.to_string()))?;
        }
        let path = root.join(&filename);
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
    let url = url
        .parse::<hyper::Uri>()
        .map_err(|_| failure("Invalid attachment URL"))?;
    let client = download_client();
    let response = client
        .get(url)
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
            })),
        )
        .await?;
    if upload.get("ret").and_then(Value::as_i64).is_some_and(|ret| ret != 0)
        || upload
            .get("errcode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
    {
        return Ok(false);
    }
    let upload_url = upload
        .get("upload_url")
        .or_else(|| upload.get("data").and_then(|data| data.get("upload_url")))
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty());
    if let Some(upload_url) = upload_url {
        put_bytes(upload_url, &data).await?;
    }
    let media = upload
        .get("media")
        .cloned()
        .or_else(|| upload.get("data").and_then(|data| data.get("media")).cloned())
        .unwrap_or_else(|| json!({ "filekey": filekey }));
    let item = if image {
        json!({
            "type": 2,
            "image_item": {
                "media": media,
                "file_name": name,
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
    Ok(!(response.get("ret").and_then(Value::as_i64).is_some_and(|ret| ret != 0)
        || response
            .get("errcode")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)))
}

async fn put_bytes(url: &str, data: &[u8]) -> Result<()> {
    let url = url
        .parse::<hyper::Uri>()
        .map_err(|_| failure("Invalid upload URL"))?;
    let client = download_client();
    let request = hyper::Request::builder()
        .method("PUT")
        .uri(url)
        .header("Content-Type", "application/octet-stream")
        .body(Full::new(Bytes::copy_from_slice(data)))
        .map_err(|_| failure("Invalid upload request"))?;
    let response = client
        .request(request)
        .await
        .map_err(|_| failure("Attachment upload transport failed"))?;
    if !response.status().is_success() {
        return Err(failure("Attachment upload HTTP error"));
    }
    Ok(())
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let download_url = format!("{}download.bin", base);
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
                    let reply = json!({"ret":0,"upload_url":format!("{upload_base}upload"),"media":{"filekey":"k"}});
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
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
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
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn outbound_send_uploads_from_path() {
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
        let _ = std::fs::remove_dir_all(home);
    }
}
