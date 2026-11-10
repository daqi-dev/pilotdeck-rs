//! Invalidation of stale last-turn replacement sidecars, shared by native reopen and portable restore.
//! Caller owns the transcript writer lease and mutation fence. Never infer absence from read failure.
use rustix::fs::{AtFlags, Dir, Mode, OFlags, openat, statat, unlinkat};
use std::{fs::File, io, path::Path};
fn parent(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Transcript has no parent"))?;
    crate::project_files::open_directory_beneath(Path::new("/"), parent)
}
pub(crate) fn valid_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|id| {
        matches!(id.get_version_num(), 1..=5) && matches!(id.get_variant(), uuid::Variant::RFC4122)
    }) && s.len() == 36
}
fn invalidate_at(directory: &File, path: &Path, preserve: bool) -> io::Result<()> {
    // A traversal/search descriptor is not necessarily readable. In particular
    // Darwin O_SEARCH loses DIRECTORY from F_GETFL, so Dir::read_from cannot
    // infer the right access mode. Reopen the pinned object, not its path.
    let readable = File::from(
        openat(
            directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let directory = &readable;

    let stem = path
        .file_stem()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Transcript has no name"))?;
    let prefix = format!(".{}.", stem.to_string_lossy());
    let mut removed = false;
    for entry in Dir::read_from(directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let Some(name) = entry.file_name().to_str().ok() else {
            continue;
        };
        let id = name.strip_prefix(&prefix).and_then(|rest| {
            rest.strip_suffix(".replace.bak")
                .or_else(|| rest.strip_suffix(".replace.json"))
        });
        if id.is_some_and(valid_uuid) {
            removed = true;
            let result = if preserve {
                // Missing is not proof of an intentional delete: these may
                // be the only recovery bytes left after a crash. Remove the
                // transaction from automatic recovery without discarding it.
                let quarantine = format!("{name}.orphaned.{}", uuid::Uuid::new_v4());
                rustix::fs::renameat_with(
                    directory,
                    entry.file_name(),
                    directory,
                    quarantine.as_str(),
                    rustix::fs::RenameFlags::NOREPLACE,
                )
            } else {
                unlinkat(directory, entry.file_name(), AtFlags::empty())
            };
            match result {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    if removed {
        // Persist invalidation before a caller publishes a new same-key
        // transcript, so power loss cannot resurrect a stale rollback.
        directory.sync_all()?;
    }
    Ok(())
}
/// A successful transcript deletion invalidates every replacement transaction for this exact key.
/// Parent traversal rejects symlinks and unlinks stay relative to one pinned directory descriptor.
pub(crate) fn discard(path: &Path) -> io::Result<()> {
    invalidate_at(&parent(path)?, path, false)
}
/// Only ENOENT at the transcript leaf permits quarantine. Existing regular files, directories,
/// and even dangling symlinks are not absence. Parent/stat errors propagate without removing data.
pub(crate) fn cleanup_if_missing(path: &Path) -> io::Result<bool> {
    let directory = parent(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Transcript has no name"))?;
    match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(false),
        Err(rustix::io::Errno::NOENT) => {
            invalidate_at(&directory, path, true)?;
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("pdrs-orphan-sidecars-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
        fn sidecars(&self) -> [std::path::PathBuf; 2] {
            let id = uuid::Uuid::new_v4();
            [
                self.0.join(format!(".session.{id}.replace.bak")),
                self.0.join(format!(".session.{id}.replace.json")),
            ]
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn darwin_search_and_linux_path_handles_are_reopened_before_enumeration() {
        let s = Scratch::new();
        let path = s.0.join("session.jsonl");
        let search = parent(&path).unwrap(); // Darwin O_SEARCH, Linux O_PATH.
        let artifacts = s.sidecars();
        for item in &artifacts {
            std::fs::write(item, b"old backup").unwrap();
        }
        invalidate_at(&search, &path, false)
            .expect("readable directory reopened from pinned search descriptor");
        assert!(artifacts.iter().all(|item| !item.exists()));
        let readable = File::from(
            openat(
                &search,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .unwrap(),
        );
        readable
            .sync_all()
            .expect("same pinned readable directory supports durability sync");
    }

    #[test]
    fn fresh_empty_session_directory_is_not_an_enumeration_error() {
        let s = Scratch::new();
        assert!(cleanup_if_missing(&s.0.join("fresh.jsonl")).unwrap());
        assert!(
            !s.0.join("fresh.jsonl").exists(),
            "cleanup does not create a transcript"
        );
    }

    #[test]
    fn only_missing_leaf_quarantines_exact_uuid_sidecars() {
        let s = Scratch::new();
        let sidecars = s.sidecars();
        for p in &sidecars {
            std::fs::write(p, b"old data").unwrap();
        }
        let unrelated =
            s.0.join(format!(".different.{}.replace.bak", uuid::Uuid::new_v4()));
        let malformed = s.0.join(".session.not-a-uuid.replace.json");
        let lock = s.0.join("session.jsonl.lock");
        for p in [&unrelated, &malformed, &lock] {
            std::fs::write(p, b"keep").unwrap();
        }
        assert!(cleanup_if_missing(&s.0.join("session.jsonl")).unwrap());
        assert!(sidecars.iter().all(|p| !p.exists()));
        for original in &sidecars {
            let prefix = format!(
                "{}.orphaned.",
                original.file_name().unwrap().to_str().unwrap()
            );
            let quarantined: Vec<_> = std::fs::read_dir(&s.0)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| entry.file_name().to_str().unwrap().starts_with(&prefix))
                .collect();
            assert_eq!(quarantined.len(), 1);
            assert_eq!(std::fs::read(quarantined[0].path()).unwrap(), b"old data");
        }
        // A second open cannot reclassify or erase quarantined recovery bytes.
        assert!(cleanup_if_missing(&s.0.join("session.jsonl")).unwrap());
        assert_eq!(std::fs::read_dir(&s.0).unwrap().count(), 5);
        for p in [&unrelated, &malformed, &lock] {
            assert_eq!(std::fs::read(p).unwrap(), b"keep");
        }
    }
    #[test]
    fn existing_file_directory_and_dangling_link_preserve_all_sidecars() {
        use std::os::unix::fs::symlink;
        for mode in 0..3 {
            let s = Scratch::new();
            let path = s.0.join("session.jsonl");
            let artifacts = s.sidecars();
            for p in &artifacts {
                std::fs::write(p, b"precious backup").unwrap();
            }
            match mode {
                0 => std::fs::write(&path, b"current").unwrap(),
                1 => std::fs::create_dir(&path).unwrap(),
                _ => symlink(s.0.join("missing-target"), &path).unwrap(),
            }
            assert!(!cleanup_if_missing(&path).unwrap());
            for p in artifacts {
                assert_eq!(std::fs::read(p).unwrap(), b"precious backup");
            }
        }
    }
    #[test]
    fn unsafe_or_invalid_parent_is_not_absence() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new();
        let artifacts = s.sidecars();
        for p in &artifacts {
            std::fs::write(p, b"keep").unwrap();
        }
        let link = s.0.join("linked");
        symlink(&s.0, &link).unwrap();
        assert!(cleanup_if_missing(&link.join("session.jsonl")).is_err());
        let file = s.0.join("not-a-directory");
        std::fs::write(&file, b"keep").unwrap();
        assert!(cleanup_if_missing(&file.join("session.jsonl")).is_err());
        for p in artifacts {
            assert_eq!(std::fs::read(p).unwrap(), b"keep");
        }
    }
}
