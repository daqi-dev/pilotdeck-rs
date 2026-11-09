//! Durable handoff to the channel message owner. getupdates acknowledges only
//! batches already fsynced here; they remain recoverable until explicit ack.
use super::{
    http::failure,
    storage::{Storage, read_checked, text, write},
};
use crate::config::routes::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct WeixinInbox {
    home: PathBuf,
}
impl WeixinInbox {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
    pub fn pending(&self) -> Result<Vec<Value>> {
        let path = self.home.join("channels/weixin-inbox");
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => return Err(failure("Cannot read Weixin inbox")),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| failure("Cannot read Weixin inbox"))?;
            if entry.path().extension().is_some_and(|e| e == "json") {
                paths.push(entry.path());
            }
        }
        paths.sort();
        paths
            .into_iter()
            .filter_map(|p| read_checked(&p).transpose())
            .collect()
    }
    /// Call only after the channel durably accepts the messages. Mapper changes
    /// are persisted before removing the handoff, so a failed ack is retryable.
    pub fn acknowledge(&self, id: &str, mapper: Option<&Value>) -> Result<()> {
        let path = self.path(id)?;
        if !path
            .try_exists()
            .map_err(|_| failure("Cannot inspect Weixin inbox"))?
        {
            return Ok(());
        }
        if let Some(mapper) = mapper {
            Storage::new(self.home.clone()).save_mapper(mapper)?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(&path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure("Cannot acknowledge Weixin inbox")),
        }
    }
    pub(super) fn enqueue(
        &self,
        credentials: &Value,
        response: &Value,
        messages: &[Value],
        mapper: &Value,
    ) -> Result<String> {
        let identity = json!([
            credentials["accountId"],
            credentials["cursor"],
            response["get_updates_buf"],
            messages
        ]);
        let id = format!("{:x}", Sha256::digest(identity.to_string().as_bytes()));
        let path = self.path(&id)?;
        if !path.exists() {
            write(
                &path,
                &json!({"id":id,"accountId":text(credentials,"accountId"),"cursorBefore":credentials["cursor"],"cursorAfter":response["get_updates_buf"],"messages":messages,"mapper":mapper}),
            )?;
            sync_dir(&path)?;
            // Persist a newly-created inbox directory before acknowledging its cursor.
            sync_dir(path.parent().unwrap())?;
        }
        Ok(id)
    }
    pub(super) fn contains(&self, id: &str) -> bool {
        self.path(id).is_ok_and(|p| p.exists())
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("Invalid Weixin inbox id"));
        }
        Ok(self
            .home
            .join("channels/weixin-inbox")
            .join(format!("{id}.json")))
    }
}
fn sync_dir(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path.parent().unwrap())
        .and_then(|f| f.sync_all())
        .map_err(|_| failure("Cannot sync Weixin inbox"))
}
