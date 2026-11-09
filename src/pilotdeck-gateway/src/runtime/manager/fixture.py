"""Isolated stdio MCP fixture; exercises the native JSON-RPC transport."""
import json
import os
import sys


for line in sys.stdin:
    request = json.loads(line)
    if "MANAGER_FIXTURE_LOG" in os.environ:
        with open(os.environ["MANAGER_FIXTURE_LOG"], "a", encoding="utf-8") as log:
            log.write(json.dumps(request) + "\n")
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "manager-jsonrpc-fixture", "version": "1"},
        }
    elif method == "tools/list":
        result = {"tools": [{
            "name": "browser_tabs",
            "description": "List fixture browser tabs",
            "inputSchema": {
                "type": "object",
                "properties": {"action": {"type": "string", "enum": ["list"]}},
                "required": ["action"],
            },
        }]}
    elif method == "tools/call":
        if request["params"] != {"name": "browser_tabs", "arguments": {"action": "list"}}:
            response = {"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32602, "message": "Only browser_tabs list is permitted"}}
            print(json.dumps(response), flush=True)
            continue
        failed = os.environ.get("MANAGER_FIXTURE_IS_ERROR") == "1"
        result = {
            "content": [{"type": "text", "text": "fixture browser failed" if failed else "### Open tabs\n- 0: (current) Fixture (https://example.test/)"}],
            "isError": failed,
        }
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
