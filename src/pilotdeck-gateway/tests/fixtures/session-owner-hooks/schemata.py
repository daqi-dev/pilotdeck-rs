#!/usr/bin/env python3
"""#996: session-owner-hooks controls as compile-time switches.

The custom-runner negative.py is deleted; each mutation is now a
``crate::mutants::on`` branch compiled into the feature-enabled library test
binary. The two old second-pass rewrites (early-snapshot's moved emit,
early-callback's delayed await) became switch-guarded branches at the same
sites. Kill anchors are the named test's own assertion messages the old
runners observed (owner_hooks_tests.rs: active case / idle callback
order/payload / non-idle callback / snapshot failed / snapshot optional
callback / snapshot before disposal finished). CARGO_TARGET_DIR must be
supplied. Use --check for a read-only anchor preflight, or NEW_OUTPUT_DIR
[switch ...].
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
ACTIVE = 'runtime::session::owner::tests::hooks::active_user_turn_matches_ts'
IDLE = ('runtime::session::owner::tests::hooks::'
        'idle_hook_matches_ts_with_turn_interleaving')
OTHER = 'runtime::session::owner::tests::hooks::non_idle_hooks_are_silent'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('no-active', OWNER, ACTIVE, 'active case'),
    ('no-admission', OWNER, ACTIVE, 'active case'),
    ('always-on', OWNER, ACTIVE, 'active case'),
    ('cron', OWNER, ACTIVE, 'active case'),
    ('project', OWNER, ACTIVE, 'active case'),
    ('no-hook', OWNER, IDLE, 'idle callback order/payload'),
    ('dispose-error', OWNER, IDLE, 'idle callback order/payload'),
    ('early-snapshot', OWNER, IDLE, 'idle callback order/payload'),
    ('timestamp', OWNER, IDLE, 'idle callback order/payload'),
    ('context', OWNER, IDLE, 'idle callback order/payload'),
    ('array-length', OWNER, IDLE, 'idle callback order/payload'),
    ('utf16-length', OWNER, IDLE, 'idle callback order/payload'),
    ('inherited-messages', OWNER, IDLE, 'idle callback order/payload'),
    ('inherited-length', OWNER, IDLE, 'idle callback order/payload'),
    ('object-length', OWNER, IDLE, 'idle callback order/payload'),
    ('undefined', OWNER, IDLE, 'idle callback order/payload'),
    ('missing-count', OWNER, IDLE, 'idle callback order/payload'),
    ('snapshot-panic', OWNER, IDLE, 'snapshot failed'),
    ('disabled-eager', OWNER, IDLE, 'snapshot optional callback'),
    ('early-callback', OWNER, IDLE, 'snapshot before disposal finished'),
    ('all-reasons', OWNER, OTHER, 'non-idle callback'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    OWNER,
    ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner_hooks_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'session-owner-hooks/oracle.json',
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
                                   requested=args.names),
            evidence=output, ignored=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
