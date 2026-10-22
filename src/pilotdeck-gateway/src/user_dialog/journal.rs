//! Durable per-session journal for live user dialogs.
//!
//! The journal is a sibling of a persistent transcript (`<transcript>.dialogs.json`).
//! Ephemeral SDK sessions have no transcript path and therefore no restart state.
//! Records are validated on every read and replaced atomically through a
//! temporary file in the same directory.

use super::{DialogError, DialogRuntime, Result};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use pilotdeck_transport::system_clock::format_epoch_iso;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Default)]
pub struct JournalOptions {
    pub runtime: DialogRuntime,
}

#[derive(Clone, Debug)]
struct JournalRecord {
    dialogs: Vec<JsValue>,
}

/// Creates the Gateway-local journal only for an absolute transcript path.
/// A missing or relative path intentionally yields no recovery state.
pub fn create_gateway_user_dialog_journal(
    transcript_path: Option<&Path>,
    options: JournalOptions,
) -> Option<GatewayUserDialogJournal> {
    let transcript_path = transcript_path?;
    if transcript_path.as_os_str().is_empty() || !transcript_path.is_absolute() {
        return None;
    }
    let mut path = transcript_path.to_path_buf().into_os_string();
    path.push(".dialogs.json");
    Some(GatewayUserDialogJournal::new(PathBuf::from(path), options))
}

/// The channel only needs these two journal operations; keeping them behind a
/// trait lets the host inject a durable implementation without changing the
/// protocol. `GatewayUserDialogJournal` is the local implementation.
pub trait UserDialogJournal: Send + Sync {
    fn record(&self, event: &JsValue) -> Result<()>;
    fn remove(&self, request_id: &JsString) -> Result<()>;
}

pub struct GatewayUserDialogJournal {
    path: PathBuf,
    runtime: DialogRuntime,
}

impl UserDialogJournal for GatewayUserDialogJournal {
    fn record(&self, event: &JsValue) -> Result<()> {
        GatewayUserDialogJournal::record(self, event)
    }
    fn remove(&self, request_id: &JsString) -> Result<()> {
        GatewayUserDialogJournal::remove(self, request_id)
    }
}

impl GatewayUserDialogJournal {
    pub fn new(path: PathBuf, options: JournalOptions) -> Self {
        Self {
            path,
            runtime: options.runtime,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Inserts or replaces the request with the same `requestId`.
    pub fn record(&self, event: &JsValue) -> Result<()> {
        let mut record = self.read()?;
        let id = event.get("requestId").and_then(JsValue::as_string);
        let index = id.and_then(|id| {
            record
                .dialogs
                .iter()
                .position(|dialog| dialog.get("requestId").and_then(JsValue::as_string) == Some(id))
        });
        match index {
            Some(index) => record.dialogs[index] = event.clone(),
            None => record.dialogs.push(event.clone()),
        }
        self.write(&record)
    }

    /// Removes every record with this id. A missing id is a no-op, and removing
    /// the last record deletes the journal file rather than leaving `[]`.
    pub fn remove(&self, request_id: &JsString) -> Result<()> {
        let record = self.read()?;
        let had_dialogs = record.dialogs.len();
        let dialogs: Vec<_> = record
            .dialogs
            .into_iter()
            .filter(|dialog| {
                dialog.get("requestId").and_then(JsValue::as_string) != Some(request_id)
            })
            .collect();
        if dialogs.len() == had_dialogs {
            return Ok(());
        }
        if dialogs.is_empty() {
            return remove_file(&self.path);
        }
        self.write(&JournalRecord { dialogs })
    }

    /// Projects records left by a prior process as restart-terminal state. The
    /// journal is deliberately not removed: a recovery answer still needs the
    /// request contract before the next turn is created.
    pub fn recover(&self) -> Result<Vec<JsValue>> {
        let record = self.read()?;
        if record.dialogs.is_empty() {
            return Ok(Vec::new());
        }
        let terminated_at = format_epoch_iso((self.runtime.now)());
        Ok(record
            .dialogs
            .into_iter()
            .map(|request| {
                let mut event = JsObject::new();
                event.insert("type".into(), "user_dialog_terminated".into());
                event.insert("request".into(), request);
                event.insert("reason".into(), "gateway_restarted".into());
                event.insert(
                    "terminatedAt".into(),
                    JsValue::String(terminated_at.clone().into()),
                );
                event.insert("recovery".into(), "next_turn_context".into());
                JsValue::Object(event)
            })
            .collect())
    }

    pub fn clear(&self) -> Result<()> {
        remove_file(&self.path)
    }

    fn read(&self) -> Result<JournalRecord> {
        let value = (|| -> Result<JournalRecord> {
            let bytes = match fs::read(&self.path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(JournalRecord {
                        dialogs: Vec::new(),
                    });
                }
                Err(error) => return Err(error.into()),
            };
            // Node's readFile('utf8') substitutes invalid UTF-8 instead of failing.
            let parsed = parse_json(&String::from_utf8_lossy(&bytes))
                .map_err(|error| DialogError::Invalid(error.to_string()))?;
            if parsed.get("version").and_then(JsValue::as_number) != Some(1.0) {
                return Err(invalid_record());
            }
            let Some(JsValue::Array(dialogs)) = parsed.get("dialogs") else {
                return Err(invalid_record());
            };
            if dialogs.iter().any(|dialog| !is_dialog_request(dialog)) {
                return Err(invalid_record());
            }
            Ok(JournalRecord {
                dialogs: dialogs.iter().map(JsValue::clone).collect(),
            })
        })();
        value.map_err(|error| {
            DialogError::Invalid(format!(
                "Unable to read Gateway user-dialog journal {}: {error}",
                self.path.display()
            ))
        })
    }

    fn write(&self, record: &JournalRecord) -> Result<()> {
        let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
        mkdir(directory)?;
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dialogs.json".into());
        let temporary = directory.join(format!(".{name}.{}.tmp", (self.runtime.uuid)()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(
                json(&JsValue::Object({
                    let mut object = JsObject::new();
                    object.insert("version".into(), JsValue::Number(1.0));
                    object.insert("dialogs".into(), JsValue::Array(record.dialogs.clone()));
                    object
                }))?
                .as_bytes(),
            )?;
            drop(file);
            fs::rename(&temporary, &self.path)?;
            Ok(())
        })();
        // Cleanup errors take precedence, matching the TS `finally` block.
        remove_file(&temporary)?;
        result
    }
}

fn is_dialog_request(value: &JsValue) -> bool {
    value
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str("user_dialog_request"))
        && value
            .get("requestId")
            .and_then(JsValue::as_string)
            .is_some()
        && value
            .get("dialogKind")
            .and_then(JsValue::as_string)
            .is_some()
}

fn invalid_record() -> DialogError {
    DialogError::Invalid("invalid dialog journal record".into())
}

fn json(value: &JsValue) -> Result<String> {
    stringify_json(value)
        .map_err(|error| DialogError::Invalid(error.to_string()))?
        .ok_or_else(invalid_record)
}

fn mkdir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
