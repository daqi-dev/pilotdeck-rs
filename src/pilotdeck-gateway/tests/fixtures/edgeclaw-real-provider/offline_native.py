#!/usr/bin/env python3
# anchor-selfcheck: unsupported local HTTP probe driver; no source mutation definitions
"""Exercise the real TS/Rust drivers with a local scripted model, never keys.

This validates probe plumbing, NOT real model quality and NOT a TS golden.
Outputs are exclusive, retained evidence. Node must be exactly v22.22.0.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import threading

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location("edgeclaw_smoke", ROOT / "scripts/smoke/edgeclaw.py")
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class ScriptedRelay(smoke.Relay):
    def forward(self, raw):
        body = smoke.capped_payload(raw)
        system = body["system"]
        content = body["messages"][0]["content"]
        payload = json.loads(content)
        if system.startswith("You classify one focus user turn"):
            store = "heron-settings.toml" in payload["focus_user_turn"]["content"]
            value = {"should_store": store, "labels": [{"type": "project", "reason": "offline fixture"}] if store else []}
        elif system.startswith("You create one append-only project memory note"):
            value = {"skip": False, "reason": "", "name": "Heron configuration", "description": "demo configuration",
                     "markdown": "## Summary\nUse heron-settings.toml on port 43127."}
        elif system.startswith("You decide whether the current query"):
            value = {"route": "none" if "长期遵守" in payload["query"] else "project"}
        elif system.startswith("You select a small number of memory files"):
            value = {"selected_ids": [entry["id"] for entry in payload["manifest"]]}
        else:
            raise ValueError("#1120 unexpected offline model request: " + system[:80])
        response = {"model": smoke.MODEL, "usage": {"output_tokens": 4}, "stop_reason": "end_turn",
                    "content": [{"type": "text", "text": json.dumps(value)}]}
        self.records.append({"verified": True, "request": body, "response": response})
        return json.dumps(response).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--node", default="/opt/homebrew/bin/node")
    parser.add_argument("--rust-probe", type=Path)
    args = parser.parse_args()
    out = args.out.resolve()
    if not out.is_relative_to(ROOT / "_tmp"):
        parser.error("--out must be in this tree _tmp")
    out.mkdir(parents=True, exist_ok=False)
    relay = ScriptedRelay(out, lambda s: s, {})
    server = smoke.server_for(relay)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    rows = []
    try:
        implementations = ["ts"] + (["rust"] if args.rust_probe else [])
        for implementation in implementations:
            for case in (smoke.read_json(smoke.FIXTURE / "cases.json")[i] for i in (0, 2)):
                cell = out / f"{implementation}-{case['id']}"
                cell.mkdir()
                smoke.write_json(cell / "input.json", case)
                relay.begin(cell)
                env = smoke.isolated_env(cell, args.baseline.resolve(), relay, server.server_port)
                with relay.lock:
                    relay.records.clear()
                command = ([str(args.rust_probe.resolve())] if implementation == "rust" else [
                    args.node, "--import", str(args.baseline / "node_modules/tsx/dist/loader.mjs"),
                    str(smoke.FIXTURE / "run.mts")])
                result = smoke.common.run_bounded(command, cell / "project", env, 60)
                smoke.write_json(cell / "process.json", result)
                with relay.lock:
                    calls = list(relay.records)
                smoke.write_json(cell / "local-model.json", calls)
                if result["exit"] != 0 or result["timedOut"]:
                    raise RuntimeError(f"#1120 {implementation} local driver failed: see {cell}/process.json")
                quality = smoke.observe(cell, case, calls)
                if case["facts"]:
                    assert all(quality["extracted"].values()), "#1120 local native writes are readable"
                    assert all(quality["recalled"].values()), "#1120 local native recall reaches context"
                else:
                    assert not quality["chatter_only_any_write"], "#1120 no-store local native control"
                if implementation == "ts":
                    print(json.dumps(smoke.read_json(cell / "node.json"), ensure_ascii=False))
                print(f"{cell.name}: offline capture/flush/recall PASS ({len(calls)} local requests)")
                rows.append({"implementation": implementation, "case": case["id"], "quality": quality})
    finally:
        server.shutdown()
        thread.join(5)
        server.server_close()
    smoke.write_json(out / "offline-result.json", {"evidence_kind": "offline-driver-control", "rows": rows})


if __name__ == "__main__":
    main()
