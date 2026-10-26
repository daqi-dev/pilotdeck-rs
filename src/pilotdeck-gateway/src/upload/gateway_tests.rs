//! Replays the real-TS gateway oracle and pins projection/ordering rules.
use super::gateway::{UploadGateway, UploadProvider};
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
        project_key: &str,
        files: &JsValue,
        idempotency_key: Option<&JsValue>,
    ) -> Result<UploadRecord> {
        self.log(object([
            ("method", text("create")),
            ("projectKey", text(project_key)),
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
        client_file_id: &str,
        bytes: &[u8],
    ) -> Result<UploadedAttachment> {
        self.log(object([
            ("method", text("writePart")),
            ("uploadId", text(upload_id)),
            ("clientFileId", text(client_file_id)),
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
    let now = Arc::new(AtomicI64::new(EPOCH));
    let resolved = project.to_owned();
    let listed = project.to_owned();
    let mut options = UploadStoreOptions::new(
        move |key| {
            Ok(if key == "missing" {
                resolved.join("absent")
            } else {
                resolved.clone()
            })
        },
        move || Ok(vec![listed.clone()]),
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
        // 4 chars per 3 decoded bytes; fixture sizes are multiples of 3.
        return text(&"A".repeat(4 * (bytes as usize).div_ceil(3)));
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
    assert_eq!(array(&oracle, "scenarios").len(), 7, "scenario coverage");
    assert!(operations >= 130, "operation coverage: {operations}");
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
    assert!(!compact(&attachment).contains("\"path\""));
    h.gateway
        .complete(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    let record = h
        .gateway
        .get(&object([("uploadId", text(&upload_id))]))
        .unwrap();
    assert!(!compact(&record).contains("\"path\""));
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
        ("UPLOAD_NOT_FOUND".into(), "Invalid uploadId.".into())
    );
    // clientFileId wins over contentBase64.
    assert_eq!(
        error(r#"{"uploadId":"upload-1","clientFileId":"  ","contentBase64":null}"#),
        (
            "UPLOAD_MANIFEST_MISMATCH".into(),
            "clientFileId is required.".into()
        )
    );
    // contentBase64 must be valid before the provider is required.
    assert_eq!(
        error(r#"{"uploadId":"upload-1","clientFileId":"file-1","contentBase64":"A"}"#),
        (
            "UPLOAD_PART_INVALID".into(),
            "contentBase64 must be a valid base64 string.".into()
        )
    );
    // create: projectKey, then files, then the store's idempotency hash.
    assert_eq!(
        h.gateway
            .create(&js(
                r#"{"projectKey":null,"files":null,"idempotencyKey":5}"#
            ))
            .unwrap_err()
            .code,
        "PROJECT_NOT_FOUND"
    );
    assert_eq!(
        h.gateway
            .create(&js(
                r#"{"projectKey":"p","files":"not-an-array","idempotencyKey":5}"#
            ))
            .unwrap_err()
            .code,
        "UPLOAD_MANIFEST_INVALID"
    );
    // Unavailable: provider comes first for create/get/complete/cancel.
    let missing = harness(&scratch.project("no-provider"), true);
    let bad = || js(r#"{"uploadId":"../evil"}"#);
    assert_eq!(
        missing.gateway.create(&JsValue::Null).unwrap_err().code,
        "CAPABILITY_UNAVAILABLE"
    );
    for code in [
        missing.gateway.get(&bad()).unwrap_err().code,
        missing.gateway.complete(&bad()).unwrap_err().code,
        missing.gateway.cancel(&bad()).unwrap_err().code,
    ] {
        assert_eq!(code, "CAPABILITY_UNAVAILABLE");
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
        "UPLOAD_NOT_FOUND"
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
    assert_eq!(string(&record, "errorCode").as_deref(), Some("E_RECORDED"));
    assert_eq!(
        string(&record, "errorMessage").as_deref(),
        Some("recorded failure")
    );
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
