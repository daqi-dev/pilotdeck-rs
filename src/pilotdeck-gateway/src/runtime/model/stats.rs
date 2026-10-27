//! Router usage statistics and Gateway usage snapshots.
//!
//! Ported against PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`:
//! - `src/router/stats/TokenStatsCollector.ts` (aggregate, JSONL journal,
//!   retention, migration, cost provenance, snapshots)
//! - `src/cli/ProjectRuntimeRegistry.ts:1310-1373` (`usageSnapshotForSdk`,
//!   `modelUsageSnapshotForSdk` projections)
//!
//! Pricing is not duplicated here: `crate::model_cost` owns the rate table and
//! the exact floating-point composition. Records are kept as plain JS objects
//! so the persisted JSONL text (key order included) matches `JSON.stringify`.
#[path = "stats_journal.rs"]
mod journal;

use super::router::{RouterModelRef, StatsConfig, usage_from_js, wall_clock};
use crate::model_cost::{self, PricingMap, PricingSource};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    cmp::Ordering,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

/// Default cap from `MAX_HOURLY_BUCKETS`.
pub const MAX_HOURLY_BUCKETS: usize = 72;
/// Default cap from `MAX_SESSIONS`.
pub const MAX_SESSIONS: usize = 200;

pub type StatsClock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// `RouterCostSource`; `legacy_unknown` is the default for old records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostSource {
    ProviderReported,
    ConfiguredPrice,
    BuiltInEstimate,
    FallbackEstimate,
    LegacyUnknown,
}
impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::ConfiguredPrice => "configured_price",
            Self::BuiltInEstimate => "built_in_estimate",
            Self::FallbackEstimate => "fallback_estimate",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct RoleAggregate {
    total_requests: f64,
    input_tokens: f64,
    output_tokens: f64,
    cache_read_tokens: f64,
    cache_write_tokens: f64,
    total_tokens: f64,
    total_cost: f64,
    cost_sources: Vec<(String, f64)>,
}
impl RoleAggregate {
    fn to_js(&self) -> JsValue {
        js_object(vec![
            ("totalRequests", number(self.total_requests)),
            ("inputTokens", number(self.input_tokens)),
            ("outputTokens", number(self.output_tokens)),
            ("cacheReadTokens", number(self.cache_read_tokens)),
            ("cacheWriteTokens", number(self.cache_write_tokens)),
            ("totalTokens", number(self.total_tokens)),
            ("totalCost", number(self.total_cost)),
            ("costSources", map_js(&self.cost_sources)),
        ])
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ModelUsage {
    provider: JsString,
    model: JsString,
    totals: RoleAggregate,
    roles: Vec<(String, RoleAggregate)>,
}
impl ModelUsage {
    fn new(provider: JsString, model: JsString) -> Self {
        Self {
            provider,
            model,
            totals: RoleAggregate::default(),
            roles: Vec::new(),
        }
    }
    fn to_js(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert("provider".into(), JsValue::String(self.provider.clone()));
        out.insert("model".into(), JsValue::String(self.model.clone()));
        for (key, value) in self.totals.to_js().as_object().unwrap().entries() {
            out.insert(key.clone(), value.clone());
        }
        let mut roles = JsObject::new();
        for (role, totals) in &self.roles {
            roles.insert(role.as_str().into(), totals.to_js());
        }
        out.insert("roles".into(), JsValue::Object(roles));
        JsValue::Object(out)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Aggregate {
    total_requests: f64,
    total_input_tokens: f64,
    total_output_tokens: f64,
    total_cost: f64,
    total_baseline_cost: f64,
    total_saved_cost: f64,
    per_scenario: Vec<(String, f64)>,
    per_model: Vec<(String, f64)>,
    per_provider: Vec<(String, f64)>,
    per_tier: Vec<(String, f64)>,
    per_role: Vec<(String, f64)>,
    cost_sources: Vec<(String, f64)>,
    per_model_usage: Vec<(String, ModelUsage)>,
}
impl Aggregate {
    fn to_js(&self) -> JsValue {
        js_object(vec![
            ("totalRequests", number(self.total_requests)),
            ("totalInputTokens", number(self.total_input_tokens)),
            ("totalOutputTokens", number(self.total_output_tokens)),
            ("totalCost", number(self.total_cost)),
            ("totalBaselineCost", number(self.total_baseline_cost)),
            ("totalSavedCost", number(self.total_saved_cost)),
            ("perScenario", map_js(&self.per_scenario)),
            ("perModel", map_js(&self.per_model)),
            ("perProvider", map_js(&self.per_provider)),
            ("perTier", map_js(&self.per_tier)),
            ("perRole", map_js(&self.per_role)),
            ("costSources", map_js(&self.cost_sources)),
            ("perModelUsage", model_map_js(&self.per_model_usage)),
        ])
    }
    fn model_usage(&mut self, provider: &JsString, model: &JsString, key: &str) -> &mut ModelUsage {
        if let Some(index) = self
            .per_model_usage
            .iter()
            .position(|(name, _)| name == key)
        {
            return &mut self.per_model_usage[index].1;
        }
        self.per_model_usage.push((
            key.to_owned(),
            ModelUsage::new(provider.clone(), model.clone()),
        ));
        let last = self.per_model_usage.len() - 1;
        &mut self.per_model_usage[last].1
    }
}

#[derive(Clone, Debug)]
struct SessionBucket {
    session_id: JsString,
    aggregate: Aggregate,
    request_log: Vec<JsValue>,
}

struct PersistedData {
    hourly: Vec<(String, Aggregate)>,
    sessions: Vec<(String, SessionBucket)>,
    global: Aggregate,
}
impl PersistedData {
    fn new() -> Self {
        Self {
            hourly: Vec::new(),
            sessions: Vec::new(),
            global: Aggregate::default(),
        }
    }
}

/// Durable Router usage collector. When disabled every mutating method is a
/// no-op except the constructor-side state, matching `enabled ?? false`.
pub struct TokenStatsCollector {
    enabled: bool,
    jsonl_path: Option<PathBuf>,
    model_pricing: Option<PricingMap>,
    baseline_model: Option<RouterModelRef>,
    retention_ms: Option<f64>,
    data: PersistedData,
    recent_records: Vec<JsValue>,
    /// Present only when retention is configured; contains every retained record.
    retained_records: Option<Vec<JsValue>>,
    expired_records_loaded: bool,
    file: Option<File>,
    clock: StatsClock,
}

impl TokenStatsCollector {
    /// `new TokenStatsCollector(config)`; `pilot_home` replaces
    /// `resolvePilotHome()` for callers that already resolved it.
    pub fn new(config: Option<&StatsConfig>, pilot_home: Option<&Path>) -> std::io::Result<Self> {
        Self::with_clock(config, pilot_home, Arc::new(wall_clock))
    }

    pub fn with_clock(
        config: Option<&StatsConfig>,
        pilot_home: Option<&Path>,
        clock: StatsClock,
    ) -> std::io::Result<Self> {
        let enabled = config.map(|config| config.enabled).unwrap_or(false);
        let model_pricing = config.and_then(|config| config.model_pricing.clone());
        let baseline_model = config.and_then(|config| config.baseline_model.clone());
        let retention_ms = config.and_then(|config| config.retention_ms);
        let mut collector = Self {
            enabled,
            jsonl_path: None,
            model_pricing,
            baseline_model,
            retention_ms,
            data: PersistedData::new(),
            recent_records: Vec::new(),
            retained_records: None,
            expired_records_loaded: false,
            file: None,
            clock,
        };
        if !enabled {
            return Ok(collector);
        }
        let router_dir = match config.and_then(|config| config.file_path.as_deref()) {
            Some(file) => file.parent().unwrap_or(Path::new(".")).to_path_buf(),
            None => match pilot_home {
                Some(home) => home.to_path_buf(),
                None => default_pilot_home()?,
            }
            .join("router"),
        };
        let _ = fs::create_dir_all(&router_dir);
        let jsonl = router_dir.join("stats.jsonl");
        collector.jsonl_path = Some(jsonl.clone());
        migrate_json_to_jsonl(&router_dir, &jsonl);
        collector.data = collector.rebuild_from_jsonl();
        if collector.expired_records_loaded {
            collector.compact_retained_records();
        }
        if collector.retention_ms.is_none() {
            collector.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&jsonl)
                .ok();
        }
        Ok(collector)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// #879 stats-cwd fixture: expose the path actually selected by the
    /// constructor so the runtime entry can be observed without replaying a
    /// path computed by the test.
    #[cfg(test)]
    pub(crate) fn journal_path(&self) -> Option<&Path> {
        self.jsonl_path.as_deref()
    }

    pub fn observe(&mut self, record: &mut JsValue) {
        if !self.enabled {
            return;
        }
        self.prune_expired_records();
        let usage = record.get("usage").cloned().unwrap_or(JsValue::Undefined);
        let usage = usage_from_js(&usage).unwrap_or_default();
        let provider = text(record, "provider").unwrap_or_default();
        let model = text(record, "model").unwrap_or_default();
        let (breakdown, source) = self.estimate_cost_breakdown(&usage, &provider, &model);
        if let JsValue::Object(object) = record {
            object.insert(
                "cost".into(),
                js_object(vec![
                    ("input", number(breakdown.input)),
                    ("output", number(breakdown.output)),
                    ("cacheRead", number(breakdown.cache_read)),
                    ("total", number(breakdown.total)),
                ]),
            );
            object.insert("costSource".into(), JsValue::String(source.as_str().into()));
            let baseline = self
                .calculate_baseline_cost(&usage, &provider, &model)
                .unwrap_or(breakdown.total);
            object.insert("baselineCost".into(), number(baseline));
        }

        if let Some(retained) = self.retained_records.as_mut() {
            retained.push(record.clone());
        }
        self.recent_records.push(record.clone());
        if self.recent_records.len() > 500 {
            let cut = self.recent_records.len() - 250;
            self.recent_records.drain(..cut);
        }

        bump_aggregate(&mut self.data.global, record);

        let hour = hour_key(record);
        if let Some(index) = self.data.hourly.iter().position(|(key, _)| key == &hour) {
            bump_aggregate(&mut self.data.hourly[index].1, record);
        } else {
            let mut aggregate = Aggregate::default();
            bump_aggregate(&mut aggregate, record);
            self.data.hourly.push((hour, aggregate));
        }
        self.prune_hourly();

        let session_id = text(record, "sessionId").unwrap_or_default();
        let session_key = session_id.to_utf8().unwrap_or_default();
        if let Some(index) = self
            .data
            .sessions
            .iter()
            .position(|(key, _)| key == &session_key)
        {
            let session = &mut self.data.sessions[index].1;
            bump_aggregate(&mut session.aggregate, record);
            session.request_log.push(record.clone());
            if session.request_log.len() > 200 {
                let cut = session.request_log.len() - 100;
                session.request_log.drain(..cut);
            }
        } else {
            let mut aggregate = Aggregate::default();
            bump_aggregate(&mut aggregate, record);
            self.data.sessions.push((
                session_key,
                SessionBucket {
                    session_id,
                    aggregate,
                    request_log: vec![record.clone()],
                },
            ));
        }
        self.prune_sessions();

        self.append_record(record);
    }

    /// `snapshot()`: a copy of the global aggregate.
    pub fn snapshot(&mut self) -> JsValue {
        self.prune_expired_records();
        self.data.global.to_js()
    }

    /// `estimateCost(usage, provider, model)`; absent usage costs zero.
    ///
    /// TS delegates to `estimateCostBreakdown`, where a finite non-negative
    /// `nativeCost` wins over the rate card (`TokenStatsCollector.ts:183-186`,
    /// `:383-391`). Only the baseline cost uses the pure rate table.
    pub fn estimate_cost(&self, usage: Option<&JsValue>, provider: &str, model: &str) -> f64 {
        let Some(usage) = usage else {
            return 0.0;
        };
        let usage = usage_from_js(usage).unwrap_or_default();
        model_cost::estimate_cost(Some(&usage), provider, model, self.model_pricing.as_ref())
    }

    /// `hourlySnapshots()`: hourly buckets sorted by `hour.localeCompare`.
    pub fn hourly_snapshots(&mut self) -> JsValue {
        self.prune_expired_records();
        let mut buckets: Vec<&(String, Aggregate)> = self.data.hourly.iter().collect();
        buckets.sort_by(|left, right| locale_compare(&left.0, &right.0));
        JsValue::Array(
            buckets
                .into_iter()
                .map(|(hour, aggregate)| {
                    let mut object = aggregate.to_js().as_object().cloned().unwrap();
                    object.insert("hour".into(), JsValue::String(hour.as_str().into()));
                    JsValue::Object(object)
                })
                .collect(),
        )
    }

    /// `sessionSnapshot(sessionId)`; the caller receives the bucket contents.
    pub fn session_snapshot(&mut self, session_id: &JsString) -> Option<SessionSnapshot> {
        self.prune_expired_records();
        let session_key = session_id.to_utf8().unwrap_or_default();
        self.data
            .sessions
            .iter()
            .find(|(key, _)| key == &session_key)
            .map(|(_, session)| SessionSnapshot {
                session_id: session.session_id.clone(),
                aggregate: session.aggregate.to_js(),
                request_log: session.request_log.clone(),
            })
    }

    /// `modelUsageSnapshot(sessionId?)`, sorted provider then model.
    pub fn model_usage_snapshot(&mut self, session_id: Option<&JsString>) -> JsValue {
        self.prune_expired_records();
        let aggregate = match session_id {
            Some(session_id) => match self
                .data
                .sessions
                .iter()
                .find(|(key, _)| key == &session_id.to_utf8().unwrap_or_default())
            {
                Some((_, session)) => &session.aggregate,
                None => return JsValue::Array(Vec::new()),
            },
            None => &self.data.global,
        };
        let mut models: Vec<&ModelUsage> = aggregate
            .per_model_usage
            .iter()
            .map(|(_, usage)| usage)
            .collect();
        models.sort_by(|left, right| {
            locale_compare_js(&left.provider, &right.provider)
                .then_with(|| locale_compare_js(&left.model, &right.model))
        });
        JsValue::Array(models.into_iter().map(ModelUsage::to_js).collect())
    }

    /// `recent(limit = 50)`. With an observation buffer this is insertion
    /// order; only the rebuilt-session fallback sorts by `startedAt`.
    pub fn recent(&mut self, limit: usize) -> JsValue {
        self.prune_expired_records();
        // JS `slice(-0)` is `slice(0)` (everything), not an empty array.
        let start = |length: usize| {
            if limit == 0 {
                0
            } else {
                length.saturating_sub(limit)
            }
        };
        if !self.recent_records.is_empty() {
            return JsValue::Array(
                self.recent_records[start(self.recent_records.len())..].to_vec(),
            );
        }
        let mut logs: Vec<JsValue> = self
            .data
            .sessions
            .iter()
            .flat_map(|(_, session)| session.request_log.iter().cloned())
            .collect();
        logs.sort_by(|left, right| {
            locale_compare_js(
                &text(left, "startedAt").unwrap_or_default(),
                &text(right, "startedAt").unwrap_or_default(),
            )
        });
        JsValue::Array(logs[start(logs.len())..].to_vec())
    }

    /// `flush()`; append-only JSONL has nothing buffered.
    pub fn flush(&mut self) {}

    /// `clear()`; resets in-memory state and truncates the journal.
    pub fn clear(&mut self) {
        self.data = PersistedData::new();
        self.recent_records.clear();
        if self.retained_records.is_some() {
            self.retained_records = Some(Vec::new());
        }
        let Some(jsonl) = self.jsonl_path.clone() else {
            return;
        };
        if self.retention_ms.is_some() {
            if let Some(lock) = self.acquire_retention_lock(&jsonl) {
                if journal::recover(&jsonl).is_ok() {
                    let _ = fs::write(&jsonl, b"");
                    let _ = journal::clear_pending(&jsonl);
                }
                release_lock(&jsonl, lock);
            } else {
                let _ = fs::write(&jsonl, b"");
                let _ = journal::clear_pending(&jsonl);
            }
        } else {
            let _ = fs::write(&jsonl, b"");
            let _ = journal::clear_pending(&jsonl);
        }
    }

    /// `dispose()`; closes the append descriptor.
    pub fn dispose(&mut self) {
        self.file = None;
    }

    // ── snapshots projected for the Gateway ────────────────────────────────

    /// `ProjectRuntimeRegistry.usageSnapshotForSdk`.
    pub fn usage_snapshot_for_sdk(&mut self, session_key: Option<&JsString>) -> JsValue {
        let session = session_key.and_then(|session_key| self.session_snapshot(session_key));
        let aggregate = match &session {
            Some(session) => session.aggregate.clone(),
            None => self.snapshot(),
        };
        let mut out = JsObject::new();
        out.insert(
            "scope".into(),
            JsValue::String(
                if session.is_some() {
                    "session"
                } else {
                    "project"
                }
                .into(),
            ),
        );
        if let Some(session) = &session {
            out.insert(
                "sessionId".into(),
                JsValue::String(session.session_id.clone()),
            );
        }
        let aggregate = aggregate.as_object().cloned().unwrap();
        let mut projected = JsObject::new();
        for key in [
            "totalRequests",
            "totalInputTokens",
            "totalOutputTokens",
            "totalCost",
            "totalBaselineCost",
            "totalSavedCost",
            "perScenario",
            "perModel",
            "perProvider",
            "perTier",
            "perRole",
            "costSources",
        ] {
            if let Some(value) = aggregate.get(key) {
                projected.insert(key.into(), value.clone());
            }
        }
        out.insert("aggregate".into(), JsValue::Object(projected));
        if let Some(session) = &session {
            out.insert(
                "requests".into(),
                JsValue::Array(session.request_log.iter().map(project_request).collect()),
            );
        }
        JsValue::Object(out)
    }

    /// `ProjectRuntimeRegistry.modelUsageSnapshotForSdk`.
    pub fn model_usage_snapshot_for_sdk(&mut self, session_key: Option<&JsString>) -> JsValue {
        let session = session_key.and_then(|session_key| self.session_snapshot(session_key));
        let models = self.model_usage_snapshot(session_key);
        let mut out = JsObject::new();
        out.insert(
            "scope".into(),
            JsValue::String(
                if session.is_some() {
                    "session"
                } else {
                    "project"
                }
                .into(),
            ),
        );
        if let Some(session) = &session {
            out.insert(
                "sessionId".into(),
                JsValue::String(session.session_id.clone()),
            );
        }
        out.insert("models".into(), models);
        JsValue::Object(out)
    }

    // ── JSONL persistence ──────────────────────────────────────────────────

    fn append_record(&mut self, record: &JsValue) {
        let Some(line) = stringify_json(record).ok().flatten() else {
            return;
        };
        let line = format!("{line}\n");
        let Some(jsonl) = self.jsonl_path.clone() else {
            return;
        };
        if self.retention_ms.is_some() {
            // Human-approved safety deviation: never append to an inode that
            // compaction may replace after a lock timeout (issue #496).
            // Ledger: specs/gateway-parity/accepted-diffs.md A6.4-STATS-SIDECAR-001.
            if let Some(lock) = self.acquire_retention_lock(&jsonl) {
                if journal::recover(&jsonl).is_ok() {
                    let _ = append_bytes(&jsonl, line.as_bytes());
                } else {
                    let _ = journal::append_pending(&jsonl, line.as_bytes());
                }
                release_lock(&jsonl, lock);
            } else {
                let _ = journal::append_pending(&jsonl, line.as_bytes());
            }
            return;
        }
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(line.as_bytes());
        } else {
            let _ = append_bytes(&jsonl, line.as_bytes());
        }
    }

    fn rebuild_from_jsonl(&mut self) -> PersistedData {
        if self.retention_ms.is_some() {
            self.retained_records = Some(Vec::new());
        }
        let Some(jsonl) = self.jsonl_path.clone() else {
            return PersistedData::new();
        };
        let Some((current, _)) = journal::snapshot(&jsonl) else {
            return PersistedData::new();
        };
        let mut records = Vec::new();
        for record in current {
            if !self.is_retained(&record) {
                self.expired_records_loaded = true;
                continue;
            }
            if let Some(retained) = self.retained_records.as_mut() {
                retained.push(record.clone());
            }
            records.push(record);
        }
        create_data_from_records(&records)
    }

    fn is_retained(&self, record: &JsValue) -> bool {
        is_retained_record(self.retention_ms, (self.clock)(), record)
    }

    fn prune_expired_records(&mut self) {
        let retention = self.retention_ms;
        let now = (self.clock)();
        let before = self.retained_records.as_ref().map_or(0, Vec::len);
        let Some(retained) = self.retained_records.as_mut() else {
            return;
        };
        // Filter in place: `JsValue` is a deep tree, and TS only filters
        // references (`TokenStatsCollector.ts:305-313`).
        retained.retain(|record| is_retained_record(retention, now, record));
        if retained.len() == before {
            return;
        }
        self.data = create_data_from_records(retained);
        self.recent_records
            .retain(|record| is_retained_record(retention, now, record));
        self.compact_retained_records();
    }

    /// Retention filtering is unchanged; the replacement also consumes the
    /// immutable sidecars visible at this snapshot, with a recoverable receipt.
    fn compact_retained_records(&mut self) {
        let Some(jsonl) = self.jsonl_path.clone() else {
            return;
        };
        if self.retained_records.is_none() {
            return;
        }
        let Some(lock) = self.acquire_retention_lock(&jsonl) else {
            return;
        };
        self.compact_locked(&jsonl);
        release_lock(&jsonl, lock);
    }

    fn compact_locked(&mut self, jsonl: &Path) {
        if journal::recover(jsonl).is_err() {
            return;
        }
        let Some((current, pending)) = journal::snapshot(jsonl) else {
            return;
        };
        let kept: Vec<JsValue> = current
            .into_iter()
            .filter(|record| self.is_retained(record))
            .collect();
        let retention = self.retention_ms;
        let now = (self.clock)();
        self.data = create_data_from_records(&kept);
        self.recent_records
            .retain(|record| is_retained_record(retention, now, record));
        self.retained_records = Some(kept.clone());
        let text = kept
            .iter()
            .filter_map(|record| stringify_json(record).ok().flatten())
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let _ = journal::replace(jsonl, text.as_bytes(), &pending);
    }

    fn acquire_retention_lock(&self, jsonl: &Path) -> Option<File> {
        let lock_path = PathBuf::from(format!("{}.retention.lock", jsonl.display()));
        for _ in 0..25 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => return Some(file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return None,
            }
        }
        None
    }

    fn prune_hourly(&mut self) {
        if self.data.hourly.len() <= MAX_HOURLY_BUCKETS {
            return;
        }
        // `Object.keys(...).sort()` is a code-unit sort, not collation.
        self.data.hourly.sort_by(|left, right| left.0.cmp(&right.0));
        let remove = self.data.hourly.len() - MAX_HOURLY_BUCKETS;
        self.data.hourly.drain(..remove);
    }

    fn prune_sessions(&mut self) {
        if self.data.sessions.len() <= MAX_SESSIONS {
            return;
        }
        let mut order: Vec<usize> = (0..self.data.sessions.len()).collect();
        order.sort_by(|left, right| {
            let last = |index: usize| {
                self.data.sessions[index]
                    .1
                    .request_log
                    .last()
                    .and_then(|record| text(record, "endedAt"))
                    .unwrap_or_default()
            };
            locale_compare_js(&last(*left), &last(*right))
        });
        let remove = self.data.sessions.len() - MAX_SESSIONS;
        let mut remove = order.into_iter().take(remove).collect::<Vec<_>>();
        remove.sort_unstable();
        for index in remove.into_iter().rev() {
            self.data.sessions.remove(index);
        }
    }

    fn estimate_cost_breakdown(
        &self,
        usage: &pilotdeck_protocol::model::Usage,
        provider: &JsString,
        model: &JsString,
    ) -> (model_cost::CostBreakdown, CostSource) {
        let provider = provider.to_utf8().unwrap_or_default();
        let model = model.to_utf8().unwrap_or_default();
        if usage
            .native_cost
            .is_some_and(|value| value.is_finite() && value >= 0.0)
        {
            return (
                model_cost::CostBreakdown {
                    total: usage.native_cost.unwrap_or(0.0),
                    ..model_cost::CostBreakdown::default()
                },
                CostSource::ProviderReported,
            );
        }
        let breakdown = model_cost::estimate_cost_breakdown(
            usage,
            &provider,
            &model,
            self.model_pricing.as_ref(),
        );
        let (_, source) = model_cost::lookup_model_pricing_with_source(
            &provider,
            &model,
            self.model_pricing.as_ref(),
        );
        (breakdown, cost_source(source))
    }

    fn calculate_baseline_cost(
        &self,
        usage: &pilotdeck_protocol::model::Usage,
        provider: &JsString,
        model: &JsString,
    ) -> Option<f64> {
        let Some(baseline) = self.baseline_model.as_ref().filter(|b| !b.model.is_empty()) else {
            return Some(self.calculate_cost(usage, provider, model));
        };
        let base_provider = if baseline.provider.is_empty() {
            provider.clone()
        } else {
            baseline.provider.clone()
        };
        if &base_provider == provider && &baseline.model == model {
            return None;
        }
        Some(self.calculate_cost(usage, &base_provider, &baseline.model))
    }

    fn calculate_cost(
        &self,
        usage: &pilotdeck_protocol::model::Usage,
        provider: &JsString,
        model: &JsString,
    ) -> f64 {
        self.rate_cost(
            usage,
            &provider.to_utf8().unwrap_or_default(),
            &model.to_utf8().unwrap_or_default(),
        )
    }

    /// `TokenStatsCollector.calculateCost`: rate-table math only. `nativeCost`
    /// is stripped because TS computes the baseline from token rates even when
    /// the primary cost was provider-reported.
    fn rate_cost(
        &self,
        usage: &pilotdeck_protocol::model::Usage,
        provider: &str,
        model: &str,
    ) -> f64 {
        let mut usage = usage.clone();
        usage.native_cost = None;
        model_cost::estimate_cost_breakdown(&usage, provider, model, self.model_pricing.as_ref())
            .total
    }
}

/// Read-only view used by the snapshot projections.
pub struct SessionSnapshot {
    pub session_id: JsString,
    pub aggregate: JsValue,
    pub request_log: Vec<JsValue>,
}
impl SessionSnapshot {
    /// Shape of `PersistedData.sessions[id]` returned by `sessionSnapshot`.
    pub fn to_js(&self) -> JsValue {
        js_object(vec![
            ("sessionId", JsValue::String(self.session_id.clone())),
            ("aggregate", self.aggregate.clone()),
            ("requestLog", JsValue::Array(self.request_log.clone())),
        ])
    }
}

fn project_request(record: &JsValue) -> JsValue {
    let mut out = JsObject::new();
    if let Some(turn_id) = text(record, "turnId") {
        out.insert("turnId".into(), JsValue::String(turn_id));
    }
    out.insert(
        "provider".into(),
        JsValue::String(text(record, "provider").unwrap_or_default()),
    );
    out.insert(
        "model".into(),
        JsValue::String(text(record, "model").unwrap_or_default()),
    );
    if let Some(role) = text(record, "role") {
        out.insert("role".into(), JsValue::String(role));
    }
    out.insert(
        "usage".into(),
        record
            .get("usage")
            .cloned()
            .unwrap_or_else(|| JsValue::Object(JsObject::new())),
    );
    if let Some(cost) = record.get("cost").filter(|cost| cost.as_object().is_some()) {
        out.insert("cost".into(), cost.clone());
    }
    if let Some(source) = text(record, "costSource") {
        out.insert("costSource".into(), JsValue::String(source));
    }
    out.insert(
        "startedAt".into(),
        JsValue::String(text(record, "startedAt").unwrap_or_default()),
    );
    out.insert(
        "endedAt".into(),
        JsValue::String(text(record, "endedAt").unwrap_or_default()),
    );
    JsValue::Object(out)
}

fn create_data_from_records(records: &[JsValue]) -> PersistedData {
    let mut data = PersistedData::new();
    for record in records {
        bump_aggregate(&mut data.global, record);

        let hour = hour_key(record);
        if let Some(index) = data.hourly.iter().position(|(key, _)| key == &hour) {
            bump_aggregate(&mut data.hourly[index].1, record);
        } else {
            let mut aggregate = Aggregate::default();
            bump_aggregate(&mut aggregate, record);
            data.hourly.push((hour, aggregate));
        }

        let session_id = text(record, "sessionId").unwrap_or_default();
        let session_key = session_id.to_utf8().unwrap_or_default();
        if let Some(index) = data
            .sessions
            .iter()
            .position(|(key, _)| key == &session_key)
        {
            data.sessions[index].1.request_log.push(record.clone());
            bump_aggregate(&mut data.sessions[index].1.aggregate, record);
        } else {
            let mut aggregate = Aggregate::default();
            bump_aggregate(&mut aggregate, record);
            data.sessions.push((
                session_key,
                SessionBucket {
                    session_id,
                    aggregate,
                    request_log: vec![record.clone()],
                },
            ));
        }
    }

    if data.hourly.len() > MAX_HOURLY_BUCKETS {
        data.hourly.sort_by(|left, right| left.0.cmp(&right.0));
        let remove = data.hourly.len() - MAX_HOURLY_BUCKETS;
        data.hourly.drain(..remove);
    }

    if data.sessions.len() > MAX_SESSIONS {
        let mut order: Vec<usize> = (0..data.sessions.len()).collect();
        order.sort_by(|left, right| {
            let last = |index: usize| {
                data.sessions[index]
                    .1
                    .request_log
                    .last()
                    .and_then(|record| text(record, "endedAt"))
                    .unwrap_or_default()
            };
            locale_compare_js(&last(*left), &last(*right))
        });
        let remove = data.sessions.len() - MAX_SESSIONS;
        let mut remove = order.into_iter().take(remove).collect::<Vec<_>>();
        remove.sort_unstable();
        for index in remove.into_iter().rev() {
            data.sessions.remove(index);
        }
    }
    for (_, session) in data.sessions.iter_mut() {
        if session.request_log.len() > 200 {
            let cut = session.request_log.len() - 100;
            session.request_log.drain(..cut);
        }
    }
    data
}

fn bump_aggregate(aggregate: &mut Aggregate, record: &JsValue) {
    let usage = record.get("usage");
    let usage_number = |field: &str| {
        usage
            .and_then(|usage| usage.get(field))
            .and_then(JsValue::as_number)
            .unwrap_or(0.0)
    };
    aggregate.total_requests += 1.0;
    aggregate.total_input_tokens += usage_number("inputTokens");
    aggregate.total_output_tokens += usage_number("outputTokens");
    let cost = record
        .get("cost")
        .and_then(|cost| cost.get("total"))
        .and_then(JsValue::as_number)
        .unwrap_or(0.0);
    let baseline = record
        .get("baselineCost")
        .and_then(JsValue::as_number)
        .unwrap_or(cost);
    aggregate.total_cost += cost;
    aggregate.total_baseline_cost += baseline;
    aggregate.total_saved_cost += baseline - cost;
    let source = text(record, "costSource").unwrap_or_else(|| "legacy_unknown".into());
    bump_map(
        &mut aggregate.cost_sources,
        source.to_utf8().unwrap_or_default(),
        1.0,
    );

    bump_map(
        &mut aggregate.per_scenario,
        text(record, "scenarioType")
            .map(|value| value.to_utf8().unwrap_or_default())
            .unwrap_or_else(|| "undefined".into()),
        1.0,
    );

    let provider = text(record, "provider").unwrap_or_default();
    let model = text(record, "model").unwrap_or_default();
    let model_key = format!(
        "{}/{}",
        provider.to_utf8().unwrap_or_default(),
        model.to_utf8().unwrap_or_default()
    );
    bump_map(&mut aggregate.per_model, model_key.clone(), 1.0);
    bump_map(
        &mut aggregate.per_provider,
        provider.to_utf8().unwrap_or_default(),
        1.0,
    );
    if let Some(tier) = text(record, "tier") {
        bump_map(
            &mut aggregate.per_tier,
            tier.to_utf8().unwrap_or_default(),
            1.0,
        );
    }
    let role = text(record, "role");
    if let Some(role) = &role {
        bump_map(
            &mut aggregate.per_role,
            role.to_utf8().unwrap_or_default(),
            1.0,
        );
    }

    let source = text(record, "costSource").unwrap_or_else(|| "legacy_unknown".into());
    let usage = usage_from_js(usage.unwrap_or(&JsValue::Undefined)).unwrap_or_default();
    let item = aggregate.model_usage(&provider, &model, &model_key);
    bump_model_usage(&mut item.totals, &usage, cost, &source);
    if let Some(role) = &role {
        let role_key = role.to_utf8().unwrap_or_default();
        if let Some(index) = item.roles.iter().position(|(key, _)| key == &role_key) {
            bump_model_usage(&mut item.roles[index].1, &usage, cost, &source);
        } else {
            let mut totals = RoleAggregate::default();
            bump_model_usage(&mut totals, &usage, cost, &source);
            item.roles.push((role_key, totals));
        }
    }
}

fn bump_model_usage(
    aggregate: &mut RoleAggregate,
    usage: &pilotdeck_protocol::model::Usage,
    cost: f64,
    source: &JsString,
) {
    let input = usage.input_tokens.unwrap_or(0.0);
    let output = usage.output_tokens.unwrap_or(0.0);
    let cache_read = usage.cache_read_tokens.unwrap_or(0.0);
    let cache_write = usage.cache_write_tokens.unwrap_or(0.0);
    aggregate.total_requests += 1.0;
    aggregate.input_tokens += input;
    aggregate.output_tokens += output;
    aggregate.cache_read_tokens += cache_read;
    aggregate.cache_write_tokens += cache_write;
    aggregate.total_tokens += usage
        .total_tokens
        .unwrap_or(input + output + cache_read + cache_write);
    aggregate.total_cost += cost;
    bump_map(
        &mut aggregate.cost_sources,
        source.to_utf8().unwrap_or_default(),
        1.0,
    );
}

fn bump_map(map: &mut Vec<(String, f64)>, key: String, delta: f64) {
    if let Some(index) = map.iter().position(|(name, _)| name == &key) {
        map[index].1 += delta;
    } else {
        map.push((key, delta));
    }
}

fn is_retained_record(retention: Option<f64>, now: f64, record: &JsValue) -> bool {
    let Some(retention) = retention else {
        return true;
    };
    let Some(ended_at) = text(record, "endedAt").and_then(|value| parse_date_ms(&value)) else {
        // `!Number.isFinite(endedAt)`: keep malformed historical timestamps.
        return true;
    };
    ended_at >= now - retention
}

#[cfg(test)]
fn read_stats_records(path: &Path) -> Vec<JsValue> {
    journal::snapshot(path)
        .map(|(records, _)| records)
        .unwrap_or_default()
}

fn parse_stats_records(raw: &str) -> Vec<JsValue> {
    let mut records = Vec::new();
    for line in raw.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(record) = parse_json(line) else {
            continue;
        };
        if text(&record, "sessionId").is_some() && text(&record, "startedAt").is_some() {
            records.push(record);
        }
    }
    records
}

/// `fs.readFileSync(path, "utf8")` throws `ERR_STRING_TOO_LONG` when the decoded
/// string exceeds V8's limit (`MAX_STRING_LENGTH`). Callers treat that exactly
/// like an IO failure; no journal is read past the limit.
fn read_bounded_text(path: &Path, limit: usize) -> Option<String> {
    // `fs.readFileSync(path, "utf8")` replaces invalid bytes with U+FFFD instead
    // of throwing, and a lossy decode maps each byte to at most one UTF-16 unit,
    // so `limit * 3` bytes is still a safe upper bound for `limit` units.
    let maximum_bytes = (limit as u64).saturating_mul(3);
    if fs::metadata(path).ok()?.len() > maximum_bytes {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (text.encode_utf16().count() <= limit).then_some(text)
}

fn append_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

fn release_lock(jsonl: &Path, _file: File) {
    let _ = fs::remove_file(format!("{}.retention.lock", jsonl.display()));
}

fn cost_source(source: PricingSource) -> CostSource {
    match source {
        PricingSource::ConfiguredPrice => CostSource::ConfiguredPrice,
        PricingSource::BuiltInEstimate => CostSource::BuiltInEstimate,
        PricingSource::FallbackEstimate => CostSource::FallbackEstimate,
    }
}

fn hour_key(record: &JsValue) -> String {
    text(record, "startedAt")
        .map(|value| value.units().iter().take(13).copied().collect())
        .and_then(|units| JsString::from_utf16(units).to_utf8().ok())
        .unwrap_or_default()
}

fn text(value: &JsValue, name: &str) -> Option<JsString> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn number(value: f64) -> JsValue {
    JsValue::Number(value)
}

fn js_object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

fn map_js(entries: &[(String, f64)]) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.as_str().into(), number(*value));
    }
    JsValue::Object(out)
}

fn model_map_js(entries: &[(String, ModelUsage)]) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.as_str().into(), value.to_js());
    }
    JsValue::Object(out)
}

fn locale_compare(left: &str, right: &str) -> Ordering {
    locale_compare_js(&left.into(), &right.into())
}

fn locale_compare_js(left: &JsString, right: &JsString) -> Ordering {
    static COLLATOR: OnceLock<icu_collator::CollatorBorrowed<'static>> = OnceLock::new();
    let collator = COLLATOR.get_or_init(|| {
        let locale: icu_locale_core::Locale = "en-US".parse().expect("valid locale");
        icu_collator::Collator::try_new(locale.into(), Default::default())
            .expect("bundled collation data")
    });
    collator.compare_utf16(left.units(), right.units())
}

/// `resolvePilotHome()`: `PILOT_HOME ?? "~/.pilotdeck"`, then Node
/// `path.resolve` against the process cwd (`paths.ts:21-29`). An empty
/// `PILOT_HOME` is a set value and resolves to the cwd, not the default.
///
/// Node reads `process.cwd()` only when `path.resolve` receives a relative
/// input. A bare `~` is returned verbatim by `normalizeHomePath`, so it never
/// reads the cwd either; `~/...` with the usual absolute HOME, an absolute
/// `PILOT_HOME` and the unset default all stay cwd-free when the cwd is
/// deleted. A failed read is returned unchanged: TS throws
/// `ENOENT ... uv_cwd` instead of resolving against `/` (#879).
fn default_pilot_home() -> std::io::Result<PathBuf> {
    let raw = std::env::var("PILOT_HOME").ok();
    let home = std::env::var_os("HOME");
    if raw.as_deref() == Some("~") {
        // `normalizeHomePath` returns `homedir()` verbatim for the bare "~";
        // it never calls `path.resolve`, so even an empty or relative HOME
        // does not need the process cwd (TokenStatsCollector.ts:90-96).
        return Ok(home
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~")));
    }
    let expanded = expand_pilot_home(raw.as_deref(), home.as_deref());
    if expanded.is_absolute() {
        // `path.resolve` never consults the cwd for an absolute input. The
        // empty base makes that explicit instead of inventing a directory.
        return Ok(crate::metadata::resolve_path(Path::new(""), &expanded));
    }
    let cwd = std::env::current_dir().map_err(|error| {
        let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
        std::io::Error::new(error.kind(), message)
    })?;
    Ok(crate::metadata::resolve_path(&cwd, &expanded))
}

fn expand_pilot_home(raw: Option<&str>, home: Option<&std::ffi::OsStr>) -> PathBuf {
    let raw = raw.unwrap_or("~/.pilotdeck");
    if raw == "~" {
        home.map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    }
}

#[cfg(test)]
fn resolve_pilot_home(raw: Option<&str>, home: Option<&std::ffi::OsStr>, cwd: &Path) -> PathBuf {
    if raw == Some("~") {
        return expand_pilot_home(raw, home);
    }
    crate::metadata::resolve_path(cwd, &expand_pilot_home(raw, home))
}

/// One-time migration from the old `stats.json` / `router-stats.json` formats.
fn migrate_json_to_jsonl(router_dir: &Path, jsonl: &Path) {
    migrate_json_to_jsonl_with(router_dir, jsonl, &mut |path, text| fs::write(path, text));
}

/// `write` is the JSONL writer seam: TS wraps each candidate in `try/catch`, so
/// a failed `writeFileSync` skips the rename and continues with the next
/// candidate (`TokenStatsCollector.ts:634-662`). The seam keeps that branch
/// testable without a fault-injecting filesystem.
fn migrate_json_to_jsonl_with(
    router_dir: &Path,
    jsonl: &Path,
    write: &mut dyn FnMut(&Path, &str) -> std::io::Result<()>,
) {
    if jsonl.exists() {
        return;
    }
    let candidates = [
        router_dir.join("stats.json"),
        router_dir
            .parent()
            .unwrap_or(router_dir)
            .join("router-stats.json"),
    ];
    for json_path in candidates {
        if !json_path.exists() {
            continue;
        }
        let Some(raw) = read_bounded_text(&json_path, super::MAX_STRING_LENGTH) else {
            continue;
        };
        let Ok(parsed) = parse_json(&raw) else {
            continue;
        };
        let Some(sessions) = parsed.get("sessions").and_then(JsValue::as_object) else {
            continue;
        };
        let mut lines: Vec<(String, String)> = Vec::new();
        for (_, session) in sessions.entries() {
            let Some(JsValue::Array(log)) = session.get("requestLog") else {
                continue;
            };
            for record in log {
                if text(record, "sessionId").is_none() || text(record, "startedAt").is_none() {
                    continue;
                }
                let Some(line) = stringify_json(record).ok().flatten() else {
                    continue;
                };
                let started = text(record, "startedAt")
                    .map(|value| value.to_utf8().unwrap_or_default())
                    .unwrap_or_default();
                lines.push((started, line));
            }
        }
        lines.sort_by(|left, right| locale_compare(&left.0, &right.0));
        if !lines.is_empty() {
            let text = lines
                .into_iter()
                .map(|(_, line)| format!("{line}\n"))
                .collect::<String>();
            if write(jsonl, &text).is_err() {
                // TS catches the write error and moves on to the next
                // candidate without renaming the source.
                continue;
            }
        }
        let _ = fs::rename(&json_path, format!("{}.bak", json_path.display()));
        return;
    }
}

/// Node 22 / V8 12.4 Date.parse, shared with the production execution clock.
/// Both ParseES5DateTime and its legacy continuation consume UTF-16, including
/// lone surrogates in comments or after NUL. Unsupported local profiles keep
/// the accounting row. See stats-dateparse/README.md for the admitted TZ data.
/// Ledger: gateway accepted-diffs A6.4-STATS-RETENTION-001 (human-approved subset).
pub(crate) fn parse_date_ms(value: &JsString) -> Option<f64> {
    pilotdeck_transport::system_clock::parse_node22_date_with_local(value, &stats_local_to_utc)
        .ok()
        .flatten()
}

fn stats_local_to_utc(wall: i64) -> Result<i64, JsString> {
    use std::io::Read;
    let unsupported = || JsString::from("STATS_LOCAL_DATE_UNVERIFIED_TZIF");
    let tz = std::env::var_os("TZ");
    let zone = if let Some(tz) = tz.as_deref() {
        let name = tz.to_str().ok_or_else(unsupported)?;
        // An OS file name is not necessarily an ICU TZ identifier: Node uses
        // UTC for TZ=localtime even when /etc/localtime contains Shanghai.
        if !matches!(name, "UTC" | "America/New_York" | "Asia/Shanghai") {
            return Err(unsupported());
        }
        name.to_owned()
    } else {
        // For the unset-TZ host default, admit only a canonical system link to
        // one of those same named profiles. A copied/unknown default is kept.
        let default = fs::canonicalize("/etc/localtime").map_err(|_| unsupported())?;
        ["UTC", "America/New_York", "Asia/Shanghai"]
            .into_iter()
            .find(|name| {
                fs::canonicalize(Path::new("/usr/share/zoneinfo").join(name))
                    .ok()
                    .as_ref()
                    == Some(&default)
            })
            .ok_or_else(unsupported)?
            .to_owned()
    };
    let path = pilotdeck_transport::system_clock::system_tzif_path(tz.as_deref())
        .ok_or_else(unsupported)?;
    // Bound reads even for an unexpected symlink under the system zone tree.
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| unsupported())?
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| unsupported())?;
    stats_tzif_local(wall, &bytes, &zone)
}

fn stats_tzif_local(wall: i64, bytes: &[u8], zone: &str) -> Result<i64, JsString> {
    use sha2::{Digest, Sha256};
    static VERIFIED: OnceLock<Vec<(String, String)>> = OnceLock::new();
    let hashes = VERIFIED.get_or_init(|| {
        let data: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/stats-dateparse/local-profiles.json"
        ))
        .unwrap_or_default();
        data.get("profiles")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|profile| {
                Some((
                    profile.get("tz")?.as_str()?.to_owned(),
                    profile.get("sha256")?.as_str()?.to_owned(),
                ))
            })
            .collect()
    });
    // Node embeds ICU tzdata; the OS may update independently. Bind BOTH the
    // ICU identifier and the exact bytes exercised by the real TS profile.
    let digest = format!("{:x}", Sha256::digest(bytes));
    let verified = hashes
        .iter()
        .any(|(name, sha)| name == zone && sha == &digest);
    if bytes.len() > 1_048_576 || !verified {
        return Err("STATS_LOCAL_DATE_UNVERIFIED_TZIF".into());
    }
    pilotdeck_transport::system_clock::resolve_tzif_local(wall, bytes)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stats_dateparse_tests.rs"]
mod dateparse_tests;

#[cfg(all(test, unix))]
#[path = "../../../tests/fixtures/stats-cwd/regression.rs"]
mod cwd_tests;
