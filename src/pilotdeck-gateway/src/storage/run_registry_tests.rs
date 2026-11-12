//! FileRunRegistry contract tests. Scenario outcomes and snapshot bytes come
//! from the real TS registry (tests/fixtures/run-registry/generate.mts).
use super::ledger::field;
use super::*;
use crate::metadata::object;
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use regex::{Captures, Regex};
use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, SystemTime},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/run-registry");
const DEAD: f64 = 2_147_483_647.0;
const HUGE: f64 = 2_147_483_648.0;
const SHORT: Duration = Duration::from_millis(300);

fn oracle() -> JsValue {
    let path = std::env::var_os("PDRS_RUN_REGISTRY_ORACLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{FIXTURES}/oracle.json")));
    let text = fs::read_to_string(path).unwrap();
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    crate::test_baseline::oracle(&text)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("pdrs-registry-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf {
        self.0.join("registry.json")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    lock.into()
}
fn string(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn run(project: Option<&str>, session: &str, run_id: &str) -> RunRef {
    RunRef {
        project_key: project.map(JsString::from),
        session_key: session.into(),
        run_id: run_id.into(),
    }
}
fn project_run(run_id: &str) -> RunRef {
    run(Some("project"), "session", run_id)
}
fn accept_input(run: &RunRef, material: &str) -> RunAcceptInput {
    RunAcceptInput {
        session_key_raw: JsValue::String(run.session_key.clone()),
        run: run.clone(),
        request_material: material.into(),
    }
}
fn event(json: &str) -> JsValue {
    parse_json(json).unwrap()
}
fn ref_of(input: &JsValue) -> RunRef {
    RunRef {
        project_key: input
            .get("projectKey")
            .map(|v| v.as_string().unwrap().clone()),
        session_key: field(input, "sessionKey").as_string().unwrap().clone(),
        run_id: field(input, "runId").as_string().unwrap().clone(),
    }
}
fn no_temporaries(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.ends_with(".tmp"), "leftover temporary {name}");
    }
}
fn set_age(path: &Path, age: Duration) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

/// Masking mirrors generate.mts: only values this run wrote (its own pid,
/// timestamps absent from the initial file) are replaced. Initial fixtures never
/// contain the generator pid, so its own-pid rule is the same one.
struct Initial {
    times: HashSet<String>,
    pids: HashSet<String>,
}
impl Initial {
    fn of(text: &str) -> Self {
        let collect = |pattern: &str| {
            Regex::new(pattern)
                .unwrap()
                .captures_iter(text)
                .map(|c| c[1].to_owned())
                .collect()
        };
        Self {
            times: collect(r#""(?:acceptedAt|updatedAt)":"([^"\\]*)""#),
            pids: collect(r#""ownerPid":([0-9]+)"#),
        }
    }
    fn masks_pid(&self, value: &str, pid: f64) -> bool {
        value.parse::<f64>().ok() == Some(pid) && !self.pids.contains(value)
    }
}
fn mask_record(record: Option<&JsValue>, initial: &Initial) -> JsValue {
    let Some(record) = record else {
        return JsValue::Null;
    };
    let JsValue::Object(source) = record else {
        return record.clone();
    };
    let mut masked = source.clone();
    let own = f64::from(std::process::id());
    if let Some(JsValue::Number(pid)) = source.get("ownerPid")
        && initial.masks_pid(&pid.to_string(), own)
    {
        masked.insert("ownerPid".into(), "<self>".into());
    }
    for key in ["acceptedAt", "updatedAt"] {
        if let Some(JsValue::String(time)) = source.get(key)
            && !initial.times.contains(&time.to_utf8().unwrap())
        {
            masked.insert(key.into(), "<new>".into());
        }
    }
    JsValue::Object(masked)
}
fn mask_bytes(text: &str, initial: &Initial, pid: f64, check_new: bool) -> String {
    let iso = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap();
    let text = Regex::new(r#""(acceptedAt|updatedAt)":"([^"\\]*)""#)
        .unwrap()
        .replace_all(text, |c: &Captures| {
            if initial.times.contains(&c[2]) {
                c[0].to_owned()
            } else {
                if check_new {
                    assert!(iso.is_match(&c[2]), "Rust timestamp {}", &c[2]);
                }
                format!(r#""{}":"<new>""#, &c[1])
            }
        });
    Regex::new(r#""ownerPid":([0-9]+)"#)
        .unwrap()
        .replace_all(&text, |c: &Captures| {
            if initial.masks_pid(&c[1], pid) {
                r#""ownerPid":"<self>""#.to_owned()
            } else {
                c[0].to_owned()
            }
        })
        .into_owned()
}

fn step_outcome(
    registry: &FileRunRegistry,
    step: &JsValue,
    times: &Initial,
) -> std::result::Result<JsValue, RunRegistryError> {
    let input = field(step, "input");
    let ok = || object([("ok", JsValue::Bool(true))]);
    match string(field(step, "op")).as_str() {
        "accept" => registry
            .accept(&RunAcceptInput {
                session_key_raw: field(input, "sessionKey").clone(),
                run: ref_of(input),
                request_material: field(input, "requestMaterial").as_string().unwrap().clone(),
            })
            .map(|a| {
                object([
                    ("duplicate", JsValue::Bool(a.duplicate)),
                    ("record", mask_record(Some(a.record.as_json()), times)),
                ])
            }),
        "append" => registry
            .append(&ref_of(input), field(step, "event"))
            .map(|()| ok()),
        "get" => registry.get(&ref_of(input)).map(|r| {
            object([(
                "record",
                mask_record(r.as_ref().map(RunRecord::as_json), times),
            )])
        }),
        "events" => registry
            .events(
                &ref_of(input),
                field(step, "afterSeq").as_number(),
                field(step, "limit").as_number(),
            )
            .map(|events| object([("events", JsValue::Array(events))])),
        "mark" => registry.mark_orphans_interrupted().map(|()| ok()),
        other => panic!("unknown op {other}"),
    }
}
/// Fixture messages carry "<dir>/<scenario>.json" for the generator's scratch
/// registry; the Rust registry path is mapped to the same placeholder.
fn assert_error(
    error: &RunRegistryError,
    expected: &JsValue,
    context: &str,
    path: &Path,
    scenario: &str,
) {
    let name = string(field(expected, "name"));
    let message = string(field(expected, "message"));
    let portable = |m: &str| {
        m.replace(
            &path.display().to_string(),
            &format!("<dir>/{scenario}.json"),
        )
    };
    match name.as_str() {
        "RunRegistryError" => {
            assert_eq!(
                error.code().map(str::to_owned),
                Some(string(field(expected, "code"))),
                "{context}"
            );
            assert_eq!(portable(&error.to_string()), message, "{context}");
        }
        "Error" if message.starts_with("Timed out acquiring run registry lock: ") => {
            assert!(
                matches!(error, RunRegistryError::LockTimeout(_)),
                "{context}: {error:?}"
            );
            assert_eq!(portable(&error.to_string()), message, "{context}");
        }
        "Error" => {
            assert!(
                matches!(error, RunRegistryError::Invalid(_)),
                "{context}: {error:?}"
            );
            assert_eq!(portable(&error.to_string()), message, "{context}");
        }
        // SyntaxError/TypeError: a corrupt snapshot carries the TS builtin
        // name (message included); the mutate-path strict errors keep the
        // plain Invalid shape, where V8 wording is not claimed.
        "SyntaxError" | "TypeError" => match error {
            RunRegistryError::Named(actual, text) => {
                assert_eq!(*actual, name, "{context}");
                assert_eq!(portable(text), message, "{context}");
            }
            RunRegistryError::Invalid(_) => {}
            other => panic!("{context}: {name} {message} vs {other:?}"),
        },
        _ => panic!("{context}: unexpected error name {name}: {error:?}"),
    }
}

#[test]
fn real_ts_scenarios_match_outcomes_and_snapshot_bytes() {
    let fixture = oracle();
    let generator_pid = field(&fixture, "generatorPid").as_number().unwrap();
    assert_eq!(field(&fixture, "dead").as_number(), Some(DEAD));
    assert_eq!(field(&fixture, "huge").as_number(), Some(HUGE));
    let JsValue::Array(scenarios) = field(&fixture, "scenarios") else {
        panic!("missing scenarios")
    };
    assert_eq!(scenarios.len(), 25, "run-registry oracle scenario count");
    let own_pid = f64::from(std::process::id());
    for scenario in scenarios {
        let name = string(field(scenario, "name"));
        let scratch = Scratch::new();
        let path = scratch.path();
        let lock = lock_path(&path);
        let initial = match field(scenario, "initial") {
            JsValue::String(file) => {
                let bytes = fs::read(format!("{FIXTURES}/{}", file.to_utf8().unwrap())).unwrap();
                fs::write(&path, &bytes).unwrap();
                Some(bytes)
            }
            _ => None,
        };
        let times = Initial::of(&String::from_utf8_lossy(
            initial.as_deref().unwrap_or_default(),
        ));
        let has_lock = !matches!(field(scenario, "lock"), JsValue::Null);
        if has_lock {
            let spec = field(scenario, "lock");
            fs::write(&lock, string(field(spec, "contents"))).unwrap();
            let age = field(spec, "ageMs").as_number().unwrap();
            set_age(&lock, Duration::from_millis(age as u64));
        }
        let opened = FileRunRegistry::open_for_test(
            &path,
            if has_lock {
                SHORT
            } else {
                Duration::from_secs(30)
            },
        );
        let open_error = field(scenario, "openError");
        match opened {
            Err(error) => {
                assert!(
                    open_error.as_object().is_some(),
                    "{name}: TS opened, Rust {error:?}"
                );
                assert_error(&error, open_error, &format!("{name} open"), &path, &name);
                assert_eq!(fs::read(&path).ok(), initial, "{name}: bytes rewritten");
            }
            Ok(registry) => {
                assert_eq!(
                    open_error,
                    &JsValue::Null,
                    "{name}: TS rejected, Rust opened"
                );
                let JsValue::Array(steps) = field(scenario, "steps") else {
                    panic!("{name}: steps")
                };
                for (index, step) in steps.iter().enumerate() {
                    let context = format!("{name} step {} {:?}", index + 1, step);
                    let expected = field(step, "outcome");
                    match (step_outcome(&registry, step, &times), expected.get("error")) {
                        (Ok(actual), None) => assert_eq!(&actual, expected, "{context}"),
                        (Err(error), Some(expected)) => {
                            assert_error(&error, expected, &context, &path, &name)
                        }
                        (actual, _) => panic!("{context}: {actual:?} vs {expected:?}"),
                    }
                }
                match field(scenario, "after") {
                    JsValue::String(after) => {
                        let expected =
                            fs::read(format!("{FIXTURES}/{}", after.to_utf8().unwrap())).unwrap();
                        let actual = fs::read(&path).unwrap();
                        assert_eq!(
                            mask_bytes(&String::from_utf8(actual).unwrap(), &times, own_pid, true),
                            mask_bytes(
                                &String::from_utf8(expected).unwrap(),
                                &times,
                                generator_pid,
                                false
                            ),
                            "{name}: snapshot bytes"
                        );
                    }
                    _ => assert!(!path.exists(), "{name}: registry file created"),
                }
            }
        }
        assert_eq!(
            JsValue::Bool(lock.exists()),
            *field(scenario, "lockLeft"),
            "{name}: lock file"
        );
        no_temporaries(&scratch.0);
    }
}

#[test]
fn duplicate_run_is_replayed_not_executed_across_restart() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let target = project_run("run-1");
    let mut executed = 0;
    // The storage half of InProcessGateway.ts:904-916; not host integration.
    let mut submit = |registry: &FileRunRegistry,
                      material: &str|
     -> std::result::Result<Vec<JsValue>, RunRegistryError> {
        let accepted = registry.accept(&accept_input(&target, material))?;
        if !accepted.duplicate {
            executed += 1;
            registry.append(
                &target,
                &event(r#"{"type":"input_accepted","runId":"run-1"}"#),
            )?;
            registry.append(
                &target,
                &event(r#"{"type":"turn_completed","runId":"run-1","usage":{},"finishReason":"completed"}"#),
            )?;
        }
        registry.events(&target, None, None)
    };
    let first = submit(&FileRunRegistry::open(&path).unwrap(), "request").unwrap();
    let restarted = FileRunRegistry::open(&path).unwrap();
    let before = fs::read(&path).unwrap();
    let second = submit(&restarted, "request").unwrap();
    let conflict = submit(&restarted, "different").unwrap_err();
    assert_eq!(executed, 1);
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
    assert_eq!(conflict.code(), Some("conflict"));
    // Duplicates and conflicts do not write.
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!lock_path(&path).exists());

    // A run whose owner died mid-flight is fenced on restart and still never re-executed.
    let crashed = project_run("crashed");
    let registry = FileRunRegistry::open(&path).unwrap();
    registry.accept(&accept_input(&crashed, "crash")).unwrap();
    registry
        .append(
            &crashed,
            &event(r#"{"type":"input_accepted","runId":"crashed"}"#),
        )
        .unwrap();
    drop(registry);
    set_owner(&path, "crashed", DEAD);
    let registry = FileRunRegistry::open(&path).unwrap();
    let record = registry.get(&crashed).unwrap().unwrap();
    assert_eq!(record.state(), Some(RunState::Interrupted));
    assert_eq!(record.revision(), Some(3.0));
    assert_eq!(record.last_seq(), Some(1.0));
    let accepted = registry.accept(&accept_input(&crashed, "crash")).unwrap();
    assert!(accepted.duplicate);
    assert_eq!(accepted.record.state(), Some(RunState::Interrupted));
    let error = registry
        .append(&crashed, &event(r#"{"type":"input_accepted"}"#))
        .unwrap_err();
    assert_eq!(error.code(), Some("terminal"));
    assert_eq!(
        error.to_string(),
        "Run crashed is already terminal (interrupted)."
    );
}

/// Controlled mutation of a Rust-written snapshot: the owner process died.
fn set_owner(path: &Path, run_id: &str, pid: f64) {
    let snapshot = parse_json(&fs::read_to_string(path).unwrap()).unwrap();
    let JsValue::Array(records) = field(&snapshot, "records") else {
        panic!()
    };
    let records = records
        .iter()
        .map(|record| {
            let mut record = record.as_object().unwrap().clone();
            if record.get("runId").and_then(JsValue::as_string) == Some(&JsString::from(run_id)) {
                record.insert("ownerPid".into(), JsValue::Number(pid));
            }
            JsValue::Object(record)
        })
        .collect();
    let mut rewritten = snapshot.as_object().unwrap().clone();
    rewritten.insert("records".into(), JsValue::Array(records));
    fs::write(
        path,
        stringify_json(&JsValue::Object(rewritten))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
}

#[test]
fn owner_orphans_are_fenced_only_when_the_owner_is_dead() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let mine = project_run("mine");
    let first = FileRunRegistry::open(&path).unwrap();
    first.accept(&accept_input(&mine, "m")).unwrap();
    // A second registry in the same live process does not fence (spec:105-107).
    let second = FileRunRegistry::open(&path).unwrap();
    assert_eq!(
        second.get(&mine).unwrap().unwrap().state(),
        Some(RunState::Accepted)
    );
    for (owner, fenced) in [(HUGE, false), (1.0, false), (DEAD, true)] {
        set_owner(&path, "mine", owner);
        let before = fs::read(&path).unwrap();
        first.mark_orphans_interrupted().unwrap();
        let record = first.get(&mine).unwrap().unwrap();
        if fenced {
            assert_eq!(record.state(), Some(RunState::Interrupted));
            assert_eq!(record.revision(), Some(2.0));
        } else {
            assert_eq!(fs::read(&path).unwrap(), before, "owner {owner} is alive");
        }
    }
    // Already fenced: a second pass changes nothing.
    let before = fs::read(&path).unwrap();
    second.mark_orphans_interrupted().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn fresh_registry_creates_nothing_until_the_first_write() {
    let scratch = Scratch::new();
    let path = scratch.0.join("missing/home/registry.json");
    let registry = FileRunRegistry::open(&path).unwrap();
    let target = project_run("run-1");
    assert_eq!(registry.get(&target).unwrap(), None);
    assert!(registry.events(&target, None, None).unwrap().is_empty());
    registry.mark_orphans_interrupted().unwrap();
    assert!(!scratch.0.join("missing").exists());
    let error = registry.append(&target, &event("{}")).unwrap_err();
    assert_eq!(error.code(), Some("not_found"));
    assert_eq!(error.to_string(), "Run run-1 is not registered.");
    assert!(!lock_path(&path).exists());
    registry.accept(&accept_input(&target, "m")).unwrap();
    assert!(path.is_file());
    assert!(!lock_path(&path).exists());
}

#[test]
fn failed_persist_leaves_disk_and_later_reads_unchanged() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let target = project_run("run-1");
    let registry = FileRunRegistry::open(&path).unwrap();
    registry.accept(&accept_input(&target, "m")).unwrap();
    let before = fs::read(&path).unwrap();
    registry.fail_persist.store(true, Ordering::SeqCst);
    let error = registry
        .append(&target, &event(r#"{"type":"input_accepted"}"#))
        .unwrap_err();
    assert!(matches!(error, RunRegistryError::Io(_)), "{error:?}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!lock_path(&path).exists());
    no_temporaries(&scratch.0);
    let record = registry.get(&target).unwrap().unwrap();
    assert_eq!(record.state(), Some(RunState::Accepted));
    assert_eq!(record.last_seq(), Some(0.0));
    assert!(registry.events(&target, None, None).unwrap().is_empty());
    registry
        .append(&target, &event(r#"{"type":"input_accepted"}"#))
        .unwrap();
    assert_eq!(
        registry.get(&target).unwrap().unwrap().last_seq(),
        Some(1.0)
    );
}

#[test]
fn file_lock_steals_only_stale_dead_or_unparsable_owners() {
    let own = std::process::id();
    let stale = Duration::from_secs(120);
    for (contents, age, stolen) in [
        (format!(r#"{{"pid":{DEAD}}}"#), Duration::ZERO, false),
        (format!(r#"{{"pid":{own}}}"#), stale, false),
        (format!(r#"{{"pid":{HUGE}}}"#), stale, false),
        (
            r#"{"pid":99999999999999999999999999}"#.to_owned(),
            stale,
            false,
        ),
        (format!(r#"{{"pid":{DEAD}}}"#), stale, true),
        (r#"{"pid":0}"#.to_owned(), stale, true),
        (r#"{"pid":"12"}"#.to_owned(), stale, true),
        (String::new(), stale, true),
    ] {
        let scratch = Scratch::new();
        let path = scratch.path();
        let registry = FileRunRegistry::open_for_test(&path, SHORT).unwrap();
        registry
            .accept(&accept_input(&project_run("held"), "m"))
            .unwrap();
        let before = fs::read(&path).unwrap();
        let lock = lock_path(&path);
        fs::write(&lock, &contents).unwrap();
        set_age(&lock, age);
        let result = registry.accept(&accept_input(&project_run("next"), "m"));
        if stolen {
            result.unwrap();
            assert!(!lock.exists(), "{contents}");
        } else {
            let error = result.unwrap_err();
            assert!(
                matches!(&error, RunRegistryError::LockTimeout(m) if *m == format!("Timed out acquiring run registry lock: {}", lock.display())),
                "{contents}: {error:?}"
            );
            assert_eq!(fs::read(&path).unwrap(), before, "{contents}");
            assert_eq!(fs::read_to_string(&lock).unwrap(), contents);
            // Reads do not take the file lock (RunRegistry.ts:266-283).
            assert!(registry.get(&project_run("held")).unwrap().is_some());
        }
    }
}

#[test]
fn lock_file_records_owner_and_is_released_after_errors() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let registry = FileRunRegistry::open(&path).unwrap();
    let target = project_run("run-1");
    registry.accept(&accept_input(&target, "m")).unwrap();
    assert!(registry.accept(&accept_input(&target, "other")).is_err());
    assert!(
        registry
            .append(&project_run("missing"), &event("{}"))
            .is_err()
    );
    assert!(!lock_path(&path).exists());
    // A Rust-held lock carries the TS JSON owner shape that TS parses.
    let lock = registry.acquire_file_lock().unwrap();
    let mut contents = String::new();
    fs::File::open(lock_path(&path))
        .unwrap()
        .read_to_string(&mut contents)
        .unwrap();
    let owner = parse_json(&contents).unwrap();
    assert_eq!(
        field(&owner, "pid").as_number(),
        Some(f64::from(std::process::id()))
    );
    assert!(field(&owner, "acquiredAt").as_string().is_some());
    drop(lock);
    assert!(!lock_path(&path).exists());
}

/// TS creates the lock, temporary (renamed snapshot) and parent directory with
/// default modes, so a handoff peer can read the lock owner; Rust matches.
#[cfg(unix)]
#[test]
fn files_and_dirs_use_default_modes_like_ts() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let path = scratch.0.join("home/registry.json");
    let registry = FileRunRegistry::open(&path).unwrap();
    registry
        .accept(&accept_input(&project_run("run-1"), "m"))
        .unwrap();
    let lock = registry.acquire_file_lock().unwrap();
    // Same process umask, default creation modes.
    fs::write(scratch.0.join("plain"), "").unwrap();
    fs::create_dir(scratch.0.join("plain-dir")).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), mode(&scratch.0.join("plain")));
    assert_eq!(mode(&lock_path(&path)), mode(&scratch.0.join("plain")));
    assert_eq!(
        mode(&scratch.0.join("home")),
        mode(&scratch.0.join("plain-dir"))
    );
    drop(lock);
}

#[test]
fn concurrent_registries_never_lose_mutations() {
    let scratch = Scratch::new();
    let path = Arc::new(scratch.path());
    let shared = Arc::new(FileRunRegistry::open(&*path).unwrap());
    // Same instance: one accept wins, the other is a duplicate (spec:38-46).
    let duplicates: Vec<bool> = (0..2)
        .map(|_| {
            let shared = shared.clone();
            thread::spawn(move || {
                shared
                    .accept(&accept_input(&project_run("same"), "m"))
                    .unwrap()
                    .duplicate
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert_eq!(duplicates.iter().filter(|d| !**d).count(), 1);
    // Separate instances only share the file lock, like two Gateway processes.
    let workers: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|name| {
            let path = path.clone();
            thread::spawn(move || {
                let registry = FileRunRegistry::open(&*path).unwrap();
                let target = project_run(name);
                registry.accept(&accept_input(&target, name)).unwrap();
                for _ in 0..5 {
                    registry
                        .append(&target, &event(r#"{"type":"assistant_text_delta"}"#))
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    for name in ["a", "b", "c"] {
        let events = shared.events(&project_run(name), None, None).unwrap();
        let seqs: Vec<_> = events
            .iter()
            .map(|e| field(e, "seq").as_number().unwrap())
            .collect();
        assert_eq!(seqs, [1.0, 2.0, 3.0, 4.0, 5.0], "{name}");
        let record = shared.get(&project_run(name)).unwrap().unwrap();
        assert_eq!(record.state(), Some(RunState::Running));
        assert_eq!(record.revision(), Some(6.0));
    }
    assert!(!lock_path(&path).exists());
    no_temporaries(&scratch.0);
}

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn real_ts_reads_rust_registry_and_honours_its_lock() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let scratch = Scratch::new();
    let path = scratch.path();
    {
        let registry = FileRunRegistry::open(&path).unwrap();
        let run1 = project_run("run-1");
        registry.accept(&accept_input(&run1, "request-1")).unwrap();
        for json in [
            r#"{"type":"input_accepted","runId":"run-1"}"#,
            r#"{"type":"assistant_text_delta","runId":"run-1","text":"中文\ud800"}"#,
            r#"{"type":"turn_completed","runId":"run-1","usage":{},"finishReason":"completed"}"#,
        ] {
            registry.append(&run1, &event(json)).unwrap();
        }
        registry
            .accept(&accept_input(&project_run("rust-live"), "live"))
            .unwrap();
        registry
            .accept(&accept_input(&project_run("rust-dead"), "dead"))
            .unwrap();
        registry
            .append(
                &project_run("rust-dead"),
                &event(r#"{"type":"input_accepted"}"#),
            )
            .unwrap();
    }
    set_owner(&path, "rust-dead", DEAD);
    let before = fs::read(&path).unwrap();
    let lock = lock_path(&path);
    fs::write(&lock, format!(r#"{{"pid":{}}}"#, std::process::id())).unwrap();
    let mut child = std::process::Command::new("node")
        .arg("--import")
        .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
        .arg(format!("{FIXTURES}/generate.mts"))
        .arg(&path)
        .arg("--verify-rust")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    if line.trim() != "WAITING" {
        let output = child.wait_with_output().unwrap();
        panic!("{line}{}", String::from_utf8_lossy(&output.stderr));
    }
    thread::sleep(SHORT);
    assert!(
        child.try_wait().unwrap().is_none(),
        "TS must wait for the Rust lock"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::remove_file(&lock).unwrap();
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let output = child.wait_with_output().unwrap();
    println!("{rest}{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success());
    assert!(rest.contains("PASS"));

    // TS exited: its accepted run is now an orphan for the next Rust open.
    let registry = FileRunRegistry::open(&path).unwrap();
    let state = |id: &str| registry.get(&project_run(id)).unwrap().unwrap().state();
    assert_eq!(state("ts-run"), Some(RunState::Interrupted));
    assert_eq!(state("rust-live"), Some(RunState::Completed));
    assert_eq!(state("rust-dead"), Some(RunState::Interrupted));
    assert_eq!(state("run-1"), Some(RunState::Completed));
    let replay = registry
        .accept(&accept_input(&project_run("run-1"), "request-1"))
        .unwrap();
    assert!(replay.duplicate);
    assert_eq!(
        registry
            .events(&project_run("ts-run"), None, None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn million_level_arrays_fail_locally_without_changing_registry_or_leaving_locks() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let deep = format!("{}1{}", "[".repeat(1_000_000), "]".repeat(1_000_000));
    let oracle = parse_json(include_str!("../../tests/fixtures/runtime-boundaries.json")).unwrap();
    for key in ["projectKey", "sessionKey", "runId"] {
        let text = format!(
            r#"{{"schemaVersion":1,"records":[{{"state":"completed","{key}":{deep}}}],"events":{{}}}}"#
        );
        fs::write(&path, &text).unwrap();
        let expected = oracle.get("registryErrors").unwrap().get(key).unwrap();
        let Err(RunRegistryError::Invalid(message)) = FileRunRegistry::open(&path) else {
            panic!("deep {key} must return a local RangeError message");
        };
        assert_eq!(expected.get("name"), Some(&"RangeError".into()));
        assert_eq!(expected.get("code"), Some(&JsValue::Null));
        assert_eq!(expected.get("message"), Some(&message.as_str().into()));
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!lock_path(&path).exists());
        no_temporaries(&scratch.0);
    }
}

#[test]
fn registry_mkdir_failure_matches_real_ts_caller() {
    let raw = include_str!("../../tests/fixtures/mkdir-boundary/oracle.json");
    crate::test_baseline::oracle(raw);
    let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
    let row = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "registry")
        .unwrap();
    let scratch = Scratch::new();
    let root = fs::canonicalize(&scratch.0).unwrap();
    let dir = root.join("registry");
    fs::create_dir(&dir).unwrap();
    let registry = FileRunRegistry::open(dir.join("block/child/registry.json")).unwrap();
    fs::write(dir.join("block"), b"block").unwrap();
    let error = registry
        .accept(&accept_input(&project_run("r"), "m"))
        .unwrap_err();
    let RunRegistryError::Io(source) = &error else {
        panic!("{error:?}");
    };
    assert_eq!(
        crate::node_io::lookup(source).unwrap().0,
        row["observed"]["code"]
    );
    assert_eq!(
        error
            .to_string()
            .replace(&root.to_string_lossy().into_owned(), "<dir>"),
        row["observed"]["message"]
    );
}

// #216 retains the TS stale-lock policy; the two-process TS probe records the
// known race separately. This replay pins ordinary dead-owner reclamation.
#[test]
fn registry_reclaims_dead_stale_lock_like_ts() {
    let raw = include_str!("../../tests/fixtures/run-registry-stale-race/oracle.json");
    crate::test_baseline::oracle(raw);
    let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
    let expected = &fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "dead-stale-replaced")
        .unwrap()["observed"];
    let scratch = Scratch::new();
    let path = scratch.path();
    let registry = FileRunRegistry::open_for_test(&path, SHORT).unwrap();
    let lock_path = lock_path(&path);
    fs::write(&lock_path, format!(r#"{{"pid":{DEAD}}}"#)).unwrap();
    set_age(&lock_path, Duration::from_secs(120));
    let held = registry.acquire_file_lock().unwrap();
    let owner: serde_json::Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    assert_eq!(
        serde_json::json!({"acquired":true,"ownerIsSelf":owner["pid"] == std::process::id()}),
        *expected
    );
    drop(held);
    assert!(!lock_path.exists());
}

// #2692: the submit side hands the registry the template spelling as the
// dedupe key and the raw value as the record body, matching TS's split
// (RunRegistry.ts:70 key template vs :221-229 `...input` spread). The disk
// bytes keep JSON null while the key spells "null", and a reopen re-keys from
// the body so both spellings stay separate (ts-deepkey-probe A/B).
#[test]
fn null_session_key_body_keeps_json_null_while_the_key_spells_null() {
    let scratch = Scratch::new();
    let path = scratch.path();
    let registry = FileRunRegistry::open(&path).unwrap();
    let accepted = registry
        .accept(&RunAcceptInput {
            session_key_raw: JsValue::Null,
            run: run(None, "null", "r"),
            request_material: "m".into(),
        })
        .unwrap();
    assert!(!accepted.duplicate, "#2692 first accept writes the record");
    let record = registry
        .get(&run(None, "null", "r"))
        .unwrap()
        .expect("#2692 the null spelling reads the record back");
    assert_eq!(
        field(record.as_json(), "sessionKey"),
        &JsValue::Null,
        "#2692 the record body keeps the raw JSON null"
    );
    // The blank spelling is a different key: no fold, no record.
    assert!(
        registry.get(&run(None, "", "r")).unwrap().is_none(),
        "#2692 the blank spelling does not alias the null spelling"
    );
    let bytes = fs::read(&path).unwrap();
    assert!(
        String::from_utf8_lossy(&bytes).contains(r#""sessionKey":null"#),
        "#2692 the disk bytes keep the JSON null body"
    );
    drop(registry);
    // A fresh instance re-keys from the body: `null` templates to "null"
    // again, so the reopened registry still separates the two spellings.
    let reopened = FileRunRegistry::open(&path).unwrap();
    assert!(
        reopened.get(&run(None, "null", "r")).unwrap().is_some(),
        "#2692 reopen re-keys the null spelling"
    );
    assert!(
        reopened.get(&run(None, "", "r")).unwrap().is_none(),
        "#2692 reopen keeps the blank spelling separate"
    );
    // An absent sessionKey writes no body property (JSON drops undefined)
    // while the key spells "undefined".
    let accepted = reopened
        .accept(&RunAcceptInput {
            session_key_raw: JsValue::Undefined,
            run: run(None, "undefined", "u"),
            request_material: "m".into(),
        })
        .unwrap();
    assert!(!accepted.duplicate, "#2692 the absent spelling accepts");
    let bytes = fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains(r#""runId":"u""#),
        "#2692 the undefined-spelling record is on disk"
    );
    assert!(
        !text.contains("sessionKey\":undefined"),
        "#2692 the undefined body never serializes"
    );
    let record = reopened
        .get(&run(None, "undefined", "u"))
        .unwrap()
        .expect("#2692 the undefined spelling reads back");
    assert_eq!(
        field(record.as_json(), "sessionKey"),
        &JsValue::Undefined,
        "#2692 the absent body field reads as undefined"
    );
}

// #2692 fix-r1 matrix row 15 (ts-prodkey-probe e): a submit accept can land
// a deep-array sessionKey record (TS persists it before the result clone
// throws), so one can sit on disk when the registry reopens. V8's key
// template join folds 2560 levels of nesting (array_nesting caliber) to ""
// and throws past that, measured at every one of the registry's real load
// call shapes — the boot chain (constructor → initialize → withFileLock →
// refreshFromDisk → the key template, fix-r6) and get / events / accept's
// own re-reads plus a 50-frame synchronous prefix under get() (fix-r7,
// review boundary#1: all five chains share the edge because the template
// runs in refreshFromDisk's post-await resumption frame, whose stack is
// caller-independent), on the baseline's own FileRunRegistry, Node 22.22.0
// (`_tmp/20261026-issue-2692/fix-r6/node-probe/fold-band-loadpoint.out`,
// `fold-band-bisect.out`, and
// `_tmp/20261026-issue-2692/fix-r7/node-probe/fold-band-reads.out`; the
// previous 2621 was a shallower-stack probe artifact, fix-r6 review
// boundary#1); Rust cannot re-spell past the shared 2000-level bound at
// all. Inside the fold band the record is skipped — TS keeps it under the
// folded "" key, where a folded-spelling get returns it through the record
// clone up to 2557 levels and the clone throws the same RangeError from
// 2558 (`fix-r7/node-probe/fold-band-inband-reads.out`), so "single record
// invisible, the rest readable" is the registered Rust-side shape
// (registered divergence). Past the band the load fails like TS's get does
// on every read face (#251 boundary oracle). fix-r5 calibrates the band to
// the join's own frame count (one per array, `array_nesting`): a scalar
// leaf and an empty innermost array throw at the same level on a fixed
// caller stack (`fix-r5/node-probe/fold-band-stackdep.out`), and fix-r6
// re-measured both shapes sharing the boundary at the real load call shape
// too, so both leaf shapes share every boundary below.
#[test]
fn deep_session_key_record_reopens_without_bricking_the_other_records() {
    let scratch = Scratch::new();
    let path = scratch.path();
    // `depth` counts arrays — the join-frame caliber — for either leaf shape.
    let snapshot_with = |depth: usize, empty_leaf: bool| {
        let deep = if empty_leaf {
            format!("{}[]{}", "[".repeat(depth - 1), "]".repeat(depth - 1))
        } else {
            format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
        };
        format!(
            r#"{{"schemaVersion":1,"records":[{{"sessionKey":null,"runId":"run-a1","state":"completed","requestMaterial":"m1"}},{{"sessionKey":{deep},"runId":"run-deep","state":"completed"}}],"events":{{}}}}"#
        )
    };
    // Inside the fold band, both leaf shapes: the null record stays readable,
    // the deep record is invisible under every spelling, and the skip is
    // counted. 2560 is the band's in-edge at every real load call shape
    // (fix-r7).
    for (depth, empty_leaf) in [(2001, false), (2560, false), (2560, true)] {
        let text = snapshot_with(depth, empty_leaf);
        fs::write(&path, &text).unwrap();
        let registry = FileRunRegistry::open(&path).unwrap();
        assert_eq!(
            registry.skipped_unrekeyable(),
            1,
            "#2692 depth {depth} sits inside V8's fold band, so the record is skipped, not fatal"
        );
        let record = registry
            .get(&run(None, "null", "run-a1"))
            .unwrap()
            .expect("#2692 the null record survives the deep record on disk");
        assert_eq!(
            field(record.as_json(), "sessionKey"),
            &JsValue::Null,
            "#2692 the null record keeps its JSON null body"
        );
        assert!(
            registry.get(&run(None, "", "run-deep")).unwrap().is_none(),
            "#2692 the skipped deep record is invisible under the blank spelling"
        );
        // A later mutation persists without the skipped record (registered:
        // TS re-persists it under the folded key; Rust cannot spell it).
        registry
            .accept(&accept_input(&run(None, "later", "run-later"), "m2"))
            .unwrap();
        let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
        assert!(
            text.contains(r#""runId":"run-a1""#),
            "#2692 the null record persists"
        );
        assert!(
            text.contains(r#""runId":"run-later""#),
            "#2692 the fresh record persists"
        );
        assert!(
            !text.contains(r#""runId":"run-deep""#),
            "#2692 the rewrite drops the skipped deep record (registered divergence)"
        );
    }
    // Past the fold band the load fails like TS's get (the #251 oracle's
    // RangeError) for either leaf shape, with the snapshot and the lock left
    // as they were. 2561 is exactly one past the band's in-edge, so the
    // empty-leaf case is the one a leaf-depth caliber under-counts (the old
    // container_depth shape) — the discriminator the registry-band-leaf-depth
    // negative-control row mutates back.
    for empty_leaf in [false, true] {
        let text = snapshot_with(2561, empty_leaf);
        fs::write(&path, &text).unwrap();
        let Err(RunRegistryError::Invalid(message)) = FileRunRegistry::open(&path) else {
            panic!("#2692 depth 2561 is past V8's fold band and must fail the load");
        };
        assert_eq!(
            message, "Maximum call stack size exceeded",
            "#2692 the load failure carries V8's RangeError text"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!lock_path(&path).exists());
        no_temporaries(&scratch.0);
    }
    // fix-r7 (review boundary#1): the band gates every read face, not just
    // the open — a snapshot that appears after the registry was opened (the
    // cross-process handoff the lock-and-reload exists for) is loaded by
    // get / events / accept on their own read chains, and TS measured those
    // chains sharing the boot boundary (fold-band-reads.out), so every face
    // bands identically here too: in-band the deep record is skipped and
    // the anchor stays readable, past the band the read fails with V8's
    // RangeError text.
    for empty_leaf in [false, true] {
        fs::remove_file(&path)
            .unwrap_or_else(|error| panic!("#2692 the spent snapshot must be removable: {error}"));
        let registry = FileRunRegistry::open(&path)
            .unwrap_or_else(|error| panic!("#2692 an empty home must open: {error}"));
        fs::write(&path, snapshot_with(2560, empty_leaf)).unwrap();
        let record = registry
            .get(&run(None, "null", "run-a1"))
            .unwrap_or_else(|error| {
                panic!("#2692 depth 2560 after open skips the deep record on the get face: {error}")
            })
            .expect("#2692 the anchor survives the deep record on the get face");
        assert_eq!(
            field(record.as_json(), "sessionKey"),
            &JsValue::Null,
            "#2692 the null record keeps its JSON null body on the get face"
        );
        assert!(
            registry
                .get(&run(None, "", "run-deep"))
                .unwrap_or_else(|error| panic!("#2692 the get face must read, not fail: {error}"))
                .is_none(),
            "#2692 the skipped deep record is invisible on the get face"
        );
        assert_eq!(
            registry.skipped_unrekeyable(),
            1,
            "#2692 the get face's own re-read counted the in-band skip"
        );
        fs::write(&path, snapshot_with(2561, empty_leaf)).unwrap();
        for face in ["get", "events", "accept"] {
            let result = match face {
                "get" => registry.get(&run(None, "null", "run-a1")).map(|_| ()),
                "events" => registry
                    .events(&run(None, "null", "run-a1"), None, None)
                    .map(|_| ()),
                _ => registry
                    .accept(&accept_input(&run(None, "null", "run-a1"), "m1"))
                    .map(|_| ()),
            };
            match result {
                Err(RunRegistryError::Invalid(message)) => assert_eq!(
                    message, "Maximum call stack size exceeded",
                    "#2692 depth 2561 past the band fails the {face} read with V8's RangeError text"
                ),
                _ => panic!("#2692 depth 2561 past the band must fail the {face} read"),
            }
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            snapshot_with(2561, empty_leaf),
            "#2692 the failed read leaves the snapshot bytes as they were"
        );
        assert!(!lock_path(&path).exists());
        no_temporaries(&scratch.0);
    }
}

// #2692 fix-r5 (review boundary#3, P0): the in-band skip used to be silent
// in production — the per-file counter was `#[cfg(test)]`-only. The process
// cumulative `unrekeyable_skips()` is the production half of the registered
// signal surface (R17): monotonic, so a reader takes a delta around an
// operation instead of trusting an absolute. Concurrent tests in this
// process skip their own records, so the assertions are deltas.
#[test]
fn unrekeyable_skips_advances_in_production_on_every_read() {
    let scratch = Scratch::new();
    let path = scratch.path();
    // In-band at the real load call shape: 2560 arrays (fix-r6; 2621 was
    // in-band only under the old shallow-probe constant).
    let deep = format!("{}[]{}", "[".repeat(2559), "]".repeat(2559));
    fs::write(
        &path,
        format!(
            r#"{{"schemaVersion":1,"records":[{{"sessionKey":{deep},"runId":"run-deep","state":"completed"}}],"events":{{}}}}"#
        ),
    )
    .unwrap();
    let before = unrekeyable_skips();
    let registry = FileRunRegistry::open(&path).unwrap();
    assert_eq!(
        registry.skipped_unrekeyable(),
        1,
        "#2692 the deep record sits inside the fold band, so the read skips it"
    );
    assert!(
        unrekeyable_skips() > before,
        "#2692 the production counter moved — the skip is not a test-only observable"
    );
    // Every read re-reads the snapshot, so the cumulative count advances
    // again (a per-read signal, not a once-per-process one).
    let between = unrekeyable_skips();
    assert!(
        registry.get(&run(None, "", "run-deep")).unwrap().is_none(),
        "#2692 the skipped record is invisible under the blank spelling"
    );
    assert!(
        unrekeyable_skips() > between,
        "#2692 the production counter moves on every read that skips"
    );
}
