//! Focused cells for the two protocol ports. The oracle replay covers the
//! weixin callers end to end; these pin the helpers no golden case reaches
//! (name/path sanitizing, base64 leniency, the fallback text) and the
//! `saveFromUrl` order that only a mock transport can drive.

use super::*;
use std::sync::Mutex;

fn scratch(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("pdrs-1822-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("scratch dir");
    path
}

struct FixedFetcher {
    bytes: Vec<u8>,
    content_type: Option<String>,
    calls: Mutex<Vec<String>>,
}
impl FixedFetcher {
    fn new(bytes: &[u8], content_type: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            bytes: bytes.to_vec(),
            content_type: content_type.map(str::to_string),
            calls: Mutex::new(Vec::new()),
        })
    }
}
impl AttachmentFetcher for FixedFetcher {
    fn fetch<'a>(&'a self, url: &'a str, _headers: &'a HeaderMap) -> AttachmentFetchFuture<'a> {
        self.calls.lock().unwrap().push(url.to_string());
        Box::pin(async move {
            Ok(FetchedAttachment {
                bytes: self.bytes.clone(),
                content_type: self.content_type.clone(),
            })
        })
    }
}

fn input(url: &str, kind: AttachmentKind) -> SaveAttachmentFromUrlInput {
    SaveAttachmentFromUrlInput {
        url: url.into(),
        headers: HeaderMap::new(),
        chat_id: "chat-1".into(),
        message_id: "msg-1".into(),
        kind,
        name: None,
        mime_type: None,
        bytes: None,
        metadata: Vec::new(),
        transform: None,
    }
}

fn store_with(dir: &Path, fetcher: Arc<FixedFetcher>) -> ImAttachmentStore {
    ImAttachmentStore::new(dir, "weixin", DEFAULT_MAX_BYTES, fetcher)
}

#[test]
fn safe_path_part_matches_the_js_class_and_unit_slice() {
    assert_eq!(safe_path_part("wxid_abc-1.2"), "wxid_abc-1.2");
    assert_eq!(safe_path_part("a b/c"), "a_b_c");
    assert_eq!(safe_path_part("a   b"), "a_b");
    assert_eq!(safe_path_part(""), "unknown");
    // A run of forbidden characters collapses to a single `_`, which is
    // truthy, so the `unknown` fallback is not reached.
    assert_eq!(safe_path_part("///"), "_");
    // 121 allowed units: `.slice(0, 120)` stops at 120. (A non-ASCII run
    // would collapse to one `_` first, so the slice is exercised in ASCII.)
    let long = "a".repeat(121);
    assert_eq!(safe_path_part(&long), "a".repeat(120));
}

#[test]
fn safe_filename_follows_the_baseline_branches() {
    // Image: the extension is replaced from the mime, and a missing name still
    // yields `image.<ext>`.
    assert_eq!(
        safe_filename(Some("photo.heic"), Some("image/png"), AttachmentKind::Image),
        "photo.png"
    );
    assert_eq!(
        safe_filename(None, Some("image/jpeg"), AttachmentKind::Image),
        "image.jpg"
    );
    // Non-image without an extension appends one.
    assert_eq!(
        safe_filename(
            Some("report"),
            Some("application/pdf"),
            AttachmentKind::File
        ),
        "report.pdf"
    );
    // A basename with an extension is kept verbatim.
    assert_eq!(
        safe_filename(Some("notes.tar.gz"), None, AttachmentKind::File),
        "notes.tar.gz"
    );
    // Forbidden characters collapse into a single `_` per run, and the
    // directory part is dropped by `basename`.
    assert_eq!(
        safe_filename(Some("../../etc/passwd"), None, AttachmentKind::File),
        "passwd.bin"
    );
    assert_eq!(
        safe_filename(Some("a:b*c?.txt"), Some("text/plain"), AttachmentKind::File),
        "a_b_c_.txt"
    );
    // `/^\./` is not an extension: `.txt` becomes `.txt.txt`.
    assert_eq!(
        safe_filename(Some(".txt"), Some("text/plain"), AttachmentKind::File),
        ".txt.txt"
    );
    // Names that survive `safeFilename` but still start with `..` reach the
    // containment guard: Node's `path.relative` returns them verbatim and
    // `startsWith("..")` rejects them.
    assert_eq!(
        safe_filename(Some("..foo.txt"), None, AttachmentKind::File),
        "..foo.txt"
    );
}

#[test]
fn containment_matches_nodes_relative_prefix_rule() {
    let dir = Path::new("/tmp/root/weixin/chat/msg");
    // Inside.
    assert!(is_path_within_directory(&dir.join("report.pdf"), dir));
    assert!(is_path_within_directory(dir, dir));
    // Node probe (v22.22.0) for `["..foo.txt","..","..."]`:
    // rel = the name itself and `rel.startsWith("..")` is true for all three.
    for name in ["..foo.txt", "..", "...", "...bin"] {
        assert!(
            !is_path_within_directory(&dir.join(name), dir),
            "{name} must be rejected"
        );
    }
    // A candidate outside the directory is rejected by the prefix itself.
    assert!(!is_path_within_directory(
        Path::new("/tmp/root/other/f.txt"),
        dir
    ));
}

#[test]
fn node_base64_semantics() {
    assert_eq!(node_base64_decode("aGk="), b"hi");
    // Characters outside the alphabet are skipped, not fatal.
    assert_eq!(node_base64_decode("a!G\r\nk="), b"hi");
    // URL-safe aliases decode to the same sextets as `+` and `/`.
    assert_eq!(node_base64_decode("-_8="), node_base64_decode("+/8="));
    // A lone trailing character carries no byte.
    assert_eq!(node_base64_decode("A"), Vec::<u8>::new());
    assert_eq!(node_base64_decode(""), Vec::<u8>::new());
    // `=` ends the input; trailing data is ignored.
    assert_eq!(node_base64_decode("aGk=ZZZZ"), b"hi");
}

#[test]
fn mime_guesses_match_the_baseline_tables() {
    assert_eq!(guess_mime_type_from_name(Some("A.PNG")), Some("image/png"));
    assert_eq!(
        guess_mime_type_from_name(Some("a.jpeg")),
        Some("image/jpeg")
    );
    assert_eq!(guess_mime_type_from_name(Some("a.log")), Some("text/plain"));
    assert_eq!(
        guess_mime_type_from_name(Some("a.md")),
        Some("text/markdown")
    );
    assert_eq!(guess_mime_type_from_name(Some("a.bin")), None);
    assert_eq!(guess_mime_type_from_name(None), None);
    assert_eq!(
        guess_mime_type_from_url("https://x.test/a/b.png?sig=1", Some("image/jpeg")).as_deref(),
        Some("image/png")
    );
    assert_eq!(
        guess_mime_type_from_url("https://x.test/a/b.bin", Some("image/jpeg")).as_deref(),
        Some("image/jpeg")
    );
    assert_eq!(guess_mime_type_from_url("not a url", None), None);
}

#[test]
fn fallback_text_names_the_path_only_when_present() {
    let with_path = OutboundAttachment {
        path: Some("/tmp/a/b.pdf".into()),
        ..OutboundAttachment::default()
    };
    assert_eq!(
        format_im_attachment_fallback(&with_path),
        "附件发送失败：b.pdf，可在本机查看：/tmp/a/b.pdf"
    );
    let without = OutboundAttachment {
        name: Some("报告.docx".into()),
        ..OutboundAttachment::default()
    };
    assert_eq!(
        format_im_attachment_fallback(&without),
        "附件发送失败：报告.docx"
    );
    assert_eq!(
        format_im_attachment_fallback(&OutboundAttachment::default()),
        "附件发送失败：附件"
    );
}

#[tokio::test]
async fn save_from_url_persists_with_the_baseline_metadata_spread() {
    let root = scratch("save");
    let fetcher = FixedFetcher::new(b"\x89PNG\r\n\x1a\nrest", Some("image/png; charset=binary"));
    let store = store_with(&root, fetcher.clone());
    let mut request = input("https://cdn.test/x?y=1", AttachmentKind::Image);
    request.metadata = vec![
        ("itemType".into(), JsValue::String("image".into())),
        // Overrides the base key while keeping its position.
        ("chatId".into(), JsValue::String("overridden".into())),
    ];
    let saved = store.save_from_url(request).await.expect("save");

    assert_eq!(saved.name.as_deref(), Some("image.png"));
    assert_eq!(saved.mime_type.as_deref(), Some("image/png"));
    assert_eq!(saved.bytes, Some(12.0));
    let path = PathBuf::from(saved.path.as_deref().expect("path"));
    assert_eq!(
        std::fs::read(&path).expect("written"),
        b"\x89PNG\r\n\x1a\nrest"
    );
    assert!(path.starts_with(&root));
    assert!(path.ends_with("weixin/chat-1/msg-1/image.png"));
    assert_eq!(
        fetcher.calls.lock().unwrap().as_slice(),
        ["https://cdn.test/x?y=1"]
    );

    let keys: Vec<&str> = saved.metadata.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["channelKey", "chatId", "messageId", "sourceUrl", "itemType"]
    );
    assert_eq!(
        saved.metadata[1].1,
        JsValue::String("overridden".into()),
        "input.metadata wins over the spread base"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn save_from_url_creates_private_paths() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("modes");
    let store = store_with(&root, FixedFetcher::new(b"data", None));
    let saved = store
        .save_from_url(input("https://cdn.test/f", AttachmentKind::File))
        .await
        .expect("save");
    let path = PathBuf::from(saved.path.expect("path"));
    let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("parent")), 0o700);
    assert_eq!(saved.name.as_deref(), Some("attachment.bin"));
}

#[tokio::test]
async fn save_from_url_guards_sizes_and_applies_the_transform() {
    let root = scratch("guards");
    // Declared size wins before the transport is even called.
    let fetcher = FixedFetcher::new(b"x", None);
    let store = store_with(&root, fetcher.clone());
    let mut oversized = input("https://cdn.test/f", AttachmentKind::File);
    oversized.bytes = Some(DEFAULT_MAX_BYTES as f64 + 1.0);
    let error = store.save_from_url(oversized).await.expect_err("declared");
    crate::negative_assert_eq!(
        error.message,
        format!(
            "Attachment https://cdn.test/f is {} bytes (limit {}).",
            DEFAULT_MAX_BYTES + 1,
            DEFAULT_MAX_BYTES
        ),
        "#1822 declared size guard"
    );
    assert!(
        fetcher.calls.lock().unwrap().is_empty(),
        "fetch not attempted"
    );

    // The transform runs before the post-size guard.
    let store = store_with(&root, FixedFetcher::new(b"abcd", None));
    let mut transformed = input("https://cdn.test/f", AttachmentKind::File);
    transformed.transform = Some(Arc::new(|buffer: Vec<u8>| {
        Ok(buffer.iter().rev().copied().collect())
    }));
    let saved = store.save_from_url(transformed).await.expect("save");
    assert_eq!(
        std::fs::read(saved.path.expect("path")).expect("read"),
        b"dcba"
    );

    let store = store_with(&root, FixedFetcher::new(b"abcd", None));
    let mut shrinking = input("https://cdn.test/f", AttachmentKind::File);
    shrinking.transform = Some(Arc::new(|_| Err(AttachmentError::error("boom"))));
    let error = store.save_from_url(shrinking).await.expect_err("transform");
    assert_eq!(error.formatted(), "Error: boom");
}

/// `page 67-72`: the size guard reads the *transformed* buffer, so a
/// transform that shrinks below the limit rescues an oversized download and
/// one that grows past it fails even though the transport stayed under.
#[tokio::test]
async fn save_from_url_checks_the_size_limit_after_the_transform() {
    let root = scratch("post-transform");
    let store = ImAttachmentStore::new(&root, "weixin", 3, FixedFetcher::new(b"12345678", None));
    let mut shrinking = input("https://cdn.test/shrink", AttachmentKind::File);
    shrinking.transform = Some(Arc::new(|buffer: Vec<u8>| Ok(buffer[..3].to_vec())));
    let saved = crate::test_negative_receipt::expect_ok(
        store.save_from_url(shrinking).await,
        "#1822 size gate runs after the transform",
    );
    crate::negative_assert_eq!(saved.bytes, Some(3.0), "#1822 post-transform size");

    let store = ImAttachmentStore::new(&root, "weixin", 3, FixedFetcher::new(b"ab", None));
    let mut growing = input("https://cdn.test/grow", AttachmentKind::File);
    growing.transform = Some(Arc::new(|_| Ok(b"abcdef".to_vec())));
    let error = crate::test_negative_receipt::expect_err(
        store.save_from_url(growing).await,
        "#1822 post-transform size guard rejects",
    );
    crate::negative_assert_eq!(
        error.message,
        "Attachment is 6 bytes (limit 3).",
        "#1822 post-transform size guard"
    );
}

#[tokio::test]
async fn save_from_url_sniffs_images_over_the_response_mime() {
    let root = scratch("sniff");
    // `detectImageMime` wins over the declared header for image attachments
    // only; the gif magic is enough on its own.
    let store = store_with(&root, FixedFetcher::new(b"GIF89a....", Some("image/png")));
    let saved = store
        .save_from_url(input("https://cdn.test/f", AttachmentKind::Image))
        .await
        .expect("save");
    crate::negative_assert_eq!(
        saved.mime_type.as_deref(),
        Some("image/gif"),
        "#1822 image sniff wins over the declared header"
    );
    assert_eq!(saved.name.as_deref(), Some("image.gif"));

    // A non-image attachment keeps whatever the header said, trimmed at `;`.
    let store = store_with(
        &root,
        FixedFetcher::new(b"GIF89a....", Some(" text/plain ; x=1")),
    );
    let saved = store
        .save_from_url(input("https://cdn.test/f", AttachmentKind::File))
        .await
        .expect("save");
    assert_eq!(saved.mime_type.as_deref(), Some("text/plain"));
}

/// `(name, mime_type, buffer, kind, chat_id)` captured by [`RecordingSink`].
type PreparedRecord = (
    String,
    Option<String>,
    Vec<u8>,
    &'static str,
    Option<String>,
);

struct RecordingSink {
    prepared: Mutex<Vec<PreparedRecord>>,
    fallbacks: Mutex<Vec<String>>,
    fail_prepared: bool,
}
impl RecordingSink {
    fn new(fail_prepared: bool) -> Arc<Self> {
        Arc::new(Self {
            prepared: Mutex::new(Vec::new()),
            fallbacks: Mutex::new(Vec::new()),
            fail_prepared,
        })
    }
}
impl ImAttachmentDeliverySink for RecordingSink {
    fn send_prepared(
        &self,
        prepared: PreparedImAttachment,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>> {
        let fail = self.fail_prepared;
        let record = (
            prepared.name,
            prepared.mime_type,
            prepared.buffer,
            prepared.file_type.as_str(),
            prepared.path,
        );
        self.prepared.lock().unwrap().push(record);
        Box::pin(async move {
            if fail {
                Err(AttachmentError::error("upload refused"))
            } else {
                Ok(())
            }
        })
    }
    fn send_text_fallback(
        &self,
        text: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>> {
        self.fallbacks.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

fn delivery_with(
    sink: Arc<RecordingSink>,
    max_bytes: usize,
) -> (ImAttachmentDelivery, Arc<Mutex<Vec<String>>>) {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let logs_out = logs.clone();
    let logger: ImAttachmentErrorLogger = Arc::new(move |line: &str| {
        logs_out.lock().unwrap().push(line.to_string());
    });
    (
        ImAttachmentDelivery::new(ImAttachmentDeliveryOptions {
            max_bytes,
            sink,
            logger: Some(logger),
        }),
        logs,
    )
}

#[tokio::test]
async fn delivery_prepares_content_and_reports_success() {
    let sink = RecordingSink::new(false);
    let (delivery, logs) = delivery_with(sink.clone(), DEFAULT_MAX_BYTES);
    let attachment = OutboundAttachment {
        kind: Some(AttachmentKind::Image),
        name: Some("截图 2026.png".into()),
        content: Some("aGk=".into()),
        source: Some("tool_result".into()),
        ..OutboundAttachment::default()
    };
    assert!(delivery.send(&attachment).await);
    let prepared = sink.prepared.lock().unwrap();
    assert_eq!(prepared.len(), 1);
    assert_eq!(prepared[0].0, "截图 2026.png");
    assert_eq!(prepared[0].2, b"hi");
    assert_eq!(prepared[0].3, "image");
    assert_eq!(prepared[0].1.as_deref(), Some("image/png"));
    drop(prepared);
    assert!(sink.fallbacks.lock().unwrap().is_empty());
    assert!(logs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delivery_falls_back_for_the_three_failure_paths() {
    // 1. A `local_path` source never reaches the sink's upload.
    let sink = RecordingSink::new(false);
    let (delivery, _) = delivery_with(sink.clone(), DEFAULT_MAX_BYTES);
    let attachment = OutboundAttachment {
        source: Some("local_path".into()),
        path: Some("/tmp/x/report.pdf".into()),
        ..OutboundAttachment::default()
    };
    assert!(!delivery.send(&attachment).await);
    assert!(sink.prepared.lock().unwrap().is_empty());
    assert_eq!(
        sink.fallbacks.lock().unwrap().as_slice(),
        ["附件 /tmp/x/report.pdf 需要授权后才能发送。"]
    );

    // 2. Neither content nor path is a hard error: log + fallback text.
    let sink = RecordingSink::new(false);
    let (delivery, logs) = delivery_with(sink.clone(), DEFAULT_MAX_BYTES);
    let attachment = OutboundAttachment {
        name: Some("a.pdf".into()),
        source: Some("media_reference".into()),
        ..OutboundAttachment::default()
    };
    assert!(!delivery.send(&attachment).await);
    assert_eq!(
        sink.fallbacks.lock().unwrap().as_slice(),
        ["附件发送失败：a.pdf"]
    );
    assert_eq!(
        logs.lock().unwrap().as_slice(),
        ["IM attachment send failed: Error: attachment has neither content nor path"]
    );

    // 3. A transport rejection keeps the same fallback shape.
    let sink = RecordingSink::new(true);
    let (delivery, logs) = delivery_with(sink.clone(), DEFAULT_MAX_BYTES);
    let attachment = OutboundAttachment {
        content: Some("aGk=".into()),
        path: Some("/tmp/x/a.bin".into()),
        source: Some("tool_result".into()),
        ..OutboundAttachment::default()
    };
    assert!(!delivery.send(&attachment).await);
    assert_eq!(
        sink.fallbacks.lock().unwrap().as_slice(),
        ["附件发送失败：a.bin，可在本机查看：/tmp/x/a.bin"]
    );
    assert_eq!(
        logs.lock().unwrap().as_slice(),
        ["IM attachment send failed: Error: upload refused"]
    );
}

#[tokio::test]
async fn delivery_reads_paths_and_enforces_the_size_limit() {
    let root = scratch("delivery");
    let path = root.join("report.pdf");
    std::fs::write(&path, b"%PDF-1.4").expect("write");
    let sink = RecordingSink::new(false);
    let (delivery, _logs) = delivery_with(sink.clone(), DEFAULT_MAX_BYTES);
    let attachment = OutboundAttachment {
        path: Some(path.to_string_lossy().into_owned()),
        source: Some("tool_result".into()),
        ..OutboundAttachment::default()
    };
    assert!(delivery.send(&attachment).await);
    {
        let prepared = sink.prepared.lock().unwrap();
        assert_eq!(prepared[0].0, "report.pdf");
        assert_eq!(prepared[0].2, b"%PDF-1.4");
        assert_eq!(prepared[0].3, "file");
        assert_eq!(prepared[0].1.as_deref(), Some("application/pdf"));
    }

    // Over the limit: no upload, fallback + log.
    let sink = RecordingSink::new(false);
    let (delivery, logs) = delivery_with(sink.clone(), 4);
    assert!(!delivery.send(&attachment).await);
    assert!(sink.prepared.lock().unwrap().is_empty());
    assert_eq!(
        logs.lock().unwrap().as_slice(),
        ["IM attachment send failed: Error: attachment report.pdf is 8 bytes (limit 4)"]
    );
    assert_eq!(
        sink.fallbacks.lock().unwrap().as_slice(),
        [format!(
            "附件发送失败：report.pdf，可在本机查看：{}",
            path.display()
        )]
    );
    drop(logs);
}
