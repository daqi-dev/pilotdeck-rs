//! TS WeComChannel.ts:772-909,1010-1258,1348-1714.
use super::*;
use aes::{
    Aes256,
    cipher::{BlockDecrypt, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use md5::{Digest, Md5};
use std::{
    io::Write,
    path::{Component, Path},
};
const MAX: usize = 20 * 1024 * 1024;
pub(super) struct Http {
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
}
impl Http {
    pub fn new() -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
        }
    }
    pub async fn download(&self, source: &str) -> Result<(Vec<u8>, String, Option<String>)> {
        let mut endpoint = url::Url::parse(source).map_err(|_| failure("Invalid media URL"))?;
        tokio::time::timeout(Duration::from_secs(30), async {
            for _ in 0..20 {
                if !matches!(endpoint.scheme(), "http" | "https") {
                    return Err(failure("unsupported media URL protocol"));
                }
                let req = hyper::Request::builder()
                    .uri(endpoint.as_str())
                    .header("user-agent", "PilotDeck/1.0")
                    .header("accept", "*/*")
                    .body(Full::new(Bytes::new()))
                    .map_err(|_| failure("Invalid media request"))?;
                let mut response = self
                    .client
                    .request(req)
                    .await
                    .map_err(|_| failure("Media download connection failed"))?;
                if response.status().is_redirection() {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| failure("Media redirect missing location"))?;
                    endpoint = endpoint
                        .join(location)
                        .map_err(|_| failure("Invalid media redirect"))?;
                    continue;
                }
                if !response.status().is_success() {
                    return Err(failure("Media download HTTP error"));
                }
                if response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<usize>().ok())
                    .is_some_and(|n| n > MAX)
                {
                    return Err(failure("Remote media exceeds 20MB limit"));
                }
                let mime = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let disposition = response
                    .headers()
                    .get("content-disposition")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let mut bytes = vec![];
                while let Some(frame) = response.body_mut().frame().await {
                    let frame = frame.map_err(|_| failure("Media download body failed"))?;
                    if let Some(data) = frame.data_ref() {
                        if bytes.len().saturating_add(data.len()) > MAX {
                            return Err(failure("Remote media exceeds 20MB limit"));
                        }
                        bytes.extend_from_slice(data);
                    }
                }
                return Ok((bytes, mime, disposition));
            }
            Err(failure("Too many media redirects"))
        })
        .await
        .map_err(|_| failure("Media download timed out"))?
    }
}
struct Prepared {
    data: Vec<u8>,
    kind: &'static str,
    name: String,
    reject: Option<String>,
    note: Option<String>,
}
impl WeComChannel {
    pub(super) async fn parts(&self, body: &Value) -> Result<(String, Vec<Value>)> {
        let mut text = vec![];
        let mut refs = vec![];
        let kind = js(&body["msgtype"]).to_lowercase();
        if kind == "mixed" {
            for item in body["mixed"]["msg_item"].as_array().into_iter().flatten() {
                match js(&item["msgtype"]).to_lowercase().as_str() {
                    "text" => push_text(&mut text, &item["text"]["content"]),
                    "image" => refs.push(("image", &item["image"])),
                    "file" => refs.push(("file", &item["file"])),
                    _ => {}
                }
            }
        } else {
            push_text(&mut text, &body["text"]["content"]);
            if kind == "voice" {
                push_text(&mut text, &body["voice"]["content"]);
            }
            if kind == "appmsg" {
                push_text(&mut text, &body["appmsg"]["title"]);
            }
            refs.push(("image", &body["image"]));
            match kind.as_str() {
                "file" => refs.push(("file", &body["file"])),
                "voice" => refs.push(("voice", &body["voice"])),
                "video" => refs.push(("video", &body["video"])),
                "appmsg" => {
                    refs.push(("file", &body["appmsg"]["file"]));
                    refs.push(("image", &body["appmsg"]["image"]));
                }
                _ => {}
            }
        }
        let quote = &body["quote"];
        match quote["msgtype"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .as_str()
        {
            "text" | "voice" if text.is_empty() => {
                let kind = quote["msgtype"].as_str().unwrap_or("").to_lowercase();
                push_text(&mut text, &quote[&kind]["content"]);
            }
            "image" => refs.push(("image", &quote["image"])),
            "file" => refs.push(("file", &quote["file"])),
            _ => {}
        }
        let mut attachments = vec![];
        for (kind, media) in refs {
            if !media.is_object() {
                continue;
            }
            if let Ok(Some(value)) = self.cache_media(kind, media).await {
                attachments.push(value);
            }
        }
        Ok((text.join("\n").trim().into(), attachments))
    }
    async fn cache_media(&self, kind: &str, media: &Value) -> Result<Option<Value>> {
        let raw = js(&media["base64"]);
        let mut data = if !raw.is_empty() {
            decode(&raw)?
        } else {
            let endpoint = js(&media["url"]);
            if endpoint.trim().is_empty() {
                return Ok(None);
            }
            self.media.download(endpoint.trim()).await?.0
        };
        let key = js(&media["aeskey"]);
        if !key.trim().is_empty() {
            data = decrypt(&data, key.trim())?;
        }
        let raw = media
            .get("filename")
            .filter(|v| !v.is_null())
            .or_else(|| media.get("name").filter(|v| !v.is_null()))
            .map(js)
            .unwrap_or_else(|| format!("wecom_{kind}{}", default_ext(kind)));
        let name = safe_name(if raw.trim().is_empty() {
            "wecom_media"
        } else {
            raw.trim()
        });
        let dir = std::env::temp_dir().join("pilotdeck-wecom-media");
        std::fs::create_dir_all(&dir).map_err(|_| failure("Cannot create WeCom media cache"))?;
        let path = dir.join(format!(
            "{}-{}-{name}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|_| failure("Cannot create WeCom attachment"))?;
        file.write_all(&data)
            .map_err(|_| failure("Cannot cache WeCom attachment"))?;
        let mime = normalize_mime(
            &media
                .get("content_type")
                .filter(|v| !v.is_null())
                .or_else(|| media.get("contentType"))
                .map(js)
                .unwrap_or_default(),
            &name,
        );
        Ok(Some(
            json!({"type":if kind=="image"{"image"}else{"file"},"name":name,"path":path,"mimeType":mime,"bytes":data.len(),"metadata":{"channelKey":"wecom","source":"wecom","mediaType":kind}}),
        ))
    }
    pub async fn send_image(
        &self,
        chat: &str,
        source: &str,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> bool {
        self.send_source(
            &Target {
                chat: chat.into(),
                group: false,
                reply: reply_to.unwrap_or("").into(),
            },
            source,
            caption,
            None,
            false,
        )
        .await
    }
    pub async fn send_image_file(
        &self,
        chat: &str,
        source: &str,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> bool {
        self.send_image(chat, source, caption, reply_to).await
    }
    pub async fn send_document(
        &self,
        chat: &str,
        source: &str,
        caption: Option<&str>,
        name: Option<&str>,
        reply_to: Option<&str>,
    ) -> bool {
        self.send_source(
            &Target {
                chat: chat.into(),
                group: false,
                reply: reply_to.unwrap_or("").into(),
            },
            source,
            caption,
            name,
            true,
        )
        .await
    }
    pub async fn send_voice(
        &self,
        chat: &str,
        source: &str,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> bool {
        self.send_image(chat, source, caption, reply_to).await
    }
    pub async fn send_video(
        &self,
        chat: &str,
        source: &str,
        caption: Option<&str>,
        reply_to: Option<&str>,
    ) -> bool {
        self.send_image(chat, source, caption, reply_to).await
    }
    async fn load_source(&self, source: &str, name: Option<&str>) -> Result<Prepared> {
        let source = source.trim();
        if source.is_empty() {
            return Err(failure("media source is required"));
        }
        let (data, mime, guessed) = if let Ok(url) = url::Url::parse(source) {
            if matches!(url.scheme(), "http" | "https") {
                let (data, mime, disposition) = self.media.download(source).await?;
                (data, mime, guess_name(&url, disposition.as_deref()))
            } else if url.scheme() == "file" {
                let p = url
                    .to_file_path()
                    .map_err(|_| failure("Invalid media file URL"))?;
                (read_file(&p)?, String::new(), basename(&p))
            } else {
                let p = Path::new(source);
                (read_file(p)?, String::new(), basename(p))
            }
        } else {
            let p = Path::new(source);
            (read_file(p)?, String::new(), basename(p))
        };
        Ok(prepare(data, &mime, name.unwrap_or(&guessed)))
    }
    async fn send_source(
        &self,
        target: &Target,
        source: &str,
        caption: Option<&str>,
        name: Option<&str>,
        force_file: bool,
    ) -> bool {
        let Ok(mut media) = self.load_source(source, name).await else {
            return false;
        };
        if force_file {
            media.kind = "file";
        }
        let sent = self.send_prepared(target, media).await;
        if sent {
            if let Some(caption) = caption.filter(|s| !s.is_empty()) {
                self.text_target(target, caption).await;
            }
        }
        sent
    }
    async fn send_prepared(&self, target: &Target, media: Prepared) -> bool {
        if let Some(reason) = media.reject {
            self.text_target(target, &reason).await;
            return false;
        }
        let Ok(id) = self.upload(&media.data, media.kind, &media.name).await else {
            return false;
        };
        let mut body = json!({"msgtype":media.kind});
        body[media.kind] = json!({"media_id":id});
        let sent = self.send_target(target, body).await;
        if sent {
            if let Some(note) = media.note {
                self.text_target(target, &note).await;
            }
        }
        sent
    }
    async fn upload(&self, data: &[u8], kind: &str, name: &str) -> Result<String> {
        if data.is_empty() {
            return Err(failure("Cannot upload empty media"));
        }
        let chunks = data.len().div_ceil(512 * 1024);
        if chunks > 100 {
            return Err(failure("Media upload exceeds 100 chunks"));
        }
        let init=self.request("aibot_upload_media_init",json!({"type":kind,"filename":name,"total_size":data.len(),"total_chunks":chunks,"md5":format!("{:x}",Md5::digest(data))}),None).await?;
        let id = init["body"]
            .get("upload_id")
            .filter(|v| !v.is_null())
            .or_else(|| init["body"].get("uploadId"))
            .map(js)
            .unwrap_or_default();
        if id.trim().is_empty() {
            return Err(failure("media upload init failed: missing upload_id"));
        }
        for (i, chunk) in data.chunks(512 * 1024).enumerate() {
            self.request(
                "aibot_upload_media_chunk",
                json!({"upload_id":id.trim(),"chunk_index":i,"base64_data":STANDARD.encode(chunk)}),
                None,
            )
            .await?;
        }
        let finish = self
            .request(
                "aibot_upload_media_finish",
                json!({"upload_id":id.trim()}),
                None,
            )
            .await?;
        let id = finish["body"]
            .get("media_id")
            .or_else(|| finish["body"].get("mediaId"))
            .or_else(|| finish.get("media_id"))
            .or_else(|| finish.get("mediaId"))
            .map(js)
            .unwrap_or_default();
        if id.trim().is_empty() {
            Err(failure("media upload did not return media_id"))
        } else {
            Ok(id.trim().into())
        }
    }
    pub(super) async fn event_media(&self, target: &Target, event: &Value) {
        if event["type"] != "tool_call_finished" {
            return;
        }
        for (i, image) in event["images"].as_array().into_iter().flatten().enumerate() {
            let mime = js(&image["mimeType"]);
            let ext = ext_mime(&mime).unwrap_or(".png");
            let name = format!(
                "tool-{}-{}{ext}",
                event["toolCallId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("image"),
                i + 1
            );
            let sent = match decode(&js(&image["data"])) {
                Ok(data) => {
                    self.send_prepared(target, prepare(data, &mime, &name))
                        .await
                }
                Err(_) => false,
            };
            if !sent {
                self.text_target(target, "图片结果生成成功，但发送到企业微信失败。")
                    .await;
            }
        }
    }
    pub(super) async fn deliver_final(&self, target: &Target, text: &str) {
        let (candidates, warnings) = extract_paths(text, &self.options.pilot_home);
        let mut visible = vec![text.trim().to_owned()];
        visible.extend(warnings);
        self.text_target(target, &visible.join("\n")).await;
        for path in candidates {
            let kind = kind_ext(
                Path::new(&path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or(""),
            );
            self.send_source(target, &path, None, None, kind == "file")
                .await;
        }
    }
}
fn read_file(path: &Path) -> Result<Vec<u8>> {
    let stat = std::fs::metadata(path).map_err(|_| failure("Cannot stat media source"))?;
    if !stat.is_file() {
        return Err(failure("media source is not a file"));
    }
    std::fs::read(path).map_err(|_| failure("Cannot read media source"))
}
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wecom_media".into())
}
fn push_text(parts: &mut Vec<String>, value: &Value) {
    let t = js(value);
    if !t.trim().is_empty() {
        parts.push(t.trim().into());
    }
}
fn default_ext(kind: &str) -> &'static str {
    match kind {
        "image" => ".jpg",
        "video" => ".mp4",
        "voice" => ".amr",
        _ => ".bin",
    }
}
fn safe_name(raw: &str) -> String {
    let name = Path::new(raw)
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let re = regex::Regex::new(r"[^A-Za-z0-9_.\-()\u{4e00}-\u{9fff}]+").unwrap();
    let result = re.replace_all(&name, "_");
    if result.is_empty() {
        "wecom_media".into()
    } else {
        result.into_owned()
    }
}
fn normalize_mime(mime: &str, name: &str) -> String {
    let mime = mime.split(';').next().unwrap_or("").trim().to_lowercase();
    if !mime.is_empty() && !matches!(mime.as_str(), "application/octet-stream" | "text/plain") {
        return mime;
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let guessed = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "amr" => "audio/amr",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        _ => "",
    };
    if !guessed.is_empty() {
        guessed.into()
    } else if !mime.is_empty() {
        mime
    } else {
        "application/octet-stream".into()
    }
}
fn ext_mime(mime: &str) -> Option<&'static str> {
    match mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .as_str()
    {
        "image/png" => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "video/mp4" => Some(".mp4"),
        "audio/amr" => Some(".amr"),
        "application/pdf" => Some(".pdf"),
        _ => None,
    }
}
fn prepare(data: Vec<u8>, mime: &str, name: &str) -> Prepared {
    let mime = normalize_mime(mime, name);
    let mut kind = if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") || mime == "application/ogg" {
        "voice"
    } else {
        "file"
    };
    let mb = data.len() as f64 / (1024.0 * 1024.0);
    let mut reject = None;
    let mut note = None;
    if data.len() > MAX {
        reject = Some(format!(
            "文件大小 {mb:.2}MB 超过企业微信 20MB 限制，无法发送。"
        ));
    } else if kind == "image" && data.len() > 10 * 1024 * 1024 {
        kind = "file";
        note = Some(format!(
            "图片大小 {mb:.2}MB 超过 10MB 限制，已转为文件格式发送。"
        ));
    } else if kind == "video" && data.len() > 10 * 1024 * 1024 {
        kind = "file";
        note = Some(format!(
            "视频大小 {mb:.2}MB 超过 10MB 限制，已转为文件格式发送。"
        ));
    } else if kind == "voice" {
        if !mime.is_empty() && mime != "audio/amr" {
            kind = "file";
            note = Some(format!(
                "语音格式 {mime} 不支持，企业微信原生语音仅支持 AMR，已转为文件格式发送。"
            ));
        } else if data.len() > 2 * 1024 * 1024 {
            kind = "file";
            note = Some(format!(
                "语音大小 {mb:.2}MB 超过 2MB 限制，已转为文件格式发送。"
            ));
        }
    }
    Prepared {
        data,
        kind,
        name: safe_name(name),
        reject,
        note,
    }
}
fn decode(text: &str) -> Result<Vec<u8>> {
    let mut s = text
        .bytes()
        .take_while(|b| *b != b'=')
        .filter_map(|b| match b {
            b'-' => Some('+'),
            b'_' => Some('/'),
            b if b.is_ascii_alphanumeric() || b == b'+' || b == b'/' => Some(char::from(b)),
            _ => None,
        })
        .collect::<String>();
    if s.len() % 4 == 1 {
        s.pop();
    }
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(s)
        .map_err(|_| failure("Invalid media base64"))
}
fn decrypt(bytes: &[u8], key: &str) -> Result<Vec<u8>> {
    let key = decode(key)?;
    if key.len() != 32 || bytes.is_empty() || bytes.len() % 16 != 0 {
        return Err(failure("Invalid WeCom media AES key or ciphertext length"));
    }
    let cipher = Aes256::new_from_slice(&key).map_err(|_| failure("Invalid WeCom AES key"))?;
    let mut previous = [0u8; 16];
    previous.copy_from_slice(&key[..16]);
    let mut raw = vec![];
    for chunk in bytes.chunks_exact(16) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        raw.extend(block.iter().zip(previous).map(|(b, iv)| b ^ iv));
        previous.copy_from_slice(chunk);
    }
    let pad = usize::from(*raw.last().unwrap());
    if !(1..=32).contains(&pad)
        || pad > raw.len()
        || !raw[raw.len() - pad..]
            .iter()
            .all(|b| usize::from(*b) == pad)
    {
        return Err(failure("Invalid PKCS#7 padding bytes"));
    }
    raw.truncate(raw.len() - pad);
    Ok(raw)
}
fn guess_name(url: &url::Url, disposition: Option<&str>) -> String {
    let text = disposition.unwrap_or("");
    let utf8 = regex::Regex::new("(?i)filename\\*=UTF-8''([^;]+)").unwrap();
    if let Some(m) = utf8.captures(text) {
        if let Ok(u) = url::Url::parse(&format!("file:///{}", &m[1])) {
            if let Ok(p) = u.to_file_path() {
                return p.to_string_lossy().trim_start_matches('/').into();
            }
        }
    }
    let quoted = regex::Regex::new("(?i)filename=\"?([^\";]+)\"?").unwrap();
    if let Some(m) = quoted.captures(text) {
        return m[1].into();
    }
    basename(Path::new(url.path()))
}
const EXTS: &str = "png|jpg|jpeg|gif|webp|bmp|tiff|svg|mp4|mov|avi|mkv|webm|mp3|wav|ogg|opus|m4a|flac|amr|pdf|docx|doc|odt|rtf|txt|md|xlsx|xls|csv|tsv|json|xml|yaml|yml|pptx|ppt|odp|zip|tar|gz|tgz|bz2|7z|html|htm";
fn kind_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "svg" => "image",
        "mp4" | "mov" | "avi" | "mkv" | "webm" => "video",
        "mp3" | "wav" | "ogg" | "opus" | "m4a" | "flac" | "amr" => "voice",
        _ => "file",
    }
}
fn extract_paths(text: &str, pilot_home: &Path) -> (Vec<String>, Vec<String>) {
    let mut masked = text.as_bytes().to_vec();
    let mut ranges = vec![];
    for pattern in [r"(?s)```.*?```", r"(?m)^>.*$", r"`[^`\n]+`"] {
        for m in regex::Regex::new(pattern).unwrap().find_iter(text) {
            if pattern.starts_with('`')
                && text[..m.start()]
                    .trim_end()
                    .to_uppercase()
                    .ends_with("MEDIA:")
            {
                continue;
            }
            ranges.push((m.start(), m.end()));
        }
    }
    mask(&mut masked, &ranges);
    let protected = String::from_utf8_lossy(&masked);
    let tags = regex::Regex::new(
        r#"(?i)["'`]?MEDIA:\s*(?:`([^`\n]+)`|"([^"\n]+)"|'([^'\n]+)'|((?:~/|/)\S+))["'`]?"#,
    )
    .unwrap();
    let mut candidates = vec![];
    let mut tag_ranges = vec![];
    for c in tags.captures_iter(&protected) {
        let m = c.get(0).unwrap();
        if let Some(path) = (1..=4).find_map(|i| c.get(i)) {
            candidates.push((m.start(), path.as_str().to_owned()));
        }
        tag_ranges.push((m.start(), m.end()));
    }
    mask(&mut masked, &tag_ranges);
    let bare = regex::Regex::new(&format!(
        r#"(?i)(^|[\s(:：])((?:~/|/)[^\s`"',;:)\]}}]+?\.(?:{EXTS}))($|[\s`"',;:)\]}}])"#
    ))
    .unwrap();
    // Rust regex has no lookahead. Resume at the end of the path so the
    // delimiter remains available as the next candidate's prefix.
    let bare_text = String::from_utf8_lossy(&masked);
    let mut position = 0;
    while let Some(c) = bare.captures_at(&bare_text, position) {
        let m = c.get(2).unwrap();
        candidates.push((m.start(), m.as_str().into()));
        position = m.end();
    }
    candidates.sort_by_key(|(at, _)| *at);
    let mut paths = vec![];
    let mut warnings = vec![];
    for (_, raw) in candidates {
        match validate_path(&raw, pilot_home) {
            Ok(p) => {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
            Err(reason) => {
                let message = format!("附件未发送：{reason}");
                if !warnings.contains(&message) {
                    warnings.push(message);
                }
            }
        }
    }
    (paths, warnings)
}
fn mask(bytes: &mut [u8], ranges: &[(usize, usize)]) {
    for (start, end) in ranges {
        for b in &mut bytes[*start..*end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
}
fn validate_path(raw: &str, pilot_home: &Path) -> std::result::Result<String, String> {
    let raw = raw
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    if raw.is_empty() {
        return Err("附件路径为空。".into());
    }
    let local = if let Ok(url) = url::Url::parse(raw) {
        if url.scheme() != "file" {
            return Err("企业微信附件投递仅支持本地文件路径。".into());
        }
        url.to_file_path()
            .map_err(|_| "附件路径必须是绝对路径或 ~/ 路径。".to_owned())?
    } else if let Some(tail) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join(tail)
    } else {
        PathBuf::from(raw)
    };
    if !local.is_absolute() {
        return Err("附件路径必须是绝对路径或 ~/ 路径。".into());
    }
    let mut path = PathBuf::new();
    for component in local.components() {
        if component == Component::ParentDir {
            path.pop();
        } else if component != Component::CurDir {
            path.push(component.as_os_str());
        }
    }
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !EXTS.split('|').any(|e| e == ext) {
        return Err(format!(
            "不支持的附件类型 {}。",
            if ext.is_empty() {
                "(无扩展名)".into()
            } else {
                format!(".{ext}")
            }
        ));
    }
    if denied(&path, pilot_home) {
        return Err("该路径位于敏感目录或敏感配置文件中。".into());
    }
    let info = std::fs::metadata(&path).map_err(|_| "附件文件不存在。".to_owned())?;
    if !info.is_file() {
        return Err("附件路径不是普通文件。".into());
    }
    let real = std::fs::canonicalize(&path).unwrap_or(path);
    if denied(&real, pilot_home) {
        return Err("该路径位于敏感目录或敏感配置文件中。".into());
    }
    Ok(real.to_string_lossy().into_owned())
}
fn denied(path: &Path, pilot_home: &Path) -> bool {
    let basename = basename(path).to_lowercase();
    [
        ".env",
        "auth.json",
        "credentials",
        "server-token",
        "pilotdeck.yaml",
    ]
    .contains(&basename.as_str())
        || path.components().any(|c| {
            [".git", ".ssh", "node_modules", "mcp-tokens", "pairing"]
                .contains(&c.as_os_str().to_string_lossy().to_lowercase().as_str())
        })
        || path.starts_with(pilot_home.join("server-token"))
        || path.starts_with(pilot_home.join("pilotdeck.yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;
    #[test]
    fn aes_media_validates_every_pkcs7_byte_and_preserves_payload() {
        let key = [7u8; 32];
        let encoded = STANDARD.encode(key);
        let mut raw = b"media bytes".to_vec();
        let pad = 32 - raw.len() % 32;
        raw.extend(std::iter::repeat_n(pad as u8, pad));
        let encrypt = |mut raw: Vec<u8>| {
            let cipher = Aes256::new_from_slice(&key).unwrap();
            let mut previous = [7; 16];
            for chunk in raw.chunks_exact_mut(16) {
                for (byte, iv) in chunk.iter_mut().zip(previous) {
                    *byte ^= iv;
                }
                let mut block = aes::Block::default();
                block.copy_from_slice(chunk);
                cipher.encrypt_block(&mut block);
                chunk.copy_from_slice(&block);
                previous.copy_from_slice(chunk);
            }
            raw
        };
        assert_eq!(
            decrypt(&encrypt(raw.clone()), &encoded).unwrap(),
            b"media bytes"
        );
        raw[20] = 0;
        assert!(decrypt(&encrypt(raw), &encoded).is_err());
        assert!(decrypt(&[0; 16], "bad key").is_err());
    }
    #[test]
    fn mime_detection_size_downgrades_and_remote_names_follow_ts() {
        assert_eq!(
            prepare(vec![0; 10 * 1024 * 1024 + 1], "image/png", "a.png").kind,
            "file"
        );
        let rejected = prepare(vec![0; MAX + 1], "image/png", "a.png");
        assert!(rejected.reject.unwrap().contains("20MB"));
        assert_eq!(prepare(vec![0; 1], "audio/ogg", "a.ogg").kind, "file");
        assert_eq!(prepare(vec![0; 1], "audio/amr", "a.amr").kind, "voice");
        assert_eq!(
            normalize_mime("text/plain; charset=utf-8", "a.png"),
            "image/png"
        );
        assert_eq!(
            guess_name(
                &url::Url::parse("http://127.0.0.1/media").unwrap(),
                Some("attachment; filename*=UTF-8''test%20file.png")
            ),
            "test file.png"
        );
    }
    #[test]
    fn deliverables_mask_code_quotes_validate_realpath_and_keep_visible_text() {
        let root = std::env::temp_dir().join(format!("wecom-delivery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("output file.pdf");
        std::fs::write(&path, b"pdf").unwrap();
        let hidden = root.join("hidden.pdf");
        std::fs::write(&hidden, b"hidden").unwrap();
        let text = format!(
            "MEDIA:`{}`\n`{}`\n> {}\n```\n{}\n```\nMEDIA:/some/missing.pdf\nMEDIA:/some/.ssh/id.pdf",
            path.display(),
            hidden.display(),
            hidden.display(),
            hidden.display()
        );
        let (paths, warnings) = extract_paths(&text, &root);
        assert_eq!(
            paths,
            vec![
                std::fs::canonicalize(&path)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            ]
        );
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("不存在")));
        assert!(warnings.iter().any(|w| w.contains("敏感")));
    }
}

#[cfg(test)]
mod adjacent_path_tests {
    use super::*;
    #[test]
    fn bare_deliverables_share_one_delimiter_and_all_remain_candidates() {
        let root = std::env::temp_dir().join(format!("wecom-adjacent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("a.pdf");
        let b = root.join("b.pdf");
        let c = root.join("c.pdf");
        for path in [&a, &b, &c] {
            std::fs::write(path, b"pdf").unwrap();
        }
        let text = format!("{} {} {}", a.display(), b.display(), c.display());
        let (paths, warnings) = extract_paths(&text, &root);
        assert!(warnings.is_empty());
        assert_eq!(
            paths,
            [a, b, c]
                .iter()
                .map(|p| std::fs::canonicalize(p)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned())
                .collect::<Vec<_>>()
        );
    }
}
