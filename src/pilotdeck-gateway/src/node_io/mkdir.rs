//! Node 22 recursive mkdir, shared by skills, uploads and the run registry.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct MkdirError {
    pub source: io::Error,
    pub path: PathBuf,
}
impl std::fmt::Display for MkdirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match super::diagnostic(&self.source, "mkdir", Some(&self.path.to_string_lossy())) {
            Some((_, message)) => f.write_str(&message),
            None => self.source.fmt(f),
        }
    }
}
impl std::error::Error for MkdirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
impl MkdirError {
    pub fn into_io(self) -> io::Error {
        io::Error::new(self.source.kind(), self)
    }
}

/// fs.promises.mkdir(path, {recursive:true}), node_file.cc MKDirpAsync.
/// Fatal/ancestor errors name the request; a stat failure names its component.
/// Extracted from skills::mkdir_p: nonfatal errors use stat fallback. The prior
/// real Node read-only-image probe covered EROFS; EDQUOT remains unmeasured.
/// Keep lexical path spelling (including repeated / and trailing /), like Node.
pub(crate) fn mkdir_recursive(path: &Path) -> Result<(), MkdirError> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let fail = |source| MkdirError {
        source,
        path: path.to_owned(),
    };
    let errno = |code| io::Error::from_raw_os_error(code);
    let mut pending = vec![path.to_owned()];
    while let Some(next) = pending.pop() {
        let Err(error) = fs::create_dir(&next) else {
            continue;
        };
        match error.raw_os_error() {
            Some(libc::EACCES | libc::ENOSPC | libc::ENOTDIR | libc::EPERM) => {
                return Err(fail(error));
            }
            Some(libc::ENOENT) => {
                let bytes = next.as_os_str().as_bytes();
                let parent = bytes
                    .iter()
                    .rposition(|b| *b == b'/')
                    .map_or(bytes, |i| &bytes[..i]);
                if parent == bytes {
                    return Err(fail(error));
                }
                let parent = PathBuf::from(std::ffi::OsString::from_vec(parent.to_vec()));
                pending.push(next);
                pending.push(parent);
            }
            code => {
                let ancestor = code == Some(libc::EEXIST) && !pending.is_empty();
                match fs::metadata(&next) {
                    Ok(meta) if meta.is_dir() && ancestor => {}
                    Ok(meta) if meta.is_dir() => return Ok(()),
                    _ if ancestor => return Err(fail(errno(libc::ENOTDIR))),
                    Ok(_) => return Err(fail(errno(libc::EEXIST))),
                    Err(source) => return Err(MkdirError { source, path: next }),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_mkdir_matches_node_failures_and_success() {
        let raw = include_str!("../../tests/fixtures/mkdir-boundary/oracle.json");
        crate::test_baseline::oracle(raw);
        let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
        let root = std::env::temp_dir().join(format!("pdrs-mkdir-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        for row in fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "mkdir")
        {
            let id = row["id"].as_str().unwrap();
            let dir = root.join(id);
            fs::create_dir(&dir).unwrap();
            if id == "ancestor" || id == "leaf" {
                fs::write(dir.join("block"), b"block").unwrap();
            }
            let path = dir.join(row["suffix"].as_str().unwrap());
            if id == "existing" {
                fs::create_dir(&path).unwrap();
            }
            match mkdir_recursive(&path) {
                Ok(()) => {
                    assert_eq!(row["observed"]["ok"], true, "{id}");
                    assert!(path.is_dir());
                }
                Err(error) => {
                    let (code, message) = crate::node_io::diagnostic(
                        &error.source,
                        "mkdir",
                        Some(&error.path.to_string_lossy()),
                    )
                    .unwrap();
                    let scrub =
                        |text: String| text.replace(&root.to_string_lossy().into_owned(), "<dir>");
                    assert_eq!(
                        serde_json::json!({"code":code,"syscall":"mkdir","path":scrub(error.path.to_string_lossy().into_owned()),"message":scrub(message)}),
                        row["observed"],
                        "{id}"
                    );
                }
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
