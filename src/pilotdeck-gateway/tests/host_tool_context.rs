#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::host::{
    HostError, HostFuture,
    modules::{
        capability::{RuntimeContext, ToolContext},
        permission::PermissionContext,
        tool_context::*,
        turn::remember_steer_grants,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in fields {
        out.insert(key.into(), value);
    }
    out
}
fn val<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(obj(fields))
}
fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(v) = value {
        v
    } else {
        panic!("not array: {value:?}")
    }
}
fn text(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn yes(value: &JsValue, key: &str) -> bool {
    value.get(key).and_then(JsValue::as_bool) == Some(true)
}
fn strings(value: &JsValue) -> Vec<JsString> {
    array(value)
        .iter()
        .map(|v| v.as_string().unwrap().clone())
        .collect()
}
fn path_values(values: &[JsString]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}
fn normalized(value: &JsValue) -> JsValue {
    parse_json(
        &stringify_json(value)
            .unwrap()
            .unwrap_or_else(|| "null".into()),
    )
    .unwrap()
}
fn equal(actual: &JsValue, expected: &JsValue, name: &str) {
    fn same(a: &JsValue, b: &JsValue) -> bool {
        match (a, b) {
            (JsValue::Object(a), JsValue::Object(b)) => {
                a.len() == b.len()
                    && a.entries()
                        .iter()
                        .all(|(k, v)| b.get_key(k).is_some_and(|bv| same(v, bv)))
            }
            (JsValue::Array(a), JsValue::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
            }
            _ => a == b,
        }
    }
    assert!(
        same(&normalized(actual), expected),
        "{name}\nactual {actual:?}\nexpected {expected:?}"
    );
}
fn outcome(result: Result<JsValue, HostError>) -> JsValue {
    match result {
        Ok(value) => normalized(&val([("ok", JsValue::Bool(true)), ("value", value)])),
        Err(error) => val([
            ("ok", JsValue::Bool(false)),
            (
                "error",
                JsValue::String(
                    error
                        .module_error
                        .map(|e| e.message)
                        .unwrap_or_else(|| error.message.into()),
                ),
            ),
        ]),
    }
}
fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(a) => JsValue::Array(a.iter().map(decode).collect()),
        JsValue::Object(o) => {
            if let Some(n) = o.get("$number") {
                return JsValue::Number(match text(n).as_str() {
                    "nan" => f64::NAN,
                    "infinity" => f64::INFINITY,
                    _ => f64::NEG_INFINITY,
                });
            }
            if o.contains_key("$undefined") {
                return JsValue::Undefined;
            }
            let mut out = JsObject::new();
            if o.contains_key("$nonplain") {
                out.set_native_tag("Date");
            }
            if o.contains_key("$nullPrototype") {
                out.set_prototype(JsValue::Null);
            }
            if let Some(prototype) = o.get("$prototype") {
                out.set_prototype(decode(prototype));
            }
            for (key, value) in o.entries() {
                if !key.equals_str("$nullPrototype") && !key.equals_str("$prototype") {
                    out.insert(key.clone(), decode(value));
                }
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}
fn merge(base: &str, patch: &JsValue) -> JsObject {
    let mut base = parse_json(base).unwrap().as_object().unwrap().clone();
    if let Some(patch) = decode(patch).as_object() {
        for (k, v) in patch.entries() {
            base.insert(k.clone(), v.clone());
        }
    }
    base
}
fn handle(name: &str) -> RuntimeContext {
    Arc::new(val([("handle", name.into())]))
}
fn handle_value(value: &RuntimeContext) -> JsValue {
    value.downcast_ref::<JsValue>().unwrap().clone()
}
fn project_files(files: &ToolFileState) -> JsValue {
    val([
        (
            "readFileState",
            JsValue::Object(files.read_file_state.lock().unwrap().clone()),
        ),
        (
            "writeSnapshots",
            JsValue::Object(files.write_snapshots.lock().unwrap().clone()),
        ),
        ("allowedReadFiles", path_values(&files.allowed_read_files)),
    ])
}
type Trace = Arc<Mutex<Vec<JsValue>>>;
fn record(trace: &Trace, event: &str, fields: JsObject) {
    let mut value = obj([("event", event.into())]);
    for (key, field) in fields.entries() {
        value.insert(key.clone(), field.clone());
    }
    trace
        .lock()
        .unwrap()
        .push(normalized(&JsValue::Object(value)));
}
struct Model {
    trace: Trace,
    fallback: bool,
}
impl TurnModel for Model {
    fn for_turn(&self, identity: JsObject) -> Option<RuntimeContext> {
        record(
            &self.trace,
            "model",
            obj([("identity", JsValue::Object(identity))]),
        );
        (!self.fallback).then(|| handle("bound-model"))
    }
    fn unbound(&self) -> RuntimeContext {
        handle("model")
    }
}
struct Goal(Trace);
impl SessionService for Goal {
    fn for_session(&self, session: &JsString) -> Option<RuntimeContext> {
        record(
            &self.0,
            "goal",
            obj([("sessionId", JsValue::String(session.clone()))]),
        );
        Some(handle("goal"))
    }
}
struct Subagent {
    trace: Trace,
    checkpoint: HostToolCheckpoint,
}
impl OneShotSubagent for Subagent {
    fn create_fork(
        &self,
        identity: JsObject,
        read: SharedFileState,
        write: SharedFileState,
    ) -> RuntimeContext {
        record(
            &self.trace,
            "fork",
            obj([
                ("identity", JsValue::Object(identity)),
                (
                    "sameRead",
                    JsValue::Bool(Arc::ptr_eq(&read, &self.checkpoint.read_file_state)),
                ),
                (
                    "sameWrite",
                    JsValue::Bool(Arc::ptr_eq(&write, &self.checkpoint.write_snapshots)),
                ),
            ]),
        );
        handle("subagent")
    }
}
struct Plans {
    trace: Trace,
    empty: bool,
}
impl PlanFiles for Plans {
    fn directory(&self) -> Option<JsString> {
        record(&self.trace, "directory", obj([]));
        Some(if self.empty { "" } else { "/plans" }.into())
    }
    fn resolve(&self, path: JsString, cwd: JsString) -> HostFuture<JsValue> {
        record(
            &self.trace,
            "resolve",
            obj([
                ("path", JsValue::String(path.clone())),
                ("cwd", JsValue::String(cwd.clone())),
            ]),
        );
        Box::pin(async move {
            Ok(
                format!("{}/{}", cwd.to_utf8().unwrap(), path.to_utf8().unwrap())
                    .as_str()
                    .into(),
            )
        })
    }
    fn read(&self, path: JsString, cwd: JsString) -> HostFuture<JsValue> {
        record(
            &self.trace,
            "read",
            obj([
                ("path", JsValue::String(path)),
                ("cwd", JsValue::String(cwd)),
            ]),
        );
        Box::pin(async { Ok("plan-content".into()) })
    }
}
struct Environment {
    config: JsObject,
    input: JsObject,
    result: JsObject,
    count: AtomicUsize,
}
impl TurnEnvironment for Environment {
    fn build(
        &self,
        base: Option<&JsValue>,
        cwd: &JsString,
        session: &JsString,
        turn: &JsString,
    ) -> Result<JsObject, HostError> {
        // The environment implementation is an injected host service. Check
        // its complete call contract; fixture output came from the real helper.
        assert_eq!(base, self.config.get("env"));
        assert_eq!(
            Some(cwd),
            self.config.get("cwd").and_then(JsValue::as_string)
        );
        assert_eq!(
            Some(session),
            self.input.get("sessionId").and_then(JsValue::as_string)
        );
        assert_eq!(
            Some(turn),
            self.input.get("turnId").and_then(JsValue::as_string)
        );
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(self.result.clone())
    }
}

fn ports(spec: &JsValue, trace: &Trace, checkpoint: &HostToolCheckpoint) -> ToolContextPorts {
    if !yes(spec, "ports") {
        return ToolContextPorts::default();
    }
    let events = trace.clone();
    ToolContextPorts {
        model: Some(Arc::new(Model {
            trace: trace.clone(),
            fallback: yes(spec, "modelFallback"),
        })),
        elicitation: Some(handle("elicitation")),
        user_dialog: Some(handle("dialog")),
        audit_recorder: Some(handle("audit")),
        file_history: Some(handle("history")),
        file_update_notifier: Some(handle("notifier")),
        clock: Some(Arc::new(|| "fixture-now".into())),
        event_emitter: Some(Arc::new(move |value| {
            record(
                &events,
                "progress",
                obj([("value", JsValue::Object(value))]),
            )
        })),
        goal: Some(Arc::new(Goal(trace.clone()))),
        subagent: Some(Arc::new(Subagent {
            trace: trace.clone(),
            checkpoint: checkpoint.clone(),
        })),
        plan_files: Some(Arc::new(Plans {
            trace: trace.clone(),
            empty: yes(spec, "emptyPlan"),
        })),
    }
}
fn with_abort(mut fields: JsObject, abort: &Option<RuntimeContext>) -> JsValue {
    if let Some(abort) = abort {
        fields.insert("abortSignal".into(), handle_value(abort));
    }
    JsValue::Object(fields)
}
async fn project_runtime(runtime: &ToolRuntimeContext) -> JsValue {
    let mut out = runtime.fields.clone();
    for (name, value) in [
        ("abortSignal", &runtime.abort_signal),
        ("model", &runtime.model),
        ("elicitation", &runtime.elicitation),
        ("userDialog", &runtime.user_dialog),
        ("auditRecorder", &runtime.audit_recorder),
        ("fileHistory", &runtime.file_history),
        ("fileUpdateNotifier", &runtime.file_update_notifier),
        ("subagent", &runtime.subagent),
        ("planTodo", &runtime.plan_todo),
        ("goal", &runtime.goal),
    ] {
        if let Some(value) = value {
            out.insert(name.into(), handle_value(value));
        }
    }
    for (key, value) in project_files(&runtime.files).as_object().unwrap().entries() {
        out.insert(key.clone(), value.clone());
    }
    if let Some(now) = &runtime.now {
        out.insert("now".into(), now());
    }
    if let Some(progress) = &runtime.progress {
        out.insert("progress".into(), JsValue::Bool(true));
        // JS evaluates plan directory result fields before calling progress.
        if let Some(dir) = &runtime.plan_directory {
            out.insert(
                "planDirectory".into(),
                val([
                    ("path", JsValue::String(dir.path.clone())),
                    ("resolve", dir.resolve("x".into()).await.unwrap()),
                    ("read", dir.read("x".into()).await.unwrap()),
                ]),
            );
        }
        progress(obj([
            ("toolCallId", "c".into()),
            ("toolName", "read".into()),
            ("message", "working".into()),
            ("metadata", val([("percent", JsValue::Number(50.0))])),
            ("createdAt", "now".into()),
            ("sessionId", "evil".into()),
            ("extra", JsValue::Bool(true)),
        ]));
    } else if let Some(dir) = &runtime.plan_directory {
        out.insert(
            "planDirectory".into(),
            val([
                ("path", JsValue::String(dir.path.clone())),
                ("resolve", dir.resolve("x".into()).await.unwrap()),
                ("read", dir.read("x".into()).await.unwrap()),
            ]),
        );
    }
    JsValue::Object(out)
}

#[tokio::test]
async fn replay_real_ts_constructors() {
    let fixture = baseline::oracle(include_str!("fixtures/host-tool-context/oracle.json"));
    assert_eq!(
        field(&fixture, "sourceSha256").as_object().unwrap().len(),
        5
    );
    let cases = field(&fixture, "cases");
    assert_eq!(array(&cases).len(), 82);
    let mut counts = [0; 4];
    for spec in array(&cases) {
        let name = text(&field(spec, "name"));
        let expected = field(spec, "expected");
        match text(&field(spec, "kind")).as_str() {
            "seed" => {
                counts[0] += 1;
                let mut input = decode(&field(spec, "input"));
                if yes(spec, "maps") {
                    let mut source = input.as_object().unwrap().clone();
                    for key in ["readFileState", "writeSnapshots"] {
                        if let Some(value) = source.get(key) {
                            let mut map = JsObject::new();
                            for (key, value) in value.as_object().unwrap().entries().iter().rev() {
                                map.insert(key.clone(), value.clone());
                            }
                            map.set_native_tag("Map");
                            source.insert(key.into(), JsValue::Object(map));
                        }
                    }
                    input = JsValue::Object(source);
                }
                equal(
                    &outcome(parse_seed_state(&input).map(|seed| {
                        seed.map(|s| {
                            let keys = |map: &Option<JsObject>| {
                                JsValue::Array(
                                    map.as_ref()
                                        .map(|m| {
                                            m.entries()
                                                .iter()
                                                .map(|(k, _)| JsValue::String(k.clone()))
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                )
                            };
                            let mut result = obj([
                                ("projection", JsValue::Object(serialize_seed_state(&s))),
                                ("readKeys", keys(&s.read_file_state)),
                                ("writeKeys", keys(&s.write_snapshots)),
                            ]);
                            if name == "number-nonfinite" {
                                let read = &s.read_file_state.as_ref().unwrap().entries()[0].1;
                                let write = s.write_snapshots.as_ref().unwrap().get("f").unwrap();
                                result.insert(
                                    "nonfinite".into(),
                                    JsValue::Array(vec![
                                        JsValue::Bool(
                                            read.get("mtimeMs")
                                                .and_then(JsValue::as_number)
                                                .is_some_and(f64::is_nan),
                                        ),
                                        JsValue::Bool(
                                            read.get("offset").and_then(JsValue::as_number)
                                                == Some(f64::INFINITY),
                                        ),
                                        JsValue::Bool(
                                            write.get("mtimeMs").and_then(JsValue::as_number)
                                                == Some(f64::NEG_INFINITY),
                                        ),
                                    ]),
                                );
                            }
                            JsValue::Object(result)
                        })
                        .unwrap_or(JsValue::Undefined)
                    })),
                    &expected,
                    &name,
                );
            }
            "checkpoint" => {
                counts[1] += 1;
                let mut seed = parse_seed_state(&field(spec, "seed")).unwrap().unwrap();
                if yes(spec, "nativeSeedMaps") {
                    let mut map = JsObject::new();
                    for (key, value) in seed
                        .read_file_state
                        .as_ref()
                        .unwrap()
                        .entries()
                        .iter()
                        .rev()
                    {
                        map.insert(key.clone(), value.clone());
                    }
                    map.set_native_tag("Map");
                    seed.read_file_state = Some(map);
                }
                let checkpoint =
                    HostToolCheckpoint::new(Some(&seed), &strings(&field(spec, "allowed")))
                        .unwrap();
                let before = checkpoint.tool_context_state();
                let detached = checkpoint.snapshot().unwrap();
                seed.read_file_state = Some(JsObject::new());
                for op in array(&field(spec, "ops")) {
                    match text(&field(op, "op")).as_str() {
                        "allow" => checkpoint.allow_read_files(&strings(&field(op, "paths"))),
                        "read" => {
                            before.read_file_state.lock().unwrap().insert(
                                field(op, "path").as_string().unwrap().clone(),
                                field(op, "value"),
                            );
                        }
                        "write" => {
                            before.write_snapshots.lock().unwrap().insert(
                                field(op, "path").as_string().unwrap().clone(),
                                field(op, "value"),
                            );
                        }
                        _ => panic!("unknown op"),
                    }
                }
                let after = checkpoint.tool_context_state();
                let snapshot = checkpoint.snapshot().unwrap();
                let keys = |map: &JsObject| {
                    JsValue::Array(
                        map.entries()
                            .iter()
                            .map(|(k, _)| JsValue::String(k.clone()))
                            .collect(),
                    )
                };
                equal(
                    &val([
                        ("before", project_files(&before)),
                        ("after", project_files(&after)),
                        ("detached", JsValue::Object(serialize_seed_state(&detached))),
                        ("snapshot", JsValue::Object(serialize_seed_state(&snapshot))),
                        ("readKeys", keys(&before.read_file_state.lock().unwrap())),
                        (
                            "snapshotReadKeys",
                            keys(snapshot.read_file_state.as_ref().unwrap()),
                        ),
                        (
                            "sameRead",
                            JsValue::Bool(Arc::ptr_eq(
                                &before.read_file_state,
                                &after.read_file_state,
                            )),
                        ),
                        (
                            "sameWrite",
                            JsValue::Bool(Arc::ptr_eq(
                                &before.write_snapshots,
                                &after.write_snapshots,
                            )),
                        ),
                    ]),
                    &expected,
                    &name,
                );
            }
            "context" => {
                counts[2] += 1;
                let trace: Trace = Arc::default();
                let config = merge(
                    r#"{"cwd":"/fixture/project","permissionMode":"default","permissionContext":{"canPrompt":true,"rules":[{"allow":"read"}],"cwd":"spoof","mode":"spoof","custom":7},"env":{"BASE":"value","PILOTDECK_SESSION_ID":"spoof"}}"#,
                    &field(spec, "config"),
                );
                let input = merge(
                    r#"{"sessionId":"session","turnId":"turn"}"#,
                    &field(spec, "input"),
                );
                let seed = parse_seed_state(&parse_json(r#"{"allowedReadFiles":["seed"],"readFileState":{"f":{"mtimeMs":1,"kind":"text"}}}"#).unwrap()).unwrap();
                let checkpoint = HostToolCheckpoint::new(seed.as_ref(), &[]).unwrap();
                let environment = Arc::new(Environment {
                    config: config.clone(),
                    input: input.clone(),
                    result: field(spec, "environment").as_object().unwrap().clone(),
                    count: AtomicUsize::new(0),
                });
                let binding = TurnBinding {
                    config,
                    input,
                    checkpoint: checkpoint.clone(),
                    abort_signal: yes(spec, "abort").then(|| handle("abort")),
                    environment: environment.clone(),
                };
                let ports = ports(spec, &trace, &checkpoint);
                let remote = decode(&field(spec, "remote"));
                let call = JsValue::Object(merge(
                    r#"{"runId":"run","operationId":"operation","sessionId":"evil","turnId":"evil"}"#,
                    &field(spec, "call"),
                ));
                let view = field(spec, "view");
                let mut out = JsObject::new();
                let result = if view.as_string().is_some_and(|v| v.equals_str("permission")) {
                    match PermissionRequestContext::new(binding, ports)
                        .prepare(remote.clone())
                        .await
                    {
                        Ok(runtime) => {
                            out.insert(
                                "runtime".into(),
                                project_runtime(
                                    runtime.downcast_ref::<ToolRuntimeContext>().unwrap(),
                                )
                                .await,
                            );
                            Ok(JsValue::Object(out))
                        }
                        Err(error) => Err(error),
                    }
                } else if view.as_string().is_some_and(|v| v.equals_str("identity")) {
                    let identity = ContextRequestIdentity::new(binding, ports)
                        .context_identity(remote.as_object());
                    out.insert(
                        "identity".into(),
                        with_abort(identity.fields, &identity.abort_signal),
                    );
                    Ok(JsValue::Object(out))
                } else {
                    let builder = ToolContextBuilder::new(
                        binding,
                        ports,
                        spec.get("aux").and_then(JsValue::as_bool) != Some(false),
                    );
                    out.insert(
                        "permission".into(),
                        JsValue::Object(builder.permission_context()),
                    );
                    let identity = builder.context_identity(remote.as_object());
                    out.insert(
                        "identity".into(),
                        with_abort(identity.fields, &identity.abort_signal),
                    );
                    let execution = builder.execution_context(&call);
                    out.insert(
                        "execution".into(),
                        with_abort(execution.fields, &execution.abort_signal),
                    );
                    match builder.tool_runtime_context(
                        &remote,
                        yes(spec, "planTodo").then(|| handle("plan")),
                        yes(spec, "oneShot"),
                    ) {
                        Ok(runtime) => {
                            out.insert("runtime".into(), project_runtime(&runtime).await);
                            Ok(JsValue::Object(out))
                        }
                        Err(error) => Err(error),
                    }
                };
                let called =
                    result.is_ok() && !view.as_string().is_some_and(|v| v.equals_str("identity"));
                assert_eq!(
                    environment.count.load(Ordering::Relaxed),
                    usize::from(called),
                    "{name}"
                );
                equal(&outcome(result), &expected, &name);
                equal(
                    &JsValue::Array(trace.lock().unwrap().clone()),
                    &field(spec, "trace"),
                    &format!("{name} trace"),
                );
            }
            "steer" => {
                counts[3] += 1;
                let checkpoint = HostToolCheckpoint::new(None, &["seed".into()]).unwrap();
                let grants = SteerAuthorizations::new(checkpoint.clone());
                // Same product path as the turn module's steer drain.
                for message in array(&field(spec, "messages")) {
                    remember_steer_grants(message.as_object().unwrap(), Some(&grants)).unwrap();
                }
                let snapshot =
                    || JsValue::Object(serialize_seed_state(&checkpoint.snapshot().unwrap()));
                let mut snapshots = vec![snapshot()];
                for id in array(&field(spec, "apply")) {
                    grants.apply(id.as_string().unwrap());
                    snapshots.push(snapshot());
                }
                equal(&JsValue::Array(snapshots), &expected, &name);
            }
            _ => panic!("unknown fixture kind"),
        }
    }
    assert_eq!(counts, [38, 5, 36, 3]);
}

#[tokio::test]
async fn capability_and_permission_keep_host_handles_and_shared_maps() {
    let config = merge(
        r#"{"cwd":"/fixture","permissionMode":"default","permissionContext":{},"env":{}}"#,
        &JsValue::Undefined,
    );
    let input = merge(r#"{"sessionId":"s","turnId":"t"}"#, &JsValue::Undefined);
    let checkpoint = HostToolCheckpoint::default();
    let abort = handle("abort");
    let environment = Arc::new(Environment {
        config: config.clone(),
        input: input.clone(),
        result: JsObject::new(),
        count: AtomicUsize::new(0),
    });
    let binding = TurnBinding {
        config,
        input,
        checkpoint: checkpoint.clone(),
        abort_signal: Some(abort.clone()),
        environment: environment.clone(),
    };
    let trace = Arc::default();
    let ports = ports(&val([("ports", JsValue::Bool(true))]), &trace, &checkpoint);
    let capability = CapabilityContext {
        builder: ToolContextBuilder::new(binding.clone(), ports.clone(), true),
        plan_todo: None,
    };
    let permission = PermissionRequestContext::new(binding, ports);
    let invocation = capability
        .prepare(
            parse_json(
                r#"{"payload":{"context":{"abortSignal":"evil"}},"runId":"r","operationId":"o"}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let runtime = invocation
        .runtime
        .downcast_ref::<ToolRuntimeContext>()
        .unwrap();
    assert!(runtime.model.is_some() && runtime.subagent.is_some());
    assert!(Arc::ptr_eq(runtime.abort_signal.as_ref().unwrap(), &abort));
    let execution = invocation
        .execution
        .downcast_ref::<ExecutionContext>()
        .unwrap();
    assert!(Arc::ptr_eq(
        execution.abort_signal.as_ref().unwrap(),
        &abort
    ));
    runtime.files.read_file_state.lock().unwrap().insert(
        "new".into(),
        parse_json(r#"{"mtimeMs":1,"kind":"text"}"#).unwrap(),
    );
    checkpoint.allow_read_files(&["granted".into()]);
    let restricted = permission.prepare(JsValue::Undefined).await.unwrap();
    let restricted = restricted.downcast_ref::<ToolRuntimeContext>().unwrap();
    assert!(
        restricted.model.is_none()
            && restricted.subagent.is_none()
            && restricted.plan_todo.is_none()
    );
    assert!(Arc::ptr_eq(
        &restricted.files.read_file_state,
        &runtime.files.read_file_state
    ));
    assert!(
        restricted
            .files
            .read_file_state
            .lock()
            .unwrap()
            .contains_key("new")
    );
    assert!(runtime.files.allowed_read_files.is_empty());
    assert_eq!(
        restricted.files.allowed_read_files,
        vec![JsString::from("granted")]
    );
    assert_eq!(environment.count.load(Ordering::Relaxed), 2);
    assert_eq!(
        trace
            .lock()
            .unwrap()
            .iter()
            .filter(|v| field(v, "event")
                .as_string()
                .is_some_and(|v| v.equals_str("model")))
            .count(),
        1
    );
}
