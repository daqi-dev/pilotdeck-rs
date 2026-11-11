#!/usr/bin/env python3
"""#897: session-composition cwd-preflight controls as compile-time switches.

The #885 relative/absolute outcome matrix stays in tool-budget-cwd; this suite
binds the #897 session-factory landing: the composition-time tool-result budget
preflight in ProjectSessionFactory.construct (reject the create itself, verbatim
TS uv_cwd message, prepared-resource rollback) and the per-session
tool-results directory layout shared with the per-turn ports. CARGO_TARGET_DIR
must be supplied. Use --check for a read-only anchor preflight, or
NEW_OUTPUT_DIR [switch ...].

Every entry is judged through the #946 nonce receipt protocol (receipts=True):
a bound test only counts as killed on a named FAILED line plus this run's
assertion receipt, never on prose.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
TOOLS = ROOT / 'src/pilotdeck-gateway/src/native_process/tools.rs'
FACTORY_TEST = ('runtime::session::factory::tests::'
                'session_cwd_preflight_rejects_create_and_rolls_back')
TOOLS_TEST = 'native_process::tests::session_tool_results_dir_layout_and_sanitization'
# (name, path, test, assertion): the switch site is counted in the target file.
# The worker-based factory test asserts rejection, message and rollback on the
# relative-deleted cell, so each mutant is judged by its own bound assertion.
switches = [
    ('session-cwd-preflight-skipped', FACTORY, FACTORY_TEST,
     '#897 deleted-cwd composition must reject the create itself'),
    ('session-cwd-preflight-message', FACTORY, FACTORY_TEST,
     '#897 create rejection must preserve the TS uv_cwd message'),
    ('session-cwd-rollback-order', FACTORY, FACTORY_TEST,
     '#897 rollback follows the TS storage-close-then-release order'),
    ('session-cwd-dir-layout', TOOLS, TOOLS_TEST,
     '#897 session tool-results directory keeps the project-relative layout'),
    ('session-cwd-dir-unsanitized', TOOLS, TOOLS_TEST,
     '#897 session tool-results directory sanitizes the session id'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    FACTORY,
    TOOLS,
    ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs',
    # The per-turn live-read difference is the registered remainder of A7.4;
    # its lock must not move while this suite runs.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs',
    ROOT / 'src/pilotdeck-gateway/tests/fixtures/tool-budget-cwd/oracle.json',
    # Workers are re-executed with env_clear; the selector forwarding lives
    # here and must not change under a running suite.
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py',
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
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
                                 mutant_switch.clean_environment(os.environ),
                                 guarded=GUARDED, prefix='', script=Path(__file__),
                                 requested=args.names, receipts=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
