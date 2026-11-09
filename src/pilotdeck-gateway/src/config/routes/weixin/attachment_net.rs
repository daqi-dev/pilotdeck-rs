//! Pinned HTTP for Weixin attachment fetch/upload (SSRF hardening).
use super::http::failure;
use crate::config::routes::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{
    Method, Request, StatusCode,
    header::{CONTENT_TYPE, HOST, LOCATION},
};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

#[cfg(test)]
static TEST_SETTINGS_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_settings_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_SETTINGS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
static TEST_DNS_MAP: Mutex<Option<HashMap<String, Vec<IpAddr>>>> = Mutex::new(None);

#[cfg(test)]
static TEST_PERMIT_LOOPBACK_FETCH: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_test_dns_map(map: Option<HashMap<String, Vec<IpAddr>>>) {
    *TEST_DNS_MAP.lock().unwrap() = map;
    dns_cache().lock().unwrap().clear();
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
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);
const DNS_CACHE_TTL: Duration = Duration::from_secs(120);
const MAX_IDLE_CONNECTIONS: usize = 32;
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;
type HttpSender = hyper::client::conn::http1::SendRequest<BoxBody>;

static TLS_CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();
static DNS_CACHE: OnceLock<Mutex<HashMap<String, (IpAddr, Instant)>>> = OnceLock::new();
static CONNECTION_POOL: Mutex<Vec<(PoolKey, HttpSender, Instant)>> = Mutex::new(Vec::new());

fn dns_cache() -> &'static Mutex<HashMap<String, (IpAddr, Instant)>> {
    DNS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Eq)]
struct PoolKey {
    pin: IpAddr,
    port: u16,
    https: bool,
    authority: String,
}

impl PartialEq for PoolKey {
    fn eq(&self, other: &Self) -> bool {
        self.pin == other.pin
            && self.port == other.port
            && self.https == other.https
            && self.authority == other.authority
    }
}

impl Hash for PoolKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pin.hash(state);
        self.port.hash(state);
        self.https.hash(state);
        self.authority.hash(state);
    }
}

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
        || (240..=254).contains(&octets[0])
        || octets[0] == 255
}

fn ipv4_compatible(v6: std::net::Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = v6.segments();
    if segments[..6].iter().all(|segment| *segment == 0) {
        Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            (segments[6] & 0xff) as u8,
            (segments[7] >> 8) as u8,
            (segments[7] & 0xff) as u8,
        ))
    } else {
        None
    }
}

fn is_blocked_ipv6(v6: std::net::Ipv6Addr) -> bool {
    if let Some(mapped) = v6.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    if let Some(compat) = ipv4_compatible(v6) {
        return is_blocked_ipv4(compat);
    }
    let segments = v6.segments();
    if (segments[0] & 0xffc0) == 0xfec0 {
        return true;
    }
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_unique_local()
        || v6.is_unicast_link_local()
        || v6.is_multicast()
}

pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
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

fn http_authority(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    let port = url
        .port()
        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
    let default = if url.scheme() == "https" { 443 } else { 80 };
    if port == default {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    }
}

fn purge_expired_dns(cache: &mut std::collections::HashMap<String, (IpAddr, Instant)>) {
    cache.retain(|_, (_, saved)| saved.elapsed() < DNS_CACHE_TTL);
}

async fn pin_for_host(host: &str) -> Result<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    {
        let mut cache = dns_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        purge_expired_dns(&mut cache);
        if let Some((ip, saved)) = cache.get(host) {
            if saved.elapsed() < DNS_CACHE_TTL {
                return Ok(*ip);
            }
            cache.remove(host);
        }
    }
    let ips = resolve_host_ips(host).await?;
    if !permit_loopback_fetch() && ips.iter().any(|ip| is_blocked_ip(*ip)) {
        return Err(failure("Attachment URL host is not permitted"));
    }
    let pin = ips
        .into_iter()
        .find(|ip| permit_loopback_fetch() || !is_blocked_ip(*ip))
        .ok_or_else(|| failure("Attachment URL host is not permitted"))?;
    dns_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(host.into(), (pin, Instant::now()));
    Ok(pin)
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
        pin_for_host(host).await?
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

    fn authority(&self) -> String {
        http_authority(&self.url)
    }

    fn pool_key(&self) -> PoolKey {
        PoolKey {
            pin: self.pin,
            port: self.port(),
            https: self.url.scheme() == "https",
            authority: self.authority(),
        }
    }
}

#[cfg(test)]
pub(crate) fn authority_for_url(url: &str) -> Result<String> {
    let parsed = Url::parse(url).map_err(|_| failure("Invalid attachment URL"))?;
    if parsed.host_str().is_none() {
        return Err(failure("Attachment URL must include a host"));
    }
    Ok(http_authority(&parsed))
}

fn redirect_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn tls_connector() -> &'static TlsConnector {
    TLS_CONNECTOR.get_or_init(|| {
        TlsConnector::from(Arc::new(
            crate::runtime::model::http::webpki_client_config(),
        ))
    })
}

fn empty_body() -> BoxBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn body_from_bytes(bytes: Bytes) -> BoxBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed()
}

async fn connect_tcp(target: &PinnedTarget) -> Result<TcpStream> {
    let addr = SocketAddr::new(target.pin, target.port());
    tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| failure("Attachment download transport failed"))?
        .map_err(|_| failure("Attachment download transport failed"))
}

async fn connect_sender(target: &PinnedTarget) -> Result<HttpSender> {
    let tcp = connect_tcp(target).await?;
    if target.url.scheme() == "https" {
        let server_name = ServerName::try_from(target.host().to_owned())
            .map_err(|_| failure("Invalid attachment URL"))?;
        let tls = tls_connector()
            .connect(server_name, tcp)
            .await
            .map_err(|_| failure("Attachment download transport failed"))?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|_| failure("Attachment download transport failed"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        return Ok(sender);
    }
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .map_err(|_| failure("Attachment download transport failed"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(sender)
}

fn pool_take(key: &PoolKey) -> Option<HttpSender> {
    let mut pool = CONNECTION_POOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pool.retain(|(_, _, idle_since)| idle_since.elapsed() < POOL_IDLE_TIMEOUT);
    if let Some(index) = pool.iter().position(|(entry, _, _)| entry == key) {
        return Some(pool.swap_remove(index).1);
    }
    None
}

fn pool_store(key: PoolKey, sender: HttpSender) {
    let mut pool = CONNECTION_POOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pool.retain(|(_, _, idle_since)| idle_since.elapsed() < POOL_IDLE_TIMEOUT);
    while pool.len() >= MAX_IDLE_CONNECTIONS {
        let evict = pool
            .iter()
            .enumerate()
            .min_by_key(|(_, (_, _, idle_since))| *idle_since)
            .map(|(index, _)| index);
        let Some(index) = evict else {
            return;
        };
        pool.swap_remove(index);
    }
    pool.push((key, sender, Instant::now()));
}

async fn acquire_sender(target: &PinnedTarget) -> Result<HttpSender> {
    let key = target.pool_key();
    if let Some(mut sender) = pool_take(&key) {
        if sender.ready().await.is_ok() {
            return Ok(sender);
        }
    }
    connect_sender(target).await
}

async fn release_sender(key: PoolKey, mut sender: HttpSender) {
    if sender.ready().await.is_ok() {
        pool_store(key, sender);
    }
}

async fn send_on_sender(
    sender: &mut HttpSender,
    target: &PinnedTarget,
    method: &Method,
    body: Option<Bytes>,
) -> Result<hyper::Response<hyper::body::Incoming>> {
    let request_body = match &body {
        Some(bytes) => body_from_bytes(bytes.clone()),
        None => empty_body(),
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(target.url.as_str())
        .header(HOST, target.authority());
    if *method == Method::POST {
        builder = builder.header(CONTENT_TYPE, "application/octet-stream");
    }
    let request = builder
        .body(request_body)
        .map_err(|_| failure("Invalid attachment request"))?;
    sender
        .send_request(request)
        .await
        .map_err(|_| failure("Attachment download transport failed"))
}

async fn send_once(
    target: &PinnedTarget,
    method: &Method,
    body: Option<Bytes>,
) -> Result<(hyper::Response<hyper::body::Incoming>, PoolKey, HttpSender)> {
    let key = target.pool_key();
    let mut sender = acquire_sender(target).await?;
    let response = send_on_sender(&mut sender, target, method, body).await?;
    Ok((response, key, sender))
}

async fn read_body(incoming: hyper::body::Incoming, limit: usize) -> Result<Vec<u8>> {
    let read = async {
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
    };
    tokio::time::timeout(BODY_READ_TIMEOUT, read)
        .await
        .map_err(|_| failure("Attachment download body timed out"))?
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
    let post_body = body.clone();
    for _ in 0..=MAX_REDIRECTS {
        let (response, pool_key, mut sender) = send_once(&target, &method, body.take()).await?;
        let status = response.status();
        let headers = response.headers().clone();
        if redirect_status(status) {
            let Some(location) = headers.get(LOCATION).and_then(|value| value.to_str().ok()) else {
                let _ = read_body(response.into_body(), limit).await;
                return Err(failure("Attachment redirect is invalid"));
            };
            let next = target
                .url
                .join(location)
                .or_else(|_| Url::parse(location))
                .map_err(|_| failure("Attachment redirect is invalid"))?;
            target = PinnedTarget::parse(next.as_str()).await?;
            match status {
                StatusCode::SEE_OTHER | StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                    method = Method::GET;
                    body = None;
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                    if method == Method::POST {
                        body = post_body.clone();
                    } else {
                        body = None;
                    }
                }
                _ => body = None,
            }
            let _ = read_body(response.into_body(), limit).await?;
            if sender.ready().await.is_ok() {
                release_sender(pool_key, sender).await;
            }
            continue;
        }
        let data = read_body(response.into_body(), limit).await?;
        if sender.ready().await.is_ok() {
            release_sender(pool_key, sender).await;
        }
        return Ok(PinnedHttpResponse {
            status,
            headers,
            body: data,
        });
    }
    Err(failure("Too many attachment redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn blocks_ipv4_compatible_and_fec0() {
        let compat: IpAddr = "::127.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(compat));
        let fec0: IpAddr = "fec0::1".parse().unwrap();
        assert!(is_blocked_ip(fec0));
        assert!(is_blocked_ipv4(Ipv4Addr::new(240, 0, 0, 1)));
        assert!(is_blocked_ipv4(Ipv4Addr::new(255, 1, 2, 3)));
        assert!(!is_blocked_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn authority_includes_non_default_port() {
        assert_eq!(
            authority_for_url("http://cdn.example:8080/path").unwrap(),
            "cdn.example:8080"
        );
        assert_eq!(
            authority_for_url("https://cdn.example/path").unwrap(),
            "cdn.example"
        );
    }

    #[tokio::test]
    async fn pinned_get_sends_http_host_matching_authority() {
        let _lock = test_settings_lock();
        set_test_permit_loopback_fetch(true);
        let captured = Arc::new(Mutex::new(String::new()));
        let seen = captured.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/body.bin");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            *seen.lock().unwrap() = String::from_utf8_lossy(&bytes).into_owned();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nhi\n")
                .await
                .unwrap();
        });
        let body = pinned_get(&url, 64).await.unwrap();
        assert_eq!(body, b"hi\n");
        let header = captured.lock().unwrap().clone();
        assert!(header.to_lowercase().contains(&format!("host: {addr}")));
        set_test_permit_loopback_fetch(false);
    }
}
