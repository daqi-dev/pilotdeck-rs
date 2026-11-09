//! SESSION and turn use the same JSONL layout; the adapter adds disk catalog/deletion.
use crate::storage::{
    JsonlTranscriptStore, LedgerError, ProjectKey, StoreResult, StoredSession, TranscriptKey,
    TranscriptRead, TranscriptStore, sanitize_session_id,
};
use pilotdeck_protocol::JsValue;
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

pub struct SessionTranscriptStore {
    home: PathBuf,
    cwd: PathBuf,
    jsonl: JsonlTranscriptStore,
}

impl SessionTranscriptStore {
    pub fn new(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            home,
            cwd,
            jsonl: JsonlTranscriptStore::new(),
        }
    }

    fn chat_dir(&self, project: &Path, home: &Path) -> StoreResult<PathBuf> {
        if home != self.home {
            return Err(LedgerError::Invalid(
                "Transcript PilotHome does not match its owner.".into(),
            ));
        }
        let project = project
            .to_str()
            .ok_or_else(|| LedgerError::Invalid("Invalid project path.".into()))?;
        let chat = crate::session_chat_dir(&self.home, &self.cwd, project);
        self.confined(&chat)?;
        Ok(chat)
    }

    fn confined(&self, path: &Path) -> StoreResult<()> {
        let relative = path
            .strip_prefix(&self.home)
            .map_err(|_| LedgerError::Invalid("Transcript path escapes PilotHome.".into()))?;
        let mut current = self.home.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(LedgerError::Invalid("Invalid transcript path.".into()));
            };
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(LedgerError::Invalid(
                        "Transcript path contains a symbolic link.".into(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn check(&self, key: &TranscriptKey) -> StoreResult<PathBuf> {
        let chat = self.chat_dir(&key.project_root, &key.pilot_home)?;
        let relative = key
            .transcript_path
            .strip_prefix(&chat)
            .map_err(|_| LedgerError::Invalid("Transcript path escapes its project.".into()))?;
        if relative.as_os_str().is_empty() {
            return Err(LedgerError::Invalid(
                "Transcript path requires a filename.".into(),
            ));
        }
        self.confined(&key.transcript_path)?;
        Ok(chat)
    }

    fn remove_file(&self, path: &Path) -> StoreResult<()> {
        self.confined(path)?;
        match fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result.map_err(Into::into),
        }
    }
}

impl TranscriptStore for SessionTranscriptStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        self.check(key)?;
        self.jsonl.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, max_bytes: Option<u64>) -> StoreResult<TranscriptRead> {
        self.check(key)?;
        self.jsonl.read(key, max_bytes)
    }
    fn has(&self, key: &TranscriptKey) -> Option<StoreResult<bool>> {
        Some((|| {
            self.check(key)?;
            match fs::metadata(&key.transcript_path) {
                Ok(metadata) => Ok(metadata.is_file()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        })())
    }
    fn list(&self, key: &ProjectKey) -> Option<StoreResult<Vec<StoredSession>>> {
        Some((|| {
            let chat = self.chat_dir(&key.project_root, &key.pilot_home)?;
            let entries = match fs::read_dir(&chat) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error.into()),
            };
            let mut records = Vec::new();
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
                    || !entry.file_type()?.is_file()
                {
                    continue;
                }
                self.confined(&path)?;
                let id = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| LedgerError::Invalid("Invalid transcript filename.".into()))?;
                let metadata = entry.metadata()?;
                let modified = metadata
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| LedgerError::Invalid(error.to_string()))?;
                records.push(StoredSession {
                    session_id: id.into(),
                    last_modified: modified.as_secs_f64() * 1000.0,
                    file_size: Some(metadata.len() as f64),
                });
            }
            // Stable input order makes equal-mtime sessions deterministic.
            records.sort_by(|a, b| a.session_id.units().cmp(b.session_id.units()));
            Ok(records)
        })())
    }
    fn delete(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        Some(
            self.check(key)
                .and_then(|_| self.remove_file(&key.transcript_path)),
        )
    }
    fn delete_session(&self, key: &TranscriptKey) -> Option<StoreResult<()>> {
        Some((|| {
            let chat = self.check(key)?;
            let id = sanitize_session_id(&key.session_id);
            if id == "." || id == ".." || key.transcript_path != chat.join(format!("{id}.jsonl")) {
                return Err(LedgerError::Invalid(
                    "Invalid session transcript path.".into(),
                ));
            }
            let sidechains = chat.join(&id);
            self.confined(&sidechains)?;
            let projection = chat.join(format!("{id}.jsonl.projections.json"));
            self.confined(&projection)?;
            self.remove_file(&key.transcript_path)?;
            self.remove_file(&projection)?;
            match fs::remove_dir_all(sidechains) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result.map_err(Into::into),
            }
        })())
    }
    fn replace(&self, key: &TranscriptKey, entries: &[JsValue]) -> Option<StoreResult<()>> {
        Some(self.check(key).and_then(|_| {
            self.jsonl
                .replace(key, entries)
                .expect("JSONL store supports atomic replacement")
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{AsyncTranscriptAdapter, SessionStorageInput};
    use std::sync::Arc;

    #[test]
    fn disk_catalog_survives_reopen_and_delete_is_project_scoped() {
        let scratch = crate::scratch::Scratch::new("session-route-store");
        let home = scratch.path().join("home");
        let cwd = scratch.path().to_path_buf();
        let store = Arc::new(SessionTranscriptStore::new(home.clone(), cwd.clone()));
        let layout = store.clone();
        let adapter = AsyncTranscriptAdapter::new(
            store,
            Arc::new(move |key| layout.chat_dir(&key.project_root, &key.pilot_home).unwrap()),
        );
        let storage = adapter.create_session_storage(SessionStorageInput {
            project_root: cwd.clone(),
            pilot_home: home.clone(),
            session_id: "one".into(),
            now: None,
        })
        .unwrap();
        let key = TranscriptKey {
            project_root: cwd.clone(),
            pilot_home: home.clone(),
            session_id: "one".into(),
            transcript_path: storage.transcript_path.clone(),
        };
        storage
            .transcript
            .record_session_metadata(
                &"one".into(),
                &"meta".into(),
                crate::metadata::object([("title", "kept".into())]),
            )
            .unwrap();
        let store = SessionTranscriptStore::new(home.clone(), cwd.clone());
        let records = store
            .list(&ProjectKey {
                project_root: cwd.clone(),
                pilot_home: home.clone(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].session_id.equals_str("one"));
        assert!(store.read(&key, None).unwrap().diagnostics.is_empty());
        let outside = TranscriptKey {
            transcript_path: scratch.path().join("outside.jsonl"),
            ..key.clone()
        };
        assert!(store.delete(&outside).unwrap().is_err());
        store.delete_session(&key).unwrap().unwrap();
        assert!(!store.has(&key).unwrap().unwrap());
        assert!(
            store
                .list(&ProjectKey {
                    project_root: cwd,
                    pilot_home: home
                })
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn transcript_symlink_cannot_read_replace_or_delete_external_data() {
        use std::os::unix::fs::symlink;
        let scratch = crate::scratch::Scratch::new("session-route-store-link");
        let home = scratch.path().join("home");
        let cwd = scratch.path().to_path_buf();
        let store = SessionTranscriptStore::new(home.clone(), cwd.clone());
        let chat = store.chat_dir(&cwd, &home).unwrap();
        fs::create_dir_all(&chat).unwrap();
        let outside = scratch.path().join("outside.jsonl");
        fs::write(&outside, "protected").unwrap();
        let path = chat.join("link.jsonl");
        symlink(&outside, &path).unwrap();
        let key = TranscriptKey {
            project_root: cwd,
            pilot_home: home,
            session_id: "link".into(),
            transcript_path: path,
        };
        assert!(store.read(&key, None).is_err());
        assert!(store.replace(&key, &[]).unwrap().is_err());
        assert!(store.delete_session(&key).unwrap().is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "protected");
    }
}
