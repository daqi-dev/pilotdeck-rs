use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
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
    pub fn validate(&self, options: &BlueBubblesOptions) -> Result<url::Url> {
        let url = url::Url::parse(&options.server_url)
            .map_err(|_| failure("Invalid BlueBubbles server URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(failure("Invalid BlueBubbles server URL"));
        }
        Ok(url)
    }
    pub async fn poll(&self, options: &BlueBubblesOptions, after: f64) -> Result<Value> {
        let mut url = self
            .validate(options)?
            .join("/api/v1/message")
            .map_err(|_| failure("Invalid BlueBubbles URL"))?;
        url.query_pairs_mut()
            .append_pair("password", &options.password)
            .append_pair("after", &after.to_string())
            .append_pair("limit", "50");
        self.request(options, url, None).await
    }
    pub async fn send(&self, options: &BlueBubblesOptions, body: Value) -> Result<()> {
        let url = self
            .validate(options)?
            .join("/api/v1/message/text")
            .map_err(|_| failure("Invalid BlueBubbles URL"))?;
        let request = self.request(options, url, Some(body));
        tokio::time::timeout(Duration::from_secs(120), request)
            .await
            .map_err(|_| failure("BlueBubbles send timed out"))??;
        Ok(())
    }
    async fn request(
        &self,
        options: &BlueBubblesOptions,
        url: url::Url,
        body: Option<Value>,
    ) -> Result<Value> {
        let sending = body.is_some();
        let mut request = hyper::Request::builder()
            .method(if sending { "POST" } else { "GET" })
            .uri(url.as_str());
        if sending {
            request = request
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", options.password));
        }
        let request = request
            .body(Full::new(Bytes::from(
                body.map(|body| body.to_string()).unwrap_or_default(),
            )))
            .map_err(|_| failure("Invalid BlueBubbles request"))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| failure("BlueBubbles HTTP transport failed"))?;
        if !response.status().is_success() {
            return Err(failure("BlueBubbles HTTP request rejected"));
        }
        if sending {
            return Ok(Value::Null);
        }
        let mut bytes = Vec::new();
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| failure("BlueBubbles body interrupted"))?;
            if let Some(data) = frame.data_ref() {
                if data.len() > 4_194_304usize.saturating_sub(bytes.len()) {
                    return Err(failure("BlueBubbles response too large"));
                }
                bytes.extend_from_slice(data);
            }
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("Invalid BlueBubbles JSON"))
    }
}
