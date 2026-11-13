//! Port of baseline `src/adapters/channel/protocol/ChannelRuntimeStatus.ts`:
//! the durable per-channel runtime-status sink under
//! `<pilotHome>/channels/runtime-status.json`. Both the lifecycle core (its
//! four states) and the weixin adapter (its login states) report through this
//! one writer; `updatedAt` is the report wall clock and is masked in goldens.
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::weixin::stringify_pretty;

/// TS `ChannelRuntimeStatusUpdate` minus the channel key it is reported for.
pub struct ChannelRuntimeStatusUpdate {
    pub state: String,
    pub message: Option<String>,
    pub account_id: Option<JsString>,
    pub error: Option<String>,
    pub qr_url: Option<JsString>,
}

pub type ChannelRuntimeStatusReporter =
    Arc<dyn Fn(&JsString, &ChannelRuntimeStatusUpdate) + Send + Sync>;

/// `new Date(0).toISOString()` — the empty snapshot's `updatedAt`.
const EPOCH_ISO: &str = "1970-01-01T00:00:00.000Z";

pub fn channel_runtime_status_path(pilot_home: &Path) -> PathBuf {
    pilot_home.join("channels").join("runtime-status.json")
}

/// `readChannelRuntimeStatusSnapshot`: an absent or unparsable file yields
/// the epoch empty snapshot instead of failing the report.
pub(crate) fn read_snapshot(pilot_home: &Path) -> JsObject {
    let empty = || {
        let mut snapshot = JsObject::new();
        snapshot.insert("updatedAt".into(), EPOCH_ISO.into());
        snapshot.insert("channels".into(), JsValue::Object(JsObject::new()));
        snapshot
    };
    let Ok(raw) = std::fs::read_to_string(channel_runtime_status_path(pilot_home)) else {
        return empty();
    };
    let Ok(parsed) = parse_json(&raw) else {
        return empty();
    };
    let mut snapshot = empty();
    if let Some(updated_at) = parsed.get("updatedAt").and_then(JsValue::as_string) {
        snapshot.insert("updatedAt".into(), JsValue::String(updated_at.clone()));
    }
    if let Some(channels) = parsed.get("channels").and_then(JsValue::as_object) {
        snapshot.insert("channels".into(), JsValue::Object(channels.clone()));
    }
    snapshot
}

/// `createChannelRuntimeStatusReporter` — synchronous fs, like the TS module.
/// A write failure surfaces as the caller's warn (the TS safe reporter's
/// catch), not a panic.
pub fn create_channel_runtime_status_reporter(pilot_home: PathBuf) -> ChannelRuntimeStatusReporter {
    create_channel_runtime_status_reporter_locked(pilot_home, Arc::new(Mutex::new(())))
}

/// The lock-sharing variant (#1725#1): TS's read-modify-write runs on one
/// event loop, so concurrent reports cannot interleave; here the whole
/// read-snapshot → merge → publish stretch must sit inside one critical
/// section, or two threads that read the same snapshot each publish their own
/// half and the other's update is lost. Callers that build several reporters
/// for the same home can share one lock (and one file) through it.
pub fn create_channel_runtime_status_reporter_locked(
    pilot_home: PathBuf,
    lock: Arc<Mutex<()>>,
) -> ChannelRuntimeStatusReporter {
    Arc::new(move |channel_key, update| {
        // #1725#1: the entire read-modify-write under one guard — the TS
        // module's synchronous handler is atomic on its single event loop,
        // and the two NativeChannels halves share this reporter, so one lock
        // covers both reporters' interleavings in-process (a cross-process
        // lost update stays possible, same as TS would have it).
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = now_iso();
        let mut channels = match read_snapshot(&pilot_home)
            .get("channels")
            .and_then(JsValue::as_object)
        {
            Some(channels) => channels.clone(),
            None => JsObject::new(),
        };
        // Entry order: channelKey, state, updatedAt, then the optional
        // fields — each only when its value is truthy (a non-empty string).
        let mut entry = JsObject::new();
        entry.insert("channelKey".into(), JsValue::String(channel_key.clone()));
        entry.insert(
            "state".into(),
            JsValue::String(update.state.as_str().into()),
        );
        entry.insert("updatedAt".into(), JsValue::String(now.clone()));
        if let Some(message) = update.message.clone().filter(|text| !text.is_empty()) {
            entry.insert("message".into(), JsValue::String(JsString::from(message)));
        }
        if let Some(account_id) = &update.account_id
            && !account_id.is_empty()
        {
            entry.insert("accountId".into(), JsValue::String(account_id.clone()));
        }
        if let Some(error) = update.error.clone().filter(|text| !text.is_empty()) {
            entry.insert("error".into(), JsValue::String(JsString::from(error)));
        }
        if let Some(qr_url) = &update.qr_url
            && !qr_url.is_empty()
        {
            entry.insert("qrUrl".into(), JsValue::String(qr_url.clone()));
        }
        channels.insert(channel_key.clone(), JsValue::Object(entry));
        let mut next = JsObject::new();
        next.insert("updatedAt".into(), JsValue::String(now));
        next.insert("channels".into(), JsValue::Object(channels));
        let path = channel_runtime_status_path(&pilot_home);
        let written = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // #1725#2: publish through a tmp + rename — the final bytes are
            // the writeFileSync shape byte for byte (same stringify, same
            // trailing newline), but a reader (or a crash) never observes a
            // half-written file. The tmp name carries a uuid suffix, like
            // the state-persistence lane's writes.
            let temporary = path.with_file_name(format!(
                ".{}.{}.tmp",
                path.file_name()
                    .expect("#1725 status file name")
                    .to_string_lossy(),
                &uuid::Uuid::new_v4().to_string()[..8]
            ));
            std::fs::write(
                &temporary,
                format!("{}\n", stringify_pretty(&JsValue::Object(next))),
            )?;
            std::fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(error) = written {
            // #2558: the failure line itself must not panic (std panics when
            // a raw console write fails) — the status task would die on the
            // write it only owed a report for.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "[adapters] failed to write runtime status for {}: {error}",
                    String::from_utf16_lossy(channel_key.units())
                ),
            );
        }
    })
}

fn now_iso() -> JsString {
    use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
    SystemExecutionClock.now_iso()
}
