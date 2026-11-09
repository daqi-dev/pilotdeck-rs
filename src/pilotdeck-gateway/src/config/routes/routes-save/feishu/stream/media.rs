//! FeishuChannel.ts:697-810 and ImAttachment{Store,Delivery}.ts.
use super::*;
#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Method,
    header::{AUTHORIZATION, HeaderValue},
};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
const LIMIT: usize = 50 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct MediaInput {
    pub chat: String,
    pub message_id: Option<String>,
    pub event_id: String,
    pub kind: String,
    pub content: Value,
}
impl MediaInput {
    pub fn from_raw(raw: &Value, stream: bool) -> Option<Self> {
        if raw["header"]["event_type"]
            .as_str()
            .is_some_and(|s| s != "im.message.receive_v1")
        {
            return None;
        }
        let event = raw.get("event").unwrap_or(raw);
        let message = event.get("message")?;
        let kind = message["message_type"]
            .as_str()
            .filter(|s| matches!(*s, "image" | "file"))?;
        let chat = message["chat_id"]
            .as_str()
            .filter(|s| !s.is_empty())?
            .to_owned();
        let message_id = message["message_id"].as_str().map(str::to_owned);
        let event_id = if stream {
            message_id
                .clone()
                .unwrap_or_else(|| format!("stream:{chat}:{}", now_ms()))
        } else {
            raw["header"]["event_id"].as_str()?.into()
        };
        let content = message["content"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(json!({}));
        Some(Self {
            chat,
            message_id,
            event_id,
            kind: kind.into(),
            content,
        })
    }
}
fn client() -> Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Full<Bytes>,
> {
    let connector = crate::runtime::model::http::https_connector(
        crate::runtime::model::http::webpki_client_config(),
    );
    Client::builder(TokioExecutor::new()).build(connector)
}
pub(super) async fn accept_batch(owner: Arc<FeishuChannel>, inputs: Vec<MediaInput>) {
    let Some(input) = inputs.first().cloned() else {
        return;
    };
    let mut tasks = JoinSet::new();
    for (index, input) in inputs.into_iter().enumerate() {
        let owner = owner.clone();
        tasks.spawn(async move {
            (
                index,
                input.message_id.is_none(),
                download(&owner, &input).await,
            )
        });
    }
    let mut results = Vec::new();
    while let Some(Ok(result)) = tasks.join_next().await {
        results.push(result);
    }
    results.sort_by_key(|(index, _, _)| *index);
    let mut attachments = Vec::new();
    let mut diagnostics = Vec::new();
    for (_, missing, result) in results {
        match result {
            Ok(attachment) => attachments.push(attachment),
            Err(error) => diagnostics.push(if missing {
                "[Attachment diagnostics] 飞书附件缺少 message_id，无法下载。".into()
            } else {
                format!(
                    "[Attachment diagnostics] 飞书附件下载失败：Error: {}",
                    error.message
                )
            }),
        }
    }
    let text = if diagnostics.is_empty() {
        "请查看我发送的附件。".into()
    } else {
        diagnostics.join("\n\n")
    };
    let chat = input.chat;
    let _ = owner
        .incoming_attachments(
            Inbound {
                chat,
                text,
                event_id: input.event_id,
                message_id: input.message_id,
            },
            attachments,
        )
        .await;
}
async fn download(owner: &FeishuChannel, input: &MediaInput) -> Result<Value> {
    let message = input
        .message_id
        .as_deref()
        .ok_or_else(|| failure("message_id missing"))?;
    let image = input.kind == "image";
    let key_name = if image { "image_key" } else { "file_key" };
    let key = input.content[key_name]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| failure(&format!("{key_name} missing")))?;
    let name = if image {
        key
    } else {
        input.content["file_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                input.content["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or(key)
    };
    let declared = input.content["size"]
        .as_f64()
        .or_else(|| input.content["size"].as_str().and_then(|n| n.parse().ok()));
    if declared.is_some_and(|n| n > LIMIT as f64) {
        return Err(failure(&format!(
            "Attachment {name} is {} bytes (limit {LIMIT}).",
            declared.unwrap()
        )));
    }
    let token = owner.token().await.map_err(|error| {
        failure(&format!(
            "tenant token unavailable: Error: {}",
            error.message
        ))
    })?;
    let url = format!(
        "{}/im/v1/messages/{}/resources/{}?type={}",
        owner.options.api_base,
        http::encode(message),
        http::encode(key),
        input.kind
    );
    // `fetch` follows 301/302/303/307/308 redirects by default.  Keep the
    // same policy for the attachment store, including stripping credentials
    // when a redirect changes origin.
    let parsed = url::Url::parse(&url).map_err(|_| failure("Invalid Feishu media URL"))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| failure("Invalid Feishu authorization"))?,
    );
    let http_client = client();
    let mut response = tokio::time::timeout(
        Duration::from_secs(60),
        crate::fetch_redirect::follow(
            parsed,
            Method::GET,
            headers,
            Bytes::new(),
            || failure("fetch failed"),
            |request| async {
                http_client
                    .request(request)
                    .await
                    .map_err(|_| failure("fetch failed"))
            },
        ),
    )
    .await
    .map_err(|_| failure("This operation was aborted"))??;
    if !response.status().is_success() {
        return Err(failure(&format!(
            "Attachment download failed HTTP {}.",
            response.status().as_u16()
        )));
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|n| n.to_str().ok())
        .and_then(|n| n.parse::<usize>().ok())
        .is_some_and(|n| n > LIMIT)
    {
        return Err(failure(&format!(
            "Attachment is {} bytes (limit {LIMIT}).",
            response
                .headers()
                .get("content-length")
                .and_then(|n| n.to_str().ok())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(LIMIT + 1)
        )));
    }
    let mut mime = if image {
        None
    } else {
        guess(name).map(str::to_owned)
    }
    .or_else(|| {
        response
            .headers()
            .get("content-type")
            .and_then(|m| m.to_str().ok())
            .map(|m| m.split(';').next().unwrap().trim().to_owned())
            .filter(|m| !m.is_empty())
    });
    let mut data = Vec::new();
    while let Some(frame) = response.body_mut().frame().await {
        let frame = frame.map_err(|_| failure("Attachment response read failed"))?;
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
    if image {
        mime = detect(&data).map(str::to_owned).or(mime);
    }
    let extension = extension(mime.as_deref());
    let mut filename = sanitize(name.trim().rsplit('/').next().unwrap_or(name));
    if filename.is_empty() {
        filename = format!(
            "{}.{}",
            if image { "image" } else { "attachment" },
            extension
        );
    }
    if image {
        filename = format!("{}.{}", stem(&filename), extension);
    } else if std::path::Path::new(&filename).extension().is_none() {
        filename.push('.');
        filename.push_str(extension);
    }
    // Explicit pilot_home keeps this isolated from the user's running profile.
    let root = if owner.options.pilot_home.as_os_str().is_empty() {
        std::env::temp_dir().join("pilotdeck-im-attachments")
    } else {
        owner.options.pilot_home.join("im-attachments")
    };
    let dir = root
        .join("feishu")
        .join(part(&input.chat))
        .join(part(message));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| failure(&e.to_string()))?;
    let path = dir.join(&filename);
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| failure(&e.to_string()))?;
    file.write_all(&data).map_err(|e| failure(&e.to_string()))?;
    let path = std::fs::canonicalize(path).map_err(|e| failure(&e.to_string()))?;
    let mut metadata =
        json!({"channelKey":"feishu","chatId":input.chat,"messageId":message,"sourceUrl":url});
    metadata[key_name] = key.into();
    let mut attachment = json!({"type":input.kind,"name":filename,"path":path,"bytes":data.len(),"metadata":metadata});
    if let Some(mime) = mime {
        attachment["mimeType"] = mime.into();
    }
    Ok(attachment)
}

pub(super) async fn send(owner: &FeishuChannel, chat: &str, attachment: &Value) -> bool {
    let path = attachment["path"].as_str();
    if attachment["source"] == "local_path" && path.is_some_and(|p| !p.is_empty()) {
        owner
            .send_reply(
                chat,
                &format!("附件 {} 需要授权后才能发送。", path.unwrap()),
            )
            .await;
        return false;
    }
    if send_inner(owner, chat, attachment).await.is_ok() {
        return true;
    }
    let name = attachment["name"]
        .as_str()
        .or_else(|| path.and_then(|p| p.rsplit(['\\', '/']).next()))
        .unwrap_or("附件");
    let suffix = path
        .map(|path| format!("，可在本机查看：{path}"))
        .unwrap_or_default();
    owner
        .send_reply(chat, &format!("附件发送失败：{name}{suffix}"))
        .await;
    false
}
async fn send_inner(owner: &FeishuChannel, chat: &str, attachment: &Value) -> Result<()> {
    let path = attachment["path"].as_str();
    let raw_name = attachment["name"]
        .as_str()
        .or_else(|| path.and_then(|p| p.rsplit(['\\', '/']).next()))
        .unwrap_or("attachment");
    let name = sanitize(raw_name);
    let name: String = if name.trim().is_empty() {
        "attachment.bin".into()
    } else {
        name.trim().into()
    };
    let data = if let Some(content) = attachment["content"].as_str().filter(|s| !s.is_empty()) {
        decode_base64(content)
    } else if let Some(path) = path {
        std::fs::read(path).map_err(|e| failure(&e.to_string()))?
    } else {
        return Err(failure("attachment has neither content nor path"));
    };
    if data.len() > LIMIT {
        return Err(failure("attachment exceeds 52428800 bytes"));
    }
    let mime = attachment["mimeType"]
        .as_str()
        .or_else(|| guess(&name))
        .unwrap_or("application/octet-stream");
    let image = attachment["type"] == "image" || mime.starts_with("image/");
    let kind = if image { "image" } else { "file" };
    let token = owner.token().await?;
    let boundary = format!("----pilotdeck{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    let fields = if image {
        vec![("image_type", "message")]
    } else {
        vec![
            ("file_type", file_type(&name, mime)),
            ("file_name", name.as_str()),
        ]
    };
    for (key, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{kind}\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = hyper::Request::builder()
        .method("POST")
        .uri(format!("{}/im/v1/{}s", owner.options.api_base, kind))
        .header("authorization", format!("Bearer {token}"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| failure("Invalid Feishu attachment upload"))?;
    let response = client()
        .request(request)
        .await
        .map_err(|_| failure("Feishu attachment upload failed"))?;
    let ok = response.status().is_success();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|_| failure("Feishu upload response failed"))?
        .to_bytes();
    let raw: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    if raw["code"] == 99991663 || raw["code"] == 99991664 {
        owner.token.lock().await.value = None;
    }
    let key_name = if image { "image_key" } else { "file_key" };
    let key = raw["data"][key_name].as_str().filter(|s| !s.is_empty());
    if !ok || raw["code"] != 0 || key.is_none() {
        return Err(failure("Feishu attachment upload rejected"));
    }
    let mut content = json!({});
    content[key_name] = key.unwrap().into();
    owner
        .rest(
            "POST",
            "/im/v1/messages?receive_id_type=chat_id",
            Some(json!({"receive_id":chat,"msg_type":kind,"content":content.to_string()})),
        )
        .await?;
    Ok(())
}
fn decode_base64(value: &str) -> Vec<u8> {
    let mut bytes = value
        .bytes()
        .filter_map(|b| match b {
            b'-' => Some(b'+'),
            b'_' => Some(b'/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => Some(b),
            _ => None,
        })
        .collect::<Vec<_>>();
    if bytes.len() % 4 == 1 {
        bytes.pop();
    }
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(bytes)
        .unwrap_or_default()
}
fn sanitize(name: &str) -> String {
    let regex = regex::Regex::new("[\\x00-\\x1f\\\\/:*?\"<>|]+").unwrap();
    String::from_utf16_lossy(
        &regex
            .replace_all(name, "_")
            .encode_utf16()
            .take(180)
            .collect::<Vec<_>>(),
    )
}
fn part(value: &str) -> String {
    let s = regex::Regex::new("[^a-zA-Z0-9._-]+")
        .unwrap()
        .replace_all(value, "_")
        .chars()
        .take(120)
        .collect::<String>();
    if s.is_empty() {
        "unknown".into()
    } else if s == "." || s == ".." {
        "_".into()
    } else {
        s
    }
}
fn stem(name: &str) -> &str {
    std::path::Path::new(name)
        .file_stem()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("image")
}
fn extension(mime: Option<&str>) -> &'static str {
    match mime.map(str::to_ascii_lowercase).as_deref() {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        Some("application/pdf") => "pdf",
        Some("text/plain") => "txt",
        _ => "bin",
    }
}
fn detect(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}
fn guess(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    match name.rsplit('.').next() {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("pdf") => Some("application/pdf"),
        Some("txt" | "log") => Some("text/plain"),
        Some("md") => Some("text/markdown"),
        Some("json") => Some("application/json"),
        _ => None,
    }
}
fn file_type(name: &str, mime: &str) -> &'static str {
    if mime == "application/pdf" {
        return "pdf";
    }
    let name = name.to_ascii_lowercase();
    match name.rsplit('.').next() {
        Some("pdf") => "pdf",
        Some("doc" | "docx") => "doc",
        Some("xls" | "xlsx") => "xls",
        Some("ppt" | "pptx") => "ppt",
        _ => "stream",
    }
}
