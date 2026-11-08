//! Bounded native backing for the existing Always-On stores. Synchronous
//! methods are for `run`'s blocking job; native async callers must use `run`.
use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, renameat, statat, unlinkat};

pub(crate) const INDEX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MARKDOWN_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const EVENT_BYTES: usize = 1024 * 1024;
pub(crate) const LOG_BYTES: usize = 32 * 1024 * 1024;
const JSON_TOKENS: usize = 100_000;
const JSON_DEPTH: usize = 128;
static SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
// All read-modify-write jobs in this backend serialize their short file
// transaction, including independently constructed stores for one project.
// No mutex is held across await or a model/tool invocation.
static TRANSACTION: Mutex<()> = Mutex::new(());

pub(crate) async fn run<T: Send + 'static>(operation: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    run_with(SLOTS.clone(), operation).await
}
async fn run_with<T: Send + 'static>(slots: Arc<tokio::sync::Semaphore>, operation: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    let permit = slots.acquire_owned().await.map_err(|_| io::Error::other("Always-On storage IO admission is closed."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _transaction = TRANSACTION.lock().unwrap_or_else(|error| error.into_inner());
        operation()
    }).await.map_err(|error| io::Error::other(format!("Always-On storage worker failed: {error}")))?
}

#[derive(Debug)]
struct StorageRefusal(&'static str);
impl std::fmt::Display for StorageRefusal { fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str(self.0) } }
impl std::error::Error for StorageRefusal {}
pub(crate) fn is_policy_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidData || error.raw_os_error() == Some(libc::ELOOP)
        || error.get_ref().is_some_and(|error| error.is::<StorageRefusal>())
}

pub(crate) fn budget(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("Always-On {what} exceeds its native resource limit."))
}

/// This preflight executes before parse_json can materialize a broad/deep
/// tree. Quoted text and escaped delimiters do not consume structural depth.
pub(crate) fn check_json(raw: &str) -> io::Result<()> {
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut tokens = 0usize;
    let mut atom = false;
    for byte in raw.bytes() {
        if quoted {
            if escaped { escaped = false; }
            else if byte == b'\\' { escaped = true; }
            else if byte == b'"' { quoted = false; }
            continue;
        }
        match byte {
            b'"' => { tokens += 1; quoted = true; atom = false; }
            b'[' | b'{' => { tokens += 1; depth += 1; atom = false; }
            b']' | b'}' => { depth = depth.saturating_sub(1); atom = false; }
            b',' | b':' | b' ' | b'\n' | b'\r' | b'\t' => { atom = false; }
            _ if !atom => { tokens += 1; atom = true; }
            _ => {}
        }
        if depth > JSON_DEPTH || tokens > JSON_TOKENS { return Err(budget("JSON structure")); }
    }
    Ok(())
}

fn directory_flags() -> OFlags {
    #[cfg(target_os = "linux")]
    let access = OFlags::PATH;
    #[cfg(target_vendor = "apple")]
    let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let access = OFlags::RDONLY;
    access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
}

/// Only the caller-selected home is a trusted alias boundary. Everything
/// beneath it is opened relative to pinned fds and refuses symlinks.
fn parent(root: &Path, path: &Path, create: bool) -> io::Result<(File, std::ffi::OsString)> {
    let relative = path.strip_prefix(root).map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, StorageRefusal("Always-On path escaped its selected storage root.")))?;
    let leaf = relative.file_name().ok_or(io::ErrorKind::InvalidInput)?.to_owned();
    let mut anchor = root;
    loop {
        match std::fs::symlink_metadata(anchor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => { anchor = anchor.parent().ok_or(error)?; }
            Err(error) => return Err(error),
        }
    }
    let canonical = anchor.canonicalize()?;
    let directory = crate::project_files::open_directory_beneath(&canonical, &canonical)?;
    let suffix = root.strip_prefix(anchor).map_err(|_| io::ErrorKind::InvalidInput)?
        .join(relative.parent().unwrap_or(Path::new("")));
    if create && !suffix.as_os_str().is_empty() {
        crate::project_files::create_directories_at(&directory, &suffix, Mode::from_raw_mode(0o700))?;
    }
    let mut parent = directory;
    for part in suffix.components() {
        let Component::Normal(name) = part else { return Err(io::ErrorKind::PermissionDenied.into()); };
        parent = File::from(openat(&parent, name, directory_flags(), Mode::empty())?);
    }
    Ok((parent, leaf))
}

fn regular(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, StorageRefusal("Always-On storage requires a regular file without hard-link aliases.")));
    }
    Ok(file)
}

pub(crate) fn read(root: &Path, path: &Path, cap: usize) -> io::Result<Option<Vec<u8>>> {
    let opened = (|| {
        let (parent, leaf) = parent(root, path, false)?;
        regular(File::from(openat(&parent, &leaf, OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK, Mode::empty())?))
    })();
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > cap as u64 { return Err(budget("file bytes")); }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > cap { return Err(budget("file bytes")); }
    Ok(Some(bytes))
}

pub(crate) fn write(root: &Path, path: &Path, bytes: &[u8], cap: usize) -> io::Result<()> {
    if bytes.len() > cap { return Err(budget("file bytes")); }
    let (parent, leaf) = parent(root, path, true)?;
    match statat(&parent, &leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, StorageRefusal("Always-On storage target must be a regular file without aliases.")));
        }
        Ok(_) | Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = format!(".always-on-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = File::from(openat(&parent, temporary.as_str(), OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o600))?);
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(&parent, temporary.as_str(), &parent, &leaf)?;
        let sync_parent = File::from(openat(&parent, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW, Mode::empty())?);
        sync_parent.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = unlinkat(&parent, temporary.as_str(), AtFlags::empty()); }
    result
}

pub(crate) fn append(root: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > EVENT_BYTES { return Err(budget("event bytes")); }
    // The serialized transaction in run prevents two in-process callers from
    // replacing one another's append. Existing history is kept on quota error.
    let mut content = read(root, path, LOG_BYTES)?.unwrap_or_default();
    if content.len().checked_add(bytes.len()).is_none_or(|size| size > LOG_BYTES) { return Err(budget("event log bytes")); }
    content.extend_from_slice(bytes);
    write(root, path, &content, LOG_BYTES)
}

pub(crate) fn root_for(state_file: &Path) -> PathBuf {
    let mut project = state_file.parent().unwrap_or(Path::new("/"));
    if state_file.file_name().is_some_and(|name| name == "index.json")
        && project.file_name().is_some_and(|name| name == "cycles" || name == "plans") {
        project = project.parent().unwrap_or(project);
    }
    if project.parent().is_some_and(|path| path.file_name().is_some_and(|name| name == "projects"))
        && project.parent().and_then(Path::parent).is_some_and(|path| path.file_name().is_some_and(|name| name == "always-on")) {
        project.parent().and_then(Path::parent).and_then(Path::parent).unwrap().to_owned()
    } else { project.to_owned() }
}

pub(crate) fn check_object(object: &pilotdeck_protocol::JsObject) -> io::Result<()> {
    fn visit(value: &pilotdeck_protocol::JsValue, depth: usize, nodes: &mut usize, units: &mut usize) -> io::Result<()> {
        use pilotdeck_protocol::JsValue;
        *nodes += 1;
        if depth > JSON_DEPTH || *nodes > JSON_TOKENS { return Err(budget("JSON structure")); }
        match value {
            JsValue::String(value) => *units = units.checked_add(value.len_utf16()).ok_or_else(|| budget("JSON string units"))?,
            JsValue::Array(values) => { for value in values { visit(value, depth + 1, nodes, units)?; } }
            JsValue::Object(object) => { for (key, value) in object.entries() {
                *nodes += 1; *units = units.checked_add(key.len_utf16()).ok_or_else(|| budget("JSON string units"))?;
                visit(value, depth + 1, nodes, units)?;
            } }
            _ => {}
        }
        if *units > INDEX_BYTES / 2 { return Err(budget("JSON string units")); }
        Ok(())
    }
    let mut nodes = 1;
    let mut units = 0usize;
    for (key, value) in object.entries() {
        nodes += 1; units = units.checked_add(key.len_utf16()).ok_or_else(|| budget("JSON string units"))?;
        visit(value, 1, &mut nodes, &mut units)?;
    }
    Ok(())
}

pub(crate) fn create_new(root: &Path, path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if bytes.len() > EVENT_BYTES { return Err(budget("lock bytes")); }
    let (parent, leaf) = parent(root, path, true)?;
    let file = match openat(&parent, &leaf, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK, Mode::from_raw_mode(0o600)) {
        Ok(file) => file,
        Err(rustix::io::Errno::EXIST) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut file = regular(File::from(file))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(true)
}

pub(crate) fn remove(root: &Path, path: &Path) -> io::Result<()> {
    let (parent, leaf) = parent(root, path, false)?;
    unlinkat(&parent, &leaf, AtFlags::empty()).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    fn scratch() -> Scratch {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-always-storage-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&root.0).unwrap(); root
    }
    #[test]
    fn bounded_regular_reads_and_atomic_writes_preserve_old_bytes_on_quota() {
        let root = scratch(); let file = root.0.join("nested/state.json");
        assert_eq!(read(&root.0, &file, 17).unwrap(), None);
        assert!(!root.0.join("nested").exists());
        write(&root.0, &file, &[b'a'; 17], 17).unwrap();
        assert_eq!(read(&root.0, &file, 17).unwrap().unwrap(), vec![b'a'; 17]);
        assert!(write(&root.0, &file, &[b'b'; 18], 17).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), vec![b'a'; 17]);
        std::fs::write(&file, [b'c'; 18]).unwrap();
        assert!(read(&root.0, &file, 17).is_err());
        assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn descriptors_reject_fifo_parent_and_leaf_aliases_without_touching_targets() {
        let root = scratch(); let outside = scratch();
        let external = outside.0.join("external.json"); std::fs::write(&external, "OWNED").unwrap();
        std::os::unix::fs::symlink(&external, root.0.join("leaf.json")).unwrap();
        assert!(read(&root.0, &root.0.join("leaf.json"), 32).is_err());
        assert!(write(&root.0, &root.0.join("leaf.json"), b"REPLACE", 32).is_err());
        std::os::unix::fs::symlink(&outside.0, root.0.join("parent")).unwrap();
        assert!(write(&root.0, &root.0.join("parent/new.json"), b"REPLACE", 32).is_err());
        let fifo = root.0.join("fifo.json");
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap().success());
        let begin = std::time::Instant::now();
        assert!(read(&root.0, &fifo, 32).is_err());
        assert!(begin.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(std::fs::read(&external).unwrap(), b"OWNED");
        assert!(!outside.0.join("new.json").exists());
    }
    #[test]
    fn json_preflight_checks_exact_token_depth_and_escaped_text_boundaries() {
        let allowed = format!("[{}]", vec!["0"; JSON_TOKENS - 1].join(","));
        let rejected = format!("[{}]", vec!["0"; JSON_TOKENS].join(","));
        assert!(check_json(&allowed).is_ok()); assert!(check_json(&rejected).is_err());
        assert!(check_json(&format!("{}0{}", "[".repeat(JSON_DEPTH), "]".repeat(JSON_DEPTH))).is_ok());
        assert!(check_json(&format!("{}0{}", "[".repeat(JSON_DEPTH + 1), "]".repeat(JSON_DEPTH + 1))).is_err());
        assert!(check_json(r#"{"text":"[[[\"{{{"}"#).is_ok());
    }
    #[tokio::test]
    async fn cancelling_the_async_caller_retains_the_actual_blocking_job_permit() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_slots = slots.clone();
        let caller = tokio::spawn(async move { run_with(worker_slots, move || {
            let _ = entered_tx.send(());
            release_rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(io::Error::other)?;
            Ok(())
        }).await });
        entered_rx.await.unwrap(); caller.abort(); let _ = caller.await;
        assert_eq!(slots.available_permits(), 0, "accepted IO must retain admission after caller cancellation");
        release_tx.send(()).unwrap();
        let permit = tokio::time::timeout(std::time::Duration::from_secs(5), slots.acquire()).await.unwrap().unwrap();
        drop(permit); assert_eq!(slots.available_permits(), 1);
    }
}

/// Directory-only fd variant for native workspace staging; the synthetic
/// leaf is never created and no caller-supplied file is opened.
pub(crate) fn directory(root: &Path, path: &Path, create: bool) -> io::Result<File> {
    parent(root, &path.join(".always-on-directory-anchor"), create).map(|(directory, _)| directory)
}
