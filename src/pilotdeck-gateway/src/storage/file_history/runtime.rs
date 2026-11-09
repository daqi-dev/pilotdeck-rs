//! Production tool port over the session's existing history and transcript owner.
use super::{FileHistoryError, FileHistoryOptions, FileHistoryStore, SnapshotRecordedEntry};
use crate::storage::recorder::Recorder;
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

/// The session owner supplies its resolved backup directory and replayed history.
/// Never opens a transcript or constructs another writer for the same session.
/// Retain the returned Arc with the session; all turns must share this store.
pub fn create_recorded_file_history(
    mut options: FileHistoryOptions,
    recorder: Arc<Recorder>,
    session_key: JsString,
    restored: &[SnapshotRecordedEntry],
) -> Arc<FileHistoryStore> {
    options.on_snapshot_recorded = Some(Arc::new(move |mut snapshot, kind| {
        // Tools use the host-set messageId (normally turnId), never a caller path.
        let turn = snapshot
            .get("messageId")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| FileHistoryError::Fs("File checkpoint requires a messageId.".into()))?;
        snapshot.insert("type".into(), "file_snapshot_recorded".into());
        snapshot.insert("snapshotKind".into(), kind.into());
        recorder
            .writer()
            .record_session_event(&session_key, &turn, snapshot)
            .map_err(|error| FileHistoryError::Fs(error.to_string()))
    }));
    let history = Arc::new(FileHistoryStore::new(options));
    history.replay_from_transcript(restored);
    history
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput};
    use std::{fs, path::PathBuf};

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recorded_file_history_shares_writer_and_restores_real_backup() {
        let root = Scratch(
            std::env::temp_dir().join(format!("pdrs-history-port-{}", uuid::Uuid::new_v4())),
        );
        fs::create_dir_all(&root.0).unwrap();
        let chats = root.0.join("chats");
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |_| chats.clone()),
        );
        let storage = adapter.create_session_storage(SessionStorageInput {
            project_root: root.0.clone(),
            pilot_home: root.0.join("pilot"),
            session_id: "session".into(),
            now: None,
        });
        let backup_dir = storage.file_history_dir.clone();
        let transcript = storage.transcript_path.clone();
        let recorder = Arc::new(Recorder::from_writer(Arc::new(storage.transcript)));
        recorder
            .start_turn(&"session".into(), &"turn".into())
            .unwrap();
        let history = create_recorded_file_history(
            FileHistoryOptions::new(backup_dir),
            recorder.clone(),
            "session".into(),
            &[],
        );
        let file = root.0.join("proof.txt");
        fs::write(
            &file,
            "original
",
        )
        .unwrap();
        history.track_edit(file.to_str().unwrap(), "turn").unwrap();
        fs::write(
            &file, "changed
",
        )
        .unwrap();
        history
            .mark_edit_committed(file.to_str().unwrap(), "turn")
            .unwrap();
        let entries = crate::storage::read_transcript(&transcript)
            .unwrap()
            .entries;
        let snapshots: Vec<_> = entries
            .iter()
            .filter(|e| e.get("type") == Some(&JsValue::from("file_snapshot_recorded")))
            .collect();
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots[1].get("expectedFileStates").is_some());
        assert_eq!(entries[0].get("sequence"), Some(&JsValue::Number(1.0)));
        assert_eq!(snapshots[1].get("sequence"), Some(&JsValue::Number(3.0)));
        assert_eq!(snapshots[0].get("parentEntryId"), entries[0].get("entryId"));
        assert_eq!(
            snapshots[1].get("parentEntryId"),
            snapshots[0].get("entryId")
        );
        assert_eq!(
            snapshots[1].get("sessionId"),
            Some(&JsValue::from("session"))
        );
        assert_eq!(snapshots[1].get("messageId"), Some(&JsValue::from("turn")));
        assert!(history.get_conflict_paths("turn").unwrap().is_empty());
        let stats = history.get_diff_stats("turn").unwrap();
        assert!(stats.insertions > 0 && stats.deletions > 0);
        let restored = history.rewind("turn").unwrap();
        assert_eq!(
            restored.files_changed,
            vec![file.to_str().unwrap().to_string()]
        );
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "original
"
        );
    }
}
