#!/usr/bin/env python3
"""#996: tool-budget-cwd controls as compile-time switches.

The #946 text-replacement negative_controls.py and session_negative.py are
deleted; each mutation is now a ``crate::mutants::on`` branch compiled into
the feature-enabled library test binary. CARGO_TARGET_DIR must be supplied.
Use --check for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
BUDGET = ROOT / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
CWD = 'runtime::context::tool_budget::cwd_tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
# The cwd-root-fallback row binds every outcome-matrix cell, so no cell is
# judged by another cell's anchor.
switches = [
    ('budget-cwd-root-fallback', BUDGET,
     [CWD + 'relative_deleted', CWD + 'relative_warmed_deleted'],
     ['#885 outcome matrix: relative-deleted',
      '#885 outcome matrix: relative-warmed-deleted']),
    ('budget-cwd-syscall', BUDGET, CWD + 'relative_deleted',
     '#885 outcome matrix: relative-deleted'),
    ('budget-cwd-rust-display', BUDGET, CWD + 'relative_deleted',
     '#885 outcome matrix: relative-deleted'),
    ('budget-absolute-reads-cwd', BUDGET, CWD + 'absolute_deleted',
     '#885 outcome matrix: absolute-deleted'),
    ('budget-session-kind', FACTORY,
     'runtime::turn::host_factory::tests::'
     'session_composition_uv_cwd_error_reaches_stream_unwrapped',
     '#885 composition failure must preserve the TS uv_cwd message on the first poll'),
    ('budget-live-prefix', LIVE,
     'runtime::turn::host_factory::tests::'
     'session_composition_uv_cwd_error_reaches_stream_unwrapped',
     '#885 composition failure must preserve the TS uv_cwd message on the first poll'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    BUDGET,
    FACTORY,
    LIVE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs',
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
                                 guarded=GUARDED, prefix='', script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
