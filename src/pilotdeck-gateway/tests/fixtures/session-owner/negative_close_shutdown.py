#!/usr/bin/env python3
"""Negative controls for the #1245 close-during-shutdown alignment.

The TS contract (`SessionRouter.ts:327-338, 421-454`, pinned by the real-router
probes ``_tmp/20261026-issue-1245/probe/close-during-shutdown.out`` and
``_tmp/20261026-issue-1245/fix-r1/probe/close-first-same-tick.out`` — in the
author tree under ``_tmp/20261026-issue-1245/``, in the delivered round copy
under the same directory's ``remote-run/artifacts/round/probe/``, Node
v22.22.0): a ``SessionOwner::close`` that finds no cached record awaits the
key's registered in-flight eviction — a close task, the idle sweep or a
shutdown drain — and transmits that eviction's own result; no entry means
fulfilled. A close that finds a record owns its eviction: TS ``close``
deletes the record synchronously at call time, so a same-tick ``shutdown()``
must not steal it — the close keeps its per-key result and ``closed``
reason, and the shutdown aggregates the failure through the pending drain.
The whole-shutdown aggregate stays ``shutdown()``'s own answer.

Every mutation below reverts one slice of that mechanism and must be killed
by the exact assertion named in its markers. Every case is classified
KILLED / SURVIVED / INVALID, all cases run to completion, and any non-KILLED
result exits non-zero.

The source tree must equal ``HEAD`` for the guard to run. In the delivery
sandbox the patch is uncommitted, so the coordinator runs this after committing:

  CARGO_TARGET_DIR=/path/to/target python3 \\
    src/pilotdeck-gateway/tests/fixtures/session-owner/negative_close_shutdown.py \\
    _tmp/<date>-issue-1245/negative-close-shutdown

Rows follow anchor_selfcheck's ``MUTATIONS`` dict layout
``name: (path, before, after, [tests], [markers])`` so the static preflight can
resolve every anchor. A row may bind several tests (one ``Case`` each) and list
several markers; ``negative_suite.classify`` treats the marker list as
alternatives — the bound test may fail at any one of them — so a row is one
mutation, not one marker per case.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
assert os.environ.get("CARGO_TARGET_DIR"), "caller must set CARGO_TARGET_DIR"
owner = root / "src/pilotdeck-gateway/src/runtime/session/owner.rs"

DURING_SHUTDOWN = (
    "runtime::session::owner::tests::"
    "close_during_shutdown_awaits_the_keys_own_eviction_matches_ts"
)
SECOND_CLOSE = (
    "runtime::session::owner::tests::"
    "second_close_awaits_the_firsts_own_eviction_matches_ts"
)
IDLE_EVICT = (
    "runtime::session::owner::tests::"
    "close_during_idle_eviction_awaits_the_idle_eviction_matches_ts"
)
CLOSE_THEN_SHUTDOWN = (
    "runtime::session::owner::tests::"
    "close_then_shutdown_same_tick_matches_ts"
)
SHUTDOWN_THEN_CLOSE = (
    "runtime::session::owner::tests::"
    "shutdown_then_close_same_tick_fulfills_matches_ts"
)
SKIP_ONLY_CLOSING = (
    "runtime::session::owner::tests::"
    "shutdown_skips_only_closing_keys_and_drains_the_rest_matches_ts"
)
DRAIN_PANIC = (
    "runtime::session::owner::tests::"
    "shutdown_drain_panic_settles_the_keys_closing_entry"
)
CLOSE_SETTLES = (
    "runtime::session::owner::tests::"
    "close_settles_its_closing_entry_before_releasing_the_ticket"
)
IDLE_SETTLES = (
    "runtime::session::owner::tests::"
    "idle_sweep_settles_its_closing_entry_before_releasing_the_ticket"
)


# Every marker names the exact assertion message that must turn red.
MUTATIONS = {
    # The #1245 core: revert the close window to the pre-alignment branch —
    # any close while `state.shutdown` is set answers the whole-shutdown
    # aggregate instead of the key's own eviction. Every mid-flight window
    # of the A2 test diverges (the succeeding eviction rejects, the unknown
    # key rejects, the failing one answers the aggregate text), and a
    # same-tick close after shutdown() awaits the aggregate instead of
    # fulfilling before the drain registers.
    "close-during-shutdown-aggregate": (
        owner,
        """        if let Some(closing) = unsettled_closing(&mut state, key) {
            return Box::pin(wait_result(closing));
        }
        if state.shutdown.is_some() {
            return Box::pin(async { Ok(()) });
        }""",
        """        if let Some(shutdown) = state.shutdown.clone() {
            return Box::pin(wait_result(shutdown));
        }""",
        [DURING_SHUTDOWN, SHUTDOWN_THEN_CLOSE],
        [
            "#1245 a succeeding mid-flight shutdown eviction must fulfill close",
            "#1245 an unregistered key must fulfill close during shutdown",
            "#1245 close during shutdown transmits the key's own dispose aggregate",
            "#1245 a same-tick close after shutdown must fulfill before the drain registers",
            "#1245 a same-tick close after shutdown must fulfill on a succeeding drain too",
        ],
    ),
    # The else branch stops awaiting: a mid-flight eviction no longer holds
    # the close, which settles immediately — the parked check fails before
    # the per-key rejection is even reached.
    "close-else-awaits-nothing": (
        owner,
        """        if let Some(closing) = unsettled_closing(&mut state, key) {
            return Box::pin(wait_result(closing));
        }""",
        """        if unsettled_closing(&mut state, key).is_some() {
            return Box::pin(async { Ok(()) });
        }""",
        [DURING_SHUTDOWN],
        [
            "#1245 close of a mid-flight shutdown eviction must await it, "
            "not answer early",
        ],
    ),
    # The close task no longer registers its completion under the key, so a
    # same-key close#2 arriving mid-flight takes the ticket path and drains to
    # Ok instead of transmitting close#1's eviction result — and a same-tick
    # shutdown() no longer sees the registered eviction, steals the record
    # and the close drains to Ok with the drain's `shutdown` reason.
    "close-call-registration-removed": (
        owner,
        """        track(&mut state, project, completion.clone());
        state.closings.insert(key.clone(), completion.clone());""",
        """        track(&mut state, project, completion.clone());""",
        [SECOND_CLOSE, CLOSE_THEN_SHUTDOWN],
        [
            "#1245 each in-flight close of the key must reject with the eviction's "
            "own result",
            "#1245 a close that starts before a same-tick shutdown must reject "
            "with its own dispose failure",
        ],
    ),
    # The idle sweep no longer registers its eviction under the key, so a
    # close arriving mid-flight drains behind the ticket and answers Ok.
    "idle-sweep-registration-removed": (
        owner,
        """            state.closings.insert(key.clone(), completion);
            let owner = self.clone();""",
        """            let owner = self.clone();""",
        [IDLE_EVICT],
        [
            "#1245 a failing mid-flight idle eviction must reject close",
        ],
    ),
    # The shutdown drains no longer register per key, so a close arriving
    # mid-flight falls to the plain shutdown branch and settles immediately —
    # the parked check fails before the per-key rejection is even reached.
    "shutdown-drain-registration-removed": (
        owner,
        """                    let (drain_done, drain_completion) = watch::channel(None);
                    owner
                        .0
                        .state
                        .lock()
                        .unwrap()
                        .closings
                        .insert(key.clone(), drain_completion);""",
        """                    let (drain_done, drain_completion) = watch::channel(None);""",
        [DURING_SHUTDOWN],
        [
            "#1245 close of a mid-flight shutdown eviction must await it, "
            "not answer early",
        ],
    ),
    # #1245 fix-r1 core: the shutdown drain list steals the record of a key
    # whose close registered its eviction but has not yet taken the record —
    # the close task then drains to Ok and the eviction flips to the drain's
    # `shutdown` reason.
    "shutdown-steals-close-owned-record": (
        owner,
        """            if unsettled_closing(&mut state, &key).is_some() {
                continue;
            }
""",
        "",
        [CLOSE_THEN_SHUTDOWN],
        [
            "#1245 a close that starts before a same-tick shutdown must reject "
            "with its own dispose failure",
        ],
    ),
    # #1245 fix-r2: the skip must not stop the drain list — breaking out of
    # the order loop leaves the plain key's record cached forever (the order
    # vec was already taken), undisposed and undrained, while shutdown still
    # succeeds through the closing key's aggregate.
    "shutdown-skip-stops-draining": (
        owner,
        """            if unsettled_closing(&mut state, &key).is_some() {
                continue;
            }""",
        """            if unsettled_closing(&mut state, &key).is_some() {
                break;
            }""",
        [SKIP_ONLY_CLOSING],
        [
            "#1245 the shutdown drain must still dispose the plain key after "
            "skipping the closing key",
        ],
    ),
    # #1245 fix-r2: the per-key drain loses its completion guard — a panicked
    # disposal then settles nothing: the entry hangs unsettled (all senders
    # dropped), the aggregate only sees wait_result's lifecycle-stopped
    # stand-in, and a later record-less close answers that same error instead
    # of falling back to the post-shutdown branch.
    "drain-guard-removed": (
        owner,
        """                    super::super::cleanup_task::spawn_with_error(
                        &tokio::runtime::Handle::current(),
                        drain_done,
                        RuntimeError::new(
                            "Session disposal task was dropped or panicked before completion.",
                        ),
                        async move {
                            // #1055 ②: fold each record's unconsumed creation or
                            // cleanup error into the shutdown aggregate, aggregated
                            // beside a failed disposal's own error, so neither is
                            // dropped.
                            let pending = record.unreported_creation_error.clone();
                            combine_pending_error(
                                owner.evict(&key, record, EvictionReason::Shutdown).await,
                                pending,
                            )
                        },
                    );""",
        """                    let settle = drain_done.clone();
                    let _unguarded = drain_done;
                    tokio::spawn(async move {
                        // #1055 ②: fold each record's unconsumed creation or
                        // cleanup error into the shutdown aggregate, aggregated
                        // beside a failed disposal's own error, so neither is
                        // dropped.
                        let pending = record.unreported_creation_error.clone();
                        let result = combine_pending_error(
                            owner.evict(&key, record, EvictionReason::Shutdown).await,
                            pending,
                        );
                        settle.send_replace(Some(result));
                    });""",
        [DRAIN_PANIC],
        [
            "#1245 a close after a panicked drain must settle through the "
            "completion guard",
            "#1245 a record-less close after shutdown must fulfill like TS "
            "await undefined",
        ],
    ),
    # #1245 fix-r3: the settle-before-release invariant loses its machine
    # witness — the closing guard releases the serial ticket first and settles
    # only afterwards, which is exactly the window where a queued same-key
    # creation's published record escapes a later shutdown's
    # unsettled-closing skip (the interleave itself has no await point, so the
    # deterministic kill is the order probe's event-sequence assertion). The
    # window reopens for the close task and the idle sweep task alike.
    "settle-after-release": (
        owner,
        """impl Drop for ClosingGuard {
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {""",
        """impl Drop for ClosingGuard {
    fn drop(&mut self) {
        self.exclusive = None;
        if let Some(outcome) = self.outcome.take() {""",
        [CLOSE_SETTLES, IDLE_SETTLES],
        [
            "#1245 the close task must settle its closing entry before "
            "releasing the serial ticket",
            "#1245 the idle sweep task must settle its closing entry before "
            "releasing the serial ticket",
        ],
    ),
}


def mutate(original, _scope, replacements):
    for old, new in replacements:
        count = original.count(old)
        if count != 1:
            raise ValueError(f"expected one mutation anchor, got {count}")
        original = original.replace(old, new, 1)
    return original


def cases_for(names):
    bound = []
    for name in names:
        source, old, new, tests, markers = MUTATIONS[name]
        for test in tests:
            bound.append(Case(name, source, old, new, test, markers))
    return bound


def check_anchors(selected):
    stale = []
    for name in selected:
        source, old = MUTATIONS[name][0], MUTATIONS[name][1]
        count = source.read_text().count(old)
        if count != 1:
            stale.append((name, source.relative_to(root).as_posix(), count))
    if stale:
        for name, path, count in stale:
            print(f"STALE ANCHOR {name}: {count} matches in {path}")
        print("STALE ANCHORS: refusing to run")
        raise SystemExit(2)
    print(f"{len(selected)} anchors: ok")


def compile_cases(selected, out):
    """Apply each mutation once, prove `cargo test --no-run`, restore."""
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(MUTATIONS[name][0] for name in selected))) as originals:
            for name in selected:
                source, old, new = MUTATIONS[name][:3]
                status, reason = "INVALID", ""
                try:
                    source.write_text(mutate(originals[source].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / "scripts/cargo-slots.sh")
                    (out / f"{name}-compile.slots.log").write_text(slot.stdout + slot.stderr)
                    result = run_owned(
                        ["cargo", "test", "--offline", "--manifest-path", "src/Cargo.toml",
                         "-p", "pilotdeck-gateway", "--lib", "--no-run"],
                        cwd=root, env=os.environ.copy(), timeout=1800,
                    )
                    (out / f"{name}-compile.log").write_text(result.stdout + result.stderr)
                    status = "COMPILED" if result.returncode == 0 else "INVALID"
                    reason = f"cargo exit={result.returncode}"
                except Exception as error:
                    reason = f"{type(error).__name__}: {error}"
                finally:
                    source.write_bytes(originals[source])
                results.append({"mutation": name, "status": status, "reason": reason})
                print(f"{name}: {status}: {reason}", flush=True)
    except Exception as error:
        for name in selected[len(results):]:
            results.append({"mutation": name, "status": "INVALID", "reason": str(error)})
        for row in results:
            row.update(status="INVALID", reason=row["reason"] + f"; source guard: {error}")
    (out / "compile-summary.json").write_text(json.dumps(results, indent=2) + "\n")
    counts = {state: sum(row["status"] == state for row in results)
              for state in ("COMPILED", "INVALID")}
    print("compile summary: " + ", ".join(f"{key}={value}" for key, value in counts.items()))
    return int(not results or any(row["status"] != "COMPILED" for row in results))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, nargs="?")
    parser.add_argument("names", nargs="*", choices=sorted(MUTATIONS))
    parser.add_argument("--sample", action="store_true")
    parser.add_argument("--compile-only", action="store_true")
    parser.add_argument("--check-anchors", action="store_true")
    args = parser.parse_args()
    selected = ["close-else-awaits-nothing"] if args.sample else (args.names or list(MUTATIONS))
    if args.check_anchors:
        check_anchors(selected)
        return 0
    assert args.out is not None, "provide the output directory"
    args.out.mkdir(parents=True, exist_ok=False)
    check_anchors(selected)
    compiled = compile_cases(selected, args.out)
    if args.compile_only:
        return compiled
    runtime = run_cases(
        root,
        cases_for(selected),
        args.out,
        os.environ.copy(),
        mutate,
        timeout=1800,
    )
    return int(compiled != 0 or runtime != 0)


if __name__ == "__main__":
    raise SystemExit(main())
