//! `get_current_time` (baseline `tool/builtin/getCurrentTime.ts`). Formatting
//! uses the same checked system TZif profile as the gateway's date parser via
//! `pilotdeck-transport`; no new timezone crate is linked into the gateway.
use super::{object, string, text, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};

const WEEKDAYS: &[&str] = &[
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

pub struct GetCurrentTimeTool {
    fields: JsObject,
}

impl GetCurrentTimeTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for GetCurrentTimeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for GetCurrentTimeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for GetCurrentTimeTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("get_current_time")),
        ("title", string("Get Current Time")),
        (
            "description",
            string(
                "Return the current local time for a timezone. Use this before creating cron_create tasks with absolute dates/times such as tonight, tomorrow morning, or next Monday. For simple relative delays like 'in 10 minutes', prefer cron_create with schedule.type='delay' instead of calling this tool.",
            ),
        ),
        ("kind", string("custom")),
        (
            "inputSchema",
            JsValue::Object(super::schema(
                r#"{"type":"object","additionalProperties":false,"properties":{"timezone":{"type":"string","description":"IANA timezone, e.g. Asia/Shanghai. Defaults to the host local timezone."}}}"#,
            )),
        ),
    ])
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let timezone = input
        .get("timezone")
        .and_then(JsValue::as_string)
        .map(text)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(host_timezone);
    let now = context
        .now_ms()
        .map_err(|error| tool_error("tool_execution_failed", error.message.clone()))?;
    if !now.is_finite() || now.abs() > 8.64e15 {
        return Err(tool_error("tool_execution_failed", "Invalid time value"));
    }
    let epoch_ms = now.trunc() as i64;
    let parts = pilotdeck_transport::system_clock::zoned_datetime_parts(epoch_ms, &timezone)
        .map_err(|_| {
            tool_error(
                "tool_execution_failed",
                format!("Invalid timezone: {timezone}"),
            )
        })?;
    let iso = pilotdeck_transport::system_clock::format_epoch_iso(epoch_ms);
    let local = format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
        format_year(parts.year),
        parts.month,
        parts.day,
        parts.hour,
        parts.minute,
        parts.second,
        format_offset(parts.offset_seconds)
    );
    let date = format!(
        "{}-{:02}-{:02}",
        format_year(parts.year),
        parts.month,
        parts.day
    );
    let days = days_from_civil(parts.year, i64::from(parts.month), i64::from(parts.day));
    let weekday = WEEKDAYS[((days + 4).rem_euclid(7)) as usize];
    let output = object([
        ("timezone", string(timezone)),
        ("iso", string(iso)),
        ("local", string(local)),
        ("date", string(date)),
        ("weekday", string(weekday)),
        ("unixMs", JsValue::Number(epoch_ms as f64)),
    ]);
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("json")),
            ("value", JsValue::Object(output.clone())),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(output)),
        metadata: None,
    })
}

fn format_year(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    }
}

fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", seconds / 3_600, seconds % 3_600 / 60)
}

/// Days since 1970-01-01 (Howard Hinnant), proleptic Gregorian;
/// **1-based** month. Shared with the Retry-After date parser.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    era * 146_097 + y * 365 + y / 4 - y / 100 + doy - 719_468
}

fn host_timezone() -> String {
    if let Ok(timezone) = std::env::var("TZ") {
        let timezone = timezone.strip_prefix(':').unwrap_or(&timezone).to_owned();
        if !timezone.is_empty()
            && pilotdeck_transport::system_clock::zoned_datetime_parts(0, &timezone).is_ok()
        {
            return timezone;
        }
    }
    for path in ["/etc/localtime", "/usr/local/etc/localtime"] {
        if let Ok(target) = std::fs::read_link(path) {
            let target = target.to_string_lossy();
            if let Some(index) = target.find("zoneinfo/") {
                return target[index + "zoneinfo/".len()..].to_owned();
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string("/etc/timezone") {
        let timezone = contents.trim();
        if !timezone.is_empty() {
            return timezone.to_owned();
        }
    }
    "UTC".into()
}
