#!/usr/bin/env python3
"""#996: live-session controls as compile-time switches.

The #743 text-replacement negative_controls.py is deleted; each mutation is now
a ``crate::mutants::on`` branch compiled into the feature-enabled library test
binary. The old entry judged kills through the #946 nonce receipt protocol
(``matrix_main`` semantics over ``negative_suite.ReceiptRun`` / ``judge``), so
the shared run keeps it (receipts=True): a bound test only counts as killed on
a named FAILED line plus this run's assertion receipt, never on prose.
CARGO_TARGET_DIR must be supplied. Use --check for a read-only anchor
preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import non_head
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SHELL = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_session.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
HOOKS = ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_hooks.rs'
STEER = ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_steer.rs'
MAILBOX = ROOT / 'src/pilotdeck-gateway/src/runtime/steer.rs'
HANDLE = ROOT / 'src/pilotdeck-gateway/src/runtime/session/handle.rs'
WRITER = ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
RUNNER = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
CLEANUP = ROOT / 'src/pilotdeck-gateway/src/runtime/cleanup_task.rs'
PRODUCTION = ('runtime::turn::host_factory::tests::'
              'session_hooks_wrap_real_turn_and_retain_lease_through_end')
SESSION_TESTS = 'runtime::turn::live_session::tests::'
TEST = 'runtime::turn::live_session::tests::session_matrix_matches_ts'
SCOPE = 'runtime::project::interaction_hooks::session::tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('skip-end', SHELL, TEST, '#743 session matrix start-success'),
    ('skip-start', SHELL, TEST, '#743 session matrix start-success'),
    ('skip-setup', SHELL, TEST, '#743 session matrix start-success'),
    ('start-source', SHELL, TEST, '#743 session matrix start-success'),
    ('session-ended-event', SHELL, TEST, '#743 session matrix start-success'),
    ('drop-current-turn-clear', LIVE, TEST,
     '#743 session matrix start-success'),
    ('aborted-result-status', LIVE, TEST,
     '#743 session matrix abort-start-cooperative'),
    ('error-result-status', LIVE, TEST,
     '#743 session matrix turn-model-failure'),
    ('runner-error-preserves-running', SHELL, TEST,
     '#743 session matrix turn-runner-throw'),
    ('session-signal-lost', HOOKS,
     SCOPE + 'preaborted_session_end_command_still_executes',
     '#743 preabort End retains signal'),
    ('barrier-cancel-lost', HOOKS,
     SCOPE + 'aborted_replacement_does_not_wait_for_unfinished_predecessor',
     '#743 session barrier cancellation terminates wait'),
    ('completion-delays-steer-close', RUNNER, PRODUCTION,
     '#743 turn_result waits durable steer close'),
    ('finish-before-terminal', STEER, PRODUCTION,
     '#743 finish occurs after terminal yield'),
    ('skip-finish-before-end', SHELL, PRODUCTION, '#743 finish before End'),
    ('owner-slot-kept-after-none', HANDLE,
     SESSION_TESTS + 'owner_holds_end_and_releases_at_none',
     '#743 owner release on None'),
    ('writer-close-does-not-fence', WRITER,
     SESSION_TESTS + 'dispose_running_signals_then_closes_writer',
     '#743 dispose closes writer'),
    ('dispose-loses-abort-signal', LIVE,
     SESSION_TESTS + 'dispose_running_signals_then_closes_writer',
     '#743 dispose aborts running signal'),
    ('recompute-end-reason', SHELL, TEST,
     '#743 session matrix abort-end-cooperative'),
    ('drop-needs-current-runtime', SHELL,
     SESSION_TESTS + 'lifecycle_drop_off_runtime_uses_captured_executor',
     '#743 off-runtime lifecycle drop cannot panic'),
    ('shutdown-bare-spawn', SHELL,
     SESSION_TESTS + 'lifecycle_shutdown_drop_settles_without_unwind',
     '#743 shutdown completion guard settles unpolled cleanup'),
    ('disposer-error-value-lost', SHELL,
     SESSION_TESTS + 'lifecycle_close_completion_covers_error_panic_and_cancellation',
     '#743 completion preserves disposer error'),
    ('cleanup-task-loss-value-lost', CLEANUP,
     SESSION_TESTS + 'lifecycle_close_completion_covers_error_panic_and_cancellation',
     '#743 completion reports panicked or cancelled cleanup'),
    ('ordinary-hook-error-rejects-session', SHELL,
     SESSION_TESTS + 'ordinary_hook_results_keep_session_tail',
     '#743 executor returned errors ignored'),
    ('drop-steer-unapplied', RUNNER, PRODUCTION,
     '#743 discarded steer emits unapplied once'),
    ('steer-insert-after-close-accepted', MAILBOX, PRODUCTION,
     '#743 steer in-flight insert rejected after close'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    SHELL,
    LIVE,
    HOOKS,
    STEER,
    MAILBOX,
    HANDLE,
    WRITER,
    RUNNER,
    CLEANUP,
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/interaction.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_session_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_hooks/session.rs',
    ROOT / 'src/pilotdeck-gateway/tests/fixtures/live-session/peer.py',
    # This suite judges kills by #946 nonce receipts; the selector forwarding
    # lives in these two files and must not change under a running suite.
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'live-session/oracle.json',
    # #2228 r11+r12 (ported at the #996-r3 rebase onto origin/main): the
    # same --lib binary this suite precompiles bakes more of the bound
    # judge's inputs than live-session/oracle.json alone. live.rs bakes
    # live-e2e/terminal-boundaries/oracle.json (:2231 here; :2143 when
    # #2228 r12 registered it) and tests/common/baseline.rs (with its
    # #[path] regeneration_masks.rs, compiled into the same --lib binary)
    # backs test_baseline::oracle(); host_factory_tests.rs (registered
    # above) bakes host-factory/oracle.json (:463/:562/:614),
    # tool-budget-cwd/oracle.json (:803) and host-manifest/oracle.json
    # (:1495). The source guard hashes these at entry and re-verifies at
    # exit: an edit that persists to the end of the run is caught. It is a
    # working-tree hash guard, not a HEAD comparison — dirt already present
    # at entry is accepted as the baseline, and a transient edit that is
    # reverted before the exit check is not (source.json leaves the
    # working-tree hashes on record).
    FIXTURES / 'live-e2e/terminal-boundaries/oracle.json',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    FIXTURES / 'host-factory/oracle.json',
    FIXTURES / 'tool-budget-cwd/oracle.json',
    FIXTURES / 'host-manifest/oracle.json',
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
                                   requested=args.names, receipts=True),
            evidence=output, ignored=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
