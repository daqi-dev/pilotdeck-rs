#!/usr/bin/env python3
"""#996: session-owner controls as compile-time switches.

The #1245-era text-replacement negative.py is deleted; each mutation is now a
``crate::mutants::on`` branch compiled into the feature-enabled library test
binary. ``tampered-source-hash`` is renamed ``owner-tampered-source-hash``
(the session-abort-contract fixture shares the old name for a different
oracle): the switch zeroes the loaded oracle's recorded hash for
src/gateway/SessionRouter.ts in memory, so the provenance panic fires before
the data is read — the same bytes the old on-disk mutation wrote. Kill anchors
are the bound tests' own assertion messages the old runner observed.
CARGO_TARGET_DIR and PDRS_BASELINE must be supplied (the
``owner-tampered-source-hash`` row only kills with a real baseline checkout;
the old negative.py failed fast on the same missing variable). Use --check for
a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import non_head
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
ADMISSION = ROOT / 'src/pilotdeck-gateway/src/runtime/session/admission.rs'
HANDLE = ROOT / 'src/pilotdeck-gateway/src/runtime/session/handle.rs'
OWNER_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner_tests.rs'
TESTS = 'runtime::session::owner::tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('reserve-activity-ignores-inactive-handle', HANDLE, TESTS
     + 'publication_cutoff_revokes_cached_handle_before_async_disposal_starts',
     'publication admitted activity on the old handle'),
    ('idle-ignores-inflight-and-queued-close', OWNER, TESTS
     + 'idle_sweep_skips_same_key_creation_and_queued_close',
     'idle sweep evicted a session during same-key creation'),
    ('close-without-ticket-serialization', OWNER, TESTS
     + 'close_waits_for_prior_same_key_close_to_drain',
     '#1245 close settled before the in-flight same-key creation published '
     'its record'),
    ('empty-project-is-paused', OWNER, TESTS
     + 'empty_project_pause_and_close_matches_ts', 'Project is being deleted.'),
    ('old-handle-is-current', OWNER, TESTS
     + 'dirty_identity_and_publication_matches_ts', 'oldCurrent'),
    ('global-factory-wait', OWNER, TESTS
     + 'slow_factory_is_per_session_matches_ts',
     'controlled lifecycle must progress independently'),
    ('ignore-maintenance', ADMISSION, TESTS
     + 'maintenance_blocks_turn_and_idle_matches_ts',
     'maintenance_blocks_turn_and_idle: value'),
    ('ignore-stop-new-publication', OWNER, TESTS
     + 'shutdown_during_creation_matches_ts', 'expected failure'),
    ('unchecked-live-abort', OWNER, TESTS
     + 'live_sink_rechecks_owner_after_abort_decision_across_threads',
     '#818 an old abort reached the new live writer'),
    ('stale-end-releases-readmitted-reservation', ADMISSION, TESTS
     + 'stale_end_after_close_and_same_run_readmission_preserves_new_reservation',
     'stale end_turn released the readmitted same-run reservation'),
    ('old-close-clears-new-reservation', OWNER, TESTS
     + 'close_and_completion_cleanup_preserve_later_same_run_reservation',
     'old close released a new reservation with the same runId'),
    ('old-complete-releases-new-owner', OWNER, TESTS
     + 'close_and_completion_cleanup_preserve_later_same_run_reservation',
     'assertion failed: owner.has_active_turn(&key)'),
    ('idle-ignores-active-turn', OWNER, TESTS
     + 'idle_strict_boundary_and_turn_matches_ts',
     'idle_strict_boundary_and_turn: value'),
    ('idle-ignores-maintenance', OWNER, TESTS
     + 'maintenance_blocks_turn_and_idle_matches_ts',
     'maintenance_blocks_turn_and_idle: value'),
    ('idle-strict-boundary-inclusive', OWNER, TESTS
     + 'idle_strict_boundary_and_turn_matches_ts',
     'idle_strict_boundary_and_turn: value'),
    ('owner-tampered-source-hash', OWNER_TESTS, TESTS
     + 'dirty_identity_and_publication_matches_ts',
     'oracle hash for src/gateway/SessionRouter.ts does not match'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    OWNER,
    ADMISSION,
    HANDLE,
    OWNER_TESTS,
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'session-owner/oracle.json',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / '_negkit/non_head.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
    Path(__file__).resolve(),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        if not os.environ.get('PDRS_BASELINE'):
            raise ValueError('PDRS_BASELINE is required: owner-tampered-source-hash '
                              'only kills against a real baseline checkout')
        output = Path(args.output).resolve()
        # #1540: the compile-time-switch executor always runs the working
        # tree, so a pinned PDRS_NEGATIVE_BASELINE cannot be honoured: the
        # request is announced as ignored and a green run is still downgraded
        # (exit 42) — it cannot certify the caller-named tree, which is the
        # mistake exit 0 invited (network-timing precedent).
        baseline_source, _ignored_ref, baseline_sha = non_head.gate(
            ROOT, ignored=True)
        output.mkdir(parents=True, exist_ok=False)
        return non_head.non_head_exit(
            baseline_source, baseline_sha,
            code=mutant_switch.run(ROOT, output, chosen,
                                   mutant_switch.clean_environment(os.environ),
                                   guarded=GUARDED, prefix='', script=Path(__file__),
                                   requested=args.names),
            evidence=output, ignored=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
