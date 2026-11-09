//! Shared legacy discovery contract for CLI adapters.
use std::time::Duration;
use tokio::time::timeout;

pub(crate) async fn healthy() -> bool {
    healthy_at("http://127.0.0.1:18789").await
}

pub(crate) async fn healthy_at(base: &str) -> bool {
    timeout(Duration::from_millis(200), async {
        // Use the same HTTP/TLS stack already in the Gateway, including redirects.
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let client: hyper_util::client::legacy::Client<_, http_body_util::Empty<bytes::Bytes>> =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);
        let mut url = url::Url::parse(&format!("{base}/health")).ok()?;
        for redirects in 0..=20 {
            let uri = url.as_str().parse::<hyper::Uri>().ok()?;
            let response = client.get(uri).await.ok()?;
            let status = response.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if redirects == 20 {
                    return None;
                }
                let location = response
                    .headers()
                    .get(hyper::header::LOCATION)?
                    .to_str()
                    .ok()?;
                url = url.join(location).ok()?;
                if !matches!(url.scheme(), "http" | "https") {
                    return None;
                }
                continue;
            }
            return Some(status.is_success());
        }
        None
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}
