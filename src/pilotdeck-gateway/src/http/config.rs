//! Authenticated Web UI configuration facade over the shared CONFIG owner.

use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::{Value, json};

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub(super) async fn handle(
    request: Request<Incoming>,
    state: &super::HttpState,
) -> Response<crate::Body> {
    if !super::authorized(request.headers(), &state.token) {
        return error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "missing or invalid token",
        );
    }
    let (method, params) = match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/config") => ("webui_config_get", JsValue::Object(Default::default())),
        (&Method::POST, "/api/config/validate") => {
            let body = match super::read_body(request.into_body(), MAX_BODY_BYTES).await {
                Ok(body) => body,
                Err(response) => return *response,
            };
            match validate_params(&body) {
                Ok(params) => ("webui_config_validate", params),
                Err(response) => return *response,
            }
        }
        (&Method::PUT, "/api/config") => {
            let body = match super::read_body(request.into_body(), MAX_BODY_BYTES).await {
                Ok(body) => body,
                Err(response) => return *response,
            };
            match body_params(&body) {
                Ok(params) => ("webui_config_save", params),
                Err(response) => return *response,
            }
        }
        (_, "/api/config" | "/api/config/validate") => {
            return error(
                StatusCode::METHOD_NOT_ALLOWED,
                "METHOD_NOT_ALLOWED",
                "method not allowed",
            );
        }
        _ => {
            return error(StatusCode::NOT_FOUND, "NOT_FOUND", "config route not found");
        }
    };
    let Some(owner) = state.config.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "CONFIG_UNAVAILABLE",
            "native CONFIG owner is unavailable",
        );
    };
    match tokio::task::spawn_blocking(move || {
        crate::config::routes::native::call_webui_blocking(&owner, method, &params)
    })
    .await
    {
        Ok(Ok(value)) => match js_json(&value) {
            Ok(value) => {
                let status = if method == "webui_config_validate"
                    && value.get("valid") == Some(&Value::Bool(false))
                {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                };
                super::json(status, value)
            }
            Err(message) => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SERIALIZE_FAILED",
                &message,
            ),
        },
        Ok(Err(route)) => {
            let status = match route.code.as_str() {
                "INVALID_WEBUI_CONFIG"
                | "WEBUI_CONFIG_VALIDATION_FAILED"
                | "WEBUI_CONFIG_YAML_INVALID"
                | "WEBUI_CONFIG_INVALID" => StatusCode::BAD_REQUEST,
                "CONFIG_HOST_STOPPED" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error(status, &route.code, &route.message)
        }
        Err(join) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONFIG_REQUEST_FAILED",
            &join.to_string(),
        ),
    }
}

fn body_params(body: &[u8]) -> Result<JsValue, Box<Response<crate::Body>>> {
    let text = std::str::from_utf8(body).map_err(|_| {
        Box::new(error(
            StatusCode::BAD_REQUEST,
            "INVALID_JSON",
            "body must be UTF-8 JSON",
        ))
    })?;
    let value = parse_json(text).map_err(|_| {
        Box::new(error(
            StatusCode::BAD_REQUEST,
            "INVALID_JSON",
            "body must be valid JSON",
        ))
    })?;
    if value.as_object().is_none() {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "INVALID_JSON",
            "body must be a JSON object",
        )));
    }
    Ok(value)
}

fn validate_params(body: &[u8]) -> Result<JsValue, Box<Response<crate::Body>>> {
    let params = body_params(body)?;
    let Some(raw) = params.get("raw") else {
        return Ok(if params.get("config").is_some() {
            params
        } else {
            crate::metadata::object([("config", params)])
        });
    };
    let Some(raw) = raw.as_string().and_then(|raw| raw.to_utf8().ok()) else {
        return Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "INVALID_CONFIG_YAML",
            "raw must be a YAML string",
        )));
    };
    let config = crate::config::webui::parse_source(&raw).map_err(|error_value| {
        Box::new(error(
            StatusCode::BAD_REQUEST,
            error_value.code,
            &error_value.message,
        ))
    })?;
    Ok(crate::metadata::object([("config", config)]))
}

fn js_json(value: &JsValue) -> Result<Value, String> {
    let text = stringify_json(value)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "undefined response".to_owned())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn error(status: StatusCode, code: &str, message: &str) -> Response<crate::Body> {
    super::json(status, json!({"error": {"code": code, "message": message}}))
}
