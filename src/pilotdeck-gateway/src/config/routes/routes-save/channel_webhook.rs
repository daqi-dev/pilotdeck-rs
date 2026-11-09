//! Production HTTP inbound for IM adapters (`submit_host.rs:236-268`, `FeishuChannel.ts:306-333`).
use super::channel_inbound::{self, ProductionRouteInputResult};
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
) -> crate::http::FeishuWebhook {
    Arc::new(move |request| {
        let home = home.clone();
        let cwd = cwd.clone();
        Box::pin(async move { handle_channel_webhook(request, &home, &cwd, "feishu").await })
    })
}

pub fn production_wecom_webhook(
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
) -> crate::http::WecomWebhook {
    Arc::new(move |request| {
        let home = home.clone();
        let cwd = cwd.clone();
        Box::pin(async move { handle_channel_webhook(request, &home, &cwd, "wecom").await })
    })
}

async fn handle_channel_webhook(
    request: Request<String>,
    home: &Path,
    cwd: &Path,
    channel_key: &str,
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

    fn installed_root() -> (
        std::path::PathBuf,
        crate::config::routes::native::SharedConfigRoutes,
    ) {
        let root =
            std::env::temp_dir().join(format!("channel-webhook-prod-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("pilotdeck.yaml"), CONFIG).unwrap();
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

    #[test]
    fn production_route_input_non_slash_passes_through() {
        let (root, _owner) = installed_root();
        let result = channel_inbound::production_route_input(&root, &root, "feishu", "你好", |_| {});
        assert!(result.pass_through);
        assert!(!result.slash_handled);
        let body = webhook_json_from_route(result);
        assert_eq!(body["passThrough"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn feishu_webhook_production_route_input_slash_command() {
        let (root, _owner) = installed_root();
        let callback = production_feishu_webhook(root.clone(), root.clone());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/feishu/webhook")
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
    async fn wecom_webhook_production_route_input_slash_command() {
        let (root, _owner) = installed_root();
        let callback = production_wecom_webhook(root.clone(), root.clone());
        let request = hyper::Request::builder()
            .method("POST")
            .uri("/wecom/webhook")
            .body(r#"{"chatId":"c1","text":"/update check"}"#.into())
            .unwrap();
        let response = callback(request).await.unwrap().expect("handled");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["routeInput"], "production_route_input");
        assert_eq!(json["handled"], true);
        let _ = std::fs::remove_dir_all(root);
    }
}
