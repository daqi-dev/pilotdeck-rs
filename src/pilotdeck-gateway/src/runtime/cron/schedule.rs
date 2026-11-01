//! #1324 A8.3-pr1b: cron schedule computation, ported from the TS baseline
//! `src/cron/runtime/CronSchedule.ts` (plus `src/cron/CronTimezone.ts`).
//! Pure computation only — nothing here is wired into the gateway process
//! (#1305 splits the dispatch; wiring is #1325).
//!
//! Timezone data profile (decision card
//! `state/decisions-20261102-issue1324-cron-tz.md`): *validity* follows the
//! recorded ICU 78.2 zone list shared with config validation
//! (`config/sections.rs::valid_timezone`), *rules* come from the checked
//! system-TZif resolver in `pilotdeck-transport`, same profile as the shipped
//! `get_current_time` tool. An ICU-valid legacy alias zone that the host
//! zoneinfo tree lacks resolves fail-closed to `None`; that divergence is
//! registered in `specs/gateway-parity/diffs/`.
use std::fmt;

use pilotdeck_protocol::JsString;
use pilotdeck_transport::system_clock::{ZonedPartsResolver, parse_node22_date};

const MINUTE_MS: f64 = 60_000.0;
const DAY_MS: f64 = 86_400_000.0;
const TIME_CLIP: f64 = 8_640_000_000_000_000.0;
const MAX_SEARCH_MINUTES: u32 = 366 * 24 * 60;

/// `CRON_SCHEDULE_COMPUTATION_VERSION` (CronSchedule.ts:4).
pub const CRON_SCHEDULE_COMPUTATION_VERSION: u8 = 3;

/// `DelayUnit` of the `delay` schedule (protocol/types.ts:19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelayUnit {
    Second,
    Minute,
    Hour,
    Day,
}
impl DelayUnit {
    /// `DELAY_UNIT_MS` (CronSchedule.ts:13).
    fn milliseconds(self) -> f64 {
        match self {
            Self::Second => 1_000.0,
            Self::Minute => MINUTE_MS,
            Self::Hour => 60.0 * MINUTE_MS,
            Self::Day => 24.0 * 60.0 * MINUTE_MS,
        }
    }
}

/// `CronCreateSchedule` (protocol/types.ts:14): the create-only union that
/// `computeNextRunAt` accepts. `timezone` is `?? fallback` (not defaulted
/// here), mirroring `schedule.timezone ?? fallbackTimezone`.
#[derive(Clone, Debug)]
pub enum CronCreateSchedule {
    Once {
        run_at: JsString,
    },
    Delay {
        amount: f64,
        unit: DelayUnit,
    },
    Cron {
        expression: JsString,
        timezone: Option<JsString>,
    },
}

/// The `Date | undefined` return of TS `computeNextRunAt`. `Invalid` is an
/// Invalid Date (`new Date(x)` beyond TimeClip / NaN on the delay path): TS
/// returns it, and the first `toISOString()` on it throws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ComputedRunAt {
    Undefined,
    Valid(f64),
    Invalid,
}

/// Node's `RangeError: Invalid time value`: `matchesCron` formats a
/// candidate `Date` with `Intl.DateTimeFormat.formatToParts`, which throws on
/// an Invalid Date (reached when the aligned scan start leaves the Date
/// range, CronSchedule.ts:57 via formatToParts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidTimeValue;
impl fmt::Display for InvalidTimeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid time value")
    }
}
impl std::error::Error for InvalidTimeValue {}

/// `cronDayFieldsUseOr` (CronSchedule.ts:7): whether the day-field OR rule
/// (version 3) applies to this expression. Unparsable expressions are `?? false`.
pub fn cron_day_fields_use_or(expression: &JsString) -> bool {
    parse_cron_expression(expression).is_some_and(|parsed| parsed.day_fields_use_or)
}

/// `delayToMilliseconds` (CronSchedule.ts:36): non-finite and non-positive
/// amounts have no delay.
pub fn delay_to_milliseconds(amount: f64, unit: DelayUnit) -> Option<f64> {
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    Some(amount * unit.milliseconds())
}

/// `computeNextRunAt` (CronSchedule.ts:20). `after_epoch_ms` is the `after`
/// Date's `getTime()` (a finite Date value or NaN). The `once` path parses
/// with the Node v22.22.0 V8 date parser port; a local-wall `runAt` whose
/// process-TZ resolution is unavailable on this host fails closed to
/// `Undefined` (TS never fails there — it has ICU-local built in).
pub fn compute_next_run_at(
    schedule: &CronCreateSchedule,
    after_epoch_ms: f64,
    fallback_timezone: &str,
) -> Result<ComputedRunAt, InvalidTimeValue> {
    match schedule {
        CronCreateSchedule::Once { run_at } => Ok(match parse_node22_date(run_at) {
            Ok(Some(epoch_ms)) => ComputedRunAt::Valid(epoch_ms),
            Ok(None) | Err(_) => ComputedRunAt::Undefined,
        }),
        CronCreateSchedule::Delay { amount, unit } => {
            Ok(match delay_to_milliseconds(*amount, *unit) {
                Some(delay_ms) => js_new_date(after_epoch_ms + delay_ms),
                None => ComputedRunAt::Undefined,
            })
        }
        CronCreateSchedule::Cron {
            expression,
            timezone,
        } => {
            // `schedule.timezone ?? fallbackTimezone`: an explicit (even
            // empty) timezone always wins over the fallback; an explicit
            // timezone with unpaired surrogates is not in the valid list,
            // so it fails closed to `undefined` like TS.
            let resolved = timezone.as_ref().and_then(|tz| tz.to_utf8().ok());
            if resolved.is_none() && timezone.is_some() {
                return Ok(ComputedRunAt::Undefined);
            }
            let timezone = resolved.as_deref().unwrap_or(fallback_timezone);
            compute_next_cron_run_at(expression, after_epoch_ms, timezone)
                .map(|next| next.map_or(ComputedRunAt::Undefined, ComputedRunAt::Valid))
        }
    }
}

/// `computeNextCronRunAt` (CronSchedule.ts:44). `Err(InvalidTimeValue)` is
/// the TS `RangeError` from formatting an Invalid Date candidate.
pub fn compute_next_cron_run_at(
    expression: &JsString,
    after_epoch_ms: f64,
    timezone: &str,
) -> Result<Option<f64>, InvalidTimeValue> {
    let Some(parsed) = parse_cron_expression(expression) else {
        return Ok(None);
    };
    if !is_valid_cron_timezone(timezone) {
        return Ok(None);
    }
    // Valid per the recorded ICU list but unresolvable through the host
    // zoneinfo tree (legacy alias zones on split tzdata installs): fail
    // closed rather than silently computing in UTC.
    let Ok(resolver) = ZonedPartsResolver::new(timezone) else {
        return Ok(None);
    };
    let candidate = (after_epoch_ms / MINUTE_MS).floor() * MINUTE_MS + MINUTE_MS;
    if is_leap_day_only_schedule(&parsed) {
        return Ok(compute_next_leap_day_run_at(candidate, &parsed, &resolver));
    }
    let mut candidate = candidate;
    for _ in 0..MAX_SEARCH_MINUTES {
        // `new Date(candidate)` is Invalid exactly when the scan left the
        // Date range; `matchesCron` then throws in formatToParts.
        if candidate.is_nan() || candidate.abs() > TIME_CLIP {
            return Err(InvalidTimeValue);
        }
        if matches_cron(candidate, &parsed, &resolver) {
            return Ok(Some(candidate));
        }
        candidate += MINUTE_MS;
    }
    Ok(None)
}

/// `isValidCronTimezone` (CronTimezone.ts:1): the recorded ICU 78.2 zone
/// list plus UTC-offset identifiers, the same acceptance as Node 22's
/// `Intl.DateTimeFormat` constructor (shared with config validation).
pub fn is_valid_cron_timezone(timezone: &str) -> bool {
    crate::config::sections::valid_timezone(timezone)
}

/// `resolveCronTimezone` (CronTimezone.ts:10): the first non-empty *valid*
/// timezone of schedule/task/config, else `"UTC"`.
pub fn resolve_cron_timezone(
    schedule_timezone: Option<&str>,
    task_timezone: Option<&str>,
    config_timezone: Option<&str>,
) -> String {
    for timezone in [schedule_timezone, task_timezone, config_timezone] {
        if let Some(timezone) = timezone
            && !timezone.is_empty()
            && is_valid_cron_timezone(timezone)
        {
            return timezone.to_owned();
        }
    }
    "UTC".to_owned()
}

/// `new Date(x)`: an Invalid Date exactly when `x` is NaN or beyond TimeClip;
/// inside the range TimeClip is `ToIntegerOrInfinity`, so fractional
/// milliseconds truncate toward zero (verified on Node v22.22.0).
fn js_new_date(epoch_ms: f64) -> ComputedRunAt {
    if epoch_ms.is_nan() || epoch_ms.abs() > TIME_CLIP {
        ComputedRunAt::Invalid
    } else {
        ComputedRunAt::Valid(epoch_ms.trunc())
    }
}

/// One parsed five-field expression (CronSchedule.ts:98). Every field is a
/// bitmask over its range (`minutes` bit 0..59, `months` bit 1..12, ...).
struct ParsedCron {
    minutes: u64,
    hours: u64,
    days_of_month: u64,
    months: u64,
    days_of_week: u64,
    day_fields_use_or: bool,
}

/// `isLeapDayOnlySchedule` (CronSchedule.ts:65): the `0 0 29 2 *` family that
/// skips the minute scan for the leap-day shortcut. `daysOfWeek.size === 7`
/// after the 7→0 normalization, so the mask is the full Sunday..Saturday set.
fn is_leap_day_only_schedule(cron: &ParsedCron) -> bool {
    !cron.day_fields_use_or
        && cron.days_of_month.count_ones() == 1
        && cron.days_of_month & (1 << 29) != 0
        && cron.months.count_ones() == 1
        && cron.months & (1 << 2) != 0
        && cron.days_of_week == 0b0111_1111
}

/// `computeNextLeapDayRunAt` (CronSchedule.ts:74). `Date.UTC` returns NaN
/// outside TimeClip, which makes the `candidate < end` guard false — the
/// same NaN-comparison semantics as JS, so f64 arithmetic reproduces it.
fn compute_next_leap_day_run_at(
    after_ms: f64,
    cron: &ParsedCron,
    resolver: &ZonedPartsResolver,
) -> Option<f64> {
    // `after.getUTCFullYear()` is NaN on an Invalid Date: the year loop
    // (`year <= startYear + 8`) never runs.
    if after_ms.is_nan() || after_ms.abs() > TIME_CLIP {
        return None;
    }
    let start_year = civil_from_days((after_ms / DAY_MS).floor() as i64).0;
    for year_offset in 0..=8 {
        let year = start_year + year_offset;
        if !is_leap_year(year) {
            continue;
        }
        let mut candidate = date_utc(year, 2, 28);
        let end = date_utc(year, 3, 2);
        while candidate < end {
            if candidate >= after_ms && matches_cron(candidate, cron, resolver) {
                return Some(candidate);
            }
            candidate += MINUTE_MS;
        }
    }
    None
}

/// `isLeapYear` (CronSchedule.ts:94).
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// `Date.UTC(year, month, day)` for the leap-day window: TimeClip'd, so a
/// time beyond the Date range is NaN.
fn date_utc(year: i64, month: i64, day: i64) -> f64 {
    let days = days_from_civil(year, month, day);
    let epoch_ms = days as f64 * DAY_MS;
    if epoch_ms.abs() > TIME_CLIP {
        f64::NAN
    } else {
        epoch_ms
    }
}

/// `matchesCron` (CronSchedule.ts:226) over the resolver's local parts.
/// `readCronDateParts` returning undefined maps to `false`.
fn matches_cron(epoch_ms: f64, cron: &ParsedCron, resolver: &ZonedPartsResolver) -> bool {
    let Ok(parts) = resolver.parts_at(epoch_ms as i64) else {
        return false;
    };
    let day_of_month_matches = cron.days_of_month & (1 << parts.day) != 0;
    let day_of_week_matches = cron.days_of_week & (1 << parts.weekday) != 0;
    let day_matches = if cron.day_fields_use_or {
        day_of_month_matches || day_of_week_matches
    } else {
        day_of_month_matches && day_of_week_matches
    };
    cron.minutes & (1 << parts.minute) != 0
        && cron.hours & (1 << parts.hour) != 0
        && cron.months & (1 << parts.month) != 0
        && day_matches
}

/// `parseCronExpression` (CronSchedule.ts:107): five whitespace-separated
/// fields; day fields keep Unix AND semantics unless neither starts with `*`.
fn parse_cron_expression(expression: &JsString) -> Option<ParsedCron> {
    let parts = js_trim_split(expression.units());
    if parts.len() != 5 {
        return None;
    }
    let Ok(fields): Result<[&[u16]; 5], _> = parts.try_into() else {
        return None;
    };
    let [minute, hour, day_of_month, month, day_of_week] = fields;
    let minutes = parse_field(minute, 0, 59)?;
    let hours = parse_field(hour, 0, 23)?;
    let days_of_month = parse_field(day_of_month, 1, 31)?;
    let months = parse_field(month, 1, 12)?;
    let days_of_week = parse_field(day_of_week, 0, 7)?;
    // `if (parsed.daysOfWeek.has(7)) { add(0); delete(7); }`
    let days_of_week = if days_of_week & (1 << 7) != 0 {
        (days_of_week | 1) & !(1 << 7)
    } else {
        days_of_week
    };
    Some(ParsedCron {
        minutes,
        hours,
        days_of_month,
        months,
        days_of_week,
        // Unix cron keeps AND semantics when either day field starts with
        // `*`, including `*/n` (CronSchedule.ts:119).
        day_fields_use_or: !starts_with_star(day_of_month) && !starts_with_star(day_of_week),
    })
}

/// `field.startsWith("*")` on the raw field text.
fn starts_with_star(field: &[u16]) -> bool {
    field.first() == Some(&u16::from(b'*'))
}

/// `parseField` (CronSchedule.ts:138): comma-separated `a`, `a-b`, `*`,
/// with `/step`; values are ECMAScript `Number.parseInt(s, 10)` (leading
/// whitespace, one sign, leading digits, trailing junk ignored — so `5x`
/// is 5 and `1.5` as a step is 1), and every part's range must fit
/// `min..=max` with `start <= end`.
fn parse_field(field: &[u16], min: u32, max: u32) -> Option<u64> {
    let mut mask = 0u64;
    for part in field.split(|&unit| unit == u16::from(b',')) {
        let part = js_trim(part);
        if part.is_empty() {
            return None;
        }
        let mut pieces = part.split(|&unit| unit == u16::from(b'/'));
        let base = pieces.next()?;
        let step_units = pieces.next();
        if pieces.next().is_some() {
            return None;
        }
        let step = match step_units {
            None => 1.0,
            Some(units) => parse_int_radix10(units)?,
        };
        if step <= 0.0 {
            return None;
        }
        let (start, end) = if base == [u16::from(b'*')] {
            (f64::from(min), f64::from(max))
        } else if base.contains(&u16::from(b'-')) {
            // `"1-2-3".split("-")` destructures only the first two pieces;
            // the third is dropped, not rejected.
            let mut pieces = base.splitn(3, |&unit| unit == u16::from(b'-'));
            let raw_start = pieces.next()?;
            let raw_end = pieces.next().unwrap_or(&[]);
            (parse_int_radix10(raw_start)?, parse_int_radix10(raw_end)?)
        } else {
            let value = parse_int_radix10(base)?;
            (value, value)
        };
        if start < f64::from(min) || end > f64::from(max) || start > end {
            return None;
        }
        // start/end are validated integral values inside min..=max.
        let mut value = start as i64;
        let top = end as i64;
        let step = step as i64;
        while value <= top {
            mask |= 1 << value;
            value = value.saturating_add(step);
        }
    }
    Some(mask)
}

/// ECMAScript `Number.parseInt(s, 10)`: leading/trailing whitespace is
/// trimmed, one optional sign, then leading decimal digits; anything else
/// (including no digits at all) is NaN.
fn parse_int_radix10(units: &[u16]) -> Option<f64> {
    let mut rest = js_trim(units);
    let mut negative = false;
    if let Some((&first, tail)) = rest.split_first() {
        if first == u16::from(b'+') {
            rest = tail;
        } else if first == u16::from(b'-') {
            negative = true;
            rest = tail;
        }
    }
    let mut value = 0.0;
    let mut digits = 0;
    for &unit in rest {
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            break;
        }
        value = value * 10.0 + f64::from(unit - u16::from(b'0'));
        digits += 1;
    }
    if digits == 0 {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// JS whitespace (WhiteSpace ∪ LineTerminator): the set both `String
///.prototype.trim` and `/\s+/` use on UTF-16 units.
fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f
            | 0x3000 | 0xfeff
    )
}

fn js_trim(units: &[u16]) -> &[u16] {
    let mut start = 0;
    let mut end = units.len();
    while start < end && is_js_whitespace(units[start]) {
        start += 1;
    }
    while end > start && is_js_whitespace(units[end - 1]) {
        end -= 1;
    }
    &units[start..end]
}

/// `expression.trim().split(/\s+/)` on UTF-16 units: split into runs of JS
/// whitespace (an all-whitespace string yields one empty field, which the
/// `!== 5` check rejects — here that shows up as zero parts).
fn js_trim_split(units: &[u16]) -> Vec<&[u16]> {
    let units = js_trim(units);
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    for (index, &unit) in units.iter().enumerate() {
        if is_js_whitespace(unit) {
            if let Some(begin) = start.take() {
                parts.push(&units[begin..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        parts.push(&units[begin..]);
    }
    parts
}

/// Days since 1970-01-01 (Howard Hinnant), proleptic Gregorian.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    era * 146_097 + y * 365 + y / 4 - y / 100 + doy - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    (y, m, d)
}
