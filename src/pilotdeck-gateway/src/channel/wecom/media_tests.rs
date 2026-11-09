use super::*;
use sha2::Sha256;
use std::sync::Mutex;
fn gold() -> serde_json::Value {
    serde_json::from_str(include_str!("media-oracle.json")).unwrap()
}
fn value(v: &JsValue) -> serde_json::Value {
    serde_json::from_str(&pilotdeck_protocol::stringify_json(v).unwrap().unwrap()).unwrap()
}
#[test]
fn prepared_limits_and_decryption_match_fixed_ts() {
    let gold = gold();
    assert_eq!(gold["fixed"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85");
    for row in gold["prepare"].as_array().unwrap() {
        let prepared = prepare_bytes(
            vec![65; row["size"].as_u64().unwrap() as usize],
            row["mime"].as_str().unwrap(),
            row["name"].as_str().unwrap(),
        );
        assert_eq!(prepared.content_type, row["contentType"]);
        assert_eq!(prepared.file_name, row["fileName"]);
        assert_eq!(prepared.detected_type, row["detectedType"]);
        assert_eq!(prepared.final_type, row["finalType"]);
        assert_eq!(
            prepared.reject_reason.as_deref(),
            row["rejectReason"].as_str()
        );
        assert_eq!(
            prepared.downgrade_note.as_deref(),
            row["downgradeNote"].as_str()
        );
    }
    let row = &gold["decrypt"];
    assert_eq!(
        decrypt_bytes(
            &node_base64(row["encrypted"].as_str().unwrap()),
            row["key"].as_str().unwrap()
        )
        .unwrap(),
        row["plain"].as_str().unwrap().as_bytes()
    );
    assert!(
        decrypt_bytes(&[0; 16], "YQ")
            .unwrap_err()
            .to_string()
            .contains("expected 32 bytes, got 1")
    );
    assert!(decrypt_bytes(&[0; 16], row["key"].as_str().unwrap()).is_err());
    assert_eq!(node_base64("QUJD=RA=="), b"ABC");
    assert_eq!(node_base64("A"), Vec::<u8>::new());
}
struct Port {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    fail: Option<String>,
}
impl MediaRequestPort for Port {
    fn request(&self, cmd: JsString, body: JsValue) -> BoxFuture<Result<JsValue, ChannelError>> {
        let cmd = shown(&cmd);
        let mut body = value(&body);
        if let Some(raw) = body
            .get("base64_data")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            let bytes = node_base64(&raw);
            body.as_object_mut().unwrap().remove("base64_data");
            body["chunkBytes"] = bytes.len().into();
            body["chunkSha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
        }
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::json!({"cmd":cmd,"body":body}));
        let fail = self
            .fail
            .as_ref()
            .is_some_and(|fail| cmd.ends_with(&format!("_{fail}")));
        Box::pin(async move {
            Ok(if fail {
                obj([(
                    "body",
                    obj([
                        ("errcode", JsValue::Number(3.0)),
                        ("errmsg", js("mock fail")),
                    ]),
                )])
            } else if cmd.ends_with("_init") {
                obj([("body", obj([("upload_id", js("upload-1"))]))])
            } else if cmd.ends_with("_finish") {
                obj([("body", obj([("media_id", js("media-1"))]))])
            } else {
                obj([])
            })
        })
    }
}
#[tokio::test]
async fn upload_chunks_md5_and_failures_match_fixed_ts() {
    for row in gold()["upload"].as_array().unwrap() {
        let requests = Arc::new(Mutex::new(vec![]));
        let port = Port {
            requests: requests.clone(),
            fail: row["fail"].as_str().map(str::to_owned),
        };
        let prepared = prepare_bytes(vec![65; 524289], "application/pdf", "sample.pdf");
        let actual = upload_media(&port, &prepared).await;
        assert_eq!(
            &*requests.lock().unwrap(),
            row["requests"].as_array().unwrap()
        );
        if let Some(error) = row["error"].as_str() {
            let actual = actual.unwrap_err();
            // TS capture explicitly records e.message; Display is String(e).
            assert_eq!(actual.message(), error);
            assert_eq!(actual.to_string(), format!("Error: {error}"));
        } else {
            assert_eq!(value(&actual.unwrap()), row["result"]);
        }
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-wecom-media-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn inbound_decrypt_cache_permissions_and_path_extraction_match_fixed_ts() {
    let root = Scratch::new();
    let gold = gold();
    let cache = MediaClient::new(
        root.0.join("cache"),
        Arc::new(|| 123),
        Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
    );
    let media = &gold["inbound"][0]["media"];
    let media = pilotdeck_protocol::parse_json(&media.to_string()).unwrap();
    let result = cache.cache_inbound("image", &media).await.unwrap().unwrap();
    let path = result
        .get("path")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        gold["inbound"][0]["result"]["bytesSha256"]
    );
    assert_eq!(result.get("name"), Some(&js("测试_.png")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let paths = root.0.join("paths");
    std::fs::create_dir_all(paths.join(".git")).unwrap();
    std::fs::write(paths.join("a.pdf"), "file").unwrap();
    std::fs::write(paths.join(".git/a.pdf"), "secret").unwrap();
    for row in gold["extract"].as_array().unwrap() {
        let input = row["input"]
            .as_str()
            .unwrap()
            .replace("$PATHS", &paths.to_string_lossy());
        let actual =
            extract_deliverables(&input.into(), &root.0.join("home"), &root.0.join("pilot")).await;
        let canonical_paths = std::fs::canonicalize(&paths).unwrap();
        let deliverables=actual.deliverables.iter().map(|d|{
            let relative=d.path.strip_prefix(&canonical_paths).expect("deliverable must remain inside the owned canonical fixture directory");
            serde_json::json!({"path":format!("$PATHS/{}",relative.display()),"mediaType":d.media_type})
        }).collect::<Vec<_>>();
        assert_eq!(
            &deliverables,
            row["result"]["deliverables"].as_array().unwrap()
        );
        assert_eq!(
            serde_json::json!(actual.warnings),
            row["result"]["warnings"]
        );
    }
}

#[tokio::test]
async fn native_download_redirect_headers_cache_and_size_guard() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for _ in 0..5 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = vec![];
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            assert!(headers.to_lowercase().contains("user-agent: pilotdeck/1.0"));
            assert!(headers.to_lowercase().contains("accept: */*"));
            let path = headers
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let response=match path{
    "/redirect"=>"HTTP/1.1 302 Found\r\nLocation: /bytes\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
    "/bytes"=>"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=\"remote.png\"\r\nContent-Length: 12\r\nConnection: close\r\n\r\nremote-bytes".to_owned(),
    "/large"=>format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",FILE_MAX_BYTES+1),
    _=>"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
   };
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let root = Scratch::new();
    let client = MediaClient::new(
        root.0.clone(),
        Arc::new(|| 123),
        Arc::new(|| "test-uuid".into()),
    );
    let prepared = client
        .load_source(&format!("{origin}/redirect"), None)
        .await
        .unwrap();
    assert_eq!(prepared.data, b"remote-bytes");
    assert_eq!(prepared.file_name, "remote.png");
    assert_eq!(prepared.final_type, "image");
    let cached = client
        .cache_inbound("file", &obj([("url", js(format!("{origin}/bytes")))]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached.get("bytes"), Some(&JsValue::Number(12.0)));
    assert!(
        client
            .download(&format!("{origin}/large"))
            .await
            .unwrap_err()
            .to_string()
            .contains("remote media exceeds limit")
    );
    assert_eq!(
        client
            .download(&format!("{origin}/error"))
            .await
            .unwrap_err()
            .message(),
        "HTTP 404"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn inbound_source_failures_are_distinct_from_cache_io_failures() {
    let root = Scratch::new();
    let path = root.0.join("not-a-directory");
    std::fs::write(&path, b"owned").unwrap();
    let client = MediaClient::new(path, Arc::new(|| 0), Arc::new(|| "id".into()));
    let failure = client
        .cache_inbound("file", &obj([("base64", js("YQ=="))]))
        .await
        .unwrap_err();
    assert!(
        failure.is_storage(),
        "cache filesystem errors must propagate, never skip attachment"
    );
    let failure = client
        .cache_inbound("file", &obj([("base64", js("YQ==")), ("aeskey", js("YQ"))]))
        .await
        .unwrap_err();
    assert!(!failure.is_storage(), "decrypt failure is warning-and-skip");
}
