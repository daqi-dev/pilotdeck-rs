//! Replays the real-TS gateway oracle and pins projection/ordering rules.
use super::gateway::{
    MAX_REMOTE_UPLOAD_PART_BYTES, UploadGateway, UploadProvider, decode_upload_part,
};
use super::*;
use sha2::{Digest, Sha256};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};

const ORACLE: &str = include_str!("../../tests/fixtures/upload-gateway/oracle.json");
const EPOCH: i64 = 1_792_281_600_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let dir = Path::new("/tmp").join(format!(
                "pdrs-upload-gateway-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(fs::canonicalize(dir).unwrap()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("{e}"),
            }
        }
    }
    fn project(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Recorder {
    store: Arc<UploadStore>,
    calls: Arc<Mutex<Vec<JsValue>>>,
}
impl Recorder {
    fn log(&self, call: JsValue) {
        self.calls.lock().unwrap().push(call);
    }
}
impl UploadProvider for Recorder {
    fn create(
        &self,
        project_key: &JsString,
        files: &JsValue,
        idempotency_key: Option<&JsValue>,
    ) -> Result<UploadRecord> {
        self.log(object([
            ("method", text("create")),
            ("projectKey", JsValue::String(project_key.clone())),
            ("files", files.clone()),
            (
                "idempotencyKey",
                idempotency_key.cloned().unwrap_or(JsValue::Null),
            ),
        ]));
        UploadProvider::create(&*self.store, project_key, files, idempotency_key)
    }
    fn get(&self, upload_id: &str) -> Result<UploadRecord> {
        self.log(object([
            ("method", text("get")),
            ("uploadId", text(upload_id)),
        ]));
        UploadProvider::get(&*self.store, upload_id)
    }
    fn write_part(
        &self,
        upload_id: &str,
        client_file_id: &JsString,
        bytes: &[u8],
    ) -> Result<UploadedAttachment> {
        self.log(object([
            ("method", text("writePart")),
            ("uploadId", text(upload_id)),
            ("clientFileId", JsValue::String(client_file_id.clone())),
            ("bytes", JsValue::Number(bytes.len() as f64)),
            ("sha256", text(&format!("{:x}", Sha256::digest(bytes)))),
        ]));
        UploadProvider::write_part(&*self.store, upload_id, client_file_id, bytes)
    }
    fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        self.log(object([
            ("method", text("complete")),
            ("uploadId", text(upload_id)),
        ]));
        UploadProvider::complete(&*self.store, upload_id)
    }
    fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        self.log(object([
            ("method", text("cancel")),
            ("uploadId", text(upload_id)),
        ]));
        UploadProvider::cancel(&*self.store, upload_id)
    }
}

struct Harness {
    gateway: UploadGateway,
    calls: Arc<Mutex<Vec<JsValue>>>,
    project: PathBuf,
    now: Arc<AtomicI64>,
}

fn harness(project: &Path, unavailable: bool) -> Harness {
    // The parameter-fidelity scenario resolves real U+FFFD to this sibling.
    fs::create_dir_all(project.join("replacement")).unwrap();
    let now = Arc::new(AtomicI64::new(EPOCH));
    let resolved = project.to_owned();
    let listed = vec![project.to_owned(), project.join("replacement")];
    let mut options = UploadStoreOptions::new(
        move |key: &JsString| {
            Ok(if key.equals_str("missing") {
                resolved.join("absent")
            } else if key.equals_str("p\u{fffd}") {
                resolved.join("replacement")
            } else {
                resolved.clone()
            })
        },
        move || Ok(listed.clone()),
    );
    let clock = now.clone();
    options.now = Arc::new(move || clock.load(Ordering::SeqCst));
    let created = Arc::new(AtomicUsize::new(0));
    options.uuid = Some(Arc::new(move || {
        format!("upload-{}", created.fetch_add(1, Ordering::SeqCst) + 1)
    }));
    let store = Arc::new(UploadStore::new(options));
    let calls = Arc::new(Mutex::new(vec![]));
    let gateway = if unavailable {
        UploadGateway::unavailable()
    } else {
        UploadGateway::new(Arc::new(Recorder {
            store,
            calls: calls.clone(),
        }))
    };
    Harness {
        gateway,
        calls,
        project: project.to_owned(),
        now,
    }
}

fn compact(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}
fn normalized(value: &JsValue, project: &Path) -> JsValue {
    let path = project.to_string_lossy();
    parse_json(&compact(value).replace(&*path, "<PROJECT>")).unwrap()
}
fn expand(value: &JsValue) -> JsValue {
    if let JsValue::Object(fields) = value
        && let Some(bytes) = fields.get("$zerosBytes").and_then(JsValue::as_number)
    {
        // Base64 of `bytes` zero bytes, including its trailing padding.
        let bytes = bytes as usize;
        let (full, rem) = (bytes / 3, bytes % 3);
        let mut encoded = "A".repeat(full * 4 + if rem == 0 { 0 } else { rem + 1 });
        encoded.push_str(match rem {
            0 => "",
            1 => "==",
            _ => "=",
        });
        return text(&encoded);
    }
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(expand).collect()),
        JsValue::Object(fields) => {
            let mut out = JsObject::new();
            for (key, item) in fields.entries() {
                out.insert(key.clone(), expand(item));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn tamper(project: &Path, kind: &str) {
    let path = project.join(".tmp/chat-uploads/upload-1/metadata.json");
    let mut record = parse_json(&fs::read_to_string(&path).unwrap()).unwrap();
    match kind {
        "metadata-extra" => {
            set(&mut record, "diskPath", text("/secret/record-path"));
            set(&mut record, "path", text("/secret/record-path"));
            set(
                &mut record,
                "unknown",
                object([("nested", JsValue::Bool(true))]),
            );
            set(&mut record, "errorCode", text(""));
            set(&mut record, "errorMessage", text(""));
            let mut attachments = array(&record, "attachments").to_vec();
            let mut first = attachments[0].clone();
            set(&mut first, "path", text("/secret/attachment-path"));
            set(&mut first, "secret", text("keep-out"));
            set(&mut first, "mimeType", text(""));
            attachments[0] = first;
            set(&mut record, "attachments", JsValue::Array(attachments));
        }
        "metadata-error" => {
            set(&mut record, "status", text("failed"));
            set(&mut record, "errorCode", text("E_RECORDED"));
            set(&mut record, "errorMessage", text("recorded failure"));
        }
        "attachments-null" => {
            set(
                &mut record,
                "attachments",
                JsValue::Array(vec![JsValue::Null]),
            );
        }
        other => panic!("unknown tamper {other}"),
    }
    fs::write(&path, json_line(&record).unwrap()).unwrap();
}

fn invoke(h: &Harness, method: &str, params: &JsValue) -> Result<JsValue> {
    match method {
        "create" => h.gateway.create(params),
        "get" => h.gateway.get(params),
        "part" => h.gateway.part(params),
        "complete" => h.gateway.complete(params),
        "cancel" => h.gateway.cancel(params),
        other => panic!("unknown method {other}"),
    }
}

fn run(h: &Harness, operation: &JsValue) -> JsValue {
    let since = h.calls.lock().unwrap().len();
    let result = if let Some(ms) = operation.get("advance").and_then(JsValue::as_number) {
        h.now.fetch_add(ms as i64, Ordering::SeqCst);
        Ok(JsValue::Null)
    } else if let Some(kind) = string(operation, "tamper") {
        tamper(&h.project, &kind);
        Ok(JsValue::Null)
    } else {
        let method = string(operation, "method").unwrap();
        let params = if operation.get("undefinedParams").and_then(JsValue::as_bool) == Some(true) {
            JsValue::Undefined
        } else {
            expand(&value(operation, "params"))
        };
        invoke(h, &method, &params).map(|value| normalized(&value, &h.project))
    };
    let calls = JsValue::Array(h.calls.lock().unwrap().split_off(since));
    match result {
        Ok(ok) => object([("ok", ok), ("calls", calls)]),
        Err(e) => object([
            (
                "error",
                object([
                    ("code", text(&e.code)),
                    (
                        "message",
                        text(
                            &e.message
                                .replace(&*h.project.to_string_lossy(), "<PROJECT>"),
                        ),
                    ),
                ]),
            ),
            ("calls", calls),
        ]),
    }
}

#[test]
fn upload_gateway_oracle_replay_matches_ts_results() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let scratch = Scratch::new();
    let mut operations = 0;
    for (index, scenario) in array(&oracle, "scenarios").iter().enumerate() {
        let name = string(scenario, "name").unwrap();
        let project = scratch.project(&format!("{index}-{name}"));
        let h = harness(&project, name == "provider-unavailable");
        let recorded = array(scenario, "operations");
        let expected = array(scenario, "results");
        assert_eq!(recorded.len(), expected.len(), "{name}");
        for (step, (operation, expected)) in recorded.iter().zip(expected).enumerate() {
            let actual = run(&h, operation);
            assert_eq!(
                compact(&actual),
                compact(expected),
                "{name}#{step} {}",
                compact(operation)
            );
            operations += 1;
        }
    }
    assert_eq!(array(&oracle, "scenarios").len(), 9, "scenario coverage");
    assert!(operations >= 145, "operation coverage: {operations}");
    println!("PDRS_UPLOAD_GATEWAY operations={operations}");
}

fn created(h: &Harness) -> String {
    let file =
        parse_json(r#"[{"clientFileId":"file-1","name":"a.txt","relativePath":"a.txt","size":5}]"#)
            .unwrap();
    let record = h
        .gateway
        .create(&object([("projectKey", text("p")), ("files", file)]))
        .unwrap();
    string(&record, "uploadId").unwrap()
}

fn part(h: &Harness, upload_id: &str) -> Result<JsValue> {
    h.gateway.part(&object([
        ("uploadId", text(upload_id)),
        ("clientFileId", text("file-1")),
        ("contentBase64", text("aGVsbG8=")),
    ]))
}

#[test]
fn upload_gateway_never_projects_attachment_disk_path() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("path"), false);
    let upload_id = created(&h);
    let attachment = part(&h, &upload_id).unwrap();
    assert!(
        !compact(&attachment).contains("\"path\""),
        "attachment projection must drop the store path: {}",
        compact(&attachment)
    );
    h.gateway
        .complete(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    let record = h
        .gateway
        .get(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    assert!(
        !compact(&record).contains("\"path\""),
        "record projection must drop attachment paths: {}",
        compact(&record)
    );
    // The store keeps the real path for lease/verify consumers.
    assert!(
        h.project
            .join(".tmp/chat-uploads/upload-1/files/file-1")
            .is_file()
    );
}

#[test]
fn upload_gateway_validation_order_matches_ts() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("order"), false);
    let js = |text: &str| parse_json(text).unwrap();
    let error = |params: &str| {
        let e = h.gateway.part(&js(params)).unwrap_err();
        (e.code, e.message)
    };
    // uploadId wins over clientFileId and contentBase64.
    assert_eq!(
        error(r#"{"uploadId":"../evil","clientFileId":null,"contentBase64":null}"#),
        ("UPLOAD_NOT_FOUND".into(), "Invalid uploadId.".into()),
        "uploadId safety must be checked before clientFileId"
    );
    // clientFileId wins over contentBase64.
    assert_eq!(
        error(r#"{"uploadId":"upload-1","clientFileId":"  ","contentBase64":null}"#),
        (
            "UPLOAD_MANIFEST_MISMATCH".into(),
            "clientFileId is required.".into()
        ),
        "whitespace-only clientFileId must be rejected before base64"
    );
    // contentBase64 must be valid before the provider is required.
    assert_eq!(
        error(r#"{"uploadId":"upload-1","clientFileId":"file-1","contentBase64":"A"}"#),
        (
            "UPLOAD_PART_INVALID".into(),
            "contentBase64 must be a valid base64 string.".into()
        ),
        "base64 shape must be checked before the provider"
    );
    // create: projectKey, then files, then the store's idempotency hash.
    assert_eq!(
        h.gateway
            .create(&js(
                r#"{"projectKey":null,"files":null,"idempotencyKey":5}"#
            ))
            .unwrap_err()
            .code,
        "PROJECT_NOT_FOUND",
        "projectKey must be checked before files and the idempotency key"
    );
    let whitespace = h
        .gateway
        .create(&js(r#"{"projectKey":"   ","files":[],"idempotencyKey":5}"#))
        .expect_err("projectKey whitespace must be rejected before the provider");
    assert_eq!(
        (whitespace.code.as_str(), whitespace.message.as_str()),
        ("PROJECT_NOT_FOUND", "projectKey is required for uploads."),
        "projectKey whitespace must be rejected before the provider"
    );
    let non_string = h
        .gateway
        .create(&js(r#"{"projectKey":5,"files":[],"idempotencyKey":5}"#))
        .expect_err("non-string projectKey must be rejected before the provider");
    assert_eq!(
        (non_string.code.as_str(), non_string.message.as_str()),
        (
            "gateway_request_failed",
            "input.projectKey?.trim is not a function"
        ),
        "non-string projectKey must be rejected before the provider"
    );
    let files = h
        .gateway
        .create(&js(
            r#"{"projectKey":"p","files":"not-an-array","idempotencyKey":5}"#,
        ))
        .unwrap_err();
    assert_eq!(
        (files.code.as_str(), files.message.as_str()),
        ("UPLOAD_MANIFEST_INVALID", "files must be an array."),
        "files must be an array before the store's manifest validation"
    );
    // Trailing non-zero bits decode like Node (`AB==` -> 00).
    let trailing = decode_upload_part(&text("AB==")).unwrap_or_else(|error| {
        panic!("non-zero trailing bits must stay accepted like Node's decoder: {error:?}")
    });
    assert_eq!(
        trailing,
        vec![0],
        "non-zero trailing bits must stay accepted like Node's decoder"
    );
    // Unavailable: provider comes first for create/get/complete/cancel.
    let missing = harness(&scratch.project("no-provider"), true);
    let bad = || js(r#"{"uploadId":"../evil"}"#);
    assert_eq!(
        missing.gateway.create(&JsValue::Null).unwrap_err().code,
        "CAPABILITY_UNAVAILABLE",
        "create must require the provider before reading params"
    );
    for (method, code) in [
        ("get", missing.gateway.get(&bad()).unwrap_err().code),
        (
            "complete",
            missing.gateway.complete(&bad()).unwrap_err().code,
        ),
        ("cancel", missing.gateway.cancel(&bad()).unwrap_err().code),
    ] {
        assert_eq!(
            code, "CAPABILITY_UNAVAILABLE",
            "{method} must require the provider before validating uploadId"
        );
    }
    // part validates before require, so the uploadId error survives.
    assert_eq!(
        missing
            .gateway
            .part(&js(
                r#"{"uploadId":"../evil","clientFileId":null,"contentBase64":null}"#
            ))
            .unwrap_err()
            .code,
        "UPLOAD_NOT_FOUND",
        "part must validate params before requiring the provider"
    );
}

#[test]
fn upload_gateway_projection_drops_unknown_and_empty_optional_fields() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("projection"), false);
    let upload_id = created(&h);
    assert!(
        h.gateway
            .get(&object([("uploadId", text(&upload_id))]))
            .unwrap()
            .get("receivedClientFileIds")
            .is_some_and(|value| matches!(value, JsValue::Array(items) if items.is_empty()))
    );
    part(&h, &upload_id).unwrap();
    h.gateway
        .complete(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    tamper(&h.project, "metadata-extra");
    let output = compact(
        &h.gateway
            .get(&object([("uploadId", text(&upload_id))]))
            .unwrap(),
    );
    for absent in [
        "diskPath",
        "keep-out",
        "unknown",
        "\"path\"",
        "\"errorCode\"",
        "\"mimeType\":\"\"",
    ] {
        assert!(!output.contains(absent), "{absent} leaked: {output}");
    }
    tamper(&h.project, "metadata-error");
    let record = h
        .gateway
        .get(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    assert_eq!(
        string(&record, "errorCode").as_deref(),
        Some("E_RECORDED"),
        "truthy recorded errorCode must stay projected"
    );
    assert_eq!(
        string(&record, "errorMessage").as_deref(),
        Some("recorded failure"),
        "truthy recorded errorMessage must stay projected"
    );
}

#[test]
fn upload_gateway_null_attachment_keeps_ts_property_error() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("null-attachment"), false);
    let upload_id = created(&h);
    tamper(&h.project, "attachments-null");
    let error = h
        .gateway
        .get(&object([("uploadId", text(&upload_id))]))
        .expect_err("null attachment must keep the TS property-access error");
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        (
            "gateway_request_failed",
            "Cannot read properties of null (reading 'attachmentId')"
        ),
        "null attachment must keep the TS property-access error"
    );
}

#[test]
fn upload_gateway_idempotency_key_type_is_checked_after_project_and_manifest() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("key-order"), false);
    let files = r#"[{"clientFileId":"f","name":"f","relativePath":"f","size":1}]"#;
    let error = |params: String| h.gateway.create(&parse_json(&params).unwrap()).unwrap_err();
    let missing = error(format!(
        r#"{{"projectKey":"missing","files":{files},"idempotencyKey":5}}"#
    ));
    assert_eq!(
        missing.code, "PROJECT_NOT_FOUND",
        "project validation must precede the idempotencyKey type"
    );
    let empty = error(r#"{"projectKey":"p","files":[],"idempotencyKey":5}"#.to_string());
    assert_eq!(
        empty.code, "UPLOAD_MANIFEST_INVALID",
        "manifest validation must precede the idempotencyKey type"
    );
    let typed = error(format!(
        r#"{{"projectKey":"p","files":{files},"idempotencyKey":5}}"#
    ));
    assert_eq!(
        typed.code, "ERR_INVALID_ARG_TYPE",
        "a valid project/manifest must then raise the hash TypeError"
    );
}

#[test]
fn upload_gateway_idempotency_number_messages_match_node() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("key-numbers"), false);
    let files = r#"[{"clientFileId":"f","name":"f","relativePath":"f","size":1}]"#;
    for (literal, received) in [("1e21", "1e+21"), ("1e-7", "1e-7"), ("1e-6", "0.000001")] {
        let error = h
            .gateway
            .create(
                &parse_json(&format!(
                    r#"{{"projectKey":"p","files":{files},"idempotencyKey":{literal}}}"#
                ))
                .unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code, "ERR_INVALID_ARG_TYPE");
        assert!(
            error
                .message
                .ends_with(&format!("type number ({received})")),
            "Node spells {literal} as {received}, got: {}",
            error.message
        );
    }
}

#[test]
fn upload_gateway_idempotency_hash_still_dedupes_after_moving_into_store() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("key-dedupe"), false);
    let files =
        parse_json(r#"[{"clientFileId":"f","name":"f","relativePath":"f","size":1}]"#).unwrap();
    let create = || {
        h.gateway
            .create(&object([
                ("projectKey", text("p")),
                ("files", files.clone()),
                ("idempotencyKey", text("once")),
            ]))
            .unwrap()
    };
    let first = string(&create(), "uploadId").unwrap();
    let second = string(&create(), "uploadId").unwrap();
    assert_eq!(
        first, second,
        "idempotencyKey must hash into the store's dedup path"
    );
}

#[test]
fn upload_gateway_preserves_raw_utf16_parameters() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("fidelity"), false);
    let files = parse_json(r#"[{"clientFileId":"file-1","name":"f","relativePath":"f","size":5}]"#)
        .unwrap();
    let lone = JsString::from_utf16(vec![u16::from(b'p'), 0xd800]);
    let fffd = JsString::from_utf16(vec![u16::from(b'p'), 0xfffd]);
    let create = |key: &JsString| {
        h.gateway
            .create(&object([
                ("projectKey", JsValue::String(key.clone())),
                ("files", files.clone()),
            ]))
            .unwrap()
    };
    let plain = create(&lone);
    let replacement = create(&fffd);
    let calls = h.calls.lock().unwrap();
    assert_eq!(
        value(&calls[0], "projectKey"),
        JsValue::String(lone.clone()),
        "lone surrogate must not be replaced before the provider"
    );
    assert_eq!(
        value(&calls[1], "projectKey"),
        JsValue::String(fffd),
        "real U+FFFD must stay distinct from the lone surrogate"
    );
    drop(calls);
    let plain_project = h.project.to_string_lossy().into_owned();
    let fffd_project = h.project.join("replacement").to_string_lossy().into_owned();
    assert_eq!(
        string(&plain, "projectKey").as_deref(),
        Some(plain_project.as_str()),
        "lone surrogate keys must resolve as themselves"
    );
    assert_eq!(
        string(&replacement, "projectKey").as_deref(),
        Some(fffd_project.as_str()),
        "U+FFFD keys must select the replacement resolver branch"
    );
    // write_part logs the raw clientFileId before the store rejects it.
    let upload_id = string(&plain, "uploadId").unwrap();
    let _ = h.gateway.part(&object([
        ("uploadId", text(&upload_id)),
        (
            "clientFileId",
            JsValue::String(JsString::from_utf16(vec![u16::from(b'f'), 0xd800])),
        ),
        ("contentBase64", text("aGVsbG8=")),
    ]));
    let calls = h.calls.lock().unwrap();
    assert_eq!(
        value(calls.last().unwrap(), "clientFileId"),
        JsValue::String(JsString::from_utf16(vec![u16::from(b'f'), 0xd800])),
        "lone surrogate clientFileId must not be replaced before the provider"
    );
}

#[test]
fn upload_gateway_lone_surrogate_idempotency_key_hashes_like_node() {
    let scratch = Scratch::new();
    let h = harness(&scratch.project("key-surrogate-hash"), false);
    let files =
        parse_json(r#"[{"clientFileId":"f","name":"f","relativePath":"f","size":1}]"#).unwrap();
    let create = |key: JsString| {
        h.gateway
            .create(&object([
                ("projectKey", text("p")),
                ("files", files.clone()),
                ("idempotencyKey", JsValue::String(key)),
            ]))
            .unwrap()
    };
    let lone = create(JsString::from_utf16(vec![u16::from(b'k'), 0xd800]));
    let fffd = create(JsString::from_utf16(vec![u16::from(b'k'), 0xfffd]));
    assert_eq!(
        string(&lone, "uploadId"),
        string(&fffd, "uploadId"),
        "Node hashes a lone surrogate and U+FFFD to the same UTF-8 bytes"
    );
}

#[test]
fn upload_gateway_part_size_cap_is_checked_before_decoding() {
    let zeros = |bytes: usize| {
        let (full, rem) = (bytes / 3, bytes % 3);
        let mut units = vec![u16::from(b'A'); full * 4 + if rem == 0 { 0 } else { rem + 1 }];
        units.extend(vec![
            u16::from(b'=');
            match rem {
                0 => 0,
                1 => 2,
                _ => 1,
            }
        ]);
        JsValue::String(JsString::from_utf16(units))
    };
    let exact = decode_upload_part(&zeros(MAX_REMOTE_UPLOAD_PART_BYTES)).unwrap();
    assert_eq!(
        exact.len(),
        MAX_REMOTE_UPLOAD_PART_BYTES,
        "exactly 8 MiB must decode"
    );
    match decode_upload_part(&zeros(MAX_REMOTE_UPLOAD_PART_BYTES + 1)) {
        Err(error) => assert_eq!(
            error.code, "UPLOAD_PART_TOO_LARGE",
            "8 MiB + 1 must hit the upload-part cap"
        ),
        Ok(bytes) => panic!(
            "oversized part decoded to {} bytes instead of being rejected by the pre-allocation cap",
            bytes.len()
        ),
    }
}

#[test]
#[ignore = "regenerates from the real TS baseline; set PDRS_BASELINE"]
fn upload_gateway_fixture_regenerates_identically() {
    crate::test_baseline::regenerates_identically("upload-gateway");
}

#[test]
fn upload_gateway_methods_stay_outside_the_default_dispatch_table() {
    for method in [
        "upload_create",
        "upload_get",
        "upload_part",
        "upload_complete",
        "upload_cancel",
    ] {
        let request = object([
            ("type", text("request")),
            ("id", text("1")),
            ("method", text(method)),
        ]);
        assert!(!crate::dispatch::is_local(&request), "{method}");
    }
}
