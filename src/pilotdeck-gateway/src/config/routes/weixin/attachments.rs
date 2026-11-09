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

static PATH_PART_SANITIZER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn path_part_sanitizer() -> &'static regex::Regex {
    PATH_PART_SANITIZER.get_or_init(|| {
        regex::Regex::new("[^a-zA-Z0-9._-]+").expect("valid regex")
    })
}

pub(crate) fn format_attachment_diagnostics_block(lines: &[String]) -> String {
    format!(
        "[Attachment diagnostics]\n{}",
        lines
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

const ATTACHMENT_DIAGNOSTICS_MARKER: &str = "[Attachment diagnostics]\n";

pub(crate) fn merge_attachment_diagnostics(text: String, lines: &[String]) -> String {
    if lines.is_empty() {
        return text;
    }
    if let Some(marker_at) = text.find(ATTACHMENT_DIAGNOSTICS_MARKER) {
        let (prefix, rest) = text.split_at(marker_at);
        let existing = &rest[ATTACHMENT_DIAGNOSTICS_MARKER.len()..];
        let mut merged: Vec<String> = existing
            .lines()
            .filter_map(|line| line.strip_prefix("- ").map(str::to_owned))
            .collect();
        merged.extend(lines.iter().cloned());
        let block = format_attachment_diagnostics_block(&merged);
        if prefix.trim().is_empty() {
            block
        } else {
            format!("{}\n{block}", prefix.trim_end())
        }
    } else if text.is_empty() {
        format_attachment_diagnostics_block(lines)
    } else {
        format!("{text}\n{}", format_attachment_diagnostics_block(lines))
    }
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
    let replaced = path_part_sanitizer()
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
    #[cfg(unix)]
    if path.exists() {
        let meta = std::fs::metadata(&path).map_err(|error| failure(&error.to_string()))?;
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_fifo() {
            return Err(failure("Attachment path escapes storage root"));
        }
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
) -> Result<Vec<String>> {
    let store_dir = attachment_store_dir(home, chat, message_id)?;
    let mut diagnostics = Vec::new();
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
                diagnostics.push(super::media::weixin_attachment_failure_diagnostic(
                    &error.message,
                ));
                attachments.remove(index);
            }
        }
    }
    Ok(diagnostics)
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
            "type": super::item_types::MESSAGE_ITEM_IMAGE,
            "image_item": {
                "media": media,
                "mid_size": aes_ecb_padded_size(data.len()),
            }
        })
    } else {
        json!({
            "type": super::item_types::MESSAGE_ITEM_FILE,
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
    if response.status != hyper::StatusCode::OK {
        let detail = if let Some(value) = response.headers.get("x-error-message") {
            value
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|_| String::from_utf8_lossy(&response.body).into_owned())
        } else {
            String::from_utf8_lossy(&response.body).into_owned()
        };
        return Err(failure(&format!(
            "weixin CDN upload HTTP {}: {detail}",
            response.status.as_u16()
        )));
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
mod tests;
