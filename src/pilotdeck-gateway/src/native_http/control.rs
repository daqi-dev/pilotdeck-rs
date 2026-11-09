//! Optional authenticated management for a source-installed native runtime.
//! It has a separate namespace and never impersonates the legacy Web JWT API.
use super::{Config, finish, handler_response};
use crate::Body;
use http_body_util::BodyExt;
use std::{future::Future, io, pin::Pin, sync::Arc, time::Duration};

pub struct ControlRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}
pub struct ControlReply {
    pub status: u16,
    pub body: String,
}
pub type ControlHandler =
    Arc<dyn Fn(ControlRequest) -> Pin<Box<dyn Future<Output = ControlReply> + Send>> + Send + Sync>;
const MAX_BODY: usize = 16 * 1024;

pub(super) async fn serve(
    request: hyper::Request<hyper::body::Incoming>,
    config: &Config,
) -> Result<hyper::Response<Body>, io::Error> {
    let version = request.version();
    let reply = |status, body: &str| {
        finish(
            version,
            handler_response(
                status,
                "application/json; charset=utf-8",
                body.into(),
                false,
            ),
        )
    };
    let expected = format!("Bearer {}", config.token);
    let mut authorization = request
        .headers()
        .get_all(hyper::header::AUTHORIZATION)
        .iter();
    if authorization.next().and_then(|v| v.to_str().ok()) != Some(expected.as_str())
        || authorization.next().is_some()
    {
        return reply(
            hyper::StatusCode::UNAUTHORIZED,
            r#"{"error":"unauthorized"}"#,
        );
    }
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    if !matches!(
        (method.as_str(), path.as_str()),
        ("GET", "/_control/context") | ("POST", "/_control/restart")
    ) {
        return reply(
            hyper::StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"method_not_allowed"}"#,
        );
    }
    let mut incoming = request.into_body();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut body = Vec::new();
        while let Some(frame) = incoming.frame().await {
            let frame = frame.map_err(|_| ())?;
            if let Ok(data) = frame.into_data() {
                if data.len() > MAX_BODY.saturating_sub(body.len()) {
                    return Err(());
                }
                body.extend_from_slice(&data);
            }
        }
        Ok(body)
    })
    .await;
    let body = match read {
        Ok(Ok(body)) => body,
        Ok(Err(())) => {
            return reply(
                hyper::StatusCode::PAYLOAD_TOO_LARGE,
                r#"{"error":"invalid_body"}"#,
            );
        }
        Err(_) => {
            return reply(
                hyper::StatusCode::REQUEST_TIMEOUT,
                r#"{"error":"body_timeout"}"#,
            );
        }
    };
    let handler = config
        .control
        .as_ref()
        .expect("router checked management handler");
    let result = handler(ControlRequest { method, path, body }).await;
    reply(
        hyper::StatusCode::from_u16(result.status)
            .unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR),
        &result.body,
    )
}
