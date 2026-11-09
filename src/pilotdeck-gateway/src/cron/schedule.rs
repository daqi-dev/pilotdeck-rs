//! Fixed-baseline CronSchedule.ts: five fields, syntax-sensitive day matching.
use super::tasks::{CronError, CronResult};
use pilotdeck_transport::system_clock::{format_epoch_iso, parse_node22_date};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const SCHEDULE_COMPUTATION_VERSION: u64 = 3;
const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;
const TIME_CLIP: i64 = 8_640_000_000_000_000;

pub fn iso(ms: i64) -> String {
    format_epoch_iso(ms)
}

pub fn parse_date(value: &str) -> CronResult<Option<i64>> {
    parse_node22_date(&value.into())
        .map(|value| value.map(|ms| ms as i64))
        .map_err(|error| {
            CronError::new(
                "cron_timezone_unavailable",
                String::from_utf16_lossy(error.units()),
            )
        })
}

pub fn valid_timezone(value: &str) -> bool {
    static ZONES: OnceLock<Vec<String>> = OnceLock::new();
    let zones = ZONES.get_or_init(|| {
        serde_json::from_str(include_str!("../config/timezones.json"))
            .expect("recorded baseline ICU timezone names")
    });
    (value.is_ascii() && zones.binary_search(&value.to_ascii_lowercase()).is_ok())
        || offset_timezone(value).is_some()
}

pub fn resolve_timezone(
    schedule: Option<&str>,
    task: Option<&str>,
    config: Option<&str>,
) -> String {
    [schedule, task, config, Some("UTC")]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty() && valid_timezone(value))
        .unwrap_or("UTC")
        .to_owned()
}

pub fn normalize_schedule(
    schedule: &Value,
    top_timezone: Option<&Value>,
    default_timezone: &str,
    now_ms: i64,
    allow_delay: bool,
) -> CronResult<Value> {
    match schedule.get("type").and_then(Value::as_str) {
        Some("once") => {
            let run_at = string(schedule, "runAt")?;
            Ok(json!({"type":"once","runAt":run_at}))
        }
        Some("delay") if allow_delay => {
            let next =
                compute_next_run_at(schedule, now_ms, default_timezone)?.ok_or_else(|| {
                    validation("Cron delay schedule must use a positive finite amount.")
                })?;
            Ok(json!({"type":"once","runAt":iso(next)}))
        }
        Some("cron") => {
            let expression = string(schedule, "expression")?;
            let requested = schedule
                .get("timezone")
                .filter(|v| !v.is_null())
                .or(top_timezone.filter(|v| !v.is_null()));
            if let Some(value) = requested
                && truthy(value)
                && !value.as_str().is_some_and(valid_timezone)
            {
                return Err(validation(format!(
                    "Invalid Cron timezone: {}",
                    display(value)
                )));
            }
            let timezone = resolve_timezone(
                requested.and_then(Value::as_str),
                None,
                Some(default_timezone),
            );
            Ok(json!({"type":"cron","expression":expression,"timezone":timezone}))
        }
        _ => Err(validation("Cron schedule is invalid.")),
    }
}

pub fn compute_next_run_at(
    schedule: &Value,
    after_ms: i64,
    fallback_timezone: &str,
) -> CronResult<Option<i64>> {
    match schedule.get("type").and_then(Value::as_str) {
        Some("once") => parse_date(string(schedule, "runAt")?),
        Some("delay") => {
            let amount = schedule
                .get("amount")
                .and_then(Value::as_f64)
                .unwrap_or(f64::NAN);
            let unit = match schedule.get("unit").and_then(Value::as_str) {
                Some("second") => 1_000.0,
                Some("minute") => 60_000.0,
                Some("hour") => 3_600_000.0,
                Some("day") => 86_400_000.0,
                _ => return Ok(None),
            };
            if !amount.is_finite() || amount <= 0.0 {
                return Ok(None);
            }
            let result = after_ms as f64 + amount * unit;
            if !result.is_finite() || result.abs() > TIME_CLIP as f64 {
                return Err(validation("Invalid time value"));
            }
            Ok(Some(result.trunc() as i64))
        }
        Some("cron") => compute_next_cron_run_at(
            string(schedule, "expression")?,
            after_ms,
            schedule
                .get("timezone")
                .and_then(Value::as_str)
                .unwrap_or(fallback_timezone),
        ),
        _ => Err(validation("Cron schedule is invalid.")),
    }
}

pub fn cron_day_fields_use_or(expression: &str) -> bool {
    Parsed::parse(expression).is_some_and(|parsed| parsed.day_or)
}

pub fn compute_next_cron_run_at(
    expression: &str,
    after_ms: i64,
    timezone: &str,
) -> CronResult<Option<i64>> {
    if !(-TIME_CLIP..=TIME_CLIP).contains(&after_ms) {
        return Err(validation("Invalid time value"));
    }
    let Some(parsed) = Parsed::parse(expression) else {
        return Ok(None);
    };
    if !valid_timezone(timezone) {
        return Ok(None);
    }
    let zone = zone(timezone)?;
    let candidate = after_ms.div_euclid(MINUTE_MS) * MINUTE_MS + MINUTE_MS;
    if parsed.leap_only() {
        let year = civil_from_days(candidate.div_euclid(DAY_MS)).0;
        for year in year..=year + 8 {
            if year % 4 != 0 || (year % 100 == 0 && year % 400 != 0) {
                continue;
            }
            let mut minute = days_from_civil(year, 2, 28) * DAY_MS;
            let end = days_from_civil(year, 3, 2) * DAY_MS;
            while minute < end {
                if minute >= candidate && matches(minute, &parsed, &zone)? {
                    return Ok(Some(minute));
                }
                minute += MINUTE_MS;
            }
        }
        return Ok(None);
    }
    let mut minute = candidate;
    for _ in 0..366 * 24 * 60 {
        if minute.abs() > TIME_CLIP {
            return Ok(None);
        }
        if matches(minute, &parsed, &zone)? {
            return Ok(Some(minute));
        }
        minute += MINUTE_MS;
    }
    Ok(None)
}

struct Parsed {
    fields: [BTreeSet<i32>; 5],
    day_or: bool,
}
impl Parsed {
    fn parse(expression: &str) -> Option<Self> {
        let parts: Vec<_> = expression.split_whitespace().collect();
        if parts.len() != 5 {
            return None;
        }
        let mut fields = [
            field(parts[0], 0, 59)?,
            field(parts[1], 0, 23)?,
            field(parts[2], 1, 31)?,
            field(parts[3], 1, 12)?,
            field(parts[4], 0, 7)?,
        ];
        if fields[4].remove(&7) {
            fields[4].insert(0);
        }
        Some(Self {
            fields,
            day_or: !parts[2].starts_with('*') && !parts[4].starts_with('*'),
        })
    }
    fn leap_only(&self) -> bool {
        !self.day_or
            && self.fields[2].len() == 1
            && self.fields[2].contains(&29)
            && self.fields[3].len() == 1
            && self.fields[3].contains(&2)
            && self.fields[4].len() == 7
    }
}

fn field(value: &str, min: i32, max: i32) -> Option<BTreeSet<i32>> {
    let mut result = BTreeSet::new();
    for part in value.split(',') {
        let parts: Vec<_> = part.trim().split('/').collect();
        if parts.len() > 2 {
            return None;
        }
        let step = if parts.len() == 1 {
            1.0
        } else {
            parse_int(parts[1])?
        };
        if step <= 0.0 || !step.is_finite() {
            return None;
        }
        let (start, end) = if parts[0] == "*" {
            (f64::from(min), f64::from(max))
        } else if parts[0].contains('-') {
            let mut bounds = parts[0].split('-');
            (parse_int(bounds.next()?)?, parse_int(bounds.next()?)?)
        } else {
            let start = parse_int(parts[0])?;
            (start, start)
        };
        if start < f64::from(min) || end > f64::from(max) || start > end {
            return None;
        }
        let mut next = start;
        while next <= end {
            result.insert(next as i32);
            next += step;
        }
    }
    (!result.is_empty()).then_some(result)
}

fn parse_int(value: &str) -> Option<f64> {
    let value = value.trim_start();
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    let count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if count == 0 {
        return None;
    }
    let sign = value.len() - digits.len();
    value[..sign + count].parse().ok()
}

fn matches(ms: i64, parsed: &Parsed, zone: &tz::TimeZone) -> CronResult<bool> {
    let offset = zone
        .find_local_time_type(ms.div_euclid(1000))
        .map_err(|error| CronError::new("cron_timezone_unavailable", error.to_string()))?
        .ut_offset();
    let local = ms + i64::from(offset) * 1000;
    let days = local.div_euclid(DAY_MS);
    let (_, month, day) = civil_from_days(days);
    let time = local.rem_euclid(DAY_MS);
    let dom = parsed.fields[2].contains(&(day as i32));
    let dow = parsed.fields[4].contains(&((days + 4).rem_euclid(7) as i32));
    Ok(parsed.fields[0].contains(&((time / MINUTE_MS % 60) as i32))
        && parsed.fields[1].contains(&((time / 3_600_000) as i32))
        && parsed.fields[3].contains(&(month as i32))
        && if parsed.day_or {
            dom || dow
        } else {
            dom && dow
        })
}

fn zone(name: &str) -> CronResult<Arc<tz::TimeZone>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<tz::TimeZone>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| CronError::new("cron_timezone_unavailable", "Timezone cache lock poisoned"))?;
    if let Some(zone) = cache.get(name) {
        return Ok(zone.clone());
    }
    let parsed = if let Some(offset) = offset_timezone(name) {
        tz::TimeZone::fixed(offset).map_err(|error| validation(error.to_string()))?
    } else {
        let root = Path::new("/usr/share/zoneinfo");
        let mut path = root.to_path_buf();
        for component in name.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(validation("Invalid Cron timezone"));
            }
            let exact = path.join(component);
            path = if exact.exists() {
                exact
            } else {
                std::fs::read_dir(&path)?
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .eq_ignore_ascii_case(component)
                    })
                    .map(|entry| entry.path())
                    .ok_or_else(|| {
                        CronError::new(
                            "cron_timezone_unavailable",
                            format!("Timezone data unavailable: {name}"),
                        )
                    })?
            };
        }
        let canonical: PathBuf = std::fs::canonicalize(path)?;
        if !canonical.starts_with(std::fs::canonicalize(root)?) {
            return Err(validation("Invalid Cron timezone"));
        }
        tz::TimeZone::from_tz_data(&std::fs::read(canonical)?)
            .map_err(|error| CronError::new("cron_timezone_unavailable", error.to_string()))?
    };
    let parsed = Arc::new(parsed);
    cache.insert(name.to_owned(), parsed.clone());
    Ok(parsed)
}

fn offset_timezone(value: &str) -> Option<i32> {
    if !value.is_ascii() {
        return None;
    }
    let sign = match value.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &value[1..];
    let (hours, minutes) = match body.len() {
        2 => (body, "00"),
        4 => (&body[..2], &body[2..]),
        5 if body.as_bytes()[2] == b':' => (&body[..2], &body[3..]),
        _ => return None,
    };
    if !hours
        .bytes()
        .chain(minutes.bytes())
        .all(|v| v.is_ascii_digit())
    {
        return None;
    }
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(sign * (hours * 3600 + minutes * 60))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    era * 146097 + y * 365 + y / 4 - y / 100 + (153 * m + 2) / 5 + day - 1 - 719468
}
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    (y, m, d)
}
fn string<'a>(value: &'a Value, key: &str) -> CronResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| validation(format!("Cron schedule {key} must be a string.")))
}
pub(super) fn validation(message: impl Into<String>) -> CronError {
    CronError::new("gateway_request_failed", message)
}
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        _ => true,
    }
}
pub(super) fn display(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
#[path = "schedule/tests.rs"]
mod tests;
