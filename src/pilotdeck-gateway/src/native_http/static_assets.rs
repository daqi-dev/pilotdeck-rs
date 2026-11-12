//! Native HTTP static asset lookup and streamed file responses.
use crate::{Body, ProxyError};
use bytes::Bytes;
use http_body_util::BodyExt;
use std::{
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
    time::SystemTime,
};
use tokio::io::{AsyncRead, ReadBuf};

/// The static extension map, exactly `staticAssets.ts`'s `CONTENT_TYPES`:
/// the lookup is case-sensitive (`.TXT` misses) and keyed on the final
/// served file's extension (the index fallback serves `.html`).
const CONTENT_TYPES: [(&str, &str); 5] = [
    (".html", "text/html; charset=utf-8"),
    (".js", "text/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".json", "application/json; charset=utf-8"),
    (".svg", "image/svg+xml"),
];

/// A streaming file response with the same framing as
/// `createReadStream().pipe(response)`.
fn file_response(
    content_type: &str,
    file: tokio::fs::File,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let mut builder = hyper::Response::builder()
        .status(hyper::StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(
            hyper::header::DATE,
            httpdate::fmt_http_date(SystemTime::now()),
        );
    if keep_alive {
        builder = builder
            .header(hyper::header::CONNECTION, "keep-alive")
            .header("keep-alive", "timeout=5");
    } else {
        builder = builder.header(hyper::header::CONNECTION, "close");
    }
    builder
        .body(FileBody { file, done: false }.boxed())
        .expect("valid response")
}

/// Streams one frame per 64 KiB read, the cadence of Node's createReadStream
/// `highWaterMark`. Chunk boundaries of files larger than one buffer are
/// outside the golden matrix.
struct FileBody {
    file: tokio::fs::File,
    done: bool,
}
impl hyper::body::Body for FileBody {
    type Data = Bytes;
    type Error = ProxyError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, ProxyError>>> {
        if self.done {
            return Poll::Ready(None);
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let mut read = ReadBuf::new(&mut buffer);
        match Pin::new(&mut self.file).poll_read(context, &mut read) {
            Poll::Ready(Ok(())) => {
                let read = read.filled().len();
                if read == 0 {
                    self.done = true;
                    Poll::Ready(None)
                } else {
                    buffer.truncate(read);
                    Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from(buffer)))))
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(Box::new(error) as ProxyError))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// `serveStaticAsset` semantics for one already-normalized path: `/` maps to
/// `index.html`, a missing path or directory falls back to the root
/// `index.html`, and the containment guard rejects anything that would
/// escape the root (defense in depth behind the URL layer).
pub(super) async fn serve_static(
    root: &Path,
    path: &str,
    keep_alive: bool,
) -> Option<hyper::Response<Body>> {
    let pathname = if path == "/" { "/index.html" } else { path };
    let candidate = node_join(root, pathname);
    if !crate::mutants::on("native-http-containment-off") && !contained(root, &candidate) {
        return None;
    }
    let file_path = if is_file(&candidate).await {
        candidate
    } else {
        let fallback = node_join(root, "/index.html");
        if !is_file(&fallback).await {
            return None;
        }
        fallback
    };
    let file = tokio::fs::File::open(&file_path).await.ok()?;
    Some(file_response(
        content_type_for(&file_path),
        file,
        keep_alive,
    ))
}

/// `resolve(staticRoot)` (staticAssets.ts:14): lexical fold of `.`/`..` and
/// trailing separators, made absolute against the process directory for a
/// relative root. No `canonicalize`, so symlinks stay the caller's choice.
/// A root of `/` is kept: TS's guard then 404s everything (the required
/// prefix would be `//`), and a failed cwd lookup maps to `/` so nothing
/// escapes through an empty prefix.
pub(super) fn normalize_root(root: &Path) -> PathBuf {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(root)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Node's `resolve(join(root, pathname))`: lexical join where `.` keeps the
/// position and `..` pops, exactly what `path.resolve` folds so the
/// containment guard sees the final path a traversal would name.
pub(super) fn node_join(root: &Path, pathname: &str) -> PathBuf {
    let mut joined = root.to_path_buf();
    for segment in pathname.split('/').filter(|segment| !segment.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                joined.pop();
            }
            _ => joined.push(segment),
        }
    }
    joined
}

/// `candidate !== root && !candidate.startsWith(root + sep)`, TS's own guard.
pub(super) fn contained(root: &Path, candidate: &Path) -> bool {
    match (root.to_str(), candidate.to_str()) {
        (Some(root), Some(candidate)) => {
            candidate == root || candidate.starts_with(&format!("{root}/"))
        }
        // Non-UTF-8 paths never reach this point in practice; reject them.
        _ => false,
    }
}

async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
}

/// `CONTENT_TYPES[filePath.slice(filePath.lastIndexOf("."))]` including the
/// JavaScript quirk that a path without a dot slices the final character,
/// which never matches an extension key.
pub(super) fn content_type_for(file_path: &Path) -> &'static str {
    let name = file_path.to_string_lossy();
    let extension = name.rfind('.').map_or("", |at| &name[at..]);
    if crate::mutants::on("native-http-content-type-txt") && extension == ".txt" {
        // #1383: simulates the mapping gaining a `.txt` entry, which would
        // break the premise `expected_response` borrows the `static-upper`
        // golden under — the pinned test fails at its own assertion.
        "text/plain; charset=utf-8"
    } else {
        CONTENT_TYPES
            .iter()
            .find(|(known, _)| *known == extension)
            .map_or("application/octet-stream", |(_, kind)| kind)
    }
}
