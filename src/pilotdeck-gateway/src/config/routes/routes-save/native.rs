//! G0's synchronous file-worker bridge to one persistent native CONFIG owner.
use super::{ConfigRoutes, RouteError};
use crate::{
    dispatch::Route,
    metadata::{Files, Metadata, object, text_field},
};
use pilotdeck_protocol::{JsValue, stringify_json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
pub type SharedConfigRoutes = std::sync::Arc<Mutex<ConfigRoutes>>;
enum Registration {
    Unmanaged(SharedConfigRoutes),
    Installed(std::sync::Weak<Mutex<ConfigRoutes>>),
    Stopped,
}
impl Registration {
    fn state(&self) -> Option<SharedConfigRoutes> {
        match self {
            Self::Unmanaged(state) => Some(state.clone()),
            Self::Installed(state) => state.upgrade(),
            Self::Stopped => None,
        }
    }
}
type Owners = HashMap<(PathBuf, PathBuf), Registration>;
static OWNERS: OnceLock<Mutex<Owners>> = OnceLock::new();

/// Registry lookups must use the same canonical identity as installation.
fn owner_key(home: &std::path::Path, cwd: &std::path::Path) -> (PathBuf, PathBuf) {
    (
        home.canonicalize().unwrap_or_else(|_| home.to_path_buf()),
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
    )
}
static EXECUTOR: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
pub(crate) fn call_webui_blocking(
    owner: &SharedConfigRoutes,
    method: &str,
    params: &JsValue,
) -> super::Result {
    let mut owner = owner.lock().unwrap();
    executor()
        .block_on(owner.handle_webui(method, params))
        .expect("registered Web UI CONFIG method")
}
pub(crate) fn response<F: Files>(request: &JsValue, metadata: Option<&Metadata<F>>) -> Route {
    let result = (|| {
        let method = text_field(request, "method").expect("registered method");
        let fallback = Metadata::from_env();
        let (home, cwd) = metadata
            .map(|m| (&m.home, &m.cwd))
            .or_else(|| fallback.as_ref().map(|m| (&m.home, &m.cwd)))
            .ok_or_else(|| {
                RouteError::new(
                    "CONFIG_HOME_REQUIRED",
                    "PILOT_HOME is required for local configuration.",
                )
            })?;
        let mut owners = OWNERS.get_or_init(Default::default).lock().unwrap();
        let key = owner_key(home, cwd);
        let paired_key = (home.clone(), cwd.clone());
        if let std::collections::hash_map::Entry::Vacant(entry) = owners.entry(key.clone()) {
            let env = std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect();
            entry.insert(Registration::Unmanaged(std::sync::Arc::new(Mutex::new(
                ConfigRoutes::new(home.clone(), cwd.clone(), env)?,
            ))));
        }
        let owner = owners
            .get(&key)
            .and_then(Registration::state)
            .ok_or_else(|| RouteError::new("CONFIG_HOST_STOPPED", "Native CONFIG host stopped"))?;
        drop(owners);
        let mut owner = owner.lock().unwrap();
        if paired_active(&paired_key)
            && !owner.host_bound
            && !matches!(
                method.as_str(),
                "resolve_settings"
                    | "output_styles_list"
                    | "commands_list"
                    | "prepare_weixin_login"
            )
        {
            return Err(RouteError::new(
                "CONFIG_HOST_STATE_REQUIRED",
                "Connected CONFIG writes require the trusted native turn host and organization policy.",
            ));
        }
        executor()
            .block_on(owner.handle(&method, super::field(request, "params")))
            .expect("registered CONFIG method")
    })();
    let mut frame = object([
        ("type", "response".into()),
        ("id", super::field(request, "id").clone()),
        ("ok", JsValue::Bool(result.is_ok())),
    ]);
    let JsValue::Object(ref mut obj) = frame else {
        unreachable!()
    };
    match result {
        Ok(result) => {
            obj.insert("result".into(), result);
        }
        Err(error) => {
            obj.insert("error".into(), error.to_js());
        }
    }
    Route::LocalResponse(
        stringify_json(&frame)
            .expect("wire response")
            .expect("object")
            .into_bytes(),
    )
}

// A receiver is connection-owned; events are scoped to the same PilotHome/project.
type Notice = (PathBuf, PathBuf, JsValue);
fn events() -> &'static tokio::sync::broadcast::Sender<Notice> {
    static EVENTS: OnceLock<tokio::sync::broadcast::Sender<Notice>> = OnceLock::new();
    EVENTS.get_or_init(|| tokio::sync::broadcast::channel(256).0)
}
pub(crate) fn subscribe() -> tokio::sync::broadcast::Receiver<Notice> {
    events().subscribe()
}
pub(super) fn publish(home: &std::path::Path, cwd: &std::path::Path, notice: JsValue) {
    let _ = events().send((home.into(), cwd.into(), notice));
}

/// Install the very same state retained by the native turn/channel host.
pub fn install(routes: ConfigRoutes) -> SharedConfigRoutes {
    let key = owner_key(&routes.home, &routes.cwd);
    let state = std::sync::Arc::new(Mutex::new(routes));
    let previous = OWNERS.get_or_init(Default::default).lock().unwrap().insert(
        key,
        Registration::Installed(std::sync::Arc::downgrade(&state)),
    );
    if let Some(previous) = previous.and_then(|p| p.state()) {
        request_shutdown(previous);
    }
    start_watching(&state);
    state
}

fn start_watching(state: &SharedConfigRoutes) {
    use crate::config::store::{Listener, WatchOptions};
    let weak = std::sync::Arc::downgrade(state);
    let listener: Listener = std::sync::Arc::new(move |event| {
        let weak = weak.clone();
        let paths = event.changed_paths.clone();
        let snapshot = event.next_snapshot.clone();
        executor().spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let mut routes = state.lock().unwrap();
                if routes.stopped {
                    return;
                }
                routes.sync_runtime_env_from_snapshot(&snapshot);
                executor().block_on(routes.config_changed(&paths));
            })
            .await;
        });
    });
    let mut routes = state.lock().unwrap();
    routes.watch_subscription = Some(routes.store.subscribe(listener));
    routes.watcher = Some(
        routes
            .store
            .start_watching_on(WatchOptions::default(), executor().handle()),
    );
}

// An upstream's private busy state/policy cannot be inferred from authentication.
// Fail closed until the host explicitly binds the same native execution state.
type ScopeKey = (PathBuf, PathBuf);
fn paired() -> &'static Mutex<HashMap<ScopeKey, usize>> {
    static PAIRED: OnceLock<Mutex<HashMap<ScopeKey, usize>>> = OnceLock::new();
    PAIRED.get_or_init(Default::default)
}
fn paired_active(key: &ScopeKey) -> bool {
    paired().lock().unwrap().contains_key(key)
}
pub(crate) struct PairedScope(Option<ScopeKey>);
pub(crate) fn paired_scope<F: Files>(metadata: Option<&Metadata<F>>) -> PairedScope {
    let key = metadata.map(|m| (m.home.clone(), m.cwd.clone()));
    if let Some(key) = &key {
        *paired().lock().unwrap().entry(key.clone()).or_default() += 1;
    }
    PairedScope(key)
}
impl Drop for PairedScope {
    fn drop(&mut self) {
        if let Some(key) = &self.0 {
            let mut scopes = paired().lock().unwrap();
            if let Some(count) = scopes.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    scopes.remove(key);
                }
            }
        }
    }
}

fn executor() -> &'static tokio::runtime::Runtime {
    EXECUTOR.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("CONFIG executor")
    })
}
/// Session factories use the same store/SDK/style state as WS controls. Blocking
/// ownership stays off the async turn executor, including across style loading.
pub async fn session_input(
    state: SharedConfigRoutes,
    key: pilotdeck_protocol::JsString,
    project: Option<pilotdeck_protocol::JsString>,
) -> super::Result<super::SessionInput> {
    tokio::task::spawn_blocking(move || {
        let mut state = state.lock().unwrap();
        if state.stopped {
            return Err(RouteError::new(
                "CONFIG_HOST_STOPPED",
                "Native CONFIG host stopped",
            ));
        }
        executor().block_on(state.prepare_session_input(&key, project.as_ref()))
    })
    .await
    .map_err(|e| RouteError::new("gateway_request_failed", e.to_string()))?
}

pub(crate) fn installed(
    home: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<SharedConfigRoutes> {
    OWNERS
        .get()?
        .lock()
        .unwrap()
        .get(&owner_key(home, cwd))
        .and_then(Registration::state)
}

/// Unregister by identity so an old server cannot remove a replacement host.
fn retire(state: &SharedConfigRoutes) {
    let mut state_guard = state.lock().unwrap();
    state_guard.stopped = true;
    state_guard.stop_watching();
    state_guard.refresh_tasks.abort_all();
    let key = owner_key(&state_guard.home, &state_guard.cwd);
    let mut owners = OWNERS.get_or_init(Default::default).lock().unwrap();
    if owners
        .get(&key)
        .and_then(Registration::state)
        .is_some_and(|current| std::sync::Arc::ptr_eq(&current, state))
    {
        owners.insert(key, Registration::Stopped);
    }
}
pub async fn shutdown(state: SharedConfigRoutes) -> super::Result<()> {
    retire(&state);
    let (channel, owner) = {
        let state = state.lock().unwrap();
        (state.weixin_login.clone(), state.session_owner.clone())
    };
    let channel_result = match channel {
        Some(channel) => channel.shutdown().await,
        None => Ok(()),
    };
    let owner_result = match owner {
        Some(owner) => owner
            .shutdown()
            .await
            .map_err(|e| RouteError::new("gateway_request_failed", e.message)),
        None => Ok(()),
    };
    channel_result.and(owner_result)
}
pub(crate) fn request_shutdown(state: SharedConfigRoutes) {
    retire(&state);
    executor().spawn(async move {
        if let Err(error) = shutdown(state).await {
            eprintln!("Native CONFIG shutdown failed: {}", error.code);
        }
    });
}

/// Async hosts enter the same serialized CONFIG executor as local WS controls.
pub async fn call(state: SharedConfigRoutes, method: String, params: JsValue) -> super::Result {
    tokio::task::spawn_blocking(move || {
        let mut state = state.lock().unwrap();
        executor()
            .block_on(state.handle(&method, &params))
            .unwrap_or_else(|| {
                Err(RouteError::new(
                    "CONFIG_METHOD_NOT_FOUND",
                    "Unknown CONFIG method",
                ))
            })
    })
    .await
    .map_err(|e| RouteError::new("gateway_request_failed", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Env;
    const CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn installed_owner_watches_external_config_edits_and_stops_with_owner() {
        let root =
            std::env::temp_dir().join(format!("config-native-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pilotdeck.yaml");
        std::fs::write(&path, CONFIG).unwrap();
        let routes = ConfigRoutes::new(
            root.clone(),
            root.clone(),
            Env::from([("PILOT_HOME".into(), root.to_string_lossy().into_owned())]),
        )
        .unwrap();
        let state = install(routes);
        let mut notices = state.lock().unwrap().subscribe();
        std::fs::write(
            &path,
            format!("{CONFIG}runPolicy: {{failureGuard: {{enabled: false}}}}\n"),
        )
        .unwrap();
        let notice = tokio::time::timeout(std::time::Duration::from_secs(5), notices.recv())
            .await
            .expect("watcher notification timeout")
            .unwrap();
        assert_eq!(
            notice.get("name").and_then(JsValue::as_string),
            Some(&"config_changed".into())
        );
        let paths = notice
            .get("payload")
            .and_then(|payload| payload.get("changedPaths"))
            .and_then(|paths| match paths {
                JsValue::Array(paths) => Some(paths),
                _ => None,
            })
            .unwrap();
        assert!(paths.iter().any(|path| {
            path.as_string().is_some_and(|path| {
                path.units()
                    .starts_with(&"runPolicy".encode_utf16().collect::<Vec<_>>())
            })
        }));
        shutdown(state).await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_claimed_config_methods_dispatch_locally_and_connected_writes_fail_closed_without_host()
     {
        let root = std::env::temp_dir().join(format!("config-dispatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("pilotdeck.yaml"), CONFIG).unwrap();
        let metadata = Metadata {
            home: root.clone(),
            cwd: root.clone(),
            files: crate::metadata::NativeFiles,
        };
        tokio::task::spawn_blocking(move || {
            for method in super::super::METHODS {
                let request = object([
                    ("type", "request".into()),
                    ("id", "regression".into()),
                    ("method", (*method).into()),
                    ("params", object([])),
                ]);
                let Route::LocalResponse(bytes) =
                    crate::dispatch::route_with_metadata(&request, Some(&metadata))
                else {
                    panic!("{method} was forwarded");
                };
                let response = crate::dispatch::parse(&bytes).unwrap();
                assert_ne!(
                    response.get("error").and_then(|e| e.get("code")),
                    Some(&"CAPABILITY_UNAVAILABLE".into()),
                    "{method}"
                );
            }
            let scope = paired_scope(Some(&metadata));
            let request = object([
                ("type", "request".into()),
                ("id", "guard".into()),
                ("method", "update_settings".into()),
                (
                    "params",
                    object([("source", "localSettings".into()), ("settings", object([]))]),
                ),
            ]);
            let Route::LocalResponse(bytes) =
                crate::dispatch::route_with_metadata(&request, Some(&metadata))
            else {
                panic!("guard forwarded");
            };
            assert_eq!(
                crate::dispatch::parse(&bytes)
                    .unwrap()
                    .get("error")
                    .unwrap()
                    .get("code"),
                Some(&"CONFIG_HOST_STATE_REQUIRED".into())
            );
            drop(scope);
            std::fs::remove_dir_all(root).unwrap();
        })
        .await
        .unwrap();
    }
}
