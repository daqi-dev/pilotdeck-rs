//! #1326 A8 U8 family tests: the six cron names route through the registry
//! to `Requests::cron_request`, the unconfigured `None` answers the
//! `requireCron` gate verbatim, and the `CronError` → `RequestError`
//! projection reproduces the GatewayWsConnection.ts:215-255 arms.

use super::*;
use crate::runtime::cron::{CronManager, CronManagerConfig, CronManagerOptions};
use crate::{negative_assert, negative_assert_eq};
use std::sync::Arc;

fn json(text: &str) -> JsValue {
    pilotdeck_protocol::parse_json(text).expect("#1326 test JSON")
}

fn text(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .and_then(|text| text.to_utf8().ok())
        .expect("#1326 string field")
}

fn wire_message(error: &RequestError) -> String {
    String::from_utf16_lossy(error.message.units())
}

/// A configured-but-unstarted `CronManager` on the fixture's scratch home:
/// enough for every control method (create/list/update/delete/stop/run_now)
/// without the scheduler half.
fn configured_manager(home: &std::path::Path) -> Arc<CronManager> {
    CronManager::new(CronManagerOptions {
        config: CronManagerConfig::default(),
        pilot_home: home.to_string_lossy().into_owned(),
        session_overrides: None,
        now: None,
        uuid: None,
        logger: None,
        scheduler_idle_poll_ms: None,
    })
    .expect("#1326 fixture cron manager")
}

/// `GatewayRequests::request`'s dispatch slice: `methods::request` routes by
/// the family registry, so a cron frame reaching the fallback
/// (`metadata::unavailable`) would answer `CAPABILITY_UNAVAILABLE` with the
/// "not configured" message instead of the manager's domain result.
async fn route(
    requests: &Requests,
    method: &str,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    super::super::request(requests, method.to_owned(), params.clone()).await
}

#[tokio::test]
async fn all_six_cron_names_route_to_the_cron_family() {
    let (scratch, mut requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    requests.cron = Some(configured_manager(&requests.options.home));

    let created = route(
        &requests,
        "cron_create",
        &json(
            r#"{"projectKey": "/proj",
                "message": "m1",
                "sessionKey": "origin-session",
                "channelKey": "origin-channel",
                "mode": "default",
                "schedule": {"type": "once", "runAt": "2027-01-01T00:00:00.000Z"}}"#,
        ),
    )
    .await
    .expect("#1326 cron_create routes to the manager");
    let task_id = text(
        created.get("task").expect("#1326 create returns {task}"),
        "taskId",
    );

    let listed = route(&requests, "cron_list", &json(r#"{"projectKey": "/proj"}"#))
        .await
        .expect("#1326 cron_list routes to the manager");
    negative_assert_eq!(
        match listed.get("tasks") {
            Some(JsValue::Array(tasks)) => tasks.len(),
            _ => panic!("#1326 tasks must be an array"),
        },
        1,
        "#1326 cron_list returns the created task"
    );

    let updated = route(
        &requests,
        "cron_update",
        &json(&format!(
            r#"{{"taskId": "{task_id}",
                "projectKey": "/proj",
                "expectedRevision": 0,
                "message": "m2",
                "schedule": {{"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}}}}"#
        )),
    )
    .await
    .expect("#1326 cron_update routes to the manager");
    negative_assert_eq!(
        updated.get("updated").and_then(JsValue::as_bool),
        Some(true),
        "#1326 cron_update goes through"
    );

    let started = route(
        &requests,
        "cron_run_now",
        &json(&format!(
            r#"{{"taskId": "{task_id}", "projectKey": "/proj"}}"#
        )),
    )
    .await
    .expect("#1326 cron_run_now routes to the manager");
    negative_assert_eq!(
        started.get("started").and_then(JsValue::as_bool),
        Some(true),
        "#1326 cron_run_now starts the task"
    );

    let stopped = route(
        &requests,
        "cron_stop",
        &json(r#"{"taskId": "not-running"}"#),
    )
    .await
    .expect("#1326 cron_stop routes to the manager");
    negative_assert_eq!(
        stopped.get("stopped").and_then(JsValue::as_bool),
        Some(false),
        "#1326 cron_stop reports the idle shape"
    );

    let deleted = route(
        &requests,
        "cron_delete",
        &json(&format!(
            r#"{{"taskId": "{task_id}", "projectKey": "/proj"}}"#
        )),
    )
    .await
    .expect("#1326 cron_delete routes to the manager");
    negative_assert_eq!(
        deleted.get("deleted").and_then(JsValue::as_bool),
        Some(true),
        "#1326 cron_delete removes the task"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1326 fixture project shutdown");
    drop(scratch);
}

#[tokio::test]
async fn unconfigured_cron_answers_the_require_cron_gate() {
    let (scratch, requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    negative_assert!(
        requests.cron.is_none(),
        "#1326 the shared fixture models the unconfigured runtime"
    );

    for method in crate::runtime::cron::CRON_METHODS {
        let error = route(&requests, method, &json("{}"))
            .await
            .expect_err("#1326 the unconfigured gate rejects every cron method");
        negative_assert_eq!(
            error.code,
            "CAPABILITY_UNAVAILABLE",
            "#1326 the DialogGatewayError keeps its code ({method})"
        );
        negative_assert_eq!(
            wire_message(&error),
            "Cron runtime is not configured on this gateway.",
            "#1326 the gate message crosses verbatim ({method})"
        );
    }

    // The gate is the cron family's alone: a method the family does not own
    // keeps the ordinary fallback answer, and no unrelated method inherits
    // the gate.
    let unknown = route(&requests, "cron_unknown", &json("{}"))
        .await
        .expect_err("#1326 an unregistered cron-ish name is not ours");
    negative_assert_eq!(
        unknown.code,
        "CAPABILITY_UNAVAILABLE",
        "#1326 unknown names keep the fallback code"
    );
    negative_assert_eq!(
        wire_message(&unknown),
        "Native gateway method cron_unknown is not configured.",
        "#1326 unknown names keep the fallback message"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1326 fixture project shutdown");
    drop(scratch);
}

/// GatewayWsConnection.ts:215-255 for the three thrown classes the six
/// methods can surface through a configured runtime: the plain
/// `Error`/`TypeError`/`RangeError` names all fall to the last arm's
/// `gateway_request_failed` with the thrown message; the `DialogGatewayError`
/// arm keeps the structured code (the gate test above carries that arm).
#[tokio::test]
async fn cron_error_envelope_matches_the_ws_connection_arms() {
    let (scratch, mut requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    requests.cron = Some(configured_manager(&requests.options.home));

    // Plain Error: `requireProjectKey`'s own message (CronManager.ts:115).
    let missing = route(&requests, "cron_create", &json(r#"{}"#))
        .await
        .expect_err("#1326 a missing projectKey throws");
    negative_assert_eq!(
        missing.code,
        "gateway_request_failed",
        "#1326 a plain Error falls to the last arm"
    );
    negative_assert_eq!(
        wire_message(&missing),
        "Cron task creation requires a projectKey.",
        "#1326 the plain Error message crosses verbatim"
    );

    // TypeError: `projectKey?.trim is not a function` on a non-string key
    // (probed on Node v22.22.0).
    let non_string = route(&requests, "cron_create", &json(r#"{"projectKey": 5}"#))
        .await
        .expect_err("#1326 a non-string projectKey throws");
    negative_assert_eq!(
        non_string.code,
        "gateway_request_failed",
        "#1326 a TypeError falls to the last arm"
    );
    negative_assert_eq!(
        wire_message(&non_string),
        "projectKey?.trim is not a function",
        "#1326 the TypeError message crosses verbatim"
    );

    // RangeError: `new Date(NaN).toISOString()` on a delay unit outside the
    // four keys (CronRuntime.ts:581 — "minutes" is not a unit).
    let invalid_delay = route(
        &requests,
        "cron_create",
        &json(
            r#"{"projectKey": "/proj",
                "message": "m1",
                "sessionKey": "origin-session",
                "channelKey": "origin-channel",
                "mode": "default",
                "schedule": {"type": "delay", "amount": 5, "unit": "minutes"}}"#,
        ),
    )
    .await
    .expect_err("#1326 an unrepresentable delay throws");
    negative_assert_eq!(
        invalid_delay.code,
        "gateway_request_failed",
        "#1326 a RangeError falls to the last arm"
    );
    negative_assert_eq!(
        wire_message(&invalid_delay),
        "Invalid time value",
        "#1326 the RangeError message crosses verbatim"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1326 fixture project shutdown");
    drop(scratch);
}

/// #1509 P2-3b: dispatch runs on the blocking pool, not on an async worker.
///
/// The method bodies are synchronous and CPU-bound — `cron.create` /
/// `cron.update` run `computeNextRunAt`, whose minute scan can walk the whole
/// `MAX_SEARCH_MINUTES` (366-day) bound before it answers, and every method
/// touches the cron store. Running that inline parks a whole tokio thread for
/// the scan.
///
/// The property pinned here is the dispatch thread, an observable identity —
/// not a scheduling side effect. `CronManagerOptions::now` runs inside
/// `create_task` (`runtime/cron/manager.rs`'s `create_task_at`), i.e. on
/// whatever thread the dispatch body runs on, so the test injects a clock
/// that records `std::thread::current().id()` and asserts the recorded id
/// differs from the async worker's (the single-threaded runtime below makes
/// that worker the test's own thread).
///
/// Why the thread and not the interleave (#2682): tokio promises nothing
/// about a `spawn_blocking` `JoinHandle` being pending on its first poll, so
/// the pre-#2682 shape — a sibling joined with `create` and asserted to run
/// first — could fail on its own when the blocking thread won the race
/// (fail-closed false red). And why no await anywhere can fake a pass
/// (#2744): a suspension inserted on the route→dispatch path leaves the
/// dispatch thread what it is, so the fail-open direction (an inline
/// dispatch hidden behind an unrelated pending point) still dies here — the
/// `cron-inline-sync-yield-now` mutant in `tests/fixtures/native-config`
/// pins exactly that shape.
#[tokio::test(flavor = "current_thread")]
async fn cron_request_dispatches_off_the_async_worker() {
    use std::sync::Mutex;

    let (scratch, mut requests) =
        std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1326 fixture setup");
    let dispatch_thread = Arc::new(Mutex::new(None::<std::thread::ThreadId>));
    let recorder = Arc::clone(&dispatch_thread);
    let now = move || {
        *recorder.lock().expect("#2682 dispatch-thread recorder") =
            Some(std::thread::current().id());
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_millis() as f64)
            .expect("#2682 the wall clock predates the unix epoch")
    };
    requests.cron = Some(
        CronManager::new(CronManagerOptions {
            config: CronManagerConfig::default(),
            pilot_home: requests.options.home.to_string_lossy().into_owned(),
            session_overrides: None,
            now: Some(Arc::new(now)),
            uuid: None,
            logger: None,
            scheduler_idle_poll_ms: None,
        })
        .expect("#1326 fixture cron manager"),
    );

    let created = route(
        &requests,
        "cron_create",
        &json(
            r#"{"projectKey": "/proj",
                "message": "m1",
                "sessionKey": "origin-session",
                "channelKey": "origin-channel",
                "mode": "default",
                "schedule": {"type": "cron", "expression": "5 * * * *", "timezone": "UTC"}}"#,
        ),
    )
    .await
    .expect("#1509 cron_create still answers through the blocking pool");
    negative_assert!(
        created.get("task").is_some(),
        "#1509 the dispatched create returns its task record"
    );
    let observed = *dispatch_thread
        .lock()
        .expect("#2682 dispatch-thread readback");
    negative_assert!(
        observed.is_some_and(|thread| thread != std::thread::current().id()),
        "#1509 cron dispatch must run off the async worker thread"
    );

    requests
        .owner
        .shutdown()
        .await
        .expect("#1326 fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1326 fixture project shutdown");
    drop(scratch);
}
