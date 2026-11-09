use super::*;
use base64::Engine;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use sha1::{Digest, Sha1};
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
    pub async fn send(&self, options: &SmsOptions, to: &str, text: &str) -> Result<()> {
        let sid = url::form_urlencoded::byte_serialize(options.account_sid.as_bytes())
            .collect::<String>();
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("Body", text)
            .append_pair("From", &options.phone_number)
            .append_pair("To", to)
            .finish();
        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", options.account_sid, options.auth_token));
        let request = Request::builder()
            .method("POST")
            .uri(format!(
                "{}/Accounts/{sid}/Messages.json",
                options.api_base.trim_end_matches('/')
            ))
            .header("authorization", format!("Basic {auth}"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Full::new(Bytes::from(form)))
            .map_err(|_| failure("Invalid Twilio request"))?;
        let send = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| failure("Twilio connection failed"))?;
            let status = response.status();
            let _ = response
                .into_body()
                .collect()
                .await
                .map_err(|_| failure("Twilio response failed"))?;
            if !status.is_success() {
                return Err(failure("Twilio rejected SMS"));
            }
            Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), send)
            .await
            .map_err(|_| failure("Twilio send timed out"))?
    }
}
pub(super) fn signature(url: &str, params: &HashMap<String, String>, token: &str) -> String {
    let mut keys = params.keys().collect::<Vec<_>>();
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    let mut data = url.to_owned();
    for key in keys {
        data.push_str(key);
        data.push_str(&params[key]);
    }
    let mut key = [0u8; 64];
    if token.len() > 64 {
        key[..20].copy_from_slice(&Sha1::digest(token.as_bytes()));
    } else {
        key[..token.len()].copy_from_slice(token.as_bytes());
    }
    let inner = key.map(|byte| byte ^ 0x36);
    let outer = key.map(|byte| byte ^ 0x5c);
    let mut digest = Sha1::new();
    digest.update(inner);
    digest.update(data.as_bytes());
    let hash = digest.finalize();
    let mut digest = Sha1::new();
    digest.update(outer);
    digest.update(hash);
    base64::engine::general_purpose::STANDARD.encode(digest.finalize())
}
pub(super) fn valid_signature(
    url: &str,
    params: &HashMap<String, String>,
    token: &str,
    sig: &str,
) -> bool {
    let expected = signature(url, params, token);
    expected.len() == sig.len()
        && expected
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            == 0
}
