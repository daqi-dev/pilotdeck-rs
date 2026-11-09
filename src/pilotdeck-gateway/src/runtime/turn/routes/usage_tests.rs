use super::*;
use crate::{
    metadata::object,
    model_cost::{ModelPricing, PricingMap},
    runtime::model::router::{RouterModelRef, StatsConfig},
    test_scratch::Scratch,
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::path::Path;

const ORACLE: &str = include_str!("../../../../tests/fixtures/token-stats/oracle.json");

fn oracle_scenario(name: &str) -> JsValue {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let JsValue::Array(scenarios) = oracle.get("scenarios").unwrap() else {
        panic!("oracle scenarios");
    };
    scenarios
        .iter()
        .find(|scenario| {
            scenario
                .get("name")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str(name))
        })
        .unwrap()
        .clone()
}

fn config(raw: &JsValue, home: &Path) -> StatsConfig {
    let model_pricing = raw
        .get("modelPricing")
        .and_then(JsValue::as_object)
        .map(|pricing| {
            let mut result = PricingMap::new();
            for (name, rates) in pricing.entries() {
                let rate = |field: &str| rates.get(field).and_then(JsValue::as_number);
                result.insert(
                    name.to_utf8().unwrap(),
                    ModelPricing {
                        input: rate("input"),
                        output: rate("output"),
                        cache_read: rate("cacheRead"),
                    },
                );
            }
            result
        });
    StatsConfig {
        enabled: true,
        file_path: Some(home.join("stats.jsonl")),
        model_pricing,
        baseline_model: raw
            .get("baselineModel")
            .and_then(RouterModelRef::from_value),
        retention_ms: None,
    }
}

fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}

fn count(snapshot: &JsValue) -> f64 {
    snapshot
        .get("aggregate")
        .unwrap()
        .get("totalRequests")
        .and_then(JsValue::as_number)
        .unwrap()
}

fn first_record() -> JsValue {
    let scenario = oracle_scenario("aggregate-and-projection");
    let JsValue::Array(steps) = scenario.get("steps").unwrap() else {
        panic!("oracle steps");
    };
    steps[0].get("record").unwrap().clone()
}

#[test]
fn usage_routes_replay_real_ts_projections_and_recover_the_journal() {
    let home = Scratch::new("turn-usage-oracle");
    let scenario = oracle_scenario("aggregate-and-projection");
    let config = config(scenario.get("config").unwrap(), home.path());
    let stats = Arc::new(Mutex::new(TokenStatsCollector::new(Some(&config), None).unwrap()));
    let resolve = |project: Option<&JsString>| {
        assert!(project.is_some_and(|key| key.equals_str("project")));
        Ok(stats.clone())
    };
    let routes = UsageRoutes::new(&resolve);
    let JsValue::Array(steps) = scenario.get("steps").unwrap() else {
        panic!("oracle steps");
    };
    let mut checked = 0;
    for step in steps {
        let op = step.get("op").unwrap().as_string().unwrap();
        if op.equals_str("observe") {
            let mut record = step.get("record").unwrap().clone();
            stats.lock().unwrap().observe(&mut record);
        } else if op.equals_str("reopen") {
            let mut guard = stats.lock().unwrap();
            guard.dispose();
            *guard = TokenStatsCollector::new(Some(&config), None).unwrap();
        } else if op.equals_str("usageProjection") || op.equals_str("modelUsageProjection") {
            let params = object([
                ("projectKey", "project".into()),
                (
                    "sessionKey",
                    step.get("sessionKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            let method = if op.equals_str("usageProjection") {
                "usage_snapshot"
            } else {
                "model_usage_snapshot"
            };
            let actual = routes.handle(method, params).unwrap().unwrap();
            let expected = step.get("expected").unwrap().get("result").unwrap();
            assert_eq!(json(&actual), json(expected), "{method}");
            checked += 1;
        }
    }
    assert_eq!(checked, 5);
}

#[test]
fn enabled_empty_storage_returns_the_baseline_zero_projection() {
    let home = Scratch::new("turn-usage-empty");
    let config = config(&object([]), home.path());
    let stats = Arc::new(Mutex::new(TokenStatsCollector::new(Some(&config), None).unwrap()));
    assert!(stats.lock().unwrap().enabled());
    let resolve = |_: Option<&JsString>| Ok(stats.clone());
    let routes = UsageRoutes::new(&resolve);
    let scenario = oracle_scenario("disabled");
    let JsValue::Array(steps) = scenario.get("steps").unwrap() else {
        panic!("oracle steps");
    };
    let expected = steps[5].get("expected").unwrap().get("result").unwrap();
    assert_eq!(
        json(&routes.usage_snapshot(object([])).unwrap()),
        json(expected)
    );
    assert_eq!(
        json(&routes.model_usage_snapshot(object([])).unwrap()),
        r#"{"scope":"project","models":[]}"#
    );
    assert!(routes.handle("another_method", JsValue::Null).is_none());
}

#[test]
fn shared_observations_remain_visible_and_missing_sessions_keep_ts_fallbacks() {
    let home = Scratch::new("turn-usage-shared");
    let config = config(&object([]), home.path());
    let stats = Arc::new(Mutex::new(TokenStatsCollector::new(Some(&config), None).unwrap()));
    let resolve = |_: Option<&JsString>| Ok(stats.clone());
    let routes = UsageRoutes::new(&resolve);
    assert_eq!(count(&routes.usage_snapshot(object([])).unwrap()), 0.0);
    let mut record = first_record();
    stats.lock().unwrap().observe(&mut record);
    assert_eq!(count(&routes.usage_snapshot(object([])).unwrap()), 1.0);
    let missing = object([("sessionKey", "missing".into())]);
    let usage = routes.usage_snapshot(missing.clone()).unwrap();
    assert_eq!(usage.get("scope"), Some(&"project".into()));
    assert_eq!(count(&usage), 1.0);
    assert_eq!(
        json(&routes.model_usage_snapshot(missing).unwrap()),
        r#"{"scope":"project","models":[]}"#
    );
    let project = routes.model_usage_snapshot(object([])).unwrap();
    for params in [
        object([("sessionKey", "".into())]),
        object([("sessionKey", JsValue::Null)]),
    ] {
        assert_eq!(
            json(&routes.model_usage_snapshot(params).unwrap()),
            json(&project)
        );
    }
}

#[test]
fn resolver_failures_and_invalid_keys_do_not_become_zero_success() {
    let resolve = |_: Option<&JsString>| Err(RequestError::new("project unavailable"));
    let routes = UsageRoutes::new(&resolve);
    for method in ["usage_snapshot", "model_usage_snapshot"] {
        let error = routes.handle(method, object([])).unwrap().unwrap_err();
        assert!(error.message.equals_str("project unavailable"));
        let error = routes.handle(method, JsValue::Null).unwrap().unwrap_err();
        assert!(
            error
                .message
                .equals_str("Cannot read properties of null (reading 'projectKey')")
        );
        let error = routes
            .handle(method, parse_json(r#"{"projectKey":42}"#).unwrap())
            .unwrap()
            .unwrap_err();
        assert!(
            error
                .message
                .equals_str("projectKey must be a string when provided.")
        );
    }
}

#[test]
fn production_registry_reuses_normalized_project_state_and_recovers_existing_path() {
    let root = Scratch::new("turn-usage-registry");
    let home = root.path().join("home");
    let cwd = root.path().join("project");
    let make = || UsageRegistry::new(home.clone(), JsString::from(cwd.to_string_lossy().as_ref()));
    let registry = Arc::new(make());
    let stats = registry.resolve(None).unwrap();
    assert!(stats.lock().unwrap().enabled());
    assert!(home.join("router/stats.jsonl").is_file());
    for key in ["", ".", "nested/.."] {
        assert!(Arc::ptr_eq(
            &stats,
            &registry.resolve(Some(&key.into())).unwrap()
        ));
    }
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let registry = registry.clone();
            std::thread::spawn(move || registry.resolve(None).unwrap())
        })
        .collect();
    for thread in threads {
        assert!(Arc::ptr_eq(&stats, &thread.join().unwrap()));
    }
    let resolve = |project: Option<&JsString>| registry.resolve(project);
    let routes = UsageRoutes::new(&resolve);
    assert_eq!(count(&routes.usage_snapshot(object([])).unwrap()), 0.0);
    let mut record = first_record();
    stats.lock().unwrap().observe(&mut record);
    let usage = routes
        .usage_snapshot(object([("sessionKey", "s1".into())]))
        .unwrap();
    let models = routes
        .model_usage_snapshot(object([("sessionKey", "s1".into())]))
        .unwrap();
    assert_eq!(count(&usage), 1.0);
    let recovered = make();
    let resolve = |project: Option<&JsString>| recovered.resolve(project);
    let recovered_routes = UsageRoutes::new(&resolve);
    assert_eq!(
        json(
            &recovered_routes
                .usage_snapshot(object([("sessionKey", "s1".into())]))
                .unwrap()
        ),
        json(&usage)
    );
    assert_eq!(
        json(
            &recovered_routes
                .model_usage_snapshot(object([("sessionKey", "s1".into())]))
                .unwrap()
        ),
        json(&models)
    );
    // Distinct project owners retain TS's shared-journal recovery behavior.
    let other = registry.resolve(Some(&"other-project".into())).unwrap();
    assert!(!Arc::ptr_eq(&stats, &other));
    assert_eq!(
        json(
            &other
                .lock()
                .unwrap()
                .usage_snapshot_for_sdk(Some(&"s1".into()))
        ),
        json(&usage)
    );
}

#[test]
fn production_registry_does_not_suppress_legacy_json_migration() {
    let root = Scratch::new("turn-usage-migrate");
    let home = root.path().join("home");
    let router = home.join("router");
    std::fs::create_dir_all(&router).unwrap();
    let scenario = oracle_scenario("json-migration");
    let initial = scenario.get("initial").unwrap().as_object().unwrap();
    for (name, contents) in initial.entries() {
        std::fs::write(
            router.join(name.to_utf8().unwrap()),
            contents.as_string().unwrap().to_utf8().unwrap(),
        )
        .unwrap();
    }
    let registry = UsageRegistry::new(
        home.clone(),
        JsString::from(root.path().to_string_lossy().as_ref()),
    );
    let stats = registry.resolve(None).unwrap();
    let JsValue::Array(steps) = scenario.get("steps").unwrap() else {
        panic!("oracle steps");
    };
    let expected = steps
        .iter()
        .find(|step| {
            step.get("op")
                .and_then(JsValue::as_string)
                .is_some_and(|op| op.equals_str("snapshot"))
        })
        .unwrap()
        .get("expected")
        .unwrap()
        .get("result")
        .unwrap();
    assert_eq!(json(&stats.lock().unwrap().snapshot()), json(expected));
    assert!(router.join("stats.jsonl").is_file());
}

#[test]
fn production_registry_reports_a_nonfile_journal_and_poisoned_stats() {
    let root = Scratch::new("turn-usage-storage-error");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join("router/stats.jsonl")).unwrap();
    let registry = UsageRegistry::new(home, JsString::from(root.path().to_string_lossy().as_ref()));
    let error = match registry.resolve(None) {
        Err(error) => error,
        Ok(_) => panic!("directory journal must not return zero statistics"),
    };
    assert!(
        error
            .message
            .equals_str("Router statistics journal is not a regular file.")
    );
    let registry = UsageRegistry::new(
        root.path().join("healthy"),
        JsString::from(root.path().to_string_lossy().as_ref()),
    );
    let stats = registry.resolve(None).unwrap();
    let worker = stats.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = worker.lock().unwrap();
            panic!("poison statistics");
        })
        .join()
        .is_err()
    );
    let resolve = |project: Option<&JsString>| registry.resolve(project);
    let routes = UsageRoutes::new(&resolve);
    for method in ["usage_snapshot", "model_usage_snapshot"] {
        let error = routes.handle(method, object([])).unwrap().unwrap_err();
        assert!(
            error
                .message
                .equals_str("Router statistics lock is poisoned.")
        );
    }
}

#[test]
fn snapshot_configuration_keeps_the_shared_owner_pricing_and_enabled_semantics() {
    let root = Scratch::new("turn-usage-snapshot");
    let registry = UsageRegistry::new(
        root.path().join("home"),
        JsString::from(root.path().to_string_lossy().as_ref()),
    );
    let snapshot = parse_json(
        r#"{"config":{"router":{"stats":{"modelPricing":{"anthropic/claude-sonnet-4-5-20250929":{"input":1,"output":2,"cacheRead":0.5}}}},"agent":{"model":{"provider":"anthropic","model":"claude-sonnet-4-5-20250929"}},"model":{"providers":{"anthropic":{"models":{"claude-sonnet-4-5-20250929":{}}}}}}}"#,
    )
    .unwrap();
    let initial = registry.resolve(None).unwrap();
    let configured = registry.resolve_with_snapshot(None, &snapshot).unwrap();
    assert!(Arc::ptr_eq(&initial, &configured));
    let mut record = first_record();
    configured.lock().unwrap().observe(&mut record);
    assert_eq!(record.get("costSource"), Some(&"configured_price".into()));
    let expected = configured.lock().unwrap().usage_snapshot_for_sdk(None);
    assert_eq!(count(&expected), 1.0);
    assert!(Arc::ptr_eq(
        &configured,
        &registry.resolve_with_snapshot(None, &snapshot).unwrap()
    ));
    assert_eq!(
        json(
            &registry
                .resolve(None)
                .unwrap()
                .lock()
                .unwrap()
                .usage_snapshot_for_sdk(None)
        ),
        json(&expected)
    );
    for disabled in [
        r#"{"config":{}}"#,
        r#"{"config":{"router":null}}"#,
        r#"{"config":{"router":{"enabled":false,"stats":{"enabled":true}}}}"#,
        r#"{"config":{"router":{"stats":{"enabled":false}}}}"#,
    ] {
        let disabled = parse_json(disabled).unwrap();
        let stats = registry.resolve_with_snapshot(None, &disabled).unwrap();
        assert!(Arc::ptr_eq(&configured, &stats));
        assert!(!stats.lock().unwrap().enabled());
        let mut ignored = first_record();
        stats.lock().unwrap().observe(&mut ignored);
        assert_eq!(
            count(&stats.lock().unwrap().usage_snapshot_for_sdk(None)),
            0.0
        );
    }
    let restored = registry.resolve_with_snapshot(None, &snapshot).unwrap();
    assert!(Arc::ptr_eq(&configured, &restored));
    assert_eq!(
        json(&restored.lock().unwrap().usage_snapshot_for_sdk(None)),
        json(&expected)
    );
    let cached = registry.projects.lock().unwrap();
    let config = &cached.values().next().unwrap().config;
    assert_eq!(
        config
            .baseline_model
            .as_ref()
            .map(|model| model.provider.clone()),
        Some("anthropic".into())
    );
    drop(cached);
    let retained = parse_json(r#"{"config":{"router":{"stats":{"retentionMs":1}}}}"#).unwrap();
    let stats = registry.resolve_with_snapshot(None, &retained).unwrap();
    assert!(Arc::ptr_eq(&restored, &stats));
    assert_eq!(
        count(&stats.lock().unwrap().usage_snapshot_for_sdk(None)),
        0.0
    );
}
