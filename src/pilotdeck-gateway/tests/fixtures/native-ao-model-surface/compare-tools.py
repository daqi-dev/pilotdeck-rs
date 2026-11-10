#!/usr/bin/env python3
# anchor-selfcheck: unsupported actual model capture comparator; no product source mutations
"""Product fixture assertion only; does not perform formal parity scoring."""
import json
from pathlib import Path

root = Path(__file__).resolve().parent
left = json.loads((root / "ts/model-requests.json").read_text())
right = json.loads((root / "rust/model-requests.json").read_text())
assert len(left) == len(right) == 10, "actual model request sequence changed"
pairs = []
for index, (ts, rust) in enumerate(zip(left, right)):
    a = {t["function"]["name"]: t["function"] for t in ts.get("tools", [])}
    b = {t["function"]["name"]: t["function"] for t in rust.get("tools", [])}
    assert a.keys() == b.keys(), f"request {index}: tool name sets differ"
    changed = [key for key in a if a[key] != b[key]]
    assert not changed, f"request {index}: tool descriptions/schema differ: {changed}"
    if a:
        assert len(a) == 30, f"request {index}: expected fixture tool topology"
        pairs.append(index)
assert pairs == [1, 3, 5, 6, 8, 9], f"tool-bearing request sequence: {pairs}"
print("PASS: actual TS/Rust Server model tools match for all 6 tool-bearing calls")
