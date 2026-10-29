#!/usr/bin/env python3
"""#978: compile-time controls for the adaptive locale idle budget and CPU timing.

The four mutations are ``crate::mutants::on`` branches compiled into the
feature-enabled library test binary; no source file is rewritten, so there is
no snapshot to restore. ``CARGO_TARGET_DIR`` must come from the caller.
Commit first: ``require_head`` refuses a working tree that differs from HEAD.

Use ``--check`` for a read-only anchor preflight, or
``NEW_OUTPUT_DIR [switch ...]`` after committing.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
from negative_process import run_owned

ROOT = Path(__file__).resolve().parents[5]
ORACLE = ROOT / 'src/pilotdeck-gateway/tests/common/oracle_locale.rs'
LOCALE_TESTS = ROOT / 'src/pilotdeck-gateway/tests/common/oracle_locale_tests.rs'
TIMING = ROOT / 'src/pilotdeck-gateway/tests/common/timing.rs'
# The html-cpu-wall-clock kill assertion lives in this product file, not in the
# switch site above: a dirty copy must refuse the run and enter source.json.
HTML_COMPAT = (
    ROOT / 'src/pilotdeck-gateway/src/runtime/tools/builtin/web/html_compat.rs'
)
HTML_COMPAT_TESTS = (
    'runtime::tools::builtin::web::html_compat::tests::'
    'cpu_sample_requires_completed_work_and_ignores_preemption'
)
# (name, switch site, bound test, assertion anchor). The switch site must occur
# exactly once in the named file; the assertion is the line that must fail.
switches = [
    ('html-cpu-wall-clock', TIMING, HTML_COMPAT_TESTS,
     '#978 CPU sample excludes preemption'),
    ('locale-idle-nonadaptive', ORACLE,
     'test_oracle_locale::tests::adaptive_idle_scales_with_host_load',
     '#978 adaptive idle must scale with host load'),
    ('locale-idle-unbounded', ORACLE,
     'test_oracle_locale::tests::silent_child_expires_at_adaptive_idle_deadline',
     '#978 adaptive idle must still expire for a silent child'),
    ('locale-sampler-unbounded', ORACLE,
     'test_oracle_locale::tests::hanging_sampler_is_reaped_and_falls_back_to_base_budget',
     '#978 sampler must stop a hanging child'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    ORACLE,
    LOCALE_TESTS,
    TIMING,
    HTML_COMPAT,
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


def require_head():
    """The compiled switches and their bindings must be the committed bytes."""
    names = [str(path.relative_to(ROOT)) for path in GUARDED if path.exists()]
    result = run_owned(['git', 'diff', '--quiet', 'HEAD', '--', *names], cwd=ROOT, timeout=30)
    if result.returncode:
        raise ValueError(
            'guarded sources differ from HEAD; commit the fix and its controls first'
        )


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
        require_head()
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
