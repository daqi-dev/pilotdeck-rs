//! Real hook executor and existing result projection, with test-only wire stimuli.
// Both private callers include these test-only fixtures in their own scope;
// keep the product modules private instead of exporting a test bridge.
#![allow(clippy::duplicate_mod)]
use super::*;
#[path = "baseline.rs"]
mod baseline;
#[path = "network_timing_private.rs"]
mod fixture;
#[path = "network_timing_registry.rs"]
mod registry;
use fixture::{RAW, Transport, URL};
use serde_json::{Value, json};

async fn child(transport: Transport, signal: Cancellation) -> Value {
    let executor = HttpHookExecutor::new();
    let options = HttpHookOptions {
        url: URL.into(),
        headers: None,
        allowed_env_vars: None,
        hook_input: JsObject::new(),
        env: None,
        signal: Some(signal),
    };
    let result = executor
        .execute_with(options, |request| transport.send(request, |error| error))
        .await;
    let result = result.unwrap_or_else(|error| ExecutionResult::failure(error, false));
    let hook: Value =
        serde_json::from_str(&stringify_json(&result.to_value()).unwrap().unwrap()).unwrap();
    json!({"name":null,"message":if hook["stderr"] == "" { Value::Null } else {hook["stderr"].clone()},"delivery":"return","content":hook["stdout"],"hook":hook})
}
async fn replay(row: &Value) -> Value {
    fixture::replay(row, child).await
}
async fn check(id: &str) {
    baseline::oracle(RAW);
    let oracle: Value = serde_json::from_str(RAW).unwrap();
    let row = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    let actual = replay(row).await;
    println!("{}", json!({"id":id,"ts":row["observable"],"rust":actual}));
    assert_eq!(actual, row["observable"], "network timing cell: {id}");
}
#[tokio::test(start_paused = true)]
async fn known_differences_remain_exact_and_ignored() {
    baseline::oracle(RAW);
    let oracle: Value = serde_json::from_str(RAW).unwrap();
    for entry in registry::validate(&oracle) {
        let row = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == entry["id"])
            .unwrap();
        if row["path"] != "hooks" {
            continue;
        }
        let io = row["phase"] == "io-headers";
        if io {
            tokio::time::resume();
        }
        let actual = replay(row).await;
        if io {
            tokio::time::pause();
        }
        assert_eq!(
            actual, entry["rust"],
            "known difference changed: {}",
            entry["id"]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_fulfilled_continuation_first() {
    check("hooks/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_fulfilled_parent_first() {
    check("hooks/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_headers_fulfilled_tie_parent_first() {
    check("hooks/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_fulfilled_tie_parent_last() {
    check("hooks/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_rejected_continuation_first() {
    check("hooks/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_rejected_parent_first() {
    check("hooks/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_headers_rejected_tie_parent_first() {
    check("hooks/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_rejected_tie_parent_last() {
    check("hooks/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_idle_continuation_first() {
    check("hooks/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_idle_parent_first() {
    check("hooks/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_headers_idle_tie_parent_first() {
    check("hooks/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_headers_idle_tie_parent_last() {
    check("hooks/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_fulfilled_continuation_first() {
    check("hooks/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_fulfilled_parent_first() {
    check("hooks/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_fulfilled_tie_parent_first() {
    check("hooks/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_fulfilled_tie_parent_last() {
    check("hooks/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_rejected_continuation_first() {
    check("hooks/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_rejected_parent_first() {
    check("hooks/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_rejected_tie_parent_first() {
    check("hooks/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_rejected_tie_parent_last() {
    check("hooks/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_idle_continuation_first() {
    check("hooks/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_idle_parent_first() {
    check("hooks/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_body_idle_tie_parent_first() {
    check("hooks/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_body_idle_tie_parent_last() {
    check("hooks/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_fulfilled_continuation_first() {
    check("hooks/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_fulfilled_parent_first() {
    check("hooks/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_redirect_hop_fulfilled_tie_parent_first() {
    check("hooks/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_fulfilled_tie_parent_last() {
    check("hooks/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_rejected_continuation_first() {
    check("hooks/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_rejected_parent_first() {
    check("hooks/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_redirect_hop_rejected_tie_parent_first() {
    check("hooks/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_rejected_tie_parent_last() {
    check("hooks/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_idle_continuation_first() {
    check("hooks/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_idle_parent_first() {
    check("hooks/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn hooks_redirect_hop_idle_tie_parent_first() {
    check("hooks/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn hooks_redirect_hop_idle_tie_parent_last() {
    check("hooks/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test]
async fn hooks_io_headers_fulfilled_continuation_first() {
    check("hooks/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
async fn hooks_io_headers_fulfilled_parent_first() {
    check("hooks/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_fulfilled_tie_parent_first() {
    check("hooks/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_fulfilled_tie_parent_last() {
    check("hooks/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
async fn hooks_io_headers_rejected_continuation_first() {
    check("hooks/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
async fn hooks_io_headers_rejected_parent_first() {
    check("hooks/io-headers/rejected/parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_rejected_tie_parent_first() {
    check("hooks/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_rejected_tie_parent_last() {
    check("hooks/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
async fn hooks_io_headers_idle_continuation_first() {
    check("hooks/io-headers/idle/continuation-first").await;
}

#[tokio::test]
async fn hooks_io_headers_idle_parent_first() {
    check("hooks/io-headers/idle/parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_idle_tie_parent_first() {
    check("hooks/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
async fn hooks_io_headers_idle_tie_parent_last() {
    check("hooks/io-headers/idle/tie-parent-last").await;
}

#[test]
fn timing_metadata_and_request_trace_negative_controls() {
    fixture::negative_contracts();
}
