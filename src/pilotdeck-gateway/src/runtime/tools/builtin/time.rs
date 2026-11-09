//! `get_current_time` (baseline `tool/builtin/getCurrentTime.ts`). Formatting
//! uses the same checked system TZif profile as the gateway's date parser via
//! `pilotdeck-transport`; no new timezone crate is linked into the gateway.
//! Zone resolution goes through the #2314 process-lifetime
//! [`CronZoneResolverCache`] (same folding + lowercase key as the cron lane,
//! one TZif parse per zone instead of one per call — `A8.3-CRON-TZ-ALIAS-001`
//! registers this tool as the same pathway).
use std::sync::{Arc, LazyLock};

use super::{object, string, text, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        cron::CronZoneResolverCache,
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
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

/// #2314: the tool lane's zone cache. Tool invocations carry only a
/// [`ToolRunContext`] — no handle on any `CronManager`, whose per-instance
/// cache they cannot outlive by construction — so the holder is a
/// process-lifetime static, matching the TS baseline (V8/ICU pins zone data
/// for the process lifetime; see the #2314 entry-3 arbitration note). Worst
/// case it and a cron manager each parse one shared zone once (bounded).
static TOOL_ZONE_CACHE: LazyLock<Arc<CronZoneResolverCache>> =
    LazyLock::new(|| Arc::new(CronZoneResolverCache::new()));

pub struct GetCurrentTimeTool {
    fields: JsObject,
    zone_cache: Arc<CronZoneResolverCache>,
}

impl GetCurrentTimeTool {
    pub fn new() -> Self {
        Self::with_zone_cache(TOOL_ZONE_CACHE.clone())
    }

    /// Production instances share the process-lifetime static; tests inject a
    /// fresh cache to observe `parse_count`.
    pub fn with_zone_cache(zone_cache: Arc<CronZoneResolverCache>) -> Self {
        Self {
            fields: definition(),
            zone_cache,
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
        let zone_cache = Arc::clone(&self.zone_cache);
        Box::pin(async move { execute(&input, &context, &zone_cache) })
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

fn execute(
    input: &JsValue,
    context: &ToolRunContext,
    zone_cache: &CronZoneResolverCache,
) -> Result<ToolOutput, ToolError> {
    let timezone = input
        .get("timezone")
        .and_then(JsValue::as_string)
        .map(text)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| host_timezone(zone_cache));
    let now = context
        .now_ms()
        .map_err(|error| tool_error("tool_execution_failed", error.message.clone()))?;
    if !now.is_finite() || now.abs() > 8.64e15 {
        return Err(tool_error("tool_execution_failed", "Invalid time value"));
    }
    let epoch_ms = now.trunc() as i64;
    // #2314: same cache, folding, and lowercase key as the cron lane — one
    // TZif parse per zone per process instead of one per call. A failure is
    // not memoized (`A8.3-CRON-TZ-ALIAS-001`'s per-call re-probe contract);
    // the parse runs outside the cache lock.
    let parts = zone_cache
        .resolver(&timezone)
        .and_then(|resolver| resolver.parts_at(epoch_ms))
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
    let weekday = WEEKDAYS[parts.weekday as usize];
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

/// Per-call reads of `TZ` and the `/etc/*` fallbacks are intentional, not a
/// #2314 named-zone cache miss: like `ZonedPartsResolver::resolve_local`,
/// there is no immutable key to invalidate on (`TZ` is mutable in-process
/// and `/etc/localtime` is repointable), so every call re-reads the host.
/// Only the *zone parsing* goes through the cache.
fn host_timezone(zone_cache: &CronZoneResolverCache) -> String {
    if let Ok(timezone) = std::env::var("TZ") {
        let timezone = timezone.strip_prefix(':').unwrap_or(&timezone).to_owned();
        // Same admission check as before the #2314 cache: parse plus one
        // parts read (the cached resolver's `parts_at(0)`), never a re-parse.
        if !timezone.is_empty()
            && zone_cache
                .resolver(&timezone)
                .and_then(|resolver| resolver.parts_at(0))
                .is_ok()
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

#[cfg(test)]
mod zone_cache_tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use pilotdeck_protocol::JsString;

    const FIXED_NOW: f64 = 1_767_225_600_000.0;

    fn input(timezone: &str) -> JsValue {
        JsValue::Object(object([("timezone", string(timezone))]))
    }

    fn fixed_context() -> ToolRunContext {
        let host = ToolRuntimeContext {
            fields: JsObject::new(),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: Some(Arc::new(|| JsValue::Number(FIXED_NOW))),
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        };
        ToolRunContext::new(Arc::new(host))
    }

    fn futures_block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("#2314 tool zone cache: test runtime")
            .block_on(future)
    }

    fn run_tool(tool: &GetCurrentTimeTool, timezone: &str) -> Result<ToolOutput, ToolError> {
        futures_block(tool.execute(input(timezone), fixed_context()))
    }

    /// #2314: the tool routes through the `CronZoneResolverCache` — a second
    /// call for the same zone must not re-open and re-parse the TZif file,
    /// and must compute the same output (`A8.3-CRON-TZ-ALIAS-001` registers
    /// this tool as the same pathway the #2181 cron fix covered).
    #[test]
    fn tool_zone_cache_parses_each_zone_once() {
        let cache = Arc::new(CronZoneResolverCache::new());
        let tool = GetCurrentTimeTool::with_zone_cache(Arc::clone(&cache));
        let first = run_tool(&tool, "Asia/Tokyo");
        let second = run_tool(&tool, "Asia/Tokyo");
        let first = first.expect("#2314 tool zone reuse: first call must succeed");
        let second = second.expect("#2314 tool zone reuse: second call must succeed");
        let timezone_of = |output: &ToolOutput| {
            output
                .data
                .as_ref()
                .and_then(JsValue::as_object)
                .expect("#2314 tool zone reuse: output must carry a data object")
                .get("timezone")
                .and_then(JsValue::as_string)
                .expect("#2314 tool zone reuse: output must carry its timezone")
                .to_utf8()
                .expect("#2314 tool zone reuse: timezone must be utf-8")
        };
        assert_eq!(
            timezone_of(&first),
            "Asia/Tokyo",
            "#2314 tool zone reuse: the echoed timezone must be the input one"
        );
        assert_eq!(
            timezone_of(&first),
            timezone_of(&second),
            "#2314 tool zone reuse: both calls must compute the same zone output"
        );
        assert_eq!(
            cache.parse_count(),
            1,
            "#2314 tool zone reuse: a second call for the same zone must not re-parse the TZif file"
        );
    }

    /// #2314: the tool lane's cache key is the folded zone, like the cron
    /// lane — case variants of one zone share the memoized parse instead of
    /// each becoming its own permanent map entry.
    #[test]
    fn tool_zone_cache_folds_case_variants_into_one_entry() {
        let cache = Arc::new(CronZoneResolverCache::new());
        let tool = GetCurrentTimeTool::with_zone_cache(Arc::clone(&cache));
        let canonical = run_tool(&tool, "Asia/Tokyo")
            .expect("#2314 tool case fold: the canonical spelling must resolve");
        let variant = run_tool(&tool, "ASIA/TOKYO").expect(
            "#2314 tool case fold: the case variant must resolve like the canonical spelling",
        );
        fn local_of(output: &ToolOutput) -> Option<&JsValue> {
            output
                .data
                .as_ref()
                .and_then(JsValue::as_object)
                .and_then(|data| data.get("local"))
        }
        assert_eq!(
            local_of(&canonical),
            local_of(&variant),
            "#2314 tool case fold: the variant must compute the same local time"
        );
        assert_eq!(
            cache.parse_count(),
            1,
            "#2314 tool case fold: a case variant must reuse the folded entry, not re-parse or hold its own"
        );
    }

    /// #2314: the tool lane keeps `A8.3-CRON-TZ-ALIAS-001`'s failure-side
    /// contract — a zone this host tree lacks is not memoized as a failure,
    /// every call re-probes (an environment that recovers must resolve on
    /// the next call) and still reports the tool's invalid-timezone error.
    #[test]
    fn tool_zone_cache_reprobes_unresolved_zones() {
        let cache = Arc::new(CronZoneResolverCache::new());
        let tool = GetCurrentTimeTool::with_zone_cache(Arc::clone(&cache));
        for label in ["first call", "second call"] {
            let error = run_tool(&tool, "Not/AZone")
                .err()
                .unwrap_or_else(|| panic!("#2314 tool failure re-probe: {label} must fail"));
            assert_eq!(
                error.message,
                JsString::from("Invalid timezone: Not/AZone"),
                "#2314 tool failure re-probe: {label} must carry the tool's error"
            );
        }
        assert_eq!(
            cache.parse_count(),
            2,
            "#2314 tool failure re-probe: a failed zone must be re-probed, never memoized as a permanent failure"
        );
    }

    /// #2314 review r2 boundary#1 (P0): a model-supplied offset spelling
    /// whose bytes have no char boundary at 2 (`+中a` / `+€a` / `+😀` — all
    /// four-byte bodies) once panicked the admission parser's byte slicing;
    /// the tool lane must report its invalid-timezone error instead.
    #[test]
    fn tool_rejects_multibyte_offset_spellings_without_panicking() {
        let cache = Arc::new(CronZoneResolverCache::new());
        let tool = GetCurrentTimeTool::with_zone_cache(Arc::clone(&cache));
        for label in ["+中a", "+€a", "+😀"] {
            let error = run_tool(&tool, label).err().unwrap_or_else(|| {
                panic!("#2314 tool multibyte offset: {label} must fail admission")
            });
            assert_eq!(
                error.message,
                JsString::from(format!("Invalid timezone: {label}")),
                "#2314 tool multibyte offset: the error must carry the offending spelling"
            );
        }
    }

    /// #2314 review r2 boundary#2 (P2): the empty-string `timezone` input
    /// falls back to `host_timezone`, which admits the host zone through the
    /// same cache — a path the other tests never touch (they all pass an
    /// explicit timezone). Missing and non-string inputs are not exercised
    /// here: `execute` (`time.rs` input projection) funnels them into the
    /// same `unwrap_or_else(host_timezone)` branch, so the empty-string form
    /// is the coverage-equivalent representative. Two calls must both
    /// succeed, echo a non-empty host zone, and the second call must not
    /// re-parse the fallback zone.
    #[test]
    fn tool_empty_timezone_falls_back_to_the_host_zone_through_the_cache() {
        let cache = Arc::new(CronZoneResolverCache::new());
        let tool = GetCurrentTimeTool::with_zone_cache(Arc::clone(&cache));
        let timezone_of = |output: &ToolOutput| {
            output
                .data
                .as_ref()
                .and_then(JsValue::as_object)
                .expect("#2314 tool empty zone: output must carry a data object")
                .get("timezone")
                .and_then(JsValue::as_string)
                .expect("#2314 tool empty zone: output must carry its timezone")
                .to_utf8()
                .expect("#2314 tool empty zone: timezone must be utf-8")
        };
        let first = run_tool(&tool, "")
            .expect("#2314 tool empty zone: the first call must resolve the host zone");
        let after_first = cache.parse_count();
        let second = run_tool(&tool, "")
            .expect("#2314 tool empty zone: the second call must resolve the host zone");
        let first_zone = timezone_of(&first);
        assert_ne!(
            first_zone, "",
            "#2314 tool empty zone: the host zone echo must be non-empty"
        );
        assert_eq!(
            first_zone,
            timezone_of(&second),
            "#2314 tool empty zone: both calls must agree on the host zone"
        );
        // Host-robust form (review r3/r4, both families): `host_timezone`
        // admits `TZ` on every call and failed admissions are never
        // memoized (#2314 contract, `schedule.rs` `parses += 1` on every
        // probe), so a set-but-unparseable `TZ` (POSIX spellings like
        // `CST-8`) adds exactly one parse to the second call. Anything
        // beyond that delta means the fallback zone itself was re-parsed
        // instead of hitting the cache.
        assert!(
            cache.parse_count() - after_first <= 1,
            "#2314 tool empty zone: the second call must not re-parse the \
             fallback zone (at most one extra TZ re-probe: {} after {})",
            cache.parse_count(),
            after_first
        );
    }
}
