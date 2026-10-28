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

Positive control (the #61 acceptance): the pristine originals must carry a
sound *identity ledger*. This is not a message whitelist over accounting.mjs
error strings (fix-r1: filtering for ``missing result`` / ``duplicate result``
let any other corruption of the originals pass); instead every original is
independently re-scanned in this script, mirroring reportCases' file/title
normalisation: each report must attest ``config.forbidOnly=true``, carry no
Playwright-level ``errors``, hold exactly the registered inventory identity
set (no unknown identity, no gap, no duplicate), and give every identity
exactly one test with exactly one attempt (retry absent or 0) whose result
status is one of passed/failed/timedOut/skipped. Behavioural failures stay
honestly red in the reconcile verdict and are deliberately NOT part of this
scan — the acceptance is the identity ledger, not a green verdict.

Six report-level negatives, each bound to one target identity picked from the
pristine reconciliation as the first old-side registration that is PASS in
every profile, is outside the fixed skip allowlist, and never triggers any
negative marker in the pristine verdict (the new side honestly stays red on
its missing surfaces, so it cannot supply a passing target):

  missing-one        delete the identity's spec from the report copy      -> ``report 1: missing result <caseId>``
  skip-one           flip its single result to ``skipped``                -> ``report 1: skip identity not in fixed allowlist <caseId>``
  duplicate-one      append a duplicate of its spec entry                 -> ``report 1: duplicate result <caseId>``
  extra-attempt-one  append a duplicate of its single attempt             -> ``report 1: expected one attempt, got 2: <caseId>``
  unknown-result-one insert a renamed clone spec (unregistered identity)  -> ``report 1: unknown result <mutant caseId>``
  unknown-status-one  flip its result status to ``interrupted``           -> ``report 1: unexecuted/interrupted status interrupted: <caseId>``

``caseId`` is the ledger identity ``JSON.stringify([file, full title])``
exactly as accounting.mjs builds it. A row is KILLED only when the reconcile
exit code is non-zero, its old-side errors contain the identity-scoped marker
prefixed by ``report 1: `` (the mutated original), that exact marker was absent
from the pristine verdict, and — except for skip policy, which is
accounting.mjs's own domain — the pristine scan above also flags the
corrupted copy on its own, so the positive control is itself exercised by
every negative. Every negative runs to completion; the summary records
KILLED / SURVIVED / INVALID with a reason and the process exits non-zero
unless the positive controls hold and every negative is KILLED.

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
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
RUN = ROOT / "scripts/fe-parity/run.mjs"
LEDGER_ROOT = ROOT / "_tmp/fe-parity"
NODE = "node"

# Appended to the target's leaf title by the unknown-result-one mutant; the
# resulting full title is an unregistered identity accounting must reject.
UNKNOWN_IDENTITY_SUFFIX = "#61-unknown-result-one"

# The report-level negatives, in execution order. Each entry is
# (name, corruption kind, scan-prefix): the identity-ledger violation the
# pristine scan must independently report on the corrupted copy, or None when
# the scan deliberately does not police that category (skip policy is
# accounting.mjs's own domain — a skipped attempt keeps the ledger sound).
NEGATIVES = [
    ("missing-one", "delete_result", "missing result"),
    ("skip-one", "skip_result", None),
    ("duplicate-one", "duplicate_result", "duplicate result"),
    ("extra-attempt-one", "extra_attempt", "expected one attempt"),
    ("unknown-result-one", "unknown_identity", "unknown result"),
    ("unknown-status-one", "unknown_status", "unexecuted/interrupted status"),
]

KNOWN_STATUSES = ("passed", "failed", "timedOut", "skipped")


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


def normalize_file(spec_file, root_dir, test_dir):
    """Mirror reportCases' file normalisation (resolve against rootDir, then
    make relative to the baseline e2e dir, always with forward slashes)."""
    if os.path.isabs(spec_file):
        source = os.path.normpath(spec_file)
    elif root_dir:
        source = os.path.normpath(os.path.join(root_dir, spec_file))
    else:
        return spec_file.replace("\\", "/")
    if os.path.isabs(source):
        return os.path.relpath(source, test_dir).replace("\\", "/")
    return source.replace("\\", "/")


def report_rows(report, test_dir):
    """Mirror reportCases: one row per (spec, test), as (file, full title, test)."""
    def visit(suite, parents):
        titles = [*parents, suite["title"]] if suite.get("title") and not suite.get("file") else parents
        for spec in suite.get("specs", []):
            file = normalize_file(spec.get("file", ""), report.get("config", {}).get("rootDir"), test_dir)
            full_title = " › ".join([*titles, spec.get("title", "")])
            for test in spec.get("tests", []):
                yield file, full_title, test
        for child in suite.get("suites", []):
            yield from visit(child, titles)

    for suite in report.get("suites", []):
        yield from visit(suite, [])


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


def scan_report(path, expected_ids, test_dir):
    """Independent identity-ledger scan of one report original (fix-r1).

    Verifies every dimension of the #61 acceptance directly on the JSON,
    without filtering accounting.mjs's error messages: the identity set equals
    the registered inventory, every identity appears exactly once, every entry
    carries exactly one attempt (retry absent or 0), every attempt has a known
    result status, and the report attests config.forbidOnly=true and carries
    no Playwright-level errors. Behavioural failures are not ledger defects
    and stay out of this scan. Returns a list of violation strings; the label
    prefix is the report file name.
    """
    report = json.loads(path.read_text(encoding="utf-8"))
    label = path.name
    violations = []
    if report.get("config", {}).get("forbidOnly") is not True:
        violations.append(f"{label}: report must attest config.forbidOnly=true")
    if report.get("errors"):
        violations.append(f"{label}: report carries {len(report['errors'])} Playwright-level errors")
    seen = {}
    for file, title, test in report_rows(report, test_dir):
        identity = case_id(file, title)
        seen[identity] = seen.get(identity, 0) + 1
        results = test.get("results") or []
        if len(results) != 1:
            violations.append(f"{label}: expected one attempt, got {len(results)}: {identity}")
        for result in results:
            retry = result.get("retry")
            if retry is not None and retry != 0:
                violations.append(f"{label}: expected one attempt, got retry {retry}: {identity}")
            if result.get("status") not in KNOWN_STATUSES:
                violations.append(f"{label}: unexecuted/interrupted status {result.get('status')}: {identity}")
    for identity, count in sorted(seen.items()):
        if identity not in expected_ids:
            violations.append(f"{label}: unknown result {identity}")
        if count > 1:
            violations.append(f"{label}: duplicate result {identity} ({count} entries)")
    for identity in sorted(expected_ids):
        if identity not in seen:
            violations.append(f"{label}: missing result {identity}")
    return violations


def inventory_identities(pristine):
    """The registered identity set, from the pristine run's own inventory
    listing (run.mjs --reconcile runs `test --list --forbid-only` first, and
    result.registration has already been checked against the exp007 frozen
    inventory)."""
    output = Path(pristine["output"])
    inventory = json.loads((output / "inventory.raw.json").read_text(encoding="utf-8"))
    test_dir = os.path.join(pristine["result"]["baseline"]["ui"], "e2e")
    return {case_id(file, title) for file, title, _ in report_rows(inventory, test_dir)}


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

    # Positive control: the untouched originals must reconcile — the exit code
    # is recorded, not asserted: the real reports are expected to stay red on
    # behavioural gaps (mock coverage, probe surfaces, D orchestration); the
    # acceptance under test is the identity ledger, checked below.
    pristine = run_reconcile("pristine", old_reports, new_reports, out_dir)
    if pristine["invalid"]:
        record("pristine", "INVALID", pristine["invalid"])
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1
    record("pristine", "PASS",
           f"reconcile exit {pristine['exit']} (honest behavioural red preserved); "
           f"{len(pristine['errors'])} ledger errors")

    # The inventory the reports are checked against must itself be sound: the
    # pristine run expands the baseline with --list --forbid-only and run.mjs
    # compares it to the exp007 frozen inventory before reconciling.
    registration = pristine["result"].get("registration", {})
    if registration.get("errors") or registration.get("actual") != registration.get("expected"):
        record("pristine-inventory", "FAIL",
               f"expanded registration broken: {json.dumps(registration, ensure_ascii=False)[:300]}")
    else:
        record("pristine-inventory", "PASS",
               f"expanded registration {registration.get('actual')} == exp007 "
               f"{registration.get('expected')}, 0 registration errors")

    # Identity ledger, as a whole, with zero defects (fix-r1): every original
    # is independently re-scanned — identity set, occurrence count, unique
    # attempt, known status, forbidOnly, no Playwright-level errors. This is
    # the "72 appear, no duplicates, no gaps, one attempt each" acceptance; a
    # corrupted original (extra attempt, unknown identity, unknown status, ...)
    # fails here regardless of which message accounting.mjs would emit.
    try:
        expected_ids = inventory_identities(pristine)
        test_dir = os.path.join(pristine["result"]["baseline"]["ui"], "e2e")
    except (KeyError, OSError, ValueError) as error:
        record("pristine-identities", "INVALID", f"cannot rebuild the inventory identity set: {error!r}")
        (out_dir / "summary.json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
        return 1
    violations = []
    for report in [*old_reports, *new_reports]:
        violations += scan_report(report, expected_ids, test_dir)
    if violations:
        record("pristine-identities", "FAIL",
               f"independent identity-ledger scan rejected the originals: {violations[:3]} "
               f"({len(violations)} violations in total)")
    else:
        record("pristine-identities", "PASS",
               f"independent scan: all {len(expected_ids)} identities appear exactly once with "
               f"exactly one known-status attempt in each of the {len(old_reports) + len(new_reports)} "
               f"reports; 0 violations")

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

    for name, kind, scan_prefix in NEGATIVES:
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
        # The pristine scan must independently flag the corrupted copy (except
        # for skip policy): the positive control is exercised by every negative.
        scan_hits = scan_report(copy, expected_ids, test_dir)
        if scan_prefix and not any(scan_prefix in hit for hit in scan_hits):
            record(name, "SURVIVED",
                   f"the identity-ledger scan did not flag the corrupted copy ({scan_prefix!r} missing)")
            continue
        if not scan_prefix and scan_hits:
            record(name, "INVALID", f"the identity-ledger scan flagged a skip corruption it must not: {scan_hits[:2]}")
            continue
        expected = f"report 1: {marker}"
        if any(expected in error for error in pristine["errors"]):
            record(name, "INVALID", f"marker {expected!r} already appears in the pristine verdict")
            continue
        verdict = run_reconcile(name, [copy, *old_reports[1:]], new_reports, out_dir)
        if verdict["invalid"]:
            record(name, "INVALID", verdict["invalid"])
            continue
        if verdict["exit"] == 0:
            record(name, "SURVIVED", f"ledger stayed green despite {kind}")
        elif not any(expected in error for error in verdict["old_errors"]):
            record(name, "SURVIVED", f"exit {verdict['exit']} without the identity-scoped marker {expected!r}")
        else:
            record(name, "KILLED",
                   f"ledger failed with {expected}; scan flagged {len(scan_hits)} violation(s) on its own")

    (out_dir / "summary.json").write_text(json.dumps(rows, indent=2, ensure_ascii=False), encoding="utf-8")
    negatives = [row for row in rows
                 if row["name"] not in ("pristine", "pristine-inventory", "pristine-identities", "target")]
    killed = sum(row["status"] == "KILLED" for row in negatives)
    green = all(row["status"] in ("PASS", "KILLED") for row in rows)
    print(f"{killed}/{len(negatives)} report negatives KILLED; "
          f"positive controls {'hold' if green else 'BROKEN'}; see {out_dir / 'summary.json'}", flush=True)
    return 0 if green and killed == len(negatives) else 1


if __name__ == "__main__":
    raise SystemExit(main())
