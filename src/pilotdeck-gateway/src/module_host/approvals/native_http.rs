//! Bounded HTTP exchange shared by the native StaffDeck SOP ports. No retry or
//! credential-bearing redirect is implicit. Dropping the future owns/cancels IO.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, header};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) type JsonFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonResponse, String>> + Send + 'a>>;
pub(crate) struct JsonRequest {
    pub method: &'static str,
    pub url: url::Url,
    pub bearer: Option<String>,
    pub body: Option<String>,
    pub parse_error_body: bool,
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
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>,
    limit: usize,
}
impl Default for HyperJsonTransport {
    fn default() -> Self {
        let config = crate::runtime::model::http::webpki_client_config();
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config)),
            limit: 8 * 1024 * 1024,
        }
    }
}
impl JsonTransport for HyperJsonTransport {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        Box::pin(async move {
            if !matches!(input.url.scheme(), "http" | "https")
                || !input.url.username().is_empty()
                || input.url.password().is_some()
            {
                return Err("fetch failed".into());
            }
            let mut builder = Request::builder()
                .method(input.method)
                .uri(input.url.as_str())
                .header(header::ACCEPT, "application/json");
            if let Some(bearer) = &input.bearer {
                builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            if input.body.is_some() {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
            }
            let request = builder
                .body(Full::new(Bytes::from(input.body.unwrap_or_default())))
                .map_err(|_| "fetch failed".to_owned())?;
            let exchange = async {
                let response = self
                    .client
                    .request(request)
                    .await
                    .map_err(|_| "fetch failed".to_owned())?;
                let status = response.status().as_u16();
                // The fixed approval authority uses redirect:'error'. Runtime
                // endpoints likewise never forward request payloads to a new owner.
                if (300..400).contains(&status) {
                    return Err("fetch failed".to_owned());
                }
                if !(200..300).contains(&status) && !input.parse_error_body {
                    return Ok(JsonResponse {
                        status,
                        body: JsValue::Undefined,
                    });
                }
                let raw = crate::fetch_redirect::read_body(response.into_body(), self.limit, None)
                    .await
                    .map_err(|_| "StaffDeck response body exceeded its read budget.".to_owned())?;
                let body = parse_json(&String::from_utf8_lossy(&raw))
                    .map_err(|_| "StaffDeck response is not valid JSON.".to_owned())?;
                Ok(JsonResponse { status, body })
            };
            tokio::time::timeout(input.timeout, exchange)
                .await
                .map_err(|_| "StaffDeck request timed out.".to_owned())?
        })
    }
}
