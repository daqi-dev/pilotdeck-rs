//! Gateway protocol adapters; G0 must inject the shared upload store.
use super::{Result, UploadError, UploadStore, object, safe_id, set, trim_js, truthy};
use base64::{
    Engine as _, alphabet,
    engine::{GeneralPurpose, GeneralPurposeConfig},
};
use pilotdeck_protocol::JsValue;
use std::future::Future;

const MAX_REMOTE_UPLOAD_PART_BYTES: usize = 8 * 1024 * 1024;
// Node Buffer accepts non-zero unused bits in the final base64 sextet.
const UPLOAD_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

/// `None` means unrecognized; a recognized request always finishes locally.
pub fn handle<'a>(
    store: &'a UploadStore,
    method: &'a str,
    params: &'a JsValue,
) -> Option<impl Future<Output = Result<JsValue>> + Send + 'a> {
    if !matches!(
        method,
        "upload_create" | "upload_get" | "upload_part" | "upload_complete" | "upload_cancel"
    ) {
        return None;
    }
    Some(async move {
        match method {
            "upload_create" => upload_create(store, params),
            "upload_part" => upload_part(store, params),
            "upload_complete" => upload_complete(store, params),
            "upload_cancel" => upload_cancel(store, params),
            _ => upload_get(store, params),
        }
        .map_err(gateway_error)
    })
}

fn upload_create(store: &UploadStore, params: &JsValue) -> Result<JsValue> {
    let key = property(params, "projectKey")?;
    let key = match &key {
        JsValue::Undefined | JsValue::Null => None,
        JsValue::String(key) => Some(key),
        _ => {
            return Err(UploadError::json(
                "input.projectKey?.trim is not a function",
            ));
        }
    };
    let key = key.filter(|key| !trim_js(key).is_empty()).ok_or_else(|| {
        UploadError::dialog("PROJECT_NOT_FOUND", "projectKey is required for uploads.")
    })?;
    let files = property(params, "files")?;
    if !matches!(files, JsValue::Array(_)) {
        return Err(UploadError::dialog(
            "UPLOAD_MANIFEST_INVALID",
            "files must be an array.",
        ));
    }
    let idempotency = property(params, "idempotencyKey")?;
    let idempotency = idempotency
        .as_string()
        .map(|s| s.to_utf8())
        .transpose()
        .map_err(UploadError::json)?;
    project_record(&store.create(
        &key.to_utf8().map_err(UploadError::json)?,
        &files,
        idempotency.as_deref(),
    )?)
}

fn upload_get(store: &UploadStore, params: &JsValue) -> Result<JsValue> {
    project_record(&store.get(&upload_id(params)?)?)
}

fn upload_complete(store: &UploadStore, params: &JsValue) -> Result<JsValue> {
    project_record(&store.complete(&upload_id(params)?)?)
}

fn upload_cancel(store: &UploadStore, params: &JsValue) -> Result<JsValue> {
    project_record(&store.cancel(&upload_id(params)?)?)
}

fn upload_part(store: &UploadStore, params: &JsValue) -> Result<JsValue> {
    let upload_id = upload_id(params)?;
    let client_file_id = property(params, "clientFileId")?;
    let client_file_id = client_file_id
        .as_string()
        .filter(|id| !trim_js(id).is_empty())
        .ok_or_else(|| {
            UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", "clientFileId is required.")
        })?;
    let bytes = decode_upload_part(&property(params, "contentBase64")?)?;
    project_attachment(&store.write_part(
        &upload_id,
        &client_file_id.to_utf8().map_err(UploadError::json)?,
        bytes.as_slice(),
    )?)
}

fn decode_upload_part(value: &JsValue) -> Result<Vec<u8>> {
    let invalid = || {
        UploadError::dialog(
            "UPLOAD_PART_INVALID",
            "contentBase64 must be a valid base64 string.",
        )
    };
    let encoded = value
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .ok_or_else(invalid)?;
    let padding = encoded.bytes().rev().take_while(|&b| b == b'=').count();
    if encoded.is_empty()
        || encoded.len() % 4 != 0
        || padding > 2
        || !encoded.as_bytes()[..encoded.len() - padding]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"+/".contains(b))
    {
        return Err(invalid());
    }
    // The validated grammar fixes the decoded length, without allocating an oversized part.
    if encoded.len() / 4 * 3 - padding > MAX_REMOTE_UPLOAD_PART_BYTES {
        return Err(UploadError::dialog(
            "UPLOAD_PART_TOO_LARGE",
            format!("Remote upload parts are limited to {MAX_REMOTE_UPLOAD_PART_BYTES} bytes."),
        ));
    }
    UPLOAD_BASE64.decode(encoded).map_err(|_| invalid())
}

fn property(input: &JsValue, key: &str) -> Result<JsValue> {
    let kind = match input {
        JsValue::Null => Some("null"),
        JsValue::Undefined => Some("undefined"),
        _ => None,
    };
    if let Some(kind) = kind {
        return Err(UploadError::json(format!(
            "Cannot read properties of {kind} (reading '{key}')"
        )));
    }
    Ok(input.get(key).cloned().unwrap_or(JsValue::Undefined))
}

fn upload_id(params: &JsValue) -> Result<String> {
    let value = property(params, "uploadId")?;
    let id = value.as_string().and_then(|s| s.to_utf8().ok());
    id.filter(|s| safe_id(s))
        .ok_or_else(|| UploadError::dialog("UPLOAD_NOT_FOUND", "Invalid uploadId."))
}

fn project_fields(input: &JsValue, fields: &[&str]) -> Result<JsValue> {
    let mut output = object([]);
    for field in fields {
        set(&mut output, field, property(input, field)?);
    }
    Ok(output)
}

fn project_attachment(input: &JsValue) -> Result<JsValue> {
    let mut output = project_fields(input, &["attachmentId", "name", "relativePath"])?;
    let mime = property(input, "mimeType")?;
    if truthy(&mime) {
        set(&mut output, "mimeType", mime);
    }
    for field in ["bytes", "sha256"] {
        set(&mut output, field, property(input, field)?);
    }
    Ok(output)
}

fn project_record(input: &JsValue) -> Result<JsValue> {
    let mut output = project_fields(
        input,
        &[
            "uploadId",
            "projectKey",
            "status",
            "manifest",
            "totalBytes",
            "uploadedBytes",
            "createdAt",
            "updatedAt",
            "expiresAt",
        ],
    )?;
    for field in [
        "idempotencyKeyHash",
        "attachments",
        "receivedClientFileIds",
        "errorCode",
        "errorMessage",
    ] {
        let mut value = property(input, field)?;
        if !truthy(&value) {
            continue;
        }
        if field == "attachments" {
            let JsValue::Array(items) = &value else {
                return Err(UploadError::json(
                    "record.attachments.map is not a function",
                ));
            };
            value = JsValue::Array(
                items
                    .iter()
                    .map(project_attachment)
                    .collect::<Result<_>>()?,
            );
        }
        set(&mut output, field, value);
    }
    Ok(output)
}

fn gateway_error(mut error: UploadError) -> UploadError {
    if error.code.is_empty() {
        error.code = "gateway_request_failed".into();
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::{Arc, Path, PathBuf, UploadStoreOptions, fs, io, string, text};
    use pilotdeck_protocol::parse_json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Harness {
        store: UploadStore,
        project: PathBuf,
    }

    impl Harness {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let base =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_tmp/20261027-g-upload/data");
            fs::create_dir_all(&base).unwrap();
            let project = loop {
                let dir = base.join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&dir) {
                    Ok(()) => break fs::canonicalize(dir).unwrap(),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("{e}"),
                }
            };
            let (resolved, listed) = (project.clone(), project.clone());
            let mut options = UploadStoreOptions::new(
                move |_| Ok(resolved.clone()),
                move || Ok(vec![listed.clone()]),
            );
            options.now = Arc::new(|| 1_792_281_600_000);
            options.uuid = Some(Arc::new(|| "upload-1".into()));
            Self {
                store: UploadStore::new(options),
                project,
            }
        }

        async fn call(&self, method: &str, params: JsValue) -> Result<JsValue> {
            handle(&self.store, method, &params)
                .expect("local upload method")
                .await
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.project);
        }
    }

    fn json(input: &str) -> JsValue {
        parse_json(input).unwrap()
    }

    fn manifest(size: usize) -> JsValue {
        json(&format!(
            r#"[{{"clientFileId":"file-1","name":"f.txt","relativePath":"f.txt","size":{size}}}]"#
        ))
    }

    fn create_params(size: usize) -> JsValue {
        object([("projectKey", text("project")), ("files", manifest(size))])
    }

    fn part_params(content: JsValue) -> JsValue {
        object([
            ("uploadId", text("upload-1")),
            ("clientFileId", text("file-1")),
            ("contentBase64", content),
        ])
    }

    async fn created_with_part(h: &Harness) -> JsValue {
        h.call("upload_create", create_params(1)).await.unwrap();
        h.call("upload_part", part_params(text("Zg==")))
            .await
            .unwrap();
        json(r#"{"uploadId":"upload-1"}"#)
    }

    #[tokio::test]
    async fn complete_requires_every_part_and_persists_failed() {
        let h = Harness::new();
        h.call("upload_create", create_params(1)).await.unwrap();
        let params = json(r#"{"uploadId":"upload-1"}"#);
        let error = h.call("upload_complete", params).await.unwrap_err();
        assert_eq!(error.code, "UPLOAD_MANIFEST_MISMATCH");
        let record = h
            .call("upload_get", json(r#"{"uploadId":"upload-1"}"#))
            .await
            .unwrap();
        assert_eq!(string(&record, "status").as_deref(), Some("failed"));
        assert_eq!(
            string(&record, "errorCode").as_deref(),
            Some("UPLOAD_MANIFEST_MISMATCH")
        );
    }

    #[tokio::test]
    async fn complete_projects_the_shared_store_record() {
        let h = Harness::new();
        let params = created_with_part(&h).await;
        let complete = h.call("upload_complete", params.clone()).await.unwrap();
        assert_eq!(string(&complete, "status").as_deref(), Some("completed"));
        assert_eq!(
            complete.get("uploadedBytes").and_then(JsValue::as_number),
            Some(1.0)
        );
        assert!(complete.get("path").is_none());
        let JsValue::Array(attachments) = complete.get("attachments").unwrap() else {
            panic!("attachments")
        };
        assert!(attachments[0].get("path").is_none());
        assert_eq!(h.call("upload_get", params).await.unwrap(), complete);
    }

    #[tokio::test]
    async fn cancel_is_idempotent_and_rejects_a_completed_upload() {
        let h = Harness::new();
        let created = h.call("upload_create", create_params(1)).await.unwrap();
        let params = json(r#"{"uploadId":"upload-1"}"#);
        let cancelled = h.call("upload_cancel", params.clone()).await.unwrap();
        assert_eq!(string(&cancelled, "status").as_deref(), Some("cancelled"));
        assert_eq!(
            h.call("upload_cancel", params.clone()).await.unwrap(),
            cancelled
        );
        for record in [&created, &cancelled] {
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
                assert!(record.get(key).is_some(), "{key}");
            }
            assert!(record.get("path").is_none());
        }
        assert!(cancelled.get("attachments").is_none());
        assert!(cancelled.get("errorCode").is_none());

        let params = created_with_part(&h).await;
        h.call("upload_complete", params.clone()).await.unwrap();
        let error = h.call("upload_cancel", params).await.unwrap_err();
        assert_eq!(error.code, "UPLOAD_ALREADY_COMPLETED");
    }

    #[tokio::test]
    async fn complete_and_cancel_validate_or_report_missing_ids() {
        let h = Harness::new();
        for method in ["upload_complete", "upload_cancel"] {
            for params in [
                json("{}"),
                json(r#"{"uploadId":"absent"}"#),
                json(r#"{"uploadId":"../escape"}"#),
            ] {
                assert_eq!(
                    h.call(method, params).await.unwrap_err().code,
                    "UPLOAD_NOT_FOUND"
                );
            }
            assert_eq!(
                h.call(method, JsValue::Null).await.unwrap_err().code,
                "gateway_request_failed"
            );
        }
    }

    #[tokio::test]
    async fn part_accepts_node_tail_bits_and_projects_attachments() {
        for (encoded, bytes) in [("Zh==", &b"f"[..]), ("Zm9=", &b"fo"[..])] {
            let h = Harness::new();
            h.call("upload_create", create_params(bytes.len()))
                .await
                .unwrap();
            let attachment = h
                .call("upload_part", part_params(text(encoded)))
                .await
                .unwrap();
            assert!(attachment.get("path").is_none());
            assert_eq!(
                string(&attachment, "attachmentId").as_deref(),
                Some("file-1")
            );
            assert_eq!(string(&attachment, "name").as_deref(), Some("f.txt"));
            assert_eq!(
                string(&attachment, "relativePath").as_deref(),
                Some("f.txt")
            );
            assert_eq!(
                attachment.get("bytes").and_then(JsValue::as_number),
                Some(bytes.len() as f64)
            );
            assert!(attachment.get("mimeType").is_none());
            let digest = if bytes.len() == 1 {
                "252f10c83610ebca1a059c0bae8255eba2f95be4d1d7bcfa89d7248a82d9f111"
            } else {
                "9c3aee7110b787f0fb5f81633a36392bd277ea945d44c874a9a23601aefe20cf"
            };
            assert_eq!(string(&attachment, "sha256").as_deref(), Some(digest));
            assert_eq!(
                fs::read(h.project.join(".tmp/chat-uploads/upload-1/files/file-1")).unwrap(),
                bytes
            );
            let record = h
                .call("upload_get", json(r#"{"uploadId":"upload-1"}"#))
                .await
                .unwrap();
            assert_eq!(
                record.get("attachments"),
                Some(&JsValue::Array(vec![attachment]))
            );
            assert_eq!(
                record.get("uploadedBytes").and_then(JsValue::as_number),
                Some(bytes.len() as f64)
            );
        }
    }

    #[tokio::test]
    async fn part_rejects_invalid_base64_before_accessing_the_store() {
        let h = Harness::new();
        for input in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(false),
            JsValue::Number(1.0),
            json("[]"),
            text(""),
            text("Zg="),
            text("Zg==\n"),
            text("Z g=="),
            text("Zg--"),
            text("Zg__"),
            text("Z==="),
            text("===="),
            text("Zg==AAAA"),
            text("é==="),
            text("Zg==\r\n"),
        ] {
            let error = h.call("upload_part", part_params(input)).await.unwrap_err();
            assert_eq!(error.code, "UPLOAD_PART_INVALID");
            assert_eq!(
                error.message,
                "contentBase64 must be a valid base64 string."
            );
        }
        assert_eq!(
            h.call("upload_part", part_params(text("Zg==")))
                .await
                .unwrap_err()
                .code,
            "UPLOAD_NOT_FOUND"
        );
    }

    #[tokio::test]
    async fn part_validates_in_ts_order_and_does_not_trim_client_file_id() {
        let h = Harness::new();
        let mut params = part_params(text("invalid"));
        set(&mut params, "uploadId", text("../bad"));
        set(&mut params, "clientFileId", JsValue::Null);
        assert_eq!(
            h.call("upload_part", params).await.unwrap_err().code,
            "UPLOAD_NOT_FOUND"
        );
        for id in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Number(1.0),
            text(" \u{feff}\u{a0}\n"),
        ] {
            let mut params = part_params(text("invalid"));
            set(&mut params, "clientFileId", id);
            let error = h.call("upload_part", params).await.unwrap_err();
            assert_eq!(error.code, "UPLOAD_MANIFEST_MISMATCH");
            assert_eq!(error.message, "clientFileId is required.");
        }
        for input in [JsValue::Null, JsValue::Undefined] {
            let error = h.call("upload_part", input).await.unwrap_err();
            assert_eq!(error.code, "gateway_request_failed");
            assert!(error.message.ends_with("(reading 'uploadId')"));
        }
        h.call("upload_create", create_params(1)).await.unwrap();
        let mut params = part_params(text("Zg=="));
        set(&mut params, "clientFileId", text(" file-1 "));
        let error = h.call("upload_part", params).await.unwrap_err();
        assert_eq!(error.code, "UPLOAD_MANIFEST_MISMATCH");
        assert_eq!(error.message, "Unexpected or duplicate file part:  file-1 ");
    }

    #[tokio::test]
    async fn part_allows_exactly_eight_mib_and_rejects_one_byte_more() {
        let h = Harness::new();
        let bytes = vec![0xfb; MAX_REMOTE_UPLOAD_PART_BYTES];
        h.call("upload_create", create_params(bytes.len()))
            .await
            .unwrap();
        let attachment = h
            .call(
                "upload_part",
                part_params(text(&UPLOAD_BASE64.encode(&bytes))),
            )
            .await
            .unwrap();
        assert_eq!(
            attachment.get("bytes").and_then(JsValue::as_number),
            Some(MAX_REMOTE_UPLOAD_PART_BYTES as f64)
        );
        assert!(attachment.get("path").is_none());
        assert_eq!(
            fs::read(h.project.join(".tmp/chat-uploads/upload-1/files/file-1")).unwrap(),
            bytes
        );

        let encoded = UPLOAD_BASE64.encode(vec![0; MAX_REMOTE_UPLOAD_PART_BYTES + 1]);
        let error = h
            .call("upload_part", part_params(text(&encoded)))
            .await
            .unwrap_err();
        assert_eq!(error.code, "UPLOAD_PART_TOO_LARGE");
        assert_eq!(
            error.message,
            "Remote upload parts are limited to 8388608 bytes."
        );
        let record = h
            .call("upload_get", json(r#"{"uploadId":"upload-1"}"#))
            .await
            .unwrap();
        assert_eq!(
            record.get("uploadedBytes").and_then(JsValue::as_number),
            Some(MAX_REMOTE_UPLOAD_PART_BYTES as f64)
        );
        let mut invalid = encoded;
        invalid.replace_range(0..1, "_");
        assert_eq!(
            h.call("upload_part", part_params(text(&invalid)))
                .await
                .unwrap_err()
                .code,
            "UPLOAD_PART_INVALID"
        );
    }

    #[tokio::test]
    async fn create_get_and_idempotency_use_the_shared_store() {
        let h = Harness::new();
        let mut params = create_params(1);
        set(&mut params, "idempotencyKey", text("key"));
        let created = h.call("upload_create", params.clone()).await.unwrap();
        assert_eq!(string(&created, "status").as_deref(), Some("created"));
        assert!(created.get("idempotencyKeyHash").is_some());
        assert_eq!(h.call("upload_create", params).await.unwrap(), created);
        assert_eq!(
            h.call("upload_get", json(r#"{"uploadId":"upload-1"}"#))
                .await
                .unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn create_validates_js_trim_files_and_preserves_the_project_key() {
        let h = Harness::new();
        for key in [
            JsValue::Undefined,
            JsValue::Null,
            text(""),
            text(" \u{feff}\u{a0}\n"),
        ] {
            let mut params = create_params(1);
            set(&mut params, "projectKey", key);
            assert_eq!(
                h.call("upload_create", params).await.unwrap_err().code,
                "PROJECT_NOT_FOUND"
            );
        }
        for files in [JsValue::Undefined, JsValue::Null, text("[]"), object([])] {
            let mut params = create_params(1);
            set(&mut params, "files", files);
            assert_eq!(
                h.call("upload_create", params).await.unwrap_err().code,
                "UPLOAD_MANIFEST_INVALID"
            );
        }
        let mut params = create_params(1);
        set(&mut params, "projectKey", JsValue::Number(1.0));
        assert_eq!(
            h.call("upload_create", params).await.unwrap_err().code,
            "gateway_request_failed"
        );
        for input in [JsValue::Null, JsValue::Undefined] {
            assert_eq!(
                h.call("upload_create", input).await.unwrap_err().code,
                "gateway_request_failed"
            );
        }
        let resolved = h.project.clone();
        let opts = UploadStoreOptions::new(
            move |key| {
                assert_eq!(key, " project ");
                Ok(resolved.clone())
            },
            || Ok(vec![]),
        );
        let store = UploadStore::new(opts);
        let mut params = create_params(1);
        set(&mut params, "projectKey", text(" project "));
        handle(&store, "upload_create", &params)
            .unwrap()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn get_missing_is_a_local_business_error_and_unknown_is_none() {
        let h = Harness::new();
        let error = h
            .call("upload_get", json(r#"{"uploadId":"absent"}"#))
            .await
            .unwrap_err();
        assert_eq!(error.code, "UPLOAD_NOT_FOUND");
        assert_eq!(error.message, "Unknown uploadId: absent");
        assert!(handle(&h.store, "other_method", &JsValue::Null).is_none());
    }

    #[tokio::test]
    async fn get_validates_ids_and_js_property_reads() {
        let h = Harness::new();
        for params in [
            json("{}"),
            json(r#"{"uploadId":"a..b"}"#),
            json(r#"{"uploadId":"é"}"#),
            json(r#"{"uploadId":42}"#),
        ] {
            assert_eq!(
                h.call("upload_get", params).await.unwrap_err().code,
                "UPLOAD_NOT_FOUND"
            );
        }
        for (params, kind) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
            let error = h.call("upload_get", params).await.unwrap_err();
            assert_eq!(error.code, "gateway_request_failed");
            assert_eq!(
                error.message,
                format!("Cannot read properties of {kind} (reading 'uploadId')")
            );
        }
    }

    #[tokio::test]
    async fn get_projects_records_without_exposing_storage_fields() {
        let h = Harness::new();
        let files = json(
            r#"[{"clientFileId":"file-1","name":"f.txt","relativePath":"f.txt","size":1,"custom":true}]"#,
        );
        h.store.create("project", &files, None).unwrap();
        h.store.write_part("upload-1", "file-1", &b"f"[..]).unwrap();
        let path = h.project.join(".tmp/chat-uploads/upload-1/metadata.json");
        let mut record = parse_json(&fs::read_to_string(&path).unwrap()).unwrap();
        set(&mut record, "path", text("internal"));
        set(&mut record, "unknown", text("private"));
        fs::write(
            &path,
            pilotdeck_protocol::stringify_json(&record)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let output = h
            .call("upload_get", json(r#"{"uploadId":"upload-1"}"#))
            .await
            .unwrap();
        assert!(output.get("path").is_none());
        assert!(output.get("unknown").is_none());
        assert_eq!(output.get("manifest"), record.get("manifest"));
        let JsValue::Array(attachments) = output.get("attachments").unwrap() else {
            panic!("attachments")
        };
        assert!(attachments[0].get("path").is_none());
        assert_eq!(string(&attachments[0], "name").as_deref(), Some("f.txt"));
    }

    #[test]
    fn errors_preserve_details_and_io_codes() {
        let mut error = UploadError::json("invalid JSON");
        error.details = Some(Box::new(json(r#"{"where":"metadata"}"#)));
        let mapped = gateway_error(error.clone());
        assert_eq!(mapped.code, "gateway_request_failed");
        assert_eq!(mapped.message, error.message);
        assert_eq!(mapped.details, error.details);
        let io = UploadError::io(io::Error::from_raw_os_error(13), "open", None);
        assert_eq!(gateway_error(io.clone()), io);
    }
}
