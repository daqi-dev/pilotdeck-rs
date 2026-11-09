use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, body::Incoming};
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
    pub async fn receive(&self, options: &SignalOptions) -> Result<Incoming> {
        let url = format!(
            "{}/v1/receive/{}",
            options.rest_url,
            encode_component(&options.account)
        );
        let request = Request::builder()
            .method("GET")
            .uri(url)
            .header("accept", "text/event-stream, application/json, */*")
            .body(Full::new(Bytes::new()))
            .map_err(|_| failure("Invalid Signal receive URL"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("Signal receive connection failed"))?;
        if !response.status().is_success() {
            return Err(failure("Signal receive HTTP rejected"));
        }
        Ok(response.into_body())
    }
    pub async fn send(&self, options: &SignalOptions, body: String) -> Result<()> {
        let request = Request::builder()
            .method("POST")
            .uri(format!("{}/v2/send", options.rest_url))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| failure("Invalid Signal send URL"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("Signal send connection failed"))?;
        if !response.status().is_success() {
            return Err(failure("Signal send HTTP rejected"));
        }
        Ok(())
    }
}
fn encode_component(input: &str) -> String {
    let mut result = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
