//! Pinned HTTP for Weixin attachment fetch/upload (SSRF hardening).
use super::http::failure;
use crate::config::routes::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    Method, Request, StatusCode,
    header::{HOST, LOCATION},
};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

#[cfg(test)]
static TEST_SETTINGS_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_settings_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_SETTINGS_LOCK.lock().unwrap()
}

#[cfg(test)]
static TEST_DNS_MAP: Mutex<Option<HashMap<String, Vec<IpAddr>>>> = Mutex::new(None);

#[cfg(test)]
static TEST_PERMIT_LOOPBACK_FETCH: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_test_dns_map(map: Option<HashMap<String, Vec<IpAddr>>>) {
    *TEST_DNS_MAP.lock().unwrap() = map;
}

#[cfg(test)]
pub(crate) fn set_test_permit_loopback_fetch(permit: bool) {
    TEST_PERMIT_LOOPBACK_FETCH.store(permit, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn test_permit_loopback_fetch() -> bool {
    TEST_PERMIT_LOOPBACK_FETCH.load(Ordering::SeqCst)
}

#[cfg(not(test))]
fn permit_loopback_fetch() -> bool {
    false
}

#[cfg(test)]
fn permit_loopback_fetch() -> bool {
    TEST_PERMIT_LOOPBACK_FETCH.load(Ordering::SeqCst)
}

const MAX_REDIRECTS: u32 = 5;

pub(crate) fn is_blocked_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_unspecified()
        || addr.is_broadcast()
        || addr.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
}

pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ipv4(mapped);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
        }
    }
}

fn host_is_local_name(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "localhost.localdomain"
    )
}

async fn resolve_host_ips(host: &str) -> Result<Vec<IpAddr>> {
    #[cfg(test)]
    if let Some(map) = TEST_DNS_MAP.lock().unwrap().as_ref() {
        if let Some(ips) = map.get(host) {
            return Ok(ips.clone());
        }
    }
    let mut ips = tokio::net::lookup_host(format!("{host}:0"))
        .await
        .map_err(|_| failure("Attachment URL host resolution failed"))?
        .map(|socket| socket.ip())
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return Err(failure("Attachment URL host resolution failed"));
    }
    ips.sort();
    ips.dedup();
    Ok(ips)
}

pub(crate) async fn validate_attachment_fetch_url(url: &str) -> Result<Url> {
    let (url, _) = validate_attachment_fetch_url_pinned(url).await?;
    Ok(url)
}

pub(crate) async fn validate_attachment_fetch_url_pinned(url: &str) -> Result<(Url, IpAddr)> {
    let parsed = Url::parse(url.trim()).map_err(|_| failure("Invalid attachment URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(failure("Attachment URL must be http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(failure("Attachment URL must not contain credentials"));
    }
    let Some(host) = parsed.host_str() else {
        return Err(failure("Attachment URL must include a host"));
    };
    if host_is_local_name(host) && !permit_loopback_fetch() {
        return Err(failure("Attachment URL host is not permitted"));
    }
    let pin = if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) && !permit_loopback_fetch() {
            return Err(failure("Attachment URL host is not permitted"));
        }
        ip
    } else {
        let ips = resolve_host_ips(host).await?;
        if !permit_loopback_fetch() && ips.iter().any(|ip| is_blocked_ip(*ip)) {
            return Err(failure("Attachment URL host is not permitted"));
        }
        ips.into_iter()
            .find(|ip| permit_loopback_fetch() || !is_blocked_ip(*ip))
            .ok_or_else(|| failure("Attachment URL host is not permitted"))?
    };
    Ok((parsed, pin))
}

#[derive(Clone)]
struct PinnedTarget {
    url: Url,
    pin: IpAddr,
}

impl PinnedTarget {
    async fn parse(url: &str) -> Result<Self> {
        let (url, pin) = validate_attachment_fetch_url_pinned(url).await?;
        Ok(Self { url, pin })
    }

    fn host(&self) -> &str {
        self.url.host_str().unwrap_or("")
    }

    fn port(&self) -> u16 {
        self.url
            .port()
            .unwrap_or(if self.url.scheme() == "https" { 443 } else { 80 })
    }
}

fn redirect_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn tls_connector() -> TlsConnector {
    TlsConnector::from(Arc::new(
        crate::runtime::model::http::webpki_client_config(),
    ))
}

async fn connect_tcp(target: &PinnedTarget) -> Result<TcpStream> {
    let addr = SocketAddr::new(target.pin, target.port());
    tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| failure("Attachment download transport failed"))?
        .map_err(|_| failure("Attachment download transport failed"))
}

async fn send_once(
    target: &PinnedTarget,
    method: &Method,
    body: Option<Bytes>,
) -> Result<hyper::Response<hyper::body::Incoming>> {
    let tcp = connect_tcp(target).await?;
    let request_body = match body {
        Some(bytes) => Full::new(bytes).map_err(|never| match never {}).boxed(),
        None => Empty::<Bytes>::new().map_err(|never| match never {}).boxed(),
    };
    let request = Request::builder()
        .method(method)
        .uri(target.url.as_str())
        .header(HOST, target.host())
        .body(request_body)
        .map_err(|_| failure("Invalid attachment request"))?;
    if target.url.scheme() == "https" {
        let server_name = ServerName::try_from(target.host().to_owned())
            .map_err(|_| failure("Invalid attachment URL"))?;
        let tls = tls_connector()
            .connect(server_name, tcp)
            .await
            .map_err(|_| failure("Attachment download transport failed"))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|_| failure("Attachment download transport failed"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        return sender
            .send_request(request)
            .await
            .map_err(|_| failure("Attachment download transport failed"));
    }
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .map_err(|_| failure("Attachment download transport failed"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender
        .send_request(request)
        .await
        .map_err(|_| failure("Attachment download transport failed"))
}

async fn read_body(incoming: hyper::body::Incoming, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut body = incoming;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| failure("Attachment download body failed"))?;
        if let Ok(bytes) = frame.into_data() {
            if data.len() + bytes.len() > limit {
                return Err(failure(&format!(
                    "Attachment is {} bytes (limit {limit}).",
                    data.len() + bytes.len()
                )));
            }
            data.extend_from_slice(&bytes);
        }
    }
    Ok(data)
}

pub(crate) struct PinnedHttpResponse {
    pub status: StatusCode,
    pub headers: hyper::HeaderMap,
    pub body: Vec<u8>,
}

pub(crate) async fn pinned_get(url: &str, limit: usize) -> Result<Vec<u8>> {
    let response = pinned_exchange(Method::GET, url, None, limit).await?;
    if !response.status.is_success() {
        return Err(failure("Attachment download HTTP error"));
    }
    Ok(response.body)
}

pub(crate) async fn pinned_post(url: &str, body: Bytes, limit: usize) -> Result<PinnedHttpResponse> {
    pinned_exchange(Method::POST, url, Some(body), limit).await
}

async fn pinned_exchange(
    method: Method,
    url: &str,
    body: Option<Bytes>,
    limit: usize,
) -> Result<PinnedHttpResponse> {
    let mut target = PinnedTarget::parse(url).await?;
    let mut method = method;
    let mut body = body;
    for _ in 0..=MAX_REDIRECTS {
        let response = send_once(&target, &method, body.take()).await?;
        let status = response.status();
        let headers = response.headers().clone();
        if redirect_status(status) {
            let Some(location) = headers.get(LOCATION).and_then(|value| value.to_str().ok()) else {
                return Err(failure("Attachment redirect is invalid"));
            };
            let next = target
                .url
                .join(location)
                .or_else(|_| Url::parse(location))
                .map_err(|_| failure("Attachment redirect is invalid"))?;
            target = PinnedTarget::parse(next.as_str()).await?;
            if status == StatusCode::SEE_OTHER {
                method = Method::GET;
            }
            let _ = read_body(response.into_body(), limit).await?;
            continue;
        }
        let data = read_body(response.into_body(), limit).await?;
        return Ok(PinnedHttpResponse {
            status,
            headers,
            body: data,
        });
    }
    Err(failure("Too many attachment redirects"))
}
