#!/usr/bin/env python3
"""#996: session-domain controls as compile-time switches.

The #743 fix-r3 rustc harness and the nth-occurrence text-replacement
negative.py are deleted; every guard/value mutation is now a
``crate::mutants::on`` branch compiled into the feature-enabled library test
binary, still bound to the golden domain test and its #743 assertion anchors.
CARGO_TARGET_DIR must be supplied. Use --check for a read-only anchor
preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
DOMAIN = ROOT / 'src/pilotdeck-gateway/src/storage/session_domain.rs'
TEST = 'storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes'
# The golden test asserts both outcome kinds; either assertion proves the kill.
ANCHORS = ['#743 domain transition outcome', '#743 domain log outcome']
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('queue-input', DOMAIN, TEST, ANCHORS),
    ('queue-reused-item', DOMAIN, TEST, ANCHORS),
    ('queue-reused-turn', DOMAIN, TEST, ANCHORS),
    ('queue-discard', DOMAIN, TEST, ANCHORS),
    ('durable-inbox-claim', DOMAIN, TEST, ANCHORS),
    ('duplicate-turn', DOMAIN, TEST, ANCHORS),
    ('queue-claim', DOMAIN, TEST, ANCHORS),
    ('queue-fifo', DOMAIN, TEST, ANCHORS),
    ('closed-turn', DOMAIN, TEST, ANCHORS),
    ('step-increase', DOMAIN, TEST, ANCHORS),
    ('context-duplicate', DOMAIN, TEST, ANCHORS),
    ('context-order', DOMAIN, TEST, ANCHORS),
    ('request-duplicate', DOMAIN, TEST, ANCHORS),
    ('permission-request', DOMAIN, TEST, ANCHORS),
    ('tool-request', DOMAIN, TEST, ANCHORS),
    ('tool-duplicate', DOMAIN, TEST, ANCHORS),
    ('open-tool-on-step-close', DOMAIN, TEST, ANCHORS),
    ('lifecycle-metadata', DOMAIN, TEST, ANCHORS),
    ('lifecycle-duplicate', DOMAIN, TEST, ANCHORS),
    ('inbox-empty-item', DOMAIN, TEST, ANCHORS),
    ('inbox-insert', DOMAIN, TEST, ANCHORS),
    ('inbox-cancel', DOMAIN, TEST, ANCHORS),
    ('inbox-claim', DOMAIN, TEST, ANCHORS),
    ('plan-value', DOMAIN, TEST, ANCHORS),
    ('plan-progress', DOMAIN, TEST, ANCHORS),
    ('todos-metadata', DOMAIN, TEST, ANCHORS),
    ('todo-item', DOMAIN, TEST, ANCHORS),
    ('goal-revision-integer', DOMAIN, TEST, ANCHORS),
    ('goal-revision-sequence', DOMAIN, TEST, ANCHORS),
    ('goal-snapshot', DOMAIN, TEST, ANCHORS),
    ('goal-blocked-reason', DOMAIN, TEST, ANCHORS),
    ('turn-pending-lifecycles', DOMAIN, TEST, ANCHORS),
    ('turn-pending-inbox', DOMAIN, TEST, ANCHORS),
    ('step-matching', DOMAIN, TEST, ANCHORS),
    ('inbox-discard', DOMAIN, TEST, ANCHORS),
    ('null-entry-property', DOMAIN, TEST, ANCHORS),
    ('discard-undefined-lookup', DOMAIN, TEST, ANCHORS),
    ('claim-undefined-lookup', DOMAIN, TEST, ANCHORS),
    ('instructions-order', DOMAIN, TEST, ANCHORS),
    ('model-event-order', DOMAIN, TEST, ANCHORS),
    ('step-already-open', DOMAIN, TEST, ANCHORS),
    ('turn-open-step', DOMAIN, TEST, ANCHORS),
    ('lifecycle-terminal', DOMAIN, TEST, ANCHORS),
    ('tool-result-pair', DOMAIN, TEST, ANCHORS),
    ('queue-must-claim', DOMAIN, TEST, ANCHORS),
    ('context-state', DOMAIN, TEST, ANCHORS),
    ('request-state', DOMAIN, TEST, ANCHORS),
    ('closed-state', DOMAIN, TEST, ANCHORS),
    ('goal-state', DOMAIN, TEST, ANCHORS),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    DOMAIN,
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
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
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
                                 mutant_switch.clean_environment(os.environ),
                                 guarded=GUARDED, script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
