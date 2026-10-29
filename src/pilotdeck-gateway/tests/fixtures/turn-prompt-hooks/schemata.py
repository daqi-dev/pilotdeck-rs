#!/usr/bin/env python3
"""#996: turn-prompt-hooks controls as compile-time switches.

The #741 text-replacement negative_controls.py is deleted; each mutation is
now a ``crate::mutants::on`` branch compiled into the feature-enabled library
test binary. CARGO_TARGET_DIR and PDRS_BASELINE must be supplied. Use --check
for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SRC = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
PROMPT = SRC / 'runner_prompt.rs'
LIVE = SRC / 'live.rs'
# The regeneration switch lives at the library-test call site: the shared
# baseline module is compiled into integration-test crates as well, where the
# library's switches do not exist.
PROMPT_TESTS = SRC / 'runner_prompt_tests.rs'
BASELINE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
UNIT = 'runtime::turn::runner::tests::prompts::'
HOST = 'runtime::project::factory::tests::interaction_host::prompt_hooks::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('prompt-wiring', LIVE, HOST + 'prompt_host_pass',
     '#741 prompt live context forwarded'),
    ('prompt-gate', LIVE, HOST + 'prompt_host_pass',
     '#741 prompt live submitted'),
    ('prompt-execute-context', LIVE, HOST + 'prompt_host_pass',
     '#741 prompt live context forwarded'),
    ('prompt-abort-context', LIVE, HOST + 'prompt_host_context_abort',
     '#741 prompt live context forwarded'),
    ('prompt-block-gate', LIVE, HOST + 'prompt_host_block',
     '#741 prompt live execute gate'),
    ('prompt-accepted-first', PROMPT, UNIT + 'before_accept',
     '#741 prompt requires accepted input'),
    ('prompt-submitted-once', PROMPT, UNIT + 'pending',
     '#741 prompt duplicate rejected'),
    ('prompt-block-effect', PROMPT, UNIT + 'block',
     '#741 prompt loop input'),
    ('prompt-hook-context', PROMPT, UNIT + 'context',
     '#741 prompt loop input'),
    ('prompt-dispatch-reject', PROMPT, UNIT + 'reject',
     ['#741 prompt rejection', '#741 prompt loop input']),
    ('prompt-cancel-pending', PROMPT, UNIT + 'abort_pending',
     '#741 prompt pending bound'),
    ('prompt-cancel-signal', PROMPT, UNIT + 'drop_pending',
     '#741 prompt signal on cancel only'),
    ('prompt-text-coercion', PROMPT, UNIT + 'blocks_coerce',
     '#741 prompt events'),
    ('prompt-null-block', PROMPT, UNIT + 'blocks_null',
     ['#741 prompt rejection', '#741 prompt loop input']),
    ('prompt-regeneration', PROMPT_TESTS, UNIT + 'regenerates_identically',
     '#741 prompt fixture regeneration'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    LIVE,
    PROMPT,
    PROMPT_TESTS,
    BASELINE,
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
