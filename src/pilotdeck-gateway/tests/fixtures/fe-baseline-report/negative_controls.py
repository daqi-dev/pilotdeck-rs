#!/usr/bin/env python3
"""#61.1 anti-skip-rule negative controls on the real baseline 72-case reports.

The ledger rule under test lives in scripts/fe-parity/accounting.mjs
(reconcileSingle inside reconcile/reconcilePair) and is exercised through the
offline entry `run.mjs --reconcile --old-report ... --new-report ...`:
verifyBaseline (Git pin + node_modules integrity) → inventory --list →
reconcilePair → baseline-parity.md. That path never listens and never starts a
browser or the baseline dev server, so these controls run on any host.

Inputs are the *real* Playwright JSON originals of a full multi-profile run
(``--suite baseline --profiles staffdeck,minimal,replacement``), e.g. the
``old-<profile>.json`` / ``new-<profile>.json`` files the runner writes. This
script never edits the originals and never mutates product source: every
negative is applied to a copy of the FIRST old-side profile report, the other
reports are passed through by path, and loadReportPair's distinct-file guard
still applies. There is no cargo, no listen, no Chrome.

Positive control (the #61 acceptance): the pristine originals must reconcile
with no ``missing result`` and no ``duplicate result`` error for any identity —
every one of the 72 identities appears exactly once with exactly one attempt —
and the target identity chosen below must be free of every negative-marker
error in the pristine verdict, so a mutant's red verdict can only come from its
own corruption.

Three report-level negatives (coordinator #61.1), each bound to one target
identity picked from the pristine reconciliation as the first old-side
registration that is PASS in every profile, is outside the fixed skip
allowlist, and never triggers any of the three markers in the pristine verdict
(the new side honestly stays red on its missing surfaces, so it cannot supply
a passing target):

  missing-one    delete the identity's spec from the report copy      -> ``report 1: missing result <caseId>``
  skip-one       flip its single result to ``skipped``                -> ``report 1: skip identity not in fixed allowlist <caseId>``
  duplicate-one  append a duplicate of its spec entry                 -> ``report 1: duplicate result <caseId>``

``caseId`` is the ledger identity ``JSON.stringify([file, full title])``
exactly as accounting.mjs builds it. A row is KILLED only when the reconcile
exit code is non-zero, its result.json contains the identity-scoped marker
prefixed by ``report 1: `` (the mutated original), and that exact marker was
absent from the pristine verdict. Every negative runs to completion; the
summary records KILLED / SURVIVED / INVALID with a reason and the process exits
non-zero unless the positive control holds and every negative is KILLED.

Usage (from the worktree root, after a real baseline run):

    python3 src/pilotdeck-gateway/tests/fixtures/fe-baseline-report/negative_controls.py \
        <out-dir> --old <old-p1.json> <old-p2.json> <old-p3.json> \
                  --new <new-p1.json> <new-p2.json> <new-p3.json>

Out-dir must not exist; each reconcile run writes its own fresh
``_tmp/fe-parity/fe-baseline-report-<n>-<tag>`` output (run.mjs refuses to
overwrite). Requires the real baseline checkout pinned at cc7c4e6 with intact
node_modules (verifyBaseline) — that is the point of #61.
"""
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
RUN = ROOT / "scripts/fe-parity/run.mjs"
LEDGER_ROOT = ROOT / "_tmp/fe-parity"
NODE = "node"

MISSING_MARKER = "missing result"
SKIP_MARKER = "skip identity not in fixed allowlist"
DUPLICATE_MARKER = "duplicate result"
# The report-level negatives, in execution order. Each entry is
# (name, marker, apply) where apply(report, locator) corrupts the loaded copy.
NEGATIVES = [
    ("missing-one", MISSING_MARKER, "delete_result"),
    ("skip-one", SKIP_MARKER, "skip_result"),
    ("duplicate-one", DUPLICATE_MARKER, "duplicate_result"),
]


def case_id(spec_file, title):
    """The ledger identity: JSON.stringify([file, title]) with JS spacing."""
    return json.dumps([spec_file, title], ensure_ascii=False, separators=(",", ":"))


def walk_specs(report):
    """Yield (suite, spec_index, spec_file, full_title) like reportCases."""
    def visit(suite, parents):
        titles = [*parents, suite["title"]] if suite.get("title") and not suite.get("file") else parents
        for index, spec in enumerate(suite.get("specs", [])):
            yield suite, index, spec.get("file", ""), " › ".join([*titles, spec.get("title", "")])
        for child in suite.get("suites", []):
            yield from visit(child, titles)

    for suite in report.get("suites", []):
        yield from visit(suite, [])


def find_target(report, spec_file, title):
    for suite, index, file, full_title in walk_specs(report):
        if file == spec_file and full_title == title:
            return suite, index
    return None, None


def apply_negative(report, spec_file, title, kind):
    """Corrupt the loaded report copy; returns a reason string on failure."""
    suite, index = find_target(report, spec_file, title)
    if suite is None:
        return "stale identity: target not present in the first old-side report"
    spec = suite["specs"][index]
    if len(spec.get("tests", [])) != 1 or len(spec["tests"][0].get("results", [])) != 1:
        return "stale identity: target does not have exactly one test/attempt in the report"
    if kind == "delete_result":
        suite["specs"].pop(index)
    elif kind == "skip_result":
        spec["tests"][0]["results"][0]["status"] = "skipped"
        spec["tests"][0]["status"] = "skipped"
    elif kind == "duplicate_result":
        suite["specs"].insert(index, json.loads(json.dumps(spec)))
    return ""


def ledger_errors(accounting):
    """All verdict lines, old side first, then new side, then pair-level."""
    errors = list(accounting.get("old", {}).get("errors", []))
    errors += list(accounting.get("new", {}).get("errors", []))
    errors += list(accounting.get("errors", []))
    return errors


def run_reconcile(tag, old_reports, new_reports, out_dir):
    """One offline reconcile run into a fresh output dir; never reuses one."""
    output = LEDGER_ROOT / f"fe-baseline-report-{int(time.time())}-{tag}"
    if output.exists():
        raise SystemExit(f"#61 refusing to reuse ledger output {output}")
    command = [NODE, str(RUN), "--reconcile", "--output", str(output)]
    for report in old_reports:
        command += ["--old-report", str(report)]
    for report in new_reports:
        command += ["--new-report", str(report)]
    finished = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=900)
    (out_dir / f"{tag}.stdout.log").write_text(finished.stdout, encoding="utf-8")
    (out_dir / f"{tag}.stderr.log").write_text(finished.stderr, encoding="utf-8")
    try:
        result = json.loads((output / "result.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        return {"tag": tag, "exit": finished.returncode, "errors": [],
                "invalid": f"reconcile produced no result.json: {error!r}"}
    accounting = result.get("accounting")
    if not accounting:
        return {"tag": tag, "exit": finished.returncode, "errors": [],
                "invalid": "reconcile result.json has no accounting (preflight failed)"}
    return {"tag": tag, "exit": finished.returncode, "errors": ledger_errors(accounting),
            "old_errors": list(accounting.get("old", {}).get("errors", [])),
            "invalid": "", "output": str(output), "result": result}


def main():
    args = sys.argv[1:]
    if len(args) < 3 or "--old" not in args or "--new" not in args:
        print(__doc__)
        return 2
    out_dir = Path(args[0]).resolve()
    out_dir.mkdir(parents=True, exist_ok=False)

    def reports(flag):
        index = args.index(flag)
        collected = []
        for value in args[index + 1:]:
            if value.startswith("--"):
                break
            collected.append(Path(value).resolve())
        return collected

    old_reports, new_reports = reports("--old"), reports("--new")
    if not old_reports or not new_reports:
        print("#61 needs at least one old and one new report original", file=sys.stderr)
        return 2

    rows = []

    def record(name, status, reason=""):
        rows.append({"name": name, "status": status, "reason": reason})
        print(f"{name}: {status} {reason}", flush=True)

    # Positive control: the untouched originals must reconcile without any
    # missing/duplicate identity — that is the "72 appear, no duplicates, no
    # gaps" acceptance — and supply the baseline verdict the mutants are
    # compared against.
    pristine = run_reconcile("pristine", old_reports, new_reports, out_dir)
    if pristine["invalid"]:
        record("pristine", "INVALID", pristine["invalid"])
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1
    # Exit code is recorded, not asserted: the real reports are expected to stay
    # red on behavioural gaps (mock coverage, probe surfaces) — the acceptance
    # here is the identity ledger, not a green verdict.
    record("pristine", "PASS",
           f"reconcile exit {pristine['exit']}; {len(pristine['errors'])} ledger errors")
    drift = [error for error in pristine["errors"]
             if MISSING_MARKER in error or DUPLICATE_MARKER in error]
    if drift:
        record("pristine-identities", "FAIL",
               f"real reports lose or duplicate identities: {drift[:3]}")
    else:
        record("pristine-identities", "PASS",
               "no missing/duplicate result error: all 72 identities present exactly once")

    # Target identity: first old-side registration PASS in every profile that is
    # outside the fixed skip allowlist, so the skip negative hits the allowlist
    # rejection, not a reason mismatch. The new side cannot supply a target: it
    # honestly stays red on the surfaces the probe does not implement yet.
    allowlist = {case_id(entry["file"], entry["title"])
                 for entry in json.loads((ROOT / "scripts/fe-parity/skip-allowlist.json").read_text(encoding="utf-8"))["tests"]}
    target = None
    for row in pristine["result"]["accounting"]["old"].get("rows", []):
        identity = case_id(row["file"], row["title"])
        if row["status"] != "PASS" or identity in allowlist:
            continue
        if all(observation.get("status") == "PASS" for observation in row.get("observations", [])):
            target = (row["file"], row["title"])
            break
    if target is None:
        record("target", "INVALID", "no old-side identity is PASS in every profile outside the skip allowlist")
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1
    identity = case_id(*target)
    record("target", "PASS", f"bound to {identity}")
    markers = [f"{marker} {identity}" for _, marker, _ in NEGATIVES]
    if any(any(marker in error for error in pristine["errors"]) for marker in markers):
        record("target", "INVALID", "a negative marker already appears in the pristine verdict")
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1

    for name, marker, kind in NEGATIVES:
        mutant_dir = out_dir / name
        mutant_dir.mkdir()
        copy = mutant_dir / "old-report-1.json"
        copy.write_text(json.dumps(json.loads(old_reports[0].read_text(encoding="utf-8")),
                                   ensure_ascii=False, indent=1), encoding="utf-8")
        report = json.loads(copy.read_text(encoding="utf-8"))
        reason = apply_negative(report, target[0], target[1], kind)
        if reason:
            record(name, "INVALID", reason)
            continue
        copy.write_text(json.dumps(report, ensure_ascii=False), encoding="utf-8")
        verdict = run_reconcile(name, [copy, *old_reports[1:]], new_reports, out_dir)
        if verdict["invalid"]:
            record(name, "INVALID", verdict["invalid"])
            continue
        expected = f"report 1: {marker} {identity}"
        if verdict["exit"] == 0:
            record(name, "SURVIVED", f"ledger stayed green despite {kind}")
        elif not any(expected in error for error in verdict["old_errors"]):
            record(name, "SURVIVED", f"exit {verdict['exit']} without the identity-scoped marker {expected!r}")
        else:
            record(name, "KILLED", f"ledger failed with {expected}")

    (out_dir / "summary.json").write_text(json.dumps(rows, indent=2, ensure_ascii=False), encoding="utf-8")
    negatives = [row for row in rows if row["name"] not in ("pristine", "pristine-identities", "target")]
    killed = sum(row["status"] == "KILLED" for row in negatives)
    green = all(row["status"] in ("PASS", "KILLED") for row in rows)
    print(f"{killed}/{len(negatives)} report negatives KILLED; "
          f"positive controls {'hold' if green else 'BROKEN'}; see {out_dir / 'summary.json'}", flush=True)
    return 0 if green and killed == len(negatives) else 1


if __name__ == "__main__":
    raise SystemExit(main())
