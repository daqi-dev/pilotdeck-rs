//! Descriptor-only native output storage. Native providers pin directories and
//! files before execution; the legacy path substrate remains unchanged.
use pilotdeck_protocol::JsString;
use std::{fs::File, io, path::PathBuf};

pub(crate) struct NativeOutputFile {
    pub file: File,
    pub path: PathBuf,
    /// A restored view of a task still owned by another live generation may
    /// read its checkpoint prefix while the original owner appends. It cannot
    /// write that task's metadata back into the shared archive.
    pub allow_prefix: bool,
    pub link_check: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
}
pub(crate) trait NativeOutputStorage: Send + Sync {
    fn create(&self, task_id: &JsString) -> io::Result<NativeOutputFile>;
    fn restore(
        &self,
        task_id: &JsString,
        expected_bytes: u64,
    ) -> io::Result<Option<NativeOutputFile>>;
}
