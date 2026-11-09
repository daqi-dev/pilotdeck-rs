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
        options: &WhatsAppOptions,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Vec<u8>)> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", options.bridge_url))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(
                body.map(|v| v.to_string()).unwrap_or_default(),
            )))
            .map_err(|_| failure("Invalid WhatsApp bridge request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("WhatsApp bridge connection failed"))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|_| failure("WhatsApp bridge response failed"))?
            .to_bytes()
            .to_vec();
        Ok((status, body))
    }
}
