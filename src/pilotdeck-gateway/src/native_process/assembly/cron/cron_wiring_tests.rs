//! #1326 A8 U8 assembly tests: a config `cron:` section constructs the one
//! `CronManager`, advertises the six capability names, and serves the six
//! frames through the live WS dispatch; an absent section keeps them gated;
//! cleanup stops cron before the channels; a `start()` failure fails the
//! whole assembly.

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
    // envelope (GatewayWsConnection.ts:253-260).
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

/// The cleanup order PilotDeckServerShutdownBundle.ts:33-37 fixes: automation
/// (cron) stops before closeServer (channels), and both precede the session
/// and project disposal.
#[tokio::test]
async fn cleanup_stops_cron_before_the_channels() {
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
        ["cron", "channels", "session", "project"],
        "#1326 cron stops first, then the channels, sessions, and project"
    );
}

/// A `CronManager::start()` failure must fail the whole assembly (the
/// `project.dispose()` fallback path), never be swallowed into a
/// half-started host. A `.cwd` marker that is a directory makes the startup
/// discovery throw where TS's `readFileSync` would.
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
    // executed one; the scheduler half that would fire runs is #1305's.
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
