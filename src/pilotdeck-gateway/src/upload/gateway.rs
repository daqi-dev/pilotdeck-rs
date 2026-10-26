//! Gateway-method adapter over [`UploadStore`], measured against PilotDeck
//! cc7c4e6 `InProcessGateway.ts:2483-2522,3603-3646`.
//!
//! This is the U1 upload component only: it is not registered in the native
//! dispatch table, so default proxies keep forwarding `upload_*` to TS. The
//! adapter owns parameter validation order, provider call order, and the
//! response projection; bytes and lifecycle stay in the existing store.
//!
//! The projection is deliberately allow-list based. `UploadStore` records the
//! attachment's absolute `path` for lease/verify use, but TS
//! `toGatewayUploadRecord` and `uploadPart` never return it, so neither do we.
use super::{
    Result, UploadError, UploadRecord, UploadStore, UploadedAttachment, assert_id, object, set,
    trim_js, truthy, value,
};
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

/// `// TS InProcessGateway.ts:223`: remote parts are capped before touching disk.
pub(super) const MAX_REMOTE_UPLOAD_PART_BYTES: usize = 8 * 1024 * 1024;
/// Node's decoder accepts non-zero trailing bits (`AB==` -> `00`); the shape
/// regex above still rejects every non-base64 spelling.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
static UNDEFINED: JsValue = JsValue::Undefined;

/// The five lifecycle calls the gateway layer is allowed to make. Keeping this
/// a trait lets tests record the exact calls without a second store
/// implementation; production uses [`UploadStore`] itself.
pub trait UploadProvider: Send + Sync {
    fn create(
        &self,
        project_key: &JsString,
        files: &JsValue,
        idempotency_key: Option<&JsValue>,
    ) -> Result<UploadRecord>;
    fn get(&self, upload_id: &str) -> Result<UploadRecord>;
    fn write_part(
        &self,
        upload_id: &str,
        client_file_id: &JsString,
        bytes: &[u8],
    ) -> Result<UploadedAttachment>;
    fn complete(&self, upload_id: &str) -> Result<UploadRecord>;
    fn cancel(&self, upload_id: &str) -> Result<UploadRecord>;
}

impl UploadProvider for UploadStore {
    fn create(
        &self,
        project_key: &JsString,
        files: &JsValue,
        idempotency_key: Option<&JsValue>,
    ) -> Result<UploadRecord> {
        // TS creates the hash inside `createAt`, after the manifest check; the
        // raw value (including its UTF-16 units) crosses this boundary intact.
        UploadStore::create_js(self, project_key, files, idempotency_key)
    }
    fn get(&self, upload_id: &str) -> Result<UploadRecord> {
        UploadStore::get(self, upload_id)
    }
    fn write_part(
        &self,
        upload_id: &str,
        client_file_id: &JsString,
        bytes: &[u8],
    ) -> Result<UploadedAttachment> {
        UploadStore::write_part_js(self, upload_id, client_file_id, bytes)
    }
    fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        UploadStore::complete(self, upload_id)
    }
    fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        UploadStore::cancel(self, upload_id)
    }
}

/// Native counterpart of one `Gateway.upload*` method group. `None` models a
/// host that did not wire `uploadLifecycle`, including its evaluation order.
#[derive(Clone)]
pub struct UploadGateway {
    provider: Option<Arc<dyn UploadProvider>>,
}

impl UploadGateway {
    pub fn new(provider: Arc<dyn UploadProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    /// TS: `requireUploadLifecycle()` with `options.uploadLifecycle` unset.
    pub fn unavailable() -> Self {
        Self { provider: None }
    }

    fn require(&self) -> Result<&Arc<dyn UploadProvider>> {
        self.provider.as_ref().ok_or_else(|| {
            UploadError::dialog("CAPABILITY_UNAVAILABLE", "Gateway uploads are unavailable.")
        })
    }

    /// `upload_create`: capability, projectKey, files, then store.create.
    pub fn create(&self, params: &JsValue) -> Result<UploadRecord> {
        let provider = self.require()?;
        let project_key = project_key(params)?;
        let files = parameter(params, "files")?;
        if !matches!(files, JsValue::Array(_)) {
            return Err(UploadError::dialog(
                "UPLOAD_MANIFEST_INVALID",
                "files must be an array.",
            ));
        }
        // TS forwards the raw property; nullish becomes undefined for the store.
        let raw_key = parameter(params, "idempotencyKey")?;
        let idempotency_key =
            (!matches!(raw_key, JsValue::Undefined | JsValue::Null)).then_some(raw_key);
        let record = provider.create(&project_key, files, idempotency_key)?;
        gateway_record(&record)
    }

    /// `upload_get`: capability, then uploadId, then store.get.
    pub fn get(&self, params: &JsValue) -> Result<UploadRecord> {
        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        gateway_record(&provider.get(&upload_id)?)
    }

    /// `upload_part`: uploadId, clientFileId, base64, capability, writePart.
    pub fn part(&self, params: &JsValue) -> Result<UploadedAttachment> {
        let upload_id = upload_id(params)?;
        let client_file_id = client_file_id(params)?;
        let content = parameter(params, "contentBase64")?;
        let bytes = decode_upload_part(content)?;
        let provider = self.require()?;
        let attachment = provider.write_part(&upload_id, &client_file_id, &bytes)?;
        gateway_attachment(&attachment)
    }

    /// `upload_complete`: capability, then uploadId, then store.complete.
    pub fn complete(&self, params: &JsValue) -> Result<UploadRecord> {
        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        gateway_record(&provider.complete(&upload_id)?)
    }

    /// `upload_cancel`: capability, then uploadId, then store.cancel.
    pub fn cancel(&self, params: &JsValue) -> Result<UploadRecord> {
        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        gateway_record(&provider.cancel(&upload_id)?)
    }
}

fn request_failed(message: impl Into<String>) -> UploadError {
    UploadError::dialog("gateway_request_failed", message)
}

/// Reads one own property with the TypeError Node raises for a null/undefined
/// params object. Primitive params box like JS and simply have no property.
fn parameter<'a>(params: &'a JsValue, key: &str) -> Result<&'a JsValue> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(request_failed(format!(
            "Cannot read properties of {kind} (reading '{key}')"
        )));
    }
    Ok(params.get(key).unwrap_or(&UNDEFINED))
}

fn project_key(params: &JsValue) -> Result<JsString> {
    let required =
        || UploadError::dialog("PROJECT_NOT_FOUND", "projectKey is required for uploads.");
    match parameter(params, "projectKey")? {
        JsValue::Undefined | JsValue::Null => Err(required()),
        JsValue::String(key) if trim_js(key).units().is_empty() => Err(required()),
        // The provider port receives the exact UTF-16 value. Node only replaces
        // lone surrogates at the filesystem boundary, not when handing the key
        // to `resolveProject`/`create` (PilotDeck InProcessGateway.ts:2483-2486).
        JsValue::String(key) => Ok(key.clone()),
        // `input.projectKey?.trim()` boxes primitives; only nullish short-circuits.
        _ => Err(request_failed("input.projectKey?.trim is not a function")),
    }
}

fn upload_id(params: &JsValue) -> Result<String> {
    let invalid = || UploadError::dialog("UPLOAD_NOT_FOUND", "Invalid uploadId.");
    let JsValue::String(raw) = parameter(params, "uploadId")? else {
        return Err(invalid());
    };
    let Ok(id) = raw.to_utf8() else {
        return Err(invalid());
    };
    assert_id(&id)?;
    Ok(id)
}

fn client_file_id(params: &JsValue) -> Result<JsString> {
    let required = || UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", "clientFileId is required.");
    let JsValue::String(raw) = parameter(params, "clientFileId")? else {
        return Err(required());
    };
    if trim_js(raw).units().is_empty() {
        return Err(required());
    }
    Ok(raw.clone())
}

pub(super) fn decode_upload_part(value: &JsValue) -> Result<Vec<u8>> {
    let invalid = || {
        UploadError::dialog(
            "UPLOAD_PART_INVALID",
            "contentBase64 must be a valid base64 string.",
        )
    };
    let JsValue::String(raw) = value else {
        return Err(invalid());
    };
    let units = raw.units();
    let Some(decoded_len) = upload_part_len(units) else {
        return Err(invalid());
    };
    // Check the exact decoded size before either allocation: a 1 GiB spelling
    // must be rejected without first copying it into an encoded buffer.
    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES {
        return Err(UploadError::dialog(
            "UPLOAD_PART_TOO_LARGE",
            format!("Remote upload parts are limited to {MAX_REMOTE_UPLOAD_PART_BYTES} bytes."),
        ));
    }
    // Shape validation proves the input is ASCII, so the byte view is exact.
    let encoded: Vec<u8> = units.iter().map(|&unit| unit as u8).collect();
    let bytes = BASE64.decode(encoded).map_err(|_| invalid())?;
    debug_assert_eq!(
        bytes.len(),
        decoded_len,
        "decoded size matches the pre-check"
    );
    Ok(bytes)
}

/// Decoded byte count without allocating, for an input that already passed the
/// `decodeUploadPart` shape gate. Every accepted spelling has a multiple-of-four
/// length and 0..=2 trailing `=`, so `len/4*3 - padding` is exact.
fn upload_part_len(units: &[u16]) -> Option<usize> {
    if units.is_empty() || !units.len().is_multiple_of(4) || !base64_shape(units) {
        return None;
    }
    let padding = units.iter().rev().take_while(|&&unit| unit == 0x3d).count();
    Some(units.len() / 4 * 3 - padding)
}

/// `/^[A-Za-z0-9+/]*={0,2}$/` over UTF-16 units: padding only at the end, at
/// most two, and no alphabet character may follow it.
fn base64_shape(units: &[u16]) -> bool {
    let mut padding = 0;
    for &unit in units {
        let alphabet = matches!(
            unit,
            0x41..=0x5a | 0x61..=0x7a | 0x30..=0x39 | 0x2b | 0x2f
        );
        if alphabet {
            if padding > 0 {
                return false;
            }
        } else if unit == 0x3d {
            padding += 1;
            if padding > 2 {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

/// `createHash("sha256").update(<non-string truthy>)` TypeError, measured on
/// Node v22.22.0. The idempotencyKey declaration is string, but a JSON client
/// can still send the other shapes, and they must not silently skip the hash.
/// Numbers use JS `Number#toString`: Node prints `1e+21` / `1e-7`, not the
/// Rust `Display` expansion (`native_archive.rs` uses the same helper).
pub(super) fn crypto_received(value: &JsValue) -> String {
    let received = match value {
        JsValue::Number(number) => {
            format!("type number ({})", crate::js_value::number_string(*number))
        }
        JsValue::Bool(boolean) => format!("type boolean ({boolean})"),
        JsValue::Array(_) => "an instance of Array".into(),
        JsValue::Object(_) => "an instance of Object".into(),
        _ => "unknown".into(),
    };
    format!(
        "The \"data\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received {received}"
    )
}

/// `toGatewayUploadRecord`: explicit allow-list, TS property order, no `path`.
fn gateway_record(record: &UploadRecord) -> Result<UploadRecord> {
    let mut out = JsObject::new();
    for key in [
        "uploadId",
        "projectKey",
        "status",
        "manifest",
        "totalBytes",
        "uploadedBytes",
        "createdAt",
        "updatedAt",
        "expiresAt",
    ] {
        out.insert(key.into(), value(record, key));
    }
    for key in [
        "idempotencyKeyHash",
        "attachments",
        "receivedClientFileIds",
        "errorCode",
        "errorMessage",
    ] {
        let field = value(record, key);
        if !truthy(&field) {
            continue;
        }
        let projected = if key == "attachments" {
            let JsValue::Array(items) = &field else {
                return Err(request_failed("record.attachments.map is not a function"));
            };
            JsValue::Array(
                items
                    .iter()
                    .map(gateway_attachment)
                    .collect::<Result<_>>()?,
            )
        } else {
            field
        };
        out.insert(key.into(), projected);
    }
    Ok(JsValue::Object(out))
}

/// `GatewayUploadAttachment` shape from TS:3641-3648; the provider's `path`
/// and any future field are intentionally not copied. TS reads
/// `attachment.attachmentId` before any mapping, so null/undefined throw
/// instead of being projected as `{}` (PilotDeck InProcessGateway.ts:3627-3635).
fn gateway_attachment(attachment: &JsValue) -> Result<JsValue> {
    if matches!(attachment, JsValue::Undefined | JsValue::Null) {
        let kind = if attachment.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(request_failed(format!(
            "Cannot read properties of {kind} (reading 'attachmentId')"
        )));
    }
    let mut out = object([
        ("attachmentId", value(attachment, "attachmentId")),
        ("name", value(attachment, "name")),
        ("relativePath", value(attachment, "relativePath")),
    ]);
    let mime_type = value(attachment, "mimeType");
    if truthy(&mime_type) {
        set(&mut out, "mimeType", mime_type);
    }
    set(&mut out, "bytes", value(attachment, "bytes"));
    set(&mut out, "sha256", value(attachment, "sha256"));
    Ok(out)
}
