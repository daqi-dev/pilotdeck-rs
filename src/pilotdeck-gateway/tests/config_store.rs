//! Replays the real TypeScript PilotConfigStore recording
//! (`fixtures/config-store/oracle.json`) step by step against
//! `config::store::ConfigStore`. Each step's result object is rebuilt in the
//! generator's key order and compared as serialized bytes (no key sorting).
//! Time runs on Tokio's paused clock: `wait` advances it, and the watcher's
//! polling and debounce timers fire deterministically without real sleeps.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::config::{
    Env,
    store::{
        ConfigStore, Listener, ReloadEvent, ReloadResult, StoreOptions, Subscription, WatchHandle,
        WatchOptions,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    collections::HashMap,
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/config-store/oracle.json");
const SCENARIOS: usize = 15;

type Pending = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Shared by every listener of one scenario, like the generator's closures.
struct Ctx {
    root: PathBuf,
    store: OnceLock<ConfigStore>,
    events: Mutex<Vec<JsValue>>,
    listeners: Mutex<HashMap<String, Listener>>,
    subscriptions: Mutex<HashMap<String, Subscription>>,
    pending: Mutex<Vec<Pending>>,
}

fn obj(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in entries {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn undefined_tag() -> JsValue {
    obj(vec![("$", "undefined".into())])
}
fn text(value: Option<&JsValue>) -> Option<String> {
    value?.as_string()?.to_utf8().ok()
}
fn num(snapshot: &JsValue) -> JsValue {
    snapshot.get("version").cloned().expect("version")
}
fn store(ctx: &Ctx) -> &ConfigStore {
    ctx.store.get().expect("store created")
}

/// Replace the scenario root with `<ROOT>` and encode `undefined` members as
/// the generator does.
fn normalize(value: &JsValue, root: &[u16]) -> JsValue {
    match value {
        JsValue::String(s) => JsValue::String(replace(s, root)),
        JsValue::Array(items) => JsValue::Array(items.iter().map(|v| normalize(v, root)).collect()),
        JsValue::Object(o) => {
            let mut out = JsObject::new();
            for (k, v) in o.entries() {
                let v = if v.is_undefined() {
                    undefined_tag()
                } else {
                    normalize(v, root)
                };
                out.insert(replace(k, root), v);
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}
fn replace(value: &JsString, needle: &[u16]) -> JsString {
    let with: Vec<u16> = "<ROOT>".encode_utf16().collect();
    let units = value.units();
    let mut out = vec![];
    let mut i = 0;
    while i < units.len() {
        if !needle.is_empty() && units[i..].starts_with(needle) {
            out.extend_from_slice(&with);
            i += needle.len();
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    JsString::from_utf16(out)
}
fn root_units(ctx: &Ctx) -> Vec<u16> {
    ctx.root.to_string_lossy().encode_utf16().collect()
}

fn error_record(ctx: &Ctx, error: &pilotdeck_gateway::config::Error) -> JsValue {
    let config_error = error.name == "PilotConfigError";
    let mut entries = vec![
        ("name", error.name.into()),
        // Only PilotConfigError carries `code`/`diagnostics` in the baseline;
        // the native error's `code` for other errors is a gateway wire code.
        (
            "code",
            if config_error {
                error.code.as_str().into()
            } else {
                undefined_tag()
            },
        ),
        ("message", JsValue::String(error.message.clone())),
    ];
    if config_error {
        entries.push(("diagnostics", JsValue::Array(error.diagnostics.clone())));
    }
    normalize(&obj(entries), &root_units(ctx))
}

fn event_record(ctx: &Ctx, name: &str, event: &ReloadEvent) -> JsValue {
    let store = store(ctx);
    let current = store.snapshot();
    obj(vec![
        ("listener", name.into()),
        ("previousVersion", num(&event.previous_snapshot)),
        ("nextVersion", num(&event.next_snapshot)),
        (
            "changedPaths",
            JsValue::Array(
                event
                    .changed_paths
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        ),
        (
            "changeClasses",
            JsValue::Array(
                event
                    .change_classes
                    .iter()
                    .map(|c| c.as_str().into())
                    .collect(),
            ),
        ),
        ("storeVersion", num(&current)),
        (
            "storeIsNext",
            JsValue::Bool(Arc::ptr_eq(&current, &event.next_snapshot)),
        ),
        (
            "diagnosticsCount",
            JsValue::Number(store.diagnostics().len() as f64),
        ),
    ])
}

fn make_listener(ctx: &Arc<Ctx>, name: String, behavior: Option<String>) -> Listener {
    let weak = Arc::downgrade(ctx);
    Arc::new(move |event: &ReloadEvent| {
        let ctx = weak.upgrade().expect("scenario context");
        let record = event_record(&ctx, &name, event);
        ctx.events.lock().unwrap().push(record);
        match behavior.as_deref() {
            Some("throw") => panic!("listener {name} failed"),
            Some("reentrant") => {
                let next = event.next_snapshot.clone();
                let reload = store(&ctx).reload("reentrant");
                let sink = Arc::downgrade(&ctx);
                let name = name.clone();
                ctx.pending.lock().unwrap().push(Box::pin(async move {
                    let result = match reload.await {
                        Ok(snapshot) if Arc::ptr_eq(&snapshot, &next) => "same-snapshot".to_owned(),
                        Ok(snapshot) => format!(
                            "version {}",
                            text(snapshot.get("version")).unwrap_or_default()
                        ),
                        Err(error) => format!("error {}", error.name),
                    };
                    let ctx = sink.upgrade().expect("scenario context");
                    ctx.events.lock().unwrap().push(obj(vec![
                        ("listener", name.as_str().into()),
                        ("reentrantResult", result.as_str().into()),
                    ]));
                }));
            }
            Some(b) if b.starts_with("subscribe:") => {
                let late = b["subscribe:".len()..].to_owned();
                if !ctx.listeners.lock().unwrap().contains_key(&late) {
                    let listener = make_listener(&ctx, late.clone(), None);
                    ctx.listeners
                        .lock()
                        .unwrap()
                        .insert(late.clone(), listener.clone());
                    let subscription = store(&ctx).subscribe(listener);
                    ctx.subscriptions.lock().unwrap().insert(late, subscription);
                }
            }
            Some(b) if b.starts_with("unsubscribe:") => {
                let victim = ctx
                    .subscriptions
                    .lock()
                    .unwrap()
                    .get(&b["unsubscribe:".len()..])
                    .cloned();
                if let Some(subscription) = victim {
                    subscription.unsubscribe();
                }
            }
            _ => {}
        }
    })
}

async fn drain_pending(ctx: &Ctx) {
    let pending = std::mem::take(&mut *ctx.pending.lock().unwrap());
    for future in pending {
        future.await;
    }
}

async fn run_step(
    ctx: &Arc<Ctx>,
    scenario: &JsValue,
    step: &JsValue,
    watch: &mut Option<WatchHandle>,
) -> JsValue {
    let op = text(step.get("op")).expect("op");
    let home = ctx.root.join("home");
    let project = ctx.root.join("project");
    let file = text(step.get("file")).map(|f| match f.as_str() {
        "global" => home.join("pilotdeck.yaml"),
        "project" => project.join(".pilotdeck/pilotdeck.yaml"),
        "projectConfigDir" => project.join(".pilotdeck"),
        "projectRoot" => project.clone(),
        other => panic!("unknown file {other}"),
    });
    let mut out: Vec<(&str, JsValue)> = vec![];
    match op.as_str() {
        "write" => {
            let file = file.unwrap();
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, text(step.get("yaml")).unwrap()).unwrap();
        }
        "remove" => match fs::remove_file(file.unwrap()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => panic!("remove: {e}"),
            _ => {}
        },
        "mkdir" => fs::create_dir_all(file.unwrap()).unwrap(),
        "create" => {
            let env = Env::from([
                ("HOME".to_owned(), home.to_string_lossy().into_owned()),
                ("PILOT_HOME".to_owned(), home.to_string_lossy().into_owned()),
            ]);
            let mut options = StoreOptions::new(&home, &ctx.root, env);
            if scenario.get("project").and_then(JsValue::as_bool) == Some(true) {
                options.project = Some(project.to_string_lossy().into_owned());
            }
            match ConfigStore::new(options) {
                Ok(store) => {
                    out.push(("version", num(&store.snapshot())));
                    out.push((
                        "diagnostics",
                        normalize(&JsValue::Array(store.diagnostics()), &root_units(ctx)),
                    ));
                    assert!(ctx.store.set(store).is_ok(), "one store per scenario");
                }
                Err(error) => out.push(("error", error_record(ctx, &error))),
            }
        }
        "subscribe" => {
            let name = text(step.get("name")).unwrap();
            let existing = ctx.listeners.lock().unwrap().get(&name).cloned();
            let listener = existing
                .unwrap_or_else(|| make_listener(ctx, name.clone(), text(step.get("behavior"))));
            ctx.listeners
                .lock()
                .unwrap()
                .insert(name.clone(), listener.clone());
            let subscription = store(ctx).subscribe(listener);
            ctx.subscriptions.lock().unwrap().insert(name, subscription);
        }
        "unsubscribe" => {
            let name = text(step.get("name")).unwrap();
            let subscription = ctx
                .subscriptions
                .lock()
                .unwrap()
                .get(&name)
                .cloned()
                .expect("subscribed");
            subscription.unsubscribe();
        }
        "reload" => {
            let count = step
                .get("count")
                .and_then(JsValue::as_number)
                .unwrap_or(1.0) as usize;
            // Issue every call first (as the generator does synchronously),
            // then let them run.
            let calls: Vec<_> = (0..count).map(|_| store(ctx).reload("manual")).collect();
            let handles: Vec<_> = calls.into_iter().map(tokio::spawn).collect();
            let mut results: Vec<ReloadResult> = vec![];
            for handle in handles {
                results.push(handle.await.expect("reload task"));
            }
            drain_pending(ctx).await;
            let records = results
                .iter()
                .map(|r| match r {
                    Ok(snapshot) => obj(vec![("version", num(snapshot))]),
                    Err(error) => obj(vec![("error", error_record(ctx, error))]),
                })
                .collect();
            let same = results.iter().all(|r| match (r, &results[0]) {
                (Ok(a), Ok(b)) => Arc::ptr_eq(a, b),
                (Err(a), Err(b)) => Arc::ptr_eq(a, b),
                _ => false,
            });
            out.push(("results", JsValue::Array(records)));
            out.push(("sameResult", JsValue::Bool(same)));
        }
        "snapshot" => out.push(("version", num(&store(ctx).snapshot()))),
        "diagnostics" => out.push((
            "diagnostics",
            normalize(&JsValue::Array(store(ctx).diagnostics()), &root_units(ctx)),
        )),
        "watch" => {
            *watch = Some(
                store(ctx)
                    .start_watching(WatchOptions::default())
                    .expect("#879 healthy watcher paths"),
            )
        }
        "unwatch" => watch.take().expect("watching").stop().await,
        "wait" => {
            let ms = step.get("ms").and_then(JsValue::as_number).expect("ms");
            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
            drain_pending(ctx).await;
        }
        other => panic!("unknown op {other}"),
    }
    out.push((
        "events",
        JsValue::Array(std::mem::take(&mut *ctx.events.lock().unwrap())),
    ));
    obj(out)
}

#[test]
fn config_store_replays_real_typescript_oracle() {
    let oracle = baseline::oracle(ORACLE);
    let JsValue::Array(scenarios) = oracle.get("scenarios").expect("scenarios") else {
        panic!("scenarios array")
    };
    assert_eq!(
        oracle.get("count").and_then(JsValue::as_number),
        Some(scenarios.len() as f64)
    );
    assert_eq!(scenarios.len(), SCENARIOS, "scenario count changed");
    let sources = oracle
        .get("sourceSha256")
        .and_then(JsValue::as_object)
        .expect("sourceSha256");
    assert!(
        sources
            .get("src/pilot/config/PilotConfigStore.ts")
            .is_some()
            && sources.get("src/pilot/config/classifyChanges.ts").is_some()
            && sources.get("src/pilot/config/loadPilotConfig.ts").is_some(),
        "the store's import closure must be hashed"
    );
    let base = unique_root();
    let mut failures = vec![];
    for (index, scenario) in scenarios.iter().enumerate() {
        let id = text(scenario.get("id")).expect("id");
        let root = base.join(index.to_string());
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let JsValue::Array(steps) = scenario.get("steps").expect("steps") else {
            panic!("steps")
        };
        let JsValue::Array(expected) = scenario.get("results").expect("results") else {
            panic!("results")
        };
        assert_eq!(steps.len(), expected.len(), "{id}: one result per step");
        // A fresh paused-clock runtime per scenario keeps timers isolated.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let ctx = Arc::new(Ctx {
            root: root.clone(),
            store: OnceLock::new(),
            events: Mutex::new(vec![]),
            listeners: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::new()),
            pending: Mutex::new(vec![]),
        });
        runtime.block_on(async {
            let mut watch = None;
            for (n, (step, expected)) in steps.iter().zip(expected).enumerate() {
                let actual = run_step(&ctx, scenario, step, &mut watch).await;
                let actual = stringify_json(&actual).unwrap().unwrap();
                let expected = stringify_json(expected).unwrap().unwrap();
                if actual != expected {
                    failures.push(format!(
                        "{id} step {n}:\n  actual   {actual}\n  expected {expected}"
                    ));
                }
            }
            if let Some(watch) = watch {
                watch.stop().await;
            }
        });
        drop(ctx);
        drop(runtime);
    }
    fs::remove_dir_all(&base).expect("remove owned root");
    assert!(
        failures.is_empty(),
        "{} step differences:\n{}",
        failures.len(),
        failures.into_iter().take(4).collect::<Vec<_>>().join("\n")
    );
}

/// Atomically create a root this process owns; never delete before owning.
fn unique_root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path: &Path =
            &std::env::temp_dir().join(format!("pdrs-config-store-{}-{n}", std::process::id()));
        match fs::create_dir(path) {
            Ok(()) => return path.to_path_buf(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create store temp root: {error}"),
        }
    }
}
