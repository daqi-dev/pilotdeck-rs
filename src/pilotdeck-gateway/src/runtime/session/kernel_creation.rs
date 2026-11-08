//! Durable product-side creation intent. This is not a grant or an execution
//! receipt. Every file is retained after an uncertain write for reconciliation.
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}
fn reject_symlink(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::other(
            "Creation owner paths must not be symbolic links.",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
fn read(path: &Path) -> io::Result<Value> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Creation state must be a regular file."));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
/// An unpublished but retained intent also owns the backend choice. Losing a
/// local publication ACK must never route this session into the legacy lane.
pub(super) fn owner_or_intent(path: &Path) -> io::Result<Option<Value>> {
    match read(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match read(&sibling(path, ".creation.intent")) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("Creation owner has no parent."))?,
    )?
    .sync_all()
}
fn open_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn write_once(path: &Path, value: &Value) -> io::Result<()> {
    reject_symlink(path)?;
    match open_new(path) {
        Ok(mut file) => {
            file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
            file.sync_all()?;
            sync_parent(path)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read(path)? != *value {
                return Err(io::Error::other(
                    "Creation staging identity conflicts; original files retained.",
                ));
            }
            File::open(path)?.sync_all()?;
            sync_parent(path)
        }
        Err(error) => Err(error),
    }
}
/// Held through create/query/open/confirmation. File locks release on process
/// exit; the persistent lock inode is not an abandoned-lock tombstone.
pub(super) struct Claim {
    path: PathBuf,
    _lock: File,
    pub document: Value,
}
impl Claim {
    pub fn acquire(path: &Path, proposed: impl FnOnce() -> Value) -> io::Result<Self> {
        let lock_path = sibling(path, ".creation.lock");
        reject_symlink(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lock = options.open(lock_path)?;
        if !lock.metadata()?.is_file() {
            return Err(io::Error::other("Creation lock must be a regular file."));
        }
        lock.try_lock().map_err(|error| {
            io::Error::other(format!(
                "Creation intent is owned by another process: {error}"
            ))
        })?;
        reject_symlink(path)?;
        let document = match read(path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let intent_path = sibling(path, ".creation.intent");
                let intent = match read(&intent_path) {
                    Ok(value) => value,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let value = proposed();
                        write_once(&intent_path, &value)?;
                        value
                    }
                    Err(error) => return Err(error),
                };
                // The source is synced before linking; readers never see a
                // partially written owner, and publication cannot clobber one.
                File::open(&intent_path)?.sync_all()?;
                std::fs::hard_link(&intent_path, path)?;
                sync_parent(path)?;
                intent
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: path.into(),
            _lock: lock,
            document,
        })
    }
    pub fn was_dispatched(&self) -> io::Result<bool> {
        let marker = sibling(&self.path, ".creation.dispatch");
        match read(&marker) {
            Ok(value)
                if value == serde_json::json!({"creation_key":self.document["creation_key"]}) =>
            {
                Ok(true)
            }
            Ok(_) => Err(io::Error::other(
                "Creation dispatch marker identity conflicts; original files retained.",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn mark_dispatched(&self) -> io::Result<()> {
        write_once(
            &sibling(&self.path, ".creation.dispatch"),
            &serde_json::json!({"creation_key":self.document["creation_key"]}),
        )
    }
    pub fn confirm(&mut self, confirmed: Value) -> io::Result<()> {
        if read(&self.path)? != self.document {
            return Err(io::Error::other(
                "Creation owner changed before confirmation; original files retained.",
            ));
        }
        let staging = sibling(&self.path, ".creation.confirming");
        write_once(&staging, &confirmed)?;
        std::fs::rename(&staging, &self.path)?;
        sync_parent(&self.path)?;
        self.document = confirmed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::{Command, Stdio};
    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!("pd-creation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn partial_intent_and_conflicting_owner_never_generate_a_replacement_key() {
        let root = scratch();
        let owner = root.join("owner.json");
        std::fs::write(sibling(&owner, ".creation.intent"), b"{\"creation_key\":").unwrap();
        assert!(Claim::acquire(&owner, || panic!("partial intent cannot be replaced")).is_err());
        assert!(!owner.exists());
        let other = root.join("other.json");
        let mut claim = Claim::acquire(
            &other,
            || json!({"creation_key":"original","state":"pending"}),
        )
        .unwrap();
        assert!(!claim.was_dispatched().unwrap());
        claim.mark_dispatched().unwrap();
        assert!(claim.was_dispatched().unwrap());
        std::fs::write(&other, b"{\"creation_key\":\"other-writer\"}").unwrap();
        assert!(
            claim
                .confirm(json!({"creation_key":"original","state":"confirmed"}))
                .is_err()
        );
        assert_eq!(read(&other).unwrap()["creation_key"], "other-writer");
        drop(claim);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn published_owner_is_complete_and_dispatch_identity_survives_confirmation() {
        let root = scratch();
        let owner = root.join("owner.json");
        let mut claim = Claim::acquire(
            &owner,
            || json!({"creation_key":"original","state":"pending"}),
        )
        .unwrap();
        assert_eq!(read(&owner).unwrap(), claim.document);
        claim.mark_dispatched().unwrap();
        claim
            .confirm(
                json!({"creation_key":"original","state":"confirmed","session_id":"host-original"}),
            )
            .unwrap();
        drop(claim);
        let reopened =
            Claim::acquire(&owner, || panic!("must reuse original durable owner")).unwrap();
        assert!(reopened.was_dispatched().unwrap());
        assert_eq!(reopened.document["session_id"], "host-original");
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn child_claimant() {
        let Some(path) = std::env::var_os("PDRS_BOOT_CLAIM_CHILD_PATH") else {
            return;
        };
        let claim = match Claim::acquire(Path::new(&path), || json!({"creation_key":"child"})) {
            Ok(claim) => claim,
            Err(_) => std::process::exit(67),
        };
        if std::env::var_os("PDRS_BOOT_CLAIM_CRASH").is_some() {
            println!("BOOT_CLAIM_READY");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        assert_eq!(claim.document["creation_key"], "original");
    }
    fn child(owner: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::session::kernel::creation::tests::child_claimant",
                "--nocapture",
            ])
            .env("PDRS_BOOT_CLAIM_CHILD_PATH", owner)
            .stderr(Stdio::null());
        command
    }
    #[test]
    fn two_process_claims_share_one_key_and_process_death_releases_lock() {
        use std::io::BufRead;
        let root = scratch();
        let owner = root.join("owner.json");
        let claim = Claim::acquire(&owner, || json!({"creation_key":"original"})).unwrap();
        assert_eq!(
            child(&owner).stdout(Stdio::null()).status().unwrap().code(),
            Some(67)
        );
        drop(claim);
        let mut holder = child(&owner)
            .env("PDRS_BOOT_CLAIM_CRASH", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(holder.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.contains("BOOT_CLAIM_READY") {
                break;
            }
        }
        assert!(Claim::acquire(&owner, || panic!("cannot replace held key")).is_err());
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert!(
            child(&owner)
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(read(&owner).unwrap()["creation_key"], "original");
        std::fs::remove_dir_all(root).unwrap();
    }
}
