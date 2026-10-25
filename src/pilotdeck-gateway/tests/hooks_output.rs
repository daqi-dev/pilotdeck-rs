//! #363: real 64 MiB subprocess output and bounded-cap drain regression.
#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::runtime::hooks::exec::{
    CommandHookExecutor, CommandHookOptions, HookOutcome, LocalShellPort,
};
use pilotdeck_protocol::{JsObject, stringify_json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn command_large_output_matches_ts_and_capped_pipes_drain() {
    baseline::oracle(include_str!("fixtures/hooks-output/oracle.json"));
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hooks-output/oracle.json")).unwrap();
    for cap in [None, Some(8192)] {
        let executor = CommandHookExecutor::new(Arc::new(match cap {
            Some(limit) => LocalShellPort::with_output_limit(limit),
            None => LocalShellPort::default(),
        }));
        for case in golden["large"].as_array().unwrap() {
            let stderr = case["stream"] == "stderr";
            let result = tokio::time::timeout(
                Duration::from_secs(60),
                executor.execute(CommandHookOptions {
                    command: format!(
                        "head -c {} /dev/zero{}",
                        case["bytes"],
                        if stderr { " >&2" } else { "" }
                    )
                    .into(),
                    hook_input: JsObject::new(),
                    cwd: "/tmp".into(),
                    env: None,
                    signal: None,
                    timeout_ms: Some(30_000.0),
                }),
            )
            .await
            .expect("large output must drain and reap before the watchdog");
            if cap.is_some() {
                assert_eq!(result.outcome, HookOutcome::NonBlockingError);
                assert!(result.stdout.units().is_empty());
                assert_eq!(
                    result.stderr.to_utf8().unwrap(),
                    golden["hookCatch"]["failures"][0]["message"]
                );
                assert_eq!(result.exit_code, None);
            } else {
                assert_eq!(result.stdout.units().len() as u64, case["stdoutLength"]);
                assert_eq!(result.stderr.units().len() as u64, case["stderrLength"]);
                let text = if stderr {
                    &result.stderr
                } else {
                    &result.stdout
                };
                let hash = format!("{:x}", Sha256::digest(text.to_utf8().unwrap().as_bytes()));
                assert_eq!(hash, case["sha256"]);
                assert_eq!(result.exit_code.unwrap(), case["exitCode"]);
                assert_eq!(result.outcome.as_str(), case["outcome"]);
                // The canonical result has no truncation marker; avoid
                // serializing the large body just to check its key set.
                let mut small = result;
                small.stdout = "".into();
                small.stderr = "".into();
                let value: serde_json::Value =
                    serde_json::from_str(&stringify_json(&small.to_value()).unwrap().unwrap())
                        .unwrap();
                let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
                assert_eq!(serde_json::json!(keys), case["keys"]);
                assert_eq!(value["output"], case["output"]);
            }
        }
    }
}

#[test]
#[ignore = "requires Node 22 and PDRS_BASELINE"]
fn hooks_output_fixtures_regenerate_identically_from_ts() {
    baseline::regenerates_identically("hooks-output");
}
