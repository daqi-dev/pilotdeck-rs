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
    pub async fn post(&self, url: &str, body: Value, parse_json: bool) -> Result<Value> {
        let request = Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .map_err(|_| failure("Invalid DingTalk request"))?;
        let send = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("DingTalk HTTP connection failed"))?;
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| failure("DingTalk HTTP response failed"))?
                .to_bytes();
            if !status.is_success() {
                return Err(failure("DingTalk HTTP request rejected"));
            }
            if !parse_json {
                return Ok(Value::Null);
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| failure("Invalid DingTalk endpoint response"))
        };
        tokio::time::timeout(Duration::from_secs(10), send)
            .await
            .map_err(|_| failure("DingTalk HTTP timed out"))?
    }
}
