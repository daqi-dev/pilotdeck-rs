#!/usr/bin/env python3
"""#1841 device-patch recompute: strict-comparator context.budget position race.

Verifies the registered device patch
(specs/parity/device-patches/20261108-1841-trace-context-budget-position.patch)
against the frozen baseline trace.py and the real evidence corpus carried
with issue #1841 (11 labels x 75 rounds/side = 150 native/sidecar pairs).

Fail-closed: the baseline must hash to the registered clean SHA256, the patch
must apply to it, and the applied file must hash to the registered patched
SHA256. Any mismatch aborts before any comparison result is reported.

Usage (must run with -I, no writes outside --out):
    python3 -I 20261108-1841-verify.py \
        --baseline <path to baseline tools/agent-loop-parity/trace.py> \
        --patch <path to 20261108-1841-trace-context-budget-position.patch> \
        --evidence <path to the #1841 evidence directory> \
        [--out <path for the machine-readable JSON result>]

Criteria (registration 20261108-1841.md):
  1. patch applicability: git apply --check, apply, patched sha, restore.
  2. full-corpus recompute: exactly the 15 recorded position-race rounds
     change, deletions only, every deleted row attributable to a
     model.request/model.response/context.budget insertion misalignment
     (path-normalized, full-value row signatures); the other 135 pairs are
     unchanged; residual #6 / text rows are kept row by row.
  3. census recompute: deviation rows match evidence/census-all-labels.txt.
  4. negative controls: value, count, exemption-boundary, and within-kind
     order injections are still caught by the patched comparator.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

CLEAN_SHA256 = "6f4139c9f5938a5bd3e44f6a6bed69a69104e593d9c580bbc2f37c5c56c0d289"
PATCHED_SHA256 = "53a1c7f6f50dead912e420e5c20e5e57dafc89b9e370b371174a39fe770e1ce6"
TARGET_PATH = "tools/agent-loop-parity/trace.py"
# Only these kinds may disappear from a strict report because of the patch:
# each removal must be an insertion row (exactly one side None) of one of
# these kinds, i.e. pure position-misalignment noise.
POSITION_NOISE_KINDS = {"model.request", "model.response", "context.budget"}
# Residual rows that must survive the patch row by row (deadline-family #6
# leftovers and the budget-family user.output text difference).
RESIDUAL_PATH_MARKERS = ("durableStopReason", "resultType")
RESIDUAL_VALUE_MARKERS = ("gateway_submit_failed",)
# Negative-control base pair: a round that passes BOTH comparators with zero
# semantic differences and is not one of the 15 position-race rounds, so any
# failure after an injection is attributable to that injection alone.
NC_BASE = (
    "20261106-issue-1811-quiet2-dd-tool",
    "rust-r1",
    "deadline_during_tool",
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def die(message: str) -> "None":
    print(f"FAIL-CLOSED: {message}", file=sys.stderr)
    raise SystemExit(2)


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def read_jsonl(path: Path) -> list[dict]:
    """JSONL reader that splits on \\n only (never str.splitlines)."""
    records: list[dict] = []
    text = path.read_text(encoding="utf-8")
    for line in text.split("\n"):
        if line.strip():
            records.append(json.loads(line))
    return records


def run_git(cwd: Path, *args: str) -> None:
    proc = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if proc.returncode != 0:
        die(f"git {' '.join(args)} failed: {proc.stderr.strip()}")


def normalized_path(path: str) -> str:
    return re.sub(r"\[\d+\]", "[#]", path)


def row_signature(difference) -> tuple[str, str, str]:
    """Path-normalized, full-value row signature.

    Index renumbering is invisible ([n] -> [#]); the field part of the path
    and both difference values are compared exactly, so a field-level row is
    compared by its field and an insertion row carries its whole record.
    """
    return (
        normalized_path(difference.path),
        json.dumps(difference.left, ensure_ascii=False, sort_keys=True),
        json.dumps(difference.right, ensure_ascii=False, sort_keys=True),
    )


def is_residual(signature: tuple[str, str, str]) -> bool:
    path, left, right = signature
    if any(marker in path for marker in RESIDUAL_PATH_MARKERS):
        return True
    if path.endswith(".text"):
        return True
    return any(marker in left or marker in right for marker in RESIDUAL_VALUE_MARKERS)


def is_position_noise_removal(signature: tuple[str, str, str]) -> tuple[bool, str]:
    """A removed row is attributable iff it is an insertion row (path has no
    pair marker, exactly one side None) of a position-noise kind."""
    path, left, right = signature
    if "~" in path or path != "trace[#]":
        return False, f"non-insertion path {path!r}"
    if (left == "null") == (right == "null"):
        return False, f"not a one-sided insertion: {path!r}"
    value = json.loads(left if left != "null" else right)
    if not isinstance(value, dict) or value.get("kind") not in POSITION_NOISE_KINDS:
        return False, f"insertion kind not in {sorted(POSITION_NOISE_KINDS)}: {value!r}"
    return True, value.get("kind")


def discover_pairs(evidence: Path) -> list[tuple[str, str, Path, Path]]:
    pairs = []
    for sidecar in sorted(evidence.glob("*/rounds/*/*.pilotdeck-current-sidecar.jsonl")):
        native = Path(str(sidecar).replace("-sidecar.jsonl", "-native.jsonl"))
        if not native.exists():
            die(f"sidecar trace without native counterpart: {sidecar}")
        label = sidecar.parts[-4]
        round_name = sidecar.parts[-2]
        pairs.append((label, round_name, native, sidecar))
    return pairs


# ---------------------------------------------------------------- criterion 1

def criterion1(baseline: Path, patch: Path) -> dict:
    clean_bytes = baseline.read_bytes()
    if sha256_bytes(clean_bytes) != CLEAN_SHA256:
        die(f"baseline {baseline} sha256 {sha256_bytes(clean_bytes)} != registered {CLEAN_SHA256}")

    patch_text = patch.read_text(encoding="utf-8")
    targets = re.findall(r"^diff --git a/(\S+) b/(\S+)$", patch_text, re.M)
    if len(targets) != 1 or targets[0] != (TARGET_PATH, TARGET_PATH):
        die(f"patch must touch exactly {TARGET_PATH}, found: {targets}")

    with tempfile.TemporaryDirectory(prefix="pdrs-1841-verify-") as tmp:
        repo = Path(tmp)
        target = repo / TARGET_PATH
        target.parent.mkdir(parents=True)
        target.write_bytes(clean_bytes)
        run_git(repo, "init", "-q", ".")
        run_git(repo, "add", TARGET_PATH)
        run_git(repo, "apply", "--check", str(patch))
        run_git(repo, "apply", str(patch))
        patched_bytes = target.read_bytes()
        patched_sha = sha256_bytes(patched_bytes)
        if patched_sha != PATCHED_SHA256:
            die(f"applied file sha256 {patched_sha} != registered {PATCHED_SHA256}")
        # Restore from the index (which still holds the clean blob) and
        # confirm the tree is back to the clean bytes.
        run_git(repo, "checkout", "--", TARGET_PATH)
        restored_sha = sha256_bytes(target.read_bytes())
        if restored_sha != CLEAN_SHA256:
            die(f"restored file sha256 {restored_sha} != clean {CLEAN_SHA256}")

    return {
        "name": "1 patch applicability",
        "clean_sha256": CLEAN_SHA256,
        "patched_sha256": PATCHED_SHA256,
        "targets": [TARGET_PATH],
        "steps": [
            "baseline sha256 == registered clean value",
            "git apply --check OK (isolated repo, file staged at tools/agent-loop-parity/trace.py)",
            "git apply OK, applied sha256 == registered patched value",
            "git checkout -- restore, sha256 == clean value",
        ],
        "passed": True,
    }


# ---------------------------------------------------------------- criterion 2

def criterion2(evidence: Path, clean, patched, census_deviations: set[tuple[str, str]] | None = None) -> dict:
    pairs = discover_pairs(evidence)
    if len(pairs) != 150:
        die(f"expected 150 native/sidecar pairs under {evidence}, found {len(pairs)}")

    changed: list[dict] = []
    unchanged = 0
    residual_mismatch: list[str] = []
    clean_pass = 0
    for label, round_name, native, sidecar in pairs:
        left = clean.load_trace(native)
        right = clean.load_trace(sidecar)
        clean_rows = Counter(row_signature(d) for d in clean.compare_trace_details(left, right).semantic)
        patched_rows = Counter(row_signature(d) for d in patched.compare_trace_details(left, right).semantic)
        if not clean_rows:
            clean_pass += 1
        removed = clean_rows - patched_rows
        added = patched_rows - clean_rows
        # Residual rows (#6 leftovers, budget-family text diffs) must be kept
        # row by row even in the changed pairs.
        residual_kept = (
            sum(1 for s in clean_rows if is_residual(s))
            == sum(1 for s in patched_rows if is_residual(s))
        )
        if not residual_kept:
            residual_mismatch.append(f"{label}/{round_name}")
        if not removed and not added:
            unchanged += 1
            continue
        attribution = [is_position_noise_removal(sig) for sig, _ in removed.items()]
        changed.append({
            "label": label,
            "round": round_name,
            "removed": [
                {"path": sig[0], "left": sig[1][:400], "right": sig[2][:400], "count": count}
                for sig, count in sorted(removed.items())
            ],
            "added_count": sum(added.values()),
            "attributable": all(ok for ok, _ in attribution),
            "attribution_detail": [detail for ok, detail in attribution if not ok],
        })

    changed_set = {(entry["label"], entry["round"]) for entry in changed}
    matches_census = census_deviations is None or changed_set == census_deviations
    passed = (
        len(changed) == 15
        and unchanged == 135
        and all(entry["added_count"] == 0 for entry in changed)
        and all(entry["attributable"] for entry in changed)
        and not residual_mismatch
        and matches_census
    )
    return {
        "name": "2 full-corpus recompute (150 pairs)",
        "pairs": len(pairs),
        "clean_pass_pairs": clean_pass,
        "changed_pairs": len(changed),
        "unchanged_pairs": unchanged,
        "only_deletions": all(entry["added_count"] == 0 for entry in changed),
        "all_deletions_attributable": all(entry["attributable"] for entry in changed),
        "residual_rows_preserved": not residual_mismatch,
        "residual_mismatch": residual_mismatch,
        "changed_matches_census_deviations": matches_census,
        "changed": changed,
        "passed": passed,
    }


# ---------------------------------------------------------------- criterion 3

def classify_census(records: list[dict]) -> str:
    first: dict[str, object] = {}
    for record in records:
        kind = record.get("kind")
        if kind in ("context.budget", "model.request", "model.response") and kind not in first:
            first[kind] = record.get("sequence")
    cb = first.get("context.budget")
    request = first.get("model.request")
    response = first.get("model.response")
    if cb is None or request is None:
        return "n/a"
    if cb < request:
        return "cb_before_request"
    if response is not None and cb < response:
        return "cb_between_req_resp"
    return "cb_after"


def criterion3(evidence: Path) -> dict:
    reference = evidence / "census-all-labels.txt"
    if not reference.exists():
        die(f"missing census reference {reference}")
    expected = [line.split("\t", 1)[1] for line in reference.read_text(encoding="utf-8").split("\n") if line.startswith("DEVIATION\t")]

    tally: Counter = Counter()
    rows: list[str] = []
    for label, round_name, native, sidecar in discover_pairs(evidence):
        side = "ts" if round_name.startswith("ts-") else "rust"
        sc = classify_census(read_jsonl(sidecar))
        nc = classify_census(read_jsonl(native))
        tally[(side, sc)] += 1
        tally[(side, "native:" + nc)] += 1
        if sc != nc:
            scenario = sidecar.name.split(".pilotdeck-current-sidecar")[0]
            rows.append(f"{label}\t{round_name}\t{scenario}\t{sc}\t{nc}")

    passed = rows == expected
    return {
        "name": "3 census recompute",
        "deviation_rows": rows,
        "expected_rows": expected,
        "deviation_pairs": sorted({(row.split("\t")[0], row.split("\t")[1]) for row in rows}),
        "tally": {f"{side}|{cls}": count for (side, cls), count in sorted(tally.items())},
        "passed": passed,
    }


# ---------------------------------------------------------------- criterion 4

def nc_result(clean, patched, left_records, right_records) -> tuple[dict, dict]:
    clean_out = clean.compare_trace_details(left_records, right_records)
    patched_out = patched.compare_trace_details(left_records, right_records)
    return (
        {"fail": bool(clean_out.semantic), "rows": len(clean_out.semantic),
         "paths": [normalized_path(d.path) for d in clean_out.semantic]},
        {"fail": bool(patched_out.semantic), "rows": len(patched_out.semantic),
         "paths": [normalized_path(d.path) for d in patched_out.semantic]},
    )


def write_temp_trace(records: list[dict], path: Path) -> None:
    path.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in records), encoding="utf-8")


def criterion4(evidence: Path, clean, patched, scratch: Path) -> dict:
    label, round_name, scenario = NC_BASE
    native = evidence / label / "rounds" / round_name / f"{scenario}.pilotdeck-current-native.jsonl"
    sidecar = evidence / label / "rounds" / round_name / f"{scenario}.pilotdeck-current-sidecar.jsonl"
    if not native.exists() or not sidecar.exists():
        die(f"negative-control base pair missing: {native}")
    left = clean.load_trace(native)
    right = clean.load_trace(sidecar)
    base_clean, base_patched = nc_result(clean, patched, left, right)
    if base_clean["fail"] or base_patched["fail"]:
        die("NC base pair must pass both comparators with zero semantic differences")

    raw_left = read_jsonl(native)
    raw_right = read_jsonl(sidecar)

    def with_sidecar(records: list[dict]) -> tuple[dict, dict]:
        sidecar_file = scratch / "nc-sidecar.jsonl"
        write_temp_trace(records, sidecar_file)
        mutated = clean.load_trace(sidecar_file)
        return nc_result(clean, patched, left, mutated)

    def with_both(left_records: list[dict], right_records: list[dict]) -> tuple[dict, dict]:
        native_file = scratch / "nc-native.jsonl"
        sidecar_file = scratch / "nc-sidecar.jsonl"
        write_temp_trace(left_records, native_file)
        write_temp_trace(right_records, sidecar_file)
        return nc_result(clean, patched, clean.load_trace(native_file), clean.load_trace(sidecar_file))

    controls: dict[str, dict] = {}

    # NC-A: value mutation inside one context.budget record.
    mutant = [dict(r) for r in raw_right]
    for record in mutant:
        if record.get("kind") == "context.budget":
            record["used"] = record["used"] + 1
            break
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-A value mutation"] = {
        "injection": "sidecar context.budget used += 1",
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "trace[#]~[#].used value difference present in patched",
    }
    controls["NC-A value mutation"]["killed"] = (
        c_patched["fail"]
        and any(p.endswith(".used") for p in c_patched["paths"])
    )

    # NC-B: count mutation (delete one context.budget record).
    mutant = [dict(r) for r in raw_right]
    for index, record in enumerate(mutant):
        if record.get("kind") == "context.budget":
            del mutant[index]
            break
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-B count mutation"] = {
        "injection": "delete the sidecar context.budget record",
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "context.budget insertion row present in patched",
    }
    controls["NC-B count mutation"]["killed"] = (
        c_patched["fail"]
        and any(p == "trace[#]" for p in c_patched["paths"])
    )

    # NC-C: exemption boundary. Move durable.status(context_budget) past the
    # model.request boundary AND past durable.status(turn_timeout): a position
    # change the reviewed continuable comparator would forgive; strict mode
    # (clean and patched) must still fail.
    mutant = [dict(r) for r in raw_right]
    ds_index = next(
        index for index, record in enumerate(mutant)
        if record.get("kind") == "durable.status" and record.get("event") == "context_budget"
    )
    request_index = next(index for index, record in enumerate(mutant) if record.get("kind") == "model.request")
    record = mutant.pop(ds_index)
    mutant.append(record)
    c_clean, c_patched = with_sidecar(mutant)
    continuable = patched.compare_continuable_trace_details(left, clean.load_trace(scratch / "nc-sidecar.jsonl"))
    controls["NC-C exemption boundary"] = {
        "injection": (
            "move durable.status(context_budget) from raw index "
            f"{ds_index} (before model.request at {request_index}) to the end: "
            "crosses the request boundary and the durable.status(turn_timeout) order"
        ),
        "clean": c_clean,
        "patched": c_patched,
        "continuable_on_mutant": "PASS" if not continuable.semantic else f"FAIL({len(continuable.semantic)})",
        "expected_row": "durable.status mismatch rows present in patched",
    }
    controls["NC-C exemption boundary"]["killed"] = (
        c_patched["fail"]
        and any(p == "trace[#]" or ".event" in p or ".statusKind" in p for p in c_patched["paths"])
    )

    # NC-C-literal (documented, not a gate): moving the record just past the
    # request boundary without crossing another durable record is invisible
    # to BOTH comparators: project_semantic_trace orders durable records only
    # within their own actor stream (pre-existing reviewed projection, not a
    # relaxation of this patch). Recorded as evidence, expected PASS.
    mutant = [dict(r) for r in raw_right]
    record = mutant.pop(ds_index)
    mutant.insert(request_index + 1, record)
    c_clean, c_patched = with_sidecar(mutant)
    controls["NC-C literal request-boundary only (documented)"] = {
        "injection": "move durable.status(context_budget) just past the first model.request",
        "clean": c_clean,
        "patched": c_patched,
        "note": (
            "invisible to clean and patched alike: durable records are ordered only "
            "within the durable actor stream of project_semantic_trace; the observable "
            "boundary (crossing durable.status(turn_timeout)) is covered by NC-C above"
        ),
        "killed": None,
    }

    # NC-D: order within the kind. Every evidence trace carries exactly one
    # context.budget record, so the documented fallback applies: add a second,
    # value-different budget record to BOTH sides, then swap the two on the
    # sidecar.
    budget_histogram = Counter()
    for _, _, pair_native, pair_sidecar in discover_pairs(evidence):
        budget_histogram[
            sum(1 for r in read_jsonl(pair_native) if r.get("kind") == "context.budget")
        ] += 1
        budget_histogram[
            sum(1 for r in read_jsonl(pair_sidecar) if r.get("kind") == "context.budget")
        ] += 1

    def add_second(records: list[dict]) -> list[dict]:
        out: list[dict] = []
        added = False
        for record in records:
            out.append(record)
            if record.get("kind") == "context.budget" and not added:
                extra = dict(record)
                extra["used"] = record["used"] + 500
                extra["displayUsed"] = record["displayUsed"] + 500
                out.append(extra)
                added = True
        assert added
        return out

    two_left = add_second(raw_left)
    two_right = add_second(raw_right)
    base2_clean, base2_patched = with_both(two_left, two_right)
    swapped = [dict(r) for r in two_right]
    budget_index = [i for i, r in enumerate(swapped) if r.get("kind") == "context.budget"]
    swapped[budget_index[0]], swapped[budget_index[1]] = swapped[budget_index[1]], swapped[budget_index[0]]
    c_clean, c_patched = with_both(two_left, swapped)
    controls["NC-D within-kind order"] = {
        "injection": (
            "swap the two context.budget records on the sidecar after adding a second, "
            "value-different budget record to BOTH sides (fallback construction: every "
            f"evidence trace has exactly {dict(sorted(budget_histogram.items()))} budget record(s))"
        ),
        "constructed_base": {"clean": base2_clean, "patched": base2_patched},
        "clean": c_clean,
        "patched": c_patched,
        "expected_row": "trace[#]~[#].used / .displayUsed order differences present in patched",
    }
    controls["NC-D within-kind order"]["killed"] = (
        base2_clean["fail"] is False
        and base2_patched["fail"] is False
        and c_patched["fail"]
        and any(p.endswith(".used") or p.endswith(".displayUsed") for p in c_patched["paths"])
    )

    gated = [name for name, control in controls.items() if control["killed"] is not None]
    passed = all(controls[name]["killed"] for name in gated)
    return {
        "name": "4 negative controls",
        "base_pair": f"{label}/rounds/{round_name}/{scenario}",
        "base_clean": base_clean,
        "base_patched": base_patched,
        "budget_records_per_trace_histogram": {str(k): v for k, v in sorted(budget_histogram.items())},
        "controls": controls,
        "gated_controls": gated,
        "killed": sum(1 for name in gated if controls[name]["killed"]),
        "passed": passed,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--patch", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    for name, path in (("--baseline", args.baseline), ("--patch", args.patch), ("--evidence", args.evidence)):
        if not path.exists():
            die(f"{name} does not exist: {path}")
    # The apply steps run inside a temporary repo; every input path must be
    # absolute so it still resolves from there.
    args.baseline = args.baseline.resolve()
    args.patch = args.patch.resolve()
    args.evidence = args.evidence.resolve()

    criterion_1 = criterion1(args.baseline, args.patch)
    criterion_3 = criterion3(args.evidence)

    with tempfile.TemporaryDirectory(prefix="pdrs-1841-verify-") as tmp:
        scratch = Path(tmp)
        target = scratch / TARGET_PATH
        target.parent.mkdir(parents=True)
        target.write_bytes(args.baseline.read_bytes())
        run_git(scratch, "init", "-q", ".")
        run_git(scratch, "add", TARGET_PATH)
        run_git(scratch, "apply", str(args.patch))
        clean = load_module(args.baseline, "pdrs1841_clean")
        patched = load_module(target, "pdrs1841_patched")
        if clean is patched or clean.compare_trace_details is patched.compare_trace_details:
            die("clean and patched modules must not alias")

        criterion_2 = criterion2(
            args.evidence, clean, patched,
            census_deviations=set(criterion_3["deviation_pairs"]),
        )
        criterion_4 = criterion4(args.evidence, clean, patched, scratch)

    result = {
        "issue": 1841,
        "device_patch": "specs/parity/device-patches/20261108-1841-trace-context-budget-position.patch",
        "baseline": str(args.baseline),
        "evidence": str(args.evidence),
        "criteria": [criterion_1, criterion_2, criterion_3, criterion_4],
    }
    result["passed"] = all(criterion["passed"] for criterion in result["criteria"])

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")

    print("#1841 device-patch verify")
    for criterion in result["criteria"]:
        print(f"- {criterion['name']}: {'PASS' if criterion['passed'] else 'FAIL'}")
    if not result["passed"]:
        for criterion in result["criteria"]:
            if not criterion["passed"]:
                print(json.dumps(criterion, ensure_ascii=False, indent=1))
    print(f"overall: {'PASS' if result['passed'] else 'FAIL'}")
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
