//! Real TS production-sidecar cross-check. The model port alone is deterministic.
use pilotdeck_gateway::host::{
    ExecuteRequest, ExecuteResult, HostClient, HostError, HostErrorKind, HostFuture, HostHandler,
    Limits,
};
use pilotdeck_protocol::{
    JsObject, JsValue, admission::ExecutionIdentity, parse_json, stringify_json,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::process::Command;

struct Model(AtomicUsize);
impl HostHandler for Model {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.0.fetch_add(1, Ordering::SeqCst);
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
            Ok(parse_json(r#"{"events":[{"type":"message_start","role":"assistant"},{"type":"text_delta","text":"cross-ok"},{"type":"message_end","finishReason":"stop"}]}"#).unwrap().as_object().unwrap().clone())
        })
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        println!("EVENT {}", stringify_json(&event).unwrap().unwrap());
        Box::pin(async { Ok(()) })
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: a6_host_probe NODE BASELINE_DIR ISOLATED_DIR NO_LISTEN_PRELOAD".into());
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
    let model = Arc::new(Model(AtomicUsize::new(0)));
    println!("SPAWN node={:?} entry={entry:?}", args[0]);
    let client = HostClient::spawn(
        &mut command,
        Some(&"pilotdeck-agent-loop".into()),
        Limits::default(),
    )
    .await?;
    println!(
        "HANDSHAKE pid={:?} binding={:?}",
        client.child_id(),
        client.binding()
    );
    let result = client.execute(ExecuteRequest { identity: ExecutionIdentity { run_id: "cross-run".into(), operation_id: "cross-op".into(),
        request_id: "cross-request".into(), session_id: "cross-session".into(), turn_id: "cross-turn".into() },
        payload: parse_json(r#"{"provider":"host-fixture","model":"controlled","task":{"prompt":"hello"},"tools":[]}"#)?.as_object().unwrap().clone(),
        idempotency_key: None, operation_deadline: None }, model.clone()).await?;
    let ExecuteResult::Terminal(terminal) = result else {
        return Err("production execute was rejected".into());
    };
    if !terminal
        .get("outcome")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str("completed"))
        || model.0.load(Ordering::SeqCst) == 0
    {
        return Err(format!(
            "unexpected terminal: {}",
            stringify_json(&terminal)?.unwrap()
        )
        .into());
    }
    println!(
        "CROSS_PASS real TS production sidecar; callbacks={}; terminal={}",
        model.0.load(Ordering::SeqCst),
        stringify_json(&terminal)?.unwrap()
    );
    Ok(())
}
