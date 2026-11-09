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
    pub async fn rest(
        &self,
        options: &MattermostOptions,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", options.server_url))
            .header("authorization", format!("Bearer {}", options.token))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            )))
            .map_err(|_| failure("Invalid Mattermost HTTP request"))?;
        let request = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("Mattermost HTTP connection failed"))?;
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|_| failure("Mattermost HTTP response failed"))?
                .to_bytes();
            if !status.is_success() {
                return Err(failure("Mattermost HTTP request rejected"));
            }
            if body.is_empty() {
                return Ok(json!({}));
            }
            Ok(serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned())))
        };
        tokio::time::timeout(Duration::from_secs(20), request)
            .await
            .map_err(|_| failure("Mattermost HTTP request timed out"))?
    }
}
