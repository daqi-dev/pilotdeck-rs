//! Port of TS `src/always-on/runtime/DiscoveryGates.ts` — pure gate
//! evaluation, fixed order, first failing gate wins
//! (`02-pilotdeck-always-on-rewrite-plan.md` §11). The lease list is consumed
//! only as a *reverse* signal: an empty list never blocks a fire; presence of
//! a busy/recent lease does. Workspace single-instance is the fire's
//! `ensureWorkspace`, not this gate.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::parse_node22_date;

use super::{AlwaysOnConfig, ChannelLease};

/// `GateBlockReason` (`src/always-on/protocol/types.ts`), with the scheduler's
/// `cycle_full` cast (`"cycle_full" as GateBlockReason`,
/// DiscoveryScheduler.ts:129) folded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateBlockReason {
    ProjectDisabled,
    ProjectMissing,
    DormantNoSignal,
    AgentBusy,
    RecentUserMsg,
    Cooldown,
    DailyBudget,
    LockBusy,
    CycleFull,
}
impl GateBlockReason {
    /// The wire string the log metadata and the tick outcome carry.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ProjectDisabled => "project_disabled",
            Self::ProjectMissing => "project_missing",
            Self::DormantNoSignal => "dormant_no_signal",
            Self::AgentBusy => "agent_busy",
            Self::RecentUserMsg => "recent_user_msg",
            Self::Cooldown => "cooldown",
            Self::DailyBudget => "daily_budget",
            Self::LockBusy => "lock_busy",
            Self::CycleFull => "cycle_full",
        }
    }
}

/// `DiscoveryGateInput` (DiscoveryGates.ts:6-17).
pub(crate) struct GateInput<'a> {
    pub project_key: &'a str,
    pub config: &'a AlwaysOnConfig,
    /// The normalized discovery state (`read` output).
    pub state: &'a JsObject,
    /// The fresh lease list — the gate does not filter staleness; that is the
    /// scheduler's `listFresh` (golden `g16`).
    pub leases: &'a [ChannelLease],
    pub now_ms: f64,
    pub project_exists: bool,
    pub lock_held: bool,
    pub session_in_flight: Option<bool>,
}

/// `GateResult` (protocol/types.ts): `ok` plus the reason or the picked lease.
#[derive(Clone, Debug)]
pub(crate) struct GateResult {
    pub ok: bool,
    pub reason: Option<GateBlockReason>,
    pub lease: Option<ChannelLease>,
}

fn blocked(reason: GateBlockReason) -> GateResult {
    GateResult {
        ok: false,
        reason: Some(reason),
        lease: None,
    }
}

/// `pickMostRecentLastUserMsgMs` (DiscoveryGates.ts:102-112): the newest
/// parseable `lastUserMsgAt`; unparseable ones read as `NaN` and lose.
fn pick_most_recent_last_user_msg_ms(leases: &[&ChannelLease]) -> Option<f64> {
    leases
        .iter()
        .filter_map(|lease| {
            let stamp = lease.last_user_msg_at.as_deref()?;
            parse_node22_date(&JsString::from(stamp))
                .ok()
                .flatten()
                .filter(|ms| ms.is_finite())
        })
        .fold(None, |best: Option<f64>, ms| {
            Some(match best {
                Some(best) if best >= ms => best,
                _ => ms,
            })
        })
}

/// `evaluateAlwaysOnDiscoveryGates` (DiscoveryGates.ts:26-99). Order:
/// project_disabled → project_missing → dormant_no_signal → agent_busy
/// (session in flight, then a busy lease) → recent_user_msg → cooldown →
/// daily_budget → lock_busy; ok picks the preferred-channel lease or the
/// first fresh one, and an empty lease list is still ok.
pub(crate) fn evaluate_always_on_discovery_gates(input: &GateInput<'_>) -> GateResult {
    let GateInput {
        project_key,
        config,
        state,
        leases,
        now_ms,
        project_exists,
        lock_held,
        session_in_flight,
    } = input;

    if !config.project(project_key).unwrap_or(false) {
        return blocked(GateBlockReason::ProjectDisabled);
    }
    if !*project_exists {
        return blocked(GateBlockReason::ProjectMissing);
    }
    // `if (state.dormant)` — JS truthiness. The store's normalize only ever
    // leaves an object or an undefined placeholder, but a `null` on disk
    // reads falsy in TS, so it must not block here either.
    if !matches!(
        state.get("dormant"),
        None | Some(JsValue::Undefined) | Some(JsValue::Null)
    ) {
        return blocked(GateBlockReason::DormantNoSignal);
    }

    let fresh: Vec<&ChannelLease> = leases
        .iter()
        .filter(|lease| lease.project_key == *project_key)
        .collect();

    if *session_in_flight == Some(true) {
        return blocked(GateBlockReason::AgentBusy);
    }
    if !fresh.is_empty() {
        if fresh.iter().any(|lease| lease.agent_busy) {
            return blocked(GateBlockReason::AgentBusy);
        }
        let recent_ms = config.recent_user_msg_minutes * 60_000.0;
        if let Some(last_user_ms) = pick_most_recent_last_user_msg_ms(&fresh)
            && now_ms - last_user_ms < recent_ms
        {
            return blocked(GateBlockReason::RecentUserMsg);
        }
    }

    let cooldown_ms = config.cooldown_minutes * 60_000.0;
    if let Some(completed) = state
        .get("lastFireCompletedAt")
        .and_then(JsValue::as_string)
        .and_then(|stamp| parse_node22_date(stamp).ok().flatten())
        && now_ms - completed < cooldown_ms
    {
        return blocked(GateBlockReason::Cooldown);
    }

    let today_run_count = state
        .get("todayRunCount")
        .and_then(JsValue::as_number)
        .unwrap_or(0.0);
    if today_run_count >= config.daily_budget {
        return blocked(GateBlockReason::DailyBudget);
    }

    if *lock_held {
        return blocked(GateBlockReason::LockBusy);
    }

    if fresh.is_empty() {
        return GateResult {
            ok: true,
            reason: None,
            lease: None,
        };
    }
    let target = fresh
        .iter()
        .find(|lease| lease.channel_key == config.prefer_channel)
        .unwrap_or(&fresh[0]);
    GateResult {
        ok: true,
        reason: None,
        lease: Some((*target).clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;

    const GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/always-on-2294/oracle.json"
    ));

    fn golden(id: &str) -> serde_json::Value {
        let records: Vec<serde_json::Value> =
            serde_json::from_str(GOLDEN).expect("#2294 golden parses");
        records
            .iter()
            .find(|record| record["id"].as_str() == Some(id))
            .map(|record| record["value"].clone())
            .unwrap_or_else(|| panic!("#2294 golden record {id} missing"))
    }

    // The probe's NOW (2026-01-01T00:05:00.000Z) and default trigger.
    const NOW_MS: f64 = 1_767_225_900_000.0;

    fn config(projects: &[(&str, bool)], trigger: &[(&str, f64)]) -> AlwaysOnConfig {
        let mut config = AlwaysOnConfig::default_config();
        config.projects = projects
            .iter()
            .map(|(key, enabled)| ((*key).into(), *enabled))
            .collect();
        for (key, value) in trigger {
            match *key {
                "tickIntervalMinutes" => config.tick_interval_minutes = *value,
                "cooldownMinutes" => config.cooldown_minutes = *value,
                "dailyBudget" => config.daily_budget = *value,
                "heartbeatStaleSeconds" => config.heartbeat_stale_seconds = *value,
                "recentUserMsgMinutes" => config.recent_user_msg_minutes = *value,
                _ => unreachable!("#2294 trigger key {key}"),
            }
        }
        config
    }

    /// The probe's `baseState` — the four-key default, optionally extended.
    fn state(extra: &[(&str, JsValue)]) -> JsObject {
        let mut state = JsObject::new();
        state.insert("schemaVersion".into(), JsValue::Number(1.0));
        state.insert(
            "todayKey".into(),
            JsValue::String(JsString::from("2026-01-01")),
        );
        state.insert("todayRunCount".into(), JsValue::Number(0.0));
        state.insert("consecutiveFailures".into(), JsValue::Number(0.0));
        for (key, value) in extra {
            state.insert((*key).into(), value.clone());
        }
        state
    }

    fn dormant_state() -> JsObject {
        state(&[(
            "dormant",
            JsValue::Object({
                let mut object = JsObject::new();
                object.insert(
                    "since".into(),
                    JsValue::String(JsString::from("2026-01-01T00:00:00.000Z")),
                );
                object.insert(
                    "lastBaselineAt".into(),
                    JsValue::String(JsString::from("2026-01-01T00:00:00.000Z")),
                );
                object
            }),
        )])
    }

    fn ok_lease(extra: &[(&str, String)]) -> ChannelLease {
        let mut lease = ChannelLease {
            schema_version: 1.0,
            written_at: "2026-01-01T00:04:30.000Z".into(),
            channel_key: "web".into(),
            writer_id: "w1".into(),
            project_key: "/proj/a".into(),
            session_key: "s1".into(),
            agent_busy: false,
            last_user_msg_at: None,
        };
        for (key, value) in extra {
            match *key {
                "channelKey" => lease.channel_key = value.clone(),
                "writerId" => lease.writer_id = value.clone(),
                "agentBusy" => lease.agent_busy = value == "true",
                "lastUserMsgAt" => {
                    lease.last_user_msg_at = if value.is_empty() {
                        None
                    } else {
                        Some(value.clone())
                    }
                }
                "writtenAt" => lease.written_at = value.clone(),
                _ => unreachable!("#2294 lease key {key}"),
            }
        }
        lease
    }

    fn evaluate(
        config: &AlwaysOnConfig,
        state: &JsObject,
        leases: &[ChannelLease],
        over: &[(&str, JsValue)],
    ) -> GateResult {
        // Owned before the input borrows it: `GateInput::project_key` is a
        // borrow, so an override must outlive the input it feeds.
        let override_key = over
            .iter()
            .find(|(key, _)| *key == "projectKey")
            .and_then(|(_, value)| value.as_string().and_then(|key| key.to_utf8().ok()));
        let mut input = GateInput {
            project_key: override_key.as_deref().unwrap_or("/proj/a"),
            config,
            state,
            leases,
            now_ms: NOW_MS,
            project_exists: true,
            lock_held: false,
            session_in_flight: Some(false),
        };
        for (key, value) in over {
            match *key {
                "projectKey" => {}
                "projectExists" => input.project_exists = value.as_bool().unwrap_or(true),
                "lockHeld" => input.lock_held = value.as_bool().unwrap_or(false),
                "sessionInFlight" => {
                    input.session_in_flight = match value {
                        JsValue::Bool(bit) => Some(*bit),
                        _ => None,
                    }
                }
                _ => unreachable!("#2294 input key {key}"),
            }
        }
        evaluate_always_on_discovery_gates(&input)
    }

    /// The gate result as the golden records captured it (`GateResult`
    /// serialized by the TS probe): `{ok, reason?}` or `{ok, lease}`.
    fn as_json(result: &GateResult) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        out.insert("ok".into(), serde_json::Value::Bool(result.ok));
        if let Some(reason) = result.reason {
            out.insert(
                "reason".into(),
                serde_json::Value::String(reason.as_str().into()),
            );
        }
        if let Some(lease) = &result.lease {
            let mut lease_json = serde_json::Map::new();
            lease_json.insert(
                "schemaVersion".into(),
                // JS `JSON.stringify(1)` writes an integer, not `1.0`.
                serde_json::json!(lease.schema_version as i64),
            );
            lease_json.insert(
                "writtenAt".into(),
                serde_json::Value::String(lease.written_at.clone()),
            );
            lease_json.insert(
                "channelKey".into(),
                serde_json::Value::String(lease.channel_key.clone()),
            );
            lease_json.insert(
                "writerId".into(),
                serde_json::Value::String(lease.writer_id.clone()),
            );
            lease_json.insert(
                "projectKey".into(),
                serde_json::Value::String(lease.project_key.clone()),
            );
            lease_json.insert(
                "sessionKey".into(),
                serde_json::Value::String(lease.session_key.clone()),
            );
            lease_json.insert(
                "agentBusy".into(),
                serde_json::Value::Bool(lease.agent_busy),
            );
            lease_json.insert(
                "lastUserMsgAt".into(),
                match &lease.last_user_msg_at {
                    Some(stamp) => serde_json::Value::String(stamp.clone()),
                    None => serde_json::Value::Null,
                },
            );
            out.insert("lease".into(), serde_json::Value::Object(lease_json));
        }
        serde_json::Value::Object(out)
    }

    // The g1-g16 replay: every gate, the first-fail-wins order, the strict
    // recent-message window and the lease pick, byte-identical to the TS
    // baseline records.
    #[test]
    fn replays_the_gate_chain_golden() {
        let enabled = [("/proj/a", true), ("/proj/off", false)];
        let head_config = config(&enabled, &[]);
        let base = state(&[]);
        let cases: Vec<(
            &str,
            AlwaysOnConfig,
            JsObject,
            Vec<ChannelLease>,
            Vec<(&str, JsValue)>,
        )> = vec![
            (
                "g1-key-not-in-config",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![("projectKey", JsValue::String(JsString::from("/not-listed")))],
            ),
            (
                "g2-disabled",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![("projectKey", JsValue::String(JsString::from("/proj/off")))],
            ),
            (
                "g3-missing",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![("projectExists", JsValue::Bool(false))],
            ),
            (
                "g4-dormant",
                head_config.clone(),
                dormant_state(),
                vec![],
                vec![],
            ),
            (
                "g5-session-in-flight",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![("sessionInFlight", JsValue::Bool(true))],
            ),
            (
                "g6-lease-agent-busy",
                head_config.clone(),
                base.clone(),
                vec![ok_lease(&[("agentBusy", "true".into())])],
                vec![],
            ),
            (
                "g7-recent-user-msg",
                head_config.clone(),
                base.clone(),
                vec![ok_lease(&[(
                    "lastUserMsgAt",
                    "2026-01-01T00:04:00.000Z".into(),
                )])],
                vec![],
            ),
            (
                "g7b-user-msg-exactly-window",
                head_config.clone(),
                base.clone(),
                vec![ok_lease(&[(
                    "lastUserMsgAt",
                    "2026-01-01T00:00:00.000Z".into(),
                )])],
                vec![],
            ),
            (
                "g8-cooldown",
                head_config.clone(),
                state(&[(
                    "lastFireCompletedAt",
                    JsValue::String(JsString::from("2026-01-01T00:04:00.000Z")),
                )]),
                vec![],
                vec![],
            ),
            (
                "g8b-cooldown-elapsed",
                config(&[("/proj/a", true)], &[("cooldownMinutes", 5.0)]),
                state(&[(
                    "lastFireCompletedAt",
                    JsValue::String(JsString::from("2026-01-01T00:00:00.000Z")),
                )]),
                vec![],
                vec![],
            ),
            (
                "g9-daily-budget",
                head_config.clone(),
                state(&[("todayRunCount", JsValue::Number(4.0))]),
                vec![],
                vec![],
            ),
            (
                "g10-lock-held",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![("lockHeld", JsValue::Bool(true))],
            ),
            (
                "g11-ok-no-leases",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![],
            ),
            (
                "g12-ok-preferred-lease",
                head_config.clone(),
                base.clone(),
                vec![
                    ok_lease(&[("channelKey", "cli".into())]),
                    ok_lease(&[("channelKey", "web".into()), ("writerId", "w2".into())]),
                ],
                vec![],
            ),
            (
                "g13-ok-fallback-first-lease",
                // The probe drove `preferChannel: 'zzz'` — a channel no lease
                // carries, so the pick falls back to `fresh[0]`.
                {
                    let mut config = config(&[("/proj/a", true)], &[]);
                    config.prefer_channel = "zzz".into();
                    config
                },
                base.clone(),
                vec![ok_lease(&[("channelKey", "cli".into())])],
                vec![],
            ),
            (
                "g14-first-fail-wins",
                head_config.clone(),
                base.clone(),
                vec![],
                vec![
                    ("projectKey", JsValue::String(JsString::from("/not-listed"))),
                    ("projectExists", JsValue::Bool(false)),
                    ("lockHeld", JsValue::Bool(true)),
                ],
            ),
            (
                "g15-order-missing-before-dormant",
                head_config.clone(),
                dormant_state(),
                vec![],
                vec![("projectExists", JsValue::Bool(false))],
            ),
            (
                "g16-lease-list-not-filtered",
                head_config.clone(),
                base.clone(),
                vec![ok_lease(&[
                    ("agentBusy", "true".into()),
                    ("writtenAt", "2025-12-01T00:00:00.000Z".into()),
                ])],
                vec![],
            ),
        ];
        for (id, config, state, leases, over) in cases {
            let result = evaluate(&config, &state, &leases, &over);
            negative_assert_eq!(
                as_json(&result),
                golden(id),
                "#2294 {id}: the gate chain answer matches the TS baseline"
            );
        }
    }

    // The state store's `read` output (undefined placeholders included) is a
    // valid gate input: the dormant check is the truthiness of the `dormant`
    // slot, and a placeholder reads falsy.
    #[test]
    fn normalized_states_drive_the_dormant_gate() {
        let config = config(&[("/proj/a", true)], &[]);
        let home = std::env::temp_dir().join("always-on-2294-gates-normalized");
        let _ = std::fs::remove_dir_all(&home);
        let paths = super::super::paths::AlwaysOnPaths {
            project_key: "/proj/a".into(),
            state_file: home.join("state.json"),
            plans_dir: home.join("plans"),
            plan_index_file: home.join("plans").join("index.json"),
            cycle_index_file: home.join("cycles").join("index.json"),
            reports_dir: home.join("reports"),
            discovery_lock_file: home.join("locks").join("discovery.lock"),
        };
        let store = super::super::state::DiscoveryStateStore::new(paths);
        let state = store.read(NOW_MS).expect("#2294 read");
        let result = evaluate(&config, &state, &[], &[]);
        negative_assert_eq!(
            result.ok,
            true,
            "#2294 a normalized non-dormant state passes the dormant gate"
        );
    }
}
