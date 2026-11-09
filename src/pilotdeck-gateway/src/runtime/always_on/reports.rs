//! Native backing of DiscoveryReportStore. Appends keep existing records on
//! quota failure; all disk/JSON work stays inside storage::run's owned job.
use super::{AlwaysOnPaths, paths::report_markdown_path, storage};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{io, path::PathBuf};

#[derive(Clone)]
pub(crate) struct DiscoveryReportStore {
    paths: AlwaysOnPaths,
}
impl DiscoveryReportStore {
    pub(crate) fn new(paths: AlwaysOnPaths) -> Self {
        Self { paths }
    }

    pub(crate) async fn write_report(
        &self,
        run_id: JsString,
        markdown: JsString,
    ) -> io::Result<PathBuf> {
        let paths = self.paths.clone();
        storage::run(move || {
            let file = report_markdown_path(&paths, &run_id);
            // fs.writeFile(..., "utf-8") replaces lone UTF-16 surrogates.
            let text = String::from_utf16_lossy(markdown.units());
            storage::write(
                &storage::root_for(&paths.state_file),
                &file,
                text.as_bytes(),
                storage::MARKDOWN_BYTES,
            )?;
            Ok(file)
        })
        .await
    }

    pub(crate) async fn append_run_event(
        &self,
        run_id: JsString,
        event: JsObject,
    ) -> io::Result<()> {
        let paths = self.paths.clone();
        storage::run(move || append(&paths, paths.run_events_path(&run_id), event)).await
    }

    pub(crate) async fn append_history(&self, event: JsObject) -> io::Result<()> {
        let paths = self.paths.clone();
        storage::run(move || append(&paths, paths.run_history_file(), event)).await
    }

    pub(crate) async fn append_event(&self, event: JsObject) -> io::Result<()> {
        let paths = self.paths.clone();
        storage::run(move || append(&paths, paths.events_file(), event)).await
    }
}
fn append(paths: &AlwaysOnPaths, file: PathBuf, event: JsObject) -> io::Result<()> {
    storage::check_object(&event)?;
    let mut text = stringify_json(&JsValue::Object(event))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Always-On event was undefined.")
        })?;
    storage::check_json(&text)?;
    text.push('\n');
    storage::append(
        &storage::root_for(&paths.state_file),
        &file,
        text.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn report_and_jsonl_persist_real_bytes_and_preserve_json_utf16() {
        let root =
            std::env::temp_dir().join(format!("pdrs-always-reports-{}", uuid::Uuid::new_v4()));
        let paths = AlwaysOnPaths {
            project_key: "/project".into(),
            state_file: root.join("state.json"),
            plans_dir: root.join("plans"),
            plan_index_file: root.join("plans/index.json"),
            cycle_index_file: root.join("cycles/index.json"),
            reports_dir: root.join("reports"),
            discovery_lock_file: root.join("locks/discovery.lock"),
        };
        let store = DiscoveryReportStore::new(paths.clone());
        let run = JsString::from_utf16(vec![114, 0xd800]);
        let text = JsString::from_utf16(vec![65, 0xd800, 66]);
        let report = store.write_report(run.clone(), text).await.unwrap();
        let mut event = JsObject::new();
        event.insert("type".into(), "fixture".into());
        event.insert("runId".into(), JsValue::String(run.clone()));
        store
            .append_run_event(run.clone(), event.clone())
            .await
            .unwrap();
        store
            .append_run_event(run.clone(), event.clone())
            .await
            .unwrap();
        store.append_history(event.clone()).await.unwrap();
        let bytes = std::fs::read(report).unwrap();
        let log = std::fs::read_to_string(paths.run_events_path(&run)).unwrap();
        let history = std::fs::read_to_string(paths.run_history_file()).unwrap();
        let clean = std::fs::remove_dir_all(root);
        assert!(clean.is_ok());
        assert_eq!(bytes, "A\u{fffd}B".as_bytes());
        assert!(log.ends_with('\n'));
        assert_eq!(log.lines().count(), 2);
        for line in log.lines().chain(history.lines()) {
            assert_eq!(
                pilotdeck_protocol::parse_json(line).unwrap(),
                JsValue::Object(event.clone())
            );
        }
    }
}
