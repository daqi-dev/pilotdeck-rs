//! Honest discovery and observation over the existing Gateway protocol.
//! This is not the new app/v1 JSON-RPC application contract.
use super::{
    ControlError, JsValue, NETWORK_TIMEOUT, Options, Result, Socket, encode, input_value, object,
    redact, string_is,
};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::Message;

pub(super) fn schema(server: Option<JsValue>) -> JsValue {
    let registered:Vec<_>=pilotdeck_gateway::native_process::registered_methods().map(|name|object(&[
        ("name",name.into()),
        ("registration","native_process::registered_methods".into()),
        ("availability","Registration does not guarantee that a provider is configured; query live capabilities and handle the actual RPC result.".into()),
        ("paramsSchema",object(&[("additionalProperties",JsValue::Bool(true))])),
        ("resultSchema",object(&[])),
        ("contractRef","src/pilotdeck-gateway/src/native_process/methods/mod.rs#registered_methods".into()),
    ])).collect();
    let commands=JsValue::Array([
        ("status","One authenticated describe_server response."),
        ("capabilities","The selected Gateway's actual advertised capabilities."),
        ("call","One non-stream request; submit_turn requires a streaming command."),
        ("connect","Bidirectional pilotdeck-connect/1 JSON-RPC over NDJSON; one authenticated connection for streams and interactive replies."),
        ("stream","One text turn with checked seq/final; close may abort the session."),
        ("watch","Notifications plus selected read-only state snapshots; reconnect requires explicit resynchronization, not event replay."),
        ("schema","Compiled method/envelope discovery, optionally augmented by live capabilities; not exhaustive domain-field validation."),
        ("serve","Explicit native foreground host."),
        ("install","Build a native source installation with an approved source identity."),
    ].into_iter().map(|(name,description)|object(&[("name",name.into()),("description",description.into())])).collect());
    let mut result=match object(&[
        ("schemaVersion",JsValue::Number(1.0)),("version","pilotdeck-control/1".into()),
        ("transport","gateway-ws".into()),("protocolVersion",pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into()),
        ("scope","Command descriptions and real registered method names; domain schemas remain open and follow the referenced implementation contracts.".into()),
        ("domainSchemaDialect","https://json-schema.org/draft/2020-12/schema".into()),
        ("commands",commands),("registeredMethods",JsValue::Array(registered)),
        ("connectionMethods",JsValue::Array(["describe_server","submit_turn","abort_turn"].into_iter().map(|name|object(&[("name",name.into()),("contractRef","src/pilotdeck-gateway/src/runtime/turn/gateway.rs and runtime/connection/mod.rs".into())])).collect())),
        ("watch",object(&[("notificationCursor",JsValue::Null),("notificationReplay",JsValue::Bool(false)),("snapshotsAtomic",JsValue::Bool(false)),("snapshotMethods",JsValue::Array(["describe_server","active_turn_snapshot","run_get"].into_iter().map(JsValue::from).collect())),("runEventsCursor","run_events has a separate run-local persistent sequence; watch does not claim it as a global notification cursor.".into())])),
        ("appProtocol",object(&[("description","app/v1 is a separate JSON-RPC contract; this discovery describes the Gateway bridge only.".into())])),
    ]){JsValue::Object(ref value)=>value.clone(),_=>unreachable!()};
    result.insert("live".into(), server.unwrap_or(JsValue::Null));
    JsValue::Object(result)
}
pub(super) fn write_schema(server: Option<JsValue>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ControlError::new("transport_error", "Cannot start schema output runtime."))?;
    let output = super::stream::Output::new()?;
    runtime
        .block_on(output.deliver(&schema(server)))
        .map_err(|_| {
            ControlError::new(
                "output_error",
                "Could not deliver schema output within 10 seconds.",
            )
        })
}

#[derive(Clone, Debug)]
pub(super) struct Selection {
    session: Option<JsValue>,
    run: Option<JsValue>,
    project: Option<JsValue>,
    interval: Duration,
}
pub(super) fn input(options: &Options) -> Result<Selection> {
    let value = if options.input_file.is_some() {
        input_value(options)?
    } else {
        object(&[])
    };
    let fields = value.as_object().expect("input_value checked object");
    if value.get("cursor").is_some() {
        return Err(ControlError::new(
            "unsupported",
            "Gateway notifications have no durable cursor; attach without a cursor to resynchronize.",
        ));
    }
    if fields.entries().iter().any(|(k, _)| {
        !["sessionKey", "runId", "projectKey", "intervalMs"]
            .iter()
            .any(|name| k.equals_str(name))
    }) {
        return Err(ControlError::new(
            "input_error",
            "Unsupported watch input field.",
        ));
    }
    for name in ["sessionKey", "runId", "projectKey"] {
        if value
            .get(name)
            .is_some_and(|v| v.as_string().is_none_or(|s| s.is_empty()))
        {
            return Err(ControlError::new(
                "input_error",
                "Watch sessionKey, runId, and projectKey must be nonempty strings when provided.",
            ));
        }
    }
    if value.get("runId").is_some() && value.get("sessionKey").is_none() {
        return Err(ControlError::new(
            "input_error",
            "Watching a run requires sessionKey.",
        ));
    }
    let millis = match value.get("intervalMs") {
        None => 1000,
        Some(JsValue::Number(n))
            if n.is_finite() && n.fract() == 0.0 && (100.0..=60000.0).contains(n) =>
        {
            *n as u64
        }
        _ => {
            return Err(ControlError::new(
                "input_error",
                "Watch intervalMs must be an integer from 100 to 60000.",
            ));
        }
    };
    Ok(Selection {
        session: value.get("sessionKey").cloned(),
        run: value.get("runId").cloned(),
        project: value.get("projectKey").cloned(),
        interval: Duration::from_millis(millis),
    })
}

struct Stop {
    flag: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
}
impl Stop {
    fn new() -> Result<Self> {
        let mut stop = Self {
            flag: Arc::new(AtomicBool::new(false)),
            ids: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            stop.ids.push(
                signal_hook::flag::register(signal, stop.flag.clone()).map_err(|_| {
                    ControlError::new("transport_error", "Cannot install watch signal handlers.")
                })?,
            );
        }
        Ok(stop)
    }
    async fn wait(&self) {
        while !self.flag.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for Stop {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
struct Emitter {
    output: super::stream::Output,
    epoch: String,
    sequence: u64,
    resync: bool,
}
impl Emitter {
    fn new() -> Result<Self> {
        Ok(Self {
            output: super::stream::Output::new()?,
            epoch: uuid::Uuid::new_v4().to_string(),
            sequence: 0,
            resync: true,
        })
    }
    async fn emit(&mut self, kind: &str, fields: &[(&str, JsValue)]) -> Result<()> {
        if self.sequence > 9_007_199_254_740_991 {
            return Err(ControlError::new(
                "protocol_error",
                "Watch local sequence exhausted; resynchronize in a new process.",
            ));
        }
        let mut frame = match object(&[
            ("version", "pilotdeck-control/1".into()),
            ("type", kind.into()),
            ("epoch", self.epoch.as_str().into()),
            ("seq", JsValue::Number(self.sequence as f64)),
            ("cursor", JsValue::Null),
        ]) {
            JsValue::Object(ref frame) => frame.clone(),
            _ => unreachable!(),
        };
        for (key, value) in fields {
            frame.insert((*key).into(), value.clone());
        }
        let delivered = self.output.enqueue(&JsValue::Object(frame))?;
        // Reserving the identity and enqueueing are one synchronous step.
        // A timer/signal may cancel the acknowledgement wait, while the
        // output thread still writes the accepted frame.
        self.sequence += 1;
        super::stream::Output::confirm(delivered).await.map_err(|_|ControlError::new("output_error","Could not deliver watch output within 10 seconds; resynchronize before continuing."))?;
        Ok(())
    }
    async fn notification(&mut self, value: &JsValue, token: &str) -> Result<()> {
        let name = value
            .get("name")
            .and_then(JsValue::as_string)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ControlError::new(
                    "protocol_error",
                    "Gateway notification must have a nonempty name string.",
                )
            })?;
        let mut payload = vec![("name", JsValue::String(name.clone()))];
        if let Some(value) = value.get("payload") {
            payload.push(("payload", redact(value, token)));
        }
        self.emit(
            "notification",
            &[
                ("source", "gateway.notification".into()),
                ("duringResync", JsValue::Bool(self.resync)),
                ("notification", object(&payload)),
            ],
        )
        .await
    }
}
fn disconnected() -> ControlError {
    ControlError::new(
        "transport_error",
        "Gateway watch connection closed; notifications may have been missed and require resynchronization.",
    )
}
fn parse(frame: Message) -> Result<Option<JsValue>> {
    if frame.is_close() {
        return Err(disconnected());
    }
    if !frame.is_text() {
        return Ok(None);
    }
    pilotdeck_protocol::parse_json(
        frame
            .to_text()
            .map_err(|_| ControlError::new("protocol_error", "Invalid watch frame."))?,
    )
    .map(Some)
    .map_err(|_| ControlError::new("protocol_error", "Invalid watch JSON."))
}
async fn receive(socket: &mut Socket, emit: &mut Emitter, token: &str) -> Result<Option<JsValue>> {
    let frame = socket
        .next()
        .await
        .ok_or_else(disconnected)?
        .map_err(|_| disconnected())?;
    let value = parse(frame)?;
    if let Some(value) = &value {
        if string_is(value, "type", "notification") {
            emit.notification(value, token).await?;
            return Ok(None);
        }
    } else {
        timeout(NETWORK_TIMEOUT, socket.flush())
            .await
            .map_err(|_| disconnected())?
            .map_err(|_| disconnected())?;
    }
    Ok(value)
}
async fn request(
    socket: &mut Socket,
    method: &str,
    params: JsValue,
    emit: &mut Emitter,
    token: &str,
) -> Result<JsValue> {
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    let id = uuid::Uuid::new_v4().to_string();
    timeout_at(deadline, async {
        socket
            .send(Message::Text(
                encode(&object(&[
                    ("type", "request".into()),
                    ("id", id.as_str().into()),
                    ("method", method.into()),
                    ("params", params),
                ]))?
                .into(),
            ))
            .await
            .map_err(|_| disconnected())?;
        loop {
            let Some(value) = receive(socket, emit, token).await? else {
                continue;
            };
            if !string_is(&value, "type", "response") || !string_is(&value, "id", &id) {
                return Err(ControlError::new(
                    "protocol_error",
                    "Unexpected watch response; resynchronization is required.",
                ));
            }
            return match value.get("ok").and_then(JsValue::as_bool) {
                Some(true) => value.get("result").cloned().ok_or_else(|| {
                    ControlError::new("protocol_error", "Watch snapshot result is missing.")
                }),
                Some(false) => Err(ControlError {
                    kind: "gateway_error",
                    message: "Gateway rejected a watch snapshot.",
                    gateway: value.get("error").map(|v| redact(v, token)),
                }),
                None => Err(ControlError::new(
                    "protocol_error",
                    "Invalid watch snapshot response status.",
                )),
            };
        }
    })
    .await
    .map_err(|_| {
        ControlError::new(
            "transport_error",
            "Gateway watch snapshot timed out; resynchronization is required.",
        )
    })?
}
fn capabilities(value: &JsValue, name: &str) -> bool {
    matches!(value.get("capabilities"),Some(JsValue::Array(values))if values.iter().any(|v|v.as_string().is_some_and(|s|s.equals_str(name))))
}
fn selector(selection: &Selection, include_events: bool) -> JsValue {
    let mut fields = Vec::new();
    if let Some(value) = &selection.session {
        fields.push(("sessionKey", value.clone()));
    }
    if let Some(value) = &selection.run {
        fields.push(("runId", value.clone()));
    }
    if let Some(value) = &selection.project {
        fields.push(("projectKey", value.clone()));
    }
    if include_events {
        fields.push(("includeEvents", JsValue::Bool(false)));
    }
    object(&fields)
}
async fn snapshots(
    socket: &mut Socket,
    selection: &Selection,
    emit: &mut Emitter,
    token: &str,
    previous: &mut BTreeMap<&'static str, JsValue>,
) -> Result<()> {
    let description = request(socket, "describe_server", object(&[]), emit, token).await?;
    if selection.run.is_some() && !capabilities(&description, "run_get") {
        return Err(ControlError::new(
            "unsupported",
            "This Gateway does not advertise run_get; the requested run cannot be watched.",
        ));
    }
    snapshot("server", description, emit, token, previous).await?;
    if selection.session.is_some() {
        let value = request(
            socket,
            "active_turn_snapshot",
            selector(selection, true),
            emit,
            token,
        )
        .await?;
        snapshot("activeTurn", value, emit, token, previous).await?;
    }
    if selection.run.is_some() {
        let value = request(socket, "run_get", selector(selection, false), emit, token).await?;
        snapshot("run", value, emit, token, previous).await?;
    }
    Ok(())
}
async fn snapshot(
    scope: &'static str,
    value: JsValue,
    emit: &mut Emitter,
    token: &str,
    previous: &mut BTreeMap<&'static str, JsValue>,
) -> Result<()> {
    if emit.resync || previous.get(scope) != Some(&value) {
        emit.emit(
            "snapshot",
            &[
                ("source", "gateway.rpc".into()),
                ("scope", scope.into()),
                ("duringResync", JsValue::Bool(emit.resync)),
                ("atomic", JsValue::Bool(false)),
                ("state", redact(&value, token)),
            ],
        )
        .await?;
        previous.insert(scope, value);
    }
    Ok(())
}
async fn connected(
    options: &Options,
    token: &str,
    selection: &Selection,
    emit: &mut Emitter,
    initial: bool,
) -> Result<()> {
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    let mut socket = timeout_at(
        deadline,
        super::open_authenticated_policy(options, token, deadline, true),
    )
    .await
    .map_err(|_| disconnected())??;
    emit.epoch = uuid::Uuid::new_v4().to_string();
    emit.sequence = 0;
    emit.resync = true;
    emit.emit(
        "resync_required",
        &[
            (
                "reason",
                if initial {
                    "initial_attach".into()
                } else {
                    "reconnected".into()
                },
            ),
            ("notificationReplay", JsValue::Bool(false)),
            ("snapshotsAtomic", JsValue::Bool(false)),
        ],
    )
    .await?;
    let mut previous = BTreeMap::new();
    snapshots(&mut socket, selection, emit, token, &mut previous).await?;
    emit.emit("resync_complete",&[("notificationReplay",JsValue::Bool(false)),("snapshotsAtomic",JsValue::Bool(false)),("meaning","Selected independent RPC snapshots are refreshed; missed notifications are not replayed.".into())]).await?;
    emit.resync = false;
    loop {
        let next = Instant::now() + selection.interval;
        loop {
            tokio::select! {
                _=tokio::time::sleep_until(next)=>break,
                value=receive(&mut socket,emit,token)=>{if value?.is_some(){return Err(ControlError::new("protocol_error","Unsolicited non-notification watch frame."));}},
            }
        }
        snapshots(&mut socket, selection, emit, token, &mut previous).await?;
    }
}
async fn observe(
    options: &Options,
    token: &str,
    selection: &Selection,
    emit: &mut Emitter,
) -> Result<()> {
    let mut initial = true;
    let mut retries = 0_u32;
    loop {
        let result = connected(options, token, selection, emit, initial).await;
        match result {
            Err(error) if error.kind == "transport_error" => {
                if initial && emit.resync && emit.sequence == 0 {
                    return Err(error);
                }
                initial = false;
                retries = retries.saturating_add(1);
                emit.emit(
                    "disconnected",
                    &[
                        ("resyncRequired", JsValue::Bool(true)),
                        ("notificationReplay", JsValue::Bool(false)),
                    ],
                )
                .await?;
                tokio::time::sleep(Duration::from_millis(
                    (250_u64.saturating_mul(1_u64 << retries.min(5))).min(5000),
                ))
                .await;
            }
            other => return other,
        }
    }
}
pub(super) async fn run(options: &Options, token: &str, selection: Selection) -> Result<()> {
    let stop = Stop::new()?;
    let mut emit = Emitter::new()?;
    {
        let observer = observe(options, token, &selection, &mut emit);
        tokio::pin!(observer);
        tokio::select! {biased;_ = stop.wait()=>{},result=&mut observer=>return result,}
    }
    // Dropping the observer closes its read-only socket, never submits or
    // replays a write. The output worker is detached and cannot delay exit.
    timeout(Duration::from_millis(500),emit.emit("stopped",&[("reason","signal".into())])).await.map_err(|_|ControlError::new("output_error","Watch stopped, but the final output could not be delivered; resynchronize before continuing."))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_watch_delivery_never_reuses_an_enqueued_sequence() {
        use std::{future::Future, task::Poll};
        let (output, receive) = super::super::stream::Output::controlled();
        let mut emitter = Emitter {
            output,
            epoch: "stable-epoch".into(),
            sequence: 0,
            resync: false,
        };
        let mut pending = Box::pin(emitter.emit("notification", &[]));
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(pending); // the watch timer wins after enqueue, before stdout acknowledgement
        let (first, first_ack) = receive.try_recv().unwrap();
        assert!(first_ack.send(Ok(())).is_err());
        assert_eq!(emitter.sequence, 1);
        let mut next = Box::pin(emitter.emit("stopped", &[]));
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let (second, second_ack) = receive.try_recv().unwrap();
        second_ack.send(Ok(())).unwrap();
        next.await.unwrap();
        let first = pilotdeck_protocol::parse_json(&first).unwrap();
        let second = pilotdeck_protocol::parse_json(&second).unwrap();
        assert_eq!(first.get("epoch"), second.get("epoch"));
        assert_eq!(first.get("seq"), Some(&JsValue::Number(0.0)));
        assert_eq!(second.get("seq"), Some(&JsValue::Number(1.0)));
    }
    #[test]
    fn compiled_schema_uses_actual_registration_and_open_domain_contracts() {
        let value = schema(None);
        assert_eq!(value.get("transport"), Some(&JsValue::from("gateway-ws")));
        assert_eq!(value.get("live"), Some(&JsValue::Null));
        let Some(JsValue::Array(methods)) = value.get("registeredMethods") else {
            panic!("methods");
        };
        let actual: std::collections::BTreeSet<_> = methods
            .iter()
            .map(|m| {
                m.get("name")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            })
            .collect();
        let expected: std::collections::BTreeSet<_> =
            pilotdeck_gateway::native_process::registered_methods()
                .map(str::to_owned)
                .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), methods.len());
        for method in methods {
            assert_eq!(
                method
                    .get("paramsSchema")
                    .and_then(|v| v.get("additionalProperties")),
                Some(&JsValue::Bool(true))
            );
        }
        let description = object(&[("capabilities", JsValue::Array(vec!["run_get".into()]))]);
        assert_eq!(
            schema(Some(description.clone())).get("live"),
            Some(&description)
        );
    }
    #[test]
    fn watch_selection_rejects_fake_resume_cursors_and_invalid_scopes() {
        let root =
            std::env::temp_dir().join(format!("control-watch-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("input.json");
        let options = Options {
            url: "ws://unused".into(),
            token_file: "unused".into(),
            command: "watch".into(),
            input_file: Some(path.to_str().unwrap().into()),
        };
        for raw in [
            r#"{"cursor":"old"}"#,
            r#"{"runId":"r"}"#,
            r#"{"sessionKey":""}"#,
            r#"{"sessionKey":4}"#,
            r#"{"intervalMs":99}"#,
            r#"{"intervalMs":60001}"#,
            r#"{"intervalMs":1.5}"#,
            r#"{"unknown":1}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            assert!(input(&options).is_err(), "{raw}");
        }
        std::fs::write(
            &path,
            r#"{"sessionKey":"s","runId":"r","projectKey":"p","intervalMs":100}"#,
        )
        .unwrap();
        let selected = input(&options).unwrap();
        assert_eq!(selected.interval, Duration::from_millis(100));
        assert!(selected.session.is_some() && selected.run.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
