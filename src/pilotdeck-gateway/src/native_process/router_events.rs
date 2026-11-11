//! ProjectRouterEventBusProvider: best-effort, append-only router observation.
use crate::runtime::model::router::RouterEventSink;
use pilotdeck_protocol::{JsValue, stringify_json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub(super) struct RouterEventJournal {
    path: PathBuf,
    writer: Mutex<()>,
}
impl RouterEventJournal {
    pub(super) fn new(home: &Path) -> Self {
        let directory = home.join("router");
        let path = directory.join("events.jsonl");
        let _ = fs::create_dir_all(directory);
        let legacy = home.join("router-events.jsonl");
        // A current journal wins. The legacy original is retained if migration
        // fails, matching the baseline's best-effort startup behavior.
        if !path.exists() && legacy.exists() {
            let _ = fs::rename(legacy, &path);
        }
        Self {
            path,
            writer: Mutex::new(()),
        }
    }
}
impl RouterEventSink for RouterEventJournal {
    fn emit(&self, event: JsValue) {
        let Ok(Some(mut line)) = stringify_json(&event) else {
            return;
        };
        line.push('\n');
        let _guard = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Journal failure must never change a routing decision.
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
    #[test]
    fn migration_and_append_preserve_old_events_and_never_replace_current_log() {
        let root =
            std::env::temp_dir().join(format!("pdrs-router-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("router-events.jsonl"), "{\"old\":true}\n").unwrap();
        let journal = RouterEventJournal::new(&root);
        let event = object([
            ("type", "pilotdeck_router_decision".into()),
            ("sessionId", "test".into()),
        ]);
        journal.emit(event.clone());
        let expected = format!(
            "{{\"old\":true}}\n{}\n",
            stringify_json(&event).unwrap().unwrap()
        );
        assert_eq!(
            fs::read_to_string(root.join("router/events.jsonl")).unwrap(),
            expected
        );
        assert!(!root.join("router-events.jsonl").exists());
        fs::write(root.join("router-events.jsonl"), "retained legacy").unwrap();
        let _next = RouterEventJournal::new(&root);
        assert_eq!(
            fs::read_to_string(root.join("router/events.jsonl")).unwrap(),
            expected
        );
        assert_eq!(
            fs::read_to_string(root.join("router-events.jsonl")).unwrap(),
            "retained legacy"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unavailable_journal_does_not_fail_the_event_producer() {
        let root =
            std::env::temp_dir().join(format!("pdrs-router-journal-file-{}", uuid::Uuid::new_v4()));
        fs::write(&root, "file blocks directory").unwrap();
        RouterEventJournal::new(&root).emit(object([("type", "pilotdeck_router_decision".into())]));
        assert_eq!(fs::read_to_string(&root).unwrap(), "file blocks directory");
        fs::remove_file(root).unwrap();
    }
}
