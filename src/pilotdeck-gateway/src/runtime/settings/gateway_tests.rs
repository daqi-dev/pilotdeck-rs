//! Replay of the real TS settings gateway oracle
//! (`tests/fixtures/settings-gateway/generate.mts`, #1318). Every matrix row
//! is one replay step: the same setup actions run against the same Rust
//! layers (the #1011 session owner, the real [`ConfigStore`] over a real
//! scratch YAML tree, the real [`RefreshBundle`] with recording
//! collaborators), then the reconstructed WS response frame, the event stream
//! and the post state must match the TS oracle rows (JSON structural
//! equality via `as_serde`: object key order is normalized by serde, so the
//! key-order bytes are not pinned).
//!
//! The mirror harness itself (the scenario probe, the registry / styles /
//! writer mirrors, the re-binding store delegate, the scratch-tree
//! `Harness`) is shared with #1320's native-route replay and lives in
//! [`super::test_support`].
//!
//! The TS golden's `assemblyCases` (a gateway assembled without the five
//! options answers CAPABILITY_UNAVAILABLE / the unsupported projection; an
//! unknown method throws "Unknown gateway method …") belong to the wiring
//! layer this component deliberately does not touch, so the capability half
//! is replayed through a bare component (the contract is the component's own
//! option handling) while the unknown-method row is recorded only; the
//! dispatch contract (`Some` for exactly the six methods, `None`
//! otherwise) has its own dedicated test below.
//!
//! The registry's `applyFlagSettingsForSdk` / `reloadOutputStylesForSdk` are
//! mirrored over the probe's map state (the TS bodies are synchronous Map
//! mutation; the mirror keeps the exact hasOwnProperty semantics), and
//! `updatePilotLocalSettings` is mirrored by the shared `MirrorWriter`: the
//! same allowlist validation, the same YAML document edit (flow maps
//! re-emitted in the canonical spaced form, blocks kept verbatim) and the
//! same atomic write, composed with the real store reload inside the adapter
//! exactly like the createLocalGateway closure.
use super::gateway::{SettingsError, SettingsGateway, SettingsGatewayOptions};
use super::test_support::{as_serde, field, harness, lock, scenario_keys, string};
use crate::config::store::ReloadEvent;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::sync::Arc;

const ORACLE: &str = include_str!("../../../tests/fixtures/settings-gateway/oracle.json");

/// The reconstructed WS response frame: `ok` carries the result, an error
/// carries `{code, message}` with the raw-throw fallback
/// `gateway_request_failed`, exactly like the WS layer's error frame.
fn envelope(result: Result<JsValue, SettingsError>) -> JsValue {
    match result {
        Ok(value) => {
            let mut object = JsObject::new();
            object.insert("type".into(), JsValue::String("response".into()));
            object.insert("ok".into(), JsValue::Bool(true));
            object.insert("result".into(), value);
            JsValue::Object(object)
        }
        Err(error) => {
            let mut error_object = JsObject::new();
            match error.code() {
                Some(code) => {
                    error_object.insert("code".into(), JsValue::String(code.clone()));
                }
                None => {
                    error_object.insert(
                        "code".into(),
                        JsValue::String("gateway_request_failed".into()),
                    );
                }
            }
            error_object.insert("message".into(), JsValue::String(error.message().clone()));
            let mut object = JsObject::new();
            object.insert("type".into(), JsValue::String("response".into()));
            object.insert("ok".into(), JsValue::Bool(false));
            object.insert("error".into(), JsValue::Object(error_object));
            JsValue::Object(object)
        }
    }
}

// ─────────────────────────────────────────────────────── the replay tests

#[tokio::test]
async fn settings_gateway_replays_the_real_ts_oracle() {
    let oracle = crate::test_baseline::oracle(ORACLE);
    let keys = scenario_keys(&oracle);
    let cases = match field(&oracle, "cases") {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1318 oracle must carry its case rows"),
    };
    assert_eq!(
        cases.len(),
        54,
        "#1318 the oracle must carry the full 54-row matrix"
    );
    let harness = harness(&keys);
    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let id = string(field(case, "id"), "case.id");
        let method = string(field(case, "method"), "case.method");
        for step in match field(case, "setup") {
            JsValue::Array(items) => items.clone(),
            _ => panic!("#1318 case {id} must carry a setup list"),
        } {
            harness.run_setup(&string(&step, "case.setup step")).await;
        }
        harness.probe.clear_events();
        let input = field(case, "params").clone();
        let dispatch = harness.gateway().dispatch(&method, &input).await;
        let result = dispatch.unwrap_or_else(|| {
            panic!("#1318 case {id}: dispatch must own the five settings methods")
        });
        // A divergent row does not stop the replay: every row is compared so
        // one run reports the whole matrix (the negative controls bind one
        // bound marker per matrix cell, so all of them must be reachable in
        // the single panic payload).
        let expected = as_serde(field(case, "response"));
        let actual = as_serde(&envelope(result));
        if actual != expected {
            failures.push(format!(
                "#1318 case {id}: the reconstructed WS response must match TS\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
        let expected_events = match field(case, "events") {
            JsValue::Array(items) => items
                .iter()
                .map(|event| string(event, "case.events entry"))
                .collect::<Vec<_>>(),
            _ => panic!("#1318 case {id} must carry an events list"),
        };
        let actual_events = harness.probe.events();
        if actual_events != expected_events {
            failures.push(format!(
                "#1318 case {id}: the event stream must match TS\n  expected: {expected_events:?}\n  actual:   {actual_events:?}"
            ));
        }
        let expected_post = as_serde(field(case, "post"));
        let actual_post = as_serde(&harness.post_state());
        if actual_post != expected_post {
            failures.push(format!(
                "#1318 case {id}: the post state must match TS\n  expected: {expected_post}\n  actual:   {actual_post}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "#1318 {} matrix row(s) diverged from the TS oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );

    // `assemblyCases.capability`: a gateway assembled without the five
    // options answers CAPABILITY_UNAVAILABLE for apply/update/styles and
    // the `{reloaded:false,reason:"unsupported"}` projection for the two
    // reloads — the component's own option handling, replayed through a
    // bare instance.
    let assembly = field(&oracle, "assemblyCases");
    let capability = match field(assembly, "capability") {
        JsValue::Array(items) => items.clone(),
        _ => panic!("#1318 oracle must carry its assemblyCases.capability list"),
    };
    let bare = SettingsGateway::new(SettingsGatewayOptions {
        owner: harness.probe.owner(),
        apply_flag_settings: None,
        update_settings: None,
        resolve_settings: None,
        reload_output_styles: None,
        reload_config: None,
        reload_extensions: None,
    });
    let input = parse_json(r#"{"sessionKey":"s"}"#)
        .expect("#1318 the capability assembly input must parse");
    for case in &capability {
        let method = string(field(case, "method"), "assemblyCases.capability.method");
        let result = bare
            .dispatch(&method, &input)
            .await
            .unwrap_or_else(|| panic!("#1318 the bare gateway still owns {method}"));
        let expected = as_serde(field(case, "response"));
        let actual = as_serde(&envelope(result));
        assert_eq!(
            actual, expected,
            "#1318 the bare assembly must answer {method} exactly like the TS WS layer"
        );
    }
    // The unknown-method row is the wiring layer's `Unknown gateway method`
    // error; for this component it must simply not be one of the five.
    let unknown = bare.dispatch("settings_probe", &input).await;
    assert!(
        unknown.is_none(),
        "#1318 dispatch must not own settings_probe; the WS layer answers Unknown gateway method"
    );
}

/// The dispatch entry owns exactly the six U7 methods; everything else is
/// the caller's business (the WS layer's unknown-method error and the
/// CAPABILITY_UNAVAILABLE answers live in the wiring layer, recorded in the
/// golden's `assemblyCases`).
#[tokio::test]
async fn settings_gateway_dispatch_returns_none_for_non_settings_methods() {
    let keys = vec!["s".to_string()];
    let harness = harness(&keys);
    // The owned methods run for real through the bundle's store delegate, so
    // the harness needs a live tree exactly like the TS assembly.
    harness.make_tree("t1", None).await;
    let input = parse_json(r#"{"sessionKey":"s"}"#).expect("#1318 dispatch input must parse");
    for method in [
        "settings_probe",
        "submit_turn",
        "set_mcp_servers",
        "apply_flag_settings_extra",
        "",
    ] {
        let dispatch = harness.gateway().dispatch(method, &input).await;
        assert!(
            dispatch.is_none(),
            "#1318 dispatch must not own non-settings method {method}"
        );
    }
    for method in [
        "apply_flag_settings",
        "update_settings",
        "reload_output_styles",
        "reload_config",
        "reload_extensions",
    ] {
        let dispatch = harness.gateway().dispatch(method, &input).await;
        assert!(
            dispatch.is_some(),
            "#1318 dispatch must own the settings method {method}"
        );
    }
}

/// The update closure's capture listener is detached `finally`-style,
/// including when the future is dropped at the reload await point (the WS
/// layer drops in-flight requests on disconnect) — the plain return-path
/// unsubscribe missed that window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_settings_detaches_the_capture_listener_when_dropped_mid_reload() {
    let harness = harness(&[]);
    harness.make_tree("t-drop", None).await;
    let store = lock(&harness.trees)
        .iter()
        .find(|(name, _)| name == "t-drop")
        .map(|(_, tree)| tree.store.clone())
        .expect("#1318-r1 update-drop: the scratch tree must be registered");
    let before = store.listener_count();
    // The first listener blocks publication (the flight stays Running), so
    // the update's own reload chains after it and parks at its await point.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(Some(release_rx));
    let blocker = store.subscribe(Arc::new(move |_: &ReloadEvent| {
        if let Some(rx) = lock(&release_rx).take() {
            let _ = entered_tx.send(());
            let _ = rx.recv_timeout(std::time::Duration::from_secs(30));
        }
    }));
    assert_eq!(
        store.listener_count(),
        before + 1,
        "#1318-r1 update-drop: the blocker must be the only new listener"
    );
    let first = tokio::spawn(store.reload("manual"));
    tokio::task::spawn_blocking(move || {
        entered_rx
            .recv()
            .expect("#1318-r1 update-drop: the blocker listener must run")
    })
    .await
    .expect("#1318-r1 update-drop: the gate join must succeed");
    let gateway = harness.gateway();
    let input = parse_json(
        r#"{"source":"localSettings","settings":{"agent":{"maxContextTokens":100000}}}"#,
    )
    .expect("#1318-r1 update-drop: the update input must parse");
    let mut future = Box::pin(gateway.update_settings(&input));
    let parked = tokio::select! {
        biased;
        result = &mut future => panic!(
            "#1318-r1 update-drop: update_settings must park at the chained reload, got {result:?}"
        ),
        _ = tokio::task::yield_now() => true,
    };
    assert!(parked, "#1318-r1 update-drop: the parking branch must win");
    assert_eq!(
        store.listener_count(),
        before + 2,
        "#1318-r1 update-drop: the capture listener must be subscribed while the reload is in flight"
    );
    drop(future);
    assert_eq!(
        store.listener_count(),
        before + 1,
        "#1318-r1 update-drop: dropping the future must detach the capture listener"
    );
    release_tx
        .send(())
        .expect("#1318-r1 update-drop: the blocker must be released");
    blocker.unsubscribe();
    first
        .await
        .expect("#1318-r1 update-drop: the first reload must join")
        .expect("#1318-r1 update-drop: the first reload must succeed");
    assert_eq!(
        store.listener_count(),
        before,
        "#1318-r1 update-drop: cleanup must leave only the probe listener"
    );
}

/// `reload_extensions` branches on the *original* JS truthiness of
/// `projectKey` (`if (input?.projectKey)`), measured on the real TS bundle in
/// `_tmp/20261026-issue-1318/fix-r2/probe-extensions-truthiness.out` (Node
/// v22.22.0; fix-r3 added the `[""]` / `[null]` cells in
/// `probe-extensions-truthiness-r3.out`): `0` / `false` / `NaN` / `""` /
/// `null` / omitted take the all-runtimes branch exactly like the nullish
/// values, while a truthy non-string key enters the project branch coerced by
/// `String()` — the registered divergence
/// `U7-EXTENSIONS-RELOAD-STRING-COERCION-001`. `[]` / `[""]` / `[null]` are
/// truthy in TS but coerce to `""`, so they still land on the all-runtimes
/// branch (those cells cannot converge before `ExtensionsReload` carries
/// `JsValue`, registered in the same ledger entry, #1320).
#[tokio::test]
async fn extensions_reload_branches_on_the_original_project_key_truthiness() {
    let harness = harness(&[]);
    let gateway = harness.gateway();
    let standard = as_serde(
        &parse_json(r#"{"reloaded":true,"changedPaths":[]}"#)
            .expect("#1318-r2 extensions-truthiness: the standard outcome must parse"),
    );
    let all_runtimes = || {
        vec![
            "log:[pilotdeck] Extensions reload requested for all runtimes: (manual)".to_owned(),
            "invalidate:null".to_owned(),
            "mark-all-dirty:extension_changed".to_owned(),
            r#"broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}"#
                .to_owned(),
        ]
    };
    let project = |key: &str| {
        vec![
            format!("log:[pilotdeck] Extensions reload requested for project {key}: (manual)"),
            format!("invalidate:{key}"),
            format!("mark-project-dirty:{key}:extension_changed"),
            r#"broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}"#
                .to_owned(),
        ]
    };
    let key_params = |value: JsValue| {
        let mut object = JsObject::new();
        object.insert("projectKey".into(), value);
        JsValue::Object(object)
    };
    let cells = [
        ("omitted", JsValue::Object(JsObject::new()), all_runtimes()),
        ("null", key_params(JsValue::Null), all_runtimes()),
        ("zero", key_params(JsValue::Number(0.0)), all_runtimes()),
        ("false", key_params(JsValue::Bool(false)), all_runtimes()),
        ("nan", key_params(JsValue::Number(f64::NAN)), all_runtimes()),
        (
            "empty-string",
            key_params(JsValue::String(JsString::from(""))),
            all_runtimes(),
        ),
        // Truthy in TS, but `String([])` is `""`: the registered divergence
        // keeps this cell on the all-runtimes branch until #1320.
        (
            "empty-array",
            key_params(JsValue::Array(Vec::new())),
            all_runtimes(),
        ),
        // Same family, measured on the real bundle in fix-r3
        // (`probe-extensions-truthiness-r3.out`): `[""]` and `[null]` are
        // truthy and their `String()` coercion is `""`, so TS takes the
        // project no-op branch while the empty-key filter routes the Rust
        // side back to the all-runtimes branch.
        (
            "empty-string-array",
            key_params(JsValue::Array(vec![JsValue::String(JsString::from(""))])),
            all_runtimes(),
        ),
        (
            "null-array",
            key_params(JsValue::Array(vec![JsValue::Null])),
            all_runtimes(),
        ),
        (
            "object",
            key_params(JsValue::Object(JsObject::new())),
            project("[object Object]"),
        ),
        ("five", key_params(JsValue::Number(5.0)), project("5")),
    ];
    for (cell, params, expected) in cells {
        let result = gateway
            .reload_extensions(&params)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "#1318-r2 extensions-truthiness: projectKey {cell} must succeed, got {error:?}"
                )
            });
        assert_eq!(
            as_serde(&result),
            standard,
            "#1318-r2 extensions-truthiness: projectKey {cell} must answer the standard outcome"
        );
        assert_eq!(
            harness.probe.events(),
            expected,
            "#1318-r2 extensions-truthiness: a JS-falsy projectKey must take the all-runtimes branch (projectKey {cell})"
        );
        harness.probe.clear_events();
    }
    // `changedPaths` entries are `String()`-coerced (the registered divergence;
    // TS keeps `[1, true, null]` as-is in the result and the broadcast).
    let mut object = JsObject::new();
    object.insert(
        "changedPaths".into(),
        JsValue::Array(vec![
            JsValue::Number(1.0),
            JsValue::Bool(true),
            JsValue::Null,
        ]),
    );
    let result = gateway
        .reload_extensions(&JsValue::Object(object))
        .await
        .expect("#1318-r2 extensions-truthiness: the changedPaths coercion call must succeed");
    assert_eq!(
        as_serde(&result),
        as_serde(
            &parse_json(r#"{"reloaded":true,"changedPaths":["1","true",""]}"#)
                .expect("#1318-r2 extensions-truthiness: the coercion expectation must parse")
        ),
        "#1318-r2 extensions-truthiness: changedPaths entries are String()-coerced (registered divergence U7-EXTENSIONS-RELOAD-STRING-COERCION-001)"
    );
    assert_eq!(
        harness.probe.events(),
        vec![
            "log:[pilotdeck] Extensions reload requested for all runtimes: 1, true, ".to_owned(),
            "invalidate:null".to_owned(),
            "mark-all-dirty:extension_changed".to_owned(),
            r#"broadcast:config_changed:{"changedPaths":["1","true",""],"changeClasses":["extension-changed"]}"#
                .to_owned(),
        ],
        "#1318-r2 extensions-truthiness: the all-runtimes events for a coerced changedPaths list"
    );
}

/// Regeneration guard for the TS golden; run once with `PDRS_BASELINE` set.
#[test]
#[ignore = "regenerates the TS fixture; needs PDRS_BASELINE and Node"]
fn settings_gateway_fixture_regenerates_identically_from_ts() {
    crate::test_baseline::regenerates_identically("settings-gateway");
}
