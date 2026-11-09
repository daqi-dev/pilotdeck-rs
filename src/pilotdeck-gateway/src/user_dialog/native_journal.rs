//! Descriptor-relative journal IO for the native CLI. The caller-selected
//! home may be an alias; entries below that root may not be symlinks.
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, renameat, statat, unlinkat};
use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) struct NativeJournalFiles {
    root: PathBuf,
    relative: PathBuf,
}
impl NativeJournalFiles {
    pub(crate) fn new(home: &Path, path: &Path) -> io::Result<Self> {
        let root = home.canonicalize()?;
        let relative = path
            .strip_prefix(home)
            .or_else(|_| path.strip_prefix(&root))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Dialog journal is outside the selected home.",
                )
            })?
            .to_owned();
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(Self { root, relative })
    }
    fn parent(&self, create: bool) -> io::Result<(File, &std::ffi::OsStr)> {
        let root = crate::project_files::open_directory_beneath(&self.root, &self.root)?;
        let relative = self.relative.parent().ok_or(io::ErrorKind::InvalidInput)?;
        if create && !relative.as_os_str().is_empty() {
            crate::project_files::create_directories_at(
                &root,
                relative,
                Mode::from_raw_mode(0o700),
            )?;
        }
        let parent =
            crate::project_files::open_directory_beneath(&self.root, &self.root.join(relative))?;
        Ok((
            parent,
            self.relative
                .file_name()
                .ok_or(io::ErrorKind::InvalidInput)?,
        ))
    }
    pub(crate) fn read_raw(&self) -> io::Result<Vec<u8>> {
        let (parent, leaf) = self.parent(false)?;
        let file = File::from(openat(
            &parent,
            leaf,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if metadata.len() > MAX_BYTES as u64 {
            return Err(limit());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(limit());
        }
        Ok(bytes)
    }
    pub(crate) fn read(&self) -> io::Result<Vec<u8>> {
        let bytes = self.read_raw()?;
        check_json_bounds(&bytes)?;
        Ok(bytes)
    }
    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_BYTES {
            return Err(limit());
        }
        let (parent, leaf) = self.parent(true)?;
        // Traversal descriptors are O_PATH on Linux (O_SEARCH on macOS).
        // They can anchor renameat but cannot be fsynced. Reopen the pinned
        // directory itself, never its path, before publishing any bytes.
        let sync_parent = File::from(openat(
            &parent,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        match statat(&parent, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat)
                if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                    || stat.st_nlink != 1 =>
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(_) | Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
        }
        let temporary = format!(".dialog-{}.tmp", uuid::Uuid::new_v4());
        let result = (|| {
            let mut file = File::from(openat(
                &parent,
                temporary.as_str(),
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::CLOEXEC
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK,
                Mode::from_raw_mode(0o600),
            )?);
            file.write_all(bytes)?;
            file.sync_all()?;
            renameat(&parent, temporary.as_str(), &parent, leaf)?;
            sync_parent.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(&parent, temporary.as_str(), AtFlags::empty());
        }
        result
    }
    pub(crate) fn remove(&self) -> io::Result<()> {
        let operation = (|| {
            let (parent, leaf) = self.parent(false)?;
            unlinkat(&parent, leaf, AtFlags::empty()).map_err(io::Error::from)
        })();
        match operation {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}
pub(crate) fn check_json_bounds(bytes: &[u8]) -> io::Result<()> {
    // Bound parser depth and work before the boundary parser recurses.
    let (mut quoted, mut escaped, mut depth, mut tokens) = (false, false, 0usize, 0usize);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => {
                quoted = true;
                tokens += 1;
            }
            b'[' | b'{' => {
                depth += 1;
                tokens += 1;
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            b',' | b':' => tokens += 1,
            _ => {}
        }
        if depth > 128 || tokens > 100_000 {
            return Err(limit());
        }
    }
    Ok(())
}
fn limit() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "User dialog journal exceeds native IO or JSON limits.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_dialog::{GatewayUserDialogJournal, JournalOptions};
    use std::os::unix::fs::symlink;
    fn root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("pdrs-native-dialog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn native_journal_fsyncs_a_readable_pinned_parent() {
        let root = root();
        let path = root.join("gateway/sdk-task-budget-ledger.jsonl");
        let native = NativeJournalFiles::new(&root, &path).unwrap();
        native.write(b"{\"kind\":\"cleared\"}\n").unwrap();
        assert_eq!(native.read().unwrap(), b"{\"kind\":\"cleared\"}\n");
        native.write(b"{\"kind\":\"updated\"}\n").unwrap();
        assert_eq!(native.read().unwrap(), b"{\"kind\":\"updated\"}\n");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_journal_preserves_utf16_and_recovers_then_removes() {
        let root = root();
        let path = root.join("chat/session.jsonl.dialogs.json");
        let journal = GatewayUserDialogJournal::new(path.clone(), JournalOptions::default())
            .with_native_home(&root)
            .unwrap();
        let request = pilotdeck_protocol::parse_json(r#"{"type":"user_dialog_request","requestId":"id","dialogKind":"input","prompt":"\ud800"}"#).unwrap();
        journal.record(&request).unwrap();
        let recovered = journal.recover().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].get("request"), Some(&request));
        assert_eq!(
            recovered[0]
                .get("reason")
                .and_then(pilotdeck_protocol::JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap(),
            "gateway_restarted"
        );
        journal.remove(&"id".into()).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_journal_rejects_fifo_links_and_limits_without_changing_original() {
        let root = root();
        let path = root.join("journal");
        let native = NativeJournalFiles::new(&root, &path).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(native.read().is_err());
        assert!(native.write(b"valid").is_err());
        std::fs::remove_file(&path).unwrap();
        let outside = root.join("outside");
        std::fs::write(&outside, b"original").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(native.read().is_err());
        assert!(native.write(b"changed").is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&outside, &path).unwrap();
        assert!(native.read().is_err());
        assert!(native.write(b"changed").is_err());
        std::fs::remove_file(&path).unwrap();
        native.write(b"original").unwrap();
        assert!(native.write(&vec![b'x'; MAX_BYTES + 1]).is_err());
        assert_eq!(native.read().unwrap(), b"original");
        std::fs::write(&path, format!("{}0{}", "[".repeat(129), "]".repeat(129))).unwrap();
        assert!(native.read().is_err());
        let file = File::create(&path).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();
        assert!(native.read().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_journal_accepts_root_alias_but_rejects_child_alias() {
        let root = root();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        let alias = root.join("alias");
        symlink(&home, &alias).unwrap();
        let native = NativeJournalFiles::new(&alias, &alias.join("a/journal")).unwrap();
        native.write(b"test").unwrap();
        assert_eq!(native.read().unwrap(), b"test");
        symlink(home.join("a"), home.join("linked")).unwrap();
        let linked = NativeJournalFiles::new(&home, &home.join("linked/journal")).unwrap();
        assert!(linked.read().is_err());
        assert!(linked.write(b"changed").is_err());
        assert!(NativeJournalFiles::new(&home, &home.join("../escape")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
