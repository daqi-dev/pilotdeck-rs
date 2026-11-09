use super::*;
use aes::{
    Aes256,
    cipher::{BlockDecrypt, KeyInit},
};
use base64::Engine;
use sha2::{Digest, Sha256};
pub(super) enum Parsed {
    Challenge(String),
    Message(Inbound),
    Ignore,
}
#[derive(Clone)]
pub(super) struct Inbound {
    pub chat: String,
    pub text: String,
    pub event_id: String,
    pub message_id: Option<String>,
}
pub(super) fn parse(body: &str, options: &FeishuOptions) -> Parsed {
    let Ok(mut raw) = serde_json::from_str::<Value>(body) else {
        return Parsed::Ignore;
    };
    if let (Some(encrypted), Some(key)) = (raw["encrypt"].as_str(), options.encrypt_key.as_deref())
    {
        let Ok(plain) = decrypt(encrypted, key) else {
            return Parsed::Ignore;
        };
        let Ok(decoded) = serde_json::from_str(&plain) else {
            return Parsed::Ignore;
        };
        raw = decoded;
    }
    if raw["type"] == "url_verification" {
        if let Some(challenge) = raw["challenge"].as_str() {
            return Parsed::Challenge(challenge.into());
        }
    }
    if let Some(expected) = options
        .verify_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        let token = raw
            .get("token")
            .filter(|token| !token.is_null())
            .or_else(|| raw["header"].get("token"));
        if token.is_some_and(|token| !token.as_str().unwrap_or("").is_empty() && token != expected)
        {
            return Parsed::Ignore;
        }
    }
    if let (Some(chat), Some(text)) = (raw["chatId"].as_str(), raw["text"].as_str()) {
        return Parsed::Message(Inbound {
            chat: chat.into(),
            text: text.into(),
            event_id: raw["eventId"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("direct:{chat}:{}", now_ms())),
            message_id: None,
        });
    }
    let header = &raw["header"];
    let message = &raw["event"]["message"];
    if let Some(event_id) = header["event_id"].as_str().filter(|id| !id.is_empty()) {
        if message.is_object() {
            if header["event_type"] != "im.message.receive_v1" || message["message_type"] != "text"
            {
                return Parsed::Ignore;
            }
            if let (Some(chat), Some(content)) = (
                message["chat_id"].as_str().filter(|chat| !chat.is_empty()),
                message["content"].as_str(),
            ) {
                let text = serde_json::from_str::<Value>(content)
                    .map(|parsed| parsed["text"].as_str().unwrap_or("").to_owned())
                    .unwrap_or_else(|_| content.into());
                return Parsed::Message(Inbound {
                    chat: chat.into(),
                    text,
                    event_id: event_id.into(),
                    message_id: message["message_id"].as_str().map(str::to_owned),
                });
            }
        }
    }
    let event = &raw["event"];
    if let (Some(chat), Some(text)) = (
        event["chat_id"].as_str().filter(|chat| !chat.is_empty()),
        event["text"].as_str(),
    ) {
        return Parsed::Message(Inbound {
            chat: chat.into(),
            text: text.into(),
            event_id: raw["uuid"]
                .as_str()
                .or_else(|| event["uuid"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("v1:{chat}:{}", now_ms())),
            message_id: None,
        });
    }
    Parsed::Ignore
}
fn decrypt(encrypted: &str, key: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|_| failure("Feishu invalid encrypted base64"))?;
    if bytes.len() < 32 || bytes.len() % 16 != 0 {
        return Err(failure("Feishu invalid encrypted length"));
    }
    let cipher = Aes256::new_from_slice(&Sha256::digest(key.as_bytes()))
        .map_err(|_| failure("Feishu invalid AES key"))?;
    let mut previous = [0; 16];
    previous.copy_from_slice(&bytes[..16]);
    let mut plain = Vec::new();
    for chunk in bytes[16..].chunks_exact(16) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        plain.extend(block.iter().zip(previous).map(|(byte, iv)| byte ^ iv));
        previous.copy_from_slice(chunk);
    }
    let padding = usize::from(*plain.last().unwrap());
    if padding == 0
        || padding > 16
        || plain.len() < padding
        || !plain[plain.len() - padding..]
            .iter()
            .all(|byte| usize::from(*byte) == padding)
    {
        return Err(failure("Feishu invalid AES padding"));
    }
    plain.truncate(plain.len() - padding);
    Ok(String::from_utf8_lossy(&plain).into_owned())
}
