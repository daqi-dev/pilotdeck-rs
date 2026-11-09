//! One configuration publication. This protects the old file from a failed
//! write/sync/rename; it does not serialize unrelated legacy configuration writers.
use rustix::fs::{AtFlags, Gid, Mode, OFlags, RenameFlags, Uid};
use std::{
    ffi::OsString,
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Refusal and OS failures retain their provenance so each caller can keep
/// its existing error wording without duplicating filesystem operations.
#[derive(Debug)]
pub enum Error {
    Refused(&'static str),
    Io {
        error: std::io::Error,
        operation: String,
        path: PathBuf,
    },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(message) => write!(f, "Cannot atomically save file: {message}."),
            Self::Io {
                error,
                operation,
                path,
            } => write!(f, "{operation} '{}': {error}", path.display()),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}
type Result<T> = std::result::Result<T, Error>;
fn fail(message: &'static str) -> Error {
    Error::Refused(message)
}
fn io(error: std::io::Error, operation: &str, path: &Path) -> Error {
    Error::Io {
        error,
        operation: operation.to_owned(),
        path: path.to_path_buf(),
    }
}
fn errno(error: rustix::io::Errno, operation: &str, path: &Path) -> Error {
    io(
        std::io::Error::from_raw_os_error(error.raw_os_error()),
        operation,
        path,
    )
}

pub struct Target {
    configured: PathBuf,
    target: PathBuf,
    parent: PathBuf,
    directory: File,
    existing: Option<File>,
    metadata: Option<Metadata>,
    configured_identity: Option<(u64, u64, bool)>,
    new_only: bool,
}
impl Target {
    pub fn prepare(configured: &Path) -> Result<Self> {
        let initial = match fs::symlink_metadata(configured) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io(error, "lstat", configured)),
        };
        let target = if let Some(metadata) = &initial {
            if !metadata.is_file() && !metadata.file_type().is_symlink() {
                return Err(fail("target must be a regular file"));
            }
            match fs::canonicalize(configured) {
                Ok(target) => target,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && metadata.file_type().is_symlink() =>
                {
                    return Err(fail("dangling symbolic links are unsupported"));
                }
                Err(error) => return Err(io(error, "realpath", configured)),
            }
        } else {
            let parent = configured
                .parent()
                .ok_or_else(|| fail("target has no parent directory"))?;
            fs::create_dir_all(parent).map_err(|error| io(error, "mkdir", parent))?;
            fs::canonicalize(parent)
                .map_err(|error| io(error, "realpath", parent))?
                .join(
                    configured
                        .file_name()
                        .ok_or_else(|| fail("target has no file name"))?,
                )
        };
        let parent = target
            .parent()
            .ok_or_else(|| fail("target has no parent directory"))?
            .to_path_buf();
        let directory = File::open(&parent).map_err(|error| io(error, "open", &parent))?;
        let (existing, metadata) = if initial.is_some() {
            // No truncation: both permissions and topology must be acceptable
            // before any replacement is prepared. NONBLOCK prevents special
            // file substitution from parking this open indefinitely.
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&target)
                .map_err(|error| io(error, "open", configured))?;
            let metadata = file
                .metadata()
                .map_err(|error| io(error, "fstat", configured))?;
            supported(&file, &target, &metadata)?;
            (Some(file), Some(metadata))
        } else {
            (None, None)
        };
        let configured_identity = initial.map(|m| (m.dev(), m.ino(), m.file_type().is_symlink()));
        let result = Self {
            configured: configured.to_path_buf(),
            target,
            parent,
            directory,
            existing,
            metadata,
            configured_identity,
            new_only: false,
        };
        result.unchanged()?;
        Ok(result)
    }
    /// Pin an absent target. The final rename still uses NOREPLACE, so an
    /// intervening creator can never be overwritten.
    pub fn prepare_new(configured: &Path) -> Result<Self> {
        let occupied = || {
            io(
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                "lstat",
                configured,
            )
        };
        match fs::symlink_metadata(configured) {
            Ok(_) => return Err(occupied()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error, "lstat", configured)),
        }
        let mut target = match Self::prepare(configured) {
            Ok(target) => target,
            Err(error) => {
                if fs::symlink_metadata(configured).is_ok() {
                    return Err(occupied());
                }
                return Err(error);
            }
        };
        if target.configured_identity.is_some() {
            return Err(occupied());
        }
        target.new_only = true;
        Ok(target)
    }
    fn unchanged(&self) -> Result<()> {
        let directory = self
            .directory
            .metadata()
            .map_err(|error| io(error, "fstat", &self.parent))?;
        let current =
            fs::metadata(&self.parent).map_err(|error| io(error, "stat", &self.parent))?;
        if (directory.dev(), directory.ino()) != (current.dev(), current.ino()) {
            return Err(fail("target changed before publication"));
        }
        if let Some((dev, ino, link)) = self.configured_identity {
            let source = fs::symlink_metadata(&self.configured)
                .map_err(|error| io(error, "lstat", &self.configured))?;
            if (source.dev(), source.ino(), source.file_type().is_symlink()) != (dev, ino, link)
                || fs::canonicalize(&self.configured)
                    .map_err(|error| io(error, "realpath", &self.configured))?
                    != self.target
            {
                return Err(fail("target changed before publication"));
            }
            let current =
                fs::metadata(&self.target).map_err(|error| io(error, "stat", &self.target))?;
            let held = self
                .existing
                .as_ref()
                .expect("existing identity has descriptor")
                .metadata()
                .map_err(|error| io(error, "fstat", &self.target))?;
            let before = self
                .metadata
                .as_ref()
                .expect("existing identity has metadata");
            if fingerprint(&current) != fingerprint(before)
                || fingerprint(&held) != fingerprint(before)
            {
                return Err(fail("target changed before publication"));
            }
            supported(self.existing.as_ref().unwrap(), &self.target, &current)?;
        } else {
            if fs::canonicalize(
                self.configured
                    .parent()
                    .ok_or_else(|| fail("target has no parent directory"))?,
            )
            .map_err(|error| io(error, "realpath", &self.configured))?
                != self.parent
            {
                return Err(fail("target changed before publication"));
            }
            match fs::symlink_metadata(&self.target) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) if self.new_only => {
                    return Err(io(
                        std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                        "lstat",
                        &self.target,
                    ));
                }
                Ok(_) => return Err(fail("target changed before publication")),
                Err(error) => return Err(io(error, "lstat", &self.target)),
            }
        }
        Ok(())
    }
    /// Read the already-pinned original without following a replaced pathname.
    pub fn read_existing_bounded(&self, limit: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let existing = self
            .existing
            .as_ref()
            .ok_or_else(|| fail("target does not exist"))?;
        let mut file = existing
            .try_clone()
            .map_err(|error| io(error, "dup", &self.target))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io(error, "seek", &self.target))?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| io(error, "read", &self.target))?;
        if bytes.len() > limit {
            return Err(fail("target exceeds the read limit"));
        }
        Ok(bytes)
    }

    pub fn publish(self, bytes: &[u8]) -> Result<()> {
        let name = OsString::from(format!(".pilotdeck-setup-{}.tmp", uuid::Uuid::new_v4()));
        let temporary_directory = self
            .directory
            .try_clone()
            .map_err(|error| io(error, "dup", &self.parent))?;
        let fd = rustix::fs::openat(
            &self.directory,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| errno(error, "open", &self.parent.join(&name)))?;
        let mut temporary = Temporary {
            file: File::from(fd),
            directory: temporary_directory,
            name,
            published: false,
        };
        temporary
            .file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| io(error, "fchmod", &self.target))?;
        let temporary_path = self.parent.join(&temporary.name);
        let initial_meta = temporary
            .file
            .metadata()
            .map_err(|error| io(error, "fstat", &temporary_path))?;
        supported(&temporary.file, &temporary_path, &initial_meta)?;
        temporary
            .file
            .write_all(bytes)
            .map_err(|error| io(error, "write", &self.target))?;
        if let Some(metadata) = &self.metadata {
            let temp_meta = temporary
                .file
                .metadata()
                .map_err(|error| io(error, "fstat", &self.target))?;
            if (temp_meta.uid(), temp_meta.gid()) != (metadata.uid(), metadata.gid()) {
                rustix::fs::fchown(
                    &temporary.file,
                    Some(Uid::from_raw(metadata.uid())),
                    Some(Gid::from_raw(metadata.gid())),
                )
                .map_err(|error| errno(error, "fchown", &self.target))?;
            }
            temporary
                .file
                .set_permissions(metadata.permissions())
                .map_err(|error| io(error, "fchmod", &self.target))?;
        }
        let temp_meta = temporary
            .file
            .metadata()
            .map_err(|error| io(error, "fstat", &temporary_path))?;
        supported(&temporary.file, &temporary_path, &temp_meta)?;
        temporary
            .file
            .sync_all()
            .map_err(|error| io(error, "fsync", &self.target))?;
        self.unchanged()?;
        let target_name = self
            .target
            .file_name()
            .ok_or_else(|| fail("target has no file name"))?;
        if self.existing.is_some() {
            rustix::fs::renameat(
                &self.directory,
                &temporary.name,
                &self.directory,
                target_name,
            )
        } else {
            rustix::fs::renameat_with(
                &self.directory,
                &temporary.name,
                &self.directory,
                target_name,
                RenameFlags::NOREPLACE,
            )
        }
        .map_err(|error| errno(error, "rename", &self.target))?;
        temporary.published = true;
        // Rename is the final fallible operation. No post-publication failure
        // is reported as though the old bytes were still installed.
        Ok(())
    }
}
#[derive(PartialEq, Eq)]
struct Fingerprint {
    identity: (u64, u64),
    access: (u32, u32, u32, u64),
    content: (u64, i64, i64, i64, i64),
    bsd_flags: u32,
}
fn fingerprint(metadata: &Metadata) -> Fingerprint {
    Fingerprint {
        bsd_flags: bsd_flags(metadata),
        identity: (metadata.dev(), metadata.ino()),
        access: (
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            metadata.nlink(),
        ),
        content: (
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
    }
}
fn bsd_flags(metadata: &Metadata) -> u32 {
    #[cfg(target_os = "macos")]
    {
        std::os::macos::fs::MetadataExt::st_flags(metadata)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = metadata;
        0
    }
}
fn supported(file: &File, path: &Path, metadata: &Metadata) -> Result<()> {
    if bsd_flags(metadata) != 0 {
        return Err(fail("BSD file flags are unsupported"));
    }
    if !metadata.is_file() {
        return Err(fail("target must be a regular file"));
    }
    if metadata.nlink() != 1 {
        return Err(fail("hard-linked configurations are unsupported"));
    }
    if metadata.mode() & 0o7000 != 0 {
        return Err(fail("special permission bits are unsupported"));
    }
    let mut names = [0u8; 1];
    match rustix::fs::flistxattr(file, &mut names[..]) {
        Ok(0) => {}
        Ok(_) | Err(rustix::io::Errno::RANGE) => {
            return Err(fail("extended attributes are unsupported"));
        }
        Err(_) => return Err(fail("extended attributes could not be inspected")),
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/ls")
            .args(["-ledn", "--"])
            .arg(path)
            .env("LC_ALL", "C")
            .output()
            .map_err(|_| fail("ACL metadata could not be inspected"))?;
        let permissions = String::from_utf8_lossy(&output.stdout);
        let mode = permissions
            .split_whitespace()
            .next()
            .ok_or_else(|| fail("ACL metadata could not be inspected"))?;
        if !output.status.success() || mode.len() < 10 {
            return Err(fail("ACL metadata could not be inspected"));
        }
        if mode.contains('+') {
            return Err(fail("ACL metadata is unsupported"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
    Ok(())
}
struct Temporary {
    file: File,
    directory: File,
    name: OsString,
    published: bool,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = rustix::fs::unlinkat(&self.directory, &self.name, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod new_only_tests {
    use super::*;
    fn already_exists(error: Error) -> bool {
        matches!(error, Error::Io { error, .. } if error.kind() == std::io::ErrorKind::AlreadyExists)
    }
    #[test]
    fn new_target_never_overwrites_preexisting_or_racing_file() {
        let root = std::env::temp_dir().join(format!("pdrs-atomic-new-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("archive");
        let prepared = Target::prepare_new(&path).unwrap();
        fs::write(&path, b"other").unwrap();
        assert!(already_exists(prepared.publish(b"ours").unwrap_err()));
        assert_eq!(fs::read(&path).unwrap(), b"other");
        assert!(already_exists(match Target::prepare_new(&path) {
            Ok(_) => panic!("must refuse"),
            Err(e) => e,
        }));
        fs::remove_file(&path).unwrap();
        Target::prepare_new(&path)
            .unwrap()
            .publish(b"ours")
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ours");
        fs::remove_dir_all(root).unwrap();
    }
}
