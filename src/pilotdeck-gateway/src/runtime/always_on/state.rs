//! Port of TS `src/always-on/storage/DiscoveryStateStore.ts` — `state.json`
//! normalization, the daily-budget reset and the `markFire*` writes.
//!
//! The state is carried as an ordered `JsObject` because the TS key order is
//! dynamic and observable in the file bytes: `normalizeState` rebuilds the
//! object with all twelve canonical keys (absent ones held as
//! `JSON.stringify`-invisible `undefined` slots), so a later assignment
//! updates in place; the ENOENT default carries only its four keys, so the
//! first `markFireStarted` appends after them (golden `s2` vs `s3`). Write
//! = `JSON.stringify(state, null, 2)` with no trailing newline (golden
//! `s10`), via the shared `stringify_pretty` port.
use std::io;

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

use crate::runtime::cron::{iso_from_epoch_ms, stringify_pretty};

use super::AlwaysOnPaths;

/// `getDayKey` (DiscoveryStateStore.ts:6-8): `toISOString().slice(0, 10)`.
pub(crate) fn day_key(now_ms: f64) -> String {
    iso_from_epoch_ms(now_ms)[..10].to_string()
}

/// `defaultDiscoveryState` (DiscoveryStateStore.ts:10-15) — the four-key
/// object an ENOENT (or unparseable) state file reads as.
fn default_state(now_ms: f64) -> JsObject {
    let mut state = JsObject::new();
    state.insert("schemaVersion".into(), JsValue::Number(1.0));
    state.insert(
        "todayKey".into(),
        JsValue::String(JsString::from(day_key(now_ms))),
    );
    state.insert("todayRunCount".into(), JsValue::Number(0.0));
    state.insert("consecutiveFailures".into(), JsValue::Number(0.0));
    state
}

fn as_string(value: Option<&JsValue>) -> Option<JsString> {
    value.and_then(JsValue::as_string).cloned()
}

fn as_count(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|count| *count >= 0.0)
}

/// `normalizeDormant` (DiscoveryStateStore.ts:231-243).
fn normalize_dormant(value: Option<&JsValue>) -> JsValue {
    let Some(object) = value.and_then(JsValue::as_object) else {
        return JsValue::Undefined;
    };
    let (Some(since), Some(last_baseline)) = (
        as_string(object.get("since")),
        as_string(object.get("lastBaselineAt")),
    ) else {
        return JsValue::Undefined;
    };
    let mut out = JsObject::new();
    out.insert("since".into(), JsValue::String(since));
    out.insert("lastBaselineAt".into(), JsValue::String(last_baseline));
    if let Some(last_change) = as_string(object.get("lastChangeAt")) {
        out.insert("lastChangeAt".into(), JsValue::String(last_change));
    }
    JsValue::Object(out)
}

/// `normalizeCurrentWorkspace` (DiscoveryStateStore.ts:182-203).
fn normalize_current_workspace(value: Option<&JsValue>) -> JsValue {
    let Some(object) = value.and_then(JsValue::as_object) else {
        return JsValue::Undefined;
    };
    let (Some(run_id), Some(cwd)) = (as_string(object.get("runId")), as_string(object.get("cwd")))
    else {
        return JsValue::Undefined;
    };
    let strategy = object
        .get("strategy")
        .and_then(JsValue::as_string)
        .filter(|strategy| {
            strategy.equals_str("git-worktree") || strategy.equals_str("snapshot-copy")
        });
    let Some(strategy) = strategy else {
        return JsValue::Undefined;
    };
    let mut out = JsObject::new();
    out.insert("runId".into(), JsValue::String(run_id));
    out.insert("cwd".into(), JsValue::String(cwd));
    out.insert("strategy".into(), JsValue::String(strategy.clone()));
    let mut metadata = JsObject::new();
    if let Some(raw) = object.get("metadata").and_then(JsValue::as_object) {
        for (key, value) in raw.entries() {
            if let JsValue::String(text) = value {
                metadata.insert(key.clone(), JsValue::String(text.clone()));
            }
        }
    }
    out.insert("metadata".into(), JsValue::Object(metadata));
    JsValue::Object(out)
}

/// `normalizeState` (DiscoveryStateStore.ts:161-180): a non-object, or a
/// `schemaVersion` other than 1, reads as the default; otherwise the state is
/// rebuilt in the canonical key order, with every key present (absent ones as
/// `undefined` slots so later assignments update in place).
pub(crate) fn normalize_state(value: &JsValue, now_ms: f64) -> JsObject {
    let Some(candidate) = value.as_object() else {
        return default_state(now_ms);
    };
    if candidate.get("schemaVersion").and_then(JsValue::as_number) != Some(1.0) {
        return default_state(now_ms);
    }
    let outcome = candidate
        .get("lastFireOutcome")
        .and_then(JsValue::as_string)
        .filter(|outcome| {
            outcome.equals_str("executed")
                || outcome.equals_str("no_plan")
                || outcome.equals_str("failed")
                || outcome.equals_str("aborted")
        });
    let string_or_undefined =
        |value: Option<JsString>| value.map(JsValue::String).unwrap_or(JsValue::Undefined);
    let mut out = JsObject::new();
    out.insert("schemaVersion".into(), JsValue::Number(1.0));
    for (key, value) in [
        (
            "lastFireStartedAt",
            string_or_undefined(as_string(candidate.get("lastFireStartedAt"))),
        ),
        (
            "lastFireCompletedAt",
            string_or_undefined(as_string(candidate.get("lastFireCompletedAt"))),
        ),
        ("lastFireOutcome", string_or_undefined(outcome.cloned())),
        (
            "lastPlanId",
            string_or_undefined(as_string(candidate.get("lastPlanId"))),
        ),
        (
            "lastRunId",
            string_or_undefined(as_string(candidate.get("lastRunId"))),
        ),
        (
            "todayKey",
            JsValue::String(
                as_string(candidate.get("todayKey"))
                    .unwrap_or_else(|| JsString::from(day_key(now_ms))),
            ),
        ),
        (
            "todayRunCount",
            JsValue::Number(as_count(candidate.get("todayRunCount")).unwrap_or(0.0)),
        ),
        (
            "consecutiveFailures",
            JsValue::Number(as_count(candidate.get("consecutiveFailures")).unwrap_or(0.0)),
        ),
        ("dormant", normalize_dormant(candidate.get("dormant"))),
        (
            "activeWorkCycleId",
            string_or_undefined(as_string(candidate.get("activeWorkCycleId"))),
        ),
        (
            "currentWorkspace",
            normalize_current_workspace(candidate.get("currentWorkspace")),
        ),
    ] {
        out.insert(key.into(), value);
    }
    out
}

/// `resetDailyBudgetIfNeeded` (DiscoveryStateStore.ts:245-253): a stale
/// `todayKey` resets the run count on read.
fn reset_daily_budget(state: &mut JsObject, now_ms: f64) {
    let today = day_key(now_ms);
    let stale = state
        .get("todayKey")
        .and_then(JsValue::as_string)
        .is_none_or(|key| !key.equals_str(&today));
    if stale {
        state.insert("todayKey".into(), JsValue::String(JsString::from(today)));
        state.insert("todayRunCount".into(), JsValue::Number(0.0));
    }
}

/// The state store (DiscoveryStateStore.ts:17-18): one `state.json` per
/// project directory.
#[derive(Clone)]
pub(crate) struct DiscoveryStateStore {
    paths: AlwaysOnPaths,
}

impl DiscoveryStateStore {
    pub(crate) fn new(paths: AlwaysOnPaths) -> Self {
        Self { paths }
    }

    /// `read` (DiscoveryStateStore.ts:20-35): ENOENT and unparseable files
    /// read as the default; other io errors propagate, like the TS rethrow.
    /// The body decodes UTF-8 with replacement, like the Node `utf8` read —
    /// an invalid byte becomes U+FFFD, which fails `JSON.parse` and reads
    /// as the default, not an `InvalidData` error (`read_to_string`).
    pub(crate) fn read(&self, now_ms: f64) -> io::Result<JsObject> {
        let Some(raw) = super::storage::read(&super::storage::root_for(&self.paths.state_file), &self.paths.state_file, super::storage::INDEX_BYTES)? else { return Ok(default_state(now_ms)); };
        let raw = String::from_utf8_lossy(&raw);
        super::storage::check_json(&raw)?;
        let mut state = match parse_json(&raw) {
            Ok(parsed) => normalize_state(&parsed, now_ms),
            Err(_) => default_state(now_ms),
        };
        reset_daily_budget(&mut state, now_ms);
        Ok(state)
    }

    /// `write` (DiscoveryStateStore.ts:37-40): `mkdir -p` then the
    /// two-space `JSON.stringify` bytes, no trailing newline.
    pub(crate) fn write(&self, state: &JsObject) -> io::Result<()> {
        super::storage::check_object(state)?;
        let text = stringify_pretty(&JsValue::Object(state.clone()));
        super::storage::check_json(&text)?;
        super::storage::write(&super::storage::root_for(&self.paths.state_file), &self.paths.state_file, text.as_bytes(), super::storage::INDEX_BYTES)
    }

    pub(crate) async fn read_async(&self, now_ms: f64) -> io::Result<JsObject> {
        let store = self.clone();
        super::storage::run(move || store.read(now_ms)).await
    }

    pub(crate) async fn clear_dormant_async(&self, now_ms: f64) -> io::Result<JsObject> {
        let store = self.clone();
        super::storage::run(move || store.clear_dormant(now_ms)).await
    }

    fn set(&self, values: &[(&str, JsValue)], now_ms: f64) -> io::Result<JsObject> {
        let mut state = self.read(now_ms)?;
        for (key, value) in values {
            state.insert((*key).into(), value.clone());
        }
        self.write(&state)?;
        Ok(state)
    }

    /// `markFireStarted` (DiscoveryStateStore.ts:42-53).
    pub(crate) fn mark_fire_started(&self, run_id: &str, now_ms: f64) -> io::Result<JsObject> {
        let count = self
            .read(now_ms)?
            .get("todayRunCount")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        self.set(
            &[
                (
                    "lastFireStartedAt",
                    JsValue::String(JsString::from(iso_from_epoch_ms(now_ms))),
                ),
                ("lastRunId", JsValue::String(JsString::from(run_id))),
                ("todayKey", JsValue::String(JsString::from(day_key(now_ms)))),
                ("todayRunCount", JsValue::Number(count + 1.0)),
            ],
            now_ms,
        )
    }

    /// `markFireCompleted` (DiscoveryStateStore.ts:55-70). `lastPlanId` rides
    /// only when the fire produced a plan — a `undefined` slot is
    /// JSON-invisible, like the TS literal's absent value.
    pub(crate) fn mark_fire_completed(
        &self,
        outcome: &str,
        run_id: &str,
        plan_id: Option<&str>,
        now_ms: f64,
    ) -> io::Result<JsObject> {
        let plan_id = plan_id.map(JsString::from);
        self.mark_fire_completed_js(outcome, run_id, plan_id.as_ref(), now_ms)
    }

    pub(crate) fn mark_fire_completed_js(
        &self,
        outcome: &str,
        run_id: &str,
        plan_id: Option<&JsString>,
        now_ms: f64,
    ) -> io::Result<JsObject> {
        let failures = self
            .read(now_ms)?
            .get("consecutiveFailures")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let mut values = vec![
            (
                "lastFireCompletedAt",
                JsValue::String(JsString::from(iso_from_epoch_ms(now_ms))),
            ),
            ("lastFireOutcome", JsValue::String(JsString::from(outcome))),
            ("lastRunId", JsValue::String(JsString::from(run_id))),
        ];
        if let Some(plan_id) = plan_id {
            values.push(("lastPlanId", JsValue::String(plan_id.clone())));
        }
        values.push((
            "consecutiveFailures",
            JsValue::Number(if outcome == "failed" {
                failures + 1.0
            } else {
                0.0
            }),
        ));
        self.set(&values, now_ms)
    }

    /// `setActiveWorkCycleId` (DiscoveryStateStore.ts:72-80): the cycle id
    /// replaces any workspace ref.
    pub(crate) fn set_active_work_cycle_id(
        &self,
        cycle_id: &str,
        now_ms: f64,
    ) -> io::Result<JsObject> {
        self.set(
            &[
                (
                    "activeWorkCycleId",
                    JsValue::String(JsString::from(cycle_id)),
                ),
                ("currentWorkspace", JsValue::Undefined),
            ],
            now_ms,
        )
    }

    /// `clearActiveWorkCycleId` (DiscoveryStateStore.ts:82-92): a state
    /// without one is returned untouched, without a write.
    pub(crate) fn clear_active_work_cycle_id(&self, now_ms: f64) -> io::Result<JsObject> {
        let state = self.read(now_ms)?;
        if state
            .get("activeWorkCycleId")
            .is_none_or(JsValue::is_undefined)
        {
            return Ok(state);
        }
        self.set(&[("activeWorkCycleId", JsValue::Undefined)], now_ms)
    }

    /// `setDormant` (DiscoveryStateStore.ts:94-103).
    pub(crate) fn set_dormant(&self, now_ms: f64) -> io::Result<JsObject> {
        let iso = JsValue::String(JsString::from(iso_from_epoch_ms(now_ms)));
        let mut dormant = JsObject::new();
        dormant.insert("since".into(), iso.clone());
        dormant.insert("lastBaselineAt".into(), iso);
        self.set(&[("dormant", JsValue::Object(dormant))], now_ms)
    }

    /// `clearDormant` (DiscoveryStateStore.ts:105-114): a non-dormant state
    /// is returned untouched, without a write.
    pub(crate) fn clear_dormant(&self, now_ms: f64) -> io::Result<JsObject> {
        let state = self.read(now_ms)?;
        if state.get("dormant").is_none_or(JsValue::is_undefined) {
            return Ok(state);
        }
        self.set(&[("dormant", JsValue::Undefined)], now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::path::PathBuf;

    const GOLDEN: &str = include_str!("../../../tests/fixtures/always-on-2294/oracle.json");

    /// The golden record for one scenario id, as a serde value.
    fn golden(id: &str) -> serde_json::Value {
        let records: Vec<serde_json::Value> =
            serde_json::from_str(GOLDEN).expect("#2294 golden parses");
        records
            .iter()
            .find(|record| record["id"].as_str() == Some(id))
            .map(|record| record["value"].clone())
            .unwrap_or_else(|| panic!("#2294 golden record {id} missing"))
    }

    /// `JSON.stringify(state)` — the compact form the probe emitted for the
    /// read-back objects; undefined slots drop out.
    fn compact(state: &JsObject) -> serde_json::Value {
        let text = pilotdeck_protocol::stringify_json(&JsValue::Object(state.clone()))
            .expect("#2294 state serializes")
            .expect("#2294 state is defined");
        serde_json::from_str(&text).expect("#2294 state re-parses")
    }

    fn write_json(path: &PathBuf, value: &serde_json::Value) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("#2294 fixture dir");
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(value).expect("#2294 fixture json"),
        )
        .expect("#2294 fixture write");
    }

    fn read_json(path: &PathBuf) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("#2294 state file"))
            .expect("#2294 state parses")
    }

    // NOW = 2026-01-01T00:05:00.000Z, the probe's fixed clock.
    const NOW_MS: f64 = 1_767_225_900_000.0;

    fn store(tag: &str) -> (PathBuf, DiscoveryStateStore) {
        let home = std::env::temp_dir().join(format!("always-on-2294-state-{tag}"));
        let project_dir = home.join("always-on").join("projects").join("x");
        let _ = std::fs::remove_dir_all(&home);
        let paths = AlwaysOnPaths {
            project_key: "/proj/a".into(),
            state_file: project_dir.join("state.json"),
            plans_dir: project_dir.join("plans"),
            plan_index_file: project_dir.join("plans").join("index.json"),
            cycle_index_file: project_dir.join("cycles").join("index.json"),
            reports_dir: project_dir.join("reports"),
            discovery_lock_file: project_dir.join("locks").join("discovery.lock"),
        };
        (home, DiscoveryStateStore::new(paths))
    }

    #[test]
    fn completed_plan_identity_preserves_unpaired_utf16() {
        let (_home, store) = store("completed-js-id");
        let id = JsString::from_utf16(vec![b'p' as u16, 0xd800]);
        store.mark_fire_completed_js("executed", "run", Some(&id), NOW_MS).unwrap();
        let restored = store.read(NOW_MS).unwrap();
        assert_eq!(restored.get("lastPlanId").and_then(JsValue::as_string), Some(&id));
    }

    // The s1-s10 sequence replays the TS probe byte for byte: the default
    // state, the markFire* key-order dynamics, dormancy, the cycle id, the
    // daily reset and the malformed/wrong-schema fallbacks.
    #[test]
    fn replays_the_state_store_golden_sequence() {
        let (_home, store) = store("sequence");
        negative_assert_eq!(
            compact(&store.read(NOW_MS).expect("#2294 read")),
            golden("s1-default"),
            "#2294 s1-default: ENOENT reads as the four-key default"
        );
        store
            .mark_fire_started("run-1", NOW_MS)
            .expect("#2294 markFireStarted");
        negative_assert_eq!(
            read_json(&store.paths.state_file),
            golden("s2-markFireStarted"),
            "#2294 s2-markFireStarted: new keys append after the default's four"
        );
        store
            .mark_fire_completed("failed", "run-1", None, NOW_MS)
            .expect("#2294 markFireCompleted");
        negative_assert_eq!(
            read_json(&store.paths.state_file),
            golden("s3-markFireCompleted"),
            "#2294 s3-markFireCompleted: a disk read normalizes to canonical order first"
        );
        store.set_dormant(NOW_MS).expect("#2294 setDormant");
        negative_assert_eq!(
            read_json(&store.paths.state_file),
            golden("s4-setDormant"),
            "#2294 s4-setDormant"
        );
        store.clear_dormant(NOW_MS).expect("#2294 clearDormant");
        negative_assert_eq!(
            read_json(&store.paths.state_file),
            golden("s5-clearDormant"),
            "#2294 s5-clearDormant"
        );
        store
            .set_active_work_cycle_id("cycle-1", NOW_MS)
            .expect("#2294 setActiveWorkCycleId");
        negative_assert_eq!(
            read_json(&store.paths.state_file),
            golden("s6-activeCycle"),
            "#2294 s6-activeCycle"
        );
        write_json(
            &store.paths.state_file,
            &serde_json::json!({"schemaVersion": 1, "todayKey": "2025-12-31", "todayRunCount": 5, "consecutiveFailures": 2}),
        );
        negative_assert_eq!(
            compact(&store.read(NOW_MS).expect("#2294 read")),
            golden("s7-daily-reset"),
            "#2294 s7-daily-reset: a stale todayKey resets the run count on read"
        );
        std::fs::write(&store.paths.state_file, "{oops").expect("#2294 malformed fixture");
        negative_assert_eq!(
            compact(&store.read(NOW_MS).expect("#2294 read")),
            golden("s8-malformed"),
            "#2294 s8-malformed: an unparseable file reads as the default"
        );
        // Invalid UTF-8 decodes with U+FFFD replacement like the Node
        // `utf8` read, so the body stays unparseable and reads as the
        // default instead of surfacing an io error.
        std::fs::write(&store.paths.state_file, [0xff, b'{', b'}']).expect("#2294 non-utf8");
        negative_assert_eq!(
            compact(&store.read(NOW_MS).expect("#2294 read")),
            golden("s8-malformed"),
            "#2294 s8b-non-utf8: invalid bytes read as the default, not an error"
        );
        write_json(
            &store.paths.state_file,
            &serde_json::json!({"schemaVersion": 2, "todayRunCount": 9}),
        );
        negative_assert_eq!(
            compact(&store.read(NOW_MS).expect("#2294 read")),
            golden("s9-wrong-schema"),
            "#2294 s9-wrong-schema: schemaVersion 2 reads as the default"
        );
        let _ = std::fs::remove_file(&store.paths.state_file);
        store
            .mark_fire_started("run-bytes", NOW_MS)
            .expect("#2294 markFireStarted");
        let bytes =
            String::from_utf8(std::fs::read(&store.paths.state_file).expect("#2294 state bytes"))
                .expect("#2294 state utf8");
        negative_assert_eq!(
            serde_json::Value::String(bytes),
            golden("s10-markFireStarted-bytes")["bytes"],
            "#2294 s10-markFireStarted-bytes: exact write bytes, no trailing newline"
        );
    }

    // The ENOENT default carries no undefined placeholder slots, so the first
    // `markFireStarted` appends (s2); a disk read carries all twelve (s3). The
    // negative control for this pair is the s3 byte comparison itself — a
    // rebuild that drops the placeholders would order the s3 keys like s2.
    #[test]
    fn mark_fire_completed_recounts_consecutive_failures() {
        let (_home, store) = store("failures");
        store.mark_fire_started("r", NOW_MS).expect("#2294 start");
        store
            .mark_fire_completed("executed", "r", Some("p1"), NOW_MS)
            .expect("#2294 completed");
        let state = store.read(NOW_MS).expect("#2294 read");
        negative_assert_eq!(
            state
                .get("consecutiveFailures")
                .and_then(JsValue::as_number),
            Some(0.0),
            "#2294 an executed outcome resets the failure count"
        );
        negative_assert_eq!(
            state
                .get("lastPlanId")
                .and_then(JsValue::as_string)
                .map(|v| String::from_utf16_lossy(v.units())),
            Some("p1".into()),
            "#2294 lastPlanId rides when the fire produced a plan"
        );
    }

    // DiscoveryStateStore.ts:82-114: clear ops on an already-clear state
    // return without writing (the TS early return on the undefined field).
    #[test]
    fn clear_ops_on_clear_states_do_not_write() {
        let (_home, store) = store("clear");
        let before = store.read(NOW_MS).expect("#2294 read");
        let cleared = store
            .clear_active_work_cycle_id(NOW_MS)
            .expect("#2294 clear cycle");
        negative_assert_eq!(
            compact(&cleared),
            compact(&before),
            "#2294 clearActiveWorkCycleId on a state without one returns it untouched"
        );
        let cleared = store.clear_dormant(NOW_MS).expect("#2294 clear dormant");
        negative_assert_eq!(
            compact(&cleared),
            compact(&before),
            "#2294 clearDormant on a non-dormant state returns it untouched"
        );
        negative_assert_eq!(
            store.paths.state_file.exists(),
            false,
            "#2294 neither clear wrote the file"
        );
    }
}
