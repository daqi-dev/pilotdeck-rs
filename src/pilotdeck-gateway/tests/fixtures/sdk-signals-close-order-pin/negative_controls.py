#!/usr/bin/env python3
"""#1382 close-order catalog/ledger pin controls: mutate the sdk-signals
catalog itself and require its --check-anchors preflight to fail at the
dedicated #1382 message, then restore.

The #1382 check in sdk-signals/negative_controls.py pins the per-ID
unordered-close mutation rows to the registered close-order ledger IDs in
both directions; raising EXPECTED_TOTAL instead of adding the per-ID
mutation is exactly the drift it must catch. Each control below breaks one
half of that pin:

  - unordered-close-row-removed: one per-ID row is deleted (fewer mutations
    than the ledger registers);
  - unordered-close-id-renamed: a row is renamed so every legacy #1145
    check (denominator, uniqueness, anchors, bindings) still passes and only
    the #1382 pin can fail — the single-edit equivalent of the historical
    "raise the ledger, the whitelist and EXPECTED_TOTAL together" hole;
  - unordered-close-boundary-dropped: the boundary registration loses one
    entry while its mutation row stays, so the name is no longer excluded
    from the per-ID set and the extra branch of the ID-set pin fires;
  - unordered-close-boundary-row-removed: the whole mutation row is deleted
    while its registration stays, which only the stale-boundary branch can
    report — without that branch the loss would surface as a stale
    denominator instead, so this is the one control that keeps the branch
    alive across a refactor of the comparisons below;
  - unordered-close-content-retargeted: a row keeps its per-ID name but its
    anchor and mutant value are pointed at another registered ID (the
    assertion half is covered by the next control), so the name-set
    comparison still passes while the named ID has no independent control and
    the other ID's mutation runs twice;
  - unordered-close-content-old-retargeted / -new-retargeted: one field per
    control, the row's anchor or its mutant value alone moves to another
    registered ID. The combined control above changes both at once, so it
    cannot show the two halves of the per-ID content comparison are checked
    independently; a comparison that dropped either half stayed green;
  - unordered-close-path-retargeted: only the row's path moves off the shared
    replay harness, so the anchor, the mutant value, the test and the
    assertion all still match and only the path half of the comparison can
    refuse it;
  - unordered-close-assertion-retargeted: only the bound assertion names
    another registered ID, leaving the anchor and mutant value intact, so the
    assertion half of the content pin cannot be left untested;
  - unordered-close-test-retargeted: only the bound test name is swapped for
    another test under an allowed prefix, so the test half of the content pin
    cannot be left untested;
  - unordered-close-boundary-content-retargeted: a whole-scope boundary row
    keeps its name but takes a sibling's assertion, so the boundary content
    pin must fail even though every name check still passes;
  - unordered-close-boundary-old-retargeted / -new-retargeted /
    -test-retargeted: one boundary row takes a sibling's anchor, mutant value
    or test, one field per control, so a boundary comparison that
    degenerated into "assertion only" (or into any single dropped field) is
    caught at exactly the position that stopped being checked;
  - unordered-close-boundary-path-retargeted: one boundary row moves off the
    shared replay harness — the fifth field of the same tuple. The per-ID
    path control above cannot stand in for it (the per-ID binding check
    refuses that one first), so without this row the boundary comparison
    could drop `path` and stay green. If it ever does, this row is the only
    one that moves: the anchor no longer resolves in the retargeted file, so
    the #1145 stale-anchor check turns it INVALID (a nonzero exit), not
    KILLED — loud either way;
  - unordered-close-boundary-key-renamed: the expected-content table renames
    one key, so the name tuple and the table disagree and the consistency
    check between them must fire before any per-row content comparison.

The "ledger gains one ID" direction is the same set difference as
unordered-close-row-removed (a registered ID without a mutation); the
ledger spec is frozen read-only, so no control mutates it even transiently.

Mutants are never cumulative, every selected control runs (no abort on the
first failure), a stale anchor is INVALID (never SURVIVED), a mutant that
does not parse as Python is INVALID, and any non-KILLED control exits
nonzero. The table itself is registered by EXPECTED_TOTAL (#1506): a
zero or negative denominator, and an empty, short, or duplicated table, are
refused before any control runs, and a run that records no result row exits
nonzero before it publishes any evidence. A run that cannot reach or keep
the source lock — the #1145 full runner holds it for its whole case list —
gives up after a short probe rather than the 900 s default, and an abort of
any kind (busy lock, failed restore, Ctrl-C) still writes the summary, with
every recorded KILLED row revoked and every unrun control marked INVALID
(#1508). A run that could not verify the tree still writes the summary;
only an interrupt during that accounting itself leaves none, and then rc 2
says so. Exit codes follow `cli-parity-lock-bounded` (#1064 fix-r2 P1-1) on
the verdict path: 2 means the device never produced a verdict — an INVALID
row (stale anchor, unparsable mutant, unproven restore), an abort of any
kind, a summary that could not be published, or a final stdout line that
could not be written (#1800: the reader's normal `... | head` exit is a pipe
error and is swallowed; every other write failure names itself on stderr
first) — and 1 is reserved for "the device ran and a control survived". The
two must not share a code, or a coordinator reads "the device never ran" as
"a control survived". This is
this device's contract, not every sibling's: pre-run refusals (usage, an
unknown or repeated selection, a missing `CARGO_TARGET_DIR`, a bad mutation
table) still exit 1, as do the shared `negative_suite` runner's lock and
selection refusals. Naming no control is not a refusal: it selects the whole
table and exits by the verdict path like any other run. The summary is
published temp-file-
then-rename, so an interrupt during that write leaves no summary at all
rather than a half-written one. Because mutating_sources proves the tree
equals HEAD, this must run after the #1382 patch is committed; a pre-commit
working tree is refused on purpose.

Usage:
  CARGO_TARGET_DIR=<dir> python3 -I negative_controls.py <new-output-dir> [name ...]
"""
from collections import Counter
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (
    _head_bytes,
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
)
from negative_suite import invalidate_completed

root = Path(__file__).resolve().parents[5]
catalog_relative = "src/pilotdeck-gateway/tests/fixtures/sdk-signals/negative_controls.py"
catalog = root / catalog_relative
# The catalog preflight reads two frozen ledgers from the working tree: the
# #1382 close-order pin reads the first, and the #1145 anchor count for the
# rejection row reads the second. mutating_sources' workspace guard only
# covers src/.cargo/toolchain, so both must be proved against HEAD here: an
# uncommitted edit must not move the comparison baseline behind our back.
#
# #1885: prove, do not write back. Every one of the 16 controls rewrites
# `catalog` and nothing else, so the ledgers used to sit in the restore set
# for the entry guard alone -- and `mutating_sources` writes every guarded
# path back on exit with a plain `write_bytes`, no atomic publish. A SIGKILL
# inside that window truncated a frozen spec ledger, which the next run then
# read as a baseline mismatch (false FAIL, manual `git checkout` to repair).
# The restore set now carries only mutated files; the ledgers get an explicit
# byte-for-byte HEAD proof instead, exactly like `sdk-signals` does for the
# close-order ledger.
ledger_relative = "specs/gateway-parity/diffs/D1-SDK-WS-CLOSE-ORDER-001.md"
ledger = root / ledger_relative
d1_ledger_relative = "specs/gateway-parity/diffs/D1-SDK-SIGNAL-UNHANDLED-REJECTION-001.md"
d1_ledger = root / d1_ledger_relative
RESTORE_SET = (catalog,)

# #1506: this table is the only register of these controls — anchor_selfcheck
# pins the script's presence and anchor counts, not its rows — so deleting a
# row used to shrink coverage with every remaining check green. Raising this
# number is the deliberate friction: adding or removing a row must re-register
# the total in the same edit.
EXPECTED_TOTAL = 16
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
    # #1507 fix-r2: `content-retargeted` moves the anchor *and* the mutant
    # value together, so it cannot tell "old is compared" from "new is
    # compared" — degrading `old != ...` or `new != ...` in the per-ID
    # binding comparison left every other control green (the stale-anchor
    # loop only counts `old`, and a sibling's anchor also occurs exactly
    # once). One field per control, same shape as the boundary retargeting
    # rows below.
    (
        "unordered-close-content-old-retargeted",
        catalog_relative,
        "'\\n    \"run--query-resume-hold-sigint\",\\n', '\\n    \"run--query-resume-hold-sigint-unregistered\",\\n'",
        "'\\n    \"run--second-sigint\",\\n', '\\n    \"run--query-resume-hold-sigint-unregistered\",\\n'",
        "wrong content: run--query-resume-hold-sigint",
    ),
    (
        "unordered-close-content-new-retargeted",
        catalog_relative,
        "'\\n    \"run--query-resume-hold-sigint\",\\n', '\\n    \"run--query-resume-hold-sigint-unregistered\",\\n'",
        "'\\n    \"run--query-resume-hold-sigint\",\\n', '\\n    \"run--second-sigint-unregistered\",\\n'",
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
    # #1507: five single-position controls. The rows above prove the boundary
    # pin and the per-ID content pin each have *an* assertion control; these
    # prove every position inside them is compared, and that the
    # stale-registration branch — the only one that sees a wholly missing
    # boundary row — is still reachable.
    (
        "unordered-close-boundary-row-removed",
        catalog_relative,
        r"    ('unordered-close-non-array', replay, '    let Some(actual) = actual.as_array() else {\n        return false;\n    };', '    let Some(actual) = actual.as_array() else {\n        return true;\n    };', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics', '#1252 non-array closes must fail:'),"
        "\n",
        "",
        "stale unordered-close boundary controls: unordered-close-non-array",
    ),
    (
        "unordered-close-path-retargeted",
        catalog_relative,
        "    ('unordered-close-run--query-resume-hold-sigint', replay,",
        "    ('unordered-close-run--query-resume-hold-sigint', signals,",
        "wrong content: run--query-resume-hold-sigint",
    ),
    (
        "unordered-close-boundary-old-retargeted",
        catalog_relative,
        r"    ('unordered-close-unregistered-id', replay, 'const UNORDERED_CLOSE_CASES: &[&str] = &[\n', ",
        r"    ('unordered-close-unregistered-id', replay, '    let Some(actual) = actual.as_array() else {\n        return false;\n    };', ",
        "wrong content: unordered-close-unregistered-id",
    ),
    (
        "unordered-close-boundary-new-retargeted",
        catalog_relative,
        r"    ('unordered-close-overbroad', replay, '    if !UNORDERED_CLOSE_CASES.contains(&id) {', '    if false && !UNORDERED_CLOSE_CASES.contains(&id) {',",
        r"    ('unordered-close-overbroad', replay, '    if !UNORDERED_CLOSE_CASES.contains(&id) {', '    let Some(actual) = actual.as_array() else {\n        return true;\n    };',",
        "wrong content: unordered-close-overbroad",
    ),
    (
        "unordered-close-boundary-test-retargeted",
        catalog_relative,
        r"    ('unordered-close-overbroad', replay, '    if !UNORDERED_CLOSE_CASES.contains(&id) {', '    if false && !UNORDERED_CLOSE_CASES.contains(&id) {', 'signal_replay::comparison_tests::all_multiclose_cells_follow_registered_order_semantics',",
        r"    ('unordered-close-overbroad', replay, '    if !UNORDERED_CLOSE_CASES.contains(&id) {', '    if false && !UNORDERED_CLOSE_CASES.contains(&id) {', 'signal_replay::comparison_tests::unordered_close_scope_equals_registration',",
        "wrong content: unordered-close-overbroad",
    ),
    # #1507 fix-r1: the boundary row's *path* position is a separate field of
    # the same five-tuple comparison, and the per-ID `unordered-close-path-
    # retargeted` control above cannot cover it (that one is refused by the
    # per-ID binding check, never by the boundary comparison). Without this
    # row, degrading `(path, old, new, test, assertion) != expected` to
    # `expected[1:]` left every other control green.
    (
        "unordered-close-boundary-path-retargeted",
        catalog_relative,
        "    ('unordered-close-non-array', replay,",
        "    ('unordered-close-non-array', signals,",
        "wrong content: unordered-close-non-array",
    ),
]


def check_mutation_table(rows):
    """#1506: refuse a control table whose row count or names do not match.

    Runs on the whole table before any command-line selection, so `[name ...]`
    can only narrow a complete registry. An empty table is just the extreme of
    "row count differs", so it reports the same denominator message.
    """
    # #1506 fix-r2: the denominator is only a guard if it is positive. With
    # `EXPECTED_TOTAL = 0` an emptied table satisfies the count check below,
    # the loop runs no control, and the run ends green with `0/0` — the exact
    # shape this guard exists to refuse.
    if EXPECTED_TOTAL < 1:
        raise RuntimeError(
            "#1506 registered denominator must be positive:"
            f" EXPECTED_TOTAL={EXPECTED_TOTAL}"
        )
    if len(rows) != EXPECTED_TOTAL:
        raise RuntimeError(
            f"#1506 fixed product mutation denominator: expected {EXPECTED_TOTAL},"
            f" found {len(rows)}"
        )
    names = [row[0] for row in rows]
    duplicates = sorted(name for name, count in Counter(names).items() if count > 1)
    if duplicates:
        raise RuntimeError(
            "#1506 unique product mutation identities: duplicates "
            + ", ".join(duplicates)
        )


check_mutation_table(mutations)


def check_restore_set():
    """#1885: only files a control rewrites may enter the restore set.

    Static, whole-table, and before `[name ...]` narrows anything: a restore
    path that no mutation targets is pure truncation exposure (it is written
    back on exit, never changed by the run). The `src/` workspace guard
    already proves every non-`specs/` leftover, so nothing is lost by
    dropping such a path.
    """
    targets = {root / row[1] for row in mutations}
    stray = sorted(str(path) for path in RESTORE_SET if path not in targets)
    if stray:
        raise RuntimeError(
            "#1885 restore set must only carry files a control rewrites: "
            + ", ".join(stray)
        )


check_restore_set()


def assert_frozen_ledgers_match_head():
    """#1885: the read-only replacement for naming the ledgers in the set.

    Same proof the restore set used to give on entry, without ever putting the
    frozen bytes in the write-back (truncation) window.
    """
    for path in (ledger, d1_ledger):
        relative = path.relative_to(root).as_posix()
        if _head_bytes(root, relative, "HEAD") != path.read_bytes():
            raise RuntimeError(
                "#1885 read-only frozen ledger must equal HEAD: " + relative
            )

# #1506 fix-r3: the registered name list is frozen here, before `[name ...]`
# narrows `mutations` below. Read after that rebinding it is just `selected`
# again, so the full-registry evidence proved nothing on a subset run.
REGISTERED = [row[0] for row in mutations]

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


# #1508: the sibling #1145 runner wraps its whole case list — each case may
# start a compile — in one `.negative-source.lock`, so the 900 s default here
# was spent entirely waiting for it and then raised a bare TimeoutError with
# no evidence at all. Probe with a short bound and name the holder instead.
LOCK_TIMEOUT_SECONDS = 60

results = []


def _stderr(message):
    # #1508 fix-r3: a fail-closed diagnostic must never be the thing that
    # escapes. `2>&1 | head` or `2>&-` makes stderr unwritable too, and an
    # exception raised from a handler that was about to exit 2 would hand the
    # reader whatever code the interpreter picked — the exact misrouting this
    # contract exists to prevent.
    try:
        print(message, file=sys.stderr, flush=True)
    except BaseException:
        pass


def record(name, status, reason, compiled):
    results.append(
        {
            "mutation": name,
            "status": status,
            "reason": reason,
            "compiled": compiled,
        }
    )
    print(f"{name}: {status}: {reason}", flush=True)


# #1508: an abort anywhere — a busy source lock, a restore that cannot be
# verified, Ctrl-C — used to escape between the loop and the summary, leaving
# the evidence as scrolling stdout only; a restore failure escaped *after* the
# green rows had already been printed. Every exit path now lands in the same
# accounting: no recorded KILLED row survives a run that could not verify the
# tree, every control that never ran is recorded INVALID, and the summary is
# written before the run exits nonzero.
aborted = None
try:
    with mutating_sources(
        root, list(RESTORE_SET), lock_timeout=LOCK_TIMEOUT_SECONDS
    ) as snapshots:
        # #1885 r3: prove inside the tree lock, exactly where the restore set
        # used to prove the ledgers on entry. Proving before `mutating_sources`
        # left a TOCTOU window (the lock probe alone may wait
        # LOCK_TIMEOUT_SECONDS) during which an uncommitted edit would land in
        # the very bytes the preflight then compares against.
        assert_frozen_ledgers_match_head()
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
                    # A stale anchor is INVALID, never SURVIVED; keep running
                    # the remaining controls and fail the whole run at the
                    # summary.
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
                            "#1382 stale unordered-close boundary controls",
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
                            status, reason = (
                                "KILLED",
                                "preflight failed at its #1382 pin",
                            )
                        elif result.returncode == 0:
                            status, reason = "SURVIVED", "preflight still passed"
                        else:
                            status, reason = (
                                "INVALID",
                                "preflight failed without the dedicated #1382"
                                " message",
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
            record(name, status, reason, compiled)
        # #1885 r3: re-prove on the way out, still under the tree lock. While
        # the ledgers were in the restore set, `__exit__` rewrote them and the
        # restore verification re-read them, so a ledger edited by anyone else
        # during the run surfaced as a failed restore. Proving only on entry
        # would let a concurrent edit stand unnoticed for the whole run.
        assert_frozen_ledgers_match_head()
except BaseException as error:
    aborted = f"{type(error).__name__}: {error}"
    if isinstance(error, TimeoutError) and "source lock busy" in str(error):
        aborted += (
            "; the other holder is most likely the #1145 sdk-signals runner"
            " (it keeps .negative-source.lock for its whole case list) —"
            " rerun this pin after it finishes"
        )
    # A run that could not restore and verify the tree invalidates every row it
    # already recorded (#979 P0: a green row over an unverified tree is a false
    # pass), and every control that never ran is still accounted for.
    #
    # #1508 fix-r2: the accounting itself can be interrupted. `record()` prints
    # as it goes, so a consumer that closed stdout (`... | head`) turns the
    # tail into a BrokenPipeError, and a second SIGINT lands here too. Escaping
    # from an except handler skips the summary and exits on whatever code the
    # interpreter picked, which is exactly the misrouting this contract exists
    # to prevent — fail closed instead: no summary, rc 2.
    try:
        invalidate_completed(results, f"run aborted: {aborted}")
        for row in mutations[len(results):]:
            record(row[0], "INVALID", f"run aborted: {aborted}", False)
    except BaseException as nested:
        _stderr(f"#1382 abort accounting failed: {type(nested).__name__}: {nested}")
        os._exit(2)

# #1508 fix-r3: everything from here to the exit runs inside one guard. Two
# separate windows used to be bare — the counts/summary construction and the
# final stdout print — and a second SIGINT or a write error landing in either
# escaped as the interpreter's own code (130 for a signal, 1 for most write
# errors), which is neither of the two codes this contract defines. Every exit
# below is `os._exit`, so nothing outside the guard can change it.
#
# The exit codes themselves (#1064 fix-r2 P1-1 shape): a run that recorded an
# INVALID row, aborted, or could not publish its summary exits 2 — the device
# produced no verdict. 1 is reserved for "the device ran and a control
# survived" (SURVIVED). The two must not share a code, or a coordinator reads
# "the device never ran" as "a control survived".
try:
    # #1506 fix-r3: an empty run is a zero denominator, never a pass, and it
    # must not publish evidence that looks like a finished run. The table guard
    # above already refuses an empty registry, and naming no control means
    # "run them all" (see Usage), so this only fires if a future path records
    # no row at all.
    if not results:
        # #1508 fix-r3: a zero denominator is "no verdict", not "a control
        # survived" — it shares the INVALID/abort code, not SURVIVED's 1.
        _stderr("#1506 no control ran: an empty run is not a pass")
        os._exit(2)
    counts = {
        state: sum(row["status"] == state for row in results)
        for state in ("KILLED", "SURVIVED", "INVALID")
    }
    # #1506: the guard verified the whole registry, but `results` only holds the
    # rows the command line selected. Record both, so a later reader can tell
    # "16 registered, 3 selected" from "the table shrank to 3". `registered` is
    # the name list itself, not just its length: a table swapped for the same
    # number of different names keeps the count and the uniqueness check green,
    # and only the names show it.
    summary = {
        "registeredTotal": len(REGISTERED),
        "registered": REGISTERED,
        "selected": [row["mutation"] for row in results],
        "counts": counts,
        "results": results,
    }
    # #1508 fix-r1: publish atomically (sibling `cli-parity-lock-bounded` shape,
    # #166 r8). `exclusive_write` created summary.json empty and filled it in
    # place, so a second SIGINT/SIGKILL during the abort publish left an
    # existing but unparsable summary — the reader sees "device broken" instead
    # of the INVALID verdict. A temp file plus rename means an interrupt here
    # leaves no summary at all, and a missing summary is the fail-closed signal.
    summary_path = out / "summary.json"
    summary_tmp = out / "summary.json.tmp"
    try:
        summary_tmp.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        summary_tmp.replace(summary_path)
    except BaseException as error:
        # #1508 fix-r2: a second interrupt between the write and the rename, or
        # any other publish failure, must not escape with an arbitrary exit
        # code over a half-written file. Drop the temp file and fail closed: no
        # summary means the device never produced a verdict, and rc 2 says so.
        try:
            summary_tmp.unlink()
        except BaseException:
            pass
        _stderr(f"#1382 summary not published: {type(error).__name__}: {error}")
        os._exit(2)
    try:
        print(
            json.dumps(
                {
                    "total": len(results),
                    "registeredTotal": len(REGISTERED),
                    "selectedTotal": len(results),
                    "counts": counts,
                }
            ),
            flush=True,
        )
    except BrokenPipeError:
        # `... | head` closed stdout. The exit code routes the reader, not
        # stdout, so swallow the pipe error rather than let it decide the
        # status.
        try:
            sys.stdout.close()
        except BaseException:
            pass
    except OSError as error:
        # #1800 fix-r3 (comment follow-up 1): any other write failure —
        # ENOSPC on a full disk (/dev/full probe: OSError errno 28, not a
        # pipe error), EIO — is not "the reader left" but "the verdict line
        # never reached the reader". The tail's catch-all would still exit
        # 2, but silently: the `#1382 run did not complete` diagnostic
        # below this print is skipped too, and the reader gets a bare rc.
        # Name the failure on stderr first, then fail closed on the shared
        # "no verdict" code — the same refusal scripts/parity-attrib
        # reserves rc 9 for (#2031 item 6: a disk that cannot be written
        # must not read as a delivered verdict). BrokenPipeError is an
        # OSError subclass, so the order above keeps the pipe case's
        # close-stdout handling; only OSError is caught, so a
        # KeyboardInterrupt still reaches the catch-all below.
        _stderr(f"#1382 stdout summary write failed: {type(error).__name__}: {error}")
        os._exit(2)
    if aborted is not None:
        # #1508 fix-r1: an aborted run is rc 2, not 1 — `cli-parity-lock-
        # bounded` reserves 1 for "a control ran and was not killed" (a red run
        # of a working device) and 2 for "the device never produced a verdict"
        # (busy lock, unverified restore, cancellation). The INVALID summary is
        # already on disk, so the exit code only routes the reader; a
        # coordinator must not read "the device never ran" as "a control
        # survived".
        _stderr(f"#1382 run did not complete: {aborted}")
        os._exit(2)
    invalid = [row["mutation"] for row in results if row["status"] == "INVALID"]
    survived = [row["mutation"] for row in results if row["status"] == "SURVIVED"]
    if invalid:
        _stderr(f"#1382 INVALID controls: {invalid}")
        if survived:
            _stderr(f"#1382 SURVIVED controls: {survived}")
        os._exit(2)
    if survived:
        _stderr(f"#1382 SURVIVED controls: {survived}")
        os._exit(1)
    # Explicit success: falling off the end would let interpreter shutdown
    # decide the status (a failing final flush turns a green run into 120).
    os._exit(0)
except BaseException:
    # Nothing in the tail may raise; if it somehow does, it is still "the
    # device never produced a verdict", never a silent green.
    os._exit(2)
