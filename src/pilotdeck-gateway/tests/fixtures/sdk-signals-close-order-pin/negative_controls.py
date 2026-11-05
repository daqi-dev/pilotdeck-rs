#!/usr/bin/env python3
"""#1382 close-order catalog/ledger pin controls: mutate the sdk-signals
catalog itself and require its --check-anchors preflight to fail at the
dedicated #1382 message, then restore.

The #1382 check in sdk-signals/negative_controls.py pins the per-ID
unordered-close mutation rows to the registered close-order ledger IDs in
both directions; raising EXPECTED_TOTAL instead of adding the per-ID
mutation is exactly the drift it must catch. Each control below breaks one
half of that pin:

  - unordered-close-row-removed: one per-ID row is deleted (fewer mutation
    than the ledger registers);
  - unordered-close-id-renamed: a row is renamed so every legacy #1145
    check (denominator, uniqueness, anchors, bindings) still passes and only
    the #1382 pin can fail — the single-edit equivalent of the historical
    "raise the ledger, the whitelist and EXPECTED_TOTAL together" hole;
  - unordered-close-boundary-dropped: the boundary registration loses one
    entry and the matching mutation row disappears with it, so the name set
    and the registered-ID pin no longer agree (the content-table
    consistency check is exercised by the two retargeting controls above);
  - unordered-close-content-retargeted: a row keeps its per-ID name but its
    anchor and mutant value are pointed at another registered ID (the
    assertion half is covered by the next control), so the name-set
    comparison still passes while the named ID has no independent control and
    the other ID's mutation runs twice;
  - unordered-close-assertion-retargeted: only the bound assertion names
    another registered ID, leaving the anchor and mutant value intact, so the
    assertion half of the content pin cannot be left untested;
  - unordered-close-test-retargeted: only the bound test name is swapped for
    another test under an allowed prefix, so the test half of the content pin
    cannot be left untested;
  - unordered-close-boundary-content-retargeted: a whole-scope boundary row
    keeps its name but takes a sibling's assertion, so the boundary content
    pin must fail even though every name check still passes;
  - unordered-close-boundary-key-renamed: the expected-content table renames
    one key, so the name tuple and the table disagree and the consistency
    check between them must fire before any per-row content comparison.

The "ledger gains one ID" direction is the same set difference as
unordered-close-row-removed (a registered ID without a mutation); the
ledger spec is frozen read-only, so no control mutates it even transiently.

Mutants are never cumulative, every selected control runs (no abort on the
first failure), a stale anchor is INVALID (never SURVIVED), a mutant that
does not parse as Python is INVALID, and any non-KILLED control exits
nonzero. Because mutating_sources proves the tree equals HEAD, this must run
after the #1382 patch is committed; a pre-commit working tree is refused on
purpose.

Usage:
  CARGO_TARGET_DIR=<dir> python3 -I negative_controls.py <new-output-dir> [name ...]
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
)

root = Path(__file__).resolve().parents[5]
catalog_relative = "src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py"
catalog = root / catalog_relative
# The catalog preflight reads two frozen ledgers from the working tree: the
# #1382 close-order pin reads the first, and the #1145 anchor count for the
# rejection row reads the second. mutating_sources' workspace guard only
# covers src/.cargo/toolchain, so name both explicitly: an uncommitted edit
# must not move the comparison baseline behind our back (the #1145 catalog
# runner already guards both).
ledger_relative = "specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md"
ledger = root / ledger_relative
d1_ledger_relative = "specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md"
d1_ledger = root / d1_ledger_relative

# (name, path, before, after, expected fragment of the dedicated failure)
mutations = [
    (
        "unordered-close-row-removed",
        catalog_relative,
        "    ('unordered-close-resume--second-sigterm', replay, '\\n    \"resume--second-sigterm\",\\n', '\\n    \"resume--second-sigterm-unregistered\",\\n', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 unordered close swap must pass: resume--second-sigterm'),\n",
        "",
        "registered IDs without a mutation: ['resume--second-sigterm']",
    ),
    (
        "unordered-close-id-renamed",
        catalog_relative,
        "    ('unordered-close-run--query-resume-hold-sigint', replay,",
        "    ('unordered-close-run--query-resume-hold-sigint-renamed', replay,",
        "registered IDs without a mutation: ['run--query-resume-hold-sigint'];"
        " mutations without a registered ID:"
        " ['run--query-resume-hold-sigint-renamed']",
    ),
    (
        "unordered-close-boundary-dropped",
        catalog_relative,
        "    \"unordered-close-overbroad\",\n",
        "",
        "mutations without a registered ID: ['overbroad']",
    ),
    (
        "unordered-close-content-retargeted",
        catalog_relative,
        "'\\n    \"run--query-resume-hold-sigint\",\\n', '\\n    \"run--query-resume-hold-sigint-unregistered\",\\n'",
        "'\\n    \"run--second-sigint\",\\n', '\\n    \"run--second-sigint-unregistered\",\\n'",
        "wrong content: run--query-resume-hold-sigint",
    ),
    (
        "unordered-close-assertion-retargeted",
        catalog_relative,
        "#1252 unordered close swap must pass: run--query-resume-hold-sigint'),",
        "#1252 unordered close swap must pass: run--second-sigint'),",
        "wrong content: run--query-resume-hold-sigint",
    ),
    (
        "unordered-close-test-retargeted",
        catalog_relative,
        "signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', "
        "'#1252 unordered close swap must pass: run--query-resume-hold-sigint'",
        "signal_replay::comparison_tests::unordered_close_scope_equals_registration', "
        "'#1252 unordered close swap must pass: run--query-resume-hold-sigint'",
        "wrong content: run--query-resume-hold-sigint",
    ),
    (
        "unordered-close-boundary-content-retargeted",
        catalog_relative,
        "#1252 stable close swap must fail:'",
        "#1252 non-array closes must fail:'",
        "wrong content: unordered-close-overbroad",
    ),
    (
        "unordered-close-boundary-key-renamed",
        catalog_relative,
        '    "unordered-close-overbroad": (\n',
        '    "unordered-close-overbroad-renamed": (\n',
        "boundary name list and expected-content table disagree",
    ),
]

if len(sys.argv) < 2:
    sys.exit(
        "usage: negative_controls.py NEW_OUTPUT_DIR [name ...] "
        "(the coordinator supplies CARGO_TARGET_DIR)"
    )
selected = sys.argv[2:]
if selected:
    known = {row[0] for row in mutations}
    unknown = [name for name in selected if name not in known]
    if unknown or len(selected) != len(set(selected)):
        sys.exit(f"#1382 unknown or duplicate selection: {selected}")
    mutations = [row for row in mutations if row[0] in selected]

if not os.environ.get("CARGO_TARGET_DIR"):
    sys.exit("#1382 CARGO_TARGET_DIR must be supplied by the caller")

out = new_output_directory(sys.argv[1])


def preflight_passed(result):
    # The count is printed by the catalog itself; do not pin a second copy.
    return result.returncode == 0 and "product anchors: PASS" in result.stdout


def run_check(label):
    result = run_owned(
        [sys.executable, "-I", str(catalog), "--check-anchors"],
        cwd=root,
        timeout=120,
    )
    exclusive_write(out / f"{label}.log", result.stdout + result.stderr)
    return result


results = []
with mutating_sources(root, [catalog, ledger, d1_ledger]) as snapshots:
    positive = run_check("positive-before")
    if not preflight_passed(positive):
        raise SystemExit(
            "#1382 baseline --check-anchors must pass before mutating: "
            f"rc={positive.returncode}"
        )
    for name, relative, before, after, criterion in mutations:
        path = root / relative
        original = snapshots[path].decode("utf-8")
        status, reason, compiled = "INVALID", "not run", False
        try:
            count = original.count(before)
            if count != 1:
                # A stale anchor is INVALID, never SURVIVED; keep running the
                # remaining controls and fail the whole run at the summary.
                reason = f"expected one anchor, found {count}"
            else:
                changed = original.replace(before, after, 1)
                try:
                    # The Python-side equivalent of the compile-first rule:
                    # a mutant that does not parse can never be a kill.
                    compile(changed, str(path), "exec")
                except SyntaxError as error:
                    reason = f"mutant does not parse: {error}"
                else:
                    compiled = True
                    exclusive_write(
                        out / f"{name}.patch",
                        "".join(
                            difflib.unified_diff(
                                original.splitlines(True),
                                changed.splitlines(True),
                                fromfile=relative,
                                tofile=relative,
                            )
                        ),
                    )
                    path.write_text(changed, encoding="utf-8")
                    result = run_check(name + "-negative")
                    log = result.stdout + result.stderr
                    markers = (
                        "#1382 unordered-close controls must equal the"
                        " registered close-order IDs",
                        "#1382 unordered-close rows must bind their own ID",
                        "#1382 whole-scope boundary rows must keep their own content",
                        "#1382 boundary name list and expected-content table disagree",
                    )
                    if (
                        result.returncode != 0
                        and any(marker in log for marker in markers)
                        and criterion in log
                    ):
                        status, reason = "KILLED", "preflight failed at its #1382 pin"
                    elif result.returncode == 0:
                        status, reason = "SURVIVED", "preflight still passed"
                    else:
                        status, reason = (
                            "INVALID",
                            "preflight failed without the dedicated #1382 message",
                        )
        except Exception as error:
            # One control's crash must not stop the remaining controls; the
            # summary below still exits nonzero on this INVALID row.
            status = "INVALID"
            reason = f"harness: {type(error).__name__}: {error}"
        finally:
            path.write_bytes(snapshots[path])
            if path.read_bytes() != snapshots[path]:
                raise RuntimeError(f"#1382 restore failed: {relative}")
        try:
            restored = run_check(name + "-restored")
            if not preflight_passed(restored):
                status = "INVALID"
                reason += "; restored preflight did not pass"
        except Exception as error:
            status = "INVALID"
            reason += f"; restored: {type(error).__name__}: {error}"
        results.append(
            {
                "mutation": name,
                "status": status,
                "reason": reason,
                "compiled": compiled,
            }
        )
        print(f"{name}: {status}: {reason}", flush=True)

counts = {
    state: sum(row["status"] == state for row in results)
    for state in ("KILLED", "SURVIVED", "INVALID")
}
exclusive_write(out / "summary.json", json.dumps(results, indent=2) + "\n")
print(json.dumps({"total": len(results), "counts": counts}), flush=True)
not_killed = [row["mutation"] for row in results if row["status"] != "KILLED"]
if not_killed:
    sys.exit(f"#1382 non-KILLED controls: {not_killed}")
