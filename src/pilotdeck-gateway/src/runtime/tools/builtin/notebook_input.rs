//! Shared input bound for notebook readers. The limit applies before JSON decoding.
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

pub(crate) const MAX_NOTEBOOK_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum NotebookReadError {
    Io(io::Error),
    Read(io::Error),
    NotRegular,
    TooLarge,
}

impl From<io::Error> for NotebookReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn read_notebook_input(path: &Path) -> Result<Vec<u8>, NotebookReadError> {
    // Resolve ordinary aliases before inspecting/opening their target.
    let real = fs::canonicalize(path)?;
    let before = fs::symlink_metadata(&real)?;
    if !before.is_file() && !before.is_dir() {
        return Err(NotebookReadError::NotRegular);
    }
    if before.is_file() && before.len() > MAX_NOTEBOOK_SOURCE_BYTES {
        return Err(NotebookReadError::TooLarge);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let mut file = options.open(&real)?;
    let opened = file.metadata()?;
    if opened.is_dir() {
        // Preserve the platform's existing directory-read IO error.
        let mut byte = [0u8; 1];
        file.read(&mut byte).map_err(NotebookReadError::Read)?;
        return Err(NotebookReadError::NotRegular);
    }
    if !opened.is_file() {
        return Err(NotebookReadError::NotRegular);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() {
            return Err(NotebookReadError::Io(io::Error::other(
                "Notebook source changed while opening.",
            )));
        }
    }
    if opened.len() > MAX_NOTEBOOK_SOURCE_BYTES {
        return Err(NotebookReadError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_NOTEBOOK_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(NotebookReadError::Read)?;
    if bytes.len() as u64 > MAX_NOTEBOOK_SOURCE_BYTES {
        return Err(NotebookReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "pilotdeck-notebook-input-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn ordinary_bytes_and_missing_file_keep_their_io_result() {
        let scratch = Scratch::new();
        let path = scratch.0.join("ordinary.ipynb");
        let bytes = b"{\"cells\":[]}\n";
        fs::write(&path, bytes).unwrap();
        assert_eq!(read_notebook_input(&path).unwrap(), bytes);
        let missing = scratch.0.join("missing.ipynb");
        match read_notebook_input(&missing) {
            Err(NotebookReadError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
            other => panic!("unexpected missing-file result: {other:?}"),
        }
    }

    #[test]
    fn directory_and_oversized_regular_file_are_rejected() {
        let scratch = Scratch::new();
        let original = fs::read(&scratch.0).unwrap_err();
        let observed = match read_notebook_input(&scratch.0) {
            Err(NotebookReadError::Io(error) | NotebookReadError::Read(error)) => error,
            other => panic!("directory error changed: {other:?}"),
        };
        assert_eq!(observed.raw_os_error(), original.raw_os_error());
        assert_eq!(observed.kind(), original.kind());
        let oversized = scratch.0.join("oversized.ipynb");
        fs::File::create(&oversized)
            .unwrap()
            .set_len(MAX_NOTEBOOK_SOURCE_BYTES + 1)
            .unwrap();
        assert!(matches!(
            read_notebook_input(&oversized),
            Err(NotebookReadError::TooLarge)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn regular_alias_is_read_and_owned_fifo_is_rejected_before_open() {
        use std::os::unix::fs::symlink;
        let scratch = Scratch::new();
        let target = scratch.0.join("regular.ipynb");
        fs::write(&target, b"{}").unwrap();
        let alias = scratch.0.join("alias.ipynb");
        symlink(&target, &alias).unwrap();
        assert_eq!(read_notebook_input(&alias).unwrap(), b"{}");
        let fifo = scratch.0.join("owned.ipynb");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(matches!(
            read_notebook_input(&fifo),
            Err(NotebookReadError::NotRegular)
        ));
        let fifo_alias = scratch.0.join("owned-alias.ipynb");
        symlink(&fifo, &fifo_alias).unwrap();
        assert!(matches!(
            read_notebook_input(&fifo_alias),
            Err(NotebookReadError::NotRegular)
        ));
    }
}
