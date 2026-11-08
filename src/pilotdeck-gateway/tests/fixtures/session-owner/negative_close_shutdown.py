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
by the exact assertion named in its markers, except the rows registered in
``EQUIVALENT``: their single-site deletion provably changes no product
observable, so no test can kill them. An equivalent row must instead compile
and leave its bound test green; if it ever starts failing the registry is
stale and the device refuses the run. Every case is classified
KILLED / SURVIVED / INVALID / EQUIVALENT, all cases run to completion, and any
non-KILLED must-kill result exits non-zero.

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
mutation, not one marker per case. A row may carry a sixth element, a list of
extra ``(before, after)`` pairs applied alongside the main pair: the #1365
fix-r1 paired mutations delete the closing guard's write *and* redirect the
outer ``Completion``'s sender to a fresh channel, because the single-site
deletion is an equivalent mutation (measured SURVIVED by the gate run — the
guard's ``send_replace`` and ``Completion::finish`` re-send the identical
outcome in the same poll, with no await point between them on the
current-thread test runtime). The extra pairs travel through ``Case.scope``
into this script's ``mutate``; the shared runner's call shape is unchanged.
The static preflight only audits fields 0-4, so ``check_anchors`` verifies the
extra anchors itself.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, case_labels, classify, passed, run_cases

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
CLOSE_PANIC = (
    "runtime::session::owner::tests::"
    "close_task_panic_settles_its_closing_entry_through_the_completion_guard"
)
QUEUED_DRAIN = (
    "runtime::session::owner::tests::"
    "queued_close_drains_through_the_ticket_release_write"
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
        """        if after.is_none()
            && let Some(closing) = unsettled_closing(&mut state, key)
        {
            return Box::pin(wait_result(closing));
        }
        if state.shutdown.is_some() {
            return if after.is_some() {
                Box::pin(async { Err(RuntimeError::new("Session router is shut down.")) })
            } else {
                Box::pin(async { Ok(()) })
            };
        }""",
        """        if after.is_none() {
            if let Some(shutdown) = state.shutdown.clone() {
                return Box::pin(wait_result(shutdown));
            }
        } else if state.shutdown.is_some() {
            return Box::pin(async { Err(RuntimeError::new("Session router is shut down.")) });
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
        """        if after.is_none()
            && let Some(closing) = unsettled_closing(&mut state, key)
        {
            return Box::pin(wait_result(closing));
        }""",
        """        if after.is_none() && unsettled_closing(&mut state, key).is_some() {
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
        """        track(&mut state, key.clone(), project, completion.clone());
        state.closings.insert(key.clone(), completion.clone());""",
        """        track(&mut state, key.clone(), project, completion.clone());""",
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
    # #1365 fix-r1 (paired, F1): the closing entry's settle write is doubly
    # covered on the success path — the guard writes, then the outer
    # `Completion::finish` re-sends the identical outcome in the same poll,
    # with no await point between them on the current-thread test runtime. A
    # mutation deleting only the guard's `send_replace` is therefore an
    # equivalent mutation in this design (the coordinator's gate run measured
    # it SURVIVED: `_tmp/20261105-issue-1365-gate/negctl/
    # closing-settle-write-removed/`, cargo exit=0, bound test still passed);
    # the guard write's own observable effect is its ordering strictly before
    # the ticket release, which `settle-after-release` kills through the
    # event sequence. This row removes BOTH writers — the guard's
    # `send_replace` plus the close task's `done` sender, redirected to a
    # fresh channel so the real closing entry is never sent a value — so
    # close() must then reject with wait_result's lifecycle-stopped stand-in
    # and the bounded().expect() turns red. IDLE_SETTLES is NOT bound: that
    # test never awaits the settle result and its probe keeps firing, so it
    # would SURVIVE (order remains correct; the idle task's own settle-write
    # is covered by the paired idle-settle-write-removed below).
    "closing-settle-write-removed": (
        owner,
        """            self.settle.send_replace(Some(outcome));
            #[cfg(test)]
            settle_order::record("closing-settled");""",
        """            #[cfg(test)]
            settle_order::record("closing-settled");""",
        [CLOSE_SETTLES],
        [
            "#1245 the close must fulfill with a succeeding disposal",
        ],
        [
            (
                """        let settle = done.clone();
        let owner = self.clone();""",
                """        let settle = done.clone();
        let (done, _) = watch::channel(None);
        let owner = self.clone();""",
            ),
        ],
    ),
    # #1365 fix-r1 (sibling of the paired close row, 自查-1): the idle sweep
    # task has the same dual-sender shape, so its settle-write needs the same
    # paired treatment — the guard's write is deleted and the idle task's
    # `done` sender is redirected to a fresh channel. A close arriving
    # mid-flight then awaits an entry nobody writes and answers wait_result's
    # lifecycle-stopped stand-in instead of transmitting the idle eviction's
    # own dispose failure.
    "idle-settle-write-removed": (
        owner,
        """            self.settle.send_replace(Some(outcome));
            #[cfg(test)]
            settle_order::record("closing-settled");""",
        """            #[cfg(test)]
            settle_order::record("closing-settled");""",
        [IDLE_EVICT],
        [
            "#1245 close during an idle eviction transmits the per-key dispose aggregate",
        ],
        [
            (
                """            let owner = self.clone();
            let settle = done.clone();""",
                """            let owner = self.clone();
            let settle = done.clone();
            let (done, _) = watch::channel(None);""",
            ),
        ],
    ),
    # #1365 fix-r1 (F3): the close task's panic path stages no outcome — the
    # guard only releases the ticket — and the outer `Completion` guard settles
    # the entry with its dropped error after the release. Losing that
    # completion guard (a bare tokio::spawn, mirroring drain-guard-removed
    # for the shutdown drain) leaves the panicked entry with no writer at
    # all: close() then answers wait_result's lifecycle-stopped stand-in
    # instead of the guard's dropped error.
    "close-panic-guard-removed": (
        owner,
        """        super::super::cleanup_task::spawn_with_error(
            &tokio::runtime::Handle::current(),
            done,
            RuntimeError::new("Session close task was dropped or panicked before completion."),
            async move {""",
        """        drop(done);
        tokio::spawn(async move {""",
        [CLOSE_PANIC],
        [
            "#1365 the panicked close settles with the completion guard's dropped error",
        ],
    ),
    # #1365 fix-r1 (P2), registered EQUIVALENT: deleting the serial ticket's
    # release write changes no product observable. The sender is dropped at the
    # end of `Ticket::drop` either way, and `Ticket::enter`'s
    # `let _ = previous.wait_for(|done| *done).await` treats `Ok(true)` and the
    # closed channel's `Err(RecvError)` identically — the queued close wakes
    # and drains in both cases. The `ticket-released` probe records
    # unconditionally after the deletion site, so the event sequence cannot
    # witness the write either. Measured SURVIVED (reviewer round:
    # `_tmp/20261105-issue-1365/review-r1/negctl.log`; fix-r2 full run:
    # `_tmp/20261105-issue-1365-fix-r2/negctl/`). The row stays here so the
    # static anchor preflight still counts it; `EQUIVALENT` below re-measures
    # that it still survives and fails the run if it ever becomes killable.
    "ticket-release-write-removed": (
        owner,
        """        self.done.send_replace(true);""",
        """        let _ = &self.done;""",
        [QUEUED_DRAIN],
        [
            "#1365 the queued close must drain after the creation's ticket release write",
        ],
    ),
}


# Registered equivalent mutations: the single-site deletion changes no product
# observable, so no test can kill it and it must not sit in the must-kill set.
# Each value is (bound test, measured reason). The row above stays in MUTATIONS
# so the static anchor preflight still resolves it; this registry re-measures
# the equivalence at runtime — the mutant must compile and the bound test must
# still pass (SURVIVED) before and after the guarded restore. If the test ever
# grows a real witness and kills the row, the run reports INVALID: the entry is
# stale and the row belongs back in the must-kill set. Registering the reason
# here (not only in a commit message) keeps the next person from re-adding it.
EQUIVALENT = {
    "ticket-release-write-removed": (
        QUEUED_DRAIN,
        "the release write is indistinguishable from dropping the sender: "
        "Ticket::enter's `let _ = previous.wait_for(|done| *done).await` "
        "ignores the closed-channel Err(RecvError) exactly like it ignores "
        "Ok(true), and the ticket-released probe records unconditionally after "
        "the deletion site, so the queued close drains and the event sequence "
        "stays green; measured SURVIVED in "
        "_tmp/20261105-issue-1365/review-r1/negctl.log and the fix-r2 full run",
    ),
}


def must_kill(names):
    """The selected rows that must be KILLED; equivalents are measured apart."""
    return [name for name in names if name not in EQUIVALENT]


def check_registries():
    """#1365 fix-r3: the two registries must classify a row the same way.

    ``MUTATIONS`` is the single source of a row's anchor, bound tests and
    markers; ``EQUIVALENT`` is the runtime certificate that the row cannot be
    killed. If the name or the bound test drifts apart, the certificate
    measures a test that no longer belongs to the row — or nothing at all —
    and still reports EQUIVALENT, so the reverse check silently expires. A
    registered equivalent must also never be selected as must-kill: the two
    tables may agree about a name, but never disagree about the verdict.
    """
    problems = []
    for name, (test, _reason) in EQUIVALENT.items():
        # #1413 fix-r4: plain membership + index access. anchor_selfcheck
        # rejects every method call on a TABLES dict (MUTATIONS is one), and
        # ``MUTATIONS.get(name)`` tripped the preflight on this file.
        if name not in MUTATIONS:
            problems.append(f'{name}: registered equivalent has no MUTATIONS row')
            continue
        row = MUTATIONS[name]
        if not row[3]:
            problems.append(f'{name}: equivalent row binds no test')
        elif test not in row[3]:
            problems.append(f'{name}: EQUIVALENT binds {test!r}, '
                            f'MUTATIONS binds {list(row[3])!r}')
        if not row[4]:
            problems.append(f'{name}: equivalent row lists no marker')
    overlap = sorted(set(EQUIVALENT).intersection(must_kill(list(MUTATIONS))))
    if overlap:
        problems.append(f'{overlap}: equivalent rows also selected as must-kill')
    if problems:
        for problem in problems:
            print(f'REGISTRY DRIFT {problem}')
        # RuntimeError, not SystemExit: main() turns an Exception into rc 1
        # (refusal), while rc 2 is reserved for an unverified restore. The
        # registries are checked before any source is touched, so nothing here
        # needs the restore-failure channel.
        raise RuntimeError('REGISTRY DRIFT: refusing to run')


def mutate(original, scope, replacements):
    for old, new in list(replacements) + list(scope or []):
        count = original.count(old)
        if count != 1:
            raise ValueError(f"expected one mutation anchor, got {count}")
        original = original.replace(old, new, 1)
    return original


def extra_pairs(name):
    """The paired mutation's extra (before, after) sites, or None."""
    row = MUTATIONS[name]
    return row[5] if len(row) > 5 else None


def cases_for(names):
    bound = []
    for name in names:
        source, old, new, tests, markers = MUTATIONS[name][:5]
        for test in tests:
            bound.append(Case(name, source, old, new, test, markers, extra_pairs(name)))
    return bound


def equivalent_bound(names):
    """Every test an equivalent row binds, each with its ``Case``.

    #1413 fix-r4: the caller computes evidence stems once over the must-kill
    and equivalent Cases together, so no two Cases across the two phases share
    a ``-positive``/``-equivalent`` file stem.
    """
    bound = []
    for name in names:
        row = MUTATIONS[name]
        for test in dict.fromkeys(row[3]):
            bound.append((Case(name, row[0], row[1], row[2], test, row[4],
                               extra_pairs(name)), test))
    return bound


def run_equivalent_cases(bound, out, labels):
    """Re-measure every registered equivalent row; a stale claim fails the run.

    An equivalent mutation must compile and leave every test its row binds
    green. If any bound test starts failing, the witness it lacked now exists:
    report INVALID so the row is promoted back into the must-kill set instead
    of silently rotting in the registry.
    """
    check_registries()
    if not bound:
        return 0
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, test):
        # #1365 fix-r3: a mutant that hangs must still leave its raw output on
        # disk. The shared executor's run() appends the timeout payload for the
        # same reason; without it the evidence directory keeps one printed line
        # and the next reader cannot see that the mutant parked the close.
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            result = run_owned(command + [test, '--', '--exact', '--include-ignored'],
                               cwd=root, env=os.environ.copy(), timeout=1800)
            (out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += (value.decode(errors='replace')
                            if isinstance(value, bytes) else value or '')
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def publish():
        (out / 'equivalent-summary.json').write_text(json.dumps(results, indent=2) + '\n')

    def record(case, label, status, detail):
        results.append({'mutation': case.mutation, 'test': case.test, 'status': status,
                        'reason': detail, 'restored': status == 'EQUIVALENT'})
        print(f'{label}: {status}: {detail}', flush=True)

    paths = list(dict.fromkeys(case.path for case, _test in bound))
    try:
        with mutating_sources(root, paths) as pristine:
            for case, test in bound:
                reason = EQUIVALENT[case.mutation][1]
                label = labels[id(case)]
                status, detail = 'INVALID', ''
                try:
                    if not passed(run(f'{label}-positive', test), test):
                        detail = 'positive control did not pass the bound test'
                    else:
                        case.path.write_text(mutate(pristine[case.path].decode(),
                                                    case.scope,
                                                    [(case.old, case.new)]))
                        measured, measured_reason = classify(
                            run(f'{label}-equivalent', test), case)
                        if measured == 'SURVIVED':
                            status = 'EQUIVALENT'
                            detail = f'registered equivalent survives: {reason}'
                        elif measured == 'KILLED':
                            detail = ('registered equivalent is now killed by its bound '
                                      'test; promote it to the must-kill set: '
                                      + measured_reason)
                        else:
                            detail = f'equivalent row not measurable: {measured_reason}'
                except Exception as error:
                    status, detail = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    # Restore before the restored check, also on timeout or bad anchors.
                    case.path.write_bytes(pristine[case.path])
                    if case.path.read_bytes() != pristine[case.path]:
                        raise RuntimeError(f'byte restoration failed: {case.path}')
                if status == 'EQUIVALENT':
                    try:
                        if not passed(run(f'{label}-restored', test), test):
                            status = 'INVALID'
                            detail += '; restored bound test did not pass'
                    except Exception as error:
                        status = 'INVALID'
                        detail += f'; restored: {type(error).__name__}: {error}'
                record(case, label, status, detail)
    except BaseException as error:
        # #1365 fix-r3: a guard failure inside the equivalent window must never
        # leave the must-kill phase's all-green summary.json as the only file on
        # disk. Void every recorded row, account for the bound tests that never
        # ran, publish the INVALID equivalent summary, then let the caller fail
        # closed. A missing summary is fail-closed; a stale all-EQUIVALENT one
        # next to a non-zero exit is not.
        guard = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + guard,
                       restored=False)
        done = {(row['mutation'], row['test']) for row in results}
        for case, test in bound:
            if (case.mutation, test) not in done:
                record(case, labels[id(case)], 'INVALID', guard)
        publish()
        raise
    publish()
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('EQUIVALENT', 'INVALID')}
    print('equivalent summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()))
    return int(not results or any(row['status'] != 'EQUIVALENT' for row in results))


def check_anchors(selected):
    stale = []
    for name in selected:
        source = MUTATIONS[name][0]
        for old in [MUTATIONS[name][1]] + [pair[0] for pair in (extra_pairs(name) or [])]:
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
                    source.write_text(mutate(originals[source].decode(), extra_pairs(name), [(old, new)]))
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
    # #1365 fix-r3: refuse before any source is touched. A drifted registry
    # would otherwise be measured against a test the row no longer binds.
    check_registries()
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
    kill = must_kill(selected)
    kill_cases = cases_for(kill)
    bound = equivalent_bound([name for name in selected if name in EQUIVALENT])
    # #1413 fix-r4: one label pass over both phases, so their evidence files
    # cannot overwrite each other in the shared output directory.
    labels = case_labels(kill_cases + [case for case, _test in bound])
    runtime = run_cases(
        root,
        kill_cases,
        args.out,
        os.environ.copy(),
        mutate,
        timeout=1800,
        labels=labels,
    ) if kill else 0
    try:
        equivalent = run_equivalent_cases(bound, args.out, labels)
    except Exception as error:
        print(f'equivalent guard: {type(error).__name__}: {error}', flush=True)
        equivalent = 1
    return int(compiled != 0 or runtime != 0 or equivalent != 0)


if __name__ == "__main__":
    raise SystemExit(main())
