#![cfg(unix)]
//! Production-path proof for SDK-defined stdio MCP tools in a native child.

use super::agent_tests::{
    ModelFixture, REPORT, Reply, assert_success, execute, input, root, router, snapshot, tool_names,
};
use crate::runtime::{network::Cancellation, session::module_host::EventBuffer};
use serde_json::json;
use std::{fs, sync::Arc, time::Duration};
use tokio::time::{sleep, timeout};

const STDIO_SERVER: &str = r#"#!/bin/sh
set -eu
cleanup() {
  printf '%s\n' cleanup >> "$MCP_REQUEST_LOG"
  printf '%s' "$MCP_PROOF" > "$MCP_CLEANUP_MARKER"
}
trap cleanup EXIT
trap 'exit 0' TERM INT

while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' initialize >> "$MCP_REQUEST_LOG"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"agent-stdio-proof","version":"1"}}}\n' "$id"
      ;;
    *'"method":"notifications/initialized"'*)
      printf '%s\n' notifications/initialized >> "$MCP_REQUEST_LOG"
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' tools/list >> "$MCP_REQUEST_LOG"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"proof","description":"Return the stdio MCP proof","inputSchema":{"type":"object","properties":{}},"annotations":{"readOnlyHint":true}}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' tools/call >> "$MCP_REQUEST_LOG"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}],"isError":false}}\n' "$id" "$MCP_PROOF"
      ;;
    *)
      printf 'unexpected:%s\n' "$line" >> "$MCP_REQUEST_LOG"
      if [ -n "$id" ]; then
        printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id"
      fi
      ;;
  esac
done
"#;

#[tokio::test]
async fn definition_stdio_mcp_initializes_lists_calls_and_cleans_up_in_production() {
    let root = root();
    let script = root.join("definition-mcp-fixture.sh");
    let request_log = root.join("definition-mcp-requests.log");
    let cleanup_marker = root.join("definition-mcp-cleanup.marker");
    fs::write(&script, STDIO_SERVER).unwrap();

    let proof = format!("native-stdio-mcp-proof-{}", uuid::Uuid::new_v4());
    let model = ModelFixture::start(vec![
        Reply::Tool {
            name: "mcp__fixture__proof".into(),
            arguments: json!({}),
        },
        Reply::Text(REPORT.into()),
    ])
    .await;
    let child = input(
        &root,
        json!({ "subagentDefinitions": { "stdio-proof": {
            "allowedTools": ["mcp__fixture__proof"],
            "mcpServers": { "fixture": {
                "type": "stdio",
                "command": "/bin/sh",
                "args": [script],
                "env": {
                    "MCP_PROOF": proof,
                    "MCP_REQUEST_LOG": request_log,
                    "MCP_CLEANUP_MARKER": cleanup_marker,
                },
                "cwd": root,
                "timeout": 2000,
            } },
        } } }),
    );
    let router = router(
        snapshot(&model.url),
        &child,
        Arc::new(EventBuffer::default()),
        Cancellation::new(),
    );
    let result = execute(
        &router,
        "agent",
        json!({
            "description": "stdio MCP proof",
            "prompt": "Call the proof MCP tool and return a structured report.",
            "subagent_type": "stdio-proof",
        }),
    )
    .await;
    assert_success(&result);

    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(tool_names(&requests[0]), vec!["mcp__fixture__proof"]);
        assert!(
            requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["role"] == "tool"
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.contains(&proof))
                }),
            "the stdio tools/call result must reach the real child model: {}",
            requests[1]
        );
    }

    timeout(Duration::from_secs(2), async {
        while !cleanup_marker.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("definition MCP cleanup must stop the stdio child");
    assert_eq!(fs::read_to_string(&cleanup_marker).unwrap(), proof);
    let methods: Vec<_> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
            "cleanup",
        ]
    );
}
