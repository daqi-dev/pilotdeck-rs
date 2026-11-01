//! Cron storage and its one-shot v1 store migration, ported from the TS
//! baseline `src/cron/storage/` (CronPaths.ts / CronTaskStore.ts /
//! CronStoreMigration.ts). Storage contract only: nothing here is wired into
//! the default proxy route (#1216 A8.3-pr1a).
mod migration;
mod paths;
mod store;

pub use migration::{MigrationInput, migrate_cron_stores};
pub use paths::{CronPaths, resolve_cron_paths};
pub use store::CronTaskStore;

#[cfg(test)]
mod tests;

use std::{
    fmt, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use icu_collator::CollatorBorrowed;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

use crate::{js_value, node_locale::node_locale, project_files::node_collator};

/// TS `CronRuntimeLogger` (src/cron/runtime/CronRuntime.ts), storage slice:
/// the migration's `warn` channel.
pub trait CronRuntimeLogger {
    fn warn(&self, message: &str, meta: &JsObject);
}

#[derive(Debug)]
pub enum CronStoreError {
    Io(io::Error),
    /// TS errors that cross without an errno: the `TypeError` from a
    /// non-string `nextRunAt` reaching `localeCompare` (CronTaskStore.ts:177)
    /// and `String()` conversion overflow.
    Type(String),
    /// CronStoreMigration.ts:448.
    LockTimeout(String),
}
impl From<io::Error> for CronStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl fmt::Display for CronStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Type(message) | Self::LockTimeout(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for CronStoreError {}

/// The `localeCompare` collator: `node_collator` on the process locale, the
/// same resolution every other collation-sensitive port uses.
pub(crate) fn collator() -> Result<CollatorBorrowed<'static>, CronStoreError> {
    node_collator(&node_locale(|key| std::env::var(key).ok()))
        .map_err(|_| CronStoreError::Type("Collation data unavailable.".into()))
}

/// `String(value ?? "")` (CronStoreMigration.ts:377,389-390): nullish becomes
/// the empty string, everything else goes through ECMAScript `String()`.
pub(crate) fn string_or_empty(value: Option<&JsValue>) -> Result<JsString, CronStoreError> {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(JsString::from("")),
        Some(other) => js_value::to_string_units(other)
            .map(JsString::from_utf16)
            .map_err(|_| CronStoreError::Type("Maximum call stack size exceeded".into())),
    }
}

/// ECMAScript truthiness (CronTaskStore.ts:187 `!candidate.schedule`).
pub(crate) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => false,
        JsValue::Bool(true) | JsValue::Object(_) | JsValue::Array(_) => true,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.is_empty(),
    }
}

/// JS `===` (CronTaskStore.ts:28,41,59,70 compare `taskId` values). A missing
/// property reads as `undefined`, so it equals an explicit `undefined`.
pub(crate) fn strict_equals(left: Option<&JsValue>, right: Option<&JsValue>) -> bool {
    let undefined = JsValue::Undefined;
    let left = left.unwrap_or(&undefined);
    let right = right.unwrap_or(&undefined);
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b,
        (JsValue::String(a), JsValue::String(b)) => a == b,
        _ => false,
    }
}

/// `JSON.stringify(value, null, 2)`. Third private copy in the workspace
/// (workspace_snapshot, dream; merge tracked as follow-up). Unlike those, the
/// key enumeration goes through `own_entries()` because cron tasks round-trip
/// arbitrary parsed JSON, whose canonical array-index keys `JSON.stringify`
/// enumerates first.
pub(crate) fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &JsValue, depth: usize, out: &mut String) {
    match value {
        JsValue::Undefined | JsValue::Null => out.push_str("null"),
        JsValue::Bool(bit) => out.push_str(if *bit { "true" } else { "false" }),
        JsValue::Number(number) => {
            if !number.is_finite() {
                out.push_str("null");
            } else {
                out.push_str(&js_value::number_string(*number));
            }
        }
        JsValue::String(text) => out.push_str(&pilotdeck_protocol::quote_json_string(text)),
        JsValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        JsValue::Object(object) => {
            let entries: Vec<_> = object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .collect();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                out.push_str(&pilotdeck_protocol::quote_json_string(key));
                out.push_str(": ");
                write_pretty(value, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// `JSON.stringify(value)` for records the port hands to `appendFile`: always
/// an object here, so only a codec failure (or top-level `undefined`) errors.
pub(crate) fn stringify_compact(value: &JsValue) -> Result<String, CronStoreError> {
    pilotdeck_protocol::stringify_json(value)
        .map_err(|error| CronStoreError::Type(error.to_string()))?
        .ok_or_else(|| CronStoreError::Type("value is not JSON".into()))
}

/// Temp-file suffix shared by both atomic writers:
/// `{path}.{pid}.{Date.now()}.{n}.tmp`. The counter stands in for TS
/// `randomUUID()` (CronTaskStore.ts:140); the name never reaches a recorded
/// byte, it only has to be unique per process.
pub(crate) fn temp_suffix(now_ms: f64) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        ".{}.{}.{}.tmp",
        std::process::id(),
        now_ms as u64,
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// `new Date(now_ms).toISOString()`. Gregorian civil date from days since the
/// Unix epoch, no process locale or timezone.
pub(crate) fn iso_from_epoch_ms(now_ms: f64) -> String {
    let total = now_ms.floor() as i64;
    let (days, time) = (total.div_euclid(86_400_000), total.rem_euclid(86_400_000));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day = z.rem_euclid(146_097);
    let year_of_era = (day - day / 1460 + day / 36_524 - day / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{date:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time % 60_000 / 1000,
        time % 1000
    )
}

/// The parent directory for `mkdir dirname(path)`: TS would throw on a
/// rootless path, which only a relative `pilotHome` could produce and
/// `resolve` already ruled out.
pub(crate) fn parent_of(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path has no parent directory"))
}
