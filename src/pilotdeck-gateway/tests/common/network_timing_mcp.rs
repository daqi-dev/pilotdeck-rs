//! Real MCP adapter and streamable text consumer, with test-only wire stimuli.
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
use fixture::{RAW, Scripted, Transport, URL};
use serde_json::{Value, json};
use std::{future::Future, task::Poll};

fn raw_error(message: pilotdeck_protocol::JsString) -> McpClientError {
    McpClientError::plain("TypeError", message.to_utf8().unwrap())
}
fn observed_error(error: McpClientError) -> Value {
    fixture::error(error.name(), error.message())
}
fn send_timer(
    transport: &Transport,
    method: Method,
) -> ExchangeFuture<Result<hyper::Response<crate::runtime::model::http::ModelBody>, McpClientError>>
{
    // Traverse actual fetch_with/follow synchronously before exposing the sole
    // final response timer. No asynchronous I/O is labelled a continuation.
    let timer = std::cell::Cell::new(false);
    let idle = std::cell::Cell::new(false);
    let url = URL.parse().unwrap();
    let headers = HeaderList::default();
    let mut traversal = Box::pin(fetch_with(method, &url, &headers, None, |request| {
        let source = transport.scripted(request);
        let timer = &timer;
        let idle = &idle;
        async move {
            match source {
                Scripted::Ready(result) => result.map_err(raw_error),
                Scripted::Timer(result) => {
                    assert!(!timer.replace(true), "single MCP final transport timer");
                    result.map_err(raw_error)
                }
                Scripted::Pending => {
                    idle.set(true);
                    std::future::pending().await
                }
            }
        }
    }));
    match traversal
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        Poll::Ready(result) if timer.get() => {
            ExchangeFuture::after(Duration::from_millis(20), result)
        }
        Poll::Ready(result) => ExchangeFuture::ready(result),
        Poll::Pending => {
            assert!(
                idle.get(),
                "only declared never-completing transport may remain pending"
            );
            ExchangeFuture::io(std::future::pending())
        }
    }
}
async fn child(path: &str, transport: Transport, signal: ToolCancellation) -> Value {
    let method = if path == "mcp_get" {
        Method::GET
    } else {
        assert_eq!(path, "mcp_post");
        Method::POST
    };
    let response = network_fetch_continuing_with(
        method.clone(),
        Fetch::Network {
            timeout_ms: if path == "mcp_get" { 10000.0 } else { 60000.0 },
        },
        &signal,
        false,
        || {
            if transport.is_io() {
                let transport = transport.clone();
                let method = method.clone();
                ExchangeFuture::io(async move {
                    fetch_with(
                        method,
                        &URL.parse().unwrap(),
                        &HeaderList::default(),
                        None,
                        |request| transport.send(request, raw_error),
                    )
                    .await
                })
            } else {
                send_timer(&transport, method.clone())
            }
        },
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => return observed_error(error),
    };
    let (body, before_abort) = response.into_body();
    match streamable_text(body, &signal, before_abort).await {
        Ok(text) => fixture::ok(text),
        Err(error) => observed_error(error),
    }
}
async fn replay(row: &Value) -> Value {
    fixture::replay(row, |transport, signal| {
        child(row["path"].as_str().unwrap(), transport, signal)
    })
    .await
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
        if !matches!(row["path"].as_str(), Some("mcp_get" | "mcp_post")) {
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
async fn mcp_get_headers_fulfilled_continuation_first() {
    check("mcp_get/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_fulfilled_parent_first() {
    check("mcp_get/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_headers_fulfilled_tie_parent_first() {
    check("mcp_get/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_fulfilled_tie_parent_last() {
    check("mcp_get/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_rejected_continuation_first() {
    check("mcp_get/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_rejected_parent_first() {
    check("mcp_get/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_headers_rejected_tie_parent_first() {
    check("mcp_get/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_rejected_tie_parent_last() {
    check("mcp_get/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_idle_continuation_first() {
    check("mcp_get/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_idle_parent_first() {
    check("mcp_get/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_headers_idle_tie_parent_first() {
    check("mcp_get/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_headers_idle_tie_parent_last() {
    check("mcp_get/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_body_fulfilled_continuation_first() {
    check("mcp_get/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn mcp_get_body_fulfilled_parent_first() {
    check("mcp_get/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn mcp_get_body_fulfilled_tie_parent_first() {
    check("mcp_get/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_body_fulfilled_tie_parent_last() {
    check("mcp_get/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_rejected_continuation_first() {
    check("mcp_get/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_rejected_parent_first() {
    check("mcp_get/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_rejected_tie_parent_first() {
    check("mcp_get/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_rejected_tie_parent_last() {
    check("mcp_get/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_idle_continuation_first() {
    check("mcp_get/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_idle_parent_first() {
    check("mcp_get/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_idle_tie_parent_first() {
    check("mcp_get/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_get_body_idle_tie_parent_last() {
    check("mcp_get/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_fulfilled_continuation_first() {
    check("mcp_get/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_fulfilled_parent_first() {
    check("mcp_get/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_redirect_hop_fulfilled_tie_parent_first() {
    check("mcp_get/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_fulfilled_tie_parent_last() {
    check("mcp_get/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_rejected_continuation_first() {
    check("mcp_get/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_rejected_parent_first() {
    check("mcp_get/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_redirect_hop_rejected_tie_parent_first() {
    check("mcp_get/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_rejected_tie_parent_last() {
    check("mcp_get/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_idle_continuation_first() {
    check("mcp_get/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_idle_parent_first() {
    check("mcp_get/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_get_redirect_hop_idle_tie_parent_first() {
    check("mcp_get/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_redirect_hop_idle_tie_parent_last() {
    check("mcp_get/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_status_fulfilled_continuation_first() {
    check("mcp_get/retry-status/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_status_fulfilled_parent_first() {
    check("mcp_get/retry-status/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn mcp_get_retry_status_fulfilled_tie_parent_first() {
    check("mcp_get/retry-status/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_status_fulfilled_tie_parent_last() {
    check("mcp_get/retry-status/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_error_fulfilled_continuation_first() {
    check("mcp_get/retry-error/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_error_fulfilled_parent_first() {
    check("mcp_get/retry-error/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍推进下一次重试"]
async fn mcp_get_retry_error_fulfilled_tie_parent_first() {
    check("mcp_get/retry-error/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_get_retry_error_fulfilled_tie_parent_last() {
    check("mcp_get/retry-error/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_fulfilled_continuation_first() {
    check("mcp_post/headers/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_fulfilled_parent_first() {
    check("mcp_post/headers/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_headers_fulfilled_tie_parent_first() {
    check("mcp_post/headers/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_fulfilled_tie_parent_last() {
    check("mcp_post/headers/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_rejected_continuation_first() {
    check("mcp_post/headers/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_rejected_parent_first() {
    check("mcp_post/headers/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_headers_rejected_tie_parent_first() {
    check("mcp_post/headers/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_rejected_tie_parent_last() {
    check("mcp_post/headers/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_idle_continuation_first() {
    check("mcp_post/headers/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_idle_parent_first() {
    check("mcp_post/headers/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_headers_idle_tie_parent_first() {
    check("mcp_post/headers/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_headers_idle_tie_parent_last() {
    check("mcp_post/headers/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_body_fulfilled_continuation_first() {
    check("mcp_post/body/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn mcp_post_body_fulfilled_parent_first() {
    check("mcp_post/body/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：headers 后 Rust 仍响应父取消，TS 已解除监听"]
async fn mcp_post_body_fulfilled_tie_parent_first() {
    check("mcp_post/body/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_body_fulfilled_tie_parent_last() {
    check("mcp_post/body/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_rejected_continuation_first() {
    check("mcp_post/body/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_rejected_parent_first() {
    check("mcp_post/body/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_rejected_tie_parent_first() {
    check("mcp_post/body/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_rejected_tie_parent_last() {
    check("mcp_post/body/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_idle_continuation_first() {
    check("mcp_post/body/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_idle_parent_first() {
    check("mcp_post/body/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_idle_tie_parent_first() {
    check("mcp_post/body/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：MCP body 失败映射为 NetworkFetchError，TS 为 TypeError"]
async fn mcp_post_body_idle_tie_parent_last() {
    check("mcp_post/body/idle/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_fulfilled_continuation_first() {
    check("mcp_post/redirect-hop/fulfilled/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_fulfilled_parent_first() {
    check("mcp_post/redirect-hop/fulfilled/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_redirect_hop_fulfilled_tie_parent_first() {
    check("mcp_post/redirect-hop/fulfilled/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_fulfilled_tie_parent_last() {
    check("mcp_post/redirect-hop/fulfilled/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_rejected_continuation_first() {
    check("mcp_post/redirect-hop/rejected/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_rejected_parent_first() {
    check("mcp_post/redirect-hop/rejected/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_redirect_hop_rejected_tie_parent_first() {
    check("mcp_post/redirect-hop/rejected/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_rejected_tie_parent_last() {
    check("mcp_post/redirect-hop/rejected/tie-parent-last").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_idle_continuation_first() {
    check("mcp_post/redirect-hop/idle/continuation-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_idle_parent_first() {
    check("mcp_post/redirect-hop/idle/parent-first").await;
}

#[tokio::test(start_paused = true)]
#[ignore = "#456 差异：同刻父先注册时 Rust 仍优先完成或超时"]
async fn mcp_post_redirect_hop_idle_tie_parent_first() {
    check("mcp_post/redirect-hop/idle/tie-parent-first").await;
}

#[tokio::test(start_paused = true)]
async fn mcp_post_redirect_hop_idle_tie_parent_last() {
    check("mcp_post/redirect-hop/idle/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_get_io_headers_fulfilled_continuation_first() {
    check("mcp_get/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_fulfilled_parent_first() {
    check("mcp_get/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_fulfilled_tie_parent_first() {
    check("mcp_get/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_fulfilled_tie_parent_last() {
    check("mcp_get/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_get_io_headers_rejected_continuation_first() {
    check("mcp_get/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_rejected_parent_first() {
    check("mcp_get/io-headers/rejected/parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_rejected_tie_parent_first() {
    check("mcp_get/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_rejected_tie_parent_last() {
    check("mcp_get/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_get_io_headers_idle_continuation_first() {
    check("mcp_get/io-headers/idle/continuation-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_idle_parent_first() {
    check("mcp_get/io-headers/idle/parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_idle_tie_parent_first() {
    check("mcp_get/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_get_io_headers_idle_tie_parent_last() {
    check("mcp_get/io-headers/idle/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_post_io_headers_fulfilled_continuation_first() {
    check("mcp_post/io-headers/fulfilled/continuation-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_fulfilled_parent_first() {
    check("mcp_post/io-headers/fulfilled/parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_fulfilled_tie_parent_first() {
    check("mcp_post/io-headers/fulfilled/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_fulfilled_tie_parent_last() {
    check("mcp_post/io-headers/fulfilled/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_post_io_headers_rejected_continuation_first() {
    check("mcp_post/io-headers/rejected/continuation-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_rejected_parent_first() {
    check("mcp_post/io-headers/rejected/parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_rejected_tie_parent_first() {
    check("mcp_post/io-headers/rejected/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_rejected_tie_parent_last() {
    check("mcp_post/io-headers/rejected/tie-parent-last").await;
}

#[tokio::test]
async fn mcp_post_io_headers_idle_continuation_first() {
    check("mcp_post/io-headers/idle/continuation-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_idle_parent_first() {
    check("mcp_post/io-headers/idle/parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_idle_tie_parent_first() {
    check("mcp_post/io-headers/idle/tie-parent-first").await;
}

#[tokio::test]
async fn mcp_post_io_headers_idle_tie_parent_last() {
    check("mcp_post/io-headers/idle/tie-parent-last").await;
}
