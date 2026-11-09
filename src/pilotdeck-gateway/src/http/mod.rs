//! Native HTTP routes; no route has a TS fallback.
mod config;
pub mod module_host;
pub(crate) mod runtime_bindings;
mod static_assets;
pub use static_assets::{FeishuWebhook, WecomWebhook};
mod web;

use crate::Body;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming, header};
use std::{convert::Infallible, path::PathBuf};

pub(crate) struct HttpState {
    pub(crate) token: String,
    pub(crate) home: Option<PathBuf>,
    pub(crate) cwd: PathBuf,
    pub(crate) workspaces_root: Option<PathBuf>,
    pub(crate) static_root: Option<PathBuf>,
    pub(crate) loopback: bool,
    pub(crate) feishu_webhook: Option<FeishuWebhook>,
    pub(crate) wecom_webhook: Option<WecomWebhook>,
    pub(crate) module_host: module_host::Bindings,
    pub(crate) ws_gateway:
        Option<std::sync::Arc<dyn crate::runtime::connection::ConnectionGateway>>,
    pub(crate) config: Option<crate::config::routes::native::SharedConfigRoutes>,
}
impl HttpState {
    pub(crate) fn new(token: String, home: Option<PathBuf>, cwd: PathBuf) -> Self {
        Self {
            token,
            home,
            cwd,
            workspaces_root: None,
            loopback: true,
            feishu_webhook: None,
            wecom_webhook: None,
            module_host: module_host::Bindings::default(),
            ws_gateway: None,
            config: None,
            static_root: std::env::var_os("PILOTDECK_STATIC_ASSETS_PATH").map(PathBuf::from),
        }
    }
}

pub(crate) async fn handle(request: Request<Incoming>, state: &HttpState) -> Response<Body> {
    match request.uri().path() {
        "/api/module-host/describe"
        | "/api/module-host/call"
        | "/api/module-host/approvals/status"
        | "/api/module-host/approvals/resume" => module_host::handle(request, state).await,
        path if path == "/api/config" || path.starts_with("/api/config/") => {
            config::handle(request, state).await
        }
        path if path == "/api/browse-filesystem" || path.starts_with("/api/web/") => {
            web::handle(request, state).await
        }
        _ => static_assets::handle(request, state).await,
    }
}

pub(super) fn json(status: StatusCode, value: serde_json::Value) -> Response<Body> {
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

pub(super) fn authorized(headers: &hyper::HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, rest)) = value.split_once(char::is_whitespace) else {
        return false;
    };
    scheme.eq_ignore_ascii_case("Bearer") && rest.trim_start_matches(char::is_whitespace) == token
}

pub(super) async fn read_body(
    mut body: Incoming,
    limit: usize,
) -> Result<Vec<u8>, Box<Response<Body>>> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| Box::new(json(StatusCode::BAD_REQUEST, serde_json::json!({"error":{"code":"invalid_body","message":"Could not read request body"}}))))?;
        if let Some(data) = frame.data_ref() {
            if bytes.len().saturating_add(data.len()) > limit {
                return Err(Box::new(json(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    serde_json::json!({"error":{"code":"input_too_large","message":"Request body too large"}}),
                )));
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
