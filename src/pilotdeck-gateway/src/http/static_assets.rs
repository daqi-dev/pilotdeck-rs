//! Terminal HTTP routes and bounded, descriptor-backed static streaming.
use crate::{Body, ProxyError};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming, header};
use std::{
    convert::Infallible,
    fs::File,
    future::Future,
    io::{self, Read},
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::task::JoinHandle;

/// `Ok(None)` declines the webhook and continues the normal static/404 fallback.
pub type FeishuWebhook = Arc<
    dyn Fn(
            Request<String>,
        )
            -> Pin<Box<dyn Future<Output = Result<Option<Response<Body>>, ProxyError>> + Send>>
        + Send
        + Sync,
>;

pub type WecomWebhook = FeishuWebhook;

const WEBHOOK_BODY_LIMIT: usize = 2 * 1024 * 1024;
const FILE_CHUNK_SIZE: usize = 64 * 1024;

pub(super) async fn handle(request: Request<Incoming>, state: &super::HttpState) -> Response<Body> {
    handle_request(request, state).await
}

async fn dispatch_channel_webhook<B>(
    request: Request<B>,
    callback: Option<&super::FeishuWebhook>,
) -> Option<Response<Body>>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let callback = callback?;
    let (parts, mut body) = request.into_parts();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Some(plain(StatusCode::BAD_REQUEST, "invalid request body"));
        };
        if let Some(data) = frame.data_ref() {
            if data.len() > WEBHOOK_BODY_LIMIT - bytes.len() {
                return Some(plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"));
            }
            bytes.extend_from_slice(data);
        }
    }
    let request = Request::from_parts(parts, String::from_utf8_lossy(&bytes).into_owned());
    match callback(request).await {
        Ok(Some(response)) => Some(response),
        Ok(None) => None,
        Err(_) => Some(plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "webhook failed",
        )),
    }
}

async fn handle_request<B>(request: Request<B>, state: &super::HttpState) -> Response<Body>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let base = url::Url::parse("http://127.0.0.1").expect("constant base URL");
    let path = match base.join(&request.uri().to_string()) {
        Ok(url) => url.path().to_owned(),
        Err(_) => return plain(StatusCode::BAD_REQUEST, "invalid request URL"),
    };
    match path.as_str() {
        "/health" => return super::json(StatusCode::OK, serde_json::json!({"ok": true})),
        "/auth/local-token" => {
            return if state.loopback {
                super::json(StatusCode::OK, serde_json::json!({"token": state.token}))
            } else {
                plain(StatusCode::NOT_FOUND, "not found")
            };
        }
        "/feishu/webhook" => {
            if let Some(response) = dispatch_channel_webhook(request, state.feishu_webhook.as_ref()).await
            {
                return response;
            }
        }
        "/wecom/webhook" => {
            if let Some(response) = dispatch_channel_webhook(request, state.wecom_webhook.as_ref()).await
            {
                return response;
            }
        }
        _ => {}
    }
    let Some(root) = &state.static_root else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    let root = if root.is_absolute() {
        root.clone()
    } else {
        state.cwd.join(root)
    };
    match tokio::task::spawn_blocking(move || open_asset(&root, &path)).await {
        Ok(Ok(Some((file, content_type)))) => {
            let mut response = Response::new(
                FileBody {
                    file: Some(file),
                    pending: None,
                }
                .boxed(),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                content_type.parse().expect("static MIME"),
            );
            response
        }
        Ok(Ok(None)) => plain(StatusCode::NOT_FOUND, "not found"),
        Ok(Err(_)) | Err(_) => plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "static asset unavailable",
        ),
    }
}

fn plain(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(message.as_bytes()))
            .map_err(|never: Infallible| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn content_type(path: &Path) -> &'static str {
    match path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension)
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn missing(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ENOTDIR)
}

fn open_asset(root: &Path, path: &str) -> io::Result<Option<(File, &'static str)>> {
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    // URL normalization already removed dot segments; keep percent escapes literal.
    let candidate = root.join(if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    });
    if let Some(file) = open_file(&root, &candidate)? {
        return Ok(Some((file, content_type(&candidate))));
    }
    let index = root.join("index.html");
    Ok(open_file(&root, &index)?.map(|file| (file, content_type(&index))))
}

fn open_file(root: &Path, path: &Path) -> io::Result<Option<File>> {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    // Component containment rejects sibling prefixes and out-of-root symlinks.
    if !canonical.starts_with(root) {
        return Ok(None);
    }
    let metadata = canonical.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let file = crate::project_files::open_beneath(root, &canonical)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "static asset changed type",
        ));
    }
    Ok(Some(file))
}

struct FileBody {
    file: Option<File>,
    pending: Option<JoinHandle<(File, io::Result<Bytes>)>>,
}
impl hyper::body::Body for FileBody {
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        if self.pending.is_none() {
            let Some(mut file) = self.file.take() else {
                return Poll::Ready(None);
            };
            // Only one bounded read is in flight; no eager file buffering.
            self.pending = Some(tokio::task::spawn_blocking(move || {
                let mut buffer = vec![0; FILE_CHUNK_SIZE];
                let result = loop {
                    match file.read(&mut buffer) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => {
                            break result.map(|length| {
                                buffer.truncate(length);
                                Bytes::from(buffer)
                            });
                        }
                    }
                };
                (file, result)
            }));
        }
        let result = match Pin::new(self.pending.as_mut().expect("pending file read")).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        self.pending = None;
        match result {
            Ok((file, Ok(bytes))) if !bytes.is_empty() => {
                self.file = Some(file);
                Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))))
            }
            Ok((_, Ok(_))) => Poll::Ready(None),
            Ok((_, Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Err(error) => Poll::Ready(Some(Err(Box::new(error)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.file.is_none() && self.pending.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::body::Body as _;
    use std::{fs, path::PathBuf};

    fn state(root: Option<PathBuf>) -> super::super::HttpState {
        let mut state = super::super::HttpState::new("test-token".into(), None, PathBuf::from("/"));
        state.static_root = root;
        state.loopback = true;
        state.feishu_webhook = None;
        state
    }

    async fn get(path: &str, state: &super::super::HttpState) -> Response<Body> {
        handle_request(
            Request::builder()
                .uri(path)
                .body(Full::new(Bytes::new()))
                .unwrap(),
            state,
        )
        .await
    }

    async fn bytes(response: Response<Body>) -> Bytes {
        response.into_body().collect().await.unwrap().to_bytes()
    }

    #[tokio::test]
    async fn health_token_and_unmatched_responses_match_baseline() {
        let mut state = state(None);
        let response = get("/health?probe=1", &state).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(bytes(response).await, r#"{"ok":true}"#);
        assert_eq!(
            bytes(get("/a/../health", &state).await).await,
            r#"{"ok":true}"#
        );
        assert_eq!(
            bytes(get("/%2e/health", &state).await).await,
            r#"{"ok":true}"#
        );
        assert_eq!(
            bytes(get("/auth/local-token", &state).await).await,
            r#"{"token":"test-token"}"#
        );
        state.loopback = false;
        for path in ["/auth/local-token", "/feishu/webhook", "/missing"] {
            let response = get(path, &state).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/plain; charset=utf-8"
            );
            assert_eq!(bytes(response).await, "not found");
        }
    }

    #[tokio::test]
    async fn static_mime_normalization_and_spa_fallback() {
        let scratch = crate::scratch::Scratch::new("http-static");
        fs::write(scratch.path().join("index.html"), "SPA").unwrap();
        let mut state = state(Some(PathBuf::from(".")));
        state.cwd = scratch.path().into();
        for (file, mime) in [
            ("a.html", "text/html; charset=utf-8"),
            (".html", "text/html; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.json", "application/json; charset=utf-8"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "application/octet-stream"),
            ("upper.JS", "application/octet-stream"),
        ] {
            fs::write(scratch.path().join(file), file).unwrap();
            // TS resolve(join(...)) strips a trailing slash before checking the file.
            for path in [format!("/dir/../../{file}?x=1"), format!("/{file}/")] {
                let response = get(&path, &state).await;
                assert_eq!(response.status(), StatusCode::OK, "{path}");
                assert_eq!(response.headers()[header::CONTENT_TYPE], mime, "{path}");
                assert_eq!(bytes(response).await.as_ref(), file.as_bytes(), "{path}");
            }
        }
        fs::create_dir(scratch.path().join("folder")).unwrap();
        fs::write(scratch.path().join("%61.js"), "literal").unwrap();
        assert_eq!(bytes(get("/%61.js", &state).await).await, "literal");
        for path in [
            "/",
            "/folder",
            "/index.html/",
            "/a.js/child",
            "/unknown/nested",
            "/feishu/webhook",
        ] {
            let response = handle_request(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
                &state,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
            assert_eq!(bytes(response).await, "SPA");
        }
        state.loopback = false;
        assert_eq!(
            get("/auth/local-token", &state).await.status(),
            StatusCode::NOT_FOUND
        );
        fs::remove_file(scratch.path().join("index.html")).unwrap();
        let response = get("/a.js/", &state).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(bytes(response).await, "a.js");
        assert_eq!(
            get("/missing", &state).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn configured_webhook_handles_declines_and_errors_locally() {
        let scratch = crate::scratch::Scratch::new("http-webhook");
        fs::write(scratch.path().join("index.html"), "SPA").unwrap();
        let mut state = state(Some(scratch.path().into()));
        state.feishu_webhook = Some(Arc::new(|request| {
            Box::pin(async move {
                assert_eq!(request.method(), "POST");
                assert_eq!(request.uri(), "/feishu/webhook?probe=1");
                assert_eq!(request.headers()["x-probe"], "header");
                assert_eq!(request.body(), "event�");
                Ok(Some(super::super::json(
                    StatusCode::ACCEPTED,
                    serde_json::json!({"handled": true}),
                )))
            })
        }));
        let request = Request::builder()
            .method("POST")
            .uri("/feishu/webhook?probe=1")
            .header("x-probe", "header")
            .body(Full::new(Bytes::from_static(b"event\xff")))
            .unwrap();
        let response = handle_request(request, &state).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(bytes(response).await, r#"{"handled":true}"#);
        state.feishu_webhook = Some(Arc::new(|_| Box::pin(async { Ok(None) })));
        assert_eq!(bytes(get("/feishu/webhook", &state).await).await, "SPA");
        state.static_root = None;
        assert_eq!(
            get("/feishu/webhook", &state).await.status(),
            StatusCode::NOT_FOUND
        );
        state.feishu_webhook = Some(Arc::new(|_| {
            Box::pin(async { Err(io::Error::other("private detail").into()) })
        }));
        let response = get("/feishu/webhook", &state).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(bytes(response).await, "webhook failed");
    }

    #[tokio::test]
    async fn webhook_body_limit_and_read_errors_do_not_reach_callback() {
        let mut state = state(None);
        state.feishu_webhook = Some(Arc::new(|_| panic!("invalid body reached callback")));
        let request = Request::builder()
            .uri("/feishu/webhook")
            .body(Full::new(Bytes::from(vec![0; WEBHOOK_BODY_LIMIT + 1])))
            .unwrap();
        assert_eq!(
            handle_request(request, &state).await.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        struct Broken;
        impl hyper::body::Body for Broken {
            type Data = Bytes;
            type Error = io::Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
                Poll::Ready(Some(Err(io::Error::other("broken input"))))
            }
        }
        let request = Request::builder()
            .uri("/feishu/webhook")
            .body(Broken)
            .unwrap();
        assert_eq!(
            handle_request(request, &state).await.status(),
            StatusCode::BAD_REQUEST
        );
        state.feishu_webhook = Some(Arc::new(|request| {
            Box::pin(async move {
                assert_eq!(request.body().len(), WEBHOOK_BODY_LIMIT);
                Ok(Some(plain(StatusCode::ACCEPTED, "accepted")))
            })
        }));
        let request = Request::builder()
            .uri("/feishu/webhook")
            .body(Full::new(Bytes::from(vec![b'a'; WEBHOOK_BODY_LIMIT])))
            .unwrap();
        assert_eq!(
            handle_request(request, &state).await.status(),
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test]
    async fn static_reads_are_streamed_and_keep_the_opened_descriptor() {
        use std::io::{Seek, SeekFrom, Write};
        let scratch = crate::scratch::Scratch::new("http-stream");
        let path = scratch.path().join("large.bin");
        fs::write(&path, vec![b'a'; FILE_CHUNK_SIZE * 2 + 7]).unwrap();
        let state = state(Some(scratch.path().into()));
        let mut body = get("/large.bin", &state).await.into_body();
        assert_eq!(body.size_hint().exact(), None);
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first.len(), FILE_CHUNK_SIZE);
        assert!(first.iter().all(|&byte| byte == b'a'));
        let mut writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        writer
            .seek(SeekFrom::Start(FILE_CHUNK_SIZE as u64))
            .unwrap();
        writer.write_all(&vec![b'b'; FILE_CHUNK_SIZE + 7]).unwrap();
        drop(writer);
        fs::remove_file(&path).unwrap();
        let remainder = body.collect().await.unwrap().to_bytes();
        assert_eq!(remainder.len(), FILE_CHUNK_SIZE + 7);
        assert!(remainder.iter().all(|&byte| byte == b'b'));
    }

    #[tokio::test]
    async fn file_stream_read_errors_and_empty_files_terminate() {
        let scratch = crate::scratch::Scratch::new("http-stream-error");
        let mut broken = FileBody {
            file: Some(File::open(scratch.path()).unwrap()),
            pending: None,
        };
        assert!(broken.frame().await.unwrap().is_err());
        assert!(broken.frame().await.is_none());
        assert!(broken.is_end_stream());
        let path = scratch.path().join("empty");
        fs::write(&path, []).unwrap();
        let mut empty = FileBody {
            file: Some(File::open(path).unwrap()),
            pending: None,
        };
        assert!(empty.frame().await.is_none());
        assert!(empty.is_end_stream());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn static_symlinks_cannot_escape_root_or_redirect_checked_opens() {
        use std::os::unix::fs::symlink;
        let scratch = crate::scratch::Scratch::new("http-containment");
        let root = scratch.path().join("assets");
        let sibling = scratch.path().join("assets-other");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&sibling).unwrap();
        fs::write(root.join("index.html"), "SPA").unwrap();
        fs::write(root.join("local.js"), "local").unwrap();
        fs::write(sibling.join("secret"), "private").unwrap();
        symlink(sibling.join("secret"), root.join("outside")).unwrap();
        symlink(root.join("local.js"), root.join("inside.css")).unwrap();
        let state = state(Some(root.clone()));
        assert_eq!(bytes(get("/outside", &state).await).await, "SPA");
        let response = get("/inside.css", &state).await;
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
        assert_eq!(bytes(response).await, "local");
        let canonical_root = root.canonicalize().unwrap();
        let checked = root.join("local.js").canonicalize().unwrap();
        fs::remove_file(root.join("local.js")).unwrap();
        symlink(sibling.join("secret"), root.join("local.js")).unwrap();
        assert!(crate::project_files::open_beneath(&canonical_root, &checked).is_err());
        fs::remove_file(root.join("index.html")).unwrap();
        symlink(sibling.join("secret"), root.join("index.html")).unwrap();
        assert_eq!(
            get("/unknown", &state).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}
