//! Actual fixed TS CronFire + CronTaskStore replay, not a copied reducer.
use super::{
    CronActiveRun, CronAgentGateway, CronError, CronFire, CronFireDeps, CronLogger, CronTaskStore,
    CronTurnSubmit, CronZoneResolverCache, resolve_cron_paths,
};
use crate::test_scratch::Scratch;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
const ORACLE: &str = include_str!("../../../tests/fixtures/cron-events/oracle.json");
const START: f64 = 1_767_225_600_000.0;
fn obj(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn f<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}
fn string(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn flag(value: &JsValue, key: &str) -> bool {
    value.get(key).and_then(JsValue::as_bool).unwrap_or(false)
}
fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(values) => values,
        _ => panic!("not an array"),
    }
}
fn task_id(value: &JsValue) -> JsString {
    f(value, "taskId").as_string().unwrap().clone()
}
fn set(value: &mut JsValue, key: &str, field: JsValue) {
    let JsValue::Object(object) = value else {
        panic!("not an object")
    };
    object.insert(key.into(), field);
}
#[derive(Clone)]
struct Logger(Arc<Mutex<Vec<JsValue>>>);
impl CronLogger for Logger {
    fn info(&self, _: &str, _: &JsObject) {}
    fn warn(&self, message: &str, meta: &JsObject) {
        self.0.lock().unwrap().push(obj(vec![
            ("message", message.into()),
            ("meta", JsValue::Object(meta.clone())),
        ]));
    }
}
struct Gateway {
    input: JsValue,
    store: CronTaskStore,
    task_id: JsString,
    active: Arc<Mutex<Vec<CronActiveRun>>>,
    calls: Arc<Mutex<Vec<JsValue>>>,
    read: Arc<AtomicUsize>,
}
impl CronAgentGateway for Gateway {
    fn submit_turn(&self, _: &CronTurnSubmit) -> Result<(), CronError> {
        panic!("event fire must use observed gateway")
    }
    fn submit_turn_observed(
        &self,
        input: &CronTurnSubmit,
        observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        let mut value = vec![
            ("sessionKey", JsValue::String(input.session_key.clone())),
            ("channelKey", JsValue::String(input.channel_key.clone())),
        ];
        if let Some(project) = &input.project_key {
            value.push(("projectKey", JsValue::String(project.clone())));
        }
        value.extend([
            ("message", JsValue::String(input.message.clone())),
            ("mode", JsValue::String(input.mode.clone())),
            ("runId", JsValue::String(input.run_id.clone())),
            ("timeoutMs", JsValue::Number(input.timeout_ms)),
        ]);
        self.calls.lock().unwrap().push(obj(vec![
            ("method", "submitTurn".into()),
            ("input", obj(value)),
        ]));
        let mut failure = None;
        for event in array(f(&self.input, "events")) {
            self.read.fetch_add(1, Ordering::SeqCst);
            if failure.is_none()
                && let Err(error) = observe(event)
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if flag(&self.input, "edit") {
            self.store.update_task_js(&self.task_id, |current| {
                let mut value = current.clone();
                set(&mut value, "message", "edited".into());
                set(
                    &mut value,
                    "revision",
                    JsValue::Number(f(current, "revision").as_number().unwrap() + 1.0),
                );
                Some(value)
            })?;
        }
        if flag(&self.input, "deleted") {
            self.store.update_task_js(&self.task_id, |_| None)?;
        }
        if flag(&self.input, "stopped") {
            self.active
                .lock()
                .unwrap()
                .iter_mut()
                .find(|run| run.run_id == input.run_id)
                .unwrap()
                .stop_requested = true;
        }
        if let Some(message) = self.input.get("throwMessage").and_then(JsValue::as_string) {
            return Err(CronError::error(message.clone()));
        }
        Ok(())
    }
    fn abort_turn(
        &self,
        session: &JsString,
        run: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        let mut value = vec![("sessionKey", JsValue::String(session.clone()))];
        if let Some(run) = run {
            value.push(("runId", JsValue::String(run.clone())));
        }
        if let Some(reason) = reason {
            value.push(("reason", JsValue::String(reason.clone())));
        }
        self.calls.lock().unwrap().push(obj(vec![
            ("method", "abortTurn".into()),
            ("input", obj(value)),
        ]));
        Ok(())
    }
    fn close_session(&self, _: &JsString, _: Option<&JsString>) -> Result<(), CronError> {
        Ok(())
    }
}
fn after_files(root: &Path) -> JsValue {
    fn visit(root: &Path, here: &Path, files: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(here).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::read_to_string(path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = JsObject::new();
    for (path, text) in files {
        out.insert(path.as_str().into(), text.as_str().into());
    }
    JsValue::Object(out)
}
fn run_row(row: &JsValue, limit: Option<usize>) -> (JsValue, usize) {
    let scratch = Scratch::new("cron-event-replay");
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/project").unwrap();
    fs::create_dir_all(&paths.project_dir).unwrap();
    fs::write(&paths.tasks_file, string(f(row, "initialTasksFile"))).unwrap();
    let input = f(row, "input").clone();
    let snapshot = f(row, "snapshot").clone();
    let run_id = f(row, "runId").as_string().unwrap().clone();
    let active = Arc::new(Mutex::new(Vec::<CronActiveRun>::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let read = Arc::new(AtomicUsize::new(0));
    let gateway = Arc::new(Gateway {
        input: input.clone(),
        store: CronTaskStore::new(paths.clone()),
        task_id: task_id(&snapshot),
        active: active.clone(),
        calls: calls.clone(),
        read: read.clone(),
    });
    let phases = Arc::new(Mutex::new(Vec::new()));
    let turns = Arc::new(Mutex::new(Vec::new()));
    let deliveries = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(Mutex::new(Vec::new()));
    let warns = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicUsize::new(0));
    let fire = CronFire::new(CronFireDeps {
        gateway,
        store: CronTaskStore::new(paths.clone()),
        now: {
            let clock = clock.clone();
            let input = input.clone();
            Arc::new(move || {
                let call = clock.fetch_add(1, Ordering::SeqCst);
                if call == 0 && flag(&input, "panicFirstClock") {
                    panic!("fixture first clock panic");
                }
                if call == 0 { START } else { START + 1000.0 }
            })
        },
        register_active_run: {
            let active = active.clone();
            Arc::new(move |mut run: CronActiveRun| {
                let mut active = active.lock().unwrap();
                if let Some(old) = active.iter_mut().find(|old| old.run_id == run.run_id) {
                    run.stop_requested |= old.stop_requested;
                    *old = run
                } else {
                    active.push(run)
                }
            })
        },
        unregister_active_run: {
            let active = active.clone();
            Arc::new(move |id: &JsString| {
                let mut active = active.lock().unwrap();
                active
                    .iter()
                    .position(|run| run.run_id == *id)
                    .map(|at| active.remove(at))
            })
        },
        get_active_run: {
            let active = active.clone();
            Arc::new(move |id: &JsString| {
                active
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|run| run.run_id == *id)
                    .cloned()
            })
        },
        run_timeout_ms: 60000.0,
        default_timezone: "UTC".into(),
        zone_cache: Arc::new(CronZoneResolverCache::new()),
        release_task_session: {
            let releases = releases.clone();
            Arc::new(move |task: &JsValue| releases.lock().unwrap().push(task.clone()))
        },
        on_result_delivery: Some({
            let deliveries = deliveries.clone();
            let input = input.clone();
            Arc::new(move |value: &JsValue| {
                deliveries.lock().unwrap().push(value.clone());
                if flag(&input, "deliveryThrows") {
                    Err(CronError::error("fixture delivery throw"))
                } else {
                    Ok(())
                }
            })
        }),
        on_phase_event: Some({
            let phases = phases.clone();
            let input = input.clone();
            Arc::new(move |value: &JsValue| {
                phases.lock().unwrap().push(value.clone());
                if input.get("phaseThrows") == value.get("phase") {
                    Err(CronError::error("fixture phase throw"))
                } else {
                    Ok(())
                }
            })
        }),
        on_turn_event: Some({
            let turns = turns.clone();
            let input = input.clone();
            Arc::new(
                move |session: &JsString, channel: &JsString, event: &JsValue| {
                    turns.lock().unwrap().push(obj(vec![
                        ("sessionKey", JsValue::String(session.clone())),
                        ("channelKey", JsValue::String(channel.clone())),
                        ("event", event.clone()),
                    ]));
                    if flag(&input, "turnThrows") {
                        Err(CronError::error("fixture turn callback throw"))
                    } else {
                        Ok(())
                    }
                },
            )
        }),
        assistant_text_limit_units: limit,
        logger: Arc::new(Logger(warns.clone())),
    });
    if flag(&input, "preRegister") {
        active.lock().unwrap().push(CronActiveRun {
            run_id: run_id.clone(),
            task_id: task_id(&snapshot),
            session_key: f(&snapshot, "sessionKey").as_string().unwrap().clone(),
            schedule_type: "once".into(),
            stop_requested: false,
        });
    }
    let error = fire
        .run_task(&snapshot, &run_id)
        .err()
        .map_or(JsValue::Null, |error| error.as_json());
    assert!(
        active.lock().unwrap().is_empty(),
        "active run leaked in {}",
        string(f(row, "id"))
    );
    let result = obj(vec![
        (
            "gatewayCalls",
            JsValue::Array(calls.lock().unwrap().clone()),
        ),
        ("phases", JsValue::Array(phases.lock().unwrap().clone())),
        ("turnEvents", JsValue::Array(turns.lock().unwrap().clone())),
        (
            "deliveries",
            JsValue::Array(deliveries.lock().unwrap().clone()),
        ),
        ("releases", JsValue::Array(releases.lock().unwrap().clone())),
        ("warns", JsValue::Array(warns.lock().unwrap().clone())),
        ("active", JsValue::Array(Vec::new())),
        (
            "clockCalls",
            JsValue::Number(clock.load(Ordering::SeqCst) as f64),
        ),
        ("error", error),
        ("after", after_files(&paths.project_dir)),
    ]);
    (result, read.load(Ordering::SeqCst))
}
#[test]
fn real_ts_twenty_three_event_cases_match_calls_phases_delivery_and_all_after_bytes() {
    let oracle = parse_json(ORACLE).unwrap();
    let rows = array(f(&oracle, "rows"));
    assert_eq!(rows.len(), 23);
    for row in rows {
        let (actual, _) = run_row(row, None);
        for key in [
            "gatewayCalls",
            "phases",
            "turnEvents",
            "deliveries",
            "releases",
            "warns",
            "active",
            "clockCalls",
            "error",
            "after",
        ] {
            // capture.clean() records JSON.parse(JSON.stringify(value)), not
            // invisible own Undefined slots (spread_task has such slots).
            // Preserve complete ordered JSON bytes; after-file strings are
            // never reparsed, sorted, or otherwise normalized.
            assert_eq!(
                pilotdeck_protocol::stringify_json(f(&actual, key)).unwrap(),
                pilotdeck_protocol::stringify_json(f(row, key)).unwrap(),
                "{} field {key}",
                string(f(row, "id"))
            );
        }
    }
}
#[test]
fn native_text_limit_uses_utf16_and_aborts_once_without_truncating_stream_drain() {
    let oracle = parse_json(ORACLE).unwrap();
    let mut row = array(f(&oracle, "rows"))[0].clone();
    let mut input = f(&row, "input").clone();
    let event = |text: &str| {
        obj(vec![
            ("type", "assistant_text_delta".into()),
            ("text", text.into()),
        ])
    };
    set(&mut input, "events", JsValue::Array(vec![event("😀")]));
    set(&mut row, "input", input.clone());
    let (exact, read) = run_row(&row, Some(2));
    assert_eq!(read, 1);
    assert_eq!(
        f(&array(f(&exact, "deliveries"))[0], "text")
            .as_string()
            .unwrap(),
        &JsString::from("😀")
    );
    set(
        &mut input,
        "events",
        JsValue::Array(vec![
            event("😀"),
            obj(vec![
                ("type", "error".into()),
                ("code", "agent_aborted".into()),
                ("message", "after abort".into()),
            ]),
            event("after error"),
        ]),
    );
    set(&mut row, "input", input);
    let (overflow, read) = run_row(&row, Some(1));
    assert_eq!(read, 3);
    let delivery = &array(f(&overflow, "deliveries"))[0];
    assert_eq!(string(f(delivery, "outcome")), "failed");
    assert_eq!(string(f(f(delivery, "error"), "code")), "cron_output_limit");
    let calls = array(f(&overflow, "gatewayCalls"));
    assert_eq!(
        calls
            .iter()
            .filter(|call| string(f(call, "method")) == "abortTurn")
            .count(),
        1
    );
    assert_eq!(
        string(f(f(&calls[1], "input"), "reason")),
        "system:output_limit"
    );
    assert_eq!(array(f(&overflow, "turnEvents")).len(), 3);
}

#[test]
fn native_four_million_unit_policy_rejects_real_overflow_and_drains_later_events() {
    let oracle = parse_json(ORACLE).unwrap();
    let mut row = array(f(&oracle, "rows"))[0].clone();
    let mut input = f(&row, "input").clone();
    let text = "x".repeat(super::NATIVE_CRON_MAX_ASSISTANT_TEXT_UNITS + 1);
    set(
        &mut input,
        "events",
        JsValue::Array(vec![
            obj(vec![
                ("type", "assistant_text_delta".into()),
                ("text", text.as_str().into()),
            ]),
            obj(vec![
                ("type", "assistant_text_delta".into()),
                ("text", "producer drained".into()),
            ]),
        ]),
    );
    set(&mut row, "input", input);
    let (actual, read) = run_row(&row, Some(super::NATIVE_CRON_MAX_ASSISTANT_TEXT_UNITS));
    assert_eq!(read, 2);
    let delivery = &array(f(&actual, "deliveries"))[0];
    assert_eq!(string(f(f(delivery, "error"), "code")), "cron_output_limit");
    assert_eq!(array(f(&actual, "turnEvents")).len(), 2);
}

#[test]
fn real_ts_utf16_scheduler_delay_and_both_recalculation_paths_match_file_bytes() {
    struct NoFire;
    impl super::CronFirePort for NoFire {
        fn run_task(&self, _: &JsValue, _: &JsString) -> Result<(), CronError> {
            panic!("must not fire");
        }
    }
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/cron-events/scheduler-oracle.json"
    ))
    .unwrap();
    let rows = array(f(&oracle, "rows"));
    assert_eq!(rows.len(), 3);
    for row in rows {
        let scratch = Scratch::new("cron-scheduler-utf16");
        let home = scratch.path().join("home");
        fs::create_dir(&home).unwrap();
        let paths = resolve_cron_paths(&home.to_string_lossy(), "/project").unwrap();
        fs::create_dir_all(&paths.project_dir).unwrap();
        fs::write(&paths.tasks_file, string(f(row, "initialTasksFile"))).unwrap();
        let scheduler = super::CronScheduler::new(super::SchedulerDeps {
            config: super::CronManagerConfig::default(),
            store: CronTaskStore::new(paths.clone()),
            fire: Arc::new(NoFire),
            uuid: Arc::new(|| panic!("must not allocate run id")),
            now: Arc::new(|| START),
            active_run_count: Arc::new(|| 1),
            logger: Arc::new(super::NoopCronLogger),
            zone_cache: Arc::new(CronZoneResolverCache::new()),
            idle_poll_ms: 60000,
            runtime: std::sync::Weak::new(),
        })
        .unwrap();
        if string(f(row, "id")) == "delay" {
            scheduler.run_tick_once().unwrap();
        } else {
            scheduler.start().unwrap();
        }
        scheduler.stop();
        assert_eq!(
            after_files(&paths.project_dir),
            *f(row, "after"),
            "scheduler {}",
            string(f(row, "id"))
        );
    }
}

#[test]
fn scheduler_pre_registration_is_removed_when_the_first_clock_panics() {
    let oracle = parse_json(ORACLE).unwrap();
    let mut row = array(f(&oracle, "rows"))[0].clone();
    let mut input = f(&row, "input").clone();
    set(&mut input, "preRegister", JsValue::Bool(true));
    set(&mut input, "panicFirstClock", JsValue::Bool(true));
    set(&mut row, "input", input);
    let (actual, read) = run_row(&row, None);
    assert_eq!(read, 0);
    assert_eq!(
        string(f(f(&actual, "error"), "message")),
        "fixture first clock panic"
    );
    assert!(array(f(&actual, "gatewayCalls")).is_empty());
    assert!(array(f(&actual, "active")).is_empty());
}
