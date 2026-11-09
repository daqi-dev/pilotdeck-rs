//! Production HTTP inbound for IM adapters (`submit_host.rs:236-268`, `FeishuChannel.ts:306-333`).
use super::channel_inbound::{self, ProductionRouteInputResult};
use super::feishu::inbound::{self, Parsed};
use crate::{Body, ProxyError};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, header};
use serde_json::Value;
use std::convert::Infallible;
use std::{path::Path, sync::Arc};

pub fn production_feishu_webhook(
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    gateway_token: String,
) -> crate::http::FeishuWebhook {
    Arc::new(move |request| {
        let home = home.clone();
        let cwd = cwd.clone();
        let gateway_token = gateway_token.clone();
        Box::pin(async move {
            handle_channel_webhook(request, &home, &cwd, "feishu", &gateway_token).await
        })
    })
}

pub fn production_wecom_webhook(
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
    gateway_token: String,
) -> crate::http::WecomWebhook {
    Arc::new(move |request| {
        let home = home.clone();
        let cwd = cwd.clone();
        let gateway_token = gateway_token.clone();
        Box::pin(async move {
            handle_channel_webhook(request, &home, &cwd, "wecom", &gateway_token).await
        })
    })
}

fn snapshot_env(home: &Path) -> super::Env {
    super::Env::from([(
        "PILOT_HOME".into(),
        home.to_string_lossy().into_owned(),
    )])
}

fn wecom_webhook_crypto(home: &Path, cwd: &Path) -> crate::application::WecomWebhookCrypto {
    let env = snapshot_env(home);
    crate::config::load_snapshot(home, cwd, &env, None, 1.0, "1970-01-01T00:00:00.000Z")
        .ok()
        .map(|snapshot| crate::application::wecom_webhook_crypto(&snapshot, &env))
        .unwrap_or_default()
}

fn feishu_options(home: &Path, cwd: &Path) -> super::feishu::FeishuOptions {
    let env = snapshot_env(home);
    crate::config::load_snapshot(home, cwd, &env, None, 1.0, "1970-01-01T00:00:00.000Z")
        .ok()
        .and_then(|snapshot| crate::application::feishu_options(&snapshot, home))
        .unwrap_or_default()
}

async fn handle_channel_webhook(
    request: Request<String>,
    home: &Path,
    cwd: &Path,
    channel_key: &str,
    gateway_token: &str,
) -> Result<Option<Response<Body>>, ProxyError> {
    if request.method() != hyper::Method::POST {
        return Ok(None);
    }
    let body = request.body();
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => {
            return Ok(Some(webhook_json(
                StatusCode::OK,
                serde_json::json!({ "ok": true }),
            )));
        }
    };
    if value.get("type").and_then(Value::as_str) == Some("url_verification") {
        let challenge = value
            .get("challenge")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Ok(Some(webhook_json(
            StatusCode::OK,
            serde_json::json!({ "challenge": challenge }),
        )));
    }
    if !gateway_token.is_empty() && !crate::http::authorized(request.headers(), gateway_token) {
        return Ok(Some(webhook_json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error":{"code":"unauthorized","message":"Bearer token required"}}),
        )));
    }
    if channel_key == "feishu" {
        let options = feishu_options(home, cwd);
        return Ok(Some(match inbound::parse(body, &options) {
            Parsed::Challenge(challenge) => webhook_json(
                StatusCode::OK,
                serde_json::json!({ "challenge": challenge }),
            ),
            Parsed::Ignore => webhook_json(StatusCode::OK, serde_json::json!({ "ok": true })),
            Parsed::Message(message) => webhook_json(
                StatusCode::OK,
                route_input_webhook_body(home, cwd, channel_key, &message.text),
            ),
        }));
    }
    if channel_key == "wecom" {
        let crypto = wecom_webhook_crypto(home, cwd);
        if !verify_wecom_webhook_signature(&request, &value, &crypto) {
            return Ok(Some(webhook_json(
                StatusCode::FORBIDDEN,
                serde_json::json!({"error":{"code":"signature","message":"msg_signature verification failed"}}),
            )));
        }
        let value = match decrypt_wecom_webhook_body(&value, &crypto) {
            Ok(value) => value,
            Err(_) => {
                return Ok(Some(webhook_json(
                    StatusCode::FORBIDDEN,
                    serde_json::json!({"error":{"code":"signature","message":"msg_signature verification failed"}}),
                )));
            }
        };
        let Some(text) = extract_inbound_text(&value) else {
            return Ok(Some(webhook_json(
                StatusCode::OK,
                serde_json::json!({ "ok": true }),
            )));
        };
        return Ok(Some(webhook_json(
            StatusCode::OK,
            route_input_webhook_body(home, cwd, channel_key, &text),
        )));
    }
    let Some(text) = extract_inbound_text(&value) else {
        return Ok(Some(webhook_json(
            StatusCode::OK,
            serde_json::json!({ "ok": true }),
        )));
    };
    Ok(Some(webhook_json(
        StatusCode::OK,
        route_input_webhook_body(home, cwd, channel_key, &text),
    )))
}

fn route_input_webhook_body(home: &Path, cwd: &Path, channel_key: &str, text: &str) -> Value {
    let result = channel_inbound::production_route_input(home, cwd, channel_key, text, |_| {});
    webhook_json_from_route(result)
}

pub(crate) fn webhook_json_from_route(result: ProductionRouteInputResult) -> Value {
    if result.pass_through {
        return serde_json::json!({
            "ok": true,
            "passThrough": true,
            "routeInput": "submit",
        });
    }
    serde_json::json!({
        "ok": true,
        "handled": result.slash_handled,
        "replies": result.replies,
        "routeInput": "production_route_input",
    })
}

fn query_param(request: &Request<String>, name: &str) -> String {
    request
        .uri()
        .query()
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

fn wecom_encrypt_material(value: &Value) -> Option<String> {
    value
        .get("encrypt")
        .or_else(|| value.get("Encrypt"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn verify_wecom_webhook_signature(
    request: &Request<String>,
    value: &Value,
    crypto: &crate::application::WecomWebhookCrypto,
) -> bool {
    let msg_signature = query_param(request, "msg_signature");
    let timestamp = query_param(request, "timestamp");
    let nonce = query_param(request, "nonce");
    if msg_signature.is_empty() || timestamp.is_empty() || nonce.is_empty() {
        return false;
    }
    let Some(encrypt) = wecom_encrypt_material(value) else {
        return false;
    };
    let token = crypto.token.trim();
    if token.is_empty() {
        return false;
    }
    super::wecom_callback::crypto::signature(token, &timestamp, &nonce, &encrypt) == msg_signature
}

fn decrypt_wecom_webhook_body(
    value: &Value,
    crypto: &crate::application::WecomWebhookCrypto,
) -> Result<Value, ()> {
    let Some(encrypt) = wecom_encrypt_material(value) else {
        return Ok(value.clone());
    };
    let key = crypto.encoding_aes_key.trim();
    if key.is_empty() {
        return Err(());
    }
    let plain = super::wecom_callback::crypto::decrypt(key, &encrypt, crypto.corp_id.trim())
        .map_err(|_| ())?;
    Ok(wecom_inbound_from_plaintext(&plain))
}

fn wecom_inbound_from_plaintext(plain: &str) -> Value {
    if plain.trim_start().starts_with('<') {
        let content = wecom_xml_tag(plain, "Content").unwrap_or_default();
        let chat = wecom_xml_tag(plain, "FromUserName").unwrap_or_default();
        serde_json::json!({ "chatId": chat, "text": content })
    } else {
        serde_json::from_str(plain).unwrap_or_else(|_| serde_json::json!({ "text": plain }))
    }
}

fn wecom_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(
        r"(?i)<{tag}><!\[CDATA\[([\s\S]*?)\]\]></{tag}>|<{tag}>([^<]*)</{tag}>"
    ))
    .ok()?;
    let matched = re.captures(xml)?;
    Some(
        crate::metadata::js_trim(matched.get(1).or_else(|| matched.get(2))?.as_str()).into(),
    )
}

fn extract_inbound_text(value: &Value) -> Option<String> {
    if let (Some(_chat_id), Some(text)) = (
        value.get("chatId").and_then(Value::as_str),
        value.get("text").and_then(Value::as_str),
    ) {
        return Some(text.to_owned());
    }
    if let Some(text) = value
        .get("event")
        .and_then(|event| event.get("text"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if let Some(text) = value.get("Text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    if let Some(text) = value
        .get("body")
        .and_then(|body| body.get("text"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    let header = value.get("header")?;
    if header.get("event_type").and_then(Value::as_str) != Some("im.message.receive_v1") {
        return None;
    }
    let message = value.get("event")?.get("message")?;
    if message.get("message_type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let content = message.get("content")?.as_str()?;
    let parsed: Value = serde_json::from_str(content).ok()?;
    parsed
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn webhook_json(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            Full::new(Bytes::from(serde_json::to_vec(&value).expect("JSON value")))
                .map_err(|never: Infallible| match never {})
                .boxed(),
        )
        .expect("valid response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::routes::native::install;
    use crate::config::routes::{ConfigRoutes, Env};
    use http_body_util::BodyExt;

    const CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\n";

    const FEISHU_CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\nadapters:\n  feishu:\n    enabled: true\n    verifyToken: vt-hook\n";

    const WECOM_KEY: &str = "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXowMTIzNDU";
    const WECOM_CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\nadapters:\n  wecom:\n    enabled: true\n    token: wecom-signing-token\n    extra:\n      encodingAesKey: YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXowMTIzNDU\n      corpId: corp\n";

    fn installed_root(yaml: &str) -> (
        std::path::PathBuf,
        crate::config::routes::native::SharedConfigRoutes,
    ) {
        let root =
            std::env::temp_dir().join(format!("channel-webhook-prod-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("pilotdeck.yaml"), yaml).unwrap();
        let owner = install(
            ConfigRoutes::new(
                root.clone(),
                root.clone(),
                Env::from([("PILOT_HOME".into(), root.to_string_lossy().into_owned())]),
            )
            .unwrap(),
        );
        (root, owner)
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[test]
    fn production_route_input_non_slash_passes_through() {
        let (root, _owner) = installed_root(CONFIG);
        let result = channel_inbound::production_route_input(&root, &root, "feishu", "你好", |_| {});
        assert!(result.pass_through);
        assert!(!result.slash_handled);
        let body = webhook_json_from_route(result);
        assert_eq!(body["passThrough"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn feishu_webhook_production_route_input_slash_command() {
        let (root, _owner) = installed_root(CONFIG);
        let callback = production_feishu_webhook(root.clone(), root.clone(), "hook-secret".into());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/feishu/webhook")
            .header(header::AUTHORIZATION, bearer("hook-secret"))
            .body(r#"{"chatId":"c1","text":"/update check"}"#.into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["routeInput"], "production_route_input");
        assert_eq!(json["handled"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unauthenticated_slash_update_is_rejected() {
        let (root, _owner) = installed_root(CONFIG);
        let callback = production_feishu_webhook(root.clone(), root.clone(), "hook-secret".into());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/feishu/webhook")
            .body(r#"{"chatId":"c1","text":"/update check"}"#.into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn feishu_webhook_rejects_wrong_event_token_without_routing() {
        let (root, _owner) = installed_root(FEISHU_CONFIG);
        let callback = production_feishu_webhook(root.clone(), root.clone(), "hook-secret".into());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/feishu/webhook")
            .header(header::AUTHORIZATION, bearer("hook-secret"))
            .body(r#"{"header":{"event_type":"im.message.receive_v1","token":"wrong"},"event":{"message":{"message_type":"text","chat_id":"c1","content":"{\"text\":\"/update check\"}"}}}"#.into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        assert!(json.get("handled").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn wecom_webhook_rejects_missing_signature_query() {
        let (root, _owner) = installed_root(WECOM_CONFIG);
        let callback = production_wecom_webhook(root.clone(), root.clone(), "hook-secret".into());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/wecom/webhook")
            .header(header::AUTHORIZATION, bearer("hook-secret"))
            .body(r#"{"chatId":"c1","text":"/update check"}"#.into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn wecom_webhook_rejects_bad_msg_signature() {
        let (root, _owner) = installed_root(WECOM_CONFIG);
        let callback = production_wecom_webhook(root.clone(), root.clone(), "hook-secret".into());
        let encrypt = super::super::wecom_callback::crypto::encrypt(
            WECOM_KEY,
            "/update check",
            "corp",
        );
        let request = hyper::Request::builder()
            .method("POST")
            .uri(format!(
                "/wecom/webhook?msg_signature=bad&timestamp=123&nonce=nonce"
            ))
            .header(header::AUTHORIZATION, bearer("hook-secret"))
            .body(format!(r#"{{"encrypt":"{encrypt}"}}"#).into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn wecom_webhook_accepts_valid_msg_signature_and_decrypts_payload() {
        use super::super::wecom_callback::crypto;
        let (root, _owner) = installed_root(WECOM_CONFIG);
        let callback = production_wecom_webhook(root.clone(), root.clone(), "hook-secret".into());
        let xml = format!(
            "<xml><MsgType><![CDATA[text]]></MsgType><FromUserName><![CDATA[c1]]></FromUserName><Content><![CDATA[/update check]]></Content></xml>"
        );
        let encrypt = crypto::encrypt(WECOM_KEY, &xml, "corp");
        let signature = crypto::signature("wecom-signing-token", "123", "nonce", &encrypt);
        let request = hyper::Request::builder()
            .method("POST")
            .uri(format!(
                "/wecom/webhook?msg_signature={signature}&timestamp=123&nonce=nonce"
            ))
            .header(header::AUTHORIZATION, bearer("hook-secret"))
            .body(format!(r#"{{"encrypt":"{encrypt}"}}"#).into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["routeInput"], "production_route_input");
        assert_eq!(json["handled"], true);
        let _ = std::fs::remove_dir_all(root);
    }
}
