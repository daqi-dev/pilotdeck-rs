//! Real TS production-sidecar cross-check. The model port alone is deterministic.
use pilotdeck_gateway::host::{
    Cancellation, ExecuteRequest, ExecuteResult, HostClient, HostError, HostErrorKind, HostFuture,
    HostHandler, Limits, ReconnectObservation,
};
use pilotdeck_protocol::{
    JsObject, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::process::Command;

struct Model {
    calls: AtomicUsize,
    cancel: Option<Cancellation>,
    interrupt_pid: Mutex<Option<u32>>,
    observations: Mutex<Vec<ReconnectObservation>>,
}
impl HostHandler for Model {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let cancellation = self.cancel.clone();
        let interrupt_pid = self.interrupt_pid.lock().unwrap().take();
        println!("CALLBACK {}", stringify_json(&call).unwrap().unwrap());
        Box::pin(async move {
            let module = call.get("module").and_then(JsValue::as_string);
            let operation = call
                .get("payload")
                .and_then(|v| v.get("operation"))
                .and_then(JsValue::as_string);
            if !module.is_some_and(|m| m.equals_str("model"))
                || !operation.is_some_and(|m| m.equals_str("stream"))
            {
                return Err(HostError::new(
                    HostErrorKind::Callback,
                    "Unexpected production callback",
                ));
            }
            if let Some(cancel) = cancellation {
                // Real TS server:437-455 aborts its pending host-module wait.
                // Callback arrival proves execute was accepted; no sleep races.
                cancel.cancel(Some("host_probe_cancel".into()));
                std::future::pending::<()>().await;
            }
            if let Some(pid) = interrupt_pid {
                // Kill only this probe's owned child after acceptance and a
                // pending real module_call. Production defaults advertise
                // resumeSupport=none (server:768), so no replacement is legal.
                #[cfg(unix)]
                rustix::process::kill_process(
                    rustix::process::Pid::from_raw(pid as i32).expect("child pid"),
                    rustix::process::Signal::KILL,
                )
                .map_err(|error| HostError::new(HostErrorKind::Io, error.to_string()))?;
                #[cfg(not(unix))]
                return Err(HostError::new(
                    HostErrorKind::Io,
                    format!("resume probe requires Unix, pid={pid}"),
                ));
                std::future::pending::<()>().await;
            }
            Ok(parse_json(r#"{"events":[{"type":"message_start","role":"assistant"},{"type":"text_delta","text":"cross-ok"},{"type":"message_end","finishReason":"stop"}]}"#).unwrap().as_object().unwrap().clone())
        })
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        println!("EVENT {}", stringify_json(&event).unwrap().unwrap());
        Box::pin(async { Ok(()) })
    }
    fn observe_transport(&self, observation: ReconnectObservation) {
        println!("OBSERVATION {observation:?}");
        self.observations.lock().unwrap().push(observation);
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(args.len() == 4 || (args.len() == 5 && (args[4] == "cancel" || args[4] == "resume"))) {
        return Err(
            "usage: a6_host_probe NODE BASELINE_DIR ISOLATED_DIR NO_LISTEN_PRELOAD [cancel|resume]"
                .into(),
        );
    }
    let baseline = PathBuf::from(&args[1]);
    let isolated = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&isolated)?;
    let entry = baseline.join("src/cli/pilotdeck-agent-loop-sidecar.ts");
    let mut command = Command::new(&args[0]);
    command
        .arg("--import")
        .arg(&args[3])
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(&entry)
        .current_dir(&isolated)
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
        .env("HOME", &isolated)
        .env("PILOT_HOME", &isolated)
        .env("TMPDIR", &isolated);
    let cancellation = Cancellation::default();
    let cancel_mode = args.get(4).is_some_and(|mode| mode == "cancel");
    let resume_mode = args.get(4).is_some_and(|mode| mode == "resume");
    println!("SPAWN node={:?} entry={entry:?}", args[0]);
    let client = if resume_mode {
        HostClient::spawn_resumable(
            command,
            Some(&"pilotdeck-agent-loop".into()),
            Limits::default(),
        )
        .await?
    } else {
        HostClient::spawn(
            &mut command,
            Some(&"pilotdeck-agent-loop".into()),
            Limits::default(),
        )
        .await?
    };
    let model = Arc::new(Model {
        calls: AtomicUsize::new(0),
        cancel: cancel_mode.then(|| cancellation.clone()),
        interrupt_pid: Mutex::new(if resume_mode { client.child_id() } else { None }),
        observations: Mutex::new(Vec::new()),
    });
    println!(
        "HANDSHAKE pid={:?} binding={:?}",
        client.child_id(),
        client.binding()
    );
    let result = client.execute_cancellable(ExecuteRequest { identity: ExecutionIdentity { run_id: "cross-run".into(), operation_id: "cross-op".into(),
        request_id: "cross-request".into(), session_id: "cross-session".into(), turn_id: "cross-turn".into() },
        payload: parse_json(r#"{"provider":"host-fixture","model":"controlled","task":{"prompt":"hello"},"tools":[]}"#)?.as_object().unwrap().clone(),
        idempotency_key: None, operation_deadline: None }, model.clone(), cancellation).await;
    if resume_mode {
        let error = result.expect_err("production TS stdio does not support resuming this stream");
        if error.kind != HostErrorKind::Disconnected
            || !error.accepted
            || error.stream_id.is_none()
            || error.last_applied_sequence.is_none()
            || model.calls.load(Ordering::SeqCst) != 1
        {
            return Err(format!("unexpected TS resume result: {error}").into());
        }
        let observations = model.observations.lock().unwrap();
        if !observations.is_empty() {
            return Err(format!("unexpected TS resume observations: {observations:?}").into());
        }
        println!(
            "CROSS_RESUME_BOUNDARY_PASS real TS stdio; interrupted during module_call; callbacks=1; original stream progress retained; resumeSupport=none prevents replacement; positive resume covered by fixture: {error}"
        );
        return Ok(());
    }
    let (terminal, expected) = match result? {
        ExecuteResult::Terminal(terminal) => (
            terminal,
            if cancel_mode {
                "cancelled"
            } else {
                "completed"
            },
        ),
        other => return Err(format!("unexpected production result: {other:?}").into()),
    };
    if !terminal
        .get("outcome")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
        || model.calls.load(Ordering::SeqCst) == 0
    {
        return Err(format!(
            "unexpected terminal: {}",
            stringify_json(&terminal)?.unwrap()
        )
        .into());
    }
    println!(
        "CROSS_PASS real TS production sidecar; mode={}; callbacks={}; terminal={}",
        if cancel_mode { "cancel" } else { "complete" },
        model.calls.load(Ordering::SeqCst),
        stringify_json(&terminal)?.unwrap()
    );
    Ok(())
}
