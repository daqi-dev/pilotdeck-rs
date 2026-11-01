#![cfg(unix)]
//! Runtime wiring and complete disposed output, compared with the real TS byte log.
//! The cfg(test) runtime unit test gates I/O to prove dispose waits for writes.
#[path = "common/scratch.rs"]
mod scratch;
use pilotdeck_gateway::runtime::background::{
    BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, StartTaskSpec,
};
use pilotdeck_protocol::{JsString, JsValue};
use sha2::{Digest, Sha256};
use std::{fs, time::Duration};

#[tokio::test]
async fn configured_runtime_preserves_complete_ts_log_after_dispose() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let scratch = scratch::Scratch::new("553-runtime");
        let state = scratch.path().join("state");
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(state.clone()),
            snapshot_store: None,
            ..Default::default()
        });
        assert!(!state.exists(), "constructing an idle runtime must not write #994 background-output assertion");
        let task = runtime.start(StartTaskSpec {
            command: JsValue::from("/usr/bin/python3 -c 'import os; os.write(1,b\"a\"*999999); os.write(1,b\"b\"); os.write(1,b\"c\"); os.write(1,b\"D\"*800000); os.write(1,\"中🙂\".encode()*60000)'"),
            cwd: JsString::from(scratch.path().to_str().expect("#994 background-output setup")), env: None,
            session_id: JsValue::Undefined, agent_id: JsValue::Undefined, kind: JsValue::Undefined,
        }).await.expect("#994 background-output setup");
        let waited = runtime.wait(&task.task_id, &JsValue::Number(10000.0), None, None).await.expect("#994 background-output setup").expect("#994 background-output setup");
        assert!(!waited.timed_out, "#994 background-output assertion");
        assert_eq!(waited.task.status.as_str(),"completed", "#994 background-output assertion");
        runtime.dispose().await.expect("#994 background-output dispose");
        let oracle: serde_json::Value = serde_json::from_str(include_str!("fixtures/background-output/oracle.json")).expect("#994 background-output setup");
        let expected = oracle["cases"][0]["steps"].as_array().expect("#994 background-output setup").iter().rev().find(|step|step["op"]=="disk").expect("#994 background-output setup")["result"].clone();
        let path = state.join("output").join(format!("{}.log",String::from_utf16_lossy(task.task_id.units())));
        let bytes = fs::read(path).expect("#994 background-output expected runtime log");
        assert_eq!(bytes.len() as u64,expected["bytes"].as_u64().expect("#994 background-output setup"), "#994 background-output assertion");
        assert_eq!(format!("{:x}",Sha256::digest(&bytes)),expected["sha256"].as_str().expect("#994 background-output setup"), "#994 background-output assertion");
        let slice = runtime.get_output(&task.task_id,&JsValue::Number(0.0),&JsValue::Number(3000000.0),None).expect("#994 background-output setup");
        assert!(!slice.truncated, "#994 background-output assertion");
        assert_eq!(slice.next_offset,bytes.len() as f64, "#994 background-output assertion");
        assert_eq!(slice.total_bytes,bytes.len() as f64, "#994 background-output assertion");
        assert_eq!(slice.content.as_bytes(),bytes, "#994 background-output assertion");
        assert!(!state.join("state.json").exists(),"snapshot recovery is pr2 #994 background-output assertion");
    }).await.expect("bounded runtime probe");
}
