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
    pub fn new(config: Option<&StatsConfig>, pilot_home: Option<&Path>) -> Self {
        Self::with_clock(config, pilot_home, Arc::new(wall_clock))
    }

    pub fn with_clock(
        config: Option<&StatsConfig>,
        pilot_home: Option<&Path>,
        clock: StatsClock,
    ) -> Self {
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
            return collector;
        }
        let router_dir = match config.and_then(|config| config.file_path.as_deref()) {
            Some(file) => file.parent().unwrap_or(Path::new(".")).to_path_buf(),
            None => pilot_home
                .map(Path::to_path_buf)
                .unwrap_or_else(default_pilot_home)
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
        collector
    }

    pub fn enabled(&self) -> bool {
        self.enabled
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
                let _ = fs::write(&jsonl, b"");
                release_lock(&jsonl, lock);
            } else {
                let _ = fs::write(&jsonl, b"");
            }
        } else {
            let _ = fs::write(&jsonl, b"");
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
            if let Some(lock) = self.acquire_retention_lock(&jsonl) {
                let _ = append_bytes(&jsonl, line.as_bytes());
                release_lock(&jsonl, lock);
            } else {
                let _ = append_bytes(&jsonl, line.as_bytes());
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
        let Some(raw) = read_bounded_text(&jsonl, super::MAX_STRING_LENGTH) else {
            return PersistedData::new();
        };
        let mut records = Vec::new();
        for line in raw.split('\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(record) = parse_json(line) else {
                continue;
            };
            if text(&record, "sessionId").is_none() || text(&record, "startedAt").is_none() {
                continue;
            }
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

    /// Retention is opt-in; rewrite only while retention-aware writers hold the
    /// journal lock. The temporary name embeds pid + `Date.now()` like TS, and
    /// is renamed onto the journal before the lock is released.
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
        let current = read_stats_records(&jsonl);
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
        let temporary = format!(
            "{}.{}.{}.tmp",
            jsonl.display(),
            std::process::id(),
            (self.clock)() as i64
        );
        let text = kept
            .iter()
            .filter_map(|record| stringify_json(record).ok().flatten())
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        if fs::write(&temporary, text).is_ok() {
            let _ = fs::rename(&temporary, &jsonl);
        }
        release_lock(&jsonl, lock);
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
    let Some(ended_at) = text(record, "endedAt")
        .and_then(|value| value.to_utf8().ok())
        .and_then(|value| parse_date_ms(&value))
    else {
        // `!Number.isFinite(endedAt)`: keep malformed historical timestamps.
        return true;
    };
    ended_at >= now - retention
}

fn read_stats_records(path: &Path) -> Vec<JsValue> {
    let Some(raw) = read_bounded_text(path, super::MAX_STRING_LENGTH) else {
        return Vec::new();
    };
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
    // A UTF-8 file can decode to at most `bytes` UTF-16 units and every scalar
    // costs at most 3 bytes per unit (4-byte scalars pair up), so a larger file
    // cannot fit and is rejected before it is read.
    let maximum_bytes = (limit as u64).saturating_mul(3);
    if fs::metadata(path).ok()?.len() > maximum_bytes {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
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
fn default_pilot_home() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    resolve_pilot_home(
        std::env::var("PILOT_HOME").ok().as_deref(),
        std::env::var_os("HOME").as_deref(),
        &cwd,
    )
}

fn resolve_pilot_home(raw: Option<&str>, home: Option<&std::ffi::OsStr>, cwd: &Path) -> PathBuf {
    let raw = raw.unwrap_or("~/.pilotdeck");
    let expanded = if raw == "~" {
        home.map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };
    crate::metadata::resolve_path(cwd, &expanded)
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

/// `Date.parse` for the timestamp shapes a router journal can contain.
///
/// Mirrors V8/Node 22.22 rules (probed with `TZ=UTC`, see
/// `tests/fixtures/token-stats/`):
/// - date-only `YYYY-M-D` is UTC; a date-time without a zone is **local** time;
/// - `t`/`z` may be lower case; seconds are optional; fractions truncate to ms;
/// - month/day must be two digits once a time follows (`2026-3-1T...` is NaN);
/// - `24:00[:00[.000]]` rolls to the next day; later values that day are NaN;
/// - numeric offsets are `±HH:MM` or `±HHMM`, both fields in range: `+99:99`,
///   `+24:00` and `+01:99` are NaN (the old port accepted them and could drop
///   an accounting row);
/// - day `31` in a 30-day month rolls over (`2026-02-30` is valid in V8) while
///   day `32` is NaN; single-digit month/day is only accepted without a time;
/// - RFC 2822 date-times (`Mon, 01 Jan 2024 00:00:00 GMT`, numeric zones and
///   `GMT±HHMM`) are accepted; named zones other than GMT/UT/UTC are not.
///
/// `None` means `Number.isFinite(Date.parse(...))` is false, which retention
/// treats as "keep this historical record".
enum ParsedDate {
    Instant(f64),
    Local { naive: f64 },
}

pub(crate) fn parse_date_ms(value: &str) -> Option<f64> {
    match parse_date_value(value)? {
        ParsedDate::Instant(millis) => Some(millis),
        ParsedDate::Local { naive } => {
            // Two passes resolve the local offset around DST boundaries the
            // same way the platform `mktime` does for V8.
            let first = naive - local_offset_ms(naive);
            Some(naive - local_offset_ms(first))
        }
    }
}

/// Test seam: resolve a zone-less timestamp with an explicit offset so the
/// expectation does not depend on the machine running the tests.
#[cfg(test)]
fn parse_date_ms_with_offset(value: &str, offset_ms: i64) -> Option<f64> {
    match parse_date_value(value)? {
        ParsedDate::Instant(millis) => Some(millis),
        ParsedDate::Local { naive } => Some(naive - offset_ms as f64),
    }
}

fn parse_date_value(value: &str) -> Option<ParsedDate> {
    parse_iso_date(value).or_else(|| parse_rfc2822_date(value))
}

fn parse_iso_date(value: &str) -> Option<ParsedDate> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let (year, _) = digits_at(bytes, &mut index, 4, 6)?;
    if bytes.get(index) != Some(&b'-') {
        return None;
    }
    index += 1;
    let (month, month_len) = digits_at(bytes, &mut index, 1, 2)?;
    if bytes.get(index) != Some(&b'-') {
        return None;
    }
    index += 1;
    let (day, day_len) = digits_at(bytes, &mut index, 1, 2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if index == bytes.len() {
        return Some(ParsedDate::Instant(civil_ms(year, month, day, 0, 0, 0, 0)));
    }
    // V8 only accepts the single-digit legacy form for a date without a time.
    if month_len != 2 || day_len != 2 || !matches!(bytes[index], b'T' | b't' | b' ') {
        return None;
    }
    index += 1;
    let (hour, _) = digits_at(bytes, &mut index, 2, 2)?;
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    let (minute, _) = digits_at(bytes, &mut index, 2, 2)?;
    let mut second = 0;
    if bytes.get(index) == Some(&b':') {
        index += 1;
        let (value, _) = digits_at(bytes, &mut index, 2, 2)?;
        second = value;
    }
    let mut millis = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return None;
        }
        let mut units = value[start..index].chars().take(3).collect::<String>();
        while units.len() < 3 {
            units.push('0');
        }
        millis = units.parse().ok()?;
    }
    if !(0..=59).contains(&minute) || !(0..=59).contains(&second) || !(0..=24).contains(&hour) {
        return None;
    }
    if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
        return None;
    }
    let rollover = if hour == 24 { 86_400_000 } else { 0 };
    let hour = if hour == 24 { 0 } else { hour };
    let naive = civil_ms(year, month, day, hour, minute, second, millis) + rollover as f64;
    let offset = if index == bytes.len() {
        None
    } else if matches!(bytes[index], b'Z' | b'z') && index + 1 == bytes.len() {
        Some(0)
    } else {
        Some(parse_offset(&value[index..])?)
    };
    Some(match offset {
        Some(offset) => ParsedDate::Instant(naive - offset as f64),
        None => ParsedDate::Local { naive },
    })
}

fn parse_rfc2822_date(value: &str) -> Option<ParsedDate> {
    let tokens: Vec<&str> = value
        .split([' ', ','])
        .filter(|token| !token.is_empty())
        .collect();
    let mut index = 0;
    if let Some(first) = tokens.first()
        && is_weekday(first)
    {
        index = 1;
    }
    let first = tokens.get(index)?;
    let (day, month) = if let Some(day) = parse_decimal(first) {
        (day, month_number(tokens.get(index + 1)?)?)
    } else {
        (parse_decimal(tokens.get(index + 1)?)?, month_number(first)?)
    };
    index += 2;
    let year_token = tokens.get(index)?;
    let mut year = parse_decimal(year_token)?;
    index += 1;
    if year_token.len() <= 2 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let (hour, minute, second) = parse_clock(tokens.get(index)?)?;
    index += 1;
    let offset = match tokens.get(index) {
        Some(zone) => {
            index += 1;
            Some(parse_rfc_zone(zone)?)
        }
        None => None,
    };
    if index != tokens.len() || !(1..=31).contains(&day) {
        return None;
    }
    let rollover = if hour == 24 { 86_400_000 } else { 0 };
    let hour = if hour == 24 { 0 } else { hour };
    let naive = civil_ms(year, month as i64, day, hour, minute, second, 0) + rollover as f64;
    Some(match offset {
        Some(offset) => ParsedDate::Instant(naive - offset as f64),
        None => ParsedDate::Local { naive },
    })
}

fn parse_clock(text: &str) -> Option<(i64, i64, i64)> {
    let mut parts = text.split(':');
    let hour = parse_decimal(parts.next()?)?;
    let minute = parse_decimal(parts.next()?)?;
    let second = match parts.next() {
        Some(value) => parse_decimal(value)?,
        None => 0,
    };
    if parts.next().is_some()
        || !(0..=24).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || (hour == 24 && (minute != 0 || second != 0))
    {
        return None;
    }
    Some((hour, minute, second))
}

/// `±HH:MM` or `±HHMM`; the whole token must be consumed.
fn parse_offset(text: &str) -> Option<i64> {
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &text[1..];
    let (hour, minute) = if let Some((hour, minute)) = rest.split_once(':') {
        (fixed_decimal(hour, 2)?, fixed_decimal(minute, 2)?)
    } else {
        if rest.len() != 4 {
            return None;
        }
        (fixed_decimal(&rest[..2], 2)?, fixed_decimal(&rest[2..], 2)?)
    };
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(sign * (hour * 3_600 + minute * 60) * 1_000)
}

fn parse_rfc_zone(zone: &str) -> Option<i64> {
    let upper = zone.to_ascii_uppercase();
    if matches!(upper.as_str(), "Z" | "UT" | "UTC" | "GMT") {
        return Some(0);
    }
    let numeric = upper
        .strip_prefix("GMT")
        .or_else(|| upper.strip_prefix("UTC"))
        .unwrap_or(&upper);
    parse_offset(numeric)
}

fn is_weekday(text: &str) -> bool {
    matches!(
        text.to_ascii_lowercase().as_str(),
        "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun"
    )
}

fn month_number(text: &str) -> Option<u32> {
    let month = match text.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    Some(month)
}

fn parse_decimal(text: &str) -> Option<i64> {
    if text.is_empty() || text.len() > 4 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn fixed_decimal(text: &str, width: usize) -> Option<i64> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn digits_at(bytes: &[u8], index: &mut usize, min: usize, max: usize) -> Option<(i64, usize)> {
    let start = *index;
    while *index < bytes.len() && bytes[*index].is_ascii_digit() && *index - start < max {
        *index += 1;
    }
    let length = *index - start;
    if length < min {
        return None;
    }
    let value = std::str::from_utf8(&bytes[start..*index])
        .ok()?
        .parse()
        .ok()?;
    Some((value, length))
}

fn civil_ms(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
) -> f64 {
    (crate::runtime::tools::builtin::time::days_from_civil(year, month, day) * 86_400_000
        + hour * 3_600_000
        + minute * 60_000
        + second * 1_000
        + millis) as f64
}

/// Local UTC offset (ms) at an instant, with the host timezone database.
fn local_offset_ms(epoch_ms: f64) -> f64 {
    use chrono::Local;
    let Some(instant) = chrono::DateTime::from_timestamp_millis(epoch_ms as i64) else {
        return 0.0;
    };
    instant.with_timezone(&Local).offset().local_minus_utc() as f64 * 1_000.0
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
