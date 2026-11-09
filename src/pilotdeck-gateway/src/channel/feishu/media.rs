//! Fixed cc7c4e6 Feishu inbound resources and outbound multipart attachments.
use super::{Runtime, client::ApiClient, parse::InboundMessage};
use crate::channel::im_attachment::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsObject, JsValue, parse_json};
use std::{sync::Arc, time::Duration};
struct Fetcher;
impl AttachmentFetcher for Fetcher {
    fn fetch<'a>(&'a self, url: &'a str, headers: &'a HeaderMap) -> AttachmentFetchFuture<'a> {
        Box::pin(async move {
            let (status, header, bytes) = exchange("GET", url, headers.clone(), vec![]).await?;
            if !(200..300).contains(&status) {
                return Err(AttachmentError::error(format!("HTTP {status}")));
            }
            Ok(FetchedAttachment {
                bytes,
                content_type: header
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            })
        })
    }
}
async fn exchange(
    method: &str,
    url: &str,
    mut headers: HeaderMap,
    data: Vec<u8>,
) -> Result<(u16, HeaderMap, Vec<u8>), AttachmentError> {
    tokio::time::timeout(Duration::from_secs(60), async {
        let config = crate::runtime::model::http::webpki_client_config();
        let client = Client::builder(TokioExecutor::new())
            .build::<_, Full<Bytes>>(crate::runtime::model::http::https_connector(config));
        let mut url = url::Url::parse(url).map_err(|e| AttachmentError::error(e.to_string()))?;
        for redirects in 0..=20 {
            let mut request = Request::builder()
                .method(method)
                .uri(url.as_str())
                .body(Full::new(Bytes::from(data.clone())))
                .map_err(|e| AttachmentError::error(e.to_string()))?;
            *request.headers_mut() = headers.clone();
            let response = client
                .request(request)
                .await
                .map_err(|e| AttachmentError::error(e.to_string()))?;
            if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) && method == "GET"
            {
                if let Some(location) = response.headers().get("location") {
                    if redirects == 20 {
                        return Err(AttachmentError::error("redirect count exceeded"));
                    }
                    let next = url
                        .join(
                            location
                                .to_str()
                                .map_err(|e| AttachmentError::error(e.to_string()))?,
                        )
                        .map_err(|e| AttachmentError::error(e.to_string()))?;
                    if next.origin() != url.origin() {
                        headers.remove("authorization");
                    }
                    url = next;
                    continue;
                }
            }
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            if headers
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|n| n > DEFAULT_MAX_BYTES as u64)
            {
                return Err(AttachmentError::error(format!(
                    "Attachment exceeds {DEFAULT_MAX_BYTES} byte limit."
                )));
            }
            let mut body = response.into_body();
            let mut bytes = vec![];
            while let Some(frame) = body.frame().await {
                if let Some(part) = frame
                    .map_err(|e| AttachmentError::error(e.to_string()))?
                    .data_ref()
                {
                    bytes.extend_from_slice(part);
                    if bytes.len() > DEFAULT_MAX_BYTES {
                        return Err(AttachmentError::error(format!(
                            "Attachment exceeds {DEFAULT_MAX_BYTES} byte limit."
                        )));
                    }
                }
            }
            return Ok((status, headers, bytes));
        }
        Err(AttachmentError::error("redirect count exceeded"))
    })
    .await
    .map_err(|_| AttachmentError::error("The operation was aborted due to timeout"))?
}
pub(super) async fn extract(
    runtime: &Runtime,
    input: &InboundMessage,
) -> (Vec<JsValue>, Vec<String>) {
    let kind = input.message_type.as_deref().unwrap_or("");
    if kind != "image" && kind != "file" {
        return (vec![], vec![]);
    }
    let Some(message_id) = input.message_id.as_ref().filter(|s| !s.is_empty()) else {
        return (
            vec![],
            vec!["[Attachment diagnostics] 飞书附件缺少 message_id，无法下载。".into()],
        );
    };
    let result =
        async {
            let api = runtime
                .api()
                .ok_or_else(|| AttachmentError::error("feishu app credentials missing"))?;
            let token = api.tenant_access_token().await.map_err(|e| {
                AttachmentError::error(format!("tenant token unavailable: Error: {e}"))
            })?;
            let content = input
                .content
                .as_deref()
                .and_then(|s| parse_json(s).ok())
                .unwrap_or(JsValue::Undefined);
            let key_name = if kind == "image" {
                "image_key"
            } else {
                "file_key"
            };
            let key = content
                .get(key_name)
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AttachmentError::error(format!("{key_name} missing")))?;
            let name = if kind == "image" {
                key.clone()
            } else {
                content
                    .get("file_name")
                    .or_else(|| content.get("name"))
                    .and_then(JsValue::as_string)
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_else(|| key.clone())
            };
            let url = format!(
                "{}/open-apis/im/v1/messages/{}/resources/{}?type={kind}",
                runtime.options().base_url,
                crate::channel::http_client::encode_uri_component(message_id),
                crate::channel::http_client::encode_uri_component(&key)
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                "authorization",
                format!("Bearer {token}").parse().map_err(
                    |e: hyper::header::InvalidHeaderValue| AttachmentError::error(e.to_string()),
                )?,
            );
            let root = runtime
                .attachment_root
                .lock()
                .expect("feishu attachment root")
                .clone();
            let store =
                ImAttachmentStore::new(root, "feishu", DEFAULT_MAX_BYTES, Arc::new(Fetcher));
            let attachment = store
                .save_from_url(SaveAttachmentFromUrlInput {
                    url,
                    headers,
                    chat_id: input.chat_id.clone(),
                    message_id: message_id.clone(),
                    kind: AttachmentKind::from(kind),
                    name: Some(name.clone()),
                    mime_type: if kind == "file" {
                        guess_mime_type_from_name(Some(&name)).map(str::to_owned)
                    } else {
                        None
                    },
                    bytes: if kind == "file" {
                        content.get("size").and_then(JsValue::as_number)
                    } else {
                        None
                    },
                    metadata: vec![
                        ("channelKey".into(), JsValue::from("feishu")),
                        ("chatId".into(), JsValue::from(input.chat_id.as_str())),
                        ("messageId".into(), JsValue::from(message_id.as_str())),
                        (
                            if kind == "image" {
                                "imageKey"
                            } else {
                                "fileKey"
                            }
                            .into(),
                            JsValue::from(key.as_str()),
                        ),
                    ],
                    transform: None,
                })
                .await?;
            Ok::<_, AttachmentError>(attachment.to_js())
        }
        .await;
    match result {
        Ok(attachment) => (vec![attachment], vec![]),
        Err(error) => {
            runtime.log(
                "warn",
                &format!("feishu: attachment download failed: {error}"),
            );
            (
                vec![],
                vec![format!(
                    "[Attachment diagnostics] 飞书附件下载失败：{error}"
                )],
            )
        }
    }
}
fn field(name: &str, value: &str, boundary: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
        .as_bytes(),
    );
}
pub(super) async fn upload(
    api: &ApiClient,
    base: &str,
    prepared: PreparedImAttachment,
) -> Result<(String, JsValue), AttachmentError> {
    let image = matches!(prepared.file_type, PreparedFileType::Image);
    let token = api
        .tenant_access_token()
        .await
        .map_err(AttachmentError::error)?;
    let boundary = format!("----pilotdeck{}", uuid::Uuid::new_v4().simple());
    let mut data = vec![];
    if image {
        field("image_type", "message", &boundary, &mut data);
    } else {
        field(
            "file_type",
            infer_type(&prepared.name, prepared.mime_type.as_deref()),
            &boundary,
            &mut data,
        );
        field("file_name", &prepared.name, &boundary, &mut data);
    }
    let part = if image { "image" } else { "file" };
    let name = prepared
        .name
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22");
    data.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{part}\"; filename=\"{name}\"\r\nContent-Type: {}\r\n\r\n",prepared.mime_type.as_deref().unwrap_or("application/octet-stream")).as_bytes());
    data.extend_from_slice(&prepared.buffer);
    data.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|e: hyper::header::InvalidHeaderValue| {
                AttachmentError::error(e.to_string())
            })?,
    );
    headers.insert(
        "content-type",
        format!("multipart/form-data; boundary={boundary}")
            .parse()
            .unwrap(),
    );
    let (status, _, bytes) = exchange(
        "POST",
        &format!(
            "{base}/open-apis/im/v1/{}",
            if image { "images" } else { "files" }
        ),
        headers,
        data,
    )
    .await?;
    let value = parse_json(&String::from_utf8_lossy(&bytes)).unwrap_or(JsValue::Undefined);
    let key = if image { "image_key" } else { "file_key" };
    let result = value
        .get("data")
        .and_then(|v| v.get(key))
        .and_then(JsValue::as_string)
        .cloned();
    if !(200..300).contains(&status)
        || value.get("code").and_then(JsValue::as_number) != Some(0.0)
        || result.as_ref().is_none_or(|s| s.is_empty())
    {
        if matches!(
            value.get("code").and_then(JsValue::as_number),
            Some(99991663.0 | 99991664.0)
        ) {
            api.invalidate_token(value.get("code").unwrap_or(&JsValue::Undefined))
                .await;
        }
        return Err(AttachmentError::error(format!(
            "feishu {} upload failed code={} msg={}",
            part,
            super::client::display(value.get("code")),
            super::client::display(value.get("msg"))
        )));
    }
    let mut object = JsObject::new();
    object.insert(key.into(), JsValue::String(result.unwrap()));
    Ok((part.into(), JsValue::Object(object)))
}
fn infer_type(name: &str, mime: Option<&str>) -> &'static str {
    let lower = name.to_lowercase();
    if mime == Some("application/pdf") || lower.ends_with(".pdf") {
        "pdf"
    } else if lower.ends_with(".doc") || lower.ends_with(".docx") {
        "doc"
    } else if lower.ends_with(".xls") || lower.ends_with(".xlsx") {
        "xls"
    } else if lower.ends_with(".ppt") || lower.ends_with(".pptx") {
        "ppt"
    } else {
        "stream"
    }
}
pub(super) fn outbound(value: &JsValue) -> OutboundAttachment {
    let get = |key| {
        value
            .get(key)
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
    };
    OutboundAttachment {
        kind: get("type").map(|s| AttachmentKind::from(s.as_str())),
        name: get("name"),
        path: get("path"),
        mime_type: get("mimeType"),
        content: get("content"),
        bytes: value.get("bytes").and_then(JsValue::as_number),
        source: get("source"),
        metadata: vec![],
    }
}
pub(super) struct Sink {
    pub runtime: Arc<Runtime>,
    pub chat: String,
}
impl ImAttachmentDeliverySink for Sink {
    fn send_prepared(
        &self,
        prepared: PreparedImAttachment,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>>
    {
        let runtime = self.runtime.clone();
        let chat = self.chat.clone();
        Box::pin(async move {
            let api = runtime
                .api()
                .ok_or_else(|| AttachmentError::error("feishu app credentials missing"))?;
            let (kind, content) = upload(&api, &runtime.options().base_url, prepared).await?;
            if api.send_raw(&chat, &kind, content).await.is_failed() {
                return Err(AttachmentError::error(
                    "feishu attachment message send failed",
                ));
            }
            Ok(())
        })
    }
    fn send_text_fallback(
        &self,
        text: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>>
    {
        let runtime = self.runtime.clone();
        let chat = self.chat.clone();
        Box::pin(async move {
            runtime.send(&chat, &text).await;
            Ok(())
        })
    }
}
pub(super) async fn send(runtime: Arc<Runtime>, chat: &str, value: &JsValue) -> bool {
    let logger = runtime.logger.fn_handle();
    let delivery = ImAttachmentDelivery::new(ImAttachmentDeliveryOptions {
        max_bytes: DEFAULT_MAX_BYTES,
        sink: Arc::new(Sink {
            runtime,
            chat: chat.into(),
        }),
        logger: Some(Arc::new(move |message| logger("error", message))),
    });
    delivery.send(&outbound(value)).await
}
