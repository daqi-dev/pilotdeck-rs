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

Positive control (the #61 acceptance), fix-r2: only the real reconcile output
is trusted — there is no second, hand-written accounting here (fix-r1's
independent Python scan drifted from accounting.mjs's strict comparisons and
accepted corrupted originals). ``assess_pristine`` reads the pristine run's own
``result.json`` and fails closed on every shape but the exact expected one:
``registration`` present with an empty error array and exactly 72 identities
matching the exp007 frozen inventory (never an empty set), ``accounting.old``
and ``accounting.new`` each carrying an ``errors`` array and a denominator of
exactly 72, an exit code that agrees with the reported status, and zero
*identity-ledger* error classes — the error classes accounting.mjs itself
emits for the ledger dimensions (missing / duplicate result, expected one
attempt, unknown result, unexecuted/interrupted status, skip outside the fixed
allowlist). Behavioural failures (failed probes, D pending orchestration, skip
floors, stale comparisons) are honestly red and are deliberately NOT ledger
defects; the acceptance is the identity ledger, not a green verdict.

Nine report-level negatives, each bound to one target identity picked from the
pristine reconciliation as the first old-side registration that is PASS in
every profile, is outside the fixed skip allowlist, and never triggers any
negative marker in the pristine verdict (the new side honestly stays red on
its missing surfaces, so it cannot supply a passing target):

  missing-one        delete the identity's spec from the report copy      -> ``report 1: missing result <caseId>``
  skip-one           flip its single result to ``skipped``                -> ``report 1: skip identity not in fixed allowlist <caseId>``
  duplicate-one      append a duplicate of its spec entry                 -> ``report 1: duplicate result <caseId>``
  extra-attempt-one  append a duplicate of its single attempt             -> ``report 1: expected one attempt, got 2: <caseId>``
  retry-null-one     set its attempt's ``retry`` to null                  -> ``report 1: expected one attempt, got 1: <caseId>``
  retry-false-one    set its attempt's ``retry`` to false                 -> ``report 1: expected one attempt, got 1: <caseId>``
  retry-one          set its attempt's ``retry`` to 1                     -> ``report 1: expected one attempt, got 1: <caseId>``
  unknown-result-one insert a renamed clone spec (unregistered identity)  -> ``report 1: unknown result <mutant caseId>``
  unknown-status-one  flip its result status to ``interrupted``           -> ``report 1: unexecuted/interrupted status interrupted: <caseId>``

``caseId`` is the ledger identity ``JSON.stringify([file, full title])``
exactly as accounting.mjs builds it. The three retry negatives exist because
accounting.mjs rejects attempts with a strict ``retry !== undefined &&
retry !== 0`` (a null or boolean retry is a retried attempt, not a clean one);
the kill judgement consumes only the real accounting verdict. A row is KILLED
only when the reconcile exit code is non-zero and its old-side errors contain
the identity-scoped marker prefixed by ``report 1: `` (the mutated original)
and that exact marker was absent from the pristine verdict. Every negative
runs to completion; the summary records KILLED / SURVIVED / INVALID with a
reason and the process exits non-zero unless the positive control holds and
every negative is KILLED.

Usage (from the worktree root, after a real baseline run):

    python3 src/pilotdeck-gateway/tests/fixtures/fe-baseline-report/negative_controls.py \\
        <out-dir> --old <old-p1.json> <old-p2.json> <old-p3.json> \\
                  --new <new-p1.json> <new-p2.json> <new-p3.json>

Out-dir must not exist; each reconcile run writes its own fresh
``_tmp/fe-parity/fe-baseline-report-<n>-<tag>`` output (run.mjs refuses to
overwrite). Requires the real baseline checkout pinned at cc7c4e6 with intact
node_modules (verifyBaseline) — that is the point of #61.
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
RUN = ROOT / "scripts/fe-parity/run.mjs"
LEDGER_ROOT = ROOT / "_tmp/fe-parity"
NODE = "node"

# The exp007 frozen registration count: every side's denominator and the
# expanded registration must be exactly this (an empty or drifted inventory
# fails the positive control instead of vacuously passing).
EXPECTED_REGISTRATION = 72

# Appended to the target's leaf title by the unknown-result-one mutant; the
# resulting full title is an unregistered identity accounting must reject.
UNKNOWN_IDENTITY_SUFFIX = "#61-unknown-result-one"

# The identity-ledger error classes of the real accounting (accounting.mjs
# reconcileSingle/reconcile): the pristine originals must carry none of them
# in either side's errors. Behavioural red (failed probes, D orchestration,
# skip floors, stale comparisons, metadata) is not a ledger defect.
LEDGER_CATEGORIES = (
    "missing result ",
    "duplicate result ",
    "expected one attempt, got ",
    "unknown result ",
    "unexecuted/interrupted status ",
    "skip identity not in fixed allowlist ",
)
A_REWRITE_LEDGER = "A rewrite result never credits"
REPORT_PREFIX = re.compile(r"^report \d+: ")

# The report-level negatives, in execution order: (name, corruption kind).
NEGATIVES = [
    ("missing-one", "delete_result"),
    ("skip-one", "skip_result"),
    ("duplicate-one", "duplicate_result"),
    ("extra-attempt-one", "extra_attempt"),
    ("retry-null-one", "retry_null"),
    ("retry-false-one", "retry_false"),
    ("retry-one", "retry_one"),
    ("unknown-result-one", "unknown_identity"),
    ("unknown-status-one", "unknown_status"),
]


def usage(stream=sys.stderr):
    print("usage: negative_controls.py <out-dir> --old <old-report-1.json> [...] "
          "--new <new-report-1.json> [...]", file=stream)
    print("  <out-dir>  must not exist; per-run logs, mutated copies and summary.json land there",
          file=stream)
    print("  --old      at least one real old-side Playwright JSON original (repeat per profile "
          "or space-separate)", file=stream)
    print("  --new      at least one real new-side Playwright JSON original", file=stream)
    print("example (worktree root, after --suite baseline --profiles staffdeck,minimal,replacement):",
          file=stream)
    print("  python3 src/pilotdeck-gateway/tests/fixtures/fe-baseline-report/negative_controls.py \\",
          file=stream)
    print("      _tmp/fe-parity/neg61 \\",
          file=stream)
    print("      --old _tmp/fe-parity/issue61-baseline-1/old-staffdeck.json \\",
          file=stream)
    print("            _tmp/fe-parity/issue61-baseline-1/old-minimal.json \\",
          file=stream)
    print("            _tmp/fe-parity/issue61-baseline-1/old-replacement.json \\",
          file=stream)
    print("      --new _tmp/fe-parity/issue61-baseline-1/new-staffdeck.json \\",
          file=stream)
    print("            _tmp/fe-parity/issue61-baseline-1/new-minimal.json \\",
          file=stream)
    print("            _tmp/fe-parity/issue61-baseline-1/new-replacement.json", file=stream)


def case_id(spec_file, title):
    """The ledger identity: JSON.stringify([file, title]) with JS spacing."""
    return json.dumps([spec_file, title], ensure_ascii=False, separators=(",", ":"))


def ledger_category(error):
    """The identity-ledger error class of a real accounting error, or None.

    Multi-profile sides prefix each error with ``report N: ``; strip it before
    matching accounting.mjs's own class prefixes, so the classes stay the
    producer's wording, not an independent re-derivation."""
    body = REPORT_PREFIX.sub("", error, count=1)
    if body.startswith(A_REWRITE_LEDGER):
        return "unknown result"
    for prefix in LEDGER_CATEGORIES:
        if body.startswith(prefix):
            return prefix.strip()
    return None


def assess_pristine(result, exit_code):
    """Fail-closed assessment of a real ``run.mjs --reconcile`` result.

    Returns a list of failures; empty means the pristine identity ledger
    holds. Every shape but the exact expected one fails closed: a missing or
    malformed registration, a non-array errors field, a denominator that is
    not exactly the registered identity count (an empty or drifted
    inventory), an exit code disagreeing with the reported status, or any
    identity-ledger error class reported by the real accounting."""
    failures = []
    if not isinstance(result, dict):
        return [f"result.json is not an object: {type(result).__name__}"]
    registration = result.get("registration")
    if not isinstance(registration, dict):
        failures.append(f"result.json has no registration object: {registration!r}")
    else:
        errors = registration.get("errors")
        if not isinstance(errors, list):
            failures.append(f"registration errors is not an array: {errors!r}")
        elif errors:
            failures.append(f"registration reports {len(errors)} error(s): {errors[:2]}")
        actual, expected = registration.get("actual"), registration.get("expected")
        for name, value in (("actual", actual), ("expected", expected)):
            if not isinstance(value, int):
                failures.append(f"registration {name} is not an integer: {value!r}")
        if isinstance(actual, int) and actual != EXPECTED_REGISTRATION:
            failures.append(f"registration actual {actual} != {EXPECTED_REGISTRATION} registered identities")
        if isinstance(actual, int) and isinstance(expected, int) and actual != expected:
            failures.append(f"registration actual {actual} != exp007 expected {expected}")
        if isinstance(actual, int) and actual <= 0:
            failures.append(f"registration is empty: {actual} identities")
    accounting = result.get("accounting")
    if not isinstance(accounting, dict):
        failures.append(f"result.json has no accounting object: {accounting!r}")
        return failures
    for side in ("old", "new"):
        account = accounting.get(side)
        if not isinstance(account, dict):
            failures.append(f"accounting.{side} is not an object: {account!r}")
            continue
        errors = account.get("errors")
        if not isinstance(errors, list):
            failures.append(f"accounting.{side}.errors is not an array: {errors!r}")
            errors = []
        denominator = account.get("denominator")
        if not isinstance(denominator, int) or denominator != EXPECTED_REGISTRATION:
            failures.append(f"accounting.{side} denominator {denominator!r} != "
                            f"{EXPECTED_REGISTRATION} registered identities")
        ledger = [error for error in errors if ledger_category(error)]
        if ledger:
            failures.append(f"accounting.{side} reports identity-ledger errors: "
                            f"{ledger[:2]} ({len(ledger)} total)")
    status = result.get("status")
    green = isinstance(status, str) and status.startswith("PASS")
    if green != (exit_code == 0):
        failures.append(f"reconcile exit {exit_code} disagrees with reported status {status!r}")
    return failures


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
    """Corrupt the loaded report copy; returns (reason, expected old-side
    marker). The marker is the identity-scoped accounting error, without the
    ``report 1: `` prefix the caller adds."""
    suite, index = find_target(report, spec_file, title)
    if suite is None:
        return "stale identity: target not present in the first old-side report", ""
    spec = suite["specs"][index]
    if len(spec.get("tests", [])) != 1 or len(spec["tests"][0].get("results", [])) != 1:
        return "stale identity: target does not have exactly one test/attempt in the report", ""
    identity = case_id(spec_file, title)
    if kind == "delete_result":
        suite["specs"].pop(index)
        return "", f"missing result {identity}"
    if kind == "skip_result":
        spec["tests"][0]["results"][0]["status"] = "skipped"
        spec["tests"][0]["status"] = "skipped"
        return "", f"skip identity not in fixed allowlist {identity}"
    if kind == "duplicate_result":
        suite["specs"].insert(index, json.loads(json.dumps(spec)))
        return "", f"duplicate result {identity}"
    if kind == "extra_attempt":
        spec["tests"][0]["results"].append(json.loads(json.dumps(spec["tests"][0]["results"][0])))
        return "", f"expected one attempt, got 2: {identity}"
    if kind in ("retry_null", "retry_false", "retry_one"):
        # accounting.mjs accepts an attempt only under the strict
        # `retry !== undefined && retry !== 0`: null, false and 1 are all
        # retried attempts and must be rejected as "expected one attempt".
        spec["tests"][0]["results"][0]["retry"] = {"retry_null": None, "retry_false": False,
                                                   "retry_one": 1}[kind]
        return "", f"expected one attempt, got 1: {identity}"
    if kind == "unknown_identity":
        clone = json.loads(json.dumps(spec))
        clone["title"] = f"{clone['title']}{UNKNOWN_IDENTITY_SUFFIX}"
        suite["specs"].insert(index + 1, clone)
        return "", f"unknown result {case_id(spec_file, title + UNKNOWN_IDENTITY_SUFFIX)}"
    if kind == "unknown_status":
        spec["tests"][0]["results"][0]["status"] = "interrupted"
        spec["tests"][0]["status"] = "interrupted"
        return "", f"unexecuted/interrupted status interrupted: {identity}"
    return f"unknown negative kind {kind!r}", ""


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
        usage()
        return 2
    out_dir = Path(args[0]).resolve()

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
        usage()
        return 2
    missing = [str(path) for path in [*old_reports, *new_reports] if not path.is_file()]
    if missing:
        print(f"#61 missing report original(s): {missing}", file=sys.stderr)
        return 2
    try:
        out_dir.mkdir(parents=True, exist_ok=False)
    except FileExistsError:
        print(f"#61 refusing to reuse out-dir {out_dir}; pass a fresh directory", file=sys.stderr)
        usage()
        return 2

    rows = []

    def record(name, status, reason=""):
        rows.append({"name": name, "status": status, "reason": reason})
        print(f"{name}: {status} {reason}", flush=True)

    # Positive control: the untouched originals must carry a sound identity
    # ledger, judged only by the real reconcile output (fix-r2). The exit code
    # is recorded as agreeing with the reported status — the real reports are
    # expected to stay red on behavioural gaps (mock coverage, probe surfaces,
    # D orchestration); the acceptance under test is the identity ledger.
    pristine = run_reconcile("pristine", old_reports, new_reports, out_dir)
    if pristine["invalid"]:
        record("pristine", "INVALID", pristine["invalid"])
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1
    failures = assess_pristine(pristine["result"], pristine["exit"])
    if failures:
        record("pristine", "FAIL", "; ".join(failures))
    else:
        accounting = pristine["result"]["accounting"]
        record("pristine", "PASS",
               f"real reconcile: exit {pristine['exit']} agrees with status "
               f"{pristine['result']['status']!r} (honest behavioural red preserved); "
               f"0 identity-ledger error classes in "
               f"{len(accounting['old']['errors'])}+{len(accounting['new']['errors'])} side errors; "
               f"denominators {accounting['old']['denominator']}/{accounting['new']['denominator']} "
               f"== registration {EXPECTED_REGISTRATION}")

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

    for name, kind in NEGATIVES:
        mutant_dir = out_dir / name
        mutant_dir.mkdir()
        copy = mutant_dir / "old-report-1.json"
        copy.write_text(json.dumps(json.loads(old_reports[0].read_text(encoding="utf-8")),
                                   ensure_ascii=False, indent=1), encoding="utf-8")
        report = json.loads(copy.read_text(encoding="utf-8"))
        reason, marker = apply_negative(report, target[0], target[1], kind)
        if reason:
            record(name, "INVALID", reason)
            continue
        copy.write_text(json.dumps(report, ensure_ascii=False), encoding="utf-8")
        expected = f"report 1: {marker}"
        if any(expected in error for error in pristine["errors"]):
            record(name, "INVALID", f"marker {expected!r} already appears in the pristine verdict")
            continue
        verdict = run_reconcile(name, [copy, *old_reports[1:]], new_reports, out_dir)
        if verdict["invalid"]:
            record(name, "INVALID", verdict["invalid"])
            continue
        if verdict["exit"] == 0:
            record(name, "SURVIVED", f"real accounting stayed green despite {kind}")
        elif not any(expected in error for error in verdict["old_errors"]):
            record(name, "SURVIVED", f"exit {verdict['exit']} without the identity-scoped marker {expected!r}")
        else:
            record(name, "KILLED", f"real accounting rejected it: {expected}")

    (out_dir / "summary.json").write_text(json.dumps(rows, indent=2, ensure_ascii=False), encoding="utf-8")
    negatives = [row for row in rows if row["name"] not in ("pristine", "target")]
    killed = sum(row["status"] == "KILLED" for row in negatives)
    green = all(row["status"] in ("PASS", "KILLED") for row in rows)
    print(f"{killed}/{len(negatives)} report negatives KILLED; "
          f"positive control {'holds' if green else 'BROKEN'}; see {out_dir / 'summary.json'}", flush=True)
    return 0 if green and killed == len(negatives) else 1


if __name__ == "__main__":
    raise SystemExit(main())
