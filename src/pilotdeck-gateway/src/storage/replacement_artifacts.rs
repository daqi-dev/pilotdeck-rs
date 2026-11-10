//! Invalidation of stale last-turn replacement sidecars, shared by native reopen and portable restore.
//! Caller owns the transcript writer lease and mutation fence. Never infer absence from read failure.
use rustix::fs::{AtFlags, Dir, Mode, OFlags, openat, statat, unlinkat};
use sha2::{Digest, Sha256};
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
fn quarantine_prefix(path: &Path) -> io::Result<String> {
    use std::os::unix::ffi::OsStrExt;
    let stem = path
        .file_stem()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Transcript has no name"))?;
    Ok(format!(
        ".pdrs-orphan.{:x}.",
        Sha256::digest(stem.as_bytes())
    ))
}
fn active_transaction(name: &str, prefix: &str) -> Option<(String, &'static str)> {
    let rest = name.strip_prefix(prefix)?;
    for kind in ["bak", "json"] {
        let suffix = format!(".replace.{kind}");
        if let Some(id) = rest.strip_suffix(&suffix).filter(|id| valid_uuid(id)) {
            return Some((id.to_owned(), kind));
        }
    }
    None
}
fn is_quarantined(name: &str, active_prefix: &str, quarantine_prefix: &str) -> bool {
    if let Some(rest) = name.strip_prefix(quarantine_prefix) {
        let fields: Vec<_> = rest.split('.').collect();
        return fields.len() == 3
            && valid_uuid(fields[0])
            && matches!(fields[1], "bak" | "json")
            && valid_uuid(fields[2]);
    }
    // Also clean artifacts quarantined by the earlier long-name format.
    name.rsplit_once(".orphaned.")
        .is_some_and(|(active, nonce)| {
            valid_uuid(nonce) && active_transaction(active, active_prefix).is_some()
        })
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
    let quarantine_prefix = quarantine_prefix(path)?;
    // Collect before mutating: rename may change directory iterator offsets.
    // The caller's writer lease/fence excludes cooperating sidecar producers.
    let mut selected = Vec::new();
    for entry in Dir::read_from(directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        let active = active_transaction(name, &prefix);
        if active.is_some() || (!preserve && is_quarantined(name, &prefix, &quarantine_prefix)) {
            selected.push((entry.file_name().to_owned(), active));
        }
    }
    let mut removed = false;
    for (name, transaction) in selected {
        let result = if preserve {
            let (id, kind) = transaction.expect("only active transactions are quarantined");
            // Fixed length even when the session stem is near NAME_MAX.
            // Retain the original transaction ID and kind for forensic recovery.
            let quarantine = format!("{quarantine_prefix}{id}.{kind}.{}", uuid::Uuid::new_v4());
            rustix::fs::renameat_with(
                directory,
                name.as_c_str(),
                directory,
                quarantine.as_str(),
                rustix::fs::RenameFlags::NOREPLACE,
            )
        } else {
            unlinkat(directory, name.as_c_str(), AtFlags::empty())
        };
        match result {
            Ok(()) => removed = true,
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(error.into()),
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
            let prefix = quarantine_prefix(&s.0.join("session.jsonl")).unwrap();
            let (_, kind) =
                active_transaction(original.file_name().unwrap().to_str().unwrap(), ".session.")
                    .unwrap();
            let quarantined: Vec<_> = std::fs::read_dir(&s.0)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_str().unwrap();
                    name.starts_with(&prefix) && name.contains(&format!(".{kind}."))
                })
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
    fn hundreds_of_long_names_quarantine_once_then_delete_only_exact_session() {
        let s = Scratch::new();
        let stem = "x".repeat(200);
        let path = s.0.join(format!("{stem}.jsonl"));
        let other = s.0.join(format!("{stem}-other.jsonl"));
        let mut originals = Vec::new();
        for index in 0..512 {
            let id = uuid::Uuid::new_v4();
            for kind in ["bak", "json"] {
                let name = format!(".{stem}.{id}.replace.{kind}");
                let bytes = format!("precious-{index}-{kind}").into_bytes();
                std::fs::write(s.0.join(&name), &bytes).unwrap();
                originals.push((id.to_string(), kind, bytes));
            }
        }
        let other_id = uuid::Uuid::new_v4();
        let other_prefix = quarantine_prefix(&other).unwrap();
        let other_name = format!("{other_prefix}{other_id}.bak.{}", uuid::Uuid::new_v4());
        std::fs::write(s.0.join(&other_name), b"other session").unwrap();
        let legacy = format!(
            ".{stem}.{}.replace.bak.orphaned.{}",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        );
        // The old format fits only shorter stems; use an independent short session.
        let short = s.0.join("legacy.jsonl");
        let legacy = legacy.replace(&stem, "legacy");
        std::fs::write(s.0.join(&legacy), b"legacy quarantine").unwrap();
        assert!(cleanup_if_missing(&path).unwrap());
        let prefix = quarantine_prefix(&path).unwrap();
        let snapshot = || -> std::collections::BTreeMap<_, _> {
            std::fs::read_dir(&s.0)
                .unwrap()
                .map(Result::unwrap)
                .filter_map(|e| {
                    let name = e.file_name().into_string().unwrap();
                    name.starts_with(&prefix)
                        .then(|| (name, std::fs::read(e.path()).unwrap()))
                })
                .collect()
        };
        let first = snapshot();
        assert_eq!(
            first.len(),
            1024,
            "all names captured before directory mutation"
        );
        for (id, kind, bytes) in originals {
            assert!(!s.0.join(format!(".{stem}.{id}.replace.{kind}")).exists());
            let matches: Vec<_> = first
                .iter()
                .filter(|(name, _)| name.starts_with(&format!("{prefix}{id}.{kind}.")))
                .collect();
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].1, &bytes);
            assert!(matches[0].0.len() <= 255);
        }
        assert!(cleanup_if_missing(&path).unwrap());
        assert_eq!(
            snapshot(),
            first,
            "reopen neither duplicates nor discards recovery bytes"
        );
        std::fs::write(&path, b"new lifetime").unwrap();
        std::fs::remove_file(&path).unwrap();
        discard(&path).unwrap();
        assert!(
            snapshot().is_empty(),
            "intentional delete clears this session quarantine"
        );
        assert_eq!(
            std::fs::read(s.0.join(&other_name)).unwrap(),
            b"other session"
        );
        assert_eq!(
            std::fs::read(s.0.join(&legacy)).unwrap(),
            b"legacy quarantine"
        );
        discard(&short).unwrap();
        assert!(!s.0.join(&legacy).exists());
        assert_eq!(
            std::fs::read(s.0.join(&other_name)).unwrap(),
            b"other session"
        );
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
        // FIFO is an existing nonregular leaf too; its worker has a hard5s bound.
        bounded_fifo_current_cleanup();
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

    #[test]
    #[ignore = "only launched by bounded parent"]
    fn fifo_current_cleanup_worker() {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let s = Scratch::new();
        let path = s.0.join("session.jsonl");
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .args(["-m", "600"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let before = std::fs::symlink_metadata(&path).unwrap();
        let artifacts = s.sidecars();
        for artifact in &artifacts {
            std::fs::write(artifact, b"precious backup").unwrap();
        }
        assert!(
            !cleanup_if_missing(&path).unwrap(),
            "existing FIFO is not missing"
        );
        for artifact in &artifacts {
            assert_eq!(std::fs::read(artifact).unwrap(), b"precious backup");
        }
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert!(after.file_type().is_fifo());
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(
            std::fs::read_dir(&s.0).unwrap().count(),
            3,
            "no quarantine or duplicate artifacts"
        );
        println!("PDRS_FIFO_CLEANUP_OK");
    }
    fn bounded_fifo_current_cleanup() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::fifo_current_cleanup_worker"),
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "cleanup FIFO exceeded5s: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("PDRS_FIFO_CLEANUP_OK"),
            "worker must actually run"
        );
    }
}
