//! Shared native StaffDeck exchange. Redirect policy is explicit: approver
//! credentials use error, the portable evaluator follows the existing fetch
//! owner. Header and body failures retain separate retry provenance.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{future::Future, pin::Pin, time::Duration};
#[derive(Debug)]
pub(crate) enum JsonFailure {
    Transport(String),
    Body(String),
}
pub(crate) type JsonFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonResponse, JsonFailure>> + Send + 'a>>;
pub(crate) struct JsonRequest {
    pub method: &'static str,
    pub url: url::Url,
    pub bearer: Option<String>,
    pub body: Option<String>,
    pub parse_error_body: bool,
    pub redirect_error: bool,
    pub timeout: Duration,
}
pub(crate) struct JsonResponse {
    pub status: u16,
    pub body: JsValue,
}
pub(crate) trait JsonTransport: Send + Sync {
    fn request(&self, request: JsonRequest) -> JsonFuture<'_>;
}
#[derive(Clone)]
pub(crate) struct HyperJsonTransport {
    client: Client<
        hyper_rustls::HttpsConnector<crate::runtime::model::http::ProxyConnect>,
        Full<Bytes>,
    >,
    limit: usize,
}
impl Default for HyperJsonTransport {
    fn default() -> Self {
        Self::from_env(&crate::config::Env::new(), None)
    }
}
impl HyperJsonTransport {
    pub(crate) fn from_env(
        env: &crate::config::Env,
        settings: Option<&crate::runtime::model::http::ProxySettings>,
    ) -> Self {
        use crate::runtime::model::http::{
            ProxyConfig, proxy_https_connector, webpki_client_config,
        };
        let proxy = ProxyConfig::from_settings_lookup(settings, |key| env.get(key).cloned());
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(proxy_https_connector(webpki_client_config(), proxy)),
            limit: 8 * 1024 * 1024,
        }
    }
    #[cfg(test)]
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
}

fn failed() -> JsonFailure {
    JsonFailure::Transport("fetch failed".into())
}
impl JsonTransport for HyperJsonTransport {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        Box::pin(async move {
            if !matches!(input.url.scheme(), "http" | "https")
                || !input.url.username().is_empty()
                || input.url.password().is_some()
            {
                return Err(failed());
            }
            let mut headers = HeaderMap::new();
            headers.insert(
                header::ACCEPT,
                if input.bearer.is_some() {
                    header::HeaderValue::from_static("application/json")
                } else {
                    header::HeaderValue::from_static("*/*")
                },
            );
            if let Some(bearer) = &input.bearer {
                headers.insert(
                    header::AUTHORIZATION,
                    format!("Bearer {bearer}").parse().map_err(|_| failed())?,
                );
            }
            if input.body.is_some() {
                headers.insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
            }
            let body = Bytes::from(input.body.unwrap_or_default());
            let method = Method::from_bytes(input.method.as_bytes()).map_err(|_| failed())?;
            let deadline = tokio::time::Instant::now() + input.timeout;
            let response = tokio::time::timeout_at(deadline, async {
                if input.redirect_error {
                    let mut request = Request::builder()
                        .method(method.clone())
                        .uri(input.url.as_str());
                    *request.headers_mut().ok_or_else(failed)? = headers;
                    let request = request.body(Full::new(body)).map_err(|_| failed())?;
                    let response = self.client.request(request).await.map_err(|_| failed())?;
                    if response.status().is_redirection() {
                        return Err(failed());
                    }
                    decode(&method, response, false)
                } else {
                    let client = self.client.clone();
                    crate::fetch_redirect::follow(
                        input.url,
                        method,
                        headers,
                        body,
                        failed,
                        move |request| {
                            let client = client.clone();
                            async move {
                                let method = request.method().clone();
                                let follow = request
                                    .extensions()
                                    .get::<crate::fetch_redirect::FollowRedirects>()
                                    .is_some();
                                let response =
                                    client.request(request).await.map_err(|_| failed())?;
                                decode(&method, response, follow)
                            }
                        },
                    )
                    .await
                }
            })
            .await
            .map_err(|_| JsonFailure::Transport("StaffDeck request timed out.".into()))??;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) && !input.parse_error_body {
                return Ok(JsonResponse {
                    status,
                    body: JsValue::Undefined,
                });
            }
            let raw = tokio::time::timeout_at(
                deadline,
                crate::fetch_redirect::read_body(response.into_body(), self.limit, None),
            )
            .await
            .map_err(|_| {
                JsonFailure::Body("StaffDeck response body exceeded its absolute deadline.".into())
            })?
            .map_err(|_| {
                JsonFailure::Body(
                    "StaffDeck response body could not be read within its 8 MiB budget.".into(),
                )
            })?;
            // The evaluator's response.json().catch(()=>undefined) rejects the
            // envelope, rather than reporting a safe-to-retry fetch failure.
            let text = String::from_utf8_lossy(&raw);
            let body = parse_json(text.strip_prefix('\u{feff}').unwrap_or(&text))
                .unwrap_or(JsValue::Undefined);
            Ok(JsonResponse { status, body })
        })
    }
}

fn decode(
    method: &Method,
    response: hyper::Response<hyper::body::Incoming>,
    follow: bool,
) -> Result<hyper::Response<crate::runtime::model::http::ModelBody>, JsonFailure> {
    use crate::runtime::model::http::{BodyFailure, encoding};
    encoding::decode_response(
        method,
        response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),
        follow,
    )
    .map_err(|_| failed())
}
