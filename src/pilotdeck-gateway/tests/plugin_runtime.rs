//! Replay the original A6.3 runtime corpus without rewriting its expectations.
//! JS-only Object.freeze and legacy outputStyles (not exposed by the Rust
//! snapshot type) are explicitly excluded; every other result and call is read
//! from the oracle. See fixtures/plugin-runtime/README.md for scope.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "../src/node_locale.rs"]
mod node_locale;
#[path = "common/oracle_locale.rs"]
mod oracle_locale;
use pilotdeck_gateway::{hooks::protocol::HooksSettings, runtime::plugins::*};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

// Shared replay setup stays attributable when the #1122 bound scenario fails
// before its order assertion. Keep #1010 markers for the existing controls.
const REPLAY_SETUP: &str = "#1010 #1122 plugin-runtime setup/observation must succeed";

fn js(value: &JsValue) -> Value {
    serde_json::from_str(
        &stringify_json(value)
            .expect(REPLAY_SETUP)
            .expect(REPLAY_SETUP),
    )
    .expect(REPLAY_SETUP)
}
fn error(e: &PluginRuntimeError) -> Value {
    let mut out =
        json!({"name":if e.is_aggregate(){"AggregateError"}else{"Error"},"message":e.message});
    if e.is_aggregate() {
        out["errors"] = json!(e.causes.iter().map(error).collect::<Vec<_>>());
    }
    out
}
fn command(c: &PluginCommandContribution) -> Value {
    let mut o = json!({"name":c.name,"content":c.content,"namespace":c.namespace});
    if let Some(d) = &c.description {
        o["description"] = json!(d);
    }
    if let Some(h) = &c.argument_hint {
        o["argumentHint"] = json!(h);
    }
    o
}
fn commands(c: &[PluginCommandContribution]) -> Value {
    json!(c.iter().map(command).collect::<Vec<_>>())
}
fn skills(c: &[PluginSkillContribution]) -> Value {
    json!(c.iter().map(|s|{
    let mut o=json!({"name":s.name,"content":s.content,"namespace":s.namespace,"path":s.path});
    if let Some(d)=&s.description{o["description"]=json!(d);}o
}).collect::<Vec<_>>())
}
fn hooks(h: &HooksSettings) -> Value {
    Value::Object(
        h.iter()
            .map(|(event, matchers)| {
                (
                    event.to_utf8().expect(REPLAY_SETUP),
                    json!(
                        matchers
                            .iter()
                            .map(|m| js(&m.to_value()))
                            .collect::<Vec<_>>()
                    ),
                )
            })
            .collect(),
    )
}
fn instruction_json<'a>(
    entries: impl Iterator<
        Item = (
            &'a pilotdeck_protocol::JsString,
            &'a pilotdeck_protocol::JsString,
        ),
    >,
) -> Value {
    let entries = entries
        .map(|(name, instructions)| {
            let mut object = JsObject::new();
            object.insert("serverName".into(), JsValue::String(name.clone()));
            object.insert("instructions".into(), JsValue::String(instructions.clone()));
            JsValue::Object(object)
        })
        .collect();
    Value::String(
        stringify_json(&JsValue::Array(entries))
            .expect(REPLAY_SETUP)
            .expect(REPLAY_SETUP),
    )
}
fn instructions(c: &[PluginMcpInstructionEntry]) -> Value {
    instruction_json(c.iter().map(|e| (&e.server_name, &e.instructions)))
}
fn encode_expected_instructions(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "mcpInstructions" {
                    let entries: Vec<_> = value
                        .as_array()
                        .expect(REPLAY_SETUP)
                        .iter()
                        .map(|entry| {
                            assert_eq!(
                                entry.as_object().expect(REPLAY_SETUP).len(),
                                2,
                                "#1010 #1122 MCP instruction replay fields"
                            );
                            (
                                pilotdeck_protocol::JsString::from(
                                    entry["serverName"].as_str().expect(REPLAY_SETUP),
                                ),
                                pilotdeck_protocol::JsString::from(
                                    entry["instructions"].as_str().expect(REPLAY_SETUP),
                                ),
                            )
                        })
                        .collect();
                    *value = instruction_json(entries.iter().map(|(name, text)| (name, text)));
                } else {
                    encode_expected_instructions(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(encode_expected_instructions),
        _ => {}
    }
}
fn ids(plugins: &[Arc<LoadedPlugin>]) -> Value {
    json!(
        plugins
            .iter()
            .map(|p| format!("{}@{}:{}", p.name, p.source.as_str(), p.path.display()))
            .collect::<Vec<_>>()
    )
}
fn report(r: &PluginRefreshResult) -> Value {
    json!({"generation":r.generation,"previous":ids(&r.previous),"next":ids(&r.next),"added":ids(&r.added),"removed":ids(&r.removed),"stagedFailures":r.staged_failures.iter().map(|f|json!({"path":f.path,"source":f.source.as_str(),"kind":match f.kind{loader::PluginKind::Plugin=>"plugin",loader::PluginKind::Skill=>"skill"}})).collect::<Vec<_>>()})
}
// Undefined is represented by the corpus's explicit tag, whereas Rust optional
// object properties are absent. Only object properties with that tag are elided.
fn normalize(v: Value, root: &str) -> Value {
    match v {
        Value::String(s) => json!(s.replace(root, "<ROOT>")),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| normalize(v, root)).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .filter(|(_, v)| v != &json!({"$":"undefined"}))
                .map(|(k, v)| (k, normalize(v, root)))
                .collect(),
        ),
        _ => v,
    }
}
#[derive(Clone)]
enum Lease {
    Catalog(Arc<AsyncMutex<PluginCommandCatalogLease>>),
    Session(Arc<AsyncMutex<PluginSessionContributionLease>>),
    Legacy(Arc<AsyncMutex<PluginContributionLease>>),
}
impl Lease {
    async fn release(&self) -> Result<Value, PluginRuntimeError> {
        match self {
            Self::Catalog(l) => l.lock().await.release().await?,
            Self::Session(l) => l.lock().await.release().await?,
            Self::Legacy(l) => l.lock().await.release().await?,
        }
        Ok(json!("released"))
    }
}
type Calls = Arc<Mutex<Vec<Value>>>;
fn event(c: &Calls, v: Value) {
    c.lock().expect(REPLAY_SETUP).push(v);
}
struct Listing {
    calls: Calls,
    runtime: Arc<PluginRuntime>,
    root: PathBuf,
    step: Value,
}
impl CommandCatalogListCommands for Listing {
    fn list(
        &self,
        input: CommandsListInput,
        pilot_home: String,
        c: Vec<PluginCommandContribution>,
    ) -> BoxCommandsListFuture {
        let mut i = json!({"projectKey":input.project_key});
        if let Some(v) = input.query {
            i["query"] = json!(v);
        }
        if let Some(v) = input.cursor {
            i["cursor"] = json!(v);
        }
        if let Some(v) = input.limit {
            i["limit"] = json!(v);
        }
        event(
            &self.calls,
            json!({"event":"listCommands","input":i,"pilotHome":pilot_home,"commands":commands(&c)}),
        );
        let (calls, runtime, root, step) = (
            self.calls.clone(),
            self.runtime.clone(),
            self.root.clone(),
            self.step.clone(),
        );
        Box::pin(async move {
            if step["list"] == "throw" {
                return Err(PluginRuntimeError::new("list failed"));
            }
            if step["list"] == "retire" {
                fs::write(
                    root.join(step["retire"].as_str().expect(REPLAY_SETUP)),
                    r#"{"name":"retired"}"#,
                )
                .expect(REPLAY_SETUP);
                let r = runtime.refresh_with_report().await?;
                event(
                    &calls,
                    json!({"event":"refreshedDuringList","generation":r.generation}),
                );
            }
            Ok(CommandsListResult {
                value: parse_json(&json!({"listed":c.len()}).to_string()).expect(REPLAY_SETUP),
            })
        })
    }
}
// JS async methods execute synchronously up to the first await. Poll the Rust
// future once in the caller before scheduling the remainder, so 'start' has
// the same admission ordering without sleeps or consulting expected results.
async fn start_eager(
    future: impl std::future::Future<Output = Result<Value, PluginRuntimeError>> + Send + 'static,
) -> JoinHandle<Result<Value, PluginRuntimeError>> {
    let mut future = Box::pin(future);
    let first = std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await;
    match first {
        std::task::Poll::Ready(value) => tokio::spawn(async move { value }),
        std::task::Poll::Pending => tokio::spawn(future),
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
async fn replay(s: &Value) {
    oracle_locale::require_en_us();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("pdrs-runtime-{}", uuid::Uuid::new_v4())));
    let root = &scratch.0;
    fs::create_dir_all(root.join("home")).expect(REPLAY_SETUP);
    fs::create_dir_all(root.join("project")).expect(REPLAY_SETUP);
    let calls: Calls = Default::default();
    let gates: HashMap<String, tokio::sync::watch::Sender<bool>> = s["builtins"]
        .as_array()
        .expect(REPLAY_SETUP)
        .iter()
        .filter_map(|b| b["gate"].as_str())
        .map(|gate| (gate.to_owned(), tokio::sync::watch::channel(false).0))
        .collect();
    let builtins = s["builtins"]
        .as_array()
        .expect(REPLAY_SETUP)
        .iter()
        .map(|b| {
            let name = b["name"].as_str().expect(REPLAY_SETUP).to_owned();
            let path = root.join(b["dir"].as_str().expect(REPLAY_SETUP));
            fs::create_dir_all(&path).expect(REPLAY_SETUP);
            let gate = b["gate"]
                .as_str()
                .map(|gate| gates.get(gate).expect(REPLAY_SETUP).subscribe());
            let throw_sync = b["throwSync"].as_bool().unwrap_or(false);
            let dispose = b["dispose"].as_str().map(|mode| {
                let (calls, name, mode) = (calls.clone(), name.clone(), mode.to_owned());
                Box::new(move || {
                    let (calls, name, mode, gate) =
                        (calls.clone(), name.clone(), mode.clone(), gate.clone());
                    if throw_sync {
                        event(&calls, json!({"event":"dispose","plugin":name}));
                        std::panic::panic_any(mode.clone());
                    }
                    Box::pin(async move {
                        event(&calls, json!({"event":"dispose","plugin":name}));
                        if let Some(mut gate) = gate {
                            while !*gate.borrow_and_update() {
                                gate.changed().await.expect(REPLAY_SETUP);
                            }
                            event(&calls, json!({"event":"disposeFinished","plugin":name}));
                        }
                        if mode == "ok" { Ok(()) } else { Err(mode) }
                    })
                        as std::pin::Pin<
                            Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
                        >
                }) as loader::DisposeFn
            });
            Arc::new(LoadedPlugin {
                name,
                path,
                source: PluginSource::Builtin,
                commands: vec![],
                skills: vec![],
                output_styles: vec![],
                hooks_config: None,
                mcp_servers: b
                    .get("mcpServers")
                    .map(|v| parse_json(&v.to_string()).expect(REPLAY_SETUP)),
                lsp_servers: None,
                dispose,
            })
        })
        .collect();
    if s["builtinSkills"] == true {
        fs::create_dir_all(root.join("builtin-skills")).expect(REPLAY_SETUP);
    }
    let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
        project_root: root.join("project"),
        pilot_home: root.join("home"),
        builtin_skills_root: (s["builtinSkills"] == true).then(|| root.join("builtin-skills")),
        builtin_plugins: builtins,
        builtin_plugins_enabled: s["enabled"]
            .as_object()
            .expect(REPLAY_SETUP)
            .iter()
            .map(|(k, v)| (k.clone(), v.as_bool().expect(REPLAY_SETUP)))
            .collect(),
    }));
    let mut leases = HashMap::<String, Lease>::new();
    let mut slots = HashMap::<String, JoinHandle<Result<Value, PluginRuntimeError>>>::new();
    let mut observed_calls = Vec::new();
    let mut expected_calls = Vec::new();
    for (index, (step, expected)) in s["steps"]
        .as_array()
        .expect(REPLAY_SETUP)
        .iter()
        .zip(s["results"].as_array().expect(REPLAY_SETUP))
        .enumerate()
    {
        let mut actual = json!({});
        let result: Option<Result<Value, PluginRuntimeError>> = match step["op"]
            .as_str()
            .expect(REPLAY_SETUP)
        {
            "write" => {
                let path = root.join(step["file"].as_str().expect(REPLAY_SETUP));
                fs::create_dir_all(path.parent().expect(REPLAY_SETUP)).expect(REPLAY_SETUP);
                fs::write(path, step["text"].as_str().expect(REPLAY_SETUP)).expect(REPLAY_SETUP);
                None
            }
            "rm" => {
                let path = root.join(step["file"].as_str().expect(REPLAY_SETUP));
                if path.is_dir() {
                    fs::remove_dir_all(path).expect(REPLAY_SETUP);
                } else {
                    fs::remove_file(path).expect(REPLAY_SETUP);
                }
                None
            }
            "refresh" => Some(runtime.refresh_with_report().await.map(|r| report(&r))),
            "state" => Some(Ok(
                json!({"generation":runtime.generation().await,"lifecycleState":runtime.lifecycle_state().await.as_str(),"snapshot":ids(&runtime.snapshot().await),"view":commands(&runtime.create_view(Vec::new()).await.all_commands()),"commands":commands(&runtime.all_commands().await),"skills":skills(&runtime.all_skills().await),"mcpServers":js(&JsValue::Object(runtime.mcp_servers().await)),"lspServers":js(&JsValue::Object(runtime.lsp_servers().await)),"mcpInstructions":instructions(&runtime.all_mcp_instructions().await)}),
            )),
            "acquire" => {
                let id = step["id"].as_str().expect(REPLAY_SETUP).to_owned();
                Some(match step["kind"].as_str().expect(REPLAY_SETUP){
                    "catalog"=>runtime.acquire_command_catalog().await.map(|l|{
                        let v=json!({"generation":l.generation,"contributions":{"generation":l.contributions.generation,"commands":commands(&l.contributions.commands)}});
                        leases.insert(id,Lease::Catalog(Arc::new(AsyncMutex::new(l))));v
                    }),
                    "session"=>runtime.acquire_session_contributions(Vec::new()).await.map(|l|{
                        let c=&l.contributions;
                        let v=json!({"generation":l.generation,"contributions":{"generation":c.generation,"tools":c.tools.len(),"routers":c.routers.len(),"commands":commands(&c.commands),"skills":skills(&c.skills),"prompts":c.prompts.len(),"hooks":hooks(&c.hooks),"mcpServers":js(&JsValue::Object(c.mcp_servers.clone())),"mcpInstructions":instructions(&c.mcp_instructions)}});
                        leases.insert(id,Lease::Session(Arc::new(AsyncMutex::new(l))));v
                    }),
                    "legacy"=>runtime.acquire_contribution_snapshot().await.map(|l|{
                        let c=&l.contributions;
                        let v=json!({"generation":l.generation,"contributions":{"generation":c.generation,"plugins":ids(&c.plugins),"tools":c.tools.len(),"routers":c.routers.len(),"commands":commands(&c.commands),"skills":skills(&c.skills),"hooks":hooks(&c.hooks),"mcpServers":js(&JsValue::Object(c.mcp_servers.clone())),"lspServers":js(&JsValue::Object(c.lsp_servers.clone())),"mcpInstructions":instruction_json(c.mcp_instructions.iter().map(|e|(&e.server_name,e.instructions.as_ref().expect(REPLAY_SETUP))))}});
                        leases.insert(id,Lease::Legacy(Arc::new(AsyncMutex::new(l))));v
                    }),_=>panic!("#1010 #1122 unknown lease")
                })
            }
            "release" => Some(
                leases
                    .get(step["id"].as_str().expect(REPLAY_SETUP))
                    .expect(REPLAY_SETUP)
                    .release()
                    .await,
            ),
            "start" => {
                let runtime = runtime.clone();
                let handle = match step["what"].as_str().expect(REPLAY_SETUP) {
                    "refresh" => {
                        start_eager(async move {
                            runtime.refresh_with_report().await.map(|r| report(&r))
                        })
                        .await
                    }
                    "dispose" => {
                        start_eager(async move { runtime.dispose().await.map(|()| json!("done")) })
                            .await
                    }
                    "release" => {
                        let lease = leases
                            .get(step["id"].as_str().expect(REPLAY_SETUP))
                            .expect(REPLAY_SETUP)
                            .clone();
                        start_eager(async move { lease.release().await.map(|_| json!("done")) })
                            .await
                    }
                    _ => panic!("#1010 #1122 unknown start"),
                };
                slots.insert(step["slot"].as_str().expect(REPLAY_SETUP).into(), handle);
                None
            }
            "await" => Some(
                slots
                    .remove(step["slot"].as_str().expect(REPLAY_SETUP))
                    .expect(REPLAY_SETUP)
                    .await
                    .expect(REPLAY_SETUP),
            ),
            "pending" => Some(Ok(json!(
                !slots
                    .get(step["slot"].as_str().expect(REPLAY_SETUP))
                    .expect(REPLAY_SETUP)
                    .is_finished()
            ))),
            "calls" => {
                let mut trace = observed_calls.clone();
                trace.extend(calls.lock().expect(REPLAY_SETUP).iter().cloned());
                Some(Ok(json!(trace)))
            }
            "open" => {
                gates
                    .get(step["gate"].as_str().expect(REPLAY_SETUP))
                    .expect(REPLAY_SETUP)
                    .send_replace(true);
                None
            }
            "flush" => {
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                None
            }
            "commandsList" => {
                let c = calls.clone();
                let project = root.join("project").to_string_lossy().into_owned();
                let resolver = Arc::new(move |key: String| {
                    event(&c, json!({"event":"resolveProjectKey","key":key}));
                    let project = project.clone();
                    Box::pin(async move {
                        if key == "missing" {
                            Err(PluginRuntimeError::new("Project not found: missing"))
                        } else {
                            Ok(project)
                        }
                    })
                        as std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Result<String, PluginRuntimeError>>
                                    + Send,
                            >,
                        >
                });
                let (c, r) = (calls.clone(), runtime.clone());
                let resolve_runtime = Arc::new(move |key: String| {
                    event(&c, json!({"event":"resolveRuntime","key":key}));
                    r.clone()
                });
                let c = calls.clone();
                let warn = Arc::new(move |message: &str, e: &PluginRuntimeError| {
                    event(
                        &c,
                        json!({"event":"warn","message":message,"error":error(e)}),
                    )
                });
                let bundle = GatewayCommandCatalogBundle::new(
                    root.join("home").to_string_lossy().into_owned(),
                    resolver,
                    resolve_runtime,
                    Arc::new(Listing {
                        calls: calls.clone(),
                        runtime: runtime.clone(),
                        root: root.clone(),
                        step: step.clone(),
                    }),
                    Some(warn),
                );
                let i = &step["input"];
                Some(
                    bundle
                        .commands_list(CommandsListInput {
                            project_key: i["projectKey"].as_str().expect(REPLAY_SETUP).into(),
                            query: i["query"].as_str().map(str::to_owned),
                            cursor: i["cursor"].as_str().map(str::to_owned),
                            limit: i["limit"]
                                .as_u64()
                                .map(|v| v.try_into().expect(REPLAY_SETUP)),
                        })
                        .await
                        .map(|r| js(&r.value)),
                )
            }
            _ => panic!("#1010 #1122 unknown step {step}"),
        };
        if let Some(r) = result {
            match r {
                Ok(v) => actual["result"] = v,
                Err(e) => actual["error"] = error(&e),
            }
        }
        observed_calls.extend(std::mem::take(&mut *calls.lock().expect(REPLAY_SETUP)));
        expected_calls.extend(
            expected["calls"]
                .as_array()
                .expect(REPLAY_SETUP)
                .iter()
                .cloned(),
        );
        let mut expected = expected.clone();
        expected
            .as_object_mut()
            .expect(REPLAY_SETUP)
            .remove("calls");
        if step["op"] == "acquire"
            && let Some(o) = expected.get_mut("result").and_then(Value::as_object_mut)
        {
            o.remove("frozen");
        }
        if step["op"] == "acquire"
            && step["kind"] == "legacy"
            && let Some(o) = expected
                .get_mut("result")
                .and_then(|r| r.get_mut("contributions"))
                .and_then(Value::as_object_mut)
        {
            assert_eq!(
                o.remove("outputStyles"),
                Some(json!([])),
                "#1010 #1122 legacy outputStyles has no Rust snapshot counterpart; nonempty values need their own implementation"
            );
        }
        encode_expected_instructions(&mut expected);
        assert_eq!(
            normalize(actual, root.to_str().expect(REPLAY_SETUP)),
            normalize(expected, root.to_str().expect(REPLAY_SETUP)),
            "#1010 #1122 {} step {index}: {step}",
            s["id"]
        );
    }
    for (_, slot) in slots {
        let _ = slot.await.expect(REPLAY_SETUP);
    }
    let _ = runtime.dispose().await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    observed_calls.extend(std::mem::take(&mut *calls.lock().expect(REPLAY_SETUP)));
    expected_calls.extend(
        s["teardown"]
            .as_array()
            .expect(REPLAY_SETUP)
            .iter()
            .cloned(),
    );
    for call in &mut expected_calls {
        if call["event"] == "listCommands" {
            call.as_object_mut().expect(REPLAY_SETUP).remove("frozen");
        }
    }
    // JS microtask vs tokio scheduling may move a callback across await/flush;
    // preserve the exact scenario-wide call order and every callback value.
    assert_eq!(
        normalize(json!(observed_calls), root.to_str().expect(REPLAY_SETUP)),
        normalize(json!(expected_calls), root.to_str().expect(REPLAY_SETUP)),
        "#1010 #1122 {} calls",
        s["id"]
    );
}
fn oracle() -> &'static Value {
    static FIXTURE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        js(&baseline::oracle(include_str!(
            "fixtures/plugin-runtime/oracle.json"
        )))
    })
}
#[test]
fn plugin_runtime_oracle_shape() {
    let fixture = oracle();
    assert_eq!(
        fixture["scenarios"]
            .as_array()
            .expect(REPLAY_SETUP)
            .iter()
            .map(|s| s["id"].as_str().expect(REPLAY_SETUP))
            .collect::<Vec<_>>(),
        REPLAY_IDS,
        "every scenario needs exactly one replay"
    );
    assert_eq!(
        fixture["scenarios"].as_array().expect(REPLAY_SETUP).len(),
        fixture["count"].as_u64().expect(REPLAY_SETUP) as usize
    );
    assert_eq!(
        fixture["scenarios"]
            .as_array()
            .expect(REPLAY_SETUP)
            .iter()
            .map(|s| s["steps"].as_array().expect(REPLAY_SETUP).len())
            .sum::<usize>(),
        fixture["steps"].as_u64().expect(REPLAY_SETUP) as usize
    );
    for s in fixture["scenarios"].as_array().expect(REPLAY_SETUP) {
        assert_eq!(
            s["steps"].as_array().expect(REPLAY_SETUP).len(),
            s["results"].as_array().expect(REPLAY_SETUP).len()
        );
    }
}
macro_rules! scenarios {
    ($(($name:ident, $id:literal $(, $reason:literal)?)),* $(,)?) => {
        const REPLAY_IDS: &[&str] = &[$($id),*];
        $(
            #[tokio::test]
            $(#[ignore = $reason])?
            async fn $name() {
                let s = oracle()["scenarios"].as_array().expect(REPLAY_SETUP).iter().find(|s| s["id"] == $id).expect(REPLAY_SETUP);
                tokio::time::timeout(Duration::from_secs(10), replay(s)).await.expect(concat!("#1010 #1122 ", $id, " timed out"));
            }
        )*
    };
}
scenarios! {
    (runtime_empty_directories, "empty-directories"),
    (runtime_command_discovery_and_precedence, "command-discovery-and-precedence"),
    (runtime_skills_hooks_and_mcp, "skills-hooks-and-mcp"),
    (runtime_staging_failures_keep_or_publish, "staging-failures-keep-or-publish"),
    (runtime_builtins_enabled_and_reloaded, "builtins-enabled-and-reloaded"),
    (runtime_leases_defer_retired_disposal, "leases-defer-retired-disposal"),
    (runtime_retired_dispose_failure_reaches_releaser, "retired-dispose-failure-reaches-releaser"),
    (runtime_unleased_dispose_failure_rejects_refresh, "unleased-dispose-failure-rejects-refresh"),
    (runtime_concurrent_refresh_is_single_flight, "concurrent-refresh-is-single-flight"),
    (runtime_dispose_drains_leases, "dispose-drains-leases"),
    (runtime_dispose_waits_for_pending_refresh, "dispose-waits-for-pending-refresh"),
    (runtime_dispose_retired_and_active_together, "dispose-retired-and-active-together"),
    (runtime_dispose_errors_aggregate, "dispose-errors-aggregate"),
    (runtime_dispose_single_error, "dispose-single-error"),
    (runtime_dispose_before_any_refresh, "dispose-before-any-refresh"),
    (runtime_dispose_active_order_and_parallel, "dispose-active-order-and-parallel"),
    (runtime_dispose_retired_generations, "dispose-retired-generations"),
    (runtime_dispose_mixed_errors_and_pending_callback, "dispose-mixed-errors-and-pending-callback"),
    (runtime_dispose_reactivated_retiree, "dispose-reactivated-retiree"),
    (runtime_dispose_sync_throw_with_retired_peer, "dispose-sync-throw-with-retired-peer"),
    (runtime_catalog_lists_one_generation, "catalog-lists-one-generation"),
    (runtime_catalog_list_failure_still_releases, "catalog-list-failure-still-releases"),
    (runtime_catalog_release_failure_only_warns, "catalog-release-failure-only-warns"),
    (runtime_catalog_resolve_failure_skips_refresh, "catalog-resolve-failure-skips-refresh"),
    (runtime_catalog_on_draining_runtime, "catalog-on-draining-runtime"),
}

#[tokio::test]
async fn plugin_runtime_locale_contract() {
    oracle_locale::replay_locale_contract("runtime_empty_directories", "plugin-runtime").await;
}
