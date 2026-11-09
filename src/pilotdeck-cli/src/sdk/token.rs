//! File-source policy only: explicit argv/env values never enter this reader.
//! Ordinary IO failures retain SDK readFile(...).catch(() => "") behavior.
use crate::{Failure, paths};
use std::path::Path;

#[cfg(unix)]
pub(super) const LIMIT: usize = 64 * 1024;
#[cfg(unix)]
pub(super) const NOT_REGULAR: &str = "Gateway auth token file must be a regular file.";
#[cfg(unix)]
pub(super) const TOO_LARGE: &str = "Gateway auth token file exceeds the 65536-byte limit.";

fn decode(bytes: &[u8]) -> String {
    paths::js_trim(&String::from_utf8_lossy(bytes)).to_owned()
}
#[cfg(not(unix))]
pub(crate) fn read(path: &Path) -> Result<String, Failure> {
    // Keep the existing platform behavior; this patch does not invent an
    // unverified Windows special-file reader or claim the Unix guarantee.
    Ok(std::fs::read(path)
        .map(|bytes| decode(&bytes))
        .unwrap_or_default())
}
#[cfg(unix)]
pub(crate) fn read(path: &Path) -> Result<String, Failure> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return Ok(String::new()),
    };
    opened(file, || {})
}
#[cfg(unix)]
fn opened(file: std::fs::File, after_metadata: impl FnOnce()) -> Result<String, Failure> {
    use std::io::Read;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return Ok(String::new()),
    };
    // EISDIR is an ordinary read failure in the original SDK, so it must keep
    // producing the existing missing-token error rather than a new message.
    if metadata.is_dir() {
        return Ok(String::new());
    }
    if !metadata.is_file() {
        return Err(Failure::Error(NOT_REGULAR.into()));
    }
    if metadata.len() > LIMIT as u64 {
        return Err(Failure::Error(TOO_LARGE.into()));
    }
    // Private seam for deterministic real-fd growth/replacement regressions.
    // Production passes an empty closure; there is no env/CLI test switch.
    after_metadata();
    let mut bytes = Vec::new();
    let result = file.take((LIMIT + 1) as u64).read_to_end(&mut bytes);
    if bytes.len() > LIMIT {
        return Err(Failure::Error(TOO_LARGE.into()));
    }
    if result.is_err() {
        return Ok(String::new());
    }
    Ok(decode(&bytes))
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::{fs, io::Write, os::unix::fs::FileTypeExt, path::PathBuf};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("pdrs-sdk-token-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn error(result: Result<String, Failure>) -> String {
        match result {
            Err(Failure::Error(message)) => message,
            Err(Failure::Todo(_)) => panic!("unexpected unsupported file branch"),
            Ok(_) => panic!("unsafe token input was accepted"),
        }
    }
    #[test]
    fn empty_missing_directory_and_utf8_lossy_trim_keep_the_original_contract() {
        let root = Scratch::new();
        let path = root.0.join("token");
        assert_eq!(read(&path).unwrap(), "");
        fs::create_dir(&path).unwrap();
        assert_eq!(read(&path).unwrap(), "");
        fs::remove_dir(&path).unwrap();
        for (bytes, expected) in [
            (b"".as_slice(), ""),
            (b" \t\r\n".as_slice(), ""),
            (b"\xffa\xe2\x82".as_slice(), "�a�"),
            ("\u{feff}\u{3000} token \r\n".as_bytes(), "token"),
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(read(&path).unwrap(), expected);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn exact_byte_limit_is_accepted_and_excess_keeps_the_input_unchanged() {
        let root = Scratch::new();
        let path = root.0.join("token");
        let bytes = vec![b'x'; LIMIT];
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read(&path).unwrap().as_bytes(), bytes);
        let bytes = vec![b'x'; LIMIT + 1];
        fs::write(&path, &bytes).unwrap();
        assert_eq!(error(read(&path)), TOO_LARGE);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn replacing_the_path_with_a_fifo_after_fstat_cannot_redirect_the_open_reader() {
        let root = Scratch::new();
        let path = root.0.join("token");
        fs::write(&path, b"original token\n").unwrap();
        let file = fs::File::open(&path).unwrap();
        let value = opened(file, || {
            fs::remove_file(&path).unwrap();
            assert!(
                std::process::Command::new("/usr/bin/mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        })
        .unwrap();
        assert_eq!(value, "original token");
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_fifo());
    }
    #[test]
    fn growth_after_fstat_is_rejected_by_the_actual_capped_read() {
        let root = Scratch::new();
        let path = root.0.join("token");
        fs::write(&path, b"short").unwrap();
        let file = fs::File::open(&path).unwrap();
        let value = opened(file, || {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&vec![b'x'; LIMIT])
                .unwrap();
        });
        assert_eq!(error(value), TOO_LARGE);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), LIMIT + 5);
        assert!(bytes.starts_with(b"short"));
        assert!(bytes[5..].iter().all(|byte| *byte == b'x'));
    }
}
