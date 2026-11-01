#!/usr/bin/env python3
"""G03 v1: compare sidecar against sidecar, offline.

Source (copied before editing):
/Users/da/ws/PilotDeck-rust-program-20261009/build/CORE01-g03-rule-decision-independent-a10/verify_a9.py
Source sha256: 7f32dea3ad31046742b63479cad3d99ea5998b1d7890d10d3a2a238013a4a7f4
The channel split and E predicates below are retained from that source.
Native/sidecar declared extensions do not apply to sidecar/sidecar pairs.
"""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

WORKTREE = Path(__file__).resolve().parents[2]


def jsonl_lines(text):
    """Only LF delimits JSONL; preserve internal blanks for caller validation."""
    if not text:
        return []
    lines = text.split('\n')
    return lines[:-1] if lines[-1] == '' else lines


def baseline_path() -> 'Path':
    # 显式 PDRS_BASELINE 优先（PR #13 第 2 轮评审 P3）；其次本 worktree 的 resources/；再次主仓的 resources/。
    configured = os.environ.get("PDRS_BASELINE")
    if configured:
        if Path(configured).is_dir():
            return Path(configured).resolve()
        raise FileNotFoundError(f"PDRS_BASELINE={configured} is not a directory")
    candidates = [WORKTREE / "resources/PilotDeck"]
    common = subprocess.run(["git", "-C", str(WORKTREE), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                            capture_output=True, text=True)
    if common.returncode == 0 and common.stdout.strip():
        candidates.append(Path(common.stdout.strip()).parent / "resources/PilotDeck")
    for candidate in candidates:
        if candidate.is_dir():
            return candidate.resolve()
    raise FileNotFoundError(f"PilotDeck baseline not found at {candidates}; set PDRS_BASELINE to an existing directory")


BASELINE = baseline_path()
TOOLS = BASELINE / "tools/agent-loop-parity"


def load_original_trace():
    # Register the private module BEFORE exec_module: dataclasses consult sys.modules.
    name = "pdrs_original_trace"
    spec = importlib.util.spec_from_file_location(name, TOOLS / "trace.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


T = load_original_trace()


def sha256(path: 'Path') -> 'str':
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canon(value: 'Any') -> 'str':
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def projected(records: 'list[dict[str, Any]]') -> 'list[dict[str, Any]]':
    out = T.project_semantic_trace(records)
    for r in out:
        r.pop("requestEvidence", None)
    return out


def identity_key(r: 'dict[str, Any]') -> 'str':
    """Phase B event identity: kind, agentScope, event|statusKind, attempt, compactionId, itemId, toolCallId."""
    tcid = r.get("toolCallId")
    if tcid is None and isinstance(r.get("result"), dict):
        tcid = r["result"].get("toolCallId")
    parts = [
        r.get("kind"),
        r.get("agentScope"),
        r.get("event"),
        r.get("statusKind"),
        (r.get("detail") or {}).get("reason") if r.get("kind") == "agent.status" and isinstance(r.get("detail"), dict) else None,
        r.get("attempt"),
        r.get("compactionId"),
        r.get("itemId"),
        tcid,
        r.get("state") if r.get("kind") == "sidecar.lifecycle" else None,
    ]
    return canon(parts)


# ---------------------------------------------------------------- identity multiplicity (E11)
def identity_multiplicity_diffs(left_raw, right_raw) -> 'list[dict[str, Any]]':
    left = Counter(identity_key(r) for r in projected(left_raw))
    right = Counter(identity_key(r) for r in projected(right_raw))
    return [{"key": json.loads(key), "left_count": left[key], "right_count": right[key]}
            for key in sorted(left.keys() | right.keys()) if left[key] != right[key]]


# ---------------------------------------------------------------- proposed rule (channel-split)
BUILTIN_GATEWAY_TOOL_NAMES = {  # gateway_impl.mjs:865 (single-turn path) U :690 (multi-turn path)
    "agent", "ask_user_question", "read_file", "write_file", "subagent", "send_message",
    "enter_plan_mode", "exit_plan_mode", "todo_write",
}
DURABLE_KINDS = {"durable.status", "durable.steer", "durable.compaction_completed", "compact.boundary", "durable.state"}  # trace.py:422
GATEWAY_KINDS = {"agent.status", "context.budget", "user.output", "steer.applied", "tool.progress", "gateway.error"}  # Phase A producer C
HARNESS_CONTROL_KINDS = {"steer.request", "cancel.requested", "cancel.acknowledged", "cancel.error", "session.lifecycle"}  # harness timers/control client


def channel_of(raw: 'dict[str, Any]', finish_channel_by_call: 'dict[Any, str] | None' = None) -> 'str':
    """Producer channel of a RAW (canonicalized, un-projected) record. Mechanical, from gateway_impl.mjs push sites.
    Built-in tool.result records carry no tool name (gateway_impl.mjs:890-895, :723-740); they are pushed synchronously
    right after their tool.finish, so they inherit the channel of the nearest preceding tool.finish with the same toolCallId."""
    k = raw.get("kind")
    if k == "tool.result" and T._tool_lifecycle_name(raw) is None and finish_channel_by_call is not None:
        ch = finish_channel_by_call.get(T._tool_lifecycle_identity(raw))
        if ch is not None:
            return ch
    if k in DURABLE_KINDS:
        return "B"
    if k == "sidecar.lifecycle":
        return "L"
    if k in GATEWAY_KINDS:
        return "C"
    if k == "permission.request" and "payload" in raw:  # gateway_impl.mjs:898 gateway form
        return "C"
    if k in ("tool.call", "tool.start", "tool.finish", "tool.result"):
        name = T._tool_lifecycle_name(raw)
        return "C" if name in BUILTIN_GATEWAY_TOOL_NAMES else "A"
    if k in HARNESS_CONTROL_KINDS:
        return "H"
    return "A"  # port invocations (model.*, fault.injected, parity tool.*, policy.*, permission.*, compaction.budget) + post-run projections


def channel_streams(raw_records: 'list[dict[str, Any]]') -> 'dict[str, list[dict[str, Any]]]':
    generated: dict[tuple[str, str], str] = {}
    streams: dict[str, list[dict[str, Any]]] = defaultdict(list)
    finish_channel_by_call: dict[Any, str] = {}
    for raw in raw_records:
        if raw.get("kind") in T._HARNESS_PROOF_KINDS:
            continue
        actor = "child" if raw.get("agentScope") == "child" else "parent"
        ch = channel_of(raw, finish_channel_by_call)
        if raw.get("kind") == "tool.finish":
            finish_channel_by_call[T._tool_lifecycle_identity(raw)] = ch
        rec = T._semantic_record(raw, generated)
        rec.pop("requestEvidence", None)
        streams[f"{actor}/{ch}"].append(rec)
    for key in list(streams):
        streams[key] = T._canonicalize_parallel_tool_lifecycle(streams[key])
    return streams


def rule_verdict(sid: 'str', left_raw, right_raw) -> 'dict[str, Any]':
    """RULE (channel-split comparator):
       1. Original _partial_order_differences on both sides (trace.py:1129-1270: E3 compaction, durable-before-visible, durable-steer-before-applied, E9 settlement).
       2. Records are partitioned per actor (parent/child) and per producer channel A (port invocation), B (persistence completion),
          C (gateway WS delivery), H (harness control), L (sidecar.lifecycle). Each stream is compared positionally with full
          semantic values using the Original's _diff_semantic_records. Order ACROSS channels is not asserted by the comparator;
          it is asserted only by the explicit edges E1-E12 (checked separately in e_rules and by step 1).
       3. Native/sidecar declared extensions are inapplicable to this same-surface pair.
       No record may be missing, added, re-valued, duplicated, or re-ordered within its own (actor, channel) stream."""
    semantic = T._partial_order_differences(left_raw, "left") + T._partial_order_differences(right_raw, "right")
    ls, rs = channel_streams(left_raw), channel_streams(right_raw)
    per_stream: dict[str, int] = {}
    for key in sorted(set(ls) | set(rs)):
        diffs = T._diff_semantic_records(ls.get(key, []), rs.get(key, []))
        for d in diffs:
            semantic.append(T.Difference(f"{key}:{d.path}", d.left, d.right))
        per_stream[key] = len(diffs)
    verdict = "PASS" if not semantic else "FAIL"
    return {"verdict": verdict, "semantic_count": len(semantic), "per_stream_diffs": per_stream,
            "stream_sizes": {k: [len(ls.get(k, [])), len(rs.get(k, []))] for k in sorted(set(ls) | set(rs))},
            "differences": [{"path": d.path, "left": d.left, "right": d.right} for d in semantic]}


# ---------------------------------------------------------------- E-rules on one trace
def idx(records, pred):
    return [i for i, r in enumerate(records) if pred(r)]


def has_tool_call_id(value, tcid):
    """Match structured toolCallId fields, including nested tool_result blocks."""
    if isinstance(value, dict):
        return value.get("toolCallId") == tcid or any(has_tool_call_id(v, tcid) for v in value.values())
    if isinstance(value, list):
        return any(has_tool_call_id(v, tcid) for v in value)
    return False


def e_rules(records: 'list[dict[str, Any]]', side: 'str', sid: 'str') -> 'dict[str, Any]':
    recs = records  # canonicalized raw order incl. harness.proof/operation.terminal
    res: dict[str, Any] = {}
    kind = lambda r: r.get("kind")
    terminal_i = idx(recs, lambda r: kind(r) == "terminal")
    terminal = recs[terminal_i[-1]] if terminal_i else None

    # E1 durable.status(X) before agent.status(X), per occurrence
    viol = []
    for X in ("max_budget_reached", "task_budget_reached", "max_turns_reached", "turn_timeout"):
        d = idx(recs, lambda r: kind(r) == "durable.status" and r.get("event") == X)
        v = idx(recs, lambda r: kind(r) == "agent.status" and r.get("event") == X)
        for n, vi in enumerate(v):
            if n >= len(d) or d[n] > vi:
                viol.append({"event": X, "occurrence": n, "durable": d[n] if n < len(d) else None, "visible": vi})
    exercised = bool(idx(recs, lambda r: kind(r) == "agent.status" and r.get("event") in ("max_budget_reached", "task_budget_reached", "max_turns_reached", "turn_timeout")))
    res["E1"] = {"status": ("FAIL" if viol else ("PASS" if exercised else "N/A")), "violations": viol}
    # E1-visible-variant (informative): durable.status(context_budget)[n] < context.budget[n]
    d = idx(recs, lambda r: kind(r) == "durable.status" and r.get("event") == "context_budget")
    v = idx(recs, lambda r: kind(r) == "context.budget")
    cbviol = [{"occurrence": n, "durable": d[n] if n < len(d) else None, "visible": vi} for n, vi in enumerate(v) if n >= len(d) or d[n] > vi]
    res["E1v_context_budget_durable_before_visible"] = {"status": ("FAIL" if cbviol else ("PASS" if v else "N/A")), "violations": cbviol,
                                                         "durable_count": len(d), "visible_count": len(v)}

    # E2 budget stop
    d = idx(recs, lambda r: kind(r) == "durable.status" and r.get("event") == "max_budget_reached")
    if d:
        di = d[0]
        resp_before = [i for i in idx(recs, lambda r: kind(r) == "model.response") if i < di]
        k = resp_before[-1] if resp_before else None
        a = idx(recs, lambda r: kind(r) == "agent.status" and r.get("event") == "max_budget_reached")
        late = [i for i in idx(recs, lambda r: kind(r) in ("tool.start", "model.request")) if k is not None and i > k]
        ds = [r for r in recs if kind(r) == "durable.state"]
        ok = (k is not None and a and di < a[0] and terminal is not None and a[0] < terminal_i[-1] and not late
              and terminal.get("code") == "agent_max_budget_reached" and terminal.get("stopReason") == "max_budget"
              and ds and ds[0].get("durableStatusCount") == 1)
        res["E2"] = {"status": "PASS" if ok else "FAIL", "k": k, "durable": di, "visible": a, "late_side_effects": late,
                     "terminal_code": terminal.get("code") if terminal else None}
    else:
        res["E2"] = {"status": "N/A"}

    # E3 compaction (own check) + Original partial order
    po = T._partial_order_differences(recs, side)
    cb = idx(recs, lambda r: kind(r) == "compact.boundary")
    e3v = []
    for i in cb:
        cid = recs[i].get("compactionId")
        done = idx(recs, lambda r: kind(r) == "durable.compaction_completed" and r.get("compactionId") == cid)
        req = idx(recs, lambda r: kind(r) == "model.request" and r.get("afterCompactionId") == cid)
        if not done or not req or not (i < done[0] < req[0]):
            e3v.append({"compactionId": cid, "boundary": i, "completed": done, "request": req})
    res["E3"] = {"status": ("FAIL" if e3v else ("PASS" if cb else "N/A")), "violations": e3v}
    res["original_partial_order"] = {"status": "FAIL" if po else "PASS", "differences": [d.path for d in po]}

    # E4 steer
    sa = idx(recs, lambda r: kind(r) == "steer.applied")
    e4v = []
    for i in sa:
        item = recs[i].get("itemId")
        ds_ = idx(recs, lambda r: kind(r) == "durable.steer" and r.get("itemId") == item)
        nxt = [j for j in idx(recs, lambda r: kind(r) == "model.request") if j > i]
        if not ds_ or ds_[0] > i:
            e4v.append({"itemId": item, "durable": ds_, "applied": i, "reason": "durable not before applied"})
        if not nxt:
            e4v.append({"itemId": item, "applied": i, "reason": "no model.request after steer.applied"})
    res["E4"] = {"status": ("FAIL" if e4v else ("PASS" if sa else "N/A")), "violations": e4v}

    # E5 cancel
    cr = idx(recs, lambda r: kind(r) == "cancel.requested")
    if cr:
        ca = idx(recs, lambda r: kind(r) == "cancel.acknowledged")
        ack = ca[0] if ca else None
        late_success = [i for i in idx(recs, lambda r: kind(r) == "tool.finish" and r.get("success") is True) if ack is not None and i > ack]
        late_req = [i for i in idx(recs, lambda r: kind(r) == "model.request") if ack is not None and i > ack]
        cancelled_finish = [{"index": i, "after_requested": i > cr[0], "after_acknowledged": (ack is not None and i > ack), "error": recs[i].get("error")}
                            for i in idx(recs, lambda r: kind(r) == "tool.finish" and r.get("success") is False)]
        tvals = {k_: (terminal or {}).get(k_) for k_ in ("outcome", "resultType", "durableStopReason")}
        ok = (ack is not None and cr[0] < ack and terminal is not None and ack < terminal_i[-1] and not late_success and not late_req
              and tvals == {"outcome": "cancelled", "resultType": "aborted", "durableStopReason": "aborted_streaming"})
        res["E5"] = {"status": "PASS" if ok else "FAIL", "requested": cr[0], "acknowledged": ack, "terminal": tvals,
                     "late_tool_finish_success": late_success, "late_model_request": late_req,
                     "tool_finish_unsuccessful_records": cancelled_finish}
    else:
        res["E5"] = {"status": "N/A"}

    # E6 deadline
    tt = idx(recs, lambda r: kind(r) == "durable.status" and r.get("event") == "turn_timeout")
    if tt:
        ops = [r for r in recs if kind(r) == "operation.terminal"]
        ok = terminal is not None and tt[0] < terminal_i[-1] and terminal.get("outcome") == "failed"
        detail = {"durable": tt[0], "terminal_outcome": terminal.get("outcome") if terminal else None}
        # Both operands are sidecars (old source checked only right).
        # scenarios.json: sidecar deadline operationOutcome = result_unknown.
        ok = ok and bool(ops) and ops[-1].get("outcome") == "result_unknown"
        detail["operation_terminal"] = {k_: ops[-1].get(k_) for k_ in ("outcome", "code")} if ops else None
        if sid == "deadline_during_tool":
            ts = idx(recs, lambda r: kind(r) == "tool.start")
            se = [r for r in recs if kind(r) == "side_effect.state"]
            ok = ok and bool(ts) and ts[0] < terminal_i[-1] and bool(se) and se[0].get("sideEffectCount") == 0
            detail["tool_start"] = ts
            detail["sideEffectCount"] = se[0].get("sideEffectCount") if se else None
        res["E6"] = {"status": "PASS" if ok else "FAIL", **detail}
    else:
        res["E6"] = {"status": "N/A"}

    # E7 terminal persistence (checkable part)
    if terminal is not None:
        e7 = []
        has_durable_projection = bool(idx(recs, lambda r: kind(r) == "durable.state"))  # multi-turn harness path (gateway_impl.mjs:686-) has none
        if has_durable_projection and terminal.get("outcome") in ("completed", "cancelled") and terminal.get("durableStopReason") != terminal.get("stopReason"):
            e7.append({"durableStopReason": terminal.get("durableStopReason"), "stopReason": terminal.get("stopReason")})
        if has_durable_projection and not idx(recs, lambda r: kind(r) == "operation.terminal"):
            e7.append("operation.terminal missing on sidecar side")
        res["E7"] = {"status": ("FAIL" if e7 else ("PASS" if has_durable_projection else "N/A(no durable projection in multi-turn harness path)")), "violations": e7,
                     "uncheckable": "transcript turn_result commit < visible turn_completed (persistence clock not in trace)"}
    else:
        res["E7"] = {"status": "FAIL", "violations": ["no terminal record"]}

    # E8 unknown acceptance fail-closed
    ops = [r for r in recs if kind(r) == "operation.terminal" and r.get("outcome") == "result_unknown"]
    if ops:
        res["E8"] = {"status": "PASS" if terminal is not None and terminal.get("outcome") == "failed" else "FAIL",
                     "terminal_outcome": terminal.get("outcome") if terminal else None}
    else:
        res["E8"] = {"status": "N/A"}

    # E9 settlement
    settle = idx(recs, lambda r: kind(r) == "sidecar.lifecycle" and r.get("state") in ("parent_closed", "parent_abort_acknowledged", "parent_abort_settled"))
    if settle:
        late = [j for j in idx(recs, lambda r: kind(r) == "model.request") if j > settle[0]]
        res["E9"] = {"status": "FAIL" if late else "PASS", "settlement": settle[0], "late_model_request": late}
    else:
        res["E9"] = {"status": "N/A"}

    # E12 tool_result visibility in next model.request of same actor
    e12v, e12n = [], 0
    for i in idx(recs, lambda r: kind(r) == "tool.call"):
        r = recs[i]
        scope = r.get("agentScope") or "parent"
        tcid = r.get("toolCallId")
        reqs = idx(recs, lambda q: kind(q) == "model.request" and (q.get("agentScope") or "parent") == scope)
        after = [j for j in reqs if j > i]
        if not after:
            continue  # terminal after tool; nothing to check
        e12n += 1
        nxt = recs[after[0]]
        if tcid is None or not has_tool_call_id(nxt.get("modelView", {}).get("messages"), tcid):
            e12v.append({"tool.call": i, "toolCallId": tcid, "next_request": after[0], "attempt": nxt.get("attempt")})
    res["E12"] = {"status": ("FAIL" if e12v else ("PASS" if e12n else "N/A")), "checked": e12n, "violations": e12v}
    return res



def scenario_catalog() -> 'dict[str, dict[str, Any]]':
    scenarios = json.loads((TOOLS / "scenarios.json").read_text())["scenarios"]
    return {s["scenarioId"]: s for s in scenarios if "pilotdeck" in s.get("pairs", [])}


def load_aligned(path: 'Path', path_align=()) -> 'list[dict[str, Any]]':
    # Align string values before the Original's canonicalization; never rewrite keys.
    generated = {}
    records = []
    def align(value):
        if isinstance(value, str):
            for old, new in path_align:
                value = value.replace(old, new)
            return value
        if isinstance(value, list):
            return [align(v) for v in value]
        if isinstance(value, dict):
            return {k: align(v) for k, v in value.items()}
        return value
    for number, line in enumerate(jsonl_lines(path.read_text(encoding="utf-8")), 1):
        if not line.strip():
            continue
        value = json.loads(line)
        if not isinstance(value, dict) or not {"kind", "scenarioId", "q", "sequence"} <= value.keys():
            raise ValueError(f"{path}:{number}: invalid Original trace record")
        records.append(T.canonicalize(align(value), generated_ids=generated))
    return records


def trace_health(sid, records, scenario=None):
    errors = []
    if not records:
        return ["empty trace"]
    if any(r.get("scenarioId") != sid for r in records):
        errors.append("scenario identity mismatch")
    sequence = [r.get("sequence") for r in records]
    if any(type(n) is not int for n in sequence) or sequence != list(range(len(records))):
        errors.append("sequence must be contiguous from zero")
    if sum(r.get("kind") == "terminal" for r in records) != 1:
        errors.append("exactly one terminal required")
    if scenario is not None:
        errors += T.validate_production_sidecar_proof(
            records, set(scenario.get("requiredSidecarModules", [])),
            set(scenario.get("requiredSidecarOperations", [])))
        errors += [d.path for d in T.validate_trace_expectations(
            records, scenario, "pilotdeck", "pilotdeck-current-sidecar")]
    identities = [r for r in records if r.get("kind") == "harness.proof" and r.get("state") == "sidecar_identity"]
    for r in identities:
        if r.get("side") not in ("ts", "rust") or not r.get("sha256") or r.get("sha256") != r.get("expectedSha256") or r.get("spawnObserved") is not True:
            errors.append("sidecar identity proof invalid")
    return errors


def compare_records(sid, left, right, scenario=None):
    channel = rule_verdict(sid, left, right)
    edges = {"left": e_rules(left, "left", sid), "right": e_rules(right, "right", sid)}
    # E10 and E11 were already enforced implicitly by the original stream comparator.
    edges["pair"] = {
        "E10": {"status": "FAIL" if any(channel["per_stream_diffs"].values()) else "PASS"},
        "E11": {"status": "FAIL" if identity_multiplicity_diffs(left, right) else "PASS"},
    }
    health = {"left": trace_health(sid, left, scenario), "right": trace_health(sid, right, scenario)}
    failed = sorted({name for rules in edges.values() for name, result in rules.items() if result.get("status") == "FAIL"})
    if any(channel["per_stream_diffs"].values()):
        failed.append("G03_channels")
    if any(health.values()):
        failed.append("trace_health")
    return {"verdict": "PASS" if channel["verdict"] == "PASS" and not failed else "FAIL",
            "triggered_rules": sorted(set(failed)), "edges": edges, "channel": channel, "health": health}


def artifact_dir(directory):
    directory = Path(directory)
    return directory / "artifacts" if (directory / "artifacts").is_dir() else directory


def compare_directories(left_dir, right_dir, *, side_left="sidecar", side_right="sidecar", path_align=()):
    if side_left != "sidecar" or side_right != "sidecar":
        raise ValueError("G03 compares sidecar against sidecar only")
    catalog = scenario_catalog()
    per_scenario = {}
    ledgers = {}
    for name, directory in (("left", left_dir), ("right", right_dir)):
        ledger = Path(directory) / "run.json"
        if not ledger.exists() and Path(directory).name == "artifacts":
            ledger = Path(directory).parent / "run.json"
        if ledger.exists():
            try:
                ledgers[name] = json.loads(ledger.read_text())
            except (OSError, ValueError):
                ledgers[name] = {"valid_round": False}
        elif artifact_dir(directory).name == "artifacts":
            ledgers[name] = {"valid_round": False, "error": "new round missing run.json"}
    for sid, scenario in catalog.items():
        paths = [artifact_dir(d) / f"{sid}.pilotdeck-current-sidecar.jsonl" for d in (left_dir, right_dir)]
        try:
            left, right = [load_aligned(p, path_align) for p in paths]
            result = compare_records(sid, left, right, scenario)
            for name, records in zip(("left", "right"), (left, right)):
                proofs = [r for r in records if r.get("kind") == "harness.proof" and r.get("state") == "sidecar_identity"]
                if name not in ledgers:
                    if not proofs:
                        continue  # upstream TS captures predate the identity wrapper
                    ledgers[name] = {"valid_round": False, "error": "identity proof missing run.json"}
                ledger = ledgers[name]
                valid_identity = (ledger.get("valid_round") is True and ledger.get("side") in ("ts", "rust")
                    and len(proofs) == 1 and proofs[0].get("side") == ledger.get("side")
                    and proofs[0].get("sha256") == ledger.get("expected_sha256") and proofs[0].get("spawnObserved") is True)
                if not valid_identity:
                    result["verdict"] = "FAIL"
                    result["triggered_rules"] = sorted(set(result["triggered_rules"] + ["trace_identity"]))
            result["trace_sha256"] = dict(zip(("left", "right"), map(sha256, paths)))
        except (OSError, ValueError, TypeError, KeyError, AttributeError, IndexError) as error:
            result = {"verdict": "FAIL", "triggered_rules": ["trace_input"], "error": str(error)}
        per_scenario[sid] = result
    # BLOCKED invalidates a TS calibration round, even when partial traces survived.
    blocked = {}
    for name, directory in (("left", left_dir), ("right", right_dir)):
        summary = artifact_dir(directory) / "summary.json"
        if summary.exists():
            try:
                contents = json.loads(summary.read_text())
                if not isinstance(contents, dict):
                    raise ValueError("summary must be a JSON object")
                blocked[name] = contents.get("blocked", [])
            except (ValueError, OSError) as error:
                blocked[name] = [str(error)]
    complete = len(catalog) == 53 and all("trace_input" not in r["triggered_rules"] for r in per_scenario.values())
    passed = [sid for sid, r in per_scenario.items() if r["verdict"] == "PASS"]
    return {"verdict": "PASS" if complete and len(passed) == 53 and not any(blocked.values()) else "FAIL",
            "complete": complete, "expected_scenarios": len(catalog), "passed": len(passed),
            "left": str(Path(left_dir).resolve()), "right": str(Path(right_dir).resolve()),
            "blocked": blocked, "per_scenario": per_scenario,
            "original_sha256": {f: sha256(TOOLS / f) for f in ("trace.py", "scenarios.json")}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    parser.add_argument("--side-left", choices=["sidecar"], default="sidecar")
    parser.add_argument("--side-right", choices=["sidecar"], default="sidecar")
    parser.add_argument("--path-align", action="append", default=[], metavar="OLD=NEW")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    align = []
    for item in args.path_align:
        if "=" not in item or not item.split("=", 1)[0]:
            parser.error("--path-align requires a nonempty OLD=NEW")
        align.append(tuple(item.split("=", 1)))
    result = compare_directories(args.left, args.right, side_left=args.side_left, side_right=args.side_right, path_align=align)
    data = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(data)
        print(json.dumps({k: result[k] for k in ("verdict", "complete", "passed", "expected_scenarios")}))
    else:
        print(data, end="")
    return 0 if result["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
