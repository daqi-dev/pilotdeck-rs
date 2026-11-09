//! Fixed TS Weixin outbound attachment preparation, AES-ECB CDN upload and ILink media send.
use super::*;
use crate::channel::im_attachment::{
    ImAttachmentDelivery, ImAttachmentDeliveryOptions, ImAttachmentDeliverySink,
    OutboundAttachment, PreparedFileType, PreparedImAttachment, format_im_attachment_fallback,
};
use crate::fetch_redirect::{MAX_STRING_LENGTH, follow, read_body};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use base64::Engine;
use bytes::Bytes;
use hyper::{HeaderMap, Method, header};
use md5::{Digest, Md5};
use pilotdeck_protocol::stringify_json;
fn string(value: &JsValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
}
fn attachment(value: &JsValue) -> OutboundAttachment {
    OutboundAttachment {
        kind: string(value, "type").map(|s| AttachmentKind::from(s.as_str())),
        name: string(value, "name"),
        path: string(value, "path"),
        mime_type: string(value, "mimeType"),
        content: string(value, "content"),
        bytes: value.get("bytes").and_then(JsValue::as_number),
        source: string(value, "source"),
        metadata: vec![],
    }
}
fn object(value: serde_json::Value) -> JsValue {
    parse_json(&value.to_string()).expect("native Weixin media object")
}
fn convert(error: WeixinClientError) -> AttachmentError {
    AttachmentError {
        name: error.name,
        message: error.message,
    }
}
pub(super) fn padded_size(size: usize) -> usize {
    (size / 16 + 1) * 16
}
pub(super) fn encrypt(buffer: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let padding = 16 - buffer.len() % 16;
    let mut data = buffer.to_vec();
    data.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for block in data.chunks_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    data
}
fn hex(buffer: &[u8]) -> String {
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}
/// Own only media I/O; cancelling this never aborts the shared A7 run.
#[derive(Default)]
pub(super) struct Lifetime {
    active: std::sync::atomic::AtomicUsize,
    drained: tokio::sync::Notify,
}
struct MediaGuard(Arc<Lifetime>);
impl Drop for MediaGuard {
    fn drop(&mut self) {
        if self
            .0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.0.drained.notify_waiters();
        }
    }
}
impl Lifetime {
    pub(super) async fn drain(&self) {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}
impl WeixinChannel {
    #[cfg(test)]
    pub(super) async fn send_attachment_with_cdn(
        &self,
        user: &JsString,
        value: &JsValue,
        cdn: &str,
    ) -> bool {
        self.send_attachment_impl(user, value, cdn).await
    }
    pub(super) async fn send_attachment(&self, user: &JsString, value: &JsValue) -> bool {
        self.send_attachment_impl(user, value, media::WEIXIN_CDN_BASE_URL)
            .await
    }
    async fn send_attachment_impl(&self, user: &JsString, value: &JsValue, cdn: &str) -> bool {
        let attachment = attachment(value);
        let (present, abort, _guard) = {
            let inner = self.lock_inner();
            if inner.abort.as_ref().is_some_and(Cancellation::is_aborted) {
                return false;
            }
            self.0
                .outbound
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                inner.client.is_some() && inner.context_tokens.contains_key(user),
                inner.abort.clone(),
                MediaGuard(self.0.outbound.clone()),
            )
        };
        let delivery = async {
            if !present {
                self.send_reply(
                    user,
                    &format_im_attachment_fallback(&attachment).into(),
                    true,
                )
                .await;
                return false;
            }
            let sink = Arc::new(Sink {
                channel: self.clone(),
                user: user.clone(),
                cdn: cdn.to_owned(),
            });
            let logger = self.clone();
            ImAttachmentDelivery::new(ImAttachmentDeliveryOptions {
                max_bytes: media::WEIXIN_ATTACHMENT_MAX_BYTES,
                sink,
                logger: Some(Arc::new(move |message| logger.log("error", message.into()))),
            })
            .send(&attachment)
            .await
        };
        if let Some(abort) = abort {
            tokio::select! {
                biased;
                _ = abort.cancelled() => false,
                sent = delivery => sent,
            }
        } else {
            delivery.await
        }
    }
}
struct Sink {
    channel: WeixinChannel,
    user: JsString,
    cdn: String,
}
impl ImAttachmentDeliverySink for Sink {
    fn send_prepared(
        &self,
        prepared: PreparedImAttachment,
    ) -> Pin<Box<dyn Future<Output = Result<(), AttachmentError>> + Send>> {
        let channel = self.channel.clone();
        let user = self.user.clone();
        let cdn = self.cdn.clone();
        Box::pin(async move {
            let (client, context) = {
                let inner = channel.lock_inner();
                (
                    inner.client.clone(),
                    inner.context_tokens.get(&user).cloned(),
                )
            };
            let client = client.ok_or_else(|| AttachmentError::error("weixin client missing"))?;
            let context =
                context.ok_or_else(|| AttachmentError::error("weixin context token missing"))?;
            let mut key = [0u8; 16];
            let mut filekey = [0u8; 16];
            let provider = rustls::crypto::ring::default_provider();
            provider
                .secure_random
                .fill(&mut key)
                .map_err(|_| AttachmentError::error("weixin secure random unavailable"))?;
            provider
                .secure_random
                .fill(&mut filekey)
                .map_err(|_| AttachmentError::error("weixin secure random unavailable"))?;
            channel.log(
                "info",
                format!(
                    "weixin: preparing attachment {} as {} bytes={}",
                    prepared.name,
                    if prepared.file_type == PreparedFileType::Image {
                        "image"
                    } else {
                        "file"
                    },
                    prepared.buffer.len()
                ),
            );
            upload_and_send(
                &*client,
                &user,
                &context,
                UploadPayload {
                    prepared,
                    key,
                    filekey: hex(&filekey),
                },
                &cdn,
                Some(Arc::new(move |message| channel.log("info", message))),
            )
            .await
        })
    }
    fn send_text_fallback(
        &self,
        text: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), AttachmentError>> + Send>> {
        let channel = self.channel.clone();
        let user = self.user.clone();
        Box::pin(async move {
            channel.send_reply(&user, &text.into(), true).await;
            Ok(())
        })
    }
}
/// One prepared media object and the per-upload encryption identity.
/// Keeping these together prevents mixing the key/filekey of concurrent sends.
struct UploadPayload {
    prepared: PreparedImAttachment,
    key: [u8; 16],
    filekey: String,
}
async fn upload_and_send(
    client: &dyn WeixinIlinkClient,
    user: &JsString,
    context: &JsString,
    payload: UploadPayload,
    cdn_base: &str,
    progress: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> Result<(), AttachmentError> {
    let UploadPayload {
        prepared,
        key,
        filekey,
    } = payload;
    let image = prepared.file_type == PreparedFileType::Image;
    let upload=client.get_upload_url(object(serde_json::json!({"filekey":filekey,"media_type":if image{1}else{3},"to_user_id":String::from_utf16_lossy(user.units()),"rawsize":prepared.buffer.len(),"rawfilemd5":format!("{:x}",Md5::digest(&prepared.buffer)),"filesize":padded_size(prepared.buffer.len()),"no_need_thumb":true,"aeskey":hex(&key)}))).await.map_err(convert)?;
    if let Some(log) = &progress {
        let mut summary = JsObject::new();
        for key in ["ret", "errmsg"] {
            if let Some(value) = upload.get(key).filter(|v| !matches!(v, JsValue::Undefined)) {
                summary.insert(key.into(), value.clone());
            }
        }
        for (label, key) in [
            ("hasUploadParam", "upload_param"),
            ("hasUploadFullUrl", "upload_full_url"),
            ("hasUploadUrl", "upload_url"),
            ("hasCdnUrl", "cdn_url"),
            ("hasDownloadUrl", "download_url"),
        ] {
            summary.insert(
                label.into(),
                JsValue::Bool(crate::channel::loader::js_truthy(upload.get(key))),
            );
        }
        log(format!(
            "weixin: upload url response for {}: {}",
            prepared.name,
            stringify_json(&JsValue::Object(summary))
                .ok()
                .flatten()
                .unwrap_or_default()
        ));
    }
    if let Some(ret) = upload
        .get("ret")
        .and_then(JsValue::as_number)
        .filter(|n| *n != 0.0)
    {
        let errmsg = upload
            .get("errmsg")
            .filter(|v| crate::channel::loader::js_truthy(Some(v)))
            .map(crate::channel::sms::js_string_of)
            .map(|s| format!(" errmsg={}", String::from_utf16_lossy(s.units())))
            .unwrap_or_default();
        return Err(AttachmentError::error(format!(
            "weixin getuploadurl failed ret={}{}",
            crate::js_value::number_string(ret),
            errmsg
        )));
    }
    let param = upload_encrypted(&prepared.buffer, &key, &filekey, &upload, cdn_base).await?;
    if let Some(log) = &progress {
        log(format!(
            "weixin: CDN upload succeeded for {}",
            prepared.name
        ));
    }
    let media = serde_json::json!({"encrypt_query_param":param,"aes_key":base64::engine::general_purpose::STANDARD.encode(hex(&key)),"encrypt_type":1});
    let item = if image {
        serde_json::json!({"type":client::MESSAGE_ITEM_TYPE_IMAGE,"image_item":{"media":media,"mid_size":padded_size(prepared.buffer.len())}})
    } else {
        serde_json::json!({"type":client::MESSAGE_ITEM_TYPE_FILE,"file_item":{"file_name":prepared.name,"len":prepared.buffer.len().to_string(),"media":media}})
    };
    client
        .send_media(user, object(item), context)
        .await
        .map_err(convert)?;
    if let Some(log) = &progress {
        log(format!(
            "weixin: sent attachment to {}: {}",
            String::from_utf16_lossy(user.units()),
            prepared.name
        ));
    }
    Ok(())
}
pub(super) async fn upload_encrypted(
    buffer: &[u8],
    key: &[u8; 16],
    filekey: &str,
    upload: &JsValue,
    cdn_base: &str,
) -> Result<String, AttachmentError> {
    let full = match upload.get("upload_full_url") {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        Some(JsValue::String(s)) => {
            let s = String::from_utf16_lossy(s.units());
            let value = crate::metadata::js_trim(&s).to_owned();
            if value.is_empty() { None } else { Some(value) }
        }
        Some(_) => {
            return Err(AttachmentError {
                name: "TypeError".into(),
                message: "upload.upload_full_url?.trim is not a function".into(),
            });
        }
    };
    let url = full
        .or_else(|| {
            upload
                .get("upload_param")
                .filter(|v| crate::channel::loader::js_truthy(Some(v)))
                .map(crate::channel::sms::js_string_of)
                .map(|p| {
                    format!(
                        "{}/upload?encrypted_query_param={}&filekey={}",
                        cdn_base.trim_end_matches('/'),
                        client::uri_encode_component(&String::from_utf16_lossy(p.units())),
                        client::uri_encode_component(filekey)
                    )
                })
        })
        .ok_or_else(|| {
            AttachmentError::error(
                "weixin upload url response missing upload_full_url/upload_param",
            )
        })?;
    let url = url::Url::parse(&url).map_err(|_| AttachmentError {
        name: "TypeError".into(),
        message: "Failed to parse URL".into(),
    })?;
    let config = crate::runtime::model::http::webpki_client_config();
    let http = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(crate::runtime::model::http::https_connector(config));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    let response = tokio::time::timeout(
        crate::channel::http_client::UNDICI_DEFAULT_TIMEOUT,
        follow(
            url,
            Method::POST,
            headers,
            Bytes::from(encrypt(buffer, key)),
            WeixinClientError::fetch_failed,
            |request| async {
                http.request(request)
                    .await
                    .map_err(|_| WeixinClientError::fetch_failed())
            },
        ),
    )
    .await
    .map_err(|_| convert(WeixinClientError::fetch_failed()))?
    .map_err(convert)?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    if status != 200 {
        let text = if let Some(error) = headers.get("x-error-message") {
            String::from_utf8_lossy(error.as_bytes()).into_owned()
        } else {
            let bytes = read_body(
                response.into_body(),
                MAX_STRING_LENGTH,
                Some(crate::channel::http_client::UNDICI_DEFAULT_TIMEOUT),
            )
            .await
            .map_err(|_| AttachmentError::error("terminated"))?;
            String::from_utf8_lossy(&bytes).into_owned()
        };
        return Err(AttachmentError::error(format!(
            "weixin CDN upload HTTP {status}: {text}"
        )));
    }
    // The original fetch reads only this response header. Dropping its unused body
    // closes the native response lease without downloading unrelated bytes.
    headers
        .get("x-encrypted-param")
        .map(|h| String::from_utf8_lossy(h.as_bytes()).into_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AttachmentError::error("weixin CDN upload response missing x-encrypted-param")
        })
}
