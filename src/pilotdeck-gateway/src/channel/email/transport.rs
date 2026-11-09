//! Real asynchronous IMAP and SMTP consumers; tests use loopback servers only.
use futures_util::TryStreamExt;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, rustls::pki_types::ServerName};
pub trait MailIo: AsyncRead + AsyncWrite + Send + Unpin + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + std::fmt::Debug> MailIo for T {}
pub type Imap = async_imap::Session<Box<dyn MailIo>>;
pub type Smtp = AsyncSmtpTransport<Tokio1Executor>;
pub struct EmailConfig {
    pub address: String,
    pub password: String,
    pub imap_host: String,
    pub smtp_host: String,
    pub imap_port: u16,
    pub smtp_port: u16,
    pub imap_tls: bool,
    pub smtp_tls: bool,
    pub poll_ms: u64,
    pub subject: String,
}
impl EmailConfig {
    pub fn from_options(o: &JsObject, env: Option<&super::super::sms::EnvLookup>) -> Self {
        let get = |k: &str, e: &str, default: JsValue| {
            o.get(k)
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .cloned()
                .or_else(|| env.and_then(|get| get(e)).map(JsValue::String))
                .or_else(|| {
                    if env.is_none() && !e.is_empty() {
                        std::env::var(e).ok().map(|s| JsValue::String(s.into()))
                    } else {
                        None
                    }
                })
                .unwrap_or(default)
        };
        let text =
            |v: JsValue| String::from_utf16_lossy(super::super::sms::js_string_of(&v).units());
        let num = |v: JsValue| match v {
            JsValue::Number(n) => n,
            JsValue::Bool(v) => {
                if v {
                    1.0
                } else {
                    0.0
                }
            }
            JsValue::String(ref v) => {
                let s = String::from_utf16_lossy(v.units());
                if s.trim().is_empty() {
                    0.0
                } else {
                    s.trim().parse().unwrap_or(f64::NAN)
                }
            }
            _ => f64::NAN,
        };
        let port = |n: f64| {
            if n.is_finite() && n >= 0.0 && n <= 65535.0 && n.fract() == 0.0 {
                n as u16
            } else {
                0
            }
        };
        let poll = num(get("pollIntervalMs", "", JsValue::Number(45000.0)));
        let poll = if poll.is_finite() && (1.0..=2147483647.0).contains(&poll) {
            poll.floor() as u64
        } else {
            1
        };
        Self {
            address: text(get("address", "EMAIL_ADDRESS", "".into())),
            password: text(get("password", "EMAIL_PASSWORD", "".into())),
            imap_host: text(get("imapHost", "IMAP_HOST", "".into())),
            smtp_host: text(get("smtpHost", "SMTP_HOST", "".into())),
            imap_port: port(num(get("imapPort", "IMAP_PORT", JsValue::Number(993.0)))),
            smtp_port: port(num(get("smtpPort", "SMTP_PORT", JsValue::Number(587.0)))),
            imap_tls: o.get("imapTls") != Some(&JsValue::Bool(false)),
            smtp_tls: o.get("smtpTls") != Some(&JsValue::Bool(false)),
            poll_ms: poll,
            subject: text(get("defaultSubject", "", "Message".into())),
        }
    }
    pub fn complete(&self) -> bool {
        !self.address.is_empty()
            && !self.password.is_empty()
            && !self.imap_host.is_empty()
            && !self.smtp_host.is_empty()
    }
}
pub async fn connect_imap(c: &EmailConfig) -> Result<Imap, String> {
    let tcp = TcpStream::connect((c.imap_host.as_str(), c.imap_port))
        .await
        .map_err(|e| e.to_string())?;
    let io: Box<dyn MailIo> = if c.imap_tls {
        let name = ServerName::try_from(c.imap_host.clone()).map_err(|e| e.to_string())?;
        let tls = TlsConnector::from(Arc::new(crate::runtime::model::http::webpki_client_config()))
            .connect(name, tcp)
            .await
            .map_err(|e| e.to_string())?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    let mut client = async_imap::Client::new(io);
    client
        .read_response()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "IMAP greeting missing".to_owned())?;
    let mut session = client
        .login(&c.address, &c.password)
        .await
        .map_err(|(e, _)| e.to_string())?;
    session.select("INBOX").await.map_err(|e| e.to_string())?;
    Ok(session)
}
pub async fn prime(imap: &mut Imap) -> Result<Vec<u32>, String> {
    let status = imap
        .status("INBOX", "(UIDNEXT)")
        .await
        .map_err(|e| e.to_string())?;
    let next = status.uid_next.unwrap_or(1);
    if next <= 1 {
        return Ok(vec![]);
    }
    let range = format!("{}:{}", next.saturating_sub(300).max(1), next - 1);
    let rows: Vec<_> = imap
        .fetch(&range, "UID")
        .await
        .map_err(|e| e.to_string())?
        .try_collect()
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().filter_map(|r| r.uid).collect())
}
pub struct Incoming {
    pub uid: u32,
    pub from: String,
    pub raw: String,
}
pub async fn unseen(imap: &mut Imap) -> Result<Vec<Incoming>, String> {
    let ids = imap.search("UNSEEN").await.map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort_unstable();
    let range = ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fetched: Vec<_> = imap
        .fetch(&range, "(UID ENVELOPE BODY.PEEK[])")
        .await
        .map_err(|e| e.to_string())?
        .try_collect()
        .await
        .map_err(|e| e.to_string())?;
    Ok(fetched
        .into_iter()
        .map(|m| {
            let from = m
                .envelope()
                .and_then(|e| e.from.as_ref())
                .and_then(|a| a.first())
                .and_then(|a| {
                    Some(format!(
                        "{}@{}",
                        String::from_utf8_lossy(a.mailbox.as_deref()?),
                        String::from_utf8_lossy(a.host.as_deref()?)
                    ))
                })
                .unwrap_or_else(|| "unknown".into());
            Incoming {
                uid: m.uid.unwrap_or(0),
                from,
                raw: String::from_utf8_lossy(m.body().unwrap_or(&[])).into_owned(),
            }
        })
        .collect())
}
pub async fn connect_smtp(c: &EmailConfig) -> Result<Smtp, String> {
    let tls = TlsParameters::new(c.smtp_host.clone()).map_err(|e| e.to_string())?;
    let tls = if c.smtp_port == 465 {
        Tls::Wrapper(tls)
    } else if c.smtp_tls {
        Tls::Required(tls)
    } else {
        Tls::Opportunistic(tls)
    };
    let smtp = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&c.smtp_host)
        .port(c.smtp_port)
        .credentials(Credentials::new(c.address.clone(), c.password.clone()))
        .tls(tls)
        .build();
    if !smtp.test_connection().await.map_err(|e| e.to_string())? {
        return Err("SMTP verification failed".into());
    }
    Ok(smtp)
}
pub async fn send(
    smtp: &Smtp,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> Result<(), String> {
    let mail = Message::builder()
        .from(
            from.parse()
                .map_err(|e: lettre::address::AddressError| e.to_string())?,
        )
        .to(to
            .parse()
            .map_err(|e: lettre::address::AddressError| e.to_string())?)
        .subject(subject)
        .body(text.to_owned())
        .map_err(|e| e.to_string())?;
    smtp.send(mail).await.map_err(|e| e.to_string())?;
    Ok(())
}
