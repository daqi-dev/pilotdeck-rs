//! #1326 A8 U8 assembly tests: a config `cron:` section constructs the one
//! `CronManager`, advertises the six capability names, and serves the six
//! frames through the live WS dispatch; an absent section keeps them gated;
//! cleanup stops cron before the channels; a `start()` failure fails the
//! whole assembly.
//!
//! #2294 A7.8 extends the same assembly: both families now sit behind the
//! automation bundle, so the always-on wiring rows here boot the identical
//! host with an `alwaysOn:` section and check the three names, the control
//! envelopes, and the `createAlwaysOn` conditions the same way.

use crate::runtime::turn::gateway::TurnGateway;
use crate::{negative_assert, negative_assert_eq};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The full config the #1231 fixture writes, plus the `cron:` section that
/// `ProjectAutomationBundle.createCron` turns into a manager.
const CRON_CONFIG: &str = "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\ncron:\n  enabled: true\n";

/// #1325's restart-unrecorded golden (tests/fixtures/native-cron/): a TS-era
/// task store whose last run was interrupted mid-flight — the exact state a
/// host carries across the switch into native mode.
const RESTART_UNRECORDED: &str =
    include_str!("../../../../tests/fixtures/native-cron/restart-unrecorded.json");

/// The #2294 wiring rows' config prefix: the #1231 fixture's model half plus
/// the `cron:` section, so those rows exercise exactly the assembly the
/// always-on rows add an `alwaysOn:` section to.
const ALWAYS_ON_BASE: &str = "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\ncron:\n  enabled: true\n";

#[derive(Default)]
struct Text(Mutex<Vec<Value>>);
impl crate::runtime::connection::GatewayTextConnection for Text {
    fn send_text(&self, text: String) {
        self.0
            .lock()
            .expect("#1326 frames lock")
            .push(serde_json::from_str(&text).expect("#1326 frame JSON"));
    }
    fn close(&self, _: u16, _: &str) {}
}

struct Harness {
    options: crate::native_process::Options,
    host: TurnGateway,
    connection: crate::runtime::turn::gateway::HostedConnection,
    text: Arc<Text>,
    /// Holds the fixture tree alive for as long as the host runs. `boot`
    /// creates it; `boot_with` leaves it to the caller (the `Scratch` drop
    /// deletes the home tree, which a live host keeps writing under).
    _scratch: Option<crate::native_process::management_tests::Scratch>,
}

async fn boot(configured: bool) -> Harness {
    let (scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1326 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 shutdown unused fixture owner");
    if configured {
        std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG)
            .expect("#1326 cron config");
    }
    let mut harness = boot_with(options, env).await;
    harness._scratch = Some(scratch);
    harness
}

/// The connection half of `boot`, over caller-supplied options — the caller
/// owns the fixture's `Scratch` (so the home tree stays alive) and does any
/// seeding before the assembly starts.
async fn boot_with(options: crate::native_process::Options, env: crate::config::Env) -> Harness {
    let host = crate::native_process::assembly::start(options.clone(), env)
        .await
        .expect("#1326 assembly start");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string(),
        )
        .await
        .expect("#1326 hello");
    Harness {
        options,
        host,
        connection,
        text,
        _scratch: None,
    }
}

fn capabilities(harness: &Harness) -> Vec<String> {
    let frames = harness.text.0.lock().expect("#1326 frames lock");
    frames
        .iter()
        .find(|frame| frame["id"] == "capabilities")
        .expect("#1326 capability describe response")["result"]["capabilities"]
        .as_array()
        .expect("#1326 capabilities array")
        .iter()
        .map(|value| value.as_str().expect("#1326 capability string").to_owned())
        .collect()
}

fn take_frame(harness: &Harness, id: &str) -> Value {
    let frames = harness.text.0.lock().expect("#1326 frames lock");
    frames
        .iter()
        .find(|frame| frame["id"] == id)
        .expect("#1326 response frame")
        .clone()
}

async fn send(harness: &Harness, id: &str, method: &str, params: Value) {
    harness
        .connection
        .receive(json!({"type":"request","id":id,"method":method,"params":params}).to_string())
        .await
        .expect("#1326 route frame");
}

#[tokio::test]
async fn configured_assembly_advertises_and_serves_the_six_cron_names() {
    let harness = boot(true).await;

    send(&harness, "capabilities", "describe_server", json!({})).await;
    let advertised = capabilities(&harness);
    for name in crate::runtime::cron::CRON_METHODS {
        negative_assert!(
            advertised.contains(&name.to_owned()),
            "#1326 the configured runtime advertises {name}"
        );
    }

    // A real round trip through the live WS dispatch: create succeeds,
    // lists back, and the delay-unit RangeError crosses as the last-arm
    // envelope (GatewayWsConnection.ts:245-255).
    let project = harness.options.project.to_string_lossy().into_owned();
    send(
        &harness,
        "create",
        "cron_create",
        json!({"projectKey": project,
               "message": "m1",
               "sessionKey": "origin-session",
               "channelKey": "origin-channel",
               "mode": "default",
               "schedule": {"type": "once", "runAt": "2027-01-01T00:00:00.000Z"}}),
    )
    .await;
    let created = take_frame(&harness, "create");
    negative_assert_eq!(
        created["ok"],
        true,
        "#1326 cron_create through the WS dispatch"
    );

    send(
        &harness,
        "list",
        "cron_list",
        json!({"projectKey": project}),
    )
    .await;
    let listed = take_frame(&harness, "list");
    negative_assert_eq!(
        listed["ok"],
        true,
        "#1326 cron_list through the WS dispatch"
    );
    negative_assert_eq!(
        listed["result"]["tasks"].as_array().map(Vec::len),
        Some(1),
        "#1326 the created task is listed"
    );

    send(
        &harness,
        "bad-delay",
        "cron_create",
        json!({"projectKey": project,
               "message": "m2",
               "sessionKey": "origin-session",
               "channelKey": "origin-channel",
               "mode": "default",
               "schedule": {"type": "delay", "amount": 5, "unit": "minutes"}}),
    )
    .await;
    let thrown = take_frame(&harness, "bad-delay");
    negative_assert_eq!(thrown["ok"], false, "#1326 the RangeError frame fails");
    negative_assert_eq!(
        thrown["error"]["code"],
        "gateway_request_failed",
        "#1326 the RangeError crosses as the last-arm code"
    );
    negative_assert_eq!(
        thrown["error"]["message"],
        "Invalid time value",
        "#1326 the RangeError message crosses verbatim"
    );

    harness
        .host
        .shutdown(harness.options.drain_budget)
        .await
        .expect("#1326 host shutdown");
}

#[tokio::test]
async fn unconfigured_assembly_keeps_the_six_names_gated() {
    let harness = boot(false).await;

    send(&harness, "capabilities", "describe_server", json!({})).await;
    let advertised = capabilities(&harness);
    for name in crate::runtime::cron::CRON_METHODS {
        negative_assert!(
            !advertised.contains(&name.to_owned()),
            "#1326 the unconfigured runtime keeps {name} out of capabilities"
        );
    }

    // InProcessGateway.ts:3016-3022: `requireCron` throws before any adapter
    // runs, and the `DialogGatewayError` crosses with its own code.
    send(&harness, "list", "cron_list", json!({})).await;
    let gated = take_frame(&harness, "list");
    negative_assert_eq!(gated["ok"], false, "#1326 the unconfigured gate rejects");
    negative_assert_eq!(
        gated["error"]["code"],
        "CAPABILITY_UNAVAILABLE",
        "#1326 the gate keeps the DialogGatewayError code"
    );
    negative_assert_eq!(
        gated["error"]["message"],
        "Cron runtime is not configured on this gateway.",
        "#1326 the gate message crosses verbatim"
    );

    harness
        .host
        .shutdown(harness.options.drain_budget)
        .await
        .expect("#1326 host shutdown");
}

/// #1353 A6: once the switch machine's S1 has stopped the cron manager, the
/// six management methods answer an explicit SWITCH_IN_PROGRESS rejection
/// through the routing seam instead of operating on the stopped manager (the
/// TS baseline keeps serving them, D5 — a registered Rust difference,
/// specs/gateway-parity/diffs/). The production entry hands out the shared
/// stop handle; the S1 face over that handle closes the management gate
/// before the stop, so a call racing the stop observes the gate, not a
/// half-stopped manager.
#[tokio::test]
async fn a_stopped_manager_rejects_the_six_management_methods() {
    let (scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1353 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1353 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1353 shutdown unused fixture owner");
    std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG).expect("#1353 cron config");
    let (host, _webhook, switch_handles) =
        crate::native_process::assembly::start_with_webhook(options.clone(), env)
            .await
            .expect("#1353 assembly start");
    let handle = switch_handles
        .cron
        .expect("#1353 the configured manager has a stop handle");
    let text = Arc::new(Text::default());
    let connection = host.connect(text.clone(), "token".into(), "fixture".into());
    connection
        .receive(
            json!({"type":"hello","protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string(),
        )
        .await
        .expect("#1353 hello");
    let project = options.project.to_string_lossy().into_owned();
    // The same manager serves the management face before the switch.
    connection
        .receive(
            json!({"type":"request","id":"before","method":"cron_list","params":{"projectKey": project}}).to_string(),
        )
        .await
        .expect("#1353 route before frame");
    let before = text
        .0
        .lock()
        .expect("#1353 frames lock")
        .iter()
        .find(|frame| frame["id"] == "before")
        .expect("#1353 before response frame")
        .clone();
    negative_assert_eq!(
        before["ok"],
        true,
        "#1353 the manager serves before the stop"
    );
    // S1: the shared handle's face — gate first, stop once.
    handle
        .stop_face()
        .await
        .expect("#1353 the S1 face stops the manager");
    // Every management method, like the sibling advertisement tests: the
    // gate sits at the `cron_request` entry and is method-independent, so
    // this pins that no method can bypass it after the stop.
    for method in crate::runtime::cron::CRON_METHODS {
        connection
            .receive(
                json!({"type":"request","id":format!("after-{method}"),"method":method,"params":{"projectKey": project}}).to_string(),
            )
            .await
            .expect("#1353 route after frame");
    }
    // The guard must not live across the shutdown `.await` below (clippy
    // `await_holding_lock`; the explicit `drop(frames)` this replaces was not
    // recognized by clippy 1.99, which kept the CI red), so the assertions
    // run inside a block that ends the guard's scope before the await.
    {
        let frames = text.0.lock().expect("#1353 frames lock");
        // #2363: the aggregate gate control — the receipt-emitting anchor of
        // the `switch-cron-management-open` negative control (it bypasses the
        // `cron_management_closed` check at the routing seam), so the first
        // failing assertion names the gate itself. A method counts as ungated
        // when its after frame is missing, answers ok, or carries any code
        // other than the switch rejection.
        let ungated = crate::runtime::cron::CRON_METHODS
            .iter()
            .filter(|method| {
                match frames
                    .iter()
                    .find(|frame| frame["id"] == format!("after-{method}"))
                {
                    Some(after) => {
                        after["ok"] != false || after["error"]["code"] != "SWITCH_IN_PROGRESS"
                    }
                    None => true,
                }
            })
            .count();
        negative_assert_eq!(
            ungated,
            0,
            "#2363 every management method answers the closed-gate rejection"
        );
        for method in crate::runtime::cron::CRON_METHODS {
            let after = frames
                .iter()
                .find(|frame| frame["id"] == format!("after-{method}"))
                .expect("#1353 after response frame")
                .clone();
            negative_assert_eq!(
                after["ok"],
                false,
                "#1353 the stopped manager rejects {method}"
            );
            negative_assert_eq!(
                after["error"]["code"],
                "SWITCH_IN_PROGRESS",
                "#1353 A6: the rejection names the switch for {method}"
            );
            negative_assert_eq!(
                after["error"]["message"],
                "switch in progress: cron management is closed",
                "#1353 A6: the rejection message crosses verbatim"
            );
        }
    }
    host.shutdown(options.drain_budget)
        .await
        .expect("#1353 host shutdown observes the earlier stop");
    drop(scratch);
}

/// The cleanup order PilotDeckServerShutdownBundle.ts:33-37 fixes: automation
/// stops before closeServer (channels), and both precede the session and
/// project disposal. #2294 routes the cron manager behind the automation
/// bundle, so the observer label the cleanup carries is the bundle's —
/// "automation" — while the stop order itself is unchanged. #1725 item 15:
/// the channel state flush rides the same observation surface as the stop it
/// follows, so the trace carries it between the channels and the sessions.
#[tokio::test]
async fn cleanup_stops_automation_before_the_channels() {
    let (_scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1326 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 shutdown unused fixture owner");
    std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG).expect("#1326 cron config");

    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let sink = order.clone();
    let observer: crate::native_process::assembly::CleanupObserver =
        Arc::new(move |name, _result| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().expect("#1326 cleanup order lock").push(name);
                Ok(())
            })
        });
    let host = crate::native_process::assembly::start_observed(
        options.clone(),
        env,
        observer,
        Arc::new(|_| {}),
    )
    .await
    .expect("#1326 observed assembly start");
    host.shutdown(options.drain_budget)
        .await
        .expect("#1326 host shutdown");

    let recorded = order.lock().expect("#1326 cleanup order lock").clone();
    negative_assert_eq!(
        recorded,
        ["automation", "channels", "flush", "session", "project"],
        "#1326 automation (the bundle that owns cron) stops first, then the channels, flush, sessions, and project"
    );
}

/// A `CronManager::start()` failure must fail the whole assembly (the
/// `project.dispose()` fallback path), never be swallowed into a
/// half-started host. A `.cwd` marker that is a directory makes the startup
/// discovery throw where TS's `readFileSync` would. #2294 wraps the failure
/// in the automation bundle's startup `AggregateError`; its `Display` carries
/// the provider causes, so the discovery error still surfaces verbatim.
#[tokio::test]
async fn cron_start_failure_fails_the_assembly() {
    let (_scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1326 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 shutdown unused fixture owner");
    std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG).expect("#1326 cron config");
    std::fs::create_dir_all(options.home.join("cron/projects/broken/.cwd"))
        .expect("#1326 unreadable project marker");

    let failure = match crate::native_process::assembly::start(options.clone(), env).await {
        Err(failure) => failure,
        Ok(_) => panic!("#1326 the cron start failure must fail the assembly"),
    };
    negative_assert!(
        failure.to_string().contains(".cwd"),
        "#1326 the failure surfaces the discovery error, not a swallowed start: {failure}"
    );
}

/// #1959: a builder failure whose cleanup budget is exhausted mid-teardown
/// (the channels stage) must still dispose the resources the cleanup never
/// reached — per resource, not by entry. The one-shot gate used to be a single
/// entry flag, so a budget cut after the teardown entered suppressed the
/// fallback entirely and leaked both the session factory and the project
/// registry.
#[tokio::test]
async fn a_budget_cut_cleanup_still_disposes_factory_and_project() {
    let (_scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1959 fixture setup");
    let mut options = requests.options.clone();
    // The short drain budget is the cut: the stalled channels observer below
    // exhausts it mid-teardown.
    options.drain_budget = std::time::Duration::from_millis(50);
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1959 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1959 shutdown unused fixture owner");
    std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG).expect("#1959 cron config");
    std::fs::create_dir_all(options.home.join("cron/projects/broken/.cwd"))
        .expect("#1959 unreadable project marker");

    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let sink = order.clone();
    let observer: crate::native_process::assembly::CleanupObserver =
        Arc::new(move |name, _result| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().expect("#1959 cleanup order lock").push(name);
                // #1959 fixture: the budget dies here, mid-teardown, after the
                // channels closed but before the session factory and the
                // project registry are disposed — the fallback below must make
                // up exactly the resources the cleanup never reached.
                if name == "channels" {
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        });
    let failure = match crate::native_process::assembly::start_observed(
        options.clone(),
        env,
        observer,
        Arc::new(|_| {}),
    )
    .await
    {
        Err(failure) => failure,
        Ok(_) => panic!("#1959 the cron start failure must fail the assembly"),
    };
    negative_assert!(
        failure.to_string().contains(".cwd"),
        "#1959 the failure surfaces the discovery error, not a swallowed start: {failure}"
    );
    let recorded = order.lock().expect("#1959 cleanup order lock").clone();
    // #2486: the automation slot is filled before the start poll, so a
    // failed builder's teardown now records the automation stop first — a
    // no-op for a generation its own failure cleanup already drained —
    // before the channels stage that exhausts the budget here.
    negative_assert_eq!(
        recorded,
        ["automation", "channels", "session", "project"],
        "#1959 a budget cut mid-teardown must not skip the factory and project fallback disposals"
    );
}

/// #2294: a config `alwaysOn:` section with one enabled project stages the
/// one `AlwaysOnManager` through the automation bundle — the assembly boots,
/// the three names are advertised, and the control methods answer the real
/// runtime envelopes through the live WS dispatch. The rows pick envelopes
/// that need no store fixtures: a cycle id no store carries
/// (`cycle_not_found`), a session key no run owns (`session_not_active`),
/// and a plan id no store carries (`plan_not_found`, whose `runId` is the
/// runtime's generated run id and therefore only checked non-empty).
#[tokio::test]
async fn configured_assembly_advertises_and_serves_the_three_always_on_names() {
    let (scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#2294 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#2294 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#2294 shutdown unused fixture owner");
    let project = options.project.to_string_lossy().into_owned();
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        format!("{ALWAYS_ON_BASE}alwaysOn:\n  projects:\n    {project}:\n      enabled: true\n"),
    )
    .expect("#2294 always-on config");
    let mut harness = boot_with(options, env).await;
    harness._scratch = Some(scratch);

    send(&harness, "capabilities", "describe_server", json!({})).await;
    let advertised = capabilities(&harness);
    for name in crate::runtime::always_on::ALWAYS_ON_METHODS {
        negative_assert!(
            advertised.contains(&name.to_string()),
            "#2294 the configured runtime advertises {name}"
        );
    }

    send(
        &harness,
        "apply",
        "always_on_apply",
        json!({"projectKey": project, "workCycleId": "c1"}),
    )
    .await;
    let apply = take_frame(&harness, "apply");
    negative_assert_eq!(apply["ok"], true, "#2294 always_on_apply answers ok");
    negative_assert_eq!(
        apply["result"],
        json!({"sessionKey": "",
               "error": {"code": "cycle_not_found",
                         "message": "Work cycle c1 not found"}}),
        "#2294 always_on_apply answers the runtime envelope"
    );

    send(
        &harness,
        "abort",
        "always_on_abort",
        json!({"projectKey": project, "sessionKey": "s1"}),
    )
    .await;
    let abort = take_frame(&harness, "abort");
    negative_assert_eq!(abort["ok"], true, "#2294 always_on_abort answers ok");
    negative_assert_eq!(
        abort["result"],
        json!({"aborted": false,
               "sessionKey": "s1",
               "error": {"code": "session_not_active",
                         "message": "No active Always-On run owns this session."}}),
        "#2294 always_on_abort answers the runtime envelope"
    );

    send(
        &harness,
        "rerun",
        "always_on_rerun_plan",
        json!({"projectKey": project, "planId": "p1"}),
    )
    .await;
    let rerun = take_frame(&harness, "rerun");
    negative_assert_eq!(rerun["ok"], true, "#2294 always_on_rerun_plan answers ok");
    negative_assert_eq!(
        rerun["result"]["error"],
        json!({"code": "plan_not_found", "message": "Plan p1 not found"}),
        "#2294 always_on_rerun_plan answers the runtime envelope"
    );
    negative_assert!(
        rerun["result"]["runId"]
            .as_str()
            .is_some_and(|run_id| !run_id.is_empty()),
        "#2294 always_on_rerun_plan carries its generated run id"
    );

    harness
        .host
        .shutdown(harness.options.drain_budget)
        .await
        .expect("#2294 host shutdown");
}

/// #2294 `createAlwaysOn` conditions at the assembly: a present
/// `alwaysOn:` section whose projects are all disabled constructs no manager
/// — the three names stay out of capabilities and the methods keep the
/// #1313 not_configured envelopes byte for byte.
#[tokio::test]
async fn a_disabled_always_on_section_keeps_the_three_names_gated() {
    let (scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#2294 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#2294 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#2294 shutdown unused fixture owner");
    let project = options.project.to_string_lossy().into_owned();
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        format!("{ALWAYS_ON_BASE}alwaysOn:\n  projects:\n    {project}:\n      enabled: false\n"),
    )
    .expect("#2294 disabled always-on config");
    let mut harness = boot_with(options, env).await;
    harness._scratch = Some(scratch);

    send(&harness, "capabilities", "describe_server", json!({})).await;
    let advertised = capabilities(&harness);
    for name in crate::runtime::always_on::ALWAYS_ON_METHODS {
        negative_assert!(
            !advertised.contains(&name.to_string()),
            "#2294 the disabled section keeps {name} out of capabilities"
        );
    }

    send(
        &harness,
        "apply",
        "always_on_apply",
        json!({"projectKey": project, "workCycleId": "c1"}),
    )
    .await;
    let apply = take_frame(&harness, "apply");
    negative_assert_eq!(
        apply["result"],
        json!({"sessionKey": "",
               "error": {"code": "not_configured",
                         "message": "Always-On apply is not configured on this gateway."}}),
        "#2294 the unconfigured envelope is byte for byte the #1313 shape"
    );

    harness
        .host
        .shutdown(harness.options.drain_budget)
        .await
        .expect("#2294 host shutdown");
}

/// Issue #1326's "one scheduler, no re-execution across the switch": the
/// TS-era store #1325's restart-unrecorded golden captures is seeded into
/// the home before the native assembly boots — the boot itself is the
/// switch. Everything the test observes goes through the live WS route
/// first (`cron_list`, `cron_run_now`), then the committed run history is
/// read back from disk. The golden is re-keyed to the project this gateway
/// serves (the route's projectKey isolation admits only the configured
/// project); its structure is otherwise untouched.
#[tokio::test]
async fn ts_seeded_task_survives_the_switch_without_re_execution() {
    let (_scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    let options = requests.options.clone();
    let env = requests.env.clone();
    requests
        .project
        .dispose()
        .await
        .expect("#1326 dispose unused fixture registry");
    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 shutdown unused fixture owner");
    std::fs::write(options.home.join("pilotdeck.yaml"), CRON_CONFIG).expect("#1326 cron config");
    let project = options.project.to_string_lossy().into_owned();
    let seeded = RESTART_UNRECORDED.replace("/restart", &project);
    let paths = crate::runtime::cron::resolve_cron_paths(&options.home.to_string_lossy(), &project)
        .expect("#1326 seeded store paths");
    std::fs::create_dir_all(&paths.project_dir).expect("#1326 seeded project dir");
    std::fs::write(&paths.tasks_file, seeded).expect("#1326 seeded tasks");
    std::fs::write(paths.project_dir.join(".cwd"), format!("{project}\n"))
        .expect("#1326 seeded marker");

    let harness = boot_with(options, env).await;
    send(
        &harness,
        "list",
        "cron_list",
        json!({"projectKey": project}),
    )
    .await;
    let listed = take_frame(&harness, "list");
    let tasks = listed["result"]["tasks"]
        .as_array()
        .expect("#1326 seeded task listed");
    negative_assert_eq!(
        tasks.len(),
        1,
        "#1326 exactly the seeded task survived the switch"
    );
    negative_assert_eq!(tasks[0]["taskId"], "task-restart", "#1326 the seeded task");
    negative_assert_eq!(
        tasks[0]["status"],
        "scheduled",
        "#1326 recovery rescheduled the interrupted task"
    );
    negative_assert_eq!(
        tasks[0]["revision"].as_f64(),
        Some(2.0),
        "#1326 the reschedule bumped the revision"
    );

    send(
        &harness,
        "now",
        "cron_run_now",
        json!({"taskId": "task-restart", "projectKey": project}),
    )
    .await;
    let started = take_frame(&harness, "now");
    negative_assert_eq!(
        started["result"]["started"],
        true,
        "#1326 the recovered task accepts a run-now submission"
    );

    harness
        .host
        .shutdown(harness.options.drain_budget)
        .await
        .expect("#1326 host shutdown");

    // The committed history carries exactly the recovery record for the
    // interrupted run — a failed `cron_run_interrupted` entry, never an
    // executed one; the firing half (scheduler.rs/fire.rs, #2295) is not
    // what this fixture drives.
    let history =
        std::fs::read_to_string(&paths.run_history_file).expect("#1326 committed run history");
    let records: Vec<Value> = history
        .split("\n")
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("#1326 history line JSON"))
        .collect();
    negative_assert_eq!(
        records.len(),
        1,
        "#1326 the switch recorded the interrupted run and executed nothing"
    );
    negative_assert_eq!(records[0]["runId"], "run-crash", "#1326 the last run's id");
    negative_assert_eq!(
        records[0]["outcome"],
        "failed",
        "#1326 the interrupted run is a failed record"
    );
    negative_assert_eq!(
        records[0]["error"]["code"],
        "cron_run_interrupted",
        "#1326 the failure carries the interruption code"
    );
    negative_assert_eq!(
        records[0]["startedAt"],
        "2026-01-01T00:00:00.000Z",
        "#1326 the record starts at the stored task.updatedAt"
    );
}

/// #1326 r1 P1 (both review channels): the port's fence and the submit
/// path's admission must be **different** `SessionAdmission` instances.
/// `SessionAdmissionCronGateway` holds its fence for the whole of
/// `run_turn`, and `NativeCronGatewayBody::run_turn` ends in
/// `TurnSubmitter::submit`, which admits the same `sessionKey` through
/// `SessionOwner::begin_turn`. Binding the owner's admission here makes the
/// inner admission answer `session_busy` for the outer fence's own key: the
/// turn never runs and the drained stream reports success.
#[test]
fn a_shared_port_fence_would_lock_out_the_submit_path() {
    use crate::runtime::cron::{
        CronError, CronGatewayBody, CronTurnSubmit, SessionAdmissionCronGateway,
    };
    use crate::runtime::session::SessionAdmission;
    use pilotdeck_protocol::JsString;

    /// Stands in for `TurnSubmitter::submit`: admits the same key through the
    /// admission the owner uses.
    struct SubmitsThrough(Arc<SessionAdmission>);
    impl CronGatewayBody for SubmitsThrough {
        fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
            match self.0.begin_turn(&input.session_key, &input.run_id) {
                Some(_) => Ok(()),
                None => Err(CronError::gateway(
                    "session_busy",
                    JsString::from("Session already has an active turn."),
                )),
            }
        }
        fn abort_turn(
            &self,
            _: &JsString,
            _: Option<&JsString>,
            _: Option<&JsString>,
        ) -> Result<(), CronError> {
            Ok(())
        }
        fn close_session(&self, _: &JsString, _: Option<&JsString>) -> Result<(), CronError> {
            Ok(())
        }
    }

    let turn = || CronTurnSubmit {
        session_key: JsString::from("cron:task-1"),
        channel_key: JsString::from("cron"),
        project_key: None,
        message: JsString::from("m"),
        mode: JsString::from("default"),
        run_id: JsString::from("run-1"),
        timeout_ms: 1000.0,
    };

    // The wiring this test guards: the port owns its fence, the body admits
    // through another instance — the submit path is not blocked.
    let port_fence = Arc::new(SessionAdmission::default());
    let submit_fence = Arc::new(SessionAdmission::default());
    let gateway =
        SessionAdmissionCronGateway::new(port_fence, Arc::new(SubmitsThrough(submit_fence)));
    use crate::runtime::cron::CronAgentGateway;
    assert!(
        gateway.submit_turn(&turn()).is_ok(),
        "#1326 a dedicated port fence must not block the submit path's admission"
    );

    // Negative control: one shared instance reproduces the deadlock, so this
    // assertion fails the moment someone binds the owner's admission again.
    let shared = Arc::new(SessionAdmission::default());
    let gateway =
        SessionAdmissionCronGateway::new(shared.clone(), Arc::new(SubmitsThrough(shared)));
    let error = gateway
        .submit_turn(&turn())
        .expect_err("#1326 the shared fence is the defect this test pins");
    assert_eq!(
        error.code(),
        Some("session_busy"),
        "#1326 the self-lock surfaces as session_busy"
    );
}

/// #1326 fix-r1 P1 (MiniMax, both the concurrency and boundary roles) and
/// fix-r2 P1 (MiniMax: only `session_busy` was read, so `workspace_busy`,
/// `CAPABILITY_UNAVAILABLE`, registry refusals and mid-turn failures still
/// drained into a silent `Ok`; boundary: the test only covered the pure
/// classifier, so deleting the drain loop's `return Err` would have survived).
///
/// The test drives `drain_turn` itself over a scripted stream — the submit
/// path answers every refusal and every mid-turn failure as an ordinary `Ok`
/// stream ending in an `error` event, and the port must answer `Err`.
#[tokio::test]
async fn a_refused_or_failed_turn_is_not_drained_into_success() {
    use pilotdeck_protocol::JsValue;
    use std::collections::VecDeque;

    use crate::runtime::turn::submit::SubmitError;

    /// One step per `next_event`; exhausting the script ends the stream.
    struct Scripted(VecDeque<Result<Option<JsValue>, SubmitError>>);
    impl super::CronTurnStream for Scripted {
        fn next_event(
            &mut self,
        ) -> impl std::future::Future<Output = Result<Option<JsValue>, SubmitError>> {
            let step = self.0.pop_front().unwrap_or(Ok(None));
            async move { step }
        }
    }

    let event = |fields: Vec<(&'static str, JsValue)>| crate::metadata::object(fields);
    let error_event = |code: &'static str, message: &'static str, recoverable: bool| {
        event(vec![
            ("type", "error".into()),
            ("code", code.into()),
            ("message", message.into()),
            ("recoverable", JsValue::Bool(recoverable)),
        ])
    };
    let drain = |events: Vec<JsValue>| {
        let mut stream = Scripted(events.into_iter().map(|event| Ok(Some(event))).collect());
        async move { super::drain_turn(&mut stream).await }
    };

    // Every preflight refusal `runtime/turn/submit.rs` can emit, plus the
    // mid-turn failures the old `session_busy`-only table let through.
    for (code, message, recoverable) in [
        (
            "session_busy",
            "Session cron:task-1 already has an active turn.",
            true,
        ),
        ("SESSION_BUSY", "SDK", true),
        ("workspace_busy", "Workspace is busy.", true),
        (
            "CAPABILITY_UNAVAILABLE",
            "Workspace snapshot host capability is unavailable.",
            true,
        ),
        ("conflict", "registry refused", true),
        ("model_failed", "boom", false),
        ("turn_timeout", "timed out", false),
        ("agent_aborted", "aborted", false),
    ] {
        let failure = drain(vec![
            event(vec![("type", "text_delta".into())]),
            error_event(code, message, recoverable),
        ])
        .await
        .expect_err("#1326 a turn that never finished must not report success");
        assert_eq!(
            failure.code(),
            Some(code),
            "#1326 {code} keeps its own code for the firing half"
        );
        assert_eq!(
            String::from_utf16_lossy(failure.message().units()),
            message,
            "#1326 {code} keeps its message"
        );
    }

    // A stream with no error event is a finished turn.
    drain(vec![
        event(vec![("type", "text_delta".into())]),
        event(vec![("type", "agent_status".into())]),
    ])
    .await
    .expect("#1326 a clean turn drains to Ok");

    // An error event with neither code nor message still fails, under the
    // transport-level default rather than an empty projection.
    let bare = drain(vec![event(vec![("type", "error".into())])])
        .await
        .expect_err("#1326 a bare error event is still a failure");
    assert_eq!(bare.code(), Some("cron_run_failed"));
    assert_eq!(
        String::from_utf16_lossy(bare.message().units()),
        "cron_run_failed"
    );

    // A transport failure (not an event) maps to the same default.
    let mut broken = Scripted(VecDeque::from([Err(SubmitError {
        code: "channel_closed".into(),
        message: "closed".into(),
    })]));
    let failure = super::drain_turn(&mut broken)
        .await
        .expect_err("#1326 a broken stream is a failed turn");
    assert_eq!(failure.code(), Some("cron_run_failed"));
    assert_eq!(
        String::from_utf16_lossy(failure.message().units()),
        "Error: closed"
    );
}
