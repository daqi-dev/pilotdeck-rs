use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
pub(super) struct Api {
    client: Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
}
impl Api {
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
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        authorization: Option<&str>,
        body: Option<String>,
    ) -> Result<Value> {
        let mut request = Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json");
        if let Some(auth) = authorization {
            request = request.header("authorization", auth);
        }
        let request = request
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|_| failure("Invalid QQ HTTP request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("QQ HTTP connection failed"))?;
        let success = response.status().is_success();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| failure("QQ HTTP response failed"))?
            .to_bytes();
        if !success {
            return Err(failure("QQ HTTP request rejected"));
        }
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("Invalid QQ HTTP JSON"))
    }
}
